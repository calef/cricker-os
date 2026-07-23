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
use elf::Elf;
use frames::{FRAME_SIZE, Frame};
use paging::{Flags, Half, MapError, Mapper};

/// Where a user program's code goes. Low half, so the hardware walks `TTBR0`.
#[cfg_attr(feature = "shell", allow(dead_code))]
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
        self.frames.push(frame); // owned: freed when the address space dies
        self.map_at(va, frame.addr(), flags)?;

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

    /// Map an **existing** physical page into this address space, at `va`, with `flags`.
    ///
    /// The frame is **not** recorded for freeing, because we do not own it: it is either a
    /// device's MMIO (the PL011, for a console server) or a page **shared** with another address
    /// space (a message buffer). Freeing MMIO is meaningless, and freeing a shared page when one
    /// of its two holders dies would hand live memory to the allocator. So `Drop` leaves it
    /// alone. The intermediate page tables reaching it *are* recorded, exactly as in `map_new`,
    /// because those genuinely belong to this address space.
    ///
    /// This one function is what lets a driver leave the kernel: it is how the UART's registers
    /// get into a userspace server's address space, and how a shared buffer gets into both a
    /// client's and a server's.
    pub fn map_physical(&mut self, va: u64, phys: u64, flags: Flags) -> Result<(), MapError> {
        self.map_at(va, phys, flags)
    }

    /// Map `phys` at `va`. Intermediate tables are recorded for freeing; the target is not.
    fn map_at(&mut self, va: u64, phys: u64, flags: Flags) -> Result<(), MapError> {
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

        mapper.map(va, phys, flags)
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

/// A physical page to map into a new process's address space, at a chosen VA.
///
/// The frame is **not** owned by the process (it is shared, or it is device MMIO), so it is not
/// freed when the process dies. See [`AddressSpace::map_physical`].
#[derive(Clone, Copy)]
pub struct Mapping {
    pub va: u64,
    pub phys: u64,
    pub flags: Flags,
}

/// **Everything a new process is handed at birth.** Its world, made explicit.
///
/// A capability system has no ambient environment: no inherited file descriptors, no `PATH`, no
/// uid. So a process gets *exactly* what is in this struct and nothing else. The whole of what it
/// can do is a function of `arg0`, `grants`, and `maps`, and reading a `Spawn` literal tells you
/// the complete authority of the thing you are about to start.
pub struct Spawn<'a> {
    /// Lands in `x0` at `_start`. A tiny channel for "which role are you" that needs no
    /// capability, the way a real kernel hands a new process its argc.
    pub arg0: u64,
    /// Lands in `x1`. A second scalar the process needs before it can name anything: the virtio
    /// driver's DMA region physical address, which it must write into device descriptors and
    /// cannot discover, because a process only knows virtual addresses.
    pub arg1: u64,
    /// Lands in `x2`. The virtio driver's device registers sit at a sub-page offset (slots are
    /// 0x200 apart, pages are 0x1000), so we map the containing page and tell the driver where in
    /// it the slot begins.
    pub arg2: u64,
    /// Capabilities, granted into slots 0, 1, 2, ... in order.
    pub grants: &'a [crate::cap::Cap],
    /// Extra pages: a shared buffer, a device's registers. Mapped after the ELF's own segments.
    pub maps: &'a [Mapping],
}

/// Load the initrd program and become it, with nothing but a fresh stack. Never returns.
#[allow(dead_code)] // the bare-client path, exercised by tests
///
/// The bare case: no capabilities, no extra mappings, no argument. It can run its own code and
/// touch its own memory, and it can name nothing else in the system.
pub fn exec_elf(image: &[u8]) -> ! {
    run(
        image,
        Spawn {
            arg0: 0,
            arg1: 0,
            arg2: 0,
            grants: &[],
            maps: &[],
        },
    )
}

/// Load the initrd program and become it, handed the world described by `spawn`. Never returns.
pub fn run(image: &[u8], spawn: Spawn) -> ! {
    let (mut space, entry) = match load(image) {
        Ok(v) => v,
        Err(e) => {
            crate::println!();
            crate::println!("  refused to load a user program: {e:?}");
            crate::println!("  the kernel is fine.");
            crate::sched::exit();
        }
    };

    // The extra pages go in BEFORE we hand the address space off: a shared message buffer, or a
    // device's MMIO for a driver. This is the line that puts a UART into a userspace process.
    for m in spawn.maps {
        space
            .map_physical(m.va, m.phys, m.flags)
            .expect("could not map a Spawn page into the new address space");
    }

    crate::sched::adopt_address_space(space);

    // HAND IT ITS WORLD. Granted in order, so slot 0 is `grants[0]`, and reading the caller's
    // `Spawn` literal tells you the entire authority of the process. There is no path it can
    // say, no uid it can be. A capability system's "environment" is not a variable, it is this.
    for &cap in spawn.grants {
        crate::sched::grant(cap).expect("no free capability slot");
    }

    enter_at(entry, spawn.arg0, spawn.arg1, spawn.arg2)
}

/// Load a program, and become it. Never returns.
///
/// # Safety
/// `program` must be position-independent aarch64 machine code that begins at its first byte.
#[cfg_attr(feature = "shell", allow(dead_code))] // the hand-written demos live in the tour
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

    enter_at(USER_CODE_VA, 0, 0, 0)
}

