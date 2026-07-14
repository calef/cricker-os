//! Userspace. EL0. The actual operating system boundary.
//!
//! Everything before this was a Rust program that boots. From here on, the machine runs code
//! that **we did not compile and do not trust**, and the kernel's job stops being "do things"
//! and starts being "decide what is allowed."
//!
//! # Entering EL0 is returning from an exception that never happened
//!
//! There is no "drop to EL0" instruction. There is only `eret`, which restores whatever
//! `SPSR_EL1` says and jumps to `ELR_EL1`, and the exception level to return to is *in*
//! `SPSR_EL1`. So we do not need a new way down. We need a **fake way back**: fabricate a
//! [`TrapFrame`] with `SPSR = EL0t`, point `sp` at it, and fall into the `exception_restore`
//! that milestone 2 already wrote.
//!
//! This is the second time the project has pulled exactly this trick. `Thread::spawn` fakes a
//! `switch_to` frame so that the `ret` which *resumes* a thread also *starts* one
//! (notes/threads.md). Both times the "start" path turned out to be the "resume" path with a
//! forged frame, and no new code at all.
//!
//! # What milestone 4 already paid for
//!
//! The kernel lives entirely in `TTBR1`, at `0xffff_...`. Userspace lives in `TTBR0`, at
//! `0x0000_...`. **The hardware picks the table register from bits 63:48 of the address**, so:
//!
//! - The kernel is mapped in every address space, for free. Nobody had to copy anything.
//! - A syscall **does not switch page tables**. There is nothing to flush and nothing to remap.
//! - Installing a process is one `msr ttbr0_el1`.
//!
//! None of that was written for milestone 7. It fell out of a higher-half decision made three
//! milestones ago, and `Flags::user_code()` / `Flags::user_data()` have been sitting in the
//! `paging` crate, unused, waiting for today.
//!
//! # What is deliberately NOT here
//!
//! **A syscall ABI.** The user program below executes `svc #0` and asks for nothing. There is
//! no syscall number, no argument convention, no return value. DECISIONS §10 chose
//! capabilities, and the syscall surface gets designed against a capability table at 7d, in one
//! piece, on purpose. Not accreted here because it was convenient.

use crate::arch::exceptions::TrapFrame;
use crate::arch::mmu::{self, phys_to_ptr};
use crate::memory;
use alloc::vec::Vec;
use frames::{FRAME_SIZE, Frame};
use paging::{Flags, Half, MapError, Mapper};

/// Where a user program's code goes. Low half, so the hardware walks `TTBR0`.
pub const USER_CODE_VA: u64 = 0x0000_0000_0040_0000;

/// Where its stack goes. One page, and `sp` starts at the top of it: stacks grow down.
pub const USER_STACK_VA: u64 = 0x0000_0000_0050_0000;
pub const USER_STACK_TOP: u64 = USER_STACK_VA + FRAME_SIZE;

/// `SPSR_EL1` for "return to EL0, AArch64, interrupts on."
///
/// - `M[4] = 0`: AArch64, not AArch32.
/// - `M[3:0] = 0b0000`: **EL0t**. The `t` means SP_EL0, which is the only stack pointer EL0
///   has. There is no EL0h.
/// - `DAIF = 0`: Debug, SError, IRQ and FIQ all **unmasked**.
///
/// So the value is zero, which looks like a bug and is not. It is worth spelling out because
/// the DAIF bits are the interesting part: **IRQs are on the moment we land in EL0.** If they
/// were masked, a user program in a tight loop could never be preempted and the machine would
/// be gone, which is the exact failure DECISIONS §5 spent a milestone refusing to accept.
const SPSR_EL0T: u64 = 0;

unsafe extern "C" {
    /// `mov sp, x0` then fall into `exception_restore`. Two instructions. See vectors.s.
    fn enter_userspace(frame: *mut TrapFrame) -> !;
}

/// A user address space: an L0 table for `TTBR0`, and every frame that hangs off it.
///
/// The `frames` vec holds **both** the pages we mapped and the intermediate page tables the
/// mapper allocated to reach them, because the allocator we hand the `Mapper` records
/// everything it hands out. That is the fix for the leak milestone 6 found the hard way
/// (`unmap_page` frees a leaf and leaves its L1/L2/L3 standing), applied *before* it bites:
/// an address space dies all at once, so we do not need `unmap` at all. We free the frames and
/// throw the whole table away.
pub struct AddressSpace {
    root: Frame,
    frames: Vec<Frame>,
}

