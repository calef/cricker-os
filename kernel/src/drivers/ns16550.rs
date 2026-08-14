//! NS16550 UART driver (RISC-V console).
//!
//! The other ancient, beautifully dumb serial port. Where aarch64's `virt` machine has a PL011,
//! RISC-V's has an NS16550 (a 16550-compatible 8250) at `0x1000_0000`. Same idea, different
//! register block: eight registers, and the transmit-ready flag lives in the Line Status Register
//! instead of a Flag Register.
//!
//! **The register block's shape is the board's, not the architecture's**, and since the
//! VisionFive 2 prep (notes/visionfive2.md) this driver carries it as data ([`Shape`]) instead of
//! assuming QEMU's. QEMU `virt` wires byte-wide registers at consecutive addresses; the JH7110's
//! UART0 is a Synopsys DesignWare DW_apb_uart, an 8250 whose registers sit **four bytes apart**
//! (`reg-shift = <2>`), want **32-bit accesses** (`reg-io-width = <4>`), and run from a **24 MHz**
//! clock, so the old byte read of LSR at offset 5 landed in the middle of the IER word and the
//! transmit poll would spin on garbage forever. The shape comes from the device tree
//! (`console::configure_from_dtb`); until that runs, [`Shape::QEMU_VIRT`] keeps the behavior this
//! driver always had, byte for byte.
//!
//! This one uses plain volatile access rather than `tock_registers` register blocks: the register
//! *indices* are what the 16550 defines, and the stride between them is a runtime value no static
//! layout macro can express. Named offsets and bit masks are clearer for a device this small.
//!
//! Same rule as every driver here (DECISIONS.md §4): **it reaches into no globals.** It is
//! constructed with a base address and a shape and knows nothing else. It is the sibling of
//! `pl011.rs`, selected by the console at compile time. See notes/riscv-port.md.

// Register indices from the UART base (multiply by the stride, `1 << reg_shift`, for the byte
// offset: index 5 is byte offset 5 on QEMU `virt` and byte offset 0x14 on the JH7110).
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
// Line Status bit: Transmitter Empty (holding register AND shift register drained). The DW busy
// quirk's precondition: a DW_apb_uart ignores an LCR write while it is busy, so LCR is only
// touched once the transmitter is completely idle.
const LSR_TEMT: u8 = 0b0100_0000;
// Interrupt Enable bit: Enable Received Data Available Interrupt (fires while the RX FIFO is nonempty).
const IER_ERBFI: u8 = 0b0000_0001;
// Interrupt Enable bit: Enable Transmitter Holding Register Empty Interrupt. Asserts as soon as it is
// set if LSR.THRE is already set, which on a polling console it always is. See `enable_tx_interrupt`.
// Test builds only, because that is where its only caller is; milestone 41's removal of the crate-wide
// riscv `allow(dead_code)` means an unused constant here is a build error, which is the point.
#[cfg(test)]
const IER_ETBEI: u8 = 0b0000_0010;

/// The console's line rate, which every machine here runs at (QEMU has no real wire and does not
/// care; the VisionFive 2's serial header is documented at 115200 8N1).
const BAUD: u32 = 115_200;

/// **How this particular 16550 is wired**: the facts the device tree states about the register
/// block, plus what to program the divisor to. One struct so the console can swap all of it in a
/// single call, and `const`-constructible so the pre-device-tree default costs nothing.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Shape {
    /// `reg-shift`: log2 of the distance in bytes between consecutive registers. 0 on QEMU `virt`,
    /// 2 on the JH7110.
    pub reg_shift: u8,
    /// `reg-io-width`: the access size in bytes, 1 or 4. Honored only when the shifted offset is
    /// 4-byte aligned (`reg_shift >= 2`), because an unaligned 32-bit volatile access would trap;
    /// any other combination falls back to byte access, which is also what a width this driver
    /// does not know (2, 8) gets.
    pub reg_io_width: u8,
    /// What to program the baud divisor to, or **0 to leave the divisor and line controls alone**
    /// and trust whatever firmware programmed. Zero is the honest choice when the tree states no
    /// `clock-frequency`: a wrong guess is 1.5 Mbaud garbage at the far terminal (divisor 1 at
    /// 24 MHz), while U-Boot has already set 115200 8N1 on any board that showed a prompt.
    pub divisor: u16,
    /// The DesignWare busy quirk (`snps,dw-apb-uart`): the part ignores an LCR write while the
    /// transmitter is busy and latches a "busy" interrupt. When set, `init` drains the transmitter
    /// (LSR.TEMT, bounded) before touching LCR.
    pub dw_busy_quirk: bool,
}

impl Shape {
    /// QEMU `virt`'s wiring, and this driver's entire behavior before the VisionFive 2 prep:
    /// consecutive byte registers, and divisor 1 (115200 from the standard 1.8432 MHz UART clock),
    /// which QEMU ignores. The default until `console::configure_from_dtb` reads the real answer.
    pub const QEMU_VIRT: Shape = Shape {
        reg_shift: 0,
        reg_io_width: 1,
        divisor: 1,
        dw_busy_quirk: false,
    };

    /// The divisor for [`BAUD`] from a stated input clock, rounded to nearest: a 16550 divides the
    /// clock by 16 x divisor. 24 MHz gives 13 (actual rate 115385, 0.16% high, well inside
    /// tolerance); QEMU `virt`'s stated 3.6864 MHz gives exactly 2.
    pub const fn divisor_for(clock_hz: u32) -> u16 {
        ((clock_hz + 8 * BAUD) / (16 * BAUD)) as u16
    }
}

