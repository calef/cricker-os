//! Exception handling.
//!
//! One mechanism serves three purposes on aarch64, and it is worth seeing that they
//! are the same thing before we build the other two:
//!
//!   - a **fault** (bad memory access, illegal instruction)  <- milestone 2, here
//!   - an **interrupt** (the timer, the UART)                <- milestone 5
//!   - a **syscall** (`svc` from userspace)                  <- milestone 7
//!
//! All three suspend the current instruction stream, switch to EL1, and jump to an
//! address the kernel chose. Only the reason differs. Build the plumbing once.
//!
//! See notes/exceptions.md.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use aarch64_cpu::asm::barrier;
use aarch64_cpu::registers::{ESR_EL1, FAR_EL1, VBAR_EL1};
use tock_registers::interfaces::{Readable, Writeable};

use super::timer;
use crate::arch::{UserFault, UserFaultAccess};
use crate::drivers::gic;
use crate::println;

/// The interrupted CPU state, as saved by `SAVE_CONTEXT` in `vectors.s`.
///
/// This layout is a **contract with assembly**. The compiler cannot check it for us,
/// so there is a size assertion below, which catches about half of the ways to get it
/// wrong. The other half (reordering two fields of the same type) it cannot catch, so
/// be careful.
#[repr(C)]
pub struct TrapFrame {
    /// `x0` through `x30`. `x30` is the link register.
    pub x: [u64; 31],

    /// Where the interrupted code will resume.
    ///
    /// **Writable, and that matters.** Advancing this is how we step past a `brk`.
    /// Milestone 7 will use it to skip past an `svc`. The hardware reloads the program
    /// counter from here on `eret`, so whatever we leave in this field is where the
    /// world continues.
    pub elr: u64,

    /// The processor state (condition flags, exception level, interrupt masks) the
    /// interrupted code was in. `eret` restores it.
    pub spsr: u64,

    /// **The user's stack pointer.** `SP_EL0`, which is a different register from the
    /// `sp` the kernel is using.
    ///
    /// At EL1 we run with `SPSel=1`, so `sp` means `SP_EL1`, the thread's kernel stack.
    /// Taking an exception from EL0 switches the hardware to `SP_EL1` and leaves `SP_EL0`
    /// alone, so the user's stack pointer is simply still sitting there. It survives the
    /// exception on its own.
    ///
    /// What it does **not** survive is a context switch to another *user* thread, which
    /// would spend `SP_EL0` on its own stack and never give it back. So it travels in the
    /// frame, with the thread.
    ///
    /// It cost nothing to add: it landed in the padding word the frame already had, which is
    /// why the size assertion below is unchanged at 272.
    pub sp_el0: u64,
}

// If this fails, `SAVE_CONTEXT` and `TrapFrame` have drifted apart, and the Rust side
// is about to read the wrong bytes.
const _: () = assert!(size_of::<TrapFrame>() == 272);

impl TrapFrame {
    /// Build the frame that drops a brand-new thread to EL0 at `entry` on `user_sp`, with `args` in
    /// `x0`..`x2`. The arch side of the userspace-entry seam (notes/riscv-port.md, leak #3):
    /// portable `user.rs` asks for "a user-entry frame" and never names `elr`/`spsr`/`sp_el0`.
    ///
    /// `spsr = 0` is the `SPSR_EL1` value for "return to `EL0t`, AArch64, interrupts unmasked":
    /// `M[4]=0` (AArch64), `M[3:0]=0` (`EL0t`, the only stack EL0 has), `DAIF=0` (IRQs on the moment we
    /// land, so a tight-loop user thread is still preemptible, per DECISIONS §5). Zero looks like a
    /// bug and is not.
    /// The syscall number the caller passed. aarch64 ABI: register `x8` (DECISIONS §10). The
    /// portable syscall dispatcher reads it through here so it never names a register directly.
    pub fn syscall_nr(&self) -> u64 {
        self.x[8]
    }

    /// Syscall argument register `i` (aarch64: `x0`..`x5`).
    pub fn arg(&self, i: usize) -> u64 {
        self.x[i]
    }

    /// Set syscall argument/return register `i`. The syscall return value and the IPC message words
    /// ride in these same registers, which is why the setter is general.
    pub fn set_arg(&mut self, i: usize, v: u64) {
        self.x[i] = v;
    }

    pub fn for_user_entry(entry: u64, user_sp: u64, args: [u64; 3]) -> Self {
        let mut x = [0u64; 31];
        x[0] = args[0]; // _start's first argument (AAPCS64 puts it in x0)
        x[1] = args[1];
        x[2] = args[2];
        TrapFrame {
            x,
            elr: entry, // where `eret` jumps
            spsr: 0,    // EL0t, interrupts on (see above)
            sp_el0: user_sp,
        }
    }
}

