//! Reading a filesystem out of a **partition on a device** (milestone 110).
//!
//! The device here is a file with a real GUID partition table at the front and a real RedoxFS
//! inside its third partition, and every read is a separate invocation of the built binary, which
//! is the same discipline `recovery.rs` keeps and for the same reason: nothing is shared between
//! the writer and the reader but the bytes.
//!
//! **The filesystem is built as an image and then placed**, rather than written through the offset
//! disk under test. That is what makes this a test of the arithmetic: the bytes in the partition
//! were produced by a writer that knew nothing about partitions, so if the offset is wrong by one
//! block in either direction nothing opens. The table is built with `crates/gpt`, which is the
//! parser the reader uses, so the one thing this cannot catch is a table both halves agree is
//! wrong; `blank_check_after_run` in xtask closes that by reading a table the *guest* wrote.
//!
//! The layout deliberately has three partitions:
//!
//! - **1, an EFI system partition**, empty. It proves a selector picks a partition rather than
//!   finding the only filesystem on the disk, and that asking for the wrong one fails cleanly.
//! - **2, misaligned on purpose**: it starts at an odd LBA, so its first byte is not on a 4096-byte
//!   filesystem block. The tool must refuse it rather than round it in.
//! - **3, the cricker-os data partition**, holding the filesystem.
//!
//! And one test uses a **320 MiB** device with the data partition past 256 MiB, because that is
//! where the capability is genuinely new rather than cosmetic. See
//! `a_partition_past_the_engines_scan_is_reachable_only_by_name`.

use std::fs::OpenOptions;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use gpt::guid::types;
use gpt::{Entry, Gpt, Guid};

/// The built binary, not the library: `cargo test` compiles it and hands us the path.
const TOOL: &str = env!("CARGO_BIN_EXE_redoxfs_host");

/// The device's logical block size. 512 is what every disk this project has met reports, and what
/// the tool tries first.
const LBA: u64 = 512;

/// 64 MiB of device, which is what the blank-disk fixture uses.
const DEVICE_BLOCKS: u64 = 131_072;

/// The two partitions that hold nothing, in LBAs. Partition 2 starts on an **odd** block on
/// purpose, so its first byte is not on a 4096-byte filesystem block.
const ESP: (u64, u64) = (2048, 10_239);
const MISALIGNED: (u64, u64) = (10_241, 20_479);

/// The data partition on the ordinary device: 12 MiB in, 20 MiB long, both ends on a filesystem
/// block.
const DATA: (u64, u64) = (24_576, 65_535);

/// A 320 MiB device whose data partition starts at 272 MiB, past the 256 MiB the engine's own
/// header scan reaches.
const FAR_DEVICE_BLOCKS: u64 = 655_360;
const FAR_DATA: (u64, u64) = (557_056, 598_015);

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("redoxfs_host-part-{}-{name}", std::process::id()))
}