/// Drop to EL0 at `entry`, on a fresh stack, with `arg0` in `x0`. Never returns.
///
/// `arg0` reaches `_start` as its first argument (AAPCS64 puts it in `x0`). It is how the kernel
/// tells one binary which of several roles to play, the way a real kernel hands a new process
/// its argc/argv. See the console server, which is the same ELF as its client with a different
/// `arg0`.
fn enter_at(entry: u64, arg0: u64, arg1: u64, arg2: u64) -> ! {
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
        let mut x = [0u64; 31];
        x[0] = arg0; // _start's first argument
        x[1] = arg1; // ...and its second
        x[2] = arg2; // ...and its third
        frame.write(TrapFrame {
            x,
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

/// Bringing the console driver up in userspace, and wiring a client to it.
///
/// **This is the milestone-8 payload.** It creates the shared machinery (two endpoints and a
/// shared page), spawns the console *server* as a user process that owns the UART, and returns
/// what a client needs to reach it. The server binary and the client binary are the *same ELF*,
/// told apart by the argument in `x0`.
#[allow(dead_code)] // the demo payload: exercised by the boot demo, mechanism unit-tested
pub mod console_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap};

    /// The PL011's physical address on QEMU `virt`. The kernel maps it for its own debug output;
    /// here we hand a *second* mapping of the same registers to the userspace server. On real
    /// hardware you would give the server exclusive ownership; in QEMU both mappings are fine,
    /// and the kernel's is now used only for panics and boot, not for anyone's `print`.
    const PL011_PHYS: u64 = 0x0900_0000;

    /// Printing-client role (`x0`), matching user/src/hello.rs.
    const ROLE_CLIENT: u64 = 2;
    /// Console-server role.
    const ROLE_SERVER: u64 = 1;

    /// What a client needs to talk to the console server: two endpoints and the shared page.
    #[derive(Clone, Copy)]
    pub struct Console {
        pub request: usize,
        pub reply: usize,
        pub shared_phys: u64,
    }

    /// Spawn the console server as a user process and return a handle for wiring up clients.
    ///
    /// The server holds: `RECV` on `request` (slot 0), `SEND` on `reply` (slot 1), the shared
    /// page mapped **read-only** (it only reads what clients wrote), and the **UART's registers**
    /// mapped as user device memory. That last mapping is the whole milestone: a driver, at EL0,
    /// holding its hardware.
    pub fn start(image: &'static [u8]) -> Console {
        let request = crate::sched::create_endpoint();
        let reply = crate::sched::create_endpoint();
        let shared_phys = crate::memory::alloc()
            .expect("no frame for the shared console buffer")
            .addr();

        // Zero the shared page so a client's first print cannot leak stale RAM.
        // SAFETY: freshly allocated, reachable through the direct map, owned by nobody yet.
        unsafe {
            core::ptr::write_bytes(
                mmu::phys_to_virt(shared_phys) as *mut u8,
                0,
                FRAME_SIZE as usize,
            );
        }

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_SERVER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(request, Rights::READ), // slot 0: RECV requests
                        endpoint_cap(reply, Rights::WRITE),  // slot 1: SEND acks
                    ],
                    maps: &[
                        Mapping {
                            va: SHARED_VA,
                            phys: shared_phys,
                            flags: Flags::user_rodata(),
                        },
                        Mapping {
                            va: UART_VA,
                            phys: PL011_PHYS,
                            flags: Flags::user_device(),
                        },
                    ],
                },
            )
        })
        .expect("could not spawn the console server");

        Console {
            request,
            reply,
            shared_phys,
        }
    }

    /// Spawn a client wired to `console`: `SEND` on request (slot 0), `RECV` on reply (slot 1),
    /// and the shared page mapped **read/write** (it writes the text it wants printed).
    pub fn spawn_client(image: &'static [u8], console: Console) {
        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_CLIENT,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(console.request, Rights::WRITE), // slot 0: SEND
                        endpoint_cap(console.reply, Rights::READ),    // slot 1: RECV ack
                    ],
                    maps: &[Mapping {
                        va: SHARED_VA,
                        phys: console.shared_phys,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("could not spawn a console client");
    }

    /// The user VAs the client and server agree on. Kept here so the kernel and the binary have
    /// one source of truth; they must match user/src/hello.rs.
    const SHARED_VA: u64 = 0x0000_0000_0060_0000;
    const UART_VA: u64 = 0x0000_0000_0070_0000;
}