unsafe extern "C" {
    /// `mov sp, x0` then fall into `exception_restore`: `eret` into the EL0 state the frame
    /// describes. Defined in vectors.s.
    fn enter_userspace(frame: *mut TrapFrame) -> !;
}

/// Drop to EL0 by loading `frame` and returning from the exception into it. The arch side of the
/// userspace-entry seam; the portable caller builds the frame with [`TrapFrame::for_user_entry`],
/// places it at the top of the current thread's kernel stack, and calls this.
///
/// **`#[inline(always)]` is load-bearing, not cosmetic.** The frame sits at the *top of the caller's
/// own kernel stack*, overlapping the caller's live call frames (see `enter_frame` in user.rs). It is
/// intact only until `enter_userspace` does `mov sp, x0`, and only if nothing pushes onto the stack
/// in between. A real call here would push a return address and prologue into exactly that region and
/// corrupt the frame (observed as a child thread getting `sp_el0 = 0`). Inlining makes the caller
/// tail-call `enter_userspace` directly, with no push, which is what the pre-seam direct call did.
///
/// # Safety
/// `frame` must be a correctly-built, writable `TrapFrame` at the top of the current thread's kernel
/// stack, with EL0's code and stack mapped and `TTBR0` installed.
#[inline(always)]
pub unsafe fn enter_user(frame: *mut TrapFrame) -> ! {
    // **Refuse to enter EL0 with no entry point**, the twin of the riscv64 check (DECISIONS §19:
    // a capability ships on every architecture or a scope note records the gap, and a diagnostic
    // that exists on one ISA would report the bug only where it happens to be looked for).
    //
    // A thread entered with `elr == 0` fetches its first instruction from address 0 and dies, and
    // whatever it served never answers, so the run ends in the lost-wakeup watchdog far from the
    // cause. The paragraph above already records this shape on this ISA: a real call frame pushed
    // over the trap frame was "observed as a child thread getting `sp_el0 = 0`". That was fixed by
    // the `#[inline(always)]`; this catches the same corruption arriving by any other route.
    //
    // A load and a branch on a frame this function already touches, so nothing is pushed over the
    // frame and the inlining property above is preserved.
    //
    // SAFETY: the caller owns the frame's validity, as documented.
    let elr = unsafe { (*frame).elr };
    if elr == 0 {
        // SAFETY: as above; read before the cold call is allowed to use this stack.
        let sp_el0 = unsafe { (*frame).sp_el0 };
        entered_user_with_no_entry_point(sp_el0);
    }

    // SAFETY: the caller owns the frame's validity, as documented.
    unsafe { enter_userspace(frame) }
}

/// The `elr == 0` case, out of line so [`enter_user`] stays a tail call.
///
/// Its argument is read from the frame **before** it is called, because the frame overlaps this very
/// stack and this call is entitled to overwrite it, which is the corruption the check exists to name.
#[cold]
#[inline(never)]
fn entered_user_with_no_entry_point(sp_el0: u64) -> ! {
    panic!(
        "thread {} on core {} was dispatched to EL0 with elr = 0 (sp_el0 {:#018x}). \
         Its context was never built, or was built and not seen by this core.",
        crate::sched::current(),
        crate::cpu::id(),
        sp_el0,
    )
}

/// **Diagnostic: the EL0 PC of a thread from the `TrapFrame` at the top of its kernel stack.** A
/// thread that trapped from EL0 (a syscall, or a timer preemption while spinning) left its frame at
/// `stack_top - size_of::<TrapFrame>()`, where `elr` is its EL0 PC. Used by the watchdog dump to say
/// *where* each thread is, not just its scheduler state. Meaningless for a pure kernel thread.
pub fn user_pc(stack_top: u64) -> u64 {
    let frame = (stack_top - size_of::<TrapFrame>() as u64) as *const TrapFrame;
    // SAFETY: diagnostic read of the frame the vector's SAVE_CONTEXT wrote at the stack top.
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*frame).elr)) }
}

