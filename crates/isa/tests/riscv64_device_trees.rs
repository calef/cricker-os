//! Reading the ISA out of real and hand-written device trees, on the host, in milliseconds.
//!
//! Three trees, and each is here for a reason no other one covers:
//!
//! * **QEMU's own `virt`**, borrowed from the `dtb` crate's fixtures so the two crates cannot
//!   disagree about what the machine we test on says. This is the tree the RISC-V suite actually
//!   boots against, and the surprise in it is the whole argument for the milestone.
//! * **`mixed-cpus`**, hand-written, for the shapes QEMU never produces: the deprecated
//!   `riscv,isa` string, the `g` abbreviation, and harts that differ from each other.
//! * **`narrow-machine`**, hand-written, so the refusal path has a witness rather than only a
//!   comment saying it would refuse.

use isa::riscv64::*;

/// The device tree of the machine the RISC-V suite boots on, shared with the `dtb` crate's own
/// fixtures rather than copied, so a regenerated tree cannot leave the two crates testing
/// different machines.
const QEMU_VIRT: &[u8] = include_bytes!("../../dtb/tests/fixtures/qemu-riscv64-virt.dtb");
const MIXED: &[u8] = include_bytes!("fixtures/mixed-cpus.dtb");
const NARROW: &[u8] = include_bytes!("fixtures/narrow-machine.dtb");

/// Read the tree only. The `sbi` half of the record stays at its default, which is "no firmware
/// answered", because a device tree cannot speak for the firmware.
fn parse_tree(bytes: &[u8]) -> Isa {
    let dt = dtb::Dtb::from_bytes(bytes).expect("fixture is a valid device tree");
    Isa::from_device_tree(&dt).expect("the CPU nodes parse")
}

/// Read the tree, then fill the firmware half the way a real boot does. Every test below except
/// the SBI one is about what the tree says, and a record nobody finished populating would fail
/// them all for the same uninteresting reason.
fn parse(bytes: &[u8]) -> Isa {
    let mut cpu = parse_tree(bytes);
    cpu.sbi.spec_major = 2;
    cpu.sbi.extensions = SBI_REQUIRED;
    cpu
}

/// **QEMU `virt` declares Sv57, and the kernel runs Sv39.**
///
/// This is the finding that justifies reading `mmu-type` at all, and it is the opposite of the
/// failure everyone expects. The worry going in was a board too narrow for us; the machine we have
/// been developing on for two milestones is *wider* than the kernel, by two whole page-table
/// levels, and nothing anywhere in the tree said so. Neither direction is visible without reading
/// the property, which is the point.
#[test]
fn qemu_virt_offers_more_than_we_use() {
    let cpu = parse(QEMU_VIRT);

    assert_eq!(cpu.mmu, MmuType::Sv57);
    assert!(
        cpu.mmu > MmuType::Sv39,
        "we configure Sv39 and could ask for more"
    );
    assert!(
        !cpu.missing_requirements().any(),
        "QEMU virt can run this kernel"
    );
}

/// The QEMU machine, in full, as the boot line reports it.
#[test]
fn qemu_virt_is_one_wide_hart() {
    let cpu = parse(QEMU_VIRT);

    assert_eq!(
        cpu.harts, 1,
        "the suite runs -smp 1 by default in this dump"
    );
    assert_eq!(cpu.described, 1);
    assert_eq!(cpu.base, Base::Rv64);
    assert!(!cpu.legacy_isa_string, "QEMU emits riscv,isa-extensions");
    assert!(!cpu.heterogeneous(), "one hart cannot differ from itself");

    // Everything in the table that QEMU's default `rv64` model has: eleven of thirteen rows, out
    // of the forty-eight extensions it declares. `-cpu rv64` turns on nearly every ratified
    // extension, which is exactly why it is a bad proxy for a board.
    for want in [I, M, A, C, F, D, H, ZICSR, ZIFENCEI, ZICBOM, SSTC] {
        assert!(cpu.common.contains(want), "QEMU virt declares it");
    }
    // And two it does not, which is the other half of the same point: even the maximal emulator
    // model is a specific machine, not all machines. `sstc` above is the one worth noticing in
    // the other direction: the timer could arm itself here and pays an SBI ecall anyway.
    assert!(
        !cpu.common.contains(V),
        "no vector on the default rv64 model"
    );
    assert!(
        !cpu.common.contains(SVINVAL),
        "and no Svinval, so sfence.vma is the only sweep"
    );
}