/// Bringing the virtio block driver up in userspace.
///
/// **Milestone 9's headline.** The kernel enumerates the bus (kernel/src/virtio.rs) to find the
/// block device, then hands a userspace driver everything it needs and nothing it does not: the
/// device's registers, a DMA page, an interrupt, and an endpoint to report what it read. The
/// kernel does not touch the device.
#[allow(dead_code)] // the demo payload; the mechanism is unit-tested
pub mod virtio_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, irq_cap, virtio_cap};

    /// Where the driver expects its DMA page. Must match user/src/virtio.rs. The device registers
    /// are NOT mapped to the driver any more: it drives the device through a `Virtio` capability,
    /// so it cannot point the device outside this DMA region.
    const DMA_VA: u64 = 0x0000_0000_0090_0000;

    const ROLE_VIRTIO_BLK: u64 = 3;

    /// Start the driver. Returns the endpoint it will report its result on, or `None` if there is
    /// no disk attached to enumerate.
    pub fn start(image: &'static [u8]) -> Option<usize> {
        let dev = crate::virtio::find_block_device()?;

        // A DMA page: physical memory the device can reach, mapped into the driver, whose
        // physical address the driver must know (a process sees only virtual addresses). We hand
        // that physical address over in `arg1`.
        let dma = crate::memory::alloc()
            .expect("no DMA frame for the virtio driver")
            .addr();
        // SAFETY: fresh frame, reachable through the direct map. Zero it so stale RAM cannot look
        // like a valid descriptor to the device before the driver writes the real ones.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(dma) as *mut u8, 0, FRAME_SIZE as usize);
        }

        // Route the device's interrupt to an endpoint and enable it, so the driver's `WAIT` on
        // its Irq capability will receive it. See milestone 9a.
        let irq_ep = crate::sched::create_endpoint();
        crate::sched::bind_irq(dev.intid, irq_ep);
        crate::drivers::gic::enable(dev.intid);

        // Where the driver reports the bytes it read.
        let report = crate::sched::create_endpoint();

        // Register the device's transport with the kernel: the kernel owns the MMIO and the
        // DMA-critical operations, and confines the device to this DMA region. The driver gets a
        // `Virtio` capability, not the registers.
        let vid = crate::virtio::register(dev.mmio_phys, dma, FRAME_SIZE);

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_VIRTIO_BLK,
                    arg1: dma, // the DMA region's PHYSICAL address (still needed to build requests)
                    arg2: 0,
                    grants: &[
                        endpoint_cap(report, Rights::WRITE), // slot 0: SEND the result
                        irq_cap(dev.intid),                  // slot 1: WAIT / ACK the interrupt
                        virtio_cap(vid),                     // slot 2: drive the device, confined
                    ],
                    maps: &[Mapping {
                        va: DMA_VA,
                        phys: dma,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("could not spawn the virtio driver");

        Some(report)
    }

    const ROLE_VIRTIO_ATTACK: u64 = 8;

    /// Spawn a MALICIOUS driver that tries to DMA over kernel memory, for the security test. It
    /// holds a real `Virtio` capability and its own DMA region, and points a descriptor at the
    /// kernel image. Returns the endpoint on which it reports whether the kernel refused it.
    pub fn start_attacker(image: &'static [u8]) -> Option<usize> {
        let dev = crate::virtio::find_block_device()?;
        let dma = crate::memory::alloc().expect("no DMA frame").addr();
        // SAFETY: fresh frame via the direct map.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(dma) as *mut u8, 0, FRAME_SIZE as usize);
        }
        let vid = crate::virtio::register(dev.mmio_phys, dma, FRAME_SIZE);
        let report = crate::sched::create_endpoint();

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_VIRTIO_ATTACK,
                    arg1: dma,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(report, Rights::WRITE),
                        irq_cap(dev.intid), // slot 1 (unused by the attacker; keeps virtio at slot 2)
                        virtio_cap(vid),    // slot 2
                    ],
                    maps: &[Mapping {
                        va: DMA_VA,
                        phys: dma,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("could not spawn the virtio attacker");

        Some(report)
    }

    const ROLE_VIRTIO_ATTACK_INDIRECT: u64 = 13;

    /// Spawn a malicious driver that tries the **indirect-descriptor** escape: it negotiates
    /// `INDIRECT_DESC` and submits one descriptor flagged indirect whose inner table aims the
    /// device at the kernel image. Same wiring as [`start_attacker`], different role. The kernel
    /// strips the feature and refuses the flag, so the attacker reports `1` (refused). Returns the
    /// report endpoint.
    pub fn start_attacker_indirect(image: &'static [u8]) -> Option<usize> {
        let dev = crate::virtio::find_block_device()?;
        let dma = crate::memory::alloc().expect("no DMA frame").addr();
        // SAFETY: fresh frame via the direct map.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(dma) as *mut u8, 0, FRAME_SIZE as usize);
        }
        let vid = crate::virtio::register(dev.mmio_phys, dma, FRAME_SIZE);
        let report = crate::sched::create_endpoint();

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_VIRTIO_ATTACK_INDIRECT,
                    arg1: dma,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(report, Rights::WRITE),
                        irq_cap(dev.intid), // slot 1 (unused; keeps virtio at slot 2)
                        virtio_cap(vid),    // slot 2
                    ],
                    maps: &[Mapping {
                        va: DMA_VA,
                        phys: dma,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("could not spawn the indirect virtio attacker");

        Some(report)
    }
}

/// Console **input** in userspace: the receive half of the terminal.
#[allow(dead_code)]
pub mod input_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, irq_cap};

    const UART_VA: u64 = 0x0000_0000_00a0_0000;
    const LINE_VA: u64 = 0x0000_0000_00b0_0000;
    const PL011_PHYS: u64 = 0x0900_0000;
    /// The PL011 on QEMU `virt` is SPI 1, which is INTID 33 (SPIs start at 32).
    const UART_INTID: u32 = 33;

    /// Spawn the input driver in a given role, wired to the UART and its receive interrupt, and
    /// return the endpoint it delivers completed lines on.
    pub fn spawn_role(image: &'static [u8], role: u64) -> usize {
        spawn_wired(image, role, None).0
    }

    /// Spawn the input driver, optionally sharing its line buffer with a reader at `line_va` in
    /// that reader's address space. Returns (line endpoint, line-buffer physical address).
    pub fn spawn_wired(image: &'static [u8], role: u64, _reader: Option<()>) -> (usize, u64) {
        let line = crate::sched::create_endpoint();

        let irq_ep = crate::sched::create_endpoint();
        crate::sched::bind_irq(UART_INTID, irq_ep);
        crate::drivers::gic::enable(UART_INTID);

        // The line buffer the driver assembles into. Shared with the reader (the shell) later; a
        // scratch page for the standalone validator.
        let line_phys = crate::memory::alloc().expect("no line-buffer frame").addr();
        // SAFETY: fresh frame, direct-mapped, owned by nobody yet.
        unsafe {
            core::ptr::write_bytes(
                mmu::phys_to_virt(line_phys) as *mut u8,
                0,
                FRAME_SIZE as usize,
            );
        }

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: role,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(line, Rights::WRITE), // slot 0: SEND completed lines
                        irq_cap(UART_INTID),               // slot 1: WAIT / ACK the RX interrupt
                    ],
                    maps: &[
                        Mapping {
                            va: UART_VA,
                            phys: PL011_PHYS,
                            flags: Flags::user_device(),
                        },
                        Mapping {
                            va: LINE_VA,
                            phys: line_phys,
                            flags: Flags::user_data(),
                        },
                    ],
                },
            )
        })
        .expect("could not spawn the input driver");

        (line, line_phys)
    }
}

