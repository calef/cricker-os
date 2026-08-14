//! The JH7110's disabled monitor core, on the host, before the board ever runs a byte of ours.
//!
//! The VisionFive 2 prep (notes/visionfive2.md). `fixtures/jh7110.dts` is hand-written, modeled on
//! the mainline `jh7110.dtsi` the note cites, and this file holds the `status = "disabled"` half
//! of what it witnesses: the S7 monitor core's ISA and MMU must not narrow the machine record, and
//! the roster must still know the hart exists and may not be started. The same fixture's PLIC
//! context layout is tested in `riscv64_plic_contexts.rs`.

use isa::cpu_list::CpuList;
use isa::riscv64::*;

const JH7110: &[u8] = include_bytes!("fixtures/jh7110.dtb");

fn tree(bytes: &[u8]) -> dtb::Dtb<'_> {
    dtb::Dtb::from_bytes(bytes).expect("fixture is a valid device tree")
}

/// **The disabled S7 does not speak for the machine.** Four schedulable U74s, `rv64imafdc`, Sv39,
/// homogeneous; the S7's `rv64imac` and MMU-less `mmu-type` contribute nothing. Before the
/// `status` gate, `common` lost F and D to the monitor core and the fixture's `riscv,none` read as
/// a machine that cannot run us at all.
#[test]
fn the_disabled_s7_does_not_narrow_the_machine() {
    let mut cpu = Isa::from_device_tree(&tree(JH7110)).expect("the CPU nodes parse");
    cpu.sbi.spec_major = 2;
    cpu.sbi.extensions = SBI_REQUIRED;

    assert_eq!(cpu.harts, 5, "five cpu@ nodes, honestly counted");
    assert_eq!(cpu.described, 4, "but only the four enabled harts speak");
    assert_eq!(cpu.base, Base::Rv64);
    assert!(cpu.legacy_isa_string, "the JH7110 tree predates isa-extensions");
    assert!(
        cpu.common.contains(F.union(D)),
        "the U74s' FPU survives: the S7's rv64imac did not narrow the intersection"
    );
    assert!(
        !cpu.heterogeneous(),
        "the four harts the kernel can run on are identical; heterogeneity was the S7's"
    );
    assert_eq!(
        cpu.mmu,
        MmuType::Sv39,
        "the S7's riscv,none must not be the narrowest MMU: the kernel never runs there"
    );
    assert!(
        !cpu.missing_requirements().any(),
        "the VisionFive 2 can run this kernel, and saying otherwise was the bug"
    );
}

/// The roster still records the S7, as unusable, so the bring-up path refuses it by name instead
/// of never seeing it. This half already worked (milestone 100 reads `status`); pinned here so the
/// two walks cannot drift apart.
#[test]
fn the_cpu_list_knows_the_s7_is_unusable() {
    let list = CpuList::from_device_tree(&tree(JH7110)).expect("cannot read /cpus");

    assert_eq!(list.described, 5);
    assert_eq!(list.cpus()[0].hwid, 0);
    assert!(!list.cpus()[0].usable, "hart 0 is the disabled S7");
    for (i, cpu) in list.cpus().iter().enumerate().skip(1) {
        assert_eq!(cpu.hwid, i as u64);
        assert!(cpu.usable, "hart {i} is a U74 the kernel may start");
    }
    assert_eq!(list.timebase_hz, Some(4_000_000));
}

/// **The DW-8250's shape, straight off the serial node.** These are exactly the reads
/// `console::configure_from_dtb` performs (reg-shift, reg-io-width, clock-frequency, and the
/// compatible that turns on the busy quirk), against the same node name the kernel pins
/// (`serial@10000000`), so the UART facts in notes/visionfive2.md have a machine-readable witness
/// and the kernel's hardcoded node name has one too.
#[test]
fn the_jh7110_serial_node_states_the_dw8250_shape() {
    let dt = tree(JH7110);
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);

    let shift = dt.node_prop(b"serial@10000000", b"reg-shift").unwrap();
    let width = dt.node_prop(b"serial@10000000", b"reg-io-width").unwrap();
    let clock = dt.node_prop(b"serial@10000000", b"clock-frequency").unwrap();
    let compat = dt.node_prop(b"serial@10000000", b"compatible").unwrap();

    assert_eq!(shift.map(be32), Some(2), "registers four bytes apart");
    assert_eq!(width.map(be32), Some(4), "32-bit accesses");
    assert_eq!(clock.map(be32), Some(24_000_000), "divisor 13 for 115200");
    assert!(
        compat
            .expect("the node names its binding")
            .split(|&b| b == 0)
            .any(|s| s == b"snps,dw-apb-uart"),
        "the DW compatible is what enables the busy quirk"
    );
}

