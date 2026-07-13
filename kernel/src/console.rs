//! The kernel console, and `print!` / `println!`.
//!
//! There is deliberately no global mutable state here. A `Pl011` handle is just a
//! pointer, so we mint a fresh one per call rather than keeping a
//! `static mut CONSOLE`. The real state lives in the hardware, not in our memory.

use crate::drivers::pl011::Pl011;
use core::fmt::Write;

/// The PL011 on QEMU's `virt` machine.
///
/// **Hardcoded on purpose, and it should stay that way.** Not a TODO.
///
/// Everywhere else we insist the machine tell us what it is rather than guessing
/// (notes/device-tree.md). The console is the one place we can't, and the reason is a
/// chicken-and-egg: the device tree parser is the code most likely to have a bug, and
/// `println!` is how you would debug it. So the console has to come up *before* the
/// device tree is parsed, which means the console cannot depend on it.
///
/// The Raspberry Pi port will need a different constant here, and that is the correct
/// shape: a per-board early-console address, chosen at compile time, that gets us far
/// enough to read the tree that tells us everything else.
///
/// (The tree does carry it, incidentally: `/chosen/stdout-path = "/pl011@9000000"`.
/// Worth reading *after* boot as a cross-check, but never worth depending on to boot.)
const PL011_BASE: usize = 0x0900_0000;

/// A handle to the console UART.
///
/// TODO(SMP / interrupts): this is not synchronized. Two cores printing at once
/// would interleave bytes, and an interrupt handler that prints while we're
/// mid-`write_str` would garble the line. Fine today: one core, no interrupts
/// (DECISIONS.md §6). Not fine at milestone 5.
fn console() -> Pl011 {
    // SAFETY: PL011_BASE is the documented UART address on QEMU `virt`, and nothing
    // else in the kernel touches it.
    unsafe { Pl011::new(PL011_BASE) }
}

pub fn init() {
    console().init();
}

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments) {
    // Writing to a UART cannot fail in any way we can act on, so drop the Result.
    let _ = console().write_fmt(args);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::console::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
