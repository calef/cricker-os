//! Console **input**, at EL0. The receive half of a terminal.
//!
//! Milestone 8 put console output in userspace; a shell also needs input. This driver owns the
//! PL011's receive side and its receive interrupt (INTID 33). It assembles a line character by
//! character, echoing as it goes, and hands each completed line to a reader (the shell) over IPC.
//! A separate process from the output console server: each does one thing and blocks on one
//! thing, which is what synchronous IPC wants.

use crate::send;
use abi::irq;

const UART_VA: u64 = 0x0000_0000_00a0_0000;
const LINE_VA: u64 = 0x0000_0000_00b0_0000;

const DR: u64 = 0x00; // data
const FR: u64 = 0x18; // flags
const IMSC: u64 = 0x38; // interrupt mask set/clear
const ICR: u64 = 0x44; // interrupt clear
const FR_RXFE: u32 = 1 << 4; // receive FIFO empty
const FR_TXFF: u32 = 1 << 5; // transmit FIFO full
const RXIM: u32 = 1 << 4; // receive interrupt

const LINE: u64 = 0; // SEND: hand a completed line's length to the reader
const IRQ: u64 = 1; // WAIT / ACK the receive interrupt
const LINE_MAX: usize = 128;

fn rd(off: u64) -> u32 {
    // SAFETY: UART_VA is our device mapping.
    unsafe { core::ptr::read_volatile((UART_VA + off) as *const u32) }
}
fn wr(off: u64, v: u32) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile((UART_VA + off) as *mut u32, v) }
}

/// Echo one byte to the terminal, spinning while the transmit FIFO is full.
fn putc(c: u8) {
    while rd(FR) & FR_TXFF != 0 {
        core::hint::spin_loop();
    }
    wr(DR, c as u32);
}

/// Read lines forever, handing each to the reader over the `LINE` endpoint. The line's bytes are
/// left in the shared `LINE_VA` page for the reader to pick up.
pub fn run() -> ! {
    let buf = LINE_VA as *mut u8;
    let mut n: usize = 0;

    // Drain anything already in the FIFO by POLLING, before arming the interrupt. Input piped in
    // at boot is sitting in the FIFO already, and the first interrupt after arming can race with
    // it; polling first sidesteps that and is why the shell never loses the first character of a
    // piped command. See notes.
    n = drain(buf, n);
    wr(IMSC, rd(IMSC) | RXIM);

    loop {
        // SAFETY: `svc`; the kernel validates the Irq capability in slot 1.
        unsafe { crate::invoke(IRQ, irq::WAIT, 0, 0, 0) };
        n = drain(buf, n);
        wr(ICR, 0x7ff); // clear the device interrupt
        // SAFETY: `svc`; re-enable the line at the GIC now that the device is quiet.
        unsafe { crate::invoke(IRQ, irq::ACK, 0, 0, 0) };
    }
}

/// Read every character currently in the FIFO into the line buffer, echoing as we go. On a
/// newline, hand the line to the reader and reset. Returns the new line length.
fn drain(buf: *mut u8, mut n: usize) -> usize {
    while rd(FR) & FR_RXFE == 0 {
        let c = rd(DR) as u8;
        if c == b'\r' || c == b'\n' {
            // Echo the newline so the shell's output starts on a fresh line, then hand over.
            putc(b'\r');
            putc(b'\n');
            send(LINE, n as u64, 0, 0); // blocks until the reader takes the line
            n = 0;
        } else if (c == 0x7f || c == 0x08) && n > 0 {
            // Backspace: erase the character on screen (back, space, back) as well as in the line.
            n -= 1;
            putc(0x08);
            putc(b' ');
            putc(0x08);
        } else if c >= 0x20 && n < LINE_MAX {
            // **Echo as you type.** The terminal is in raw mode, so nothing echoes locally; the
            // input driver is the only thing that can show a character back. This is safe against
            // interleaving because the reader (the shell) is blocked waiting for the line while
            // you type, so it is not writing the UART. See notes/shell.md.
            putc(c);
            // SAFETY: n < LINE_MAX and the line page is mapped read/write.
            unsafe { core::ptr::write_volatile(buf.add(n), c) };
            n += 1;
        }
    }
    n
}
