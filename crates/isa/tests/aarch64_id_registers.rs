//! Decoding the aarch64 ID registers, on the host, from words a real part reports.
//!
//! There is no fixture file here and there does not need to be one: an ID register *is* the
//! fixture, three 64-bit words, and the values below were read out of the machines named beside
//! them rather than copied from a manual. That is the same discipline the device-tree fixtures
//! follow, one layer down.

use isa::aarch64::*;

/// `MIDR_EL1`, `ID_AA64MMFR0_EL1` and `ID_AA64MMFR2_EL1` as QEMU's `cortex-a72` reports them at
/// EL1. This is the CPU model the aarch64 suite runs on, so a change to how these decode is a
/// change to what every boot of this project prints.
const A72: (u64, u64, u64) = (0x410f_d083, 0x0000_0000_0000_1124, 0);

/// Assemble an `ID_AA64MMFR0_EL1` from the four fields this decoder reads, so a test below says
/// which field it is setting instead of leaving a reader to count shifts across a bare expression.
fn mmfr0(tgran4: u64, tgran64: u64, tgran16: u64, asid_bits: u64, pa_range: u64) -> u64 {
    (tgran4 << 28) | (tgran64 << 24) | (tgran16 << 20) | (asid_bits << 4) | pa_range
}

#[test]
fn a_cortex_a72_decodes_to_a_cortex_a72() {
    let cpu = Isa::decode(A72.0, A72.1, A72.2);

    assert_eq!(cpu.implementer, 0x41);
    assert_eq!(cpu.implementer_name(), Some("Arm"));
    assert_eq!(cpu.part, 0xd08, "the part number ARM assigned the A72");
    assert_eq!((cpu.variant, cpu.revision), (0, 3), "r0p3");

    assert_eq!(cpu.pa_bits(), 44, "PARange 0b0100");
    assert_eq!(
        cpu.asid_bits, 16,
        "ASIDBits 0b0010, and crates/asid needs 8"
    );
    assert_eq!(
        cpu.va_bits, 48,
        "no LVA, which is every part before ARMv8.2"
    );

    assert!(
        cpu.granules.k4,
        "the granule every page table here is built on"
    );
    assert!(cpu.granules.k64);
    assert!(!cpu.granules.k16, "the A72 genuinely has no 16 KiB granule");

    assert!(
        !cpu.missing_requirements().any(),
        "this kernel runs on it, and does"
    );
}

/// **`TGran16` is encoded backwards relative to its siblings**, and a decoder that treats the three
/// uniformly gets every part in existence wrong. `0b0000` means *not* supported for 16 KiB and
/// *supported* for 4 KiB and 64 KiB.
///
/// The A72 above is the case that would hide a uniform decoder's bug in one direction (all three
/// fields are zero, so 4 KiB and 64 KiB come out right and only 16 KiB is wrong). This is the case
/// that catches it in the other.
#[test]
fn the_sixteen_kib_granule_field_is_inverted() {
    // 4 KiB yes, 64 KiB no, 16 KiB yes, 16-bit ASIDs, 48-bit PA.
    let cpu = Isa::decode(0, mmfr0(0b0000, 0b1111, 0b0001, 0b0010, 0b0101), 0);

    assert!(cpu.granules.k4);
    assert!(
        !cpu.granules.k64,
        "0b1111 is 'not supported' for the two normal fields"
    );
    assert!(cpu.granules.k16, "0b0001 is 'supported' for this one");
    assert_eq!(cpu.pa_bits(), 48);
}

/// **A machine with no 4 KiB granule cannot run this kernel**, and the record has to say so rather
/// than let `mmu::init` build tables the hardware will not walk.
///
/// No shipping `ARMv8` part is like this; it is architecturally permitted, and the check costs one
/// comparison at boot. The same argument as the RISC-V ASID probe: the cheap check is worth having
/// on the day some core somewhere takes the option.
#[test]
fn a_machine_without_the_4k_granule_is_refused() {
    // 4 KiB no, 64 KiB yes, 16 KiB yes, 16-bit ASIDs, 48-bit PA.
    let missing =
        Isa::decode(0, mmfr0(0b1111, 0b0000, 0b0001, 0b0010, 0b0101), 0).missing_requirements();

    assert!(missing.any());
    assert!(missing.granule_4k);
    assert!(
        !missing.asid_bits,
        "its ASIDs are fine; only the granule is wrong"
    );
}