/// How many `brk` instructions we have caught and stepped over.
///
/// Exists so the tests can prove the handler actually ran, rather than proving only
/// that we didn't crash.
pub static BRK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Exception Class: `ESR_EL1` bits 31:26.
///
/// The single most useful field in the machine when something has gone wrong. It says
/// *what kind* of thing happened, and everything else is detail.
mod ec {
    pub const UNKNOWN: u64 = 0x00;
    pub const TRAPPED_WFI_WFE: u64 = 0x01;
    pub const ILLEGAL_EXECUTION_STATE: u64 = 0x0e;
    pub const SVC64: u64 = 0x15;
    pub const TRAPPED_MSR_MRS: u64 = 0x18;
    pub const INSTRUCTION_ABORT_LOWER_EL: u64 = 0x20;
    pub const INSTRUCTION_ABORT_SAME_EL: u64 = 0x21;
    pub const PC_ALIGNMENT_FAULT: u64 = 0x22;
    pub const DATA_ABORT_LOWER_EL: u64 = 0x24;
    pub const DATA_ABORT_SAME_EL: u64 = 0x25;
    pub const SP_ALIGNMENT_FAULT: u64 = 0x26;
    pub const SERROR: u64 = 0x2f;
    pub const BREAKPOINT_LOWER_EL: u64 = 0x30;
    pub const BREAKPOINT_SAME_EL: u64 = 0x31;
    pub const BRK64: u64 = 0x3c;
}

fn ec_name(class: u64) -> &'static str {
    match class {
        ec::UNKNOWN => "Unknown reason",
        ec::TRAPPED_WFI_WFE => "Trapped WFI/WFE",
        ec::ILLEGAL_EXECUTION_STATE => "Illegal execution state",
        ec::SVC64 => "SVC (syscall) from AArch64",
        ec::TRAPPED_MSR_MRS => "Trapped system register access",
        ec::INSTRUCTION_ABORT_LOWER_EL => "Instruction abort from a lower EL",
        ec::INSTRUCTION_ABORT_SAME_EL => "Instruction abort from the same EL",
        ec::PC_ALIGNMENT_FAULT => "PC alignment fault",
        ec::DATA_ABORT_LOWER_EL => "Data abort from a lower EL",
        ec::DATA_ABORT_SAME_EL => "Data abort from the same EL",
        ec::SP_ALIGNMENT_FAULT => "SP alignment fault",
        ec::SERROR => "SError",
        ec::BREAKPOINT_LOWER_EL => "Breakpoint from a lower EL",
        ec::BREAKPOINT_SAME_EL => "Breakpoint from the same EL",
        ec::BRK64 => "BRK instruction",
        _ => "unrecognized exception class",
    }
}

/// The sixteen slots in the vector table, in hardware order. See `vectors.s`.
const VECTOR_NAMES: [&str; 16] = [
    "Current EL, SP_EL0, Synchronous",
    "Current EL, SP_EL0, IRQ",
    "Current EL, SP_EL0, FIQ",
    "Current EL, SP_EL0, SError",
    "Current EL, SP_ELx, Synchronous",
    "Current EL, SP_ELx, IRQ",
    "Current EL, SP_ELx, FIQ",
    "Current EL, SP_ELx, SError",
    "Lower EL, AArch64, Synchronous",
    "Lower EL, AArch64, IRQ",
    "Lower EL, AArch64, FIQ",
    "Lower EL, AArch64, SError",
    "Lower EL, AArch32, Synchronous",
    "Lower EL, AArch32, IRQ",
    "Lower EL, AArch32, FIQ",
    "Lower EL, AArch32, SError",
];

/// Install the vector table.
///
/// After this returns, a fault produces a legible report instead of a silent death.
/// Until it returns, it doesn't.
pub fn init() {
    unsafe extern "C" {
        static exception_vectors: core::ffi::c_void;
    }

    let base = (&raw const exception_vectors) as u64;
    VBAR_EL1.set(base);

    // An Instruction Synchronization Barrier makes the CPU discard everything it has
    // already fetched or speculated past this point and start again.
    //
    // Without it, the write to VBAR_EL1 is not architecturally guaranteed to be in
    // effect for the *very next instruction*. And "the very next instruction" is
    // exactly when a fault might arrive. This is one line, it is easy to leave out,
    // and leaving it out produces a bug that appears only under timing you cannot
    // reproduce.
    barrier::isb(barrier::SY);
}

/// Called from every vector entry, with the saved state and which slot fired.
///
/// `extern "C"` because assembly calls it: `frame` arrives in `x0`, `index` in `x1`,
/// per AAPCS64. See notes/registers.md.
/// Vector slot 5: Current EL, `SP_ELx`, IRQ. **The kernel being interrupted.**
const VECTOR_IRQ_CURRENT: u64 = 5;
/// Vector slot 9: Lower EL, AArch64, IRQ. Userspace being interrupted. Milestone 7.
const VECTOR_IRQ_LOWER: u64 = 9;

unsafe extern "C" {
    /// Switch to `top` (or stay put if it is 0), call [`exception_body`], come back.
    /// Defined in vectors.s, because moving `sp` is assembly and policy is not.
    fn dispatch_on_interrupt_stack(frame: &mut TrapFrame, index: u64, top: u64) -> bool;
}

