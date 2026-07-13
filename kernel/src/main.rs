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
mod panic;

#[cfg(test)]
mod testing;

/// The kernel's Rust entry point, called from `_start` once we have a stack and a
/// zeroed `.bss`.
///
/// `extern "C"` matters: it tells Rust to follow the aarch64 calling convention
/// (AAPCS64), because assembly is about to call this and the two need to agree on
/// where arguments live. `dtb` arrives in `x0`. See notes/registers.md.
///
/// `-> !` means this never returns, which is true: there is nowhere to return *to*.
/// The physical address of the Device Tree Blob, as handed to us in `x0`.
///
/// We don't parse it yet. Stashing it here lets the tests assert that the boot
/// protocol actually delivered one, which is the whole point of shipping a flat
/// arm64 Image instead of an ELF. See notes/boot-protocol.md.
pub static DTB: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(dtb: usize) -> ! {
    DTB.store(dtb, core::sync::atomic::Ordering::Relaxed);

    // Console first, exceptions second, and the order is not arbitrary: the fault
    // handler's entire job is to print, so it is useless until the UART works. The
    // window between these two lines is the last place in the kernel where a fault
    // still kills us silently.
    console::init();
    arch::init();

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

        println!();
        println!("milestone 1: we are running our own code on a CPU with nothing underneath it.");
        println!("milestone 2: and when it goes wrong, we get told.");
        println!("           : and the machine now tells us what it is, instead of us guessing.");
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
}
