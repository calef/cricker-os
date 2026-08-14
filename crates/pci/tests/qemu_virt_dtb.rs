//! Hold the kernel's PCI reads and hardcodes against the machine's own device tree.
//!
//! The kernel's PCIe windows come from the tree at boot (`memory::init` reads the
//! `pci-host-ecam-generic` node's `reg` and `ranges`; the parse is this crate's
//! `mem32_window`); the tests here run the same reads over the fixture trees and pin the
//! answers, so a QEMU layout change fails on the host before the kernel maps the wrong window
//! on the machine. What is still a hardcode under witness: `PCI_IRQ_BASE` in each
//! kernel/src/arch/*/mmu.rs and the swizzle in this crate (`intx_irq`); a bare-metal crate
//! cannot be a dev-dependency, so those values are asserted as literals, the same
//! hardcode-with-a-witness pattern as the UART test in the dtb crate.

use dtb::{Dtb, Region};
use pci::{intx_irq, mem32_window};

const QEMU_RISCV_VIRT: &[u8] = include_bytes!("../../dtb/tests/fixtures/qemu-riscv64-virt.dtb");

/// The discovery `memory::init` performs, over the riscv `virt` tree: the node found by its
/// binding, the ECAM window from its `reg`, the BAR window from its `ranges`. The pinned values
/// are the constants the kernel carried until the first VisionFive 2 boot (`PCI_ECAM_BASE`,
/// `PCI_BAR_BASE`), which is the regression claim: on QEMU, discovery changes provenance and
/// nothing else. The kernel-side twin (same claim against the live tree, on the machine) is the
/// test in kernel/src/pci.rs.
#[test]
fn discovery_finds_the_riscv_windows_the_constants_named() {
    let dtb = Dtb::from_bytes(QEMU_RISCV_VIRT).unwrap();
    let mut regs = [Region { start: 0, size: 0 }; 2];
    assert_eq!(
        dtb.node_reg_compatible(b"pci-host-ecam-generic", &mut regs)
            .unwrap(),
        1
    );
    assert_eq!(
        regs[0],
        Region {
            start: 0x3000_0000, // was PCI_ECAM_BASE in arch/riscv64/mmu.rs
            size: 0x1000_0000,  // 256 buses x 1 MB; the kernel maps bus 0's megabyte
        }
    );
    let ranges = dtb
        .node_prop_compatible(b"pci-host-ecam-generic", b"ranges")
        .unwrap()
        .expect("the bridge states its ranges");
    assert_eq!(
        mem32_window(ranges),
        Some((0x4000_0000, 0x4000_0000)), // base was PCI_BAR_BASE in arch/riscv64/mmu.rs
    );
}

const JH7110: &[u8] = include_bytes!("../../isa/tests/fixtures/jh7110.dtb");

/// **The JH7110 describes no generic-ECAM bridge, and discovery must say so.** Its PCIe
/// controller is a PLDA core (`starfive,jh7110-pcie`), a different device this kernel does not
/// drive; the honest answer is None, no window mapped, every probe reporting nobody home. The
/// alternative was the first-silicon panic: QEMU's BAR constant (`0x4000_0000`) is this board's
/// DRAM base, and mapping it collided with the direct map (notes/visionfive2.md).
#[test]
fn the_jh7110_has_no_generic_ecam_bridge_and_discovery_says_none() {
    let dtb = Dtb::from_bytes(JH7110).unwrap();
    let mut regs = [Region { start: 0, size: 0 }; 2];
    assert_eq!(
        dtb.node_reg_compatible(b"pci-host-ecam-generic", &mut regs)
            .unwrap(),
        0,
        "no node claims the generic-ECAM binding"
    );
    assert_eq!(
        dtb.node_prop_compatible(b"pci-host-ecam-generic", b"ranges")
            .unwrap(),
        None,
        "and no ranges either: both reads memory::init performs come back empty"
    );
}

