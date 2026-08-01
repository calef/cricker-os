//! **Traps, RISC-V.** The `stvec` vector, the saved [`TrapFrame`], and the dispatch into the
//! portable syscall and fault handlers. The S-mode analog of aarch64's `VBAR` table + `ESR` decode.
//!
//! RISC-V has a single trap entry (`stvec`), not aarch64's 16-slot table; interrupt-versus-exception
//! is the top bit of `scause`, and the syscall path is the `ecall` cause. The trap-entry assembly is
//! in trap.s; it fills a [`TrapFrame`] and calls [`riscv_trap_dispatch`], which fans out on `scause`.
//! The syscall-ABI reconciliation is done: the portable dispatcher reads the number and arguments
//! through `TrapFrame::{syscall_nr, arg, set_arg}` (see this module's `impl`), so `ecall`'s a7/a0..a5
//! map correctly without `syscall.rs` naming a register.

use crate::arch::{UserFault, UserFaultAccess};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// The registers saved on a trap. `x` is the RISC-V general-register file `x0`..`x31` (`x[0]` is the
/// hardwired zero); the trap CSRs follow. `#[repr(C)]` because the trap-entry assembly (the traps
/// step) will fill it field for field.
///
/// `x` is `pub` because the portable syscall dispatcher indexes it; the rest is arch-internal.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    /// The general registers `x0`..`x31`. `x0` is always zero; it is kept in the array so an index
    /// *is* a register number.
    pub x: [u64; 32],
    /// `sepc`: the PC the trap interrupted, where `sret` resumes.
    pub sepc: u64,
    /// `scause`: the trap cause (top bit = interrupt vs exception).
    pub scause: u64,
    /// `stval`: the trap value (faulting address, bad instruction, ...).
    pub stval: u64,
    /// `sstatus` at the trap, restored on the way out.
    pub sstatus: u64,
}

impl TrapFrame {
    /// The syscall number the caller passed. RISC-V `ecall` ABI: register `a7` (`x17`). The portable
    /// dispatcher reads it here so it never names a register directly. This, with `arg`/`set_arg`, is
    /// the resolution of the syscall-ABI leak flagged during the port (DECISIONS §17): aarch64 uses
    /// x8 + x0..x5, RISC-V uses a7 + a0..a5, and each maps its own registers.
    pub fn syscall_nr(&self) -> u64 {
        self.x[17] // a7
    }

    /// Syscall argument register `i` (RISC-V: `a0`..`a5`, i.e. `x10`..`x15`).
    pub fn arg(&self, i: usize) -> u64 {
        self.x[10 + i]
    }

    /// Set syscall argument/return register `i`. The return value and IPC message words ride in
    /// `a0`..`a2` (`x10`..`x12`).
    pub fn set_arg(&mut self, i: usize, v: u64) {
        self.x[10 + i] = v;
    }