/// **A reserved `ASIDBits` encoding decodes to zero and stops the boot**, rather than being rounded
/// up to the kind answer.
///
/// ARM defines exactly two values, 8 and 16, so this is unreachable on a conforming part. It is
/// checked because the failure it guards is silent: `crates/asid` hands out 255 numbers on the
/// stated assumption that the hardware can tell them apart, and hardware that cannot puts two
/// address spaces' TLB entries under one tag. RISC-V has to *measure* this number; here it is
/// architected, and the only way to get it wrong is to trust an encoding ARM never defined.
#[test]
fn a_reserved_asid_encoding_is_refused_rather_than_rounded() {
    // ASIDBits = 0b0001, which ARM does not define. Everything else is a normal part.
    let cpu = Isa::decode(0, mmfr0(0b0000, 0b0000, 0b0001, 0b0001, 0b0101), 0);

    assert_eq!(cpu.asid_bits, 0);
    assert!(cpu.missing_requirements().asid_bits);
}

/// Every `PARange` encoding ARM has defined, and the reserved ones reported as zero rather than
/// guessed at. The raw field goes into `TCR_EL1.IPS` whatever it is; this decoding is for the boot
/// line, and a wrong number there is a wrong number in the one place a reader would trust it.
#[test]
fn every_defined_physical_address_range_decodes() {
    for (encoding, bits) in [
        (0b0000, 32),
        (0b0001, 36),
        (0b0010, 40),
        (0b0011, 42),
        (0b0100, 44),
        (0b0101, 48),
        (0b0110, 52),
        (0b0111, 56),
    ] {
        let cpu = Isa::decode(0, encoding, 0);
        assert_eq!(cpu.pa_bits(), bits, "PARange {encoding:#06b}");
        assert_eq!(
            cpu.pa_range, encoding as u8,
            "the raw field is kept for TCR_EL1.IPS"
        );
    }

    assert_eq!(
        Isa::decode(0, 0b1000, 0).pa_bits(),
        0,
        "reserved, and reported as such"
    );
}

/// `VARange`, the direct twin of RISC-V's `mmu-type`: what the part could address, as opposed to
/// what we configured. Reported, and acted on by nothing (see the module BUGS).
#[test]
fn the_virtual_address_range_is_reported_not_assumed() {
    assert_eq!(Isa::decode(0, 0, 0).va_bits, 48);
    assert_eq!(Isa::decode(0, 0, 0b0001 << 16).va_bits, 52);
    assert_eq!(
        Isa::decode(0, 0, 0b1111 << 16).va_bits,
        48,
        "reserved reads as the base case"
    );
}

/// **Every published vendor is reachable by its code, and an unpublished one is not guessed at.**
///
/// The boot line names the vendor, and it is the first thing somebody bringing up a board reads.
/// The shadowing this guards against was possible while the lookup was a `match`: two arms with the
/// same code compile, and the second is unreachable forever with no warning. The table's own
/// `const` assertion catches the duplicate; this catches a row the lookup cannot reach.
///
/// The codes go in through `MIDR_EL1[31:24]` rather than through the field directly, so this also
/// holds `decode`'s shift to the table. A decoder off by a byte would report every vendor as
/// unpublished, which reads like a missing entry rather than a broken shift.
#[test]
fn every_implementer_is_reachable_through_midr() {
    for row in &IMPLEMENTERS {
        let cpu = Isa::decode((row.code as u64) << 24, 0, 0);
        assert_eq!(cpu.implementer, row.code, "MIDR_EL1[31:24] is the code");
        assert_eq!(
            cpu.implementer_name(),
            Some(row.name),
            "code {:#04x} does not reach {}",
            row.code,
            row.name
        );
    }

    // The two this project actually meets: QEMU's cortex-a72, and the Apple Silicon it is
    // developed on.
    assert_eq!(Isa::decode(A72.0, 0, 0).implementer_name(), Some("Arm"));
    assert_eq!(
        Isa::decode(0x6100_0000, 0, 0).implementer_name(),
        Some("Apple")
    );

    // And codes ARM has not assigned. Reported as a number, because a name here would be a guess
    // about who made the chip somebody is trying to bring a kernel up on.
    assert_eq!(Isa::decode(0x0700_0000, 0, 0).implementer_name(), None);
    assert_eq!(Isa::decode(0x0700_0000, 0, 0).implementer, 0x07);
    assert_eq!(Isa::decode(0xff00_0000, 0, 0).implementer_name(), None);
}