/// `intx_irq`'s swizzle formula reproduces every entry of the machine's own `interrupt-map`.
///
/// The map is the authoritative routing table: entries of six 32-bit cells here
/// (child-addr:3, whose high cell carries the device number at bits 11+; pin:1; the PLIC
/// phandle:1; the PLIC input:1: widths fixed by the pci node's `#address-cells = 3`,
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

const QEMU_AARCH64_VIRT: &[u8] = include_bytes!("../../dtb/tests/fixtures/qemu-aarch64-virt.dtb");

/// The aarch64 discovery, same claim as the riscv one, with the machine's own twist: the ECAM is
/// the **highmem** window. The node is named `pcie@10000000` (the low MMIO base) but its `reg`
/// is `0x40_1000_0000`, and trusting the name instead of the `reg` is exactly the mistake this
/// witness exists to catch.
#[test]
fn discovery_finds_the_aarch64_windows_the_constants_named() {
    let dtb = Dtb::from_bytes(QEMU_AARCH64_VIRT).unwrap();
    let mut regs = [Region { start: 0, size: 0 }; 2];
    assert_eq!(
        dtb.node_reg_compatible(b"pci-host-ecam-generic", &mut regs)
            .unwrap(),
        1
    );
    assert_eq!(
        regs[0],
        Region {
            start: 0x40_1000_0000, // was PCI_ECAM_BASE in arch/aarch64/mmu.rs
            size: 0x1000_0000,
        }
    );
    let ranges = dtb
        .node_prop_compatible(b"pci-host-ecam-generic", b"ranges")
        .unwrap()
        .expect("the bridge states its ranges");
    assert_eq!(
        mem32_window(ranges),
        Some((0x1000_0000, 0x2eff_0000)), // base was PCI_BAR_BASE in arch/aarch64/mmu.rs
    );
}

/// The swizzle against the aarch64 machine's `interrupt-map`. Entries are TEN cells here, not
/// six: the GIC's `#interrupt-cells = 3` (type, number, flags) where the PLIC's is 1, plus a
/// two-cell parent unit address because the GIC also declares `#address-cells = 2` (the spec
/// says a map entry carries the parent's address cells; the PLIC declares zero). Every entry
/// must be a type-0 (SPI) interrupt, and `intx_irq(35, dev, pin)` (INTID 32 + SPI 3 as the
/// base, from arch/aarch64/mmu.rs) must reproduce `32 + SPI` for all sixteen.
#[test]
fn the_intx_swizzle_matches_the_aarch64_interrupt_map() {
    let dtb = Dtb::from_bytes(QEMU_AARCH64_VIRT).unwrap();
    let map = dtb
        .node_prop(b"pcie@", b"interrupt-map")
        .unwrap()
        .expect("the pcie node has no interrupt-map");

    const ENTRY_CELLS: usize = 10; // addr:3, pin:1, phandle:1, parent-addr:2, gic (type, spi, flags):3
    let cell = |i: usize| -> u32 { u32::from_be_bytes(map[i * 4..i * 4 + 4].try_into().unwrap()) };
    assert_eq!(map.len() % (ENTRY_CELLS * 4), 0, "unexpected entry width");
    let entries = map.len() / (ENTRY_CELLS * 4);
    assert_eq!(entries, 16, "QEMU virt routes 4 devices x 4 pins");

    for e in 0..entries {
        let at = e * ENTRY_CELLS;
        let dev = ((cell(at) >> 11) & 0x1f) as u8;
        let pin = cell(at + 3) as u8;
        let gic_type = cell(at + 7);
        let spi = cell(at + 8);
        assert_eq!(gic_type, 0, "PCI INTx must arrive as an SPI");
        assert_eq!(
            intx_irq(35, dev, pin), // 35 = PCI_IRQ_BASE in arch/aarch64/mmu.rs
            32 + spi,               // SPIs start at INTID 32
            "swizzle disagrees with the machine for device {dev} pin {pin}",
        );
    }
}