/// The shell, and everything it talks to. **Milestone 10: proof the whole stack works.**
///
/// Wires up four processes and the channels between them: the console server (output), the input
/// driver (a line of text at a time), the shell itself, and a kernel-side spawn service that
/// starts worker processes on the shell's request. When it returns, an interactive shell is
/// running at EL0, and everything the user types is a conversation between processes.
#[allow(dead_code)]
pub mod shell_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap};

    const OUT_VA: u64 = 0x0000_0000_0060_0000; // shell <-> console server
    const LINE_VA: u64 = 0x0000_0000_00b0_0000; // shell <-> input driver

    const ROLE_INPUT: u64 = 4;
    const ROLE_SHELL: u64 = 5;
    const ROLE_WORKER: u64 = 6;

    /// **How many children the shell may have alive at once.** The bound that stops a spawn flood
    /// (or workers that block forever without exiting) from exhausting kernel memory: each live
    /// child costs a `Thread`, a 16 KiB kernel stack, and an address space, and there can be at
    /// most this many. A child returns its slot when it is reaped. See notes/quotas.md.
    static SPAWN_QUOTA: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(8);

    pub fn start(image: &'static [u8]) {
        // Output: the console server (milestone 8), and the shell as its client.
        let console = console_service::start(image);

        // Input: the receive driver (milestone 10), delivering lines on `line`.
        let (line, line_phys) = input_service::spawn_wired(image, ROLE_INPUT, None);

        // The shell asks for spawns here; it receives worker results here.
        let spawn_ep = crate::sched::create_endpoint();
        let result_ep = crate::sched::create_endpoint();

        // The spawn service: a kernel thread that starts a worker process for each request. This
        // is the kernel acting as the "process server"; a full capability system would compose
        // spawn from Untyped/Tcb capabilities in userspace (milestone 11), but the shell does not
        // care where the service lives, only that it can name it.
        crate::sched::spawn(move || {
            loop {
                let n = crate::sched::ipc_recv(spawn_ep)[0];
                let spawned = crate::sched::spawn_with_quota(&SPAWN_QUOTA, move || {
                    run(
                        image,
                        Spawn {
                            arg0: ROLE_WORKER,
                            arg1: n, // the worker's input
                            arg2: 0,
                            grants: &[endpoint_cap(result_ep, Rights::WRITE)],
                            maps: &[],
                        },
                    )
                });
                // **Do not panic on out-of-memory.** A spawn flood must degrade, not kill the
                // machine: if the kernel is out of memory we cannot make the worker, so we tell
                // the shell its request failed (a sentinel result) and carry on serving. The
                // security audit flagged the old `.expect(...)` here as a userspace-triggerable
                // kernel panic. (Per-process spawn quotas are the real fix, and they now exist as
                // `QuotaToken` in thread.rs, so this path is bounded as well as panic-free. See
                // notes/quotas.md. Not panicking remains the cheap, honest floor beneath the quota.)
                if spawned.is_none() {
                    // u64::MAX is the "could not spawn" sentinel the shell recognises.
                    crate::sched::ipc_send(result_ep, [u64::MAX, 0, 0]);
                }
            }
        })
        .expect("could not spawn the process service"); // once, at boot, not attacker-reachable

        // The shell itself.
        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_SHELL,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(console.request, Rights::WRITE), // 0: print
                        endpoint_cap(console.reply, Rights::READ),    // 1: console ack
                        endpoint_cap(line, Rights::READ),             // 2: read a line
                        endpoint_cap(spawn_ep, Rights::WRITE),        // 3: request a spawn
                        endpoint_cap(result_ep, Rights::READ),        // 4: worker result
                    ],
                    maps: &[
                        Mapping {
                            va: OUT_VA,
                            phys: console.shared_phys,
                            flags: Flags::user_data(),
                        },
                        Mapping {
                            va: LINE_VA,
                            phys: line_phys,
                            flags: Flags::user_rodata(),
                        },
                    ],
                },
            )
        })
        .expect("could not spawn the shell");
    }
}

/// Milestone 11: hand a process an untyped budget and let it spend it.
#[allow(dead_code)]
pub mod untyped_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, untyped_cap};

    const ROLE_UNTYPED_DEMO: u64 = 7;

    /// Carve `pages` of memory into an untyped region, hand it to a fresh process, and return the
    /// region id and the endpoint the process reports on. The kernel's ONE allocation is the
    /// untyped itself; everything the process maps afterward spends that, not the allocator.
    pub fn start(image: &'static [u8], pages: u64) -> Option<(usize, usize)> {
        let region = crate::untyped::create(pages)?;
        let report = crate::sched::create_endpoint();

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_UNTYPED_DEMO,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        untyped_cap(region),                 // slot 0: the memory budget
                        endpoint_cap(report, Rights::WRITE), // slot 1: report the result
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the untyped demo");

        Some((region, report))
    }
}

