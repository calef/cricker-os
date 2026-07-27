//! Hold the kernel's PCI hardcodes against the machine's own device tree.
//!
//! The kernel constants under witness live in kernel/src/arch/riscv64/mmu.rs (`PCI_ECAM_BASE`,
//! `PCI_IRQ_BASE`) and the swizzle in this crate (`intx_irq`); a bare-metal crate cannot be a
//! dev-dependency, so the values are asserted as literals here, the same
//! hardcode-with-a-witness pattern as the UART test in the dtb crate. If QEMU ever moves the
//! ECAM window or routes INTx differently, these fail on the host before the kernel misroutes
//! anything on the machine.

use dtb::{Dtb, Region};
use pci::intx_irq;

const QEMU_RISCV_VIRT: &[u8] = include_bytes!("../../dtb/tests/fixtures/qemu-riscv-virt.dtb");

/// The ECAM window the kernel maps (PCI_ECAM_BASE) is where the machine says its
/// `pci-host-ecam-generic` bridge lives.
#[test]
fn the_ecam_window_matches_the_machine() {
    let dtb = Dtb::from_bytes(QEMU_RISCV_VIRT).unwrap();
    let mut regs = [Region { start: 0, size: 0 }; 2];
    assert_eq!(dtb.node_reg(b"pci@", &mut regs).unwrap(), 1);
    assert_eq!(
        regs[0],
        Region {
            start: 0x3000_0000, // PCI_ECAM_BASE in arch/riscv64/mmu.rs
            size: 0x1000_0000,  // 256 buses x 1 MB; the kernel maps bus 0's megabyte
        }
    );
}

/// `intx_irq`'s swizzle formula reproduces every entry of the machine's own `interrupt-map`.
///
/// The map is the authoritative routing table: entries of six 32-bit cells here
/// (child-addr:3, whose high cell carries the device number at bits 11+; pin:1; the PLIC
/// phandle:1; the PLIC input:1 — widths fixed by the pci node's `#address-cells = 3`,
/// `#interrupt-cells = 1`, and the PLIC's `#interrupt-cells = 1`). QEMU's riscv virt maps four
/// devices x four pins; the formula must agree on all sixteen, not just the one device we
/// happen to attach.
#[test]
fn the_intx_swizzle_matches_the_interrupt_map() {
    let dtb = Dtb::from_bytes(QEMU_RISCV_VIRT).unwrap();
    let map = dtb
        .node_prop(b"pci@", b"interrupt-map")
        .unwrap()
        .expect("the pci node has no interrupt-map");

    const ENTRY_CELLS: usize = 6;
    let cell = |i: usize| -> u32 { u32::from_be_bytes(map[i * 4..i * 4 + 4].try_into().unwrap()) };
    assert_eq!(map.len() % (ENTRY_CELLS * 4), 0, "unexpected entry width");
    let entries = map.len() / (ENTRY_CELLS * 4);
    assert_eq!(entries, 16, "QEMU virt routes 4 devices x 4 pins");

    for e in 0..entries {
        let at = e * ENTRY_CELLS;
        let dev = ((cell(at) >> 11) & 0x1f) as u8;
        let pin = cell(at + 3) as u8;
        let plic_input = cell(at + 5);
        assert_eq!(
            intx_irq(32, dev, pin), // 32 = PCI_IRQ_BASE in arch/riscv64/mmu.rs
            plic_input,
            "swizzle disagrees with the machine for device {dev} pin {pin}",
        );
    }
}
