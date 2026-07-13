//! cricker-os
//!
//! # Why the attributes at the top
//!
//! `no_std`  — there is no operating system beneath us, because we *are* the
//!             operating system. `std`'s `File::open` would make a syscall, and
//!             there is nobody to answer it. We link only `core`.
//!
//! `no_main` — in a normal program `main` is not the first thing to run. The C
//!             runtime (`crt0`) sets up the stack, initializes libc, builds `argv`,
//!             and *then* calls `main`. There is no libc here and nobody has set up
//!             a stack, so there can be no `main`. Our entry point is `_start`, in
//!             assembly, and it sets up the stack itself.
//!
//! See notes/no-std.md.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crate::testing::runner)]
#![reexport_test_harness_main = "test_main"]

mod arch;
mod console;
mod drivers;
mod memory;
mod panic;
mod stack;
mod sync;

#[cfg(test)]
mod testing;

/// The physical address of the Device Tree Blob, as handed to us in `x0`.
///
/// Stashed here so the tests can assert that the boot protocol actually delivered one,
/// which is the whole point of shipping a flat arm64 Image instead of an ELF.
/// See notes/boot-protocol.md.
pub static DTB: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// The kernel's Rust entry point, called from `_start` once we have a stack and a
/// zeroed `.bss`.
///
/// `extern "C"` matters: it tells Rust to follow the aarch64 calling convention
/// (AAPCS64), because assembly is about to call this and the two need to agree on
/// where arguments live. `dtb` arrives in `x0`. See notes/registers.md.
///
/// `-> !` means this never returns, which is true: there is nowhere to return *to*.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(dtb: usize) -> ! {
    DTB.store(dtb, core::sync::atomic::Ordering::Relaxed);

    // Console first, exceptions second, and the order is not arbitrary: the fault
    // handler's entire job is to print, so it is useless until the UART works. The
    // window between these two lines is the last place in the kernel where a fault
    // still kills us silently.
    console::init();
    arch::init();
    stack::init();

    // Now that faults are reportable, go find out how much RAM we actually have. A bug
    // in here is a fault, and a fault is now legible rather than fatal-and-silent.
    memory::init(dtb);

    // And now the sketchiest moment in the kernel. The instant SCTLR_EL1.M is set, the very
    // next instruction is fetched through the MMU. See arch/aarch64/mmu.rs.
    arch::mmu::init();

    #[cfg(test)]
    test_main();

    #[cfg(not(test))]
    {
        use aarch64_cpu::registers::CurrentEL;
        use tock_registers::interfaces::Readable;

        println!();
        println!("cricker-os");
        println!("  exception level : EL{}", CurrentEL.read(CurrentEL::EL));
        println!("  stack top       : {:#018x}", stack_top());
        println!("  device tree     : {dtb:#018x}");
        memory::print_summary();
        arch::mmu::print_summary();

        println!();
        println!("milestone 1: we are running our own code on a CPU with nothing underneath it.");
        println!("milestone 2: and when it goes wrong, we get told.");
        println!("           : and the machine now tells us what it is, instead of us guessing.");
        println!("milestone 3: and we know which parts of it are ours to give away.");
        println!();
    }

    arch::halt()
}

/// Read `__stack_top` back out of the linker script, just to prove we can.
///
/// The linker invents this symbol and writes its address into the ELF; we declare
/// it here so Rust can see it. Note that we want the *address of* the symbol, not
/// its contents. There is no value there. See notes/linker-scripts.md.
#[cfg(not(test))]
fn stack_top() -> usize {
    unsafe extern "C" {
        static __stack_top: core::ffi::c_void;
    }
    (&raw const __stack_top) as usize
}

#[cfg(test)]
mod tests {
    /// Proves the harness itself works. If this fails, nothing else is meaningful.
    #[test_case]
    fn harness_runs() {
        assert_eq!(1 + 1, 2);
    }

