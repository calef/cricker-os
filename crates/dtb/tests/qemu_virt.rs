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

// --- /chosen and the initrd ---

const QEMU_VIRT_INITRD: &[u8] = include_bytes!("fixtures/qemu-virt-initrd.dtb");

#[test]
fn finds_the_initrd() {
    let dtb = Dtb::from_bytes(QEMU_VIRT_INITRD).unwrap();
    let initrd = dtb.initrd().unwrap().expect("this fixture has an initrd");

    // The bootloader loaded a file into RAM and told us where. Nobody else protects it.
    // If the frame allocator hands this memory out, the initrd is gone before milestone 8
    // ever reads a byte of it.
    assert_eq!(initrd.start, 0x4400_0000);
    assert_eq!(initrd.end(), 0x4403_0d40);
    assert_eq!(
        initrd.size, 200_000,
        "the fixture was made from a 200 KB file"
    );
}

#[test]
fn no_initrd_is_not_an_error() {
    // The common case: nobody passed -initrd. `None`, not an error, and definitely not a
    // zero-length region that the allocator then tries to reason about.
    let dtb = Dtb::from_bytes(QEMU_VIRT).unwrap();
    assert_eq!(dtb.initrd().unwrap(), None);
}

#[test]
fn an_initrd_does_not_disturb_the_memory_map() {
    // Regression guard: the /chosen walk and the /memory walk are separate passes over
    // the same token stream. A bug in one must not corrupt the other.
    let dtb = Dtb::from_bytes(QEMU_VIRT_INITRD).unwrap();
    let mut regions = [Region { start: 0, size: 0 }; 8];
    assert_eq!(dtb.memory_regions(&mut regions).unwrap(), 1);
    assert_eq!(regions[0].start, 0x4000_0000);
}

// --- finding a device by name ---

#[test]
fn finds_the_interrupt_controller() {
    let dtb = Dtb::from_bytes(QEMU_VIRT).unwrap();
    let mut regs = [Region { start: 0, size: 0 }; 4];

    let n = dtb.node_reg(b"intc@", &mut regs).unwrap();

    // The GIC has TWO register blocks, and the order is part of the binding.
    assert_eq!(
        n, 2,
        "the GIC should have a distributor and a CPU interface"
    );

    assert_eq!(
        regs[0],
        Region {
            start: 0x0800_0000,
            size: 0x1_0000
        }
    ); // GICD, distributor
    assert_eq!(
        regs[1],
        Region {
            start: 0x0801_0000,
            size: 0x1_0000
        }
    ); // GICC, CPU interface
}

#[test]
fn finds_the_uart_the_console_hardcodes() {
    // The console hardcodes 0x0900_0000 on purpose (it must come up before the DTB parser
    // exists to debug it). But now we can *check* the hardcode against what the machine says,
    // which is the cross-check that catches the day it changes.
    let dtb = Dtb::from_bytes(QEMU_VIRT).unwrap();
    let mut regs = [Region { start: 0, size: 0 }; 2];

    assert_eq!(dtb.node_reg(b"pl011@", &mut regs).unwrap(), 1);
    assert_eq!(regs[0].start, 0x0900_0000);
}

#[test]
fn a_missing_node_is_zero_regions_not_an_error() {
    let dtb = Dtb::from_bytes(QEMU_VIRT).unwrap();
    let mut regs = [Region { start: 0, size: 0 }; 2];
    assert_eq!(dtb.node_reg(b"nonesuch@", &mut regs).unwrap(), 0);
}