/// **The outer half of the trap path, and the half that stays on the interrupted stack.**
///
/// It does exactly two things the inner half must not: it picks the stack the handler runs on, and
/// it runs the deferred `schedule()` afterwards. The second is the reason the split exists.
/// `schedule()` parks this thread's `sp` in its `Context` and resumes it there later, so it may only
/// ever be called on a stack that belongs to the thread. Calling it from the interrupt stack would
/// park a per-core address in a thread, and the thread would resume on bytes the next interrupt had
/// already spent. See `kernel/src/interrupt_stack.rs`, which holds the whole rule and its
/// mechanisms.
#[unsafe(no_mangle)]
extern "C" fn exception_dispatch(frame: &mut TrapFrame, index: u64) {
    let top = crate::interrupt_stack::top_for_trap(from_lower_el(index));
    // SAFETY: `top` is either 0 or this core's own interrupt-stack top, from the module that owns
    // the region; the trampoline calls `exception_body` with our own two arguments and restores
    // `sp` before returning. The frame outlives the call: it is on the stack we are standing on.
    let deferred_switch = unsafe { dispatch_on_interrupt_stack(frame, index, top) };

    // Back on the interrupted thread's stack, whichever branch ran. Preemption happens HERE.
    if deferred_switch {
        crate::sched::preempt_if_needed();
    }
}

/// The trap handler proper: everything that runs on the interrupt stack when there is one.
///
/// Returns whether the caller owes a deferred `schedule()`, which is true for an IRQ and false for
/// everything else. It used to *be* the switch, at the bottom of `handle_irq`; what changed is that
/// the decision travels out to a frame that is provably on the interrupted thread's own stack.
#[unsafe(no_mangle)]
extern "C" fn exception_body(frame: &mut TrapFrame, index: u64) -> bool {
    // IRQ is dispatched by SLOT, not by ESR.
    //
    // ESR_EL1 describes a *synchronous* exception: what instruction did what wrong. An IRQ is
    // asynchronous. It has nothing to do with the instruction it interrupted, and ESR_EL1 holds
    // whatever the last synchronous exception left there. Reading it here would be reading a
    // stale answer to a question nobody asked.
    if index == VECTOR_IRQ_CURRENT || index == VECTOR_IRQ_LOWER {
        handle_irq(frame);
        // The one arm that owes a reschedule check, and the caller runs it on the interrupted
        // stack. `handle_irq` used to end with the check itself.
        return true;
    }

    let esr = ESR_EL1.get();
    let class = (esr >> 26) & 0x3f;

    match class {
        // NOTE THE GUARD. Without it, a `brk` from EL0 would be *stepped over* as if it were
        // one of ours, and a user program could park a `brk` in a loop and burn the kernel's
        // time forever, immortal. A breakpoint is a debugging affordance for code we trust.
        // From EL0 it is a fault, and it falls through to `user_fault` below.
        ec::BRK64 if !from_lower_el(index) => {
            // `brk` is a deliberate trap: a breakpoint the program asked for.
            //
            // The subtlety: ELR_EL1 points AT the `brk` instruction, not past it.
            // (Compare `svc`, where the hardware already advances it for you.) So if
            // we just `eret`, we execute the `brk` again, forever.
            //
            // Stepping over it means advancing ELR by one instruction, and every
            // aarch64 instruction is exactly 4 bytes. That fixed-width design we
            // liked in notes/aarch64.md is what makes this a `+= 4` instead of a
            // decode.
            BRK_COUNT.fetch_add(1, Ordering::Relaxed);
            frame.elr += 4;
        }

        // `svc` from EL0. **The syscall.**
        //
        // At 7a this arm did nothing but count, deliberately, because DECISIONS §8 said "if we
        // find ourselves hacking in a syscall without having had that conversation, the plan has
        // failed." We had the conversation (§10), chose capabilities, and 7d designed the whole
        // surface at once against a capability table. It is three calls. See syscall.rs.
        //
        // Note ELR already points PAST the `svc`: the hardware advances it for us. Compare
        // `brk` above, where it points AT the instruction and we must step over it by hand.
        //
        // `dispatch` writes the result into `frame.x[0]`, and RESTORE_CONTEXT pops that into the
        // register the user program is waiting on. **Writing to the trap frame is writing to the
        // user's registers.**
        ec::SVC64 if from_lower_el(index) => {
            SVC_COUNT.fetch_add(1, Ordering::Relaxed);
            crate::syscall::dispatch(frame);
        }

        // Anything else from EL0 is the user program being wrong.
        //
        // **It dies. The kernel does not.** That is the whole promise of a privilege boundary,
        // and this is the first moment in the project's life that we can keep it.
        _ if from_lower_el(index) => user_fault(frame, esr),

        // Everything else is a KERNEL bug, and fatal. As the kernel grows, cases move out of
        // `fatal` and into real handlers: IRQs at milestone 5, `svc` here, and data aborts
        // become page faults if we ever do demand paging.
        _ => fatal(frame, index, esr),
    }

    // A synchronous exception has already done whatever it was going to do (stepped over a `brk`,
    // run a syscall, killed a thread). Nothing is deferred, so nothing is owed.
    false
}

