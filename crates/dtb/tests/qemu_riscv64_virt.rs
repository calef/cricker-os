//! Parse the RISC-V virt board's device tree, the second machine we boot on.
//!
//! The aarch64 twin of this file (`qemu_aarch64_virt.rs`) explains why these run on the host. This one
//! exists because the riscv boot path leans on two parser features the aarch64 tree never
//! exercises: a device nested under `/soc` (the PLIC), and the `/reserved-memory` node OpenSBI
//! uses to fence off its own firmware. Without these tests, both paths were only ever executed
//! inside a booting kernel, where a parsing bug surfaces as an allocator handing out firmware
//! RAM, far from its cause.
//!
//! Regenerate the fixture with:
//!
//!     qemu-system-riscv64 -machine virt,dumpdtb=f.dtb -nographic
//!     dtc -I dtb -O dts f.dtb -o crates/dtb/tests/fixtures/qemu-riscv64-virt.dts
//!     (re-add the /reserved-memory node; see the comment in the .dts for why it is hand-added)
//!     dtc -I dts -O dtb crates/dtb/tests/fixtures/qemu-riscv64-virt.dts \
//!         -o crates/dtb/tests/fixtures/qemu-riscv64-virt.dtb

use dtb::{Dtb, Region};

const QEMU_RISCV_VIRT: &[u8] = include_bytes!("fixtures/qemu-riscv64-virt.dtb");

#[test]
fn finds_the_ram() {
    let dtb = Dtb::from_bytes(QEMU_RISCV_VIRT).unwrap();
    let mut regions = [Region { start: 0, size: 0 }; 8];
    let n = dtb.memory_regions(&mut regions).unwrap();

    assert_eq!(n, 1, "riscv virt has one memory node");
    assert_eq!(
        regions[0],
        Region {
            start: 0x8000_0000, // riscv virt's RAM base, not aarch64's 0x4000_0000
            size: 0x800_0000,   // 128 MiB, QEMU's default
        }
    );
}

/// The PLIC sits under `/soc`, not at the top level, and its `reg` must be decoded with the
/// cell counts `/soc` declares (2/2), not the PLIC's own `#address-cells = <0>` (which applies
/// to the PLIC's *children*). This is the exact confusion `node_reg`'s per-depth stack exists to
/// prevent; get it wrong and the decoded address is garbage, silently.
#[test]
fn finds_the_plic_nested_under_soc() {
    let dtb = Dtb::from_bytes(QEMU_RISCV_VIRT).unwrap();
    let mut regions = [Region { start: 0, size: 0 }; 4];
    let n = dtb.node_reg(b"plic@", &mut regions).unwrap();

    assert_eq!(n, 1, "the PLIC has one register block (the GIC has two)");
    assert_eq!(
        regions[0],
        Region {
            start: 0xc00_0000,
            size: 0x60_0000,
        }
    );
}

/// The OpenSBI firmware regions, from the `/reserved-memory` node. Missing these is the bug the
/// function exists to prevent: the frame allocator hands out OpenSBI's RAM and the first write
/// faults on a PMP violation, in code nowhere near the allocator.
#[test]
fn finds_opensbis_reserved_memory() {
    let dtb = Dtb::from_bytes(QEMU_RISCV_VIRT).unwrap();
    let mut regions = [Region { start: 0, size: 0 }; 8];
    let n = dtb.reserved_memory_regions(&mut regions).unwrap();

    assert_eq!(n, 2, "OpenSBI reserves two regions on virt");
    assert_eq!(
        regions[0],
        Region {
            start: 0x8000_0000,
            size: 0x4_0000,
        }
    );
    assert_eq!(
        regions[1],
        Region {
            start: 0x8004_0000,
            size: 0x4_0000,
        }
    );
}

/// **The RTC, found by binding rather than by label** (milestone 51). The twin of the aarch64
/// fixture's test: same call, different `compatible`, different address, and the node name here
/// (`rtc@101000`) shares no prefix with aarch64's `pl031@9010000`.
#[test]
fn finds_the_rtc_by_compatible() {
    let dtb = Dtb::from_bytes(QEMU_RISCV_VIRT).unwrap();
    let mut regs = [Region { start: 0, size: 0 }; 2];

    assert_eq!(
        dtb.node_reg_compatible(b"google,goldfish-rtc", &mut regs)
            .unwrap(),
        1
    );
    assert_eq!(
        regs[0],
        Region {
            start: 0x0010_1000,
            size: 0x1000,
        }
    );
}

/// And the PL031 is not on this board. Both boots probe for both devices, so both absences are
/// answers the code takes every time it runs.
#[test]
fn the_pl031_is_absent_on_riscv() {
    let dtb = Dtb::from_bytes(QEMU_RISCV_VIRT).unwrap();
    let mut regs = [Region { start: 0, size: 0 }; 2];
    assert_eq!(dtb.node_reg_compatible(b"arm,pl031", &mut regs).unwrap(), 0);
}
