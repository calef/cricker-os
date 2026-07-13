//! Parse a real device tree, dumped from the machine we actually boot on.
//!
//! These run on the **host**, in milliseconds, with no emulator. That is the whole
//! argument for keeping pure logic out of the kernel crate (DECISIONS.md §7).
//!
//! Regenerate the fixture with:
//!
//!     qemu-system-aarch64 -machine virt,dumpdtb=f.dtb -cpu cortex-a72 -nographic
//!     dtc -I dtb -O dtb -o crates/dtb/tests/fixtures/qemu-virt.dtb f.dtb
//!
//! (The `dtc` round-trip is not cosmetic: QEMU pads its dump out to a full megabyte
//! and says so in the header, so the raw dump is a 1 MB file describing 7 KB of tree.)

use dtb::{Dtb, Error, Region};

const QEMU_VIRT: &[u8] = include_bytes!("fixtures/qemu-virt.dtb");

#[test]
fn parses_the_header() {
    let dtb = Dtb::from_bytes(QEMU_VIRT).expect("should parse");
    assert_eq!(dtb.total_size(), QEMU_VIRT.len());
}

#[test]
fn finds_the_ram() {
    let dtb = Dtb::from_bytes(QEMU_VIRT).unwrap();
    let mut regions = [Region { start: 0, size: 0 }; 8];
    let n = dtb.memory_regions(&mut regions).unwrap();

    assert_eq!(n, 1, "virt has one memory node");

    // This is the number we hardcoded in milestone 1, now read from the machine
    // instead of from a `dtc` dump we squinted at.
    assert_eq!(
        regions[0],
        Region {
            start: 0x4000_0000,
            size: 0x800_0000, // 128 MiB, QEMU's default
        }
    );
}

#[test]
fn qemu_virt_reserves_nothing() {
    let dtb = Dtb::from_bytes(QEMU_VIRT).unwrap();
    let mut regions = [Region { start: 0, size: 0 }; 8];

    // QEMU's virt leaves the reservation block empty. Real firmware often does not,
    // and this assertion exists to document that we handle the empty case rather than
    // to celebrate it.
    assert_eq!(dtb.reserved_regions(&mut regions).unwrap(), 0);
}

#[test]
fn rejects_junk() {
    // The failure we actually care about: someone hands us a pointer to something that
    // isn't a device tree. The magic check is the only thing standing between that and
    // a parser walking off into random memory, which in a kernel is unbounded.
    match Dtb::from_bytes(&[0xffu8; 64]) {
        Err(Error::BadMagic(m)) => assert_eq!(m, 0xffff_ffff),
        other => panic!("expected BadMagic, got {other:?}"),
    }
    match Dtb::from_bytes(&[0u8; 64]) {
        Err(Error::BadMagic(0)) => {}
        other => panic!("expected BadMagic(0), got {other:?}"),
    }
}

#[test]
fn rejects_a_truncated_blob() {
    // Correct magic, but the blob is cut short. The header says it's 7566 bytes and
    // we only have 32. A parser that trusts the header here walks off the end of the
    // buffer, which in a kernel means reading whatever memory happens to follow.
    match Dtb::from_bytes(&QEMU_VIRT[..32]) {
        Err(Error::Truncated) => {}
        other => panic!("expected Truncated, got {other:?}"),
    }
}

#[test]
fn refuses_to_overflow_the_callers_slice() {
    let dtb = Dtb::from_bytes(QEMU_VIRT).unwrap();
    let mut too_small: [Region; 0] = [];
    match dtb.memory_regions(&mut too_small) {
        Err(Error::TooManyRegions) => {}
        other => panic!("expected TooManyRegions, got {other:?}"),
    }
}