/// Did this exception come from a lower exception level, i.e. from EL0?
///
/// Slots 8-11 are "Lower EL, AArch64" (see `VECTOR_NAMES`). The distinction carries enormous
/// weight: the **same** exception class means "a bug in the kernel, halt the machine" when it
/// arrives at slot 4, and "a bug in the user program, kill it" when it arrives at slot 8.
fn from_lower_el(index: u64) -> bool {
    (8..=11).contains(&index)
}

/// How many `svc` instructions we have caught from EL0.
pub static SVC_COUNT: AtomicUsize = AtomicUsize::new(0);

/// How many user threads have been killed for faulting.
pub static USER_FAULTS: AtomicUsize = AtomicUsize::new(0);

/// How many device/SGI interrupts were routed to a userspace endpoint. Bring-up diagnostic.
pub static ROUTED_IRQS: AtomicUsize = AtomicUsize::new(0);

/// The most recent user fault, [`UserFault::encode`]d, and the address it named. Two relaxed
/// stores on a path that is already killing a thread; read back through [`last_user_fault`].
///
/// **Private on purpose.** They used to be a public `LAST_USER_FAULT_ESR`/`FAR` pair, which meant
/// every test that wanted to say "that was a permission fault at exactly this address" had to
/// decode `ESR_EL1` inline, and that is aarch64 spelling sitting in a test we want to run on two
/// ISAs. The accessor below is the same fact in words RISC-V can also say. See [`UserFault`].
static LAST_USER_FAULT: AtomicU64 = AtomicU64::new(0);
static LAST_USER_FAULT_ADDR: AtomicU64 = AtomicU64::new(0);

/// The last user fault's kind and the address it named, or `None` if no user thread has faulted
/// yet. The RISC-V twin is `arch::riscv64::exceptions::last_user_fault`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn last_user_fault() -> Option<(UserFault, u64)> {
    // Pairs with the `Release` on `USER_FAULTS` in [`user_fault`]. A caller that has already seen
    // the counter rise (every one of them has; that is how they know to look) reads the record the
    // faulting core wrote and not something older. We are on ARM, so this is not free ordering we
    // can assume: the two relaxed stores below the counter's release are exactly the reordering the
    // architecture permits.
    //
    // PAIR: `USER_FAULTS.fetch_add(1, Ordering::Release)` in [`user_fault`], below in this file.
    // Both halves are here and both are load-bearing; the riscv64 twin is the same pair.
    core::sync::atomic::fence(Ordering::Acquire);
    let kind = UserFault::decode(LAST_USER_FAULT.load(Ordering::Relaxed))?;
    Some((kind, LAST_USER_FAULT_ADDR.load(Ordering::Relaxed)))
}

/// Read `ESR_EL1` as a [`UserFault`].
///
/// **aarch64 is told the answer.** The fault status code in `ESR_EL1[5:0]` distinguishes a
/// permission fault (`0b0011LL`) from a translation fault (`0b0001LL`) in silicon, at the instant
/// of the fault, with the walk level in the low two bits. Nothing here is inferred; contrast the
/// RISC-V twin, which must derive the same distinction because `scause` does not carry it.
fn classify(esr: u64) -> UserFault {
    /// EC for an instruction abort taken from a lower EL: a failed *fetch*.
    const EC_INSTRUCTION_ABORT_LOWER: u64 = 0x20;
    /// EC for a data abort taken from a lower EL: a failed load or store.
    const EC_DATA_ABORT_LOWER: u64 = 0x24;
    /// ISS bit 6 of a data abort: set for a write, clear for a read.
    const WNR: u64 = 1 << 6;

    let access = match (esr >> 26) & 0x3f {
        EC_INSTRUCTION_ABORT_LOWER => UserFaultAccess::Fetch,
        EC_DATA_ABORT_LOWER if esr & WNR != 0 => UserFaultAccess::Write,
        EC_DATA_ABORT_LOWER => UserFaultAccess::Read,
        // An illegal instruction, a `brk`, a stack-alignment fault: not a memory access, so
        // neither "permission" nor "translation" is a true thing to say about it.
        _ => return UserFault::Other,
    };

    // (I|D)FSC[5:2] is the fault type; [1:0] is the walk level, which nothing above this cares
    // about. 0b0001 = translation, 0b0011 = permission.
    match (esr >> 2) & 0xf {
        0b0001 => UserFault::Translation(access),
        0b0011 => UserFault::Permission(access),
        _ => UserFault::Other,
    }
}

