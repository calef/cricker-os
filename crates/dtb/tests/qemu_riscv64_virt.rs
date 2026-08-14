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

/// **A property, found by binding rather than by label.** `node_prop_compatible` exists because
/// the JH7110 names its PLIC node `interrupt-controller@c000000` where QEMU says `plic@c000000`,
/// so a name-prefix read of `interrupts-extended` works on one machine and silently finds nothing
/// on the other. On this (single-hart) dump the property is two entries of two cells: context 0 is
/// the hart's M external (11), context 1 its S external (9), which is the `2h+1` layout the kernel
/// used to assume; reading it from the machine is what lets the JH7110's different answer in.
#[test]
fn finds_the_plics_context_list_by_compatible() {
    let dtb = Dtb::from_bytes(QEMU_RISCV_VIRT).unwrap();
    let prop = dtb
        .node_prop_compatible(b"sifive,plic-1.0.0", b"interrupts-extended")
        .unwrap()
        .expect("the PLIC states its context wiring");

    assert_eq!(prop.len(), 16, "two (phandle, irq) entries of two cells");
    let cell = |i: usize| u32::from_be_bytes(prop[i * 4..i * 4 + 4].try_into().unwrap());
    assert_eq!(cell(1), 11, "context 0 is a machine external");
    assert_eq!(cell(3), 9, "context 1 is the hart's supervisor external");
    assert_eq!(cell(0), cell(2), "both contexts belong to the same hart");

    // A compatible nothing here carries, and a property the node does not have: both are `None`,
    // not errors.
    assert_eq!(
        dtb.node_prop_compatible(b"acme,unobtainium", b"interrupts-extended")
            .unwrap(),
        None
    );
    assert_eq!(
        dtb.node_prop_compatible(b"sifive,plic-1.0.0", b"no-such-property")
            .unwrap(),
        None
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

/// **Every matching node, not just the first** (milestone 60). `node_prop` answers for one node and
/// stops, which is right for a device that appears once and wrong for `cpu@`: a heterogeneous
/// machine describes each hart separately, and reading the first one silently answers for a core
/// the scheduler may never place a thread on.
///
/// This dump is one hart, so the interesting half is the `None`: a matched node that lacks the
/// property is a different answer from a node that does not exist.
#[test]
fn reads_a_property_from_every_matching_node() {
    let dtb = Dtb::from_bytes(QEMU_RISCV_VIRT).unwrap();

    let mut isa = [None; 4];
    assert_eq!(
        dtb.node_props(b"cpu@", b"riscv,isa-base", &mut isa)
            .unwrap(),
        1
    );
    assert_eq!(isa[0], Some(&b"rv64i\0"[..]));
    assert_eq!(isa[1], None, "untouched slots stay empty");

    // A node that exists and has no such property: matched, and reported as `None`.
    let mut absent = [Some(&b"stale"[..]); 4];
    assert_eq!(
        dtb.node_props(b"cpu@", b"mmu-flavour", &mut absent)
            .unwrap(),
        1
    );
    assert_eq!(
        absent[0], None,
        "the slot is cleared, not left holding the caller's junk"
    );

    // A prefix nothing matches.
    let mut nothing = [None; 4];
    assert_eq!(dtb.node_props(b"gpu@", b"reg", &mut nothing).unwrap(), 0);
}

/// **More matching nodes than slots is counted, not hidden.** A caller that under-provisions its
/// array gets a count larger than the array and can say so, which is the difference between "this
/// machine has four harts and I read two" and "this machine has two harts".
#[test]
fn more_nodes_than_slots_reports_the_real_count() {
    let dtb = Dtb::from_bytes(QEMU_RISCV_VIRT).unwrap();

    // Every node under the root, matched by the empty prefix, into no space at all. The count is
    // the honest one; nothing was written, because there was nowhere to write it.
    let mut none: [Option<&[u8]>; 0] = [];
    let all = dtb.node_props(b"", b"compatible", &mut none).unwrap();
    assert!(
        all > 1,
        "the tree has many nodes, and it said so with nowhere to put them"
    );
}