    /// Build the frame that drops a brand-new thread to U-mode at `entry` on `user_sp`, with `args`
    /// in `a0`..`a2`. The RISC-V side of the userspace-entry seam (notes/riscv-port.md, leak #3),
    /// mirroring aarch64's `for_user_entry`. `sret` will resume at `sepc` in the privilege named by
    /// `sstatus.SPP`: SPP = 0 is U-mode, and SPIE = 1 makes interrupts enabled after the return, so a
    /// tight-loop user thread stays preemptible (the RISC-V analog of aarch64's DAIF = 0).
    ///
    /// The register indices are the RISC-V ABI: `a0`..`a2` are `x10`..`x12`, `sp` is `x2`. This is
    /// also where the syscall-ABI reconciliation (the traps step) will settle, since the dispatcher
    /// reads its arguments from this same frame.
    pub fn for_user_entry(entry: u64, user_sp: u64, args: [u64; 3]) -> Self {
        // sstatus for the sret: SPIE (interrupts on after the return), SPP left 0 (return to U-mode),
        // and UXL = 2 (U-mode is 64-bit, bits 33:32). UXL is load-bearing: the trap-return path
        // writes this whole value into sstatus, so omitting UXL would clear it to 0 and make the
        // U-mode XLEN illegal, faulting on the first user instruction.
        const SPIE: u64 = 1 << 5;
        const UXL_64: u64 = 2 << 32;

        // **SIE must stay clear in any fabricated frame, and this assertion is load-bearing.**
        // `trap_return` masks interrupts on entry (the fix for the exception-return race,
        // notes/exceptions.md) and then writes this whole word into `sstatus`. On RISC-V `sstatus`
        // holds both the staged fields and the LIVE interrupt-enable bit, so a frame carrying
        // SIE = 1 would re-enable interrupts right after `sepc` is staged and defeat the mask
        // outright. Real traps cannot do it (the hardware clears SIE on entry); a hand-built frame
        // like this one could, silently, so the compiler checks instead of a comment asking nicely.
        // aarch64 needs no equivalent: its staged `SPSR_EL1` is a different register from PSTATE.
        // See trap.s `trap_return` and notes/arch-audit.md.
        const SIE: u64 = 1 << 1;
        const _: () = assert!((SPIE | UXL_64) & SIE == 0);

        let mut x = [0u64; 32];
        x[10] = args[0]; // a0: _start's first argument
        x[11] = args[1]; // a1
        x[12] = args[2]; // a2
        x[2] = user_sp; // sp
        // x[4] (tp) is left 0: U-mode gets no kernel pointer. RISC-V's tp is a general register (not
        // a system register like aarch64's TPIDR_EL1), so the kernel per-CPU pointer must not ride in
        // U-mode. trap.s restores the kernel tp from this hart's per-hart trap stash (via sscratch)
        // on the way in, so the handler's cpu::current() is valid without leaking a kernel address.
        TrapFrame {
            x,
            sepc: entry,
            scause: 0,
            stval: 0,
            sstatus: SPIE | UXL_64,
        }
    }
}

unsafe extern "C" {
    /// Load `frame` as the trap frame and `sret` into it (the first entry to U-mode). Defined in
    /// trap.s; shares the restore path with the trap return. Its first instruction is `mv sp, a0`,
    /// so it does not touch the caller's stack.
    fn user_return(frame: *mut TrapFrame) -> !;
}

/// Drop to U-mode by loading `frame` and executing `sret`. The RISC-V side of the userspace-entry
/// seam (the counterpart of aarch64's `enter_user`).
///
/// **`#[inline(always)]` is load-bearing**, exactly as on aarch64: the frame sits at the top of the
/// caller's own kernel stack, so a real call frame pushed here would overwrite it before
/// `user_return`'s `mv sp, a0` takes effect. Inlining makes the caller tail-jump to `user_return`
/// with no push.
///
/// # Safety
/// `frame` must be a correctly-built, writable `TrapFrame` at the top of the current thread's kernel
/// stack, with the user address space installed.
#[inline(always)]
pub unsafe fn enter_user(frame: *mut TrapFrame) -> ! {
    // SAFETY: the caller's contract; `user_return` never returns.
    unsafe { user_return(frame) }
}

/// Diagnostic stub (the watchdog dump's EL0-PC column): the riscv trap frame is not at a fixed
/// stack offset (it rides below the live sp via sscratch), so return 0 rather than misread it. The
/// aarch64 dump carries the real PC, which is the ISA the FS hang reproduces on.
pub fn user_pc(_stack_top: u64) -> u64 {
    0
}

/// Interrupts routed to a userspace handler (delegated IRQs). Bumped by the trap dispatcher.
pub static ROUTED_IRQS: AtomicUsize = AtomicUsize::new(0);

/// Interrupts taken with no source enabled to explain them (should stay zero until the timer/PLIC
/// steps enable real sources).
pub static SPURIOUS_IRQS: AtomicUsize = AtomicUsize::new(0);

/// System calls served (`ecall` from U-mode). Read by the boot tour; bumped by the trap dispatcher.
pub static SVC_COUNT: AtomicUsize = AtomicUsize::new(0);

/// User faults taken (a page fault or illegal instruction from U-mode). Read by the boot tour;
/// bumped by the trap dispatcher.
pub static USER_FAULTS: AtomicUsize = AtomicUsize::new(0);