/// A user thread did something it is not allowed to do. Kill it.
///
/// # Why this can simply call `sched::exit()`
///
/// We are inside the exception handler, standing on the faulting thread's **kernel** stack,
/// with its `TrapFrame` just below us. `exit()` marks the thread `Finished` and calls
/// `schedule()`, which switches to somebody else and **never comes back**. `exception_restore`
/// is never reached, the `eret` never happens, and the user program is simply not resumed.
///
/// The kernel stack we are standing on right now is freed by the **next** thread, which is
/// precisely the reaper that milestone 6 built for a completely unrelated reason: a thread
/// cannot free the stack it is standing on. See notes/threads.md.
///
/// So the mechanism behind "a driver bug is a crashed process, not a dead machine"
/// (DECISIONS §10) was already sitting here, finished, before we knew we needed it.
fn user_fault(frame: &TrapFrame, esr: u64) -> ! {
    let class = (esr >> 26) & 0x3f;
    let far = FAR_EL1.get();

    // **The record first, the counter last, and the counter's store is the release.** Every test
    // that reads this record finds it by watching `USER_FAULTS` rise and then calling
    // [`last_user_fault`], so the counter is the publication flag for the record and must be
    // written after it. It was written first, and that is a race a test cannot see when it passes:
    // the reader either gets the record left by an *earlier* fault (an assertion satisfied by the
    // wrong evidence) or, if it is the first fault of the boot, a zero that reads as "no user
    // thread has faulted". Found by breaking `a_user_program_cannot_read_a_kernel_address` on
    // purpose and running it alone, where "the first fault of the boot" is the only case there is.
    LAST_USER_FAULT_ADDR.store(far, Ordering::Relaxed);
    LAST_USER_FAULT.store(classify(esr).encode(), Ordering::Relaxed);
    USER_FAULTS.fetch_add(1, Ordering::Release);

    crate::println!();
    crate::println!(
        "  user thread {} killed: {}",
        crate::sched::current(),
        ec_name(class),
    );
    crate::println!(
        "    pc {:#018x}   far {:#018x}   user sp {:#018x}   esr {:#010x}",
        frame.elr,
        far,
        frame.sp_el0,
        esr,
    );
    crate::println!("  the kernel is fine.");

    // Deliver the fault to a supervisor if this thread had one, and become a corpse; otherwise this
    // is `exit`'s unsupervised path (Finished, reaped by the next thread). See DECISIONS §26 and
    // sched::depart. `frame.elr` is the faulting pc, `far` the faulting address.
    crate::sched::fault(frame.elr, far);
}