// The two clocks this kernel expects to meet, proved at compile time so the arithmetic cannot
// drift: the JH7110's 24 MHz and QEMU virt's 3.6864 MHz (notes/visionfive2.md).
const _: () = {
    assert!(Shape::divisor_for(24_000_000) == 13);
    assert!(Shape::divisor_for(3_686_400) == 2);
};

/// A handle to one NS16550: a base pointer and the block's [`Shape`].
pub struct Ns16550 {
    base: *mut u8,
    shape: Shape,
}

// SAFETY: the pointer names MMIO, not memory Rust manages. Concurrent use is excluded by the
// console's lock, not by this type, exactly as for `Pl011`.
unsafe impl Send for Ns16550 {}

impl Ns16550 {
    /// # Safety
    /// `base` must be the address of a real, mapped NS16550 register block. The shape starts as
    /// [`Shape::QEMU_VIRT`]; [`configure`](Self::configure) replaces it once the device tree has
    /// been read.
    pub const unsafe fn new(base: usize) -> Self {
        Self {
            base: base as *mut u8,
            shape: Shape::QEMU_VIRT,
        }
    }

    /// Adopt the shape the device tree stated and re-run [`init`](Self::init) with it. Called by
    /// `console::configure_from_dtb` under the console lock, before the first `println!`, so no
    /// output is ever produced with a stale stride.
    pub fn configure(&mut self, shape: Shape) {
        self.shape = shape;
        self.init();
    }

    fn read(&self, reg: usize) -> u8 {
        let off = reg << self.shape.reg_shift;
        // SAFETY: `reg` is one of the register indices above; the shifted offset stays within the
        // block promised by `new` (the largest, LSR at shift 2, is 0x14 of a 0x10000 block). The
        // 32-bit arm requires 4-byte alignment, checked, because an unaligned volatile u32 traps.
        unsafe {
            if self.shape.reg_io_width == 4 && off & 3 == 0 {
                core::ptr::read_volatile(self.base.add(off) as *const u32) as u8
            } else {
                core::ptr::read_volatile(self.base.add(off))
            }
        }
    }

    fn write(&self, reg: usize, val: u8) {
        let off = reg << self.shape.reg_shift;
        // SAFETY: as `read`.
        unsafe {
            if self.shape.reg_io_width == 4 && off & 3 == 0 {
                core::ptr::write_volatile(self.base.add(off) as *mut u32, val as u32);
            } else {
                core::ptr::write_volatile(self.base.add(off), val);
            }
        }
    }

    /// Configure the UART: FIFOs on, interrupts off (this is a polling console), and, when the
    /// divisor is known, 8N1 at [`BAUD`].
    ///
    /// A shape with `divisor == 0` deliberately touches neither LCR nor the divisor latch: it
    /// means the input clock is unstated, and reprogramming against a guessed clock is how a
    /// working firmware console turns to garbage (the Shape field's doc has the arithmetic). On
    /// the DW part the busy quirk is honored first: LCR writes are ignored while the transmitter
    /// is busy, so it is drained, bounded, before being touched.
    pub fn init(&self) {
        self.write(IER, 0x00); // interrupts off: the console polls LSR

        if self.shape.divisor != 0 {
            if self.shape.dw_busy_quirk {
                // Drain the transmitter so the LCR writes below take. Bounded: at 115200 the
                // FIFO and shift register drain in low milliseconds, so a bound this size only
                // ever trips on silicon that is not answering, and then skipping the reprogram
                // (firmware's settings stay) beats hanging a console nobody can see yet.
                let mut spins = 1_000_000u32;
                while self.read(LSR) & LSR_TEMT == 0 && spins > 0 {
                    core::hint::spin_loop();
                    spins -= 1;
                }
            }
            // Program the baud divisor behind DLAB, then drop back to 8N1 (which clears DLAB).
            self.write(LCR, LCR_DLAB);
            self.write(THR, (self.shape.divisor & 0xff) as u8); // divisor low
            self.write(IER, (self.shape.divisor >> 8) as u8); // divisor high
            self.write(LCR, LCR_8N1);
        }

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

    /// Turn on the transmit-holding-register-empty interrupt, and turn it off again.
    ///
    /// A 16550 asserts its line the moment `IER.ETBEI` is set while `LSR.THRE` is already set, and
    /// on a console that has just finished printing THRE is *always* set. So this pair is a way to
    /// raise and lower this device's interrupt line with two register writes, no transfer, no
    /// external stimulus and nothing to read back. That is what `kernel::sched`'s RISC-V
    /// interrupt-delivery tests use in place of aarch64's SGI, which RISC-V has no equivalent of
    /// (notes/interrupts.md, "Testing it with no device on RISC-V").
    ///
    /// It is 16550 architecture, not a QEMU behaviour, so it should carry to any 16550-compatible
    /// part (the VisionFive 2's UART is a `DesignWare` 8250). Nothing in this kernel drives transmit
    /// by interrupt (the console polls `LSR`), so an asserted THRE line has no other consumer.
    ///
    /// Test builds only: a production caller would be a transmit-interrupt console, which this is
    /// deliberately not.
    #[cfg(test)]
    pub fn enable_tx_interrupt(&self) {
        self.write(IER, IER_ETBEI);
    }

    /// Mask every interrupt source in this UART, quieting a line raised by
    /// [`enable_tx_interrupt`]. The console is a polling console, so all-off is its resting state
    /// (`init` writes the same value). Test builds only, for the same reason as its partner above.
    #[cfg(test)]
    pub fn disable_interrupts(&self) {
        self.write(IER, 0x00);
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