impl AddressSpace {
    pub fn new() -> Option<Self> {
        let root = memory::alloc()?;

        // SAFETY: a fresh frame, reachable through the direct map, and nothing walks it until
        // `activate`. Zero it first: a page table full of whatever was in RAM is a set of
        // pointers to nowhere, followed at speed by the hardware.
        unsafe {
            (*phys_to_ptr(root.addr())).entries = [0; paging::ENTRIES];
        }

        Some(AddressSpace {
            root,
            frames: Vec::new(),
        })
    }

    /// Map one fresh, zeroed page at `va`, and hand back a **kernel** view of it.
    ///
    /// The returned slice is at `pa | KERNEL_VA_BASE` (the direct map), because the kernel
    /// cannot address `va` itself: `va` is a *low* address and means something entirely
    /// different from EL1's point of view. Two names for one frame, which is what the direct
    /// map is for.
    pub fn map_new(&mut self, va: u64, flags: Flags) -> Result<&'static mut [u8], MapError> {
        let frame = memory::alloc().ok_or(MapError::OutOfFrames)?;
        self.frames.push(frame);

        let root = self.root.addr();
        let recorded = &mut self.frames;

        // SAFETY: `root` is a zeroed L0 table. Half::Low, so the mapper refuses a high address:
        // mapping the kernel's half into TTBR0 would build a translation the hardware never
        // consults, and we would chase the ghost for hours.
        let mut mapper = unsafe {
            Mapper::new(
                root,
                Half::Low,
                || {
                    let f = memory::alloc()?;
                    recorded.push(f); // <- intermediate tables get freed too
                    Some(f.addr())
                },
                phys_to_ptr,
            )
        };

        mapper.map(va, frame.addr(), flags)?;

        // SAFETY: the frame is ours, freshly allocated, and the direct map is valid for it.
        // 'static is a lie we tell for convenience and then keep: the frame outlives every use
        // of this slice, because `Drop` is the only thing that frees it.
        let page = unsafe {
            core::slice::from_raw_parts_mut(
                mmu::phys_to_virt(frame.addr()) as *mut u8,
                FRAME_SIZE as usize,
            )
        };
        page.fill(0);
        Ok(page)
    }

    /// The physical address of the L0 table. What goes in `TTBR0_EL1`.
    pub fn root(&self) -> u64 {
        self.root.addr()
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        // If we are the live address space, stop being it BEFORE the frames go back on the free
        // list. Otherwise the TTBR0 the CPU is walking points at memory the allocator has
        // already handed to somebody else, and the next low-half access reads whatever they put
        // there. (`deactivate_user` flushes the TLB, which is the other half of the same
        // problem: without it the stale translations survive the table.)
        if mmu::current_user_root() == self.root.addr() {
            mmu::deactivate_user();
        }

        for frame in self.frames.drain(..) {
            memory::free(frame);
        }
        memory::free(self.root);
    }
}

