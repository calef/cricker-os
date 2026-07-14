//! The kernel console, and `print!` / `println!`.
//!
//! There is deliberately no global mutable state here. A `Pl011` handle is just a
//! pointer, so we mint a fresh one per call rather than keeping a
//! `static mut CONSOLE`. The real state lives in the hardware, not in our memory.

use crate::drivers::pl011::Pl011;
use crate::sync::{IrqSafeMutex, rank};
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
///
/// **This is a virtual address.** The UART is physically at `0x0900_0000`, and it lives in
/// the kernel's direct map at `pa | KERNEL_VA_BASE`. boot.s maps it before any Rust runs, and
/// `mmu::init` preserves it, so this is valid from the kernel's first instruction.
const PL011_BASE: usize = crate::arch::mmu::phys_to_virt(0x0900_0000) as usize;

/// The console UART.
///
/// It used to be lock-free: we minted a fresh `Pl011` handle per `print!`, since the handle
/// is just a pointer and the real state lives in the hardware. That was fine with no
/// interrupts. It stops being fine the moment an interrupt handler can print in the middle
/// of somebody else's `write_str`, because the UART is written **one byte at a time** and
/// the two writers would splice into each other mid-word.
///
/// SAFETY: PL011_BASE is the documented UART address on QEMU `virt`, and nothing else in
/// the kernel touches it.
static CONSOLE: IrqSafeMutex<Pl011> =
    IrqSafeMutex::new(rank::CONSOLE, unsafe { Pl011::new(PL011_BASE) });

pub fn init() {
    CONSOLE.lock().init();
}

/// Break the console lock open. **Panic and fault paths only.**
///
/// # Safety
///
/// If we fault in the middle of a `println!`, the fault handler's own attempt to print
/// would take this lock again and hang, and we would lose the only message that mattered.
/// So the panic path breaks the lock first. Output may be spliced. That is a fine price
/// for getting the message out at all.
///
/// See sync.rs, and DECISIONS.md §9.
pub unsafe fn force_unlock() {
    unsafe { CONSOLE.force_unlock() }
}

/// Write raw bytes. **Not `println!`**: these came from a user program and are not a format
/// string, are not UTF-8 by contract, and are not ours to interpret.
///
/// The only thing the kernel does to a user's bytes is put them on the wire.
pub fn write_bytes(bytes: &[u8]) {
    // ONE lock acquisition for the whole write, not one per byte. Otherwise a kernel `println!`
    // could splice itself into the middle of a user's line, and the UART is written a byte at a
    // time, so "the middle of a line" means "the middle of a word."
    let console = CONSOLE.lock();
    for &b in bytes {
        console.write_byte(b);
    }
}

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments) {
    // Writing to a UART cannot fail in any way we can act on, so drop the Result.
    let _ = CONSOLE.lock().write_fmt(args);
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

#[cfg(test)]
mod tests {
    //! Tests for the console.

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
}