    /// Proves `boot.s` zeroed `.bss`.
    ///
    /// A zero-initialized static lands in `.bss`, which occupies no bytes in the
    /// ELF file. Nobody loaded it. If our zeroing loop were wrong, this would hold
    /// whatever garbage was in RAM at power-on. See notes/elf.md.
    #[test_case]
    fn bss_was_zeroed() {
        use core::sync::atomic::{AtomicU64, Ordering};
        static CANARY: AtomicU64 = AtomicU64::new(0);
        assert_eq!(CANARY.load(Ordering::Relaxed), 0);
    }

    /// Proves `boot.s` gave us a usable, correctly aligned stack.
    ///
    /// aarch64 faults on a misaligned `sp` when it's used, so a bug here would show
    /// up as a mysterious early crash rather than as anything legible.
    /// See notes/stack.md.
    #[test_case]
    fn stack_pointer_is_16_byte_aligned() {
        let sp: usize;
        // SAFETY: reads a register. No side effects.
        unsafe { core::arch::asm!("mov {}, sp", out(reg) sp) };
        assert_eq!(sp % 16, 0, "sp = {sp:#x}");
    }

    /// Proves we are where we think we are.
    ///
    /// QEMU's `virt` machine drops us at EL1 by default, which is exactly where a
    /// kernel belongs. If this ever reads EL2, we've been handed the hypervisor
    /// level and will need to drop down ourselves. See notes/aarch64.md.
    #[test_case]
    fn running_at_el1() {
        use aarch64_cpu::registers::CurrentEL;
        use tock_registers::interfaces::Readable;
        assert_eq!(CurrentEL.read(CurrentEL::EL), 1);
    }