/// Service one hardware interrupt.
///
/// **This runs with interrupts masked** (the hardware masks IRQ on entry to the vector), and it
/// runs on whatever stack the interrupted code was using. DECISIONS.md §9 is the law here:
/// **record and defer, do not do work.** Everything below is either an MMIO write or an atomic
/// increment. Nothing allocates. Nothing takes a lock above rank GIC.
fn handle_irq(_frame: &mut TrapFrame) {
    // Reading IAR is what ACKNOWLEDGES the interrupt. It has a side effect, so exactly once.
    let intid = gic::acknowledge();

    // 1023: the GIC changed its mind between raising the line and us getting here. Do nothing,
    // and in particular do NOT signal end-of-interrupt: completing an interrupt we never took
    // corrupts the GIC's priority stack.
    if intid == gic::SPURIOUS {
        SPURIOUS_IRQS.fetch_add(1, Ordering::Relaxed);
        return;
    }

    match intid {
        timer::TIMER_INTID => {
            timer::tick();
            // RECORD. Do not switch here: we still hold nothing, but we are mid-handler and
            // the GIC has not been told we are done. DECISIONS.md §9: handlers record and
            // defer. The deferral happens at the bottom of this function.
            crate::sched::on_tick();
        }
        crate::sched::RESCHED_SGI => {
            // Another core poked us for one of two reasons (SMP 3c, DECISIONS §28). It may have
            // handed us a thread via our inbox: drain it onto our run queue and reschedule (the
            // deferral at the bottom runs schedule()). Or an idle core asked us for work: serve the
            // steal by handing one queued thread back. The same SGI carries both; we do both.
            crate::sched::drain_inbox();
            crate::sched::serve_steal_request();
        }
        other => {
            // Is this interrupt routed to a userspace driver? If so, **it becomes a message.**
            //
            // Mask it at the distributor first, then deliver the notification. A level-triggered
            // device holds its interrupt line asserted until the driver quiets it, so if we left
            // it enabled it would re-fire the instant we EOI, in an unbreakable storm. The driver
            // re-enables it (its `Irq` capability's `ACK`) once it has serviced the device. This
            // is exactly seL4's IRQHandler protocol, and it is what lets a driver that owns no
            // privilege still own an interrupt. See notes/interrupts.md.
            if let Some(ep) = crate::sched::irq_route(other) {
                ROUTED_IRQS.fetch_add(1, Ordering::Relaxed);
                gic::disable(other);
                crate::sched::irq_notify(ep);
            } else {
                UNEXPECTED_IRQS.fetch_add(1, Ordering::Relaxed);
                println!("[IRQ] unexpected interrupt {other}, ignoring");
            }
        }
    }

    // Until this is written, the GIC will not deliver another interrupt of equal or lower
    // priority. Forget it and the timer fires exactly once and then never again.
    gic::end_of_interrupt(intid);

    // --- and preemption used to be here ---
    //
    // It is now four frames out, in `exception_dispatch`, and the move is milestone 124's
    // structural fix rather than tidying. This function may be running on **this core's interrupt
    // stack**, and `schedule()` parks the running `sp` in the outgoing thread's `Context`: park a
    // per-core address there and the thread resumes on bytes the next interrupt has spent. So the
    // handler returns first, back onto the interrupted thread's own stack, and the caller runs
    // `sched::preempt_if_needed` there. See kernel/src/interrupt_stack.rs.
    //
    // The EOI above still has to come first, for the reason it always did: switching away with the
    // interrupt unacknowledged would leave the GIC refusing to deliver anything of equal or lower
    // priority to the thread we switch *to*. Returning from here does not change that ordering,
    // because nothing between here and the deferred switch touches the GIC.
}

/// Interrupts the GIC raised and then withdrew. Not an error; worth counting.
pub static SPURIOUS_IRQS: AtomicUsize = AtomicUsize::new(0);

/// Interrupts we enabled but have no handler for. Definitely worth counting.
pub static UNEXPECTED_IRQS: AtomicUsize = AtomicUsize::new(0);

/// Print everything we know and stop.
///
/// A kernel with a violated invariant has no business continuing, so this does not
/// return. But it prints first, because a silent death teaches you nothing.
fn fatal(frame: &TrapFrame, index: u64, esr: u64) -> ! {
    // SAFETY: same reasoning as the panic handler. A fault taken mid-`println!` would
    // otherwise deadlock on the console lock and we would print nothing at all.
    // SAFETY: same reasoning as the panic handler. A fault taken while holding a lock would
    // otherwise deadlock, or trip the lock-ranking assertion, and we would print nothing at all.
    unsafe {
        crate::sync::force_reset_ranks();
        crate::console::force_unlock();
    };

    let class = (esr >> 26) & 0x3f;
    let name = VECTOR_NAMES
        .get(index as usize)
        .copied()
        .unwrap_or("<vector index out of range>");

    println!();
    println!("[EXCEPTION]  {name}");
    println!("             {} (EC {:#04x})", ec_name(class), class);
    println!();
    println!("  ESR_EL1   {esr:#018x}   what happened");

    // FAR_EL1 only holds a meaningful address for aborts and alignment faults. For
    // anything else it is stale garbage from an earlier fault, and printing it as if
    // it meant something would be a lie.
    let far_is_meaningful = matches!(
        class,
        ec::INSTRUCTION_ABORT_LOWER_EL
            | ec::INSTRUCTION_ABORT_SAME_EL
            | ec::DATA_ABORT_LOWER_EL
            | ec::DATA_ABORT_SAME_EL
            | ec::PC_ALIGNMENT_FAULT
    );
    if far_is_meaningful {
        println!(
            "  FAR_EL1   {:#018x}   the address that faulted",
            FAR_EL1.get()
        );
        // And say whether that address is a guard page, which decides what everything below means.
        // Only here: for a class where FAR is stale garbage the classifier would be reading a
        // previous fault's address and could name a stack at random. See the riscv64 twin.
        // The interrupted `SP_EL1`, computed rather than read: `SAVE_CONTEXT` built this frame at
        // the live `sp` minus its own size, so the frame's own address plus that size IS the `sp`
        // the trap interrupted. Reading the live one here would name the interrupt stack this
        // handler is standing on (milestone 124), which has nothing to do with the fault.
        let trapped_sp = frame as *const TrapFrame as u64 + size_of::<TrapFrame>() as u64;
        crate::stack::warn_if_guard_page(FAR_EL1.get(), trapped_sp);
    } else {
        println!("  FAR_EL1   (not meaningful for this exception class)");
    }

    println!(
        "  ELR_EL1   {:#018x}   the instruction that did it",
        frame.elr
    );
    println!("  SPSR_EL1  {:#018x}   the state it was in", frame.spsr);
    println!();

    for row in 0..8 {
        let a = row * 4;
        print_reg_row(frame, a);
    }
    println!();
    crate::stack::warn_if_smashed();

    panic!("unhandled exception: {}", ec_name(class));
}

