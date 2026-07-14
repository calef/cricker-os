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
use elf::Elf;
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

/// Why a binary was refused.
///
/// **A bad user program must not be a kernel panic.** Every one of these is a thing a file can
/// simply *say*, and the answer is to decline and kill the thread, not to take the machine down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadError {
    /// The file is not an aarch64 static ELF we are willing to run. See `elf::Error`.
    NotLoadable(elf::Error),

    /// It asked to be loaded somewhere it may not go.
    ///
    /// **Including a KERNEL address.** An ELF gets to name its own load address, so this is
    /// exactly the thing a hostile binary tries: ask to be mapped over the kernel. It is
    /// refused by construction rather than by a check, because the `Mapper` is built with
    /// `Half::Low` and a high address is not a thing it can express (`MapError::WrongHalf`).
    Unmappable(MapError),

}

/// Parse an ELF, build an address space, and put it in memory. Do **not** run it.
///
/// Split out from [`exec_elf`] on purpose: this is the part that can fail, so it is the part a
/// test can call without dying.
pub fn load(image: &[u8]) -> Result<(AddressSpace, u64), LoadError> {
    let elf = Elf::parse(image).map_err(LoadError::NotLoadable)?;

    let mut space = AddressSpace::new().ok_or(LoadError::Unmappable(MapError::OutOfFrames))?;

    for seg in elf.segments() {
        // **Honour what the file asked for, and not one bit more.**
        //
        // `elf` has already refused a segment that is both writable and executable, so these
        // three are the only shapes that reach here, and each maps to exactly one `Flags`
        // constructor. Note that a read-only segment gets `user_rodata`, not `user_data`: a
        // loader that widens permissions is a loader you cannot reason about.
        let flags = if seg.is_executable() {
            Flags::user_code()
        } else if seg.is_writable() {
            Flags::user_data()
        } else {
            Flags::user_rodata()
        };

        let (start, end) = seg.page_range(FRAME_SIZE);

        let mut va = start;
        while va < end {
            // `map_new` hands back a ZEROED page, which is what makes `.bss` free: the bytes
            // between `filesz` and `memsz` are simply the ones we never copy over.
            //
            // Forgetting that is the classic ELF loader bug, and the consequence is a program
            // whose `.bss` holds whoever used that frame last.
            let page = space.map_new(va, flags).map_err(LoadError::Unmappable)?;

            // Which of the file's bytes land in this page? Computed as an intersection rather
            // than assumed, because `p_vaddr` need not be page-aligned.
            let file_lo = seg.vaddr;
            let file_hi = seg.vaddr + seg.data.len() as u64;
            let lo = va.max(file_lo);
            let hi = (va + FRAME_SIZE).min(file_hi);

            if lo < hi {
                let dst = (lo - va) as usize;
                let src = (lo - file_lo) as usize;
                let n = (hi - lo) as usize;
                page[dst..dst + n].copy_from_slice(&seg.data[src..src + n]);
            }

            if seg.is_executable() {
                sync_icache(page.as_ptr() as u64, FRAME_SIZE as usize);
            }

            va += FRAME_SIZE;
        }
    }

    space
        .map_new(USER_STACK_VA, Flags::user_data())
        .map_err(LoadError::Unmappable)?;

    Ok((space, elf.entry()))
}

/// The program QEMU loaded into RAM for us, found via the device tree.
///
/// **The same road Linux's initramfs travels.** Nothing about this binary is known to the kernel
/// at build time: QEMU put a file somewhere in RAM and wrote the address into
/// `/chosen/linux,initrd-start`, and `memory::init` read it there and told the frame allocator
/// to keep its hands off. That reservation was written at milestone 3, for this.
pub fn initrd() -> Option<&'static [u8]> {
    let (start, size) = memory::initrd_region()?;

    // SAFETY: the region came from the device tree, it is inside RAM, the frame allocator has
    // been told it is forbidden, and the direct map names it. Nothing else will ever write here.
    Some(unsafe {
        core::slice::from_raw_parts(mmu::phys_to_virt(start) as *const u8, size as usize)
    })
}