    // --- milestone 2 ---

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
    /// VBAR_EL1 says, slot 4 (Current EL, SP_ELx, Synchronous) fires, SAVE_CONTEXT
    /// writes a frame that matches `TrapFrame`, Rust decodes ESR_EL1 and recognizes
    /// EC 0x3c, it advances ELR past the `brk` (which the hardware does NOT do for
    /// us, unlike `svc`), RESTORE_CONTEXT puts the machine back, and `eret` returns
    /// to exactly the right address.
    ///
    /// Get any of that wrong and you don't get a failing assertion. You get an
    /// infinite loop, or a crash. So arriving here at all is most of the test.
    #[test_case]
    fn breakpoint_is_caught_and_execution_resumes() {
        use crate::arch::exceptions::BRK_COUNT;
        use core::sync::atomic::Ordering;

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
    /// A bug in SAVE_CONTEXT/RESTORE_CONTEXT (a wrong offset, a swapped pair) would
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

    // --- the arm64 Image header ---

    /// Proves the boot protocol actually delivered a device tree.
    ///
    /// This is the test that closes the correction from milestone 1. Back then we
    /// shipped an ELF, QEMU took its bare-metal path, and `x0` arrived as zero. Now
    /// we ship a flat binary carrying an arm64 Image header, QEMU recognizes it as a
    /// kernel, follows the Linux boot protocol, and hands us a real pointer.
    ///
    /// A zero here means we have silently regressed to the ELF path, which would be
    /// easy to do by editing the runner script and hard to notice any other way.
    #[test_case]
    fn device_tree_pointer_was_provided() {
        use core::sync::atomic::Ordering;
        assert_ne!(
            crate::DTB.load(Ordering::Relaxed),
            0,
            "no DTB pointer in x0: did we fall back to booting as an ELF?"
        );
    }

    /// Proves the pointer points at an actual device tree, not just at something.
    ///
    /// A nonzero pointer is necessary but not sufficient. Every flattened device tree
    /// begins with the magic `0xd00dfeed`, stored **big-endian** (the format predates
    /// the little-endian consensus and never changed), so we have to byte-swap on the
    /// way in. If this passes, the machine is genuinely describing itself to us.
    #[test_case]
    fn device_tree_has_the_right_magic() {
        use core::sync::atomic::Ordering;
        let ptr = crate::DTB.load(Ordering::Relaxed) as *const u32;

        // SAFETY: QEMU told us it put a device tree here, and the MMU is off, so this
        // is a physical address we can read directly.
        let magic = unsafe { core::ptr::read_volatile(ptr) };

        assert_eq!(
            u32::from_be(magic),
            0xd00d_feed,
            "no device tree magic at {ptr:p}"
        );
    }

    // --- milestone 3: physical memory ---

    /// Proves we read a plausible memory map out of the device tree.
    ///
    /// The allocator logic is tested exhaustively on the host (`cargo test -p frames`,
    /// 14 tests, no emulator). What *only* the real machine can tell us is whether we
    /// pointed it at the right memory, so that's all this checks.
    #[test_case]
    fn memory_map_came_from_the_device_tree() {
        use frames::FRAME_SIZE;

        let s = crate::memory::stats().expect("allocator not initialized");

        // QEMU virt gives us 128 MiB by default. If this ever reads zero, or something
        // absurd, we have misparsed `reg` (which is big-endian, and whose cell width is
        // declared by the *parent* node, both of which are easy to get wrong).
        let total_bytes = s.total as u64 * FRAME_SIZE;
        assert_eq!(total_bytes, 128 * 1024 * 1024, "unexpected RAM size");

        // Some memory must already be spoken for: at minimum the kernel image, the
        // bitmap, and the device tree. A zero here means we reserved nothing, which
        // means we are about to hand out our own code.
        assert!(s.used > 0, "nothing is reserved?");
        assert!(s.free() > 0, "no free memory at all?");
    }

    /// **The one that matters.** Every frame the kernel image touches must be reserved.
    ///
    /// This states the invariant `mark_used` exists to maintain, directly. Our image ends
    /// at 0x40097010, which is not frame-aligned, so the last frame is only *partly*
    /// ours. Round that end down instead of up and the frame stays free, the allocator
    /// hands it out, something writes to it, and the tail of the kernel is quietly
    /// overwritten. The crash lands somewhere else entirely, much later, in code that did
    /// nothing wrong.
    ///
    /// Checking the bitmap directly is both stronger and cheaper than draining the
    /// allocator: it covers *every* frame of the image, and it allocates nothing.
    #[test_case]
    fn every_frame_of_the_kernel_image_is_reserved() {
        use frames::{FRAME_SIZE, Frame};

        let (start, end) = crate::memory::image_bounds();
        let mut addr = start - start % FRAME_SIZE; // round DOWN to the containing frame

        while addr < end {
            assert_eq!(
                crate::memory::is_frame_used(Frame::from_addr(addr)),
                Some(true),
                "frame {addr:#x} overlaps the kernel image but is marked FREE"
            );
            addr += FRAME_SIZE;
        }
    }

    /// And prove `alloc` actually respects that bitmap.
    ///
    /// Keep this array SMALL. It was `[Option<Frame>; 1024]` (16 KiB) on a 64 KiB stack,
    /// and it silently overflowed into .bss, .data, and .text, and hung the machine while
    /// printing something unrelated. See notes/stack.md. The canary catches that now, but
    /// the right move is to not do it.
    #[test_case]
    fn allocator_never_hands_out_the_kernel() {
        let mut taken = [None; 64];

        for slot in taken.iter_mut() {
            let Some(frame) = crate::memory::alloc() else {
                break;
            };
            assert!(
                !crate::memory::is_in_kernel_image(frame.addr()),
                "allocator handed out {:#x}, which is inside the kernel image",
                frame.addr()
            );
            *slot = Some(frame);
        }

        for frame in taken.into_iter().flatten() {
            crate::memory::free(frame);
        }
    }

    /// Proves a frame we were given is real, writable memory that nothing else owns.
    ///
    /// Host tests prove the *bookkeeping* is right. Only the machine can prove the
    /// bookkeeping corresponds to actual RAM. Writing a pattern and reading it back is
    /// the cheapest way to find out we've been handing out an MMIO hole.
    #[test_case]
    fn an_allocated_frame_is_real_memory() {
        use frames::FRAME_SIZE;

        let frame = crate::memory::alloc().expect("out of memory");
        let ptr = frame.addr() as *mut u64;
        let words = (FRAME_SIZE / 8) as usize;

        // SAFETY: the allocator just gave us this frame, so we own it exclusively. The
        // MMU is off, so the physical address is directly usable.
        unsafe {
            for i in 0..words {
                core::ptr::write_volatile(ptr.add(i), 0xcafe_f00d_0000_0000 | i as u64);
            }
            for i in 0..words {
                assert_eq!(
                    core::ptr::read_volatile(ptr.add(i)),
                    0xcafe_f00d_0000_0000 | i as u64,
                    "frame {:#x} word {i} did not hold what we wrote",
                    frame.addr()
                );
            }
        }

        crate::memory::free(frame);
    }

    /// The bitmap must not sit on top of anything already spoken for.
    ///
    /// We used to place it immediately after the kernel image and hope. That worked, but
    /// only because QEMU happens to put the device tree 64 MiB higher up. `image_size` in
    /// the arm64 Image header stops at `__stack_top`, so everything past `__image_end` is
    /// memory we never told the bootloader we wanted, and different firmware need not
    /// leave it alone. Now the placement is scanned and proven; this checks it.
    #[test_case]
    fn bitmap_overlaps_nothing() {
        let (bstart, bsize) = crate::memory::bitmap_region();
        assert!(bsize > 0, "bitmap has no size?");

        let (istart, iend) = crate::memory::image_bounds();
        assert!(
            bstart + bsize <= istart || bstart >= iend,
            "bitmap {bstart:#x}+{bsize:#x} overlaps the kernel image {istart:#x}..{iend:#x}"
        );

        let dtb = crate::DTB.load(core::sync::atomic::Ordering::Relaxed) as u64;
        assert!(
            bstart + bsize <= dtb || bstart >= dtb + 64 * 1024,
            "bitmap {bstart:#x}+{bsize:#x} is sitting on the device tree at {dtb:#x}"
        );

        if let Some((istart, isize)) = crate::memory::initrd_region() {
            assert!(
                bstart + bsize <= istart || bstart >= istart + isize,
                "bitmap {bstart:#x}+{bsize:#x} is sitting on the initrd"
            );
        }
    }

    /// If the bootloader gave us an initrd, the allocator must never hand it out.
    ///
    /// Only meaningful when QEMU is run with `-initrd`, which the default test run isn't.
    /// It asserts the invariant when there IS one, and passes trivially when there isn't,
    /// which is the right shape: the check exists so that the day someone adds `-initrd`
    /// to the runner, this catches it rather than milestone 10 catching it.
    #[test_case]
    fn initrd_is_reserved_if_present() {
        use frames::{FRAME_SIZE, Frame};

        let Some((start, size)) = crate::memory::initrd_region() else {
            return;
        };

        let mut addr = start - start % FRAME_SIZE;
        while addr < start + size {
            assert_eq!(
                crate::memory::is_frame_used(Frame::from_addr(addr)),
                Some(true),
                "frame {addr:#x} is part of the initrd but is marked FREE"
            );
            addr += FRAME_SIZE;
        }
    }

    // --- milestone 4: the MMU ---

    /// The MMU is on, and we are still alive to say so.
    #[test_case]
    fn mmu_is_enabled() {
        assert!(crate::arch::mmu::is_enabled(), "SCTLR_EL1.M is not set");
    }

    /// **The guard page must not be mapped.** That is its entire job.
    ///
    /// Verified at boot too (mmu::verify panics if it's mapped), but stated here as well
    /// because it is the thing that closes the milestone 3 incident, and a protection you
    /// only discover is missing *during* a stack overflow is no protection at all.
    ///
    /// Proven by deliberate overflow: FAR_EL1 comes back as exactly this address.
    #[test_case]
    fn the_guard_page_is_a_hole() {
        use crate::arch::mmu;
        assert_eq!(
            mmu::translate(mmu::stack_guard()),
            None,
            "the guard page IS mapped: a stack overflow would silently eat .bss again"
        );

        // And the pages either side of it must be mapped, or we've put the hole in the
        // wrong place and are protecting nothing.
        assert!(mmu::translate(mmu::stack_guard() - 4096).is_some(), "below the guard");
        assert!(mmu::translate(mmu::stack_bottom()).is_some(), "the stack itself");
    }

    /// W^X, checked against the tables the hardware is actually walking.
    ///
    /// Not a copy of what we intended: `translate` reads TTBR0_EL1 back out of the CPU and
    /// walks from there.
    #[test_case]
    fn kernel_text_is_executable_and_not_writable() {
        use crate::arch::mmu;

        let (pa, flags) = mmu::translate(mmu::text_start()).expect(".text is not mapped");
        assert_eq!(pa, mmu::text_start(), "identity map is not identity");

        assert!(flags.is_kernel_executable(), ".text is not executable");
        assert!(!flags.is_writable(), ".text is WRITABLE: W^X is broken");
        assert!(!flags.is_user_executable(), ".text is executable by EL0");
    }

    /// Constants are read-only, and not executable by anyone.
    #[test_case]
    fn kernel_rodata_is_read_only_and_not_executable() {
        use crate::arch::mmu;

        let (_, flags) = mmu::translate(mmu::rodata_start()).expect(".rodata is not mapped");
        assert!(!flags.is_writable(), ".rodata is writable");
        assert!(!flags.is_kernel_executable(), ".rodata is executable");
    }

    /// The stack is writable and NOT executable.
    #[test_case]
    fn the_stack_is_writable_and_not_executable() {
        use crate::arch::mmu;

        let (_, flags) = mmu::translate(mmu::stack_bottom()).expect("stack is not mapped");
        assert!(flags.is_writable());
        assert!(
            !flags.is_kernel_executable(),
            "the stack is EXECUTABLE: data on the stack could be run as code"
        );
    }

    /// The UART is device-typed.
    ///
    /// Map MMIO as normal memory and the CPU may cache it, reorder writes to it, merge two
    /// writes into one, and speculatively read it. Speculatively reading a UART FIFO
    /// register CONSUMES THE BYTE. See notes/page-tables.md.
    #[test_case]
    fn the_uart_is_mapped_as_device_memory() {
        use crate::arch::mmu;

        let (_, flags) = mmu::translate(0x0900_0000).expect("the UART is not mapped");

        // AttrIndx, bits [4:2], must name the MAIR slot that says Device-nGnRnE.
        let slot = (flags.bits() >> 2) & 0b111;
        assert_eq!(slot, paging::mair::DEVICE, "the UART is not device memory");

        assert!(flags.is_writable(), "we do need to write to it");
        assert!(!flags.is_kernel_executable());
    }

    /// A frame from the allocator is still real, writable memory *through the MMU*.
    ///
    /// Before, this proved the bookkeeping matched physical RAM. Now it also proves the
    /// identity map covers everything the allocator can hand out, which is a different and
    /// newly-necessary claim: with paging on, a physical address the kernel cannot NAME is a
    /// physical address it cannot use.
    #[test_case]
    fn an_allocated_frame_is_reachable_through_the_mmu() {
        use crate::arch::mmu;

        let frame = crate::memory::alloc().expect("out of memory");
        let (pa, flags) = mmu::translate(frame.addr()).expect("allocated frame is NOT MAPPED");

        assert_eq!(pa, frame.addr());
        assert!(flags.is_writable());
        assert!(!flags.is_kernel_executable(), "RAM is executable");

        crate::memory::free(frame);
    }

    // --- locking (DECISIONS.md §9) ---

    /// The lock must mask interrupts for as long as it is held.
    ///
    /// If it doesn't, a timer interrupt can land inside a critical section, try to take the
    /// same lock, and spin forever waiting for code that cannot run until it returns. On one
    /// core. Permanently. See notes/locking.md.
    #[test_case]
    fn irq_safe_mutex_masks_interrupts_while_held() {
        use crate::arch::interrupts;
        use crate::sync::IrqSafeMutex;

        static M: IrqSafeMutex<u32> = IrqSafeMutex::new(7);

        interrupts::enable();
        assert!(interrupts::enabled(), "test setup: IRQs should be on");

        {
            let guard = M.lock();
            assert_eq!(*guard, 7);
            assert!(
                !interrupts::enabled(),
                "IRQs are still live while the lock is held: this is the deadlock"
            );
        }

        assert!(
            interrupts::enabled(),
            "IRQs were not restored after the guard dropped"
        );
    }

    /// **The important one.** The guard must RESTORE the previous state, not enable.
    ///
    /// A lock taken inside a context that already had interrupts masked (an interrupt
    /// handler, or inside an outer lock) must not unmask them on release. Blindly enabling
    /// would turn interrupts back on *inside an interrupt handler*, and the resulting fault
    /// is one you will not enjoy explaining.
    ///
    /// This is exactly why Linux's is called `irqsave`/`irqrestore` rather than
    /// `irqoff`/`irqon`, and it is the single easiest thing to get wrong here.
    #[test_case]
    fn irq_safe_mutex_restores_rather_than_enables() {
        use crate::arch::interrupts;
        use crate::sync::IrqSafeMutex;

        static M: IrqSafeMutex<u32> = IrqSafeMutex::new(0);

        // Pretend we are inside an interrupt handler: IRQs already masked.
        let outer = interrupts::disable();
        assert!(!interrupts::enabled());

        {
            let _guard = M.lock();
            assert!(!interrupts::enabled());
        }

        assert!(
            !interrupts::enabled(),
            "dropping the guard ENABLED interrupts inside an IRQ-disabled context"
        );

        interrupts::restore(outer);
    }

    /// Nesting must not corrupt the state either.
    #[test_case]
    fn nested_locks_restore_correctly() {
        use crate::arch::interrupts;
        use crate::sync::IrqSafeMutex;

        static A: IrqSafeMutex<u32> = IrqSafeMutex::new(1);
        static B: IrqSafeMutex<u32> = IrqSafeMutex::new(2);

        interrupts::enable();

        {
            let a = A.lock();
            assert!(!interrupts::enabled());
            {
                let b = B.lock();
                assert!(!interrupts::enabled());
                assert_eq!(*a + *b, 3);
            }
            // The INNER guard dropped. It must not have re-enabled interrupts, because the
            // outer one is still held.
            assert!(
                !interrupts::enabled(),
                "the inner guard re-enabled IRQs while the outer lock is still held"
            );
        }

        assert!(interrupts::enabled(), "the outer guard failed to restore");
    }

    /// The panic path must be able to print even if the console lock is held.
    ///
    /// Otherwise a fault taken in the middle of a `println!` deadlocks in the fault
    /// handler, and we lose the one message that mattered.
    #[test_case]
    fn console_lock_can_be_busted() {
        // SAFETY: this is exactly the panic path's move, done deliberately.
        unsafe { crate::console::force_unlock() };

        // If force_unlock left the lock in a bad state, this hangs and the test times out
        // rather than failing, which is its own kind of signal.
        crate::println!("    (console still works after force_unlock)");
    }

    /// Proves the stack canary works, without actually smashing the stack.
    ///
    /// The runner checks this after every test (see testing.rs), so a test that blows the
    /// stack is now caught immediately and by name, rather than corrupting the kernel and
    /// hanging somewhere unrelated. That is exactly how milestone 3 went wrong.
    #[test_case]
    fn stack_canary_is_intact_and_we_have_headroom() {
        assert!(crate::stack::intact(), "stack canary is already dead");
        assert!(
            crate::stack::headroom() > 4096,
            "less than 4 KiB of stack left: {}",
            crate::stack::headroom()
        );
    }

    /// Proves alloc and free actually balance, on the real memory map.
    #[test_case]
    fn alloc_and_free_balance() {
        let before = crate::memory::stats().unwrap();

        let a = crate::memory::alloc().unwrap();
        let b = crate::memory::alloc_contiguous(8).unwrap();

        assert_eq!(crate::memory::stats().unwrap().used, before.used + 9);

        crate::memory::free(a);
        for i in 0..8u64 {
            crate::memory::free(frames::Frame::from_addr(
                b.addr() + i * frames::FRAME_SIZE,
            ));
        }

        assert_eq!(
            crate::memory::stats().unwrap(),
            before,
            "frames leaked or were double-counted"
        );
    }
}