/// **Capability delegation: authority moves between processes at runtime.**
///
/// Every other capability in cricker-os is minted by the kernel and handed to a process at spawn.
/// That made the kernel a central authority-granting oracle, which is the ambient-authority shape
/// §10 argued against, just relocated. A capability system's defining move is that a process can
/// pass authority it holds to another process, narrowing it on the way, and only if it was trusted
/// to (`GRANT`). This wires the smallest scenario that exercises all three: a *granter* delegates a
/// resource capability to a *receiver* over a channel, narrowed to `WRITE` (no `GRANT`); the
/// receiver uses it and then cannot pass it on. See user/src/hello.rs granter()/receiver().
/// **Frame capabilities: shared memory a process holds, maps, and delegates.**
///
/// The payoff of delegation applied to memory. A *producer* retypes a page out of its own untyped
/// into a `Frame` capability, maps it, writes into it, and delegates a READ-only view to a
/// *consumer*, which maps the same physical page and reads what the producer wrote. The kernel
/// copies nothing and pre-arranges nothing: the two processes compose the sharing themselves, and
/// the read-only narrowing means the consumer can look but not write. See user/src/hello.rs
/// frame_producer()/frame_consumer().
#[cfg(test)]
pub mod frame_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, untyped_cap};

    const ROLE_PRODUCER: u64 = 11;
    const ROLE_CONSUMER: u64 = 12;

    /// Spawn the pair, each with its own untyped budget, and return the endpoint the consumer
    /// reports its verdict on. Eight pages of untyped apiece covers one frame plus the page tables
    /// each side needs to map it.
    pub fn wire(image: &'static [u8]) -> usize {
        let channel = crate::sched::create_endpoint();
        let report = crate::sched::create_endpoint();
        let prod_ut = crate::untyped::create(8).expect("no untyped for the frame producer");
        let cons_ut = crate::untyped::create(8).expect("no untyped for the frame consumer");

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_PRODUCER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        untyped_cap(prod_ut),                 // slot 0: retype the frame + page tables
                        endpoint_cap(channel, Rights::WRITE), // slot 1: delegate the frame
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the frame producer");

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_CONSUMER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(channel, Rights::READ), // slot 0: receive the frame
                        untyped_cap(cons_ut),                // slot 1: page tables for its mappings
                        endpoint_cap(report, Rights::WRITE), // slot 2: report the verdict
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the frame consumer");

        report
    }
}

#[cfg(test)]
pub mod delegation_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap};

    const ROLE_GRANTER: u64 = 9;
    const ROLE_RECEIVER: u64 = 10;

    /// The word the receiver sends back through the delegated capability, so a test can confirm a
    /// capability minted by one process works when invoked by another.
    pub const USED_WORD: u64 = 0x5A;

    /// Spawn the pair and return `(resource endpoint, report endpoint)`. The granter delegates its
    /// `resource` capability (held `WRITE | GRANT`) to the receiver, narrowed to `WRITE`. The
    /// receiver `SEND`s [`USED_WORD`] on the received capability (a `RECV` on `resource` collects
    /// it) and reports a two-bit verdict on `report`.
    pub fn wire(image: &'static [u8]) -> (usize, usize) {
        let channel = crate::sched::create_endpoint(); // granter SEND_CAP -> receiver RECV_CAP
        let resource = crate::sched::create_endpoint(); // the capability being delegated
        let loopback = crate::sched::create_endpoint(); // the receiver's refused re-delegation target
        let report = crate::sched::create_endpoint(); // the receiver's verdict

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_GRANTER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(channel, Rights::WRITE), // slot 0: SEND_CAP over it
                        endpoint_cap(resource, Rights::WRITE.union(Rights::GRANT)), // slot 1: delegate this
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the delegation granter");

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_RECEIVER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(channel, Rights::READ),   // slot 0: RECV_CAP
                        endpoint_cap(report, Rights::WRITE),   // slot 1: report the verdict
                        endpoint_cap(loopback, Rights::WRITE), // slot 2: attempt re-delegation here
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the delegation receiver");

        (resource, report)
    }
}

