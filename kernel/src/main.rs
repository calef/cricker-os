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

mod arch;
mod console;
mod drivers;
mod panic;

use aarch64_cpu::registers::CurrentEL;
use tock_registers::interfaces::Readable;

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
    console::init();

    println!();
    println!("cricker-os");
    println!("  exception level : EL{}", CurrentEL.read(CurrentEL::EL));
    println!("  stack top       : {:#018x}", stack_top());

    // QEMU only populates x0 with the DTB pointer when it uses the Linux boot
    // protocol, which it picks for flat arm64 `Image` files. We ship an ELF, so
    // it takes the bare-metal path and hands us nothing. See notes/portability.md.
    if dtb == 0 {
        println!("  device tree     : none (ELF boot; see notes/portability.md)");
    } else {
        println!("  device tree     : {dtb:#018x}");
    }

    println!();
    println!("milestone 1: we are running our own code on a CPU with nothing underneath it.");
    println!();

    arch::halt()
}

/// Read `__stack_top` back out of the linker script, just to prove we can.
///
/// The linker invents this symbol and writes its address into the ELF; we declare
/// it here so Rust can see it. Note that we want the *address of* the symbol, not
/// its contents. There is no value there. See notes/linker-scripts.md.
fn stack_top() -> usize {
    unsafe extern "C" {
        static __stack_top: core::ffi::c_void;
    }
    (&raw const __stack_top) as usize
}