/// **The intersection is the set the kernel may assume, and it is narrower than any one hart.**
///
/// `cpu@0` has no FPU, `cpu@1` and `cpu@2` do. A kernel that read the first node would think the
/// machine has no FPU; one that read the last would think it has one everywhere. Both are wrong,
/// and `common` versus `any` is how the boot line can say so.
#[test]
fn heterogeneous_harts_intersect() {
    let cpu = parse(MIXED);

    assert_eq!(cpu.harts, 3);
    assert_eq!(cpu.described, 3);
    assert!(cpu.heterogeneous());

    assert!(cpu.common.contains(I.union(M).union(A).union(C)));
    assert!(
        !cpu.common.contains(F),
        "cpu@0 has no F, so the kernel has none"
    );
    assert!(cpu.any.contains(F), "but two of the three do");
    assert!(cpu.any.contains(D));
    assert!(
        cpu.any.contains(SSTC),
        "cpu@1 spells it in the legacy string's tail"
    );

    // The narrowest MMU any hart declares, not the widest. Sv57 on one core is no use to a thread
    // the scheduler might place on the Sv39 one.
    assert_eq!(cpu.mmu, MmuType::Sv39);

    // The tree can run us even though its cores disagree, because the intersection still has imac.
    assert!(!cpu.missing_requirements().any());
}

/// **`riscv,isa-base` is preferred, and the legacy string still answers when it is absent.**
///
/// Two of the three harts have no `riscv,isa-base`, so the base comes from the `rv64` prefix of
/// their `riscv,isa`, and the flag records that the answer went through the deprecated property.
#[test]
fn legacy_isa_string_is_read_and_flagged() {
    let cpu = parse(MIXED);

    assert_eq!(cpu.base, Base::Rv64);
    assert!(
        cpu.legacy_isa_string,
        "cpu@0 and cpu@1 have no riscv,isa-extensions"
    );
}

/// **A machine we cannot run on says so, in the three ways it is wrong at once.**
#[test]
fn narrow_machine_is_refused() {
    let cpu = parse(NARROW);
    let missing = cpu.missing_requirements();

    assert!(missing.any());
    assert!(missing.base, "rv32 is not rv64");
    assert!(missing.mmu, "sv32 cannot hold the kernel's high half");
    assert!(missing.extensions.contains(A), "no atomics, so no locks");
    assert!(
        !missing.extensions.contains(M),
        "and M is present, so it is not named"
    );
}

/// **Missing SBI is reported even on a machine whose silicon is fine**, because the four calls the
/// kernel makes are unconditional. A device tree cannot answer this; the firmware has to.
#[test]
fn sbi_requirements_are_separate_from_the_tree() {
    let mut cpu = parse_tree(QEMU_VIRT);
    assert!(
        !cpu.sbi.answered(),
        "a record filled from the tree alone has heard nothing from firmware"
    );

    // SBI v2.0, with everything.
    cpu.sbi.spec_major = 2;
    cpu.sbi.extensions = SBI_REQUIRED;
    assert!(
        !cpu.missing_requirements().any(),
        "and with firmware, nothing is missing"
    );

    // Drop one back out. RFENCE is the one whose absence would be silent: the ecall returns an
    // error nobody reads and the other harts simply never invalidate.
    cpu.sbi.extensions = SBI_REQUIRED.difference(SBI_RFENCE);
    let missing = cpu.missing_requirements();
    assert!(missing.sbi.contains(SBI_RFENCE));
    assert!(!missing.sbi.contains(SBI_TIME));
}

/// **Firmware too old to be asked is not firmware that failed.**
///
/// SBI v0.1 predates the base extension, so `probe_extension` cannot run and every answer comes
/// back empty. Treating that as "no RFENCE" would refuse to boot on a machine whose firmware may
/// well have it. The same rule as a device tree that does not describe its ISA: report the silence,
/// do not manufacture a failure out of it.
#[test]
fn firmware_that_cannot_be_asked_is_not_a_failure() {
    let mut cpu = parse_tree(QEMU_VIRT);
    cpu.sbi = Sbi::default();

    assert!(!cpu.sbi.answered());
    assert!(
        cpu.missing_requirements().sbi.is_empty(),
        "nothing to report: the question never got through"
    );
    assert!(!cpu.missing_requirements().any());
}

/// A tree with no CPU node at all. The kernel is running, so silence is not evidence of a machine
/// that cannot run it, and `missing_requirements` must not invent a failure out of nothing.
#[test]
fn a_silent_tree_is_not_a_failure() {
    // The aarch64 `virt` tree, which has `cpu@` nodes but no RISC-V properties on them: the
    // closest thing in the tree to firmware that does not describe its ISA.
    let bytes = include_bytes!("../../dtb/tests/fixtures/qemu-aarch64-virt.dtb");
    let cpu = parse(bytes);

    assert!(cpu.harts > 0, "it does have CPU nodes");
    assert_eq!(cpu.described, 0, "none of them describes a RISC-V ISA");
    assert_eq!(
        cpu.common,
        Extensions::NONE,
        "so the intersection says nothing"
    );
    assert_eq!(cpu.base, Base::Unknown);
    assert_eq!(cpu.mmu, MmuType::Unknown);

    let missing = cpu.missing_requirements();
    assert!(!missing.base);
    assert!(!missing.mmu);
    assert!(
        missing.extensions.is_empty(),
        "silence is not a missing extension"
    );
}
