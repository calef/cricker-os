//! The kernel console, and `print!` / `println!`.
//!
//! There is deliberately no global mutable state here. A `Pl011` handle is just a
//! pointer, so we mint a fresh one per call rather than keeping a
//! `static mut CONSOLE`. The real state lives in the hardware, not in our memory.

use core::fmt::Write;

// The early console UART, selected by architecture at compile time. Two concrete drivers, not a
// trait: there are exactly two, they are chosen here and nowhere else, and a trait would be an
// abstraction ahead of a third requirement (DECISIONS.md, rules 2/3). aarch64's `virt` has a PL011;
// RISC-V's has an NS16550. Both expose `new`/`init`/`impl Write`, so the console code below names
// neither. See notes/riscv-port.md.
#[cfg(target_arch = "riscv64")]
use crate::drivers::ns16550::Ns16550 as ConsoleUart;
#[cfg(target_arch = "aarch64")]
use crate::drivers::pl011::Pl011 as ConsoleUart;
use crate::sync::{IrqSafeMutex, rank};

/// The console UART's **physical** address on QEMU's `virt` machine.
#[cfg(target_arch = "aarch64")]
const UART_PHYS: u64 = 0x0900_0000; // PL011
#[cfg(target_arch = "riscv64")]
const UART_PHYS: u64 = 0x1000_0000; // NS16550

/// The console UART's address, as the kernel sees it.
///
/// **Hardcoded on purpose, and it should stay that way.** Not a TODO.
///
/// Everywhere else we insist the machine tell us what it is rather than guessing
/// (notes/device-tree.md). The console is the one place we can't, and the reason is a
/// chicken-and-egg: the device tree parser is the code most likely to have a bug, and
/// `println!` is how you would debug it. So the console has to come up *before* the
/// device tree is parsed, which means the console cannot depend on it.
///
/// A new board needs a different constant here, and that is the correct shape: a per-board
/// early-console address, chosen at compile time, that gets us far enough to read the tree that
/// tells us everything else.
///
/// **This is a virtual address.** It lives in the kernel's direct map at `pa | KERNEL_VA_BASE`; on
/// aarch64 boot.s maps it before any Rust runs and `mmu::init` preserves it. On RISC-V the kernel
/// currently runs bare (identity map), so `phys_to_virt` is the identity until the Sv39 step.
const UART_BASE: usize = crate::arch::mmu::phys_to_virt(UART_PHYS) as usize;

/// The console UART.
///
/// It used to be lock-free: we minted a fresh handle per `print!`, since the handle is just a
/// pointer and the real state lives in the hardware. That was fine with no interrupts. It stops
/// being fine the moment an interrupt handler can print in the middle of somebody else's
/// `write_str`, because the UART is written **one byte at a time** and the two writers would splice
/// into each other mid-word.
///
/// SAFETY: `UART_BASE` is the documented UART address on QEMU `virt`, and nothing else in the kernel
/// touches it.
static CONSOLE: IrqSafeMutex<ConsoleUart> =
    IrqSafeMutex::new(rank::CONSOLE, unsafe { ConsoleUart::new(UART_BASE) });

pub fn init() {
    CONSOLE.lock().init();
}

/// Turn on the console UART's receive interrupt (RISC-V, milestone 20). After this the NS16550 raises
/// its line into the PLIC whenever a keystroke is waiting.
///
/// The kernel arms the device and then stays out of the way: the *byte* is read by the userspace
/// input driver, which `riscv_shell_boot` hands the NS16550's registers as a `DeviceFrame`
/// capability. There used to be a kernel-side `rx_read` here to drain it, from milestone 20 when
/// the input path was still in the kernel; milestone 41 deleted it, along with `Ns16550::read_byte`
/// and `LSR_DR`, after removing the crate-wide riscv `allow(dead_code)` showed they had no caller
/// in *any* configuration, `--features shell` included.
///
/// riscv-only: the aarch64 console stays polling, and its `ConsoleUart` (a PL011) has no such method.
#[cfg(target_arch = "riscv64")]
pub fn rx_enable() {
    CONSOLE.lock().enable_rx_interrupt();
}

/// **Raise and lower the console UART's own interrupt line, for the RISC-V interrupt-delivery
/// tests.** Test builds only.
///
/// aarch64 raises a test interrupt with a GIC SGI, which needs no device at all. RISC-V has no SGI:
/// the only software-raised interrupt it has is the SBI's IPI, which arrives as a *software*
/// interrupt (`scause` = 1) down a different arm of the trap dispatcher than a device's, so it would
/// not exercise the PLIC claim/route/complete path at all. What it has instead is a device whose
/// line software can assert without any transfer or external stimulus: setting the 16550's
/// transmit-empty interrupt enable makes it interrupt at once, because the transmitter of a polling
/// console is always empty. Two register writes up, one down.
///
/// The console is the right device for it precisely because the kernel does not drive it by
/// interrupt: transmit is polled (`write_byte` spins on `LSR`), so an asserted transmit line has no
/// other consumer to disturb, and lowering it restores exactly the state `init` left.
///
/// See `kernel::sched::tests` and notes/interrupts.md.
#[cfg(all(test, target_arch = "riscv64"))]
pub fn raise_uart_interrupt() {
    CONSOLE.lock().enable_tx_interrupt();
}

/// Quiet the line [`raise_uart_interrupt`] raised. Test builds only.
#[cfg(all(test, target_arch = "riscv64"))]
pub fn quiet_uart_interrupt() {
    CONSOLE.lock().disable_interrupts();
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

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments) {
    // Output is forward progress: it keeps the test hang-watchdog's heartbeat alive so a slow but
    // live test is not mistaken for a deadlock (test builds only; see testing::note_progress).
    #[cfg(test)]
    crate::testing::note_progress();
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