/// Breakpoints (`ebreak`) caught. Exists so a test can prove the trap round-trip actually ran,
/// rather than proving only that we did not crash. The aarch64 analog is its own `BRK_COUNT`.
pub static BRK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// The `sstatus.SPP` bit: the privilege the trap came from (0 = U-mode, 1 = S-mode).
const SPP: u64 = 1 << 8;
/// The high bit of `scause`: set for an interrupt, clear for an exception.
const INTERRUPT: u64 = 1 << 63;
/// `scause` interrupt code for a supervisor external interrupt: a device raised a line into the
/// PLIC, which routed it here. The RISC-V analog of an aarch64 IRQ exception carrying a GIC intid.
const S_EXTERNAL: u32 = 9;
/// `scause` interrupt code for a supervisor software interrupt: another hart's reschedule IPI, set
/// through the SBI (which pends `sip.SSIP`). The RISC-V analog of aarch64's reschedule SGI.
const S_SOFTWARE: u32 = 1;

/// Clear this hart's pending software interrupt (`sip.SSIP`, bit 1). The IPI stays pending until
/// acknowledged, so the handler clears it or it would re-fire the instant interrupts reopen.
fn clear_software_interrupt() {
    const SSIP: u64 = 1 << 1;
    // SAFETY: clears a `sip` bit; no memory effect. `csrc` is atomic read-clear.
    unsafe { core::arch::asm!("csrc sip, {}", in(reg) SSIP, options(nomem, nostack)) };
}

/// Unmask this hart's software interrupts (`sie.SSIE`, bit 1): the reschedule-IPI source. Armed per
/// hart when it becomes a scheduler participant, alongside the timer's `sie.STIE`. Without it a
/// reschedule IPI sets `sip.SSIP` but is never taken, so a migrated thread would sit in the inbox
/// until the target's next timer tick.
pub fn enable_software_interrupts() {
    const SSIE: u64 = 1 << 1;
    // SAFETY: sets a `sie` bit; takes effect under sstatus.SIE. No memory effect.
    unsafe {
        core::arch::asm!("csrs sie, {}", in(reg) SSIE, options(nomem, nostack, preserves_flags))
    };
}
/// `scause` exception code for `ecall` taken from U-mode: the syscall.
const CAUSE_ECALL_U: u64 = 8;
/// `scause` exception code for a breakpoint (`ebreak`).
const CAUSE_BREAKPOINT: u64 = 3;
/// `scause` exception code for a page fault on an instruction fetch.
const CAUSE_INSTRUCTION_PAGE_FAULT: u64 = 12;
/// `scause` exception code for a page fault on a load.
const CAUSE_LOAD_PAGE_FAULT: u64 = 13;
/// `scause` exception code for a page fault on a store or an atomic.
const CAUSE_STORE_PAGE_FAULT: u64 = 15;

/// The most recent user fault, [`UserFault::encode`]d, and the address it named (`stval`). Read
/// back through [`last_user_fault`]; the aarch64 twin is `aarch64::exceptions::last_user_fault`.
static LAST_USER_FAULT: AtomicU64 = AtomicU64::new(0);
static LAST_USER_FAULT_ADDR: AtomicU64 = AtomicU64::new(0);

/// The last user fault's kind and the address it named, or `None` if no user thread has faulted
/// yet.
///
/// **This is new on RISC-V, and it is the reason the fault tests can run here at all.** Until it
/// existed the kernel recorded that a user thread faulted and threw away everything about the
/// fault, so a test on this ISA could assert "something died" and nothing more. See [`classify`]
/// for what "kind" costs here that it does not cost on aarch64.
#[cfg_attr(not(test), allow(dead_code))]
pub fn last_user_fault() -> Option<(UserFault, u64)> {
    // Pairs with the `Release` on `USER_FAULTS` in [`user_fault`], so a caller that has seen the
    // counter rise reads the record the faulting hart wrote rather than something older.
    core::sync::atomic::fence(Ordering::Acquire);
    let kind = UserFault::decode(LAST_USER_FAULT.load(Ordering::Relaxed))?;
    Some((kind, LAST_USER_FAULT_ADDR.load(Ordering::Relaxed)))
}