/// Load a program, and become it. Never returns.
///
/// # Safety
/// `program` must be position-independent aarch64 machine code that begins at its first byte.
pub unsafe fn exec(program: &[u8]) -> ! {
    assert!(
        program.len() as u64 <= FRAME_SIZE,
        "the 7a loader is one page. An ELF loader is 7c."
    );

    let mut space = AddressSpace::new().expect("no memory for a user address space");

    let code = space
        .map_new(USER_CODE_VA, Flags::user_code())
        .expect("could not map the user's code");
    code[..program.len()].copy_from_slice(program);

    space
        .map_new(USER_STACK_VA, Flags::user_data())
        .expect("could not map the user's stack");

    // The code page is `user_code()`: readable and executable by EL0, and **PXN**, so the
    // kernel cannot execute it even by accident. A bug that jumped EL1 into a user page would
    // otherwise run user-controlled instructions at EL1, which is a total compromise. That is
    // one bit in a page descriptor, set by a decision made at milestone 4.
    //
    // The instructions we just wrote went out through the DATA path. The instruction fetcher
    // has its own cache and has never heard of them. On aarch64 the I-cache is not coherent
    // with the D-cache, so without this the CPU can fetch whatever was in that frame *before*
    // we wrote the program into it.
    sync_icache(code.as_ptr() as u64, program.len());

    // GIVE THE ADDRESS SPACE TO THE THREAD, and this is not bookkeeping.
    //
    // `TTBR0_EL1` is one register, and it is **global**. Threads are not. The first version of
    // this code activated the address space here and left it, and the result was a bug worth
    // the whole afternoon: a user thread was still spinning at EL0 when the *next* `exec`
    // installed a different TTBR0. It kept running, at EL0, in **somebody else's address
    // space**, where its own code page was a page of zeroes. It died executing them.
    //
    // An address space is a property of a thread, so the context switch has to carry it, the
    // same way it carries a stack and a register file. `sched::schedule` now installs the
    // incoming thread's root (or the empty reserved table, for a kernel thread) before it
    // switches. See notes/userspace.md.
    //
    // And ownership does the rest for free: the `Thread` owns the `AddressSpace`, so when the
    // reaper drops a dead thread it unmaps and frees the whole low half, exactly as it already
    // does for the `KernelStack`. There is nothing to leak.
    crate::sched::adopt_address_space(space);

    // THE TRAPFRAME IS NOT AN ORDINARY LOCAL, and this cost us an afternoon.
    //
    // It must sit at the TOP OF THIS THREAD'S KERNEL STACK, because that is where the hardware
    // will look for it. `enter_userspace` does `mov sp, x0`, and `exception_restore` leaves
    // SP_EL1 = x0 + 272 across the `eret`. So when the user traps back in, `SAVE_CONTEXT`
    // subtracts 272 and rebuilds the frame **at exactly this address**. It had better be
    // writable, and it had better be a stack.
    //
    // The first version wrote `enter_userspace(&TrapFrame { .. })`, and every field of that
    // struct is a compile-time constant, so Rust CONST-PROMOTED IT INTO .rodata. The kernel
    // set SP_EL1 to read-only memory, and the user's first `svc` faulted trying to write its
    // own trap frame there. See notes/userspace.md: the kernel then walked `sp` DOWNWARD
    // through .rodata and the whole of .text, 272 bytes and one fault at a time, until it fell
    // out of the bottom of the image into writable RAM and could finally tell us.
    let top = crate::sched::current_kernel_stack_top()
        .expect("a user thread needs a kernel stack of its own to be trapped onto");

    let frame = (top - size_of::<TrapFrame>() as u64) as *mut TrapFrame;

    // And prove it, rather than trusting the reasoning above. This is one check, once per
    // exec, against a bug whose symptom is a nested fault storm that eats the kernel image.
    assert!(
        mmu::translate(frame as u64).is_some_and(|(_, f)| f.is_writable()),
        "the user's TrapFrame at {frame:p} is not in writable memory",
    );

    // SAFETY: `frame` is 16-byte-aligned writable kernel stack (a KernelStack top is page
    // aligned and TrapFrame is 272, a multiple of 16), EL0's code and stack are mapped, and
    // TTBR0 is installed.
    unsafe {
        frame.write(TrapFrame {
            x: [0; 31],
            elr: USER_CODE_VA,      // ...where `eret` jumps
            spsr: SPSR_EL0T,        // ...and the exception level it jumps to
            sp_el0: USER_STACK_TOP, // ...on the stack it will jump onto
        });

        enter_userspace(frame)
    }
}

/// Make the instruction fetcher aware of code we just wrote as data.
///
/// The D-cache and the I-cache are **not coherent** on aarch64. This is not a QEMU quirk, it is
/// the architecture: the assumption is that writing code is rare and paying for coherence on
/// every store is not worth it. So the loader has to say so explicitly, and every loader on
/// every ARM machine does exactly this.
///
/// `dc cvau` cleans the data cache to the point of unification, `ic ivau` invalidates the
/// instruction cache, and the barriers make the two agree. Get it wrong and the CPU executes
/// whatever was in that frame *before* the program landed there, which is an extremely
/// entertaining bug.
fn sync_icache(va: u64, len: usize) {
    const LINE: u64 = 64; // conservative: the real size is in CTR_EL0

    let mut p = va & !(LINE - 1);
    let end = va + len as u64;

    // SAFETY: cache maintenance on a mapped, readable range is always sound.
    unsafe {
        while p < end {
            core::arch::asm!("dc cvau, {p}", p = in(reg) p, options(nostack));
            p += LINE;
        }
        core::arch::asm!("dsb ish", options(nostack));

        let mut p = va & !(LINE - 1);
        while p < end {
            core::arch::asm!("ic ivau, {p}", p = in(reg) p, options(nostack));
            p += LINE;
        }
        core::arch::asm!("dsb ish", "isb", options(nostack));
    }
}

// --- the programs ---
//
// Hand-written aarch64, assembled into `.rodata` and copied into a user page at load time.
// There is no ELF loader yet (that is 7c) and no filesystem to load from (that is milestone 9),
// so the "binary" rides along inside the kernel image. Honest scaffolding, and it goes away.