/// Load an ELF and become it. Never returns.
///
/// On a bad binary this kills the calling thread. **It does not panic.** A user program the
/// kernel cannot load is a user program's problem.
pub fn exec_elf(image: &[u8]) -> ! {
    exec_elf_with(image, true)
}

/// Load an ELF, hand it a console (slot 0) and a **SEND capability on `ep`** (slot 1), and run
/// it. This is how a client is wired to a server: the endpoint is the only name it has for the
/// thing on the other end, and `WRITE`-only means it can send and cannot receive.
pub fn exec_elf_with_endpoint(image: &[u8], ep: usize) -> ! {
    exec_elf_inner(image, true, Some(ep))
}

/// Load an ELF and become it, choosing whether to hand it a console.
///
/// **`console: false` is not a test contrivance, it is the demonstration.** A program with no
/// console capability cannot print. Not "is denied when it tries": it holds an empty slot, so
/// there is *nothing to invoke*, and `NoSuchSlot` is the answer. It cannot name the console,
/// because naming a thing you were not handed is not an operation this kernel has.
pub fn exec_elf_with(image: &[u8], console: bool) -> ! {
    exec_elf_inner(image, console, None)
}

fn exec_elf_inner(image: &[u8], console: bool, endpoint: Option<usize>) -> ! {
    let (space, entry) = match load(image) {
        Ok(v) => v,
        Err(e) => {
            crate::println!();
            crate::println!("  refused to load a user program: {e:?}");
            crate::println!("  the kernel is fine.");
            crate::sched::exit();
        }
    };

    crate::sched::adopt_address_space(space);

    // HAND IT ITS WORLD, and note how little that is.
    //
    // Slot 0: a console it may WRITE to, and may **not** GRANT onward. That is the entire set of
    // things this process can name. There is no path it can say, no uid it can be, and no second
    // thing to reach for. A capability system's "environment" is not a variable, it is this.
    if console {
        crate::sched::grant(crate::cap::console_cap()).expect("no free capability slot");
    }
    if let Some(ep) = endpoint {
        // WRITE only: it may SEND and may not RECV. Lands in slot 1, the first free slot after
        // the console.
        crate::sched::grant(crate::cap::endpoint_cap(ep, crate::cap::Rights::WRITE))
            .expect("no free capability slot");
    }

    enter_at(entry)
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


    crate::sched::adopt_address_space(space);

    enter_at(USER_CODE_VA)
}