/// Read a U-mode trap as a [`UserFault`].
///
/// **RISC-V is not told the answer, so this derives it.** `scause` has exactly three page-fault
/// codes, one per access kind, and no field anywhere says *why* the walk refused: a missing leaf
/// and a leaf whose `U` bit is clear produce the identical `scause` 13. aarch64 gets that
/// distinction free in `ESR_EL1`'s fault status code. Here the only source of it is the page table,
/// so we walk it ([`mmu::is_mapped_in_current_space`]) and read "there is a translation" as "the
/// refusal was about permission".
///
/// # BUGS
///
/// The walk happens *after* the fault, not during it, so it is evidence about the tables a few
/// hundred cycles later rather than the hardware's own verdict. If another hart unmapped that page
/// in the gap, a permission fault would be reported as a translation fault (and the reverse for a
/// concurrent map). Nothing in the kernel does that to a live user page today, and the tests that
/// read this record fault a thread that owns the address space alone, but the gap is real and no
/// amount of care here closes it: the architecture did not record the fact at the instant it had
/// it. The honest summary is that aarch64's answer is a measurement and RISC-V's is an inference.
fn classify(frame: &TrapFrame, code: u64) -> UserFault {
    let access = match code {
        CAUSE_INSTRUCTION_PAGE_FAULT => UserFaultAccess::Fetch,
        CAUSE_LOAD_PAGE_FAULT => UserFaultAccess::Read,
        CAUSE_STORE_PAGE_FAULT => UserFaultAccess::Write,
        // An illegal instruction, an `ebreak`, a misaligned access: not a memory-permission
        // question, so neither "permission" nor "translation" is a true thing to say about it.
        _ => return UserFault::Other,
    };

    if super::mmu::is_mapped_in_current_space(frame.stval) {
        UserFault::Permission(access)
    } else {
        UserFault::Translation(access)
    }
}

/// Unmask supervisor external interrupts (`sie.SEIE`, bit 9): the PLIC's deliveries. The caller must
/// have `sstatus.SIE` on (the timer step turned it on) for these to actually be taken, exactly as for
/// the timer's `sie.STIE`. Setting this is what lets a device interrupt reach [`riscv_trap_dispatch`].
pub fn enable_external() {
    const SEIE: u64 = 1 << 9;
    // SAFETY: setting sie.SEIE only unmasks the external-interrupt source; it takes effect under SIE.
    unsafe {
        core::arch::asm!("csrs sie, {}", in(reg) SEIE, options(nomem, nostack, preserves_flags))
    };
}

/// Install the trap vector: `stvec` = [`trap_entry`], direct mode (all traps to one handler; the low
/// two bits of `stvec` select the mode and must be 0, which trap.s's `.balign 4` guarantees).
pub fn init() {
    unsafe extern "C" {
        fn trap_entry();
    }
    let vector = trap_entry as *const () as usize;
    // SAFETY: `vector` is our 4-byte-aligned trap entry; writing stvec has no memory effect.
    unsafe { core::arch::asm!("csrw stvec, {}", in(reg) vector, options(nomem, nostack)) };
}

/// Advance `sepc` past the instruction that trapped: 2 bytes if it is compressed (low two bits not
/// `0b11`), otherwise 4. Used to step over a handled breakpoint. `ecall` is always 4 bytes.
fn advance_past_trapping_insn(frame: &mut TrapFrame) {
    // SAFETY: `sepc` is the address of the instruction that trapped, which is mapped and readable.
    let low = unsafe { core::ptr::read_volatile(frame.sepc as *const u16) };
    frame.sepc += if low & 0b11 == 0b11 { 4 } else { 2 };
}