/// Four registers per line, x28..x30 on the short last row.
fn print_reg_row(frame: &TrapFrame, first: usize) {
    use crate::print;
    print!("  ");
    for i in first..(first + 4).min(31) {
        print!("x{:<2} {:#018x}  ", i, frame.x[i]);
    }
    println!();
}

#[cfg(test)]
mod tests {
    //! Tests for exception handling.
    //!
    //! `registers_survive_an_exception` is the load-bearing one. The `TrapFrame` layout is a
    //! contract with assembly that the compiler cannot check, and a wrong offset would scramble a
    //! register while still returning happily to the right address: corrupting a caller's state
    //! and blaming innocent code thousands of instructions later.

    /// Proves the vector table is installed, and that the hardware's alignment rule
    /// is satisfied.
    ///
    /// The 2048-byte alignment is not a style preference. The CPU computes the target
    /// of an exception as `VBAR_EL1 + offset`, and it assumes the low 11 bits of the
    /// base are zero. A misaligned table sends every exception to a wrong address.
    #[test_case]
    fn vbar_el1_points_at_our_vector_table() {
        use aarch64_cpu::registers::VBAR_EL1;
        use tock_registers::interfaces::Readable;

        unsafe extern "C" {
            static exception_vectors: core::ffi::c_void;
        }
        let expected = (&raw const exception_vectors) as u64;

        assert_eq!(VBAR_EL1.get(), expected, "VBAR_EL1 not installed");
        assert_eq!(expected % 2048, 0, "vector table misaligned: {expected:#x}");
    }

    /// The real one: take an exception and come back from it.
    ///
    /// `brk #0` raises a synchronous exception. To reach the line after it, every
    /// single piece of milestone 2 has to be correct: the vector table is where
    /// `VBAR_EL1` says, slot 4 (Current EL, `SP_ELx`, Synchronous) fires, `SAVE_CONTEXT`
    /// writes a frame that matches `TrapFrame`, Rust decodes `ESR_EL1` and recognizes
    /// EC 0x3c, it advances ELR past the `brk` (which the hardware does NOT do for
    /// us, unlike `svc`), `RESTORE_CONTEXT` puts the machine back, and `eret` returns
    /// to exactly the right address.
    ///
    /// Get any of that wrong and you don't get a failing assertion. You get an
    /// infinite loop, or a crash. So arriving here at all is most of the test.
    #[test_case]
    fn breakpoint_is_caught_and_execution_resumes() {
        use core::sync::atomic::Ordering;

        use crate::arch::exceptions::BRK_COUNT;

        let before = BRK_COUNT.load(Ordering::Relaxed);

        // SAFETY: this deliberately faults. We handle it.
        unsafe { core::arch::asm!("brk #0") };

        assert_eq!(
            BRK_COUNT.load(Ordering::Relaxed),
            before + 1,
            "the handler didn't run, but we resumed anyway?"
        );
    }

    /// Proves the trap frame actually round-trips a register.
    ///
    /// The previous test proves we *return*. This proves we return with the machine
    /// intact, which is a different claim. Put a known value in a register, take an
    /// exception, read it back.
    ///
    /// A bug in `SAVE_CONTEXT/RESTORE_CONTEXT` (a wrong offset, a swapped pair) would
    /// scramble registers while still returning perfectly happily to the right
    /// address. That is the nastiest possible failure: it corrupts a caller's state
    /// and blames a completely innocent piece of code, thousands of instructions
    /// later. This is the test that catches it.
    #[test_case]
    fn registers_survive_an_exception() {
        let sent: u64 = 0xdead_beef_cafe_f00d;
        let got: u64;

        // SAFETY: deliberately faults; we handle it. x20 is callee-saved, so we tell
        // the compiler we're clobbering it.
        unsafe {
            core::arch::asm!(
                "mov x20, {sent}",
                "brk #0",
                "mov {got}, x20",
                sent = in(reg) sent,
                got = out(reg) got,
                out("x20") _,
            );
        }

        assert_eq!(got, sent, "the trap frame scrambled a register");
    }
}