fn tool(args: &[&str]) -> Vec<u8> {
    let out = Command::new(TOOL)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run the tool: {e}"));
    assert!(
        out.status.success(),
        "redoxfs_host {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    out.stdout
}

/// stdout and stderr, for the calls whose warning is the thing under test.
fn tool_output(args: &[&str]) -> (Vec<u8>, String) {
    let out = Command::new(TOOL)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run the tool: {e}"));
    assert!(
        out.status.success(),
        "redoxfs_host {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    (
        out.stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn tool_fails(args: &[&str]) -> String {
    let out = Command::new(TOOL)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run the tool: {e}"));
    assert!(
        !out.status.success(),
        "redoxfs_host {args:?} was expected to fail and did not",
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A version-4-shaped GUID with a recognisable body, so a failure names which one it was.
fn guid(tag: u8) -> Guid {
    Guid::v4_from_random([tag; 16])
}

/// Build a device: a protective MBR, a primary and backup GPT, and a RedoxFS image dropped into the
/// data partition.
///
/// Written sparsely, block by block, rather than assembled in one buffer: the far-partition device
/// is 320 MiB and only about 20 MiB of it is ever written.
fn build_device(device: &Path, payload: &[u8], data: (u64, u64), device_blocks: u64) {
    // The filesystem first, as an ordinary image, by the code that has always made images. Named
    // after the device so two tests running at once cannot share one scratch file.
    let fs_bytes_len = (data.1 - data.0 + 1) * LBA;
    let fs_image = device.with_extension("inner");
    let _ = std::fs::remove_file(&fs_image);
    redoxfs_host::mkfs(&fs_image, fs_bytes_len).expect("mkfs failed");
    redoxfs_host::put(&fs_image, "motd", payload).expect("put failed");
    let fs_bytes = std::fs::read(&fs_image).expect("cannot read the image back");
    assert_eq!(fs_bytes.len() as u64, fs_bytes_len);

    let parts = [
        Entry::new(types::EFI_SYSTEM, guid(0x11), ESP.0, ESP.1)
            .with_name("cricker boot")
            .unwrap(),
        Entry::new(
            types::LINUX_FILESYSTEM,
            guid(0x22),
            MISALIGNED.0,
            MISALIGNED.1,
        ),
        Entry::new(types::CRICKER_DATA, guid(0x33), data.0, data.1)
            .with_name("cricker data")
            .unwrap(),
    ];
    let mut array = [0u8; gpt::ENTRY_ARRAY_BYTES];
    let table = Gpt::create(
        guid(0x44),
        LBA as usize,
        device_blocks,
        &parts,
        &mut array[..],
    )
    .expect("the fixture table is not valid");

    let _ = std::fs::remove_file(device);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(device)
        .expect("cannot create the device image");
    file.set_len(device_blocks * LBA).expect("cannot size it");

    let mut block = [0u8; LBA as usize];
    table
        .write_protective_mbr(&mut block)
        .expect("protective MBR");
    file.write_all_at(&block, 0).unwrap();
    table
        .write_primary_header(&mut block)
        .expect("primary header");
    file.write_all_at(&block, gpt::PRIMARY_HEADER_LBA * LBA)
        .unwrap();
    table
        .write_backup_header(&mut block)
        .expect("backup header");
    file.write_all_at(&block, table.backup_header_lba() * LBA)
        .unwrap();
    for lba in [table.primary_entry_lba(), table.backup_entry_lba()] {
        file.write_all_at(table.entry_array(), lba * LBA).unwrap();
    }

    file.write_all_at(&fs_bytes, data.0 * LBA).unwrap();
    file.sync_all().unwrap();
    let _ = std::fs::remove_file(&fs_image);
}

/// A cheap content fingerprint, to prove a read wrote nothing back.
fn digest(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

#[test]
fn a_filesystem_inside_a_partition_reads_back_through_the_tool() {
    let device = scratch("device.img");
    let payload = b"CRK110: this filesystem lives in partition 3 of a device\n";
    build_device(&device, payload, DATA, DEVICE_BLOCKS);
    let dev = device.display().to_string();
    let data_type = String::from_utf8(types::CRICKER_DATA.to_ascii().to_vec()).unwrap();

    let before = digest(&std::fs::read(&device).unwrap());

    // The table, read by the tool that will read the filesystem out of it.
    let listing = String::from_utf8(tool(&["partitions", &dev])).unwrap();
    assert!(
        listing.contains("512-byte logical blocks"),
        "the listing does not say what block size it found: {listing}"
    );
    assert!(
        listing.contains("cricker-os data") && listing.contains("EFI system"),
        "the listing does not name the partition types: {listing}"
    );
    assert!(
        listing.contains("cricker data"),
        "the listing dropped the GPT label: {listing}"
    );

    // The same partition, both ways of naming it.
    for selector in [
        vec!["cat", &dev, "motd", "--partition", "3"],
        vec!["cat", &dev, "motd", "--partition-type", &data_type],
        // The flag before the verb, and with an `=`, because both are things people type.
        vec!["--partition=3", "cat", &dev, "motd"],
    ] {
        assert_eq!(
            tool(&selector),
            payload,
            "the payload did not come back through {selector:?}",
        );
    }

    // A whole subtree out of a partition, which is the verb an actual recovery uses.
    let dest = scratch("recovered");
    let _ = std::fs::remove_dir_all(&dest);
    tool(&[
        "extract",
        &dev,
        "/",
        &dest.display().to_string(),
        "--partition",
        "3",
    ]);
    assert_eq!(
        std::fs::read(dest.join("motd")).expect("motd was not extracted"),
        payload,
    );
    let _ = std::fs::remove_dir_all(&dest);

    // **The reads wrote nothing to the device.** The offset makes this a sharper question than it is
    // for an image: a stray write through a window lands in somebody else's partition.
    assert_eq!(
        digest(&std::fs::read(&device).unwrap()),
        before,
        "reading the partition changed the device",
    );

    let _ = std::fs::remove_file(&device);
}

/// **A device read whole is a device read by luck, and the tool now says so.**
///
/// `FileSystem::open` scans for a valid header, so a partitioned disk handed over without a
/// selector does not fail: it opens whichever filesystem lies in the first 256 MiB. That is why the
/// warning exists, and why the next test matters more than this one.
#[test]
fn a_device_read_without_a_selector_warns_that_it_guessed() {
    let device = scratch("unselected.img");
    let payload = b"found by scanning, not by being told\n";
    build_device(&device, payload, DATA, DEVICE_BLOCKS);
    let dev = device.display().to_string();

    let (out, err) = tool_output(&["cat", &dev, "motd"]);
    assert_eq!(
        out, payload,
        "the scan did not find the filesystem after all"
    );
    assert!(
        err.contains("has a GUID partition table") && err.contains("--partition"),
        "reading a device whole should say what it did: {err}"
    );

    // An image file, which is what this tool has always taken, says nothing extra.
    let image = scratch("quiet.img");
    let _ = std::fs::remove_file(&image);
    redoxfs_host::mkfs(&image, 8 * 1024 * 1024).expect("mkfs failed");
    redoxfs_host::put(&image, "motd", payload).expect("put failed");
    let (out, err) = tool_output(&["cat", &image.display().to_string(), "motd"]);
    assert_eq!(out, payload);
    assert!(err.is_empty(), "an image file should read silently: {err}");

    let _ = std::fs::remove_file(&device);
    let _ = std::fs::remove_file(&image);
}

/// **The capability this milestone adds, stated as the case the old tool could not do at all.**
///
/// The engine's header scan gives up at block 65536, which is 256 MiB in. A data partition beyond
/// that is unreachable by scanning and reachable by arithmetic, and a disk with a boot partition
/// and a root partition ahead of the data is the ordinary shape, not a contrived one.
#[test]
fn a_partition_past_the_engines_scan_is_reachable_only_by_name() {
    let device = scratch("far.img");
    let payload = b"272 MiB into the disk, where no scan reaches\n";
    build_device(&device, payload, FAR_DATA, FAR_DEVICE_BLOCKS);
    let dev = device.display().to_string();

    let err = tool_fails(&["cat", &dev, "motd"]);
    assert!(
        err.contains("not a RedoxFS image"),
        "a filesystem past the scan should not be found by luck: {err}"
    );
    assert_eq!(
        tool(&["cat", &dev, "motd", "--partition", "3"]),
        payload,
        "naming the partition did not reach it",
    );

    let _ = std::fs::remove_file(&device);
}

#[test]
fn the_refusals_say_which_thing_was_wrong() {
    let device = scratch("refusals.img");
    build_device(&device, b"payload\n", DATA, DEVICE_BLOCKS);
    let dev = device.display().to_string();

    // Partition 1 exists and is empty. This is the message a person gets when they pick the wrong
    // number, and it must not be about partitions at all: the partition was found.
    let err = tool_fails(&["cat", &dev, "motd", "--partition", "1"]);
    assert!(
        err.contains("not a RedoxFS image"),
        "an empty partition should report an empty filesystem: {err}"
    );

    // Partition 2 starts on an odd LBA, so its first byte is not on a filesystem block.
    let err = tool_fails(&["cat", &dev, "motd", "--partition", "2"]);
    assert!(
        err.contains("not a multiple of the 4096-byte filesystem block"),
        "a misaligned partition should be refused by name: {err}"
    );

    // Off the end of the table, and off the bottom of it.
    let err = tool_fails(&["cat", &dev, "motd", "--partition", "9"]);
    assert!(
        err.contains("empty slot"),
        "an unused slot should say it is unused: {err}"
    );
    let err = tool_fails(&["cat", &dev, "motd", "--partition", "0"]);
    assert!(
        err.contains("start at 1"),
        "partition 0 should say numbering starts at 1: {err}"
    );

    // A type nothing on this disk carries.
    let apfs = String::from_utf8(types::APPLE_APFS.to_ascii().to_vec()).unwrap();
    let err = tool_fails(&["cat", &dev, "motd", "--partition-type", &apfs]);
    assert!(
        err.contains("no partition of type"),
        "a missing type should name the type: {err}"
    );

    // **The write verbs refuse a selector.** Recovery does not write, and a device opened
    // read-write by accident is the mistake this refusal exists to prevent.
    let err = tool_fails(&["put", &dev, "x", "/etc/hosts", "--partition", "3"]);
    assert!(
        err.contains("refused on mkfs, put and import"),
        "a write verb took a partition selector: {err}"
    );

    let _ = std::fs::remove_file(&device);
}

#[test]
fn a_file_with_no_table_is_not_reported_as_a_missing_partition() {
    // An image file handed to a partition selector by mistake. The answer has to be "there is no
    // table here", because "no such partition" would send somebody looking for a disk problem.
    let image = scratch("plain.img");
    let _ = std::fs::remove_file(&image);
    redoxfs_host::mkfs(&image, 8 * 1024 * 1024).expect("mkfs failed");
    let err = tool_fails(&["ls", &image.display().to_string(), "/", "--partition", "1"]);
    assert!(
        err.contains("no GUID partition table"),
        "an image with no table should say so: {err}"
    );
    let _ = std::fs::remove_file(&image);
}