/// The Rust half of the trap path, called from trap.s with the saved [`TrapFrame`]. Fans out on
/// `scause`: an `ecall` from U-mode is a syscall; a breakpoint is a debug/self-test trap; an
/// interrupt goes to the (not-yet-built) interrupt path; anything else is a fault.
#[unsafe(no_mangle)]
extern "C" fn riscv_trap_dispatch(frame: &mut TrapFrame) {
    let scause = frame.scause;

    if scause & INTERRUPT != 0 {
        let code = scause & 0xff;
        match code as u32 {
            // The S-mode timer: count the tick, rearm, and RECORD that a reschedule is due. We do
            // not switch here; we are mid-handler with a half-saved world. DECISIONS §9 says
            // handlers record and the switch happens at a safe point, which is the deferral below.
            super::timer::TIMER_INTID => {
                super::timer::tick();
                crate::sched::on_tick();
            }
            // A device interrupt, arrived through the PLIC. Claim it (which acknowledges and masks
            // that source), and if it is routed to a userspace driver, deliver it as a message. The
            // shape is exactly aarch64's handle_irq: mask the source, then notify. A level-triggered
            // device holds its line asserted until the driver quiets it, so leaving the source live
            // would re-fire in a storm; we `disable` it at the PLIC and the driver's ACK (or, in the
            // kernel-side demo, an explicit re-enable) brings it back. `complete` ends the PLIC's
            // claim so it will arbitrate the next interrupt. See notes/interrupts.md, drivers/plic.rs.
            S_EXTERNAL => {
                // Claim, mask, and complete against THIS hart's own context. IRQ affinity may have
                // routed the source to a secondary hart's context, and that hart claims from its own
                // context, not the boot hart's (drivers/plic.rs, arch::irq::this_s_context).
                let ctx = super::irq::this_s_context();
                let source = crate::drivers::plic::claim(ctx);
                if source != 0 {
                    if let Some(ep) = crate::sched::irq_route(source) {
                        ROUTED_IRQS.fetch_add(1, Ordering::Relaxed);
                        crate::drivers::plic::disable(source, ctx);
                        crate::sched::irq_notify(ep);
                    } else {
                        SPURIOUS_IRQS.fetch_add(1, Ordering::Relaxed);
                    }
                    crate::drivers::plic::complete(source, ctx);
                }
            }
            // A reschedule IPI from another hart (SBI set our sip.SSIP). Clear the pending bit, then
            // drain our inbox: another hart handed us a thread and poked us, and drain_inbox moves it
            // onto our own run queue and sets need_resched, so the deferral below switches to it.
            // The RISC-V twin of aarch64's RESCHED_SGI case. See sched::place_on / drain_inbox.
            S_SOFTWARE => {
                clear_software_interrupt();
                // Two reasons a hart pokes us (DECISIONS §28): it handed us a thread (drain it), or
                // an idle hart asked us for work (serve the steal). The same IPI carries both.
                crate::sched::drain_inbox();
                crate::sched::serve_steal_request();
            }
            _ => {
                SPURIOUS_IRQS.fetch_add(1, Ordering::Relaxed);
            }
        }

        // --- and here is preemption, the same four lines as aarch64 (see that file's handle_irq).
        // If a tick asked for a reschedule and the scheduler is up, switch away now. `schedule()`
        // saves this thread's kernel context and may not return until this thread is picked again;
        // when it does, we fall through to `trap_return`, which restores `frame` and `sret`s back to
        // exactly the instruction the timer interrupted. A thread that never yields is preempted
        // anyway, which is the whole point (DECISIONS §5). It works for a U-mode thread and an
        // S-mode kernel thread alike, because trap.s saved a full frame for whichever we interrupted.
        if crate::sched::take_need_resched() && crate::sched::is_running() {
            crate::sched::count_preemption();
            crate::sched::schedule();
        }
        return;
    }

    let code = scause & 0xff;
    let from_user = frame.sstatus & SPP == 0;

    match code {
        CAUSE_ECALL_U => {
            // The syscall. `sepc` points AT the `ecall` (unlike aarch64, where the hardware advances
            // ELR past `svc`), so step over it before dispatching, and `ecall` is always 4 bytes.
            SVC_COUNT.fetch_add(1, Ordering::Relaxed);
            frame.sepc += 4;
            crate::syscall::dispatch(frame);
        }
        // A breakpoint from S-mode is the trap self-test: count it and step over. From U-mode it
        // falls through to `user_fault` below, because every userspace panic handler ends in
        // `ebreak` *expecting to die* ("a driver bug is a dead driver"); stepping over it would
        // resume a program that just declared itself broken.
        CAUSE_BREAKPOINT if !from_user => {
            BRK_COUNT.fetch_add(1, Ordering::Relaxed);
            advance_past_trapping_insn(frame);
        }
        // A user thread did something illegal (or `ebreak`ed on purpose). It dies; the kernel
        // does not. Same promise as aarch64's `user_fault`, kept on the second ISA.
        _ if from_user => user_fault(frame, scause, code),
        _ => {
            // An exception from S-mode is a KERNEL bug, and fatal. Report it with the detail
            // that makes it legible.
            panic!(
                "unexpected RISC-V trap: scause={scause:#x} (code {code}) stval={:#x} sepc={:#x} \
                 from_user={from_user}",
                frame.stval, frame.sepc,
            );
        }
    }
}

