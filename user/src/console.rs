//! The console server: a whole program in one job.
//!
//! Milestone 19f.3, the second program lifted out of `hello` into its own binary (after the worker).
//! It owns a device mapping of the PL011 UART and one request/reply channel. A client writes text
//! into a page it shares with the server, SENDs the length on the request endpoint, and the server
//! copies that many bytes to the UART one at a time and ACKs on the reply endpoint. The kernel never
//! touches the bytes: a driver at EL0, confined by the same capability walls as any workload. A bad
//! length faults the *server* (a read out of its own mapping), not the kernel.
//!
//! Its whole authority is three things init hands it: the request endpoint (slot 0, RECV), the reply
//! endpoint (slot 1, SEND), and the UART registers, plus the shared page mapped read-only. It has no
//! role selector; a standalone binary needs none. It shares the `user` package's `link.ld` but not a
//! line of hello's code.
//!
//! The syscall runtime (`send`/`recv`) comes from the shared `user_rt` crate (19f.6).
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39), among the names recorded there as always
//! right.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use user_rt::{recv, send};

/// The request endpoint (slot 0): the server RECVs a byte count on it.
const REQUEST: u64 = 0;
/// The reply endpoint (slot 1): the server SENDs the acked count back on it.
const REPLY: u64 = 1;

/// The page the client writes text into, mapped read-only in the server's space. Must match what
/// the client (init, or the shell) maps and what init hands the server (`CON_SHARED_VA`).
const SHARED_VA: u64 = 0x0060_0000;
/// How much of it there is. One frame, which is what `console_service` maps, and the bound every
/// byte count from a client is clamped to.
const PAGE: u64 = 4096;
/// The server's device mapping of the UART registers. Must match init's `CON_UART_VA`.
const UART_VA: u64 = 0x0070_0000;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    loop {
        // Block until a client hands us a length.
        let (len, _, _) = recv(REQUEST);

        // **Clamp to the page, because the length is the CLIENT's** (milestone 43,
        // notes/shared-page-audit.md finding 3). This used to be unbounded, with a comment calling
        // an over-long count "a driver bug" and shrugging it off as a crashed process. The count
        // does not come from the driver; it arrives in the request word from whoever holds WRITE
        // on the request endpoint, so `u64::MAX` was a one-message kill of a server every other
        // client of this console shares. `line_editor` clamps at all four of its length sites; the
        // asymmetry was the finding.
        let len = len.min(PAGE);

        // Copy that many bytes from the shared page to the UART, one at a time, exactly as the
        // kernel's PL011 driver used to. The difference is only where this code runs.
        let shared = SHARED_VA as *const u8;
        for i in 0..len {
            // SAFETY: the shared page is mapped read-only in our address space and `len` is
            // clamped to it above, so every offset is inside the one frame the wiring mapped.
            let byte = unsafe { core::ptr::read_volatile(shared.add(i as usize)) };
            uart_put(byte);
        }

        // Acknowledge with the count actually printed, not the count asked for: a client that
        // asked for more than a page learns that fewer bytes went out rather than being told its
        // whole request was honoured.
        send(REPLY, len, 0, 0);
    }
}

/// Transmit one byte, spinning while the transmit path is busy. The register layout is the one
/// arch-specific thing a UART driver is *for*: aarch64's `virt` has a PL011 (32-bit registers, the
/// transmit-FIFO-full flag in the Flag Register), RISC-V's has an NS16550 (byte registers, the
/// transmit-holding-empty flag in the Line Status Register). The kernel configured the device at
/// boot; we only transmit.
#[cfg(target_arch = "aarch64")]
fn uart_put(byte: u8) {
    const DR: u64 = 0x00; // data register
    const FR: u64 = 0x18; // flag register
    const FR_TXFF: u32 = 1 << 5; // transmit FIFO full
    // SAFETY: UART_VA is our device mapping of the PL011, handed to us at spawn.
    unsafe {
        while core::ptr::read_volatile((UART_VA + FR) as *const u32) & FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        core::ptr::write_volatile((UART_VA + DR) as *mut u32, byte as u32);
    }
}

/// The NS16550 twin (RISC-V): byte registers, Transmit Holding Register Empty in the LSR.
#[cfg(target_arch = "riscv64")]
fn uart_put(byte: u8) {
    const THR: u64 = 0x00; // transmit holding register
    const LSR: u64 = 0x05; // line status register
    const LSR_THRE: u8 = 1 << 5; // transmit holding register empty
    // SAFETY: UART_VA is our device mapping of the NS16550, handed to us at spawn.
    unsafe {
        while core::ptr::read_volatile((UART_VA + LSR) as *const u8) & LSR_THRE == 0 {
            core::hint::spin_loop();
        }
        core::ptr::write_volatile((UART_VA + THR) as *mut u8, byte);
    }
}

user_rt::panic_handler!();
