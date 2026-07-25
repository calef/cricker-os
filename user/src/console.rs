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

#![no_std]
#![no_main]

use user_rt::{recv, send};

/// The request endpoint (slot 0): the server RECVs a byte count on it.
const REQUEST: u64 = 0;
/// The reply endpoint (slot 1): the server SENDs the acked count back on it.
const REPLY: u64 = 1;

/// The page the client writes text into, mapped read-only in the server's space. Must match what
/// the client (init, or the shell) maps and what init hands the server (`CON_SHARED_VA`).
const SHARED_VA: u64 = 0x0060_0000;
/// The server's device mapping of the PL011 UART registers. Must match init's `CON_UART_VA`.
const UART_VA: u64 = 0x0070_0000;

const UART_DR: u64 = 0x00; // data register: write a byte to transmit it
const UART_FR: u64 = 0x18; // flag register
const UART_FR_TXFF: u32 = 1 << 5; // transmit FIFO full

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    loop {
        // Block until a client hands us a length.
        let (len, _, _) = recv(REQUEST);

        // Copy that many bytes from the shared page to the UART, one at a time, exactly as the
        // kernel's PL011 driver used to. The difference is only where this code runs.
        let shared = SHARED_VA as *const u8;
        for i in 0..len {
            // SAFETY: the shared page is mapped read-only in our address space. A malicious length
            // is a read out of our OWN mapping, which faults us, not the kernel: a driver bug is a
            // crashed process.
            let byte = unsafe { core::ptr::read_volatile(shared.add(i as usize)) };
            uart_put(byte);
        }

        // Acknowledge, so the client knows the buffer is free to reuse.
        send(REPLY, len, 0, 0);
    }
}

/// Transmit one byte on the PL011, spinning while the transmit FIFO is full.
fn uart_put(byte: u8) {
    // SAFETY: UART_VA is our device mapping of the PL011, handed to us at spawn. The kernel
    // configured the device at boot; we only transmit.
    unsafe {
        let fr = (UART_VA + UART_FR) as *const u32;
        while core::ptr::read_volatile(fr) & UART_FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        core::ptr::write_volatile((UART_VA + UART_DR) as *mut u32, byte as u32);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // No channel to report on that we would trust: a fault the kernel turns into a kill is the
    // only honest signal. A driver bug is a dead driver.
    unsafe { core::arch::asm!("brk #0", options(nostack, nomem)) };
    loop {
        core::hint::spin_loop();
    }
}