/// Drop to EL0 at `entry`, on a fresh stack. Never returns.
fn enter_at(entry: u64) -> ! {
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
            elr: entry,             // ...where `eret` jumps
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
    mov     x8,  #1             // SYS_YIELD. Until 7d a bare `svc` meant nothing; now the
    svc     #0                  // syscall number is in x8, and 0 would mean SYS_EXIT.
    mov     x8,  #1
    svc     #0                  // if we reach here, `eret` PUT US BACK at EL0
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
        /// `allow(dead_code)` because 7c handed the demo over to the real ELF from the initrd,
        /// and these hand-written programs are now exercised only by the tests. They stay
        /// because they test things the real binary cannot: `outlaw` deliberately commits a
        /// privilege violation, and `spin` is a program with no `.data`, no stack use, and
        /// nothing but a loop, which is the purest form of DECISIONS §5's hostile binary.
        #[allow(dead_code)]
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
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

    /// Forge an ELF64 header by hand, so a test can ask for something no linker would emit.
    ///
    /// The kernel has `alloc`, so this is thirty lines. It is worth every one of them: the ELF
    /// **names its own load address**, and this is the file that names the kernel's.
    fn forged_elf(vaddr: u64, flags: u32) -> alloc::vec::Vec<u8> {
        const EHDR: usize = 64;
        const PHDR: usize = 56;
        let code: [u8; 16] = [0; 16];

        let mut out = alloc::vec![0u8; EHDR + PHDR + code.len()];
        out[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        out[4] = 2; // ELFCLASS64
        out[5] = 1; // little-endian
        out[6] = 1; // EV_CURRENT
        out[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        out[18..20].copy_from_slice(&183u16.to_le_bytes()); // EM_AARCH64
        out[24..32].copy_from_slice(&vaddr.to_le_bytes()); // e_entry
        out[32..40].copy_from_slice(&(EHDR as u64).to_le_bytes()); // e_phoff
        out[54..56].copy_from_slice(&(PHDR as u16).to_le_bytes());
        out[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum

        let p = EHDR;
        out[p..p + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        out[p + 4..p + 8].copy_from_slice(&flags.to_le_bytes());
        out[p + 8..p + 16].copy_from_slice(&((EHDR + PHDR) as u64).to_le_bytes()); // p_offset
        out[p + 16..p + 24].copy_from_slice(&vaddr.to_le_bytes()); // p_vaddr
        out[p + 32..p + 40].copy_from_slice(&(code.len() as u64).to_le_bytes()); // p_filesz
        out[p + 40..p + 48].copy_from_slice(&(code.len() as u64).to_le_bytes()); // p_memsz
        out
    }

    /// **A binary that asks to be loaded over the kernel.**
    ///
    /// This is the attack. An ELF names its own load address, so a hostile one simply names
    /// `0xffff_0000_4008_0000` and waits to see whether the loader is credulous.
    ///
    /// It is refused **by construction, not by a check we remembered to write**: the user
    /// `Mapper` is built with `Half::Low`, and a high address is not a thing it can express. The
    /// same `WrongHalf` guard has been in `paging` since milestone 4, put there because a *host*
    /// test discovered that bits 63:48 are not translated. It has been waiting for this file.
    #[test_case]
    fn an_elf_that_asks_to_be_loaded_over_the_kernel_is_refused() {
        let image = forged_elf(0xffff_0000_4008_0000, elf::PF_R | elf::PF_X);

        assert_eq!(
            load(&image).err(),
            Some(LoadError::Unmappable(MapError::WrongHalf)),
            "the kernel agreed to map a user program on top of itself",
        );
    }

    /// And a binary asking for a page that is both writable and executable.
    ///
    /// Caught in `crates/elf`, on the host, in microseconds. But assert it end-to-end too: the
    /// value of the host test is that it is fast, not that it is the only line of defence.
    #[test_case]
    fn an_elf_that_asks_for_a_writable_executable_page_is_refused() {
        let image = forged_elf(0x40_0000, elf::PF_R | elf::PF_W | elf::PF_X);

        assert_eq!(
            load(&image).err(),
            Some(LoadError::NotLoadable(elf::Error::WritableAndExecutable)),
        );
    }

    /// Junk is refused, and refusing it does not take the kernel down.
    #[test_case]
    fn a_bad_binary_is_refused_rather_than_panicking() {
        assert!(load(b"#!/bin/sh\necho hi\n").is_err());
        assert!(load(&[]).is_err());
        assert!(load(&[0u8; 4096]).is_err());
        // And we are still executing, which is the assertion.
    }

    /// The initrd is there, and it is the program we built.
    #[test_case]
    fn the_initrd_holds_an_aarch64_executable() {
        let image = initrd().expect("no initrd: was -initrd passed to QEMU?");
        let e = elf::Elf::parse(image).expect("the initrd is not a loadable aarch64 ELF");

        assert_eq!(e.entry(), 0x40_0000, "linked somewhere unexpected");

        // Three segments, and NONE of them writable-and-executable.
        let segs: alloc::vec::Vec<_> = e.segments().collect();
        assert!(segs.len() >= 3, "expected .text, .rodata and .data");
        assert!(segs.iter().any(|s| s.is_executable() && !s.is_writable()));
        assert!(segs.iter().any(|s| s.is_writable() && !s.is_executable()));

        // And one of them has a .bss: memsz > filesz. If this is not true, the test below is
        // vacuous, and we would never know.
        assert!(
            segs.iter().any(|s| s.memsz as usize > s.data.len()),
            "no segment has a .bss, so the zero-fill is untested",
        );
    }

    /// **The whole of 7c.** A separately compiled binary, arriving in the initrd, running at EL0.
    ///
    /// The program checks its own image and speaks with the only two words it has: `svc` if
    /// every expectation about its own memory holds, `brk` if not. **No data crosses the
    /// boundary**, because there is no ABI yet and we are not going to invent one by accident.
    ///
    /// So `svc` and no fault means: `.text` executed, `.rodata` was readable, `.data` was copied
    /// from the file, `.bss` was zeroed (the file does not contain those bytes), and the stack
    /// worked well enough to recurse eight frames.
    #[test_case]
    fn a_real_elf_from_the_initrd_runs_at_el0_and_verifies_itself() {
        let svc = SVC_COUNT.load(Ordering::Relaxed);
        let faults = USER_FAULTS.load(Ordering::Relaxed);

        sched::spawn(|| exec_elf(initrd().expect("no initrd"))).expect("spawn failed");

        assert!(
            wait_for(|| SVC_COUNT.load(Ordering::Relaxed) > svc),
            "the program never reached its `svc`",
        );
        assert_eq!(
            USER_FAULTS.load(Ordering::Relaxed),
            faults,
            "the program reached EL0 and then FAILED its own self-check: one of \
             .text/.rodata/.data/.bss/stack was not what the ELF asked for",
        );
    }

    /// The loader honours the file's permissions, and does not widen them.
    ///
    /// An ELF's `.rodata` segment is `PF_R` alone. The tempting shortcut is to map every
    /// non-executable segment as `user_data()`, which is **writable** — quietly granting the
    /// program authority its own file never asked for.
    #[test_case]
    fn a_read_only_segment_is_mapped_read_only() {
        let image = initrd().expect("no initrd");
        let (space, _) = load(image).expect("the initrd did not load");

        let rodata = elf::Elf::parse(image)
            .unwrap()
            .segments()
            .find(|s| s.is_readable() && !s.is_writable() && !s.is_executable())
            .expect("the test binary has no read-only segment");

        // Install it so we can ask the CPU's own tables, rather than our record of them.
        // SAFETY: nothing is at EL0 right now; we are a kernel thread mid-test.
        unsafe { mmu::activate_user(space.root()) };

        let (_, flags) = mmu::translate_user(rodata.vaddr).expect(".rodata is not mapped at all");

        assert!(flags.is_user_accessible(), "EL0 cannot read its own .rodata");
        assert!(!flags.is_writable(), "the loader made .rodata WRITABLE");
        assert!(!flags.is_user_executable(), ".rodata is executable at EL0");
        assert!(!flags.is_kernel_executable(), ".rodata is executable at EL1");

        mmu::deactivate_user();
        drop(space);
    }

    /// **The question the kernel must ask, asked of the hardware.**
    ///
    /// `AT S1E0R` means *translate this address as EL0 would, for a read*. One instruction, and
    /// it is the difference between a kernel and a confused deputy.
    ///
    /// Note the precondition assertion. Without it the test is vacuous: "EL0 cannot read the
    /// kernel's text" proves nothing if the kernel's text is not mapped in the first place.
    #[test_case]
    fn the_hardware_says_el0_cannot_read_the_kernels_memory() {
        const KERNEL_TEXT: u64 = 0xffff_0000_4008_0000;

        let (space, _) = load(initrd().expect("no initrd")).expect("the initrd did not load");

        // SAFETY: nothing is at EL0; we are a kernel thread mid-test.
        unsafe { mmu::activate_user(space.root()) };

        // The precondition, and it is what gives the assertion below its teeth: that address IS
        // mapped, and the KERNEL can read it. It reads it all day.
        assert!(
            mmu::translate(KERNEL_TEXT).is_some(),
            "the kernel's text is not mapped, so this test proves nothing",
        );

        // And EL0 cannot. Not "we decline to"; the silicon says no.
        assert!(
            !mmu::user_can_read(KERNEL_TEXT),
            "the hardware says EL0 could read the kernel's own text",
        );
        assert!(!mmu::user_can_write(KERNEL_TEXT));

        // It can read its own code, or the check is a rubber stamp that says no to everything.
        assert!(
            mmu::user_can_read(0x40_0000),
            "EL0 cannot read its own .text, so the check refuses everything and proves nothing",
        );

        // And not an address in its own half that nobody mapped.
        assert!(!mmu::user_can_read(0x7000_0000));

        mmu::deactivate_user();
        drop(space);
    }

    /// **A program with the capability can print. The same program without it cannot.**
    ///
    /// The binary is byte-identical. Nothing about it changed. What changed is what it was
    /// *handed*, and that is the entire content of DECISIONS §10.
    ///
    /// It reports by `brk`, which the kernel treats as a fault: the program expects `NoSuchSlot`
    /// from an empty slot and expects `BadPointer` when it asks the kernel to read the kernel's
    /// own memory, and it kills itself if either is wrong. So **no fault** means every one of
    /// those held.
    #[test_case]
    fn a_capability_is_the_only_way_to_print() {
        let image = initrd().expect("no initrd");

        // With it.
        let svc = SVC_COUNT.load(Ordering::Relaxed);
        let faults = USER_FAULTS.load(Ordering::Relaxed);

        sched::spawn(move || exec_elf(image)).expect("spawn failed");

        assert!(
            wait_for(|| SVC_COUNT.load(Ordering::Relaxed) > svc),
            "the program never made a syscall",
        );
        assert_eq!(
            USER_FAULTS.load(Ordering::Relaxed),
            faults,
            "the program printed, and then one of its expectations FAILED: an empty slot did not \
             say NoSuchSlot, or the kernel agreed to read its own memory on the program's behalf",
        );

        // Without it. Same bytes. Empty table.
        let faults = USER_FAULTS.load(Ordering::Relaxed);

        sched::spawn(move || exec_elf_with(image, false)).expect("spawn failed");

        assert!(
            wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
            "a program with an EMPTY capability table printed anyway",
        );
    }

    /// A thread can name nothing until somebody hands it something.
    #[test_case]
    fn a_new_thread_holds_no_capabilities() {
        use crate::cap::Error;

        // The current thread is a kernel thread, spawned by the harness, and was handed nothing.
        for slot in 0..16 {
            assert_eq!(
                sched::current_cap(slot).err(),
                Some(Error::NoSuchSlot),
                "slot {slot} is not empty in a thread nobody granted anything",
            );
        }
    }

    /// **A user program sends a word to a server it can only name.**
    ///
    /// The client holds one SEND capability on an endpoint, in slot 1, and no other way to reach
    /// or even see the server. The server is a kernel thread blocked on RECV. This is the whole
    /// of 7e end to end, across the EL0 boundary: the message `0x5eed_1e55` leaves a user
    /// register, crosses the `svc`, rendezvouses in the scheduler, and lands in a server that the
    /// client cannot address any other way.
    #[test_case]
    fn a_user_program_can_send_to_a_server_over_an_endpoint() {
        static GOT: AtomicU64 = AtomicU64::new(0);
        static DONE: AtomicBool = AtomicBool::new(false);

        let ep = sched::create_endpoint();

        sched::spawn(move || {
            let msg = sched::ipc_recv(ep);
            GOT.store(msg[0], Ordering::SeqCst);
            DONE.store(true, Ordering::SeqCst);
        })
        .expect("spawn failed");

        let faults = USER_FAULTS.load(Ordering::Relaxed);
        sched::spawn(move || {
            exec_elf_with_endpoint(initrd().expect("no initrd"), ep)
        })
        .expect("spawn failed");

        assert!(
            wait_for(|| DONE.load(Ordering::SeqCst)),
            "the server never received the client's message",
        );
        assert_eq!(
            GOT.load(Ordering::SeqCst),
            0x5eed_1e55,
            "the wrong word crossed the boundary",
        );
        assert_eq!(
            USER_FAULTS.load(Ordering::Relaxed),
            faults,
            "the client faulted instead of sending cleanly",
        );
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
