//! NS16550 UART driver (RISC-V console).
//!
//! The other ancient, beautifully dumb serial port. Where aarch64's `virt` machine has a PL011,
//! RISC-V's has an NS16550 (a 16550-compatible 8250) at `0x1000_0000`. Same idea, different
//! register block: eight byte-wide registers at consecutive addresses, and the transmit-ready flag
//! lives in the Line Status Register instead of a Flag Register.
//!
//! This one uses plain volatile byte access rather than `tock_registers` register blocks: the
//! registers are `u8`, and the `register_structs!` layout macro emits alignment checks that reduce
//! to "n % 1" on byte registers, which is just noise here. Named offsets and bit masks are clearer
//! for a device this small.
//!
//! Same rule as every driver here (DECISIONS.md §4): **it reaches into no globals.** It is
//! constructed with a base address and knows nothing about the rest of the kernel. It is the
//! sibling of `pl011.rs`, selected by the console at compile time. See notes/riscv-port.md.

// Register offsets from the UART base (reg-shift 0: byte registers at consecutive addresses, which
// is how QEMU's `virt` NS16550 is wired).
const THR: usize = 0; // Transmit Holding (write) / Receive Buffer (read); divisor low when DLAB=1.
const IER: usize = 1; // Interrupt Enable; divisor high when DLAB=1.
const FCR: usize = 2; // FIFO Control (write).
const LCR: usize = 3; // Line Control.
const LSR: usize = 5; // Line Status.

// Line Control bits.
const LCR_8N1: u8 = 0b0000_0011; // 8 data bits, no parity, one stop bit.
const LCR_DLAB: u8 = 0b1000_0000; // Divisor Latch Access Bit.

// FIFO Control bits: enable, and clear both FIFOs.
const FCR_ENABLE_CLEAR: u8 = 0b0000_0111;

// Line Status bit: Transmit Holding Register Empty (room for another byte).
const LSR_THRE: u8 = 0b0010_0000;
// Interrupt Enable bit: Enable Received Data Available Interrupt (fires while the RX FIFO is nonempty).
const IER_ERBFI: u8 = 0b0000_0001;

/// A handle to one NS16550. Just a base pointer, like `Pl011`.
pub struct Ns16550 {
    base: *mut u8,
}

// SAFETY: the pointer names MMIO, not memory Rust manages. Concurrent use is excluded by the
// console's lock, not by this type, exactly as for `Pl011`.
unsafe impl Send for Ns16550 {}

impl Ns16550 {
    /// # Safety
    /// `base` must be the address of a real, mapped NS16550 register block.
    pub const unsafe fn new(base: usize) -> Self {
        Self {
            base: base as *mut u8,
        }
    }

    fn read(&self, off: usize) -> u8 {
        // SAFETY: `off` is one of the register offsets above, within the block promised by `new`.
        unsafe { core::ptr::read_volatile(self.base.add(off)) }
    }

    fn write(&self, off: usize, val: u8) {
        // SAFETY: as `read`.
        unsafe { core::ptr::write_volatile(self.base.add(off), val) }
    }

    /// Configure the UART: 8 data bits, no parity, one stop bit, FIFOs on, interrupts off (this is
    /// a polling console). QEMU ignores the baud divisor (there is no real wire), but a real 16550
    /// needs it, so we set it, mirroring the PL011 driver's stance.
    pub fn init(&self) {
        self.write(IER, 0x00); // interrupts off: the console polls LSR

        // Program the baud divisor behind DLAB. 115200 from the standard 1.8432 MHz UART clock is
        // divisor 1; QEMU does not care, a real part will.
        self.write(LCR, LCR_DLAB);
        self.write(THR, 0x01); // divisor low
        self.write(IER, 0x00); // divisor high
        self.write(LCR, LCR_8N1); // clears DLAB, sets 8N1

        self.write(FCR, FCR_ENABLE_CLEAR);
    }

    /// Write one byte, spinning until the transmit holding register has room.
    pub fn write_byte(&self, byte: u8) {
        while self.read(LSR) & LSR_THRE == 0 {
            core::hint::spin_loop();
        }
        self.write(THR, byte);
    }

    /// Turn on the receive-data-available interrupt. After this, the UART raises its interrupt line
    /// (into the PLIC) whenever a byte sits unread in the RX buffer. It is **level-triggered**: the
    /// line stays asserted until the byte is read, so *something* must read the byte to quiet it
    /// before completing the interrupt, or it re-fires immediately. That something is the userspace
    /// input driver, which holds this device's registers as a capability; the kernel arms the
    /// interrupt and reads nothing. The console still polls for transmit.
    pub fn enable_rx_interrupt(&self) {
        self.write(IER, IER_ERBFI);
    }
}

impl core::fmt::Write for Ns16550 {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            // Terminals want CRLF; Rust gives us LF.
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
        Ok(())
    }
}
