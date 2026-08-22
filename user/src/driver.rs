//! A userspace device driver: the interrupt-driven console input driver (milestone 20).
//!
//! This is the point of the whole interrupt story on RISC-V, and the demonstrator's thesis in one
//! small program: **a device driver is an unprivileged userspace process.** It holds no privilege at
//! all. Its entire authority is three things the kernel handed it: an `Irq` capability for the UART's
//! interrupt (slot 0), a report endpoint to send what it reads (slot 1), and a device-typed mapping
//! of the NS16550's registers at [`UART_VA`]. From those, and nothing else, it services the device.
//!
//! The loop is the seL4 `IRQHandler` protocol:
//!
//! 1. `WAIT` on the `Irq` capability. This blocks until the kernel delivers the UART interrupt as a
//!    message (the interrupt the driver owns by capability, not by privilege).
//! 2. Read the received byte straight from the UART's receive register, through the driver's own
//!    device mapping. The kernel never touches the byte; it is not in the data path.
//! 3. `SEND` the byte on the report endpoint, so whoever is watching sees it.
//! 4. `ACK` the `Irq` capability. The kernel masked the source when it fired; the ACK re-arms it
//!    (kernel side: `arch::irq::enable`, the PLIC on RISC-V, the GIC on aarch64) now that the device
//!    is quiet. Without this, the level-triggered UART would re-fire forever.
//!
//! Fully portable: it names no architecture. `user_rt` supplies the `ecall`/`svc` ABI, and the one
//! device-specific fact (the NS16550 register layout) is the driver's own knowledge, which is exactly
//! what a driver is for.
//!
//! Name: unrecorded, and the vaguest name on this surface. It is the UART interrupt driver, and the
//! word could label a dozen programs in a tree that has several drivers. Introduced 2026-07-27 when
//! the UART interrupt moved to userspace; nothing records the choice.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use user_rt::{invoke, send};

/// The `Irq` capability for the UART interrupt (slot 0), and the report endpoint (slot 1).
const IRQ: u64 = 0;
const REPORT: u64 = 1;

/// Where the kernel mapped the NS16550's registers, device-typed, in this driver's address space.
/// Must match the kernel's `riscv_uart_driver_demo`.
const UART_VA: u64 = 0x0070_0000;
/// NS16550 register offsets: the Receive Buffer (byte 0) and the Line Status Register (byte 5).
const RBR: usize = 0;
const LSR: usize = 5;
/// Line Status: Data Ready (a received byte is waiting).
const LSR_DR: u8 = 0b0000_0001;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, _a1: u64, _a2: u64) -> ! {
    loop {
        // 1. Block until the kernel delivers the UART interrupt as a message.
        // SAFETY: `ecall`; WAIT blocks on the endpoint the kernel routed this interrupt to.
        unsafe { invoke(IRQ, abi::irq::WAIT, 0, 0, 0) };

        // 2. Read the byte from the device, quieting the receive line.
        let byte = read_uart();

        // 3. Report it. The kernel receives this; it never saw the byte itself.
        send(REPORT, byte as u64, 0, 0);

        // 4. Re-arm the interrupt now that the device is serviced.
        // SAFETY: `ecall`; ACK re-enables the source at the controller (kernel: arch::irq::enable).
        unsafe { invoke(IRQ, abi::irq::ACK, 0, 0, 0) };
    }
}

/// Read one received byte from the UART, spinning briefly until Data Ready is set. The interrupt
/// means a byte arrived, so this returns almost immediately.
fn read_uart() -> u8 {
    // SAFETY: the kernel mapped the NS16550 device-typed at UART_VA; these are its registers.
    unsafe {
        let base = UART_VA as *const u8;
        while core::ptr::read_volatile(base.add(LSR)) & LSR_DR == 0 {
            core::hint::spin_loop();
        }
        core::ptr::read_volatile(base.add(RBR))
    }
}

user_rt::panic_handler!();
