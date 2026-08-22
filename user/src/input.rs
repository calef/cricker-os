//! Console **input**, at EL0: the raw receive driver (milestone 19f.4, raw since milestone 28).
//!
//! The receive half of a terminal, reduced to what a driver actually is: own the UART's receive
//! side and its interrupt, and forward every byte that arrives. It assembles nothing, echoes
//! nothing, and interprets nothing; the editing, echo, and line assembly that used to live here
//! moved to the line discipline component (`line_editor`), where Unix keeps them too (a UART driver
//! feeds the tty layer; it is not the tty layer). What remains is the irreducible driver loop:
//! WAIT on the interrupt, drain the FIFO, hand the bytes on, ACK.
//!
//! Bytes travel packed in the words of an `OP_BYTES` CALL (up to 8 per message, the terminal
//! contract's driver half; see notes/terminal-contract.md). A keystroke is one byte and control
//! flow, not bulk data, so the words-in-registers path fits §10's rule; a paste drains in
//! 8-byte messages, and the CALL's rendezvous is the flow control that keeps a fast sender from
//! outrunning the discipline.
//!
//! Its whole authority: WRITE on the terminal endpoint (slot 0), the RX interrupt capability
//! (slot 1), and the UART registers mapped device-typed. It cannot print, spawn, or read what
//! anyone else typed. No role selector; the syscall runtime comes from `user_rt`.
//!
//! The one arch-specific thing is the UART register layout, in the `uart` module below
//! (aarch64 PL011, RISC-V NS16550).
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39), among the names recorded there as always
//! right.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use abi::irq;
use line_editor::proto;
use user_rt::{call, invoke};

const UART_VA: u64 = 0x0000_0000_00a0_0000;

const TERM: u64 = 0; // CALL: forward raw wire bytes to the line discipline
const IRQ: u64 = 1; // WAIT / ACK the receive interrupt

/// The UART, the one arch-specific part of an input driver. aarch64's `virt` has a PL011 (32-bit
/// registers, RX-FIFO-empty and TX-FIFO-full flags in the Flag Register, a maskable RX interrupt in
/// IMSC, an explicit interrupt-clear register ICR). RISC-V's has an NS16550 (byte registers,
/// data-ready and transmit-holding-empty in the Line Status Register, RX interrupt enabled in IER,
/// and the RX interrupt is cleared by *reading* the received byte, so there is nothing to clear).
#[cfg(target_arch = "aarch64")]
mod uart {
    use super::UART_VA;
    const DR: u64 = 0x00;
    const FR: u64 = 0x18;
    const IMSC: u64 = 0x38;
    const ICR: u64 = 0x44;
    const FR_RXFE: u32 = 1 << 4;
    const RXIM: u32 = 1 << 4;

    fn rd(off: u64) -> u32 {
        // SAFETY: UART_VA is our device mapping of the PL011.
        unsafe { core::ptr::read_volatile((UART_VA + off) as *const u32) }
    }
    fn wr(off: u64, v: u32) {
        // SAFETY: as above.
        unsafe { core::ptr::write_volatile((UART_VA + off) as *mut u32, v) }
    }

    pub fn rx_pending() -> bool {
        rd(FR) & FR_RXFE == 0
    }
    pub fn rx_get() -> u8 {
        rd(DR) as u8
    }
    pub fn arm_rx_interrupt() {
        wr(IMSC, rd(IMSC) | RXIM);
    }
    pub fn clear_interrupt() {
        wr(ICR, 0x7ff);
    }
}

#[cfg(target_arch = "riscv64")]
mod uart {
    use super::UART_VA;
    const RBR: u64 = 0x00; // receive buffer (read)
    const IER: u64 = 0x01; // interrupt enable
    const LSR: u64 = 0x05; // line status
    const IER_ERBFI: u8 = 1 << 0; // enable received-data-available interrupt
    const LSR_DR: u8 = 1 << 0; // data ready

    fn rd(off: u64) -> u8 {
        // SAFETY: UART_VA is our device mapping of the NS16550.
        unsafe { core::ptr::read_volatile((UART_VA + off) as *const u8) }
    }
    fn wr(off: u64, v: u8) {
        // SAFETY: as above.
        unsafe { core::ptr::write_volatile((UART_VA + off) as *mut u8, v) }
    }

    pub fn rx_pending() -> bool {
        rd(LSR) & LSR_DR != 0
    }
    pub fn rx_get() -> u8 {
        rd(RBR) // reading clears the receive interrupt; that is why clear_interrupt is a no-op
    }
    pub fn arm_rx_interrupt() {
        wr(IER, IER_ERBFI);
    }
    pub fn clear_interrupt() {
        // The NS16550 clears the receive interrupt when the byte is read (rx_get, in drain).
    }
}

/// Forward wire bytes forever. No arguments: a standalone binary.
#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    // Drain anything already in the FIFO by POLLING, before arming the interrupt: input piped in at
    // boot is sitting in the FIFO already, and the first interrupt after arming can race with it.
    // This narrows the window but does NOT close it. A line burst-piped into QEMU during the few
    // instructions between the driver starting and this drain running can still lose its leading
    // character. A real user typing after the prompt never hits it, and every line after the first is
    // interrupt-driven and intact. Fully closing it needs the driver armed before any input arrives.
    drain();
    uart::arm_rx_interrupt();

    loop {
        // SAFETY: the trap validates the Irq capability in slot 1.
        unsafe { invoke(IRQ, irq::WAIT, 0, 0, 0) };
        drain();
        uart::clear_interrupt(); // quiet the device (PL011: ICR; NS16550: reading RBR already did)
        // SAFETY: re-enable the line at the controller now that the device is quiet.
        unsafe { invoke(IRQ, irq::ACK, 0, 0, 0) };
    }
}

/// Read everything in the FIFO and forward it, up to 8 bytes per `OP_BYTES` message, packed
/// little-endian in the second word. The CALL blocks until the discipline has taken the bytes;
/// the FIFO fills while we wait, and we drain what accumulated on return.
fn drain() {
    loop {
        let mut word: u64 = 0;
        let mut n: u64 = 0;
        while n < 8 && uart::rx_pending() {
            word |= (uart::rx_get() as u64) << (8 * n);
            n += 1;
        }
        if n == 0 {
            return;
        }
        call(TERM, proto::req(proto::OP_BYTES, n), word);
    }
}

user_rt::panic_handler!();