/// A user thread did something it is not allowed to do. Kill it; keep the machine.
///
/// The mechanism is aarch64's `user_fault`, verbatim in spirit: we are in the trap handler on the
/// faulting thread's kernel stack, `sched::exit()` marks it Finished and schedules away forever,
/// and the reaper frees the stack from the next thread. The blocking-syscall path (`ipc_recv`)
/// already schedules away from this exact context, so nothing here is novel.
///
/// **Correction, on the record.** Until milestone 32 this arm panicked the whole kernel, behind a
/// comment claiming no user thread could run on RISC-V, which had been false since parity (user
/// threads shipped with milestone 20 and the parity workstreams). Nothing noticed because no riscv
/// test made a user thread fault; the kill-mid-write test is the first, and it flushed this out.
/// The stale comment survived two workstreams past the decision that obsoleted it, which is
/// exactly the "a TODO that outlives its decision becomes misinformation" failure notes/teardown.md
/// documents.
fn user_fault(frame: &TrapFrame, scause: u64, code: u64) -> ! {
    // Classify BEFORE anything else: the walk reads the address space this thread is still
    // installed on, and `sched::fault` below is where that stops being true.
    //
    // **The record first, the counter last, and the counter's store is the release.** Every test
    // that reads this record finds it by watching `USER_FAULTS` rise and then calling
    // [`last_user_fault`], so the counter is the publication flag for the record and must be
    // written after it. It was written first on both ISAs, which is a race a passing test cannot
    // see: the reader gets either an earlier fault's record (an assertion satisfied by the wrong
    // evidence) or, on the boot's first fault, a zero that reads as "nothing faulted". The aarch64
    // twin carries the same fix.
    LAST_USER_FAULT_ADDR.store(frame.stval, Ordering::Relaxed);
    LAST_USER_FAULT.store(classify(frame, code).encode(), Ordering::Relaxed);
    USER_FAULTS.fetch_add(1, Ordering::Release);

    crate::println!();
    crate::println!(
        "  user thread {} killed: scause {:#x} (code {})",
        crate::sched::current(),
        scause,
        code,
    );
    crate::println!(
        "    pc {:#018x}   stval {:#018x}   user sp {:#018x}",
        frame.sepc,
        frame.stval,
        frame.x[2],
    );
    crate::println!("  the kernel is fine.");

    // Deliver the fault to a supervisor if this thread had one, and become a corpse; otherwise the
    // unsupervised path (Finished, reaped by the next thread), exactly as `exit`. DECISIONS §26,
    // sched::depart. `frame.sepc` is the faulting pc, `frame.stval` the faulting address.
    crate::sched::fault(frame.sepc, frame.stval);
}

/// Prove the trap path works end to end: execute a breakpoint and return. If traps are wired,
/// [`riscv_trap_dispatch`] catches `scause` = breakpoint, [`advance_past_trapping_insn`] steps `sepc`
/// past the `ebreak`, and `sret` lands us right back here. If they are not, this never returns.
/// Returns the breakpoint count so the caller can confirm the handler actually ran.
pub fn self_test() -> usize {
    let before = BRK_COUNT.load(Ordering::Relaxed);
    // SAFETY: `ebreak` raises a breakpoint the dispatcher handles; it has no other effect.
    unsafe { core::arch::asm!("ebreak") };
    BRK_COUNT.load(Ordering::Relaxed) - before
}

#[cfg(test)]
mod tests {
    //! Tests for trap handling.
    //!
    //! `registers_survive_a_trap` is the load-bearing one. The [`TrapFrame`](super::TrapFrame)
    //! layout is a contract with trap.s that the compiler cannot check, and a wrong offset would
    //! scramble a register while still `sret`ing happily to the right address, corrupting a caller's
    //! state and blaming innocent code thousands of instructions later.