core::arch::global_asm!(
    r#"
.section .rodata.user_programs, "a"
.balign 4

// Go to EL0, come back, go again. Proves the round trip, not just the departure.
.global USER_HELLO_START
USER_HELLO_START:
    mov     x0,  #42
    svc     #0                  // -> EL1, vector slot 8, ESR.EC = 0x15
    mov     x0,  #43            // if we reach here, `eret` PUT US BACK at EL0
    svc     #0
1:  b       1b                  // and now spin, so the timer can preempt us
.global USER_HELLO_END
USER_HELLO_END:

// A hostile program. It yields nothing, calls nothing, and asks for nothing.
//
// This is DECISIONS §5's arbitrary ELF binary, in the flesh: "it has its own stack, it never
// yields, and it will loop forever because we will write a bug." The ONLY thing in the universe
// that can take the CPU back is a timer interrupt landing between these two instructions.
.global USER_SPIN_START
USER_SPIN_START:
1:  add     x0,  x0,  #1
    b       1b
.global USER_SPIN_END
USER_SPIN_END:

// An outlaw. It reaches for a KERNEL address.
//
// 0xffff_0000_4008_0000 is in the direct map, and it IS mapped, and it IS readable. Just not by
// EL0: `Flags::kernel_data()` sets AP such that EL1 may read and write and EL0 may do neither.
//
// So this is not a translation fault (there is a translation), it is a PERMISSION fault, and
// that distinction is the entire privilege boundary. The hardware picks TTBR1 from bits 63:48,
// walks the kernel's own tables, finds the page, reads the AP bits, and says no.
.global USER_OUTLAW_START
USER_OUTLAW_START:
    movz    x0,  #0x4008, lsl #16
    movk    x0,  #0xffff, lsl #48       // x0 = 0xffff_0000_4008_0000
    ldr     x1,  [x0]                   // <- data abort, EC 0x24, from a lower EL
1:  b       1b                          // never reached
.global USER_OUTLAW_END
USER_OUTLAW_END:
"#
);

macro_rules! user_program {
    ($name:ident, $start:ident, $end:ident) => {
        pub fn $name() -> &'static [u8] {
            unsafe extern "C" {
                static $start: u8;
                static $end: u8;
            }
            let start = (&raw const $start) as usize;
            let end = (&raw const $end) as usize;

            // SAFETY: both symbols are in .rodata, in this image, and the assembler emitted
            // them in this order.
            unsafe { core::slice::from_raw_parts(start as *const u8, end - start) }
        }
    };
}