/// **Milestone 12: Call/Reply, at EL0.** One request endpoint, a server that answers a caller it was
/// never wired to, and the one-shot reply capability proven across the boundary. See
/// user/src/hello.rs call_server()/call_client().
#[cfg(test)]
pub mod call_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap};

    const ROLE_SERVER: u64 = 14;
    const ROLE_CLIENT: u64 = 15;

    /// Spawn the pair, sharing one request endpoint. Returns `(client reply report, server one-shot
    /// report)`: the client publishes the reply it got, the server publishes whether a second reply
    /// was refused.
    pub fn wire(image: &'static [u8]) -> (usize, usize) {
        let ep = crate::sched::create_endpoint(); // client CALL <-> server RECV_CAP
        let call_report = crate::sched::create_endpoint();
        let oneshot_report = crate::sched::create_endpoint();

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_SERVER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(ep, Rights::READ),              // slot 0: RECV calls
                        endpoint_cap(oneshot_report, Rights::WRITE), // slot 1: report the verdict
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the call server");

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_CLIENT,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(ep, Rights::WRITE),          // slot 0: CALL
                        endpoint_cap(call_report, Rights::WRITE), // slot 1: report the reply
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the call client");

        (call_report, oneshot_report)
    }
}

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

        assert!(
            flags.is_user_accessible(),
            "EL0 cannot read its own .rodata"
        );
        assert!(!flags.is_writable(), "the loader made .rodata WRITABLE");
        assert!(!flags.is_user_executable(), ".rodata is executable at EL0");
        assert!(
            !flags.is_kernel_executable(),
            ".rodata is executable at EL1"
        );

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
    fn a_user_client_moves_data_through_shared_memory() {
        // What the client prints first. Must match user/src/hello.rs.
        const FIRST_LINE: &[u8] =
            b"      hello from EL0, printed by a driver that also runs at EL0.\n";
        const SHARED_VA: u64 = 0x0000_0000_0060_0000;

        static CAPTURED: AtomicBool = AtomicBool::new(false);
        static LEN: AtomicU64 = AtomicU64::new(0);
        static mut BUF: [u8; 128] = [0; 128];

        let image = initrd().expect("no initrd");
        let request = sched::create_endpoint();
        let reply = sched::create_endpoint();

        // The shared page, owned by the test (not by either address space), so `map_physical`
        // will not free it. Deliberately leaked: the client spins forever, so there is no safe
        // moment to reclaim it, and one page is a fine price for the test.
        let shared = crate::memory::alloc().expect("no shared frame").addr();

        // The server: a kernel thread that reads the shared page and records the first message.
        sched::spawn(move || {
            loop {
                let m = sched::ipc_recv(request);
                let len = m[0].min(128);
                if !CAPTURED.load(Ordering::SeqCst) {
                    // SAFETY: the shared frame is ours via the direct map; the client wrote `len`
                    // bytes before sending. Single-threaded capture.
                    let src = crate::arch::mmu::phys_to_virt(shared) as *const u8;
                    let dst = &raw mut BUF as *mut u8;
                    for i in 0..len as usize {
                        // SAFETY: both pointers are in range; BUF is 128 bytes and len <= 128.
                        unsafe {
                            core::ptr::write_volatile(
                                dst.add(i),
                                core::ptr::read_volatile(src.add(i)),
                            )
                        };
                    }
                    LEN.store(len, Ordering::SeqCst);
                    CAPTURED.store(true, Ordering::SeqCst);
                }
                sched::ipc_send(reply, [0, 0, 0]); // ack, so the client reuses the buffer
            }
        })
        .expect("spawn failed");

        // The client: the real binary, client role, wired to the endpoints and the shared page.
        let faults = USER_FAULTS.load(Ordering::Relaxed);
        sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: 2, // printing-client role (matches user/src/hello.rs)
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        crate::cap::endpoint_cap(request, crate::cap::Rights::WRITE),
                        crate::cap::endpoint_cap(reply, crate::cap::Rights::READ),
                    ],
                    maps: &[Mapping {
                        va: SHARED_VA,
                        phys: shared,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("spawn failed");

        assert!(
            wait_for(|| CAPTURED.load(Ordering::SeqCst)),
            "the server never received a message through shared memory",
        );
        assert_eq!(
            USER_FAULTS.load(Ordering::Relaxed),
            faults,
            "the client faulted instead of printing cleanly",
        );

        let len = LEN.load(Ordering::SeqCst) as usize;
        // SAFETY: written by the server thread, which stopped touching BUF once CAPTURED.
        let got = unsafe { core::slice::from_raw_parts(&raw const BUF as *const u8, len) };
        assert_eq!(
            got, FIRST_LINE,
            "the wrong bytes arrived through shared memory"
        );
    }

    /// `map_physical` puts one physical frame into an address space at a chosen VA, with exactly
    /// the permissions asked for and no more. The mechanism a driver leaves the kernel on.
    #[test_case]
    fn map_physical_maps_a_shared_frame_and_a_device_page() {
        const DATA_VA: u64 = 0x0000_0000_0060_0000;
        const DEV_VA: u64 = 0x0000_0000_0070_0000;
        const PL011_PHYS: u64 = 0x0900_0000;

        let mut space = AddressSpace::new().expect("no address space");
        let frame = crate::memory::alloc().expect("no frame").addr();

        space
            .map_physical(DATA_VA, frame, Flags::user_data())
            .expect("shared map failed");
        space
            .map_physical(DEV_VA, PL011_PHYS, Flags::user_device())
            .expect("device map failed");

        // SAFETY: nothing is at EL0; we are a kernel thread mid-test.
        unsafe { mmu::activate_user(space.root()) };

        let (data_pa, data_f) = mmu::translate_user(DATA_VA).expect("shared page not mapped");
        assert_eq!(data_pa, frame, "shared page maps the wrong frame");
        assert!(data_f.is_user_accessible() && data_f.is_writable());
        assert!(!data_f.is_user_executable());

        let (dev_pa, dev_f) = mmu::translate_user(DEV_VA).expect("device page not mapped");
        assert_eq!(
            dev_pa, PL011_PHYS,
            "device page maps the wrong physical address"
        );
        assert!(dev_f.is_user_accessible() && dev_f.is_writable());

        mmu::deactivate_user();
        crate::memory::free(Frame::from_addr(frame));
        drop(space);
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

    /// **A userspace driver reads a file off a real virtio disk.** Milestone 9, end to end.
    ///
    /// The kernel enumerates the bus and hands a driver at EL0 the device registers, a DMA page,
    /// and an interrupt. The driver sets up a virtqueue, reads the superblock by DMA, parses the
    /// crickerfs directory, reads the `motd` file, and reports its first bytes. We check them
    /// against the known contents, which proves real disk data crossed DMA and the EL0 boundary.
    ///
    /// It also proves the interrupt path (9a) carried the completion: `ROUTED_IRQS` counts device
    /// interrupts turned into messages, and it must rise. And it proves the idle thread works: the
    /// driver blocks waiting for that interrupt with nothing else to run, and the scheduler idles
    /// rather than declaring a deadlock.
    #[test_case]
    fn a_userspace_driver_reads_a_file_from_a_virtio_disk() {
        use crate::arch::exceptions::ROUTED_IRQS;

        let report = match virtio_service::start(initrd().expect("no initrd")) {
            Some(r) => r,
            None => {
                // No disk attached to this run. Nothing to test; do not fail.
                crate::println!("    (no virtio disk attached; skipping)");
                return;
            }
        };

        let irqs_before = ROUTED_IRQS.load(Ordering::Relaxed);

        // Blocks until the driver has done the whole read. If the driver faults, it never sends,
        // and the scheduler idles; the QEMU-level timeout is the backstop.
        let word = sched::ipc_recv(report)[0];

        assert_eq!(
            &word.to_le_bytes(),
            b"cricker-",
            "the driver reported the wrong file contents",
        );
        assert!(
            ROUTED_IRQS.load(Ordering::Relaxed) > irqs_before,
            "the read completed but no device interrupt was delivered as a message",
        );
    }

    /// **The shell's `run` mechanism: spawn a process, get its answer.** Milestone 10's core.
    ///
    /// A worker process is started at EL0 with an argument, computes `n*n`, reports the result on
    /// an endpoint it was handed, and exits. The whole lifecycle a shell drives when you type
    /// `run n` — minus the interactive loop, which is exercised by the piped demo instead.
    #[test_case]
    fn a_spawned_worker_process_computes_and_reports() {
        const ROLE_WORKER: u64 = 6;

        let result = sched::create_endpoint();
        let faults = USER_FAULTS.load(Ordering::Relaxed);

        sched::spawn(move || {
            run(
                initrd().expect("no initrd"),
                Spawn {
                    arg0: ROLE_WORKER,
                    arg1: 9, // the worker computes 9*9
                    arg2: 0,
                    grants: &[crate::cap::endpoint_cap(result, crate::cap::Rights::WRITE)],
                    maps: &[],
                },
            )
        })
        .expect("spawn failed");

        let answer = sched::ipc_recv(result)[0];
        assert_eq!(answer, 81, "the spawned worker computed the wrong answer");
        assert_eq!(
            USER_FAULTS.load(Ordering::Relaxed),
            faults,
            "the worker faulted instead of computing cleanly",
        );
    }

    /// **The kernel stops allocating.** Milestone 11's whole point, as one number.
    ///
    /// We carve an untyped region, then a process maps page after page out of it until the region
    /// is exhausted. The assertion that matters: the kernel's used-frame count **does not change
    /// while the process allocates**, because every page comes from the untyped, not the kernel
    /// allocator. A process cannot make the kernel allocate, so it cannot exhaust kernel memory;
    /// it runs out of its own budget and stops, cleanly, with the kernel untouched.
    #[test_case]
    fn a_process_spends_untyped_and_the_kernel_never_allocates() {
        let used = || crate::memory::stats().expect("no allocator").used;

        const PAGES: u64 = 24;
        let (region, report) = untyped_service::start(initrd().expect("no initrd"), PAGES)
            .expect("could not create the untyped region");

        // The process sends a "ready" signal once it is fully loaded (its ELF and stack are
        // kernel-allocated, like any process). We measure the frame count THERE, so the window we
        // check contains only what it does next: map pages out of its untyped.
        sched::ipc_recv(report); // ready
        let baseline = used();
        let faults = USER_FAULTS.load(Ordering::Relaxed);

        let mapped = sched::ipc_recv(report)[0]; // the count, after it exhausted the untyped

        assert_eq!(
            used(),
            baseline,
            "the kernel allocated {} frames while a process mapped {mapped} pages: untyped is not \
             backing the process's memory",
            used() as i64 - baseline as i64,
        );
        assert!(mapped > 0, "the process mapped nothing");
        assert_eq!(
            USER_FAULTS.load(Ordering::Relaxed),
            faults,
            "the process faulted instead of exhausting its budget cleanly",
        );

        // And the untyped is genuinely spent: the process mapped until it ran dry.
        let (watermark, total) = crate::untyped::usage(region).expect("region vanished");
        assert_eq!(total, PAGES);
        assert!(
            watermark >= mapped,
            "the process mapped {mapped} pages but the untyped only advanced {watermark}",
        );
        assert!(
            total - watermark < 4,
            "the untyped had {} pages left unspent; the process gave up early",
            total - watermark,
        );
    }

    /// **The DMA-confinement fix, end to end.** A malicious driver at EL0 holds a real `Virtio`
    /// capability and its own DMA region, and points a descriptor at the kernel's image, asking
    /// the device to write there. Because the device has no IOMMU, this would succeed if the
    /// driver could ring it directly. The kernel validates every descriptor on submit and refuses
    /// this one, so the device is never told to go and never touches the kernel. The driver
    /// reports `1` when it was refused.
    #[test_case]
    fn the_kernel_refuses_a_dma_descriptor_that_escapes_the_drivers_region() {
        let report = match virtio_service::start_attacker(initrd().expect("no initrd")) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio disk attached; skipping)");
                return;
            }
        };
        let refused = sched::ipc_recv(report)[0];
        assert_eq!(
            refused, 1,
            "a malicious driver's descriptor pointing at kernel memory was NOT refused: the \
             device could have DMA'd over the kernel",
        );
    }

    /// **The indirect-descriptor escape, end to end.** The direct-descriptor test above proves the
    /// obvious case. This proves the subtle one: a driver that negotiates `INDIRECT_DESC` and
    /// submits an indirect descriptor whose inner table (in its own region) aims the device at the
    /// kernel. A validator that walked only the flat chain would pass the outer descriptor and let
    /// the device follow the table out. The kernel strips the feature and refuses the flag, so the
    /// device is never rung. The driver reports `1` when it was refused.
    #[test_case]
    fn the_kernel_refuses_an_indirect_descriptor_escape() {
        let report = match virtio_service::start_attacker_indirect(initrd().expect("no initrd")) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio disk attached; skipping)");
                return;
            }
        };
        let refused = sched::ipc_recv(report)[0];
        assert_eq!(
            refused, 1,
            "an indirect descriptor whose inner table pointed at kernel memory was NOT refused: \
             the device could have followed it out of the driver's region",
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

        // The steady-state thread count to return to after each spawned thread is reaped. It is
        // NOT a constant: the boot thread and core 0's idle are two, plus one idle thread per
        // secondary core (SMP, §11). Capture it dynamically so the test does not bake in a core
        // count.
        let baseline = sched::thread_count();

        // Warm up: the first user thread ever created pays for page tables in a region of
        // kernel VA that nothing has touched. Measure the STEADY state, which is the one that
        // has to hold forever.
        sched::spawn(|| unsafe { exec(outlaw()) }).expect("spawn failed");
        let f0 = USER_FAULTS.load(Ordering::Relaxed);
        assert!(wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > f0));
        assert!(wait_for(|| sched::thread_count() <= baseline));

        let before = used();

        for _ in 0..4 {
            let f = USER_FAULTS.load(Ordering::Relaxed);
            sched::spawn(|| unsafe { exec(outlaw()) }).expect("spawn failed");
            assert!(wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > f));
            assert!(wait_for(|| sched::thread_count() <= baseline));
        }

        assert_eq!(
            used(),
            before,
            "four user address spaces came and went and {} frames did not come back",
            used() as i64 - before as i64,
        );
    }

    /// **Capability delegation, end to end.** A granter process passes a resource capability to a
    /// receiver process over an IPC channel, narrowed to `WRITE`. Three things must hold, and this
    /// checks all three: the receiver *gets* the capability, the receiver can *use* it (a
    /// capability minted for it by another process works when it invokes it), and the receiver
    /// *cannot pass it on* because it was handed the capability without `GRANT`. This is the
    /// operation that makes the capability model composable by processes instead of brokered by the
    /// kernel at spawn. See user/src/hello.rs and user::delegation_service.
    #[test_case]
    fn a_capability_can_be_delegated_over_ipc_and_grant_gates_re_delegation() {
        let image = initrd().expect("no initrd");
        let (resource, report) = delegation_service::wire(image);

        // The receiver invoked the *delegated* capability to SEND this word. Collecting it here is
        // proof the capability the granter minted for the receiver actually carries authority.
        let used = sched::ipc_recv(resource)[0];
        assert_eq!(
            used,
            delegation_service::USED_WORD,
            "a delegated capability did not work when its recipient invoked it",
        );

        // The receiver's own two-bit verdict: bit 0 it received a capability, bit 1 the kernel
        // refused its attempt to re-delegate a capability it holds without GRANT.
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict & 0b01,
            0b01,
            "the receiver never received the delegated capability",
        );
        assert_eq!(
            verdict & 0b10,
            0b10,
            "a capability held WITHOUT grant was allowed to be re-delegated: rights did not gate it",
        );
    }

    /// **Milestone 12: a process calls a server it was never wired to, and the reply cap is
    /// one-shot.** The client `CALL`s across the boundary; the server `RECV_CAP`s the request plus a
    /// kernel-minted reply capability naming the caller, answers through it (the round trip through
    /// the real syscall path), then tries to answer a second time and reports that the kernel
    /// refused. This is what a pre-wired reply endpoint cannot guarantee.
    #[test_case]
    fn a_process_calls_a_server_and_the_reply_is_one_shot() {
        let (call_report, oneshot_report) = call_service::wire(initrd().expect("no initrd"));

        let reply = sched::ipc_recv(call_report)[0];
        assert_eq!(
            reply, 42,
            "the CALL did not return the server's reply (40 + 2)"
        );

        let one_shot = sched::ipc_recv(oneshot_report)[0];
        assert_eq!(
            one_shot, 1,
            "the server's second reply was NOT refused: the reply capability is not one-shot",
        );
    }

    /// **Frame capabilities, end to end.** A producer retypes a page into a `Frame`, maps it, writes
    /// a sentinel, and delegates a READ-only view to a consumer. Two things must hold: the consumer
    /// reads the producer's sentinel through its *own* mapping of the same physical page (the memory
    /// is genuinely shared, and the kernel copied nothing), and the consumer *cannot* map that page
    /// writable, because it was handed the frame with `READ` alone. This is §10's "shared memory
    /// carries data" done by the processes rather than wired by the kernel at spawn. See
    /// user/src/hello.rs and user::frame_service.
    #[test_case]
    fn a_frame_capability_shares_a_page_and_a_read_only_view_cannot_write_it() {
        let image = initrd().expect("no initrd");
        let report = frame_service::wire(image);

        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict & 0b01,
            0b01,
            "the consumer did not read the producer's sentinel through the shared frame: the page was not shared",
        );
        assert_eq!(
            verdict & 0b10,
            0b10,
            "a frame delegated READ-only was mappable writable: rights did not confine the mapping",
        );
    }
}
