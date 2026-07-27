//! Console **input**, at EL0: a whole program in one job (milestone 19f.4, split out of hello).
//!
//! The receive half of a terminal. Milestone 8 put console *output* in userspace; a shell also
//! needs input. This driver owns the UART's receive side and its receive interrupt, assembles a line
//! character by character, echoing as it goes, and hands each completed line to a reader (the shell)
//! over IPC. A separate process from the output console server: each does one thing and blocks on one
//! thing, which is what synchronous IPC wants.
//!
//! Its whole authority is what init grants it: SEND on the line endpoint (slot 0), the RX interrupt
//! capability (slot 1), the UART registers mapped device-typed, and a line buffer page. No role
//! selector; a standalone binary needs none. The syscall runtime (`invoke`/`send`) comes from the
//! shared `user_rt` crate (19f.6).
//!
//! The line assembly, echoing, and IPC are portable; the one arch-specific thing is the UART register
//! layout, in the `uart` module below (aarch64 PL011, RISC-V NS16550).

#![no_std]
#![no_main]

use abi::irq;
use user_rt::{invoke, send};

const UART_VA: u64 = 0x0000_0000_00a0_0000;
const LINE_VA: u64 = 0x0000_0000_00b0_0000;

const LINE: u64 = 0; // SEND: hand a completed line's length to the reader
const IRQ: u64 = 1; // WAIT / ACK the receive interrupt
const LINE_MAX: usize = 128;

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
    const FR_TXFF: u32 = 1 << 5;
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
    pub fn tx_put(c: u8) {
        while rd(FR) & FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        wr(DR, c as u32);
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
    const RBR: u64 = 0x00; // receive buffer (read) / transmit holding (write)
    const IER: u64 = 0x01; // interrupt enable
    const LSR: u64 = 0x05; // line status
    const IER_ERBFI: u8 = 1 << 0; // enable received-data-available interrupt
    const LSR_DR: u8 = 1 << 0; // data ready
    const LSR_THRE: u8 = 1 << 5; // transmit holding register empty

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
    pub fn tx_put(c: u8) {
        while rd(LSR) & LSR_THRE == 0 {
            core::hint::spin_loop();
        }
        wr(RBR, c);
    }
    pub fn arm_rx_interrupt() {
        wr(IER, IER_ERBFI);
    }
    pub fn clear_interrupt() {
        // The NS16550 clears the receive interrupt when the byte is read (rx_get, in drain). There
        // is no separate interrupt-clear register, so this is nothing.
    }
}

/// Read lines forever, handing each to the reader over the `LINE` endpoint. The line's bytes are
/// left in the shared `LINE_VA` page for the reader to pick up. No arguments: a standalone binary.
#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    let buf = LINE_VA as *mut u8;
    let mut n: usize;

    // Drain anything already in the FIFO by POLLING, before arming the interrupt: input piped in at
    // boot is sitting in the FIFO already, and the first interrupt after arming can race with it.
    // This narrows the window but does NOT close it. A line burst-piped into QEMU during the few
    // instructions between the driver starting and this drain running can still lose its leading
    // character. A real user typing after the prompt never hits it, and every line after the first is
    // interrupt-driven and intact. Fully closing it needs the driver armed before any input arrives.
    n = drain(buf, 0);
    uart::arm_rx_interrupt();

    loop {
        // SAFETY: the trap validates the Irq capability in slot 1.
        unsafe { invoke(IRQ, irq::WAIT, 0, 0, 0) };
        n = drain(buf, n);
        uart::clear_interrupt(); // quiet the device (PL011: ICR; NS16550: reading RBR already did)
        // SAFETY: re-enable the line at the controller now that the device is quiet.
        unsafe { invoke(IRQ, irq::ACK, 0, 0, 0) };
    }
}

/// Read every character currently in the FIFO into the line buffer, echoing as we go. On a
/// newline, hand the line to the reader and reset. Returns the new line length.
fn drain(buf: *mut u8, mut n: usize) -> usize {
    while uart::rx_pending() {
        let c = uart::rx_get();
        if c == b'\r' || c == b'\n' {
            // Echo the newline so the shell's output starts on a fresh line, then hand over.
            uart::tx_put(b'\r');
            uart::tx_put(b'\n');
            send(LINE, n as u64, 0, 0); // blocks until the reader takes the line
            n = 0;
        } else if (c == 0x7f || c == 0x08) && n > 0 {
            // Backspace: erase the character on screen (back, space, back) as well as in the line.
            n -= 1;
            uart::tx_put(0x08);
            uart::tx_put(b' ');
            uart::tx_put(0x08);
        } else if c >= 0x20 && n < LINE_MAX {
            // **Echo as you type.** The terminal is in raw mode, so nothing echoes locally; the
            // input driver is the only thing that can show a character back. This is safe against
            // interleaving because the reader (the shell) is blocked waiting for the line while
            // you type, so it is not writing the UART. See notes/shell.md.
            uart::tx_put(c);
            // SAFETY: n < LINE_MAX and the line page is mapped read/write.
            unsafe { core::ptr::write_volatile(buf.add(n), c) };
            n += 1;
        }
    }
    n
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // A driver bug is a dead driver: fault, and let the kernel turn it into a kill.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem))
    };
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
