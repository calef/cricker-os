//! PL011 UART driver.
//!
//! The serial port: ancient, and beautifully dumb. Write a byte to a magic address
//! and it goes out a wire, one bit at a time. It is the simplest way a computer can
//! say anything at all, which is why it is the first thing every kernel learns to
//! do. See notes/qemu.md.
//!
//! Note the rule from DECISIONS.md §4: **this driver reaches into no globals.** It
//! is constructed with a base address and knows nothing about the rest of the
//! kernel. Whoever owns it decides where it lives. (Compare NetBSD's `bus_space`,
//! notes/portability.md.)

use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::registers::{ReadOnly, ReadWrite, WriteOnly};
use tock_registers::{register_bitfields, register_structs};

register_bitfields! {
    u32,

    /// Flag register.
    FR [
        /// Transmit FIFO full. Writing to DR while this is set would drop the byte.
        TXFF OFFSET(5) NUMBITS(1) [],
        /// UART is busy transmitting.
        BUSY OFFSET(3) NUMBITS(1) [],
    ],

    /// Line control register.
    LCR_H [
        /// Word length.
        WLEN OFFSET(5) NUMBITS(2) [
            EightBit = 0b11,
        ],
        /// Enable the transmit/receive FIFOs.
        FEN OFFSET(4) NUMBITS(1) [],
    ],

    /// Control register.
    CR [
        /// Receive enable.
        RXE OFFSET(9) NUMBITS(1) [],
        /// Transmit enable.
        TXE OFFSET(8) NUMBITS(1) [],
        /// UART enable.
        UARTEN OFFSET(0) NUMBITS(1) [],
    ],
}

register_structs! {
    /// The PL011's memory-mapped register block.
    ///
    /// The `_reserved` gaps are load-bearing: they make the offsets line up with
    /// the real hardware. tock-registers verifies the whole layout at compile time,
    /// so an off-by-four here is a build error rather than a mystery at runtime.
    #[allow(non_snake_case)]
    pub RegisterBlock {
        (0x00 => DR: ReadWrite<u32>),
        (0x04 => _reserved0),
        (0x18 => FR: ReadOnly<u32, FR::Register>),
        (0x1c => _reserved1),
        (0x24 => IBRD: WriteOnly<u32>),
        (0x28 => FBRD: WriteOnly<u32>),
        (0x2c => LCR_H: WriteOnly<u32, LCR_H::Register>),
        (0x30 => CR: WriteOnly<u32, CR::Register>),
        (0x34 => _reserved2),
        (0x44 => ICR: WriteOnly<u32>),
        (0x48 => @END),
    }
}

/// A handle to one PL011.
///
/// This is just a pointer. Constructing it is free, which is why the console can
/// mint one per `print!` instead of holding global mutable state.
pub struct Pl011 {
    base: *mut RegisterBlock,
}

// SAFETY: the pointer names MMIO, not memory Rust manages. Moving the handle between
// contexts is harmless; *concurrent use* is what needs excluding, and that is the lock's
// job (see console.rs), not this type's.
unsafe impl Send for Pl011 {}

impl Pl011 {
    /// # Safety
    ///
    /// `base` must be the address of a real, mapped PL011 register block.
    pub const unsafe fn new(base: usize) -> Self {
        Self {
            base: base as *mut RegisterBlock,
        }
    }

    fn regs(&self) -> &RegisterBlock {
        // SAFETY: guaranteed by the contract on `new`.
        unsafe { &*self.base }
    }

    /// Configure the UART: 8 data bits, no parity, one stop bit, FIFOs on.
    pub fn init(&self) {
        let r = self.regs();

        // Turn the UART off while we reconfigure it.
        r.CR.set(0);

        // Clear every pending interrupt.
        r.ICR.set(0x7ff);

        // Baud rate divisors. QEMU ignores these completely (there is no real wire
        // and no real clock), but a real PL011 needs them and the Raspberry Pi will.
        // For 115200 baud from a 48 MHz clock:
        //     divisor = 48_000_000 / (16 * 115_200) = 26.0416...
        //     IBRD = 26,  FBRD = round(0.0416... * 64) = 3
        r.IBRD.set(26);
        r.FBRD.set(3);

        r.LCR_H.write(LCR_H::WLEN::EightBit + LCR_H::FEN::SET);

        r.CR.write(CR::UARTEN::SET + CR::TXE::SET + CR::RXE::SET);
    }

    /// Write one byte, spinning until the transmit FIFO has room for it.
    pub fn write_byte(&self, byte: u8) {
        let r = self.regs();
        while r.FR.is_set(FR::TXFF) {
            core::hint::spin_loop();
        }
        r.DR.set(byte as u32);
    }
}

/// This is what earns us `println!("{:#x}", addr)` on bare metal.
///
/// The entire formatting engine (`{:?}`, `{:x}`, width, padding) lives in
/// `core::fmt`, which we still have without `std`. All `std` was ever contributing
/// was *somewhere for the bytes to go*. We supply that in five lines.
/// See notes/no-std.md.
impl core::fmt::Write for Pl011 {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            // Terminals want CRLF. Rust gives us LF.
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
        Ok(())
    }
}