user_program!(hello, USER_HELLO_START, USER_HELLO_END);
user_program!(spin, USER_SPIN_START, USER_SPIN_END);
user_program!(outlaw, USER_OUTLAW_START, USER_OUTLAW_END);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::exceptions::{
        LAST_USER_FAULT_ESR, LAST_USER_FAULT_FAR, SVC_COUNT, USER_FAULTS,
    };
    use crate::arch::timer;
    use crate::sched;
    use core::sync::atomic::Ordering;

    /// Spin the scheduler until `done()`, or give up. Returns whether it happened.
    fn wait_for(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..2000 {
            if done() {
                return true;
            }
            sched::yield_now();
        }
        done()
    }

    /// **We are running on `SP_EL1`, and the whole trap frame depends on it.**
    ///
    /// At EL1 the name `sp` means `SP_EL1` if `SPSel.SP == 1`, and `SP_EL0` if it is 0. Every
    /// `SAVE_CONTEXT` in the kernel does `sub sp, sp, #272` and every user entry does
    /// `msr sp_el0, x3`. If `SPSel` were 0, those two would be **the same register**, and the
    /// kernel would restore a user stack pointer straight into its own stack pointer.
    ///
    /// This has been true since boot.s and we never checked it. A test that can only fail if
    /// the world is upside down is still worth having when the failure is silent.
    #[test_case]
    fn el1_runs_on_sp_el1() {
        let spsel: u64;
        // SAFETY: reading SPSel has no side effects.
        unsafe { core::arch::asm!("mrs {}, spsel", out(reg) spsel, options(nostack, nomem)) };

        assert_eq!(
            spsel & 1,
            1,
            "SPSel says EL1 is using SP_EL0: the trap frame's sp_el0 field aliases the \
             kernel's own stack pointer"
        );
    }

    /// EL0. The boundary.
    ///
    /// Two `svc`s, not one, and that is the point: the second can only happen if the `eret`
    /// **put us back at EL0** after the first. One `svc` proves we left. Two prove we came back.
    #[test_case]
    fn a_user_program_reaches_el0_and_returns_twice() {
        let before = SVC_COUNT.load(Ordering::Relaxed);

        sched::spawn(|| unsafe { exec(hello()) }).expect("spawn failed");

        assert!(
            wait_for(|| SVC_COUNT.load(Ordering::Relaxed) >= before + 2),
            "saw {} svc from EL0, wanted 2",
            SVC_COUNT.load(Ordering::Relaxed) - before,
        );
    }

    /// **The privilege boundary is real, and it is a PERMISSION fault, not a missing page.**
    ///
    /// The address the user reaches for is mapped, and readable, and the kernel reads it all
    /// day. The hardware picks `TTBR1` from bits 63:48, walks the kernel's own tables, finds
    /// the page, reads the `AP` bits, and says no.
    ///
    /// So `DFSC = 0b001111` (permission fault) rather than a translation fault is the whole
    /// assertion. A translation fault would mean we had merely failed to map something, which
    /// would pass a sloppier test and prove nothing at all.
    #[test_case]
    fn a_user_program_cannot_read_a_kernel_address() {
        const KERNEL_ADDR: u64 = 0xffff_0000_4008_0000;

        let before = USER_FAULTS.load(Ordering::Relaxed);

        sched::spawn(|| unsafe { exec(outlaw()) }).expect("spawn failed");

        assert!(
            wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > before),
            "the user program read a kernel address and was NOT stopped",
        );

        let esr = LAST_USER_FAULT_ESR.load(Ordering::Relaxed);
        let far = LAST_USER_FAULT_FAR.load(Ordering::Relaxed);

        assert_eq!((esr >> 26) & 0x3f, 0x24, "not a data abort from a lower EL");
        assert_eq!(esr & 0x3f, 0x0f, "not a PERMISSION fault: esr {esr:#x}");
        assert_eq!(esr & (1 << 6), 0, "not a read");
        assert_eq!(far, KERNEL_ADDR, "faulted on the wrong address");

        // And the kernel is executing this line, which is the other half of the claim.
    }

    /// DECISIONS §5's arbitrary ELF binary, at EL0, in the flesh.
    ///
    /// A program with no yield, no syscall, and not even a function call. The **only** thing in
    /// the universe that can take the CPU back from it is a timer interrupt landing between two
    /// of its instructions. Milestone 6 proved this for a kernel thread we compiled. This is the
    /// case that actually mattered.
    #[test_case]
    fn a_user_program_that_never_yields_is_preempted_anyway() {
        let preemptions = sched::preemptions();
        let faults = USER_FAULTS.load(Ordering::Relaxed);

        sched::spawn(|| unsafe { exec(spin()) }).expect("spawn failed");

        // Give it the CPU and then take it back, without asking.
        timer::spin_for(timer::frequency() / 10);

        assert!(
            sched::preemptions() > preemptions,
            "nothing was preempted while a user thread spun at EL0",
        );
        assert_eq!(
            USER_FAULTS.load(Ordering::Relaxed),
            faults,
            "the spinning user thread faulted; it was supposed to just spin",
        );

        // And we are here, running, having taken the CPU back from a program that never
        // offered it.
    }

    /// A dead user thread's address space is freed, all of it, including its page tables.
    ///
    /// The milestone 6 reaper test found that stack VAs were bump-allocated and never reused,
    /// because `unmap_page` leaves intermediate tables standing. An `AddressSpace` sidesteps
    /// that entirely: it dies **all at once**, so it never unmaps anything. It records every
    /// frame the mapper hands it, leaves and tables alike, and frees the lot.
    ///
    /// The assertion is exact, not approximate. Approximate would have hidden the milestone 6
    /// bug.
    #[test_case]
    fn a_dead_user_thread_frees_its_whole_address_space() {
        let used = || crate::memory::stats().expect("no allocator").used;

        // Warm up: the first user thread ever created pays for page tables in a region of
        // kernel VA that nothing has touched. Measure the STEADY state, which is the one that
        // has to hold forever.
        sched::spawn(|| unsafe { exec(outlaw()) }).expect("spawn failed");
        let f0 = USER_FAULTS.load(Ordering::Relaxed);
        assert!(wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > f0));
        assert!(wait_for(|| sched::thread_count() <= 2));

        let before = used();

        for _ in 0..4 {
            let f = USER_FAULTS.load(Ordering::Relaxed);
            sched::spawn(|| unsafe { exec(outlaw()) }).expect("spawn failed");
            assert!(wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > f));
            assert!(wait_for(|| sched::thread_count() <= 2));
        }

        assert_eq!(
            used(),
            before,
            "four user address spaces came and went and {} frames did not come back",
            used() as i64 - before as i64,
        );
    }
}