    /// The trap vector is installed, in direct mode, at an address the hardware will accept.
    ///
    /// `stvec` is not a plain pointer: its low two bits are the MODE field (0 = direct, all traps to
    /// one handler; 1 = vectored, interrupts to `base + 4*cause`). So the base must be 4-byte
    /// aligned or its low bits *are* a mode selector, and writing a 2-byte-aligned entry point would
    /// silently select vectored mode and send every interrupt to a wrong address. trap.s's
    /// `.balign 4` is what makes that impossible, and this is the assertion that says so.
    ///
    /// The aarch64 twin checks `VBAR_EL1`'s 2048-byte alignment, which is the same class of rule
    /// (the hardware assumes low bits are zero) at a very different scale, because that ISA's vector
    /// is a 16-slot table and this one is a single entry point.
    #[test_case]
    fn stvec_points_at_our_trap_entry() {
        unsafe extern "C" {
            fn trap_entry();
        }
        let expected = trap_entry as *const () as u64;

        let stvec: u64;
        // SAFETY: reads a CSR. No side effects.
        unsafe { core::arch::asm!("csrr {}, stvec", out(reg) stvec, options(nomem, nostack)) };

        assert_eq!(
            stvec & !0b11,
            expected,
            "stvec does not point at trap_entry"
        );
        assert_eq!(stvec & 0b11, 0, "stvec is not in direct mode: {stvec:#x}");
        assert_eq!(expected % 4, 0, "trap entry misaligned: {expected:#x}");
    }

    /// The real one: take a trap and come back from it.
    ///
    /// `ebreak` raises a synchronous breakpoint. To reach the line after it, every piece of the
    /// RISC-V trap path has to be right: `stvec` points at trap.s, the entry recovers the kernel
    /// `tp` and stack through `sscratch`, it writes a frame matching `TrapFrame`, the dispatcher
    /// decodes `scause` and recognizes the breakpoint cause, it advances `sepc` past the instruction
    /// (which the hardware does NOT do for us, unlike `ecall`... which it also does not do, unlike
    /// aarch64's `svc`), the restore path puts the machine back, and `sret` returns to exactly the
    /// right address. Get any of that wrong and you do not get a failing assertion; you get an
    /// infinite loop on the `ebreak`, or a crash. So arriving here at all is most of the test.
    ///
    /// **It is also the S-mode witness, which is why this ISA needs no `running_at_el1`.** aarch64
    /// reads `CurrentEL` and checks it is 1; RISC-V deliberately gives S-mode no way to read its own
    /// privilege. It does not need one here. The breakpoint arm is guarded by `!from_user`
    /// (`sstatus.SPP == 1`), so `BRK_COUNT` cannot move unless the trap came from S-mode. And it
    /// could not have been taken at all in M-mode, where `mtvec` (OpenSBI's) owns the trap and our
    /// handler never runs. A count that went up is a machine executing in S-mode.
    ///
    /// The `ebreak` also exercises the compressed-instruction case: with the C extension the
    /// assembler emits the 2-byte `c.ebreak`, so `advance_past_trapping_insn` must read the opcode
    /// and step 2 rather than 4. Stepping 4 would resume in the middle of the next instruction.
    #[test_case]
    fn breakpoint_is_caught_and_execution_resumes() {
        use crate::arch::exceptions::BRK_COUNT;
        use core::sync::atomic::Ordering;

        let before = BRK_COUNT.load(Ordering::Relaxed);

        // SAFETY: this deliberately traps. We handle it.
        unsafe { core::arch::asm!("ebreak") };

        assert_eq!(
            BRK_COUNT.load(Ordering::Relaxed),
            before + 1,
            "the handler didn't run, but we resumed anyway?"
        );
    }

    /// Proves the trap frame actually round-trips a register.
    ///
    /// The previous test proves we *return*. This proves we return with the machine intact, which is
    /// a different claim. Put a known value in a callee-saved register, take a trap, read it back.
    ///
    /// A bug in trap.s's save/restore (a wrong offset, a swapped pair) would scramble registers
    /// while still `sret`ing perfectly happily to the right address. That is the nastiest possible
    /// failure: it corrupts a caller's state and blames a completely innocent piece of code
    /// thousands of instructions later. This is the test that catches it.
    ///
    /// `s2` (`x18`) is the register under test: callee-saved, so the compiler is told we clobber it,
    /// and far enough into the file that an off-by-one in the frame layout lands on it.
    #[test_case]
    fn registers_survive_a_trap() {
        let sent: u64 = 0xdead_beef_cafe_f00d;
        let got: u64;

        // SAFETY: deliberately traps; we handle it. s2 is callee-saved, so we declare the clobber.
        unsafe {
            core::arch::asm!(
                "mv s2, {sent}",
                "ebreak",
                "mv {got}, s2",
                sent = in(reg) sent,
                got = out(reg) got,
                out("s2") _,
            );
        }

        assert_eq!(got, sent, "the trap frame scrambled a register");
    }
}
