//! Building tables, reading them back, and every rule about what a table may not be.
//!
//! The companion to `real_disks.rs`, which runs the same code against bytes other people's tools
//! wrote. These are the cases a real disk cannot supply, because no real tool will make one:
//! partitions that overlap by exactly one block, a disk too small for its own table, a header with
//! a stray byte after it.

use gpt::guid::{Guid, types};
use gpt::{
    DEFAULT_ENTRY_COUNT, ENTRY_ARRAY_BYTES, Entry, Error, Gpt, Header, MbrProblem, block_size_ok,
    entry, mbr, testing,
};

const BLOCK: usize = 512;
const BLOCKS: u64 = 131_072;
const DISK: Guid = Guid::from_fields(0x1234_5678, 0x9ABC, 0x4DEF, [1, 2, 3, 4, 5, 6, 7, 8]);
const PART: Guid = Guid::from_fields(0xFEDC_BA98, 0x7654, 0x4321, [9, 8, 7, 6, 5, 4, 3, 2]);

/// A table and the four regions it wants written, as one value, so a test can build a disk in a
/// line and then check any part of it. The three block buffers are the largest block GPT allows;
/// [`build_on`] hands out a prefix of the size under test.
struct Disk {
    block_size: usize,
    mbr_buf: [u8; 4096],
    header_buf: [u8; 4096],
    backup_buf: [u8; 4096],
    array: [u8; ENTRY_ARRAY_BYTES],
}

impl Disk {
    fn mbr(&self) -> &[u8] {
        &self.mbr_buf[..self.block_size]
    }
    fn header(&self) -> &[u8] {
        &self.header_buf[..self.block_size]
    }
    fn backup(&self) -> &[u8] {
        &self.backup_buf[..self.block_size]
    }
}

fn build(partitions: &[Entry]) -> Result<Disk, Error> {
    build_on(BLOCK, BLOCKS, partitions)
}

fn build_on(block_size: usize, blocks: u64, partitions: &[Entry]) -> Result<Disk, Error> {
    let mut disk = Disk {
        block_size,
        mbr_buf: [0; 4096],
        header_buf: [0; 4096],
        backup_buf: [0; 4096],
        array: [0; ENTRY_ARRAY_BYTES],
    };
    let table = Gpt::create(DISK, block_size, blocks, partitions, &mut disk.array)?;
    table.write_protective_mbr(&mut disk.mbr_buf[..block_size])?;
    table.write_primary_header(&mut disk.header_buf[..block_size])?;
    table.write_backup_header(&mut disk.backup_buf[..block_size])?;
    Ok(disk)
}

/// The whole crate in one test: build a table, write every region, read them all back, and get
/// exactly what went in. The primary, the backup and the protective MBR each go through a different
/// validator.
#[test]
fn a_table_we_build_is_a_table_we_parse() {
    let part = Entry::new(types::CRICKER_DATA, PART, 2048, 100_000)
        .with_name("cricker data")
        .unwrap();
    let disk = build(&[part]).unwrap();

    let table = Gpt::parse(disk.header(), &disk.array).unwrap();
    table.check_backup(disk.backup(), &disk.array).unwrap();
    table.check_protective_mbr(disk.mbr()).unwrap();

    assert_eq!(table.disk_guid(), DISK);
    assert_eq!(table.block_count(), BLOCKS);
    assert_eq!(table.entry_count(), DEFAULT_ENTRY_COUNT);
    assert_eq!(table.first_usable_lba(), 34);
    assert_eq!(table.last_usable_lba(), BLOCKS - 34);
    assert_eq!(table.backup_header_lba(), BLOCKS - 1);
    assert_eq!(table.backup_entry_lba(), BLOCKS - 33);

    let found: Vec<_> = table.partitions().collect();
    assert_eq!(found, vec![(0usize, part)]);
}

/// A 4K-native disk. The header is still one block and the entries are still 128 bytes, so the only
/// thing that changes is that the 16 KiB array is four blocks instead of thirty-two and the usable
/// range moves in by that much at both ends.
///
/// Worth a test rather than an assumption: everything here takes its block size from the length of
/// the block it was handed, and a hardcoded 512 anywhere would survive every other test in the
/// suite, because both fixtures are 512-byte disks.
#[test]
fn a_4k_native_disk_works_and_the_geometry_moves() {
    let part = Entry::new(types::LINUX_FILESYSTEM, PART, 256, 4000);
    let disk = build_on(4096, 8192, &[part]).unwrap();
    let table = Gpt::parse(disk.header(), &disk.array).unwrap();

    assert_eq!(table.block_size(), 4096);
    assert_eq!(table.entry_array_blocks(), 4, "16 KiB in 4 KiB blocks");
    assert_eq!(
        table.first_usable_lba(),
        6,
        "MBR, header, four array blocks"
    );
    assert_eq!(table.last_usable_lba(), 8192 - 6);
    assert_eq!(table.backup_entry_lba(), 8192 - 5);
    table.check_backup(disk.backup(), &disk.array).unwrap();
    assert_eq!(table.partitions().count(), 1);
}

/// The mistake this catches is the one everybody makes with inclusive ranges: two partitions where
/// one ends on the block the next starts on. `last_lba` is the last block *in* the partition, so
/// that is an overlap, and two filesystems would fight over one sector forever.
#[test]
fn partitions_that_share_one_block_overlap() {
    let a = Entry::new(types::LINUX_FILESYSTEM, PART, 2048, 4096);
    let touching = Entry::new(types::CRICKER_DATA, PART, 4096, 8192);
    assert_eq!(
        build(&[a, touching]).err(),
        Some(Error::PartitionsOverlap { a: 0, b: 1 })
    );

    let adjacent = Entry::new(types::CRICKER_DATA, PART, 4097, 8192);
    assert!(build(&[a, adjacent]).is_ok(), "abutting is not overlapping");

    // Order does not matter: the pair is caught whichever way round it is given.
    assert_eq!(
        build(&[touching, a]).err(),
        Some(Error::PartitionsOverlap { a: 0, b: 1 })
    );
    // Nor does distance in the array: entry 0 against entry 3, with holes between.
    let far = [a, Entry::UNUSED, Entry::UNUSED, touching];
    assert_eq!(
        build(&far).err(),
        Some(Error::PartitionsOverlap { a: 0, b: 3 })
    );
}

/// An unused entry overlaps nothing, whatever its LBA fields say. A wiped entry can keep its old
/// bounds, and it is still not a partition.
#[test]
fn an_unused_entry_is_not_a_partition_however_it_looks() {
    let mut ghost = Entry::new(types::LINUX_FILESYSTEM, PART, 2048, 4096);
    ghost.type_guid = Guid::ZERO;
    assert!(!ghost.is_used());

    let real = Entry::new(types::CRICKER_DATA, PART, 2048, 4096);
    let disk = build(&[ghost, real]).expect("the ghost does not collide");
    let table = Gpt::parse(disk.header(), &disk.array).unwrap();
    assert_eq!(table.partitions().count(), 1);
    assert_eq!(
        table.partitions().next().unwrap().0,
        1,
        "and the index survives the hole, because it is what a tool prints as the partition number"
    );
}

#[test]
fn a_partition_outside_the_usable_range_is_refused() {
    // Below first_usable_lba, which is 34: that is where the table itself lives.
    let low = Entry::new(types::CRICKER_DATA, PART, 33, 4096);
    assert_eq!(
        build(&[low]).err(),
        Some(Error::PartitionOutsideUsable { index: 0 })
    );

    // Past last_usable_lba, the check that catches a table cloned onto a smaller disk.
    let high = Entry::new(types::CRICKER_DATA, PART, 2048, BLOCKS - 33);
    assert_eq!(
        build(&[high]).err(),
        Some(Error::PartitionOutsideUsable { index: 0 })
    );

    let backwards = Entry::new(types::CRICKER_DATA, PART, 4096, 2048);
    assert_eq!(
        build(&[backwards]).err(),
        Some(Error::PartitionRange { index: 0 })
    );

    // The boundaries themselves are allowed, which is the other half of the claim.
    let exact = Entry::new(types::CRICKER_DATA, PART, 34, BLOCKS - 34);
    assert!(build(&[exact]).is_ok());
}

/// The smallest disk that can hold a 128-entry table is 68 blocks: MBR, header, 32 array blocks,
/// one usable block, 32 array blocks, header. One block less and there is nowhere to put a
/// partition, which is an error rather than a table with an empty usable range.
#[test]
fn a_disk_too_small_for_its_own_table_is_refused() {
    let mut array = [0u8; ENTRY_ARRAY_BYTES];
    assert_eq!(
        Gpt::create(DISK, BLOCK, 67, &[], &mut array).err(),
        Some(Error::DiskTooSmall {
            blocks: 67,
            need: 68
        })
    );
    let table = Gpt::create(DISK, BLOCK, 68, &[], &mut array).unwrap();
    assert_eq!(table.first_usable_lba(), 34);
    assert_eq!(table.last_usable_lba(), 34, "exactly one usable block");
}

#[test]
fn the_entry_array_buffer_sets_the_entry_count() {
    let mut small = [0u8; 4 * entry::SIZE];
    let table = Gpt::create(DISK, BLOCK, 1024, &[], &mut small).unwrap();
    assert_eq!(table.entry_count(), 4);
    assert_eq!(table.entry_array_blocks(), 1, "512 bytes is one block");
    assert_eq!(table.first_usable_lba(), 3);

    let mut ragged = [0u8; 100];
    assert_eq!(
        Gpt::create(DISK, BLOCK, 1024, &[], &mut ragged).err(),
        Some(Error::EntryArrayShape { have: 100 })
    );

    let mut four = [0u8; 4 * entry::SIZE];
    let five = [Entry::new(types::CRICKER_DATA, PART, 10, 20); 5];
    assert_eq!(
        Gpt::create(DISK, BLOCK, 1024, &five, &mut four).err(),
        Some(Error::TooManyPartitions { given: 5, room: 4 })
    );
}

/// **Every single-byte corruption of a table, complete.** Every byte of the header block and every
/// byte of the entry array, crossed with all 255 other values each could have held: 261,120 cases,
/// and every one of them must be rejected.
///
/// This is the small-table twin of the sweeps in `real_disks.rs`, and the reason it exists as well
/// as those is cost. A four-entry array is 512 bytes rather than 16 KiB, so the complete cross
/// product is 130 MB of CRC instead of 68 GB, and runs in the time a test should take. The
/// real-disk version proves the same thing about bytes a real tool wrote.
///
/// Enumeration is the right tool at this size and it is *stronger* than a solver result, which is
/// the point `ntp_proto` made and this crate inherits: a model checker is for domains too big to
/// count. The symbolic form of this property (any buffer, not this one) is the Kani harness
/// `a_single_byte_change_always_changes_the_crc`.
#[test]
fn every_single_byte_corruption_of_a_small_table_is_caught() {
    let mut array = [0u8; 4 * entry::SIZE];
    let mut header = [0u8; BLOCK];
    let part = Entry::new(types::CRICKER_DATA, PART, 8, 900)
        .with_name("small")
        .unwrap();
    let table = Gpt::create(DISK, BLOCK, 1024, &[part], &mut array).unwrap();
    table.write_primary_header(&mut header).unwrap();
    Gpt::parse(&header, &array).expect("the clean table parses");

    let mut cases = 0u32;
    let mut corrupt_header = [0u8; BLOCK];
    for position in 0..BLOCK {
        for value in 0..=255u8 {
            if value == header[position] {
                continue;
            }
            corrupt_header.copy_from_slice(&header);
            corrupt_header[position] = value;
            assert!(
                Gpt::parse(&corrupt_header, &array).is_err(),
                "header byte {position} -> {value:#04x}"
            );
            cases += 1;
        }
    }

    let mut corrupt_array = array;
    for position in 0..array.len() {
        for value in 0..=255u8 {
            if value == array[position] {
                continue;
            }
            corrupt_array[position] = value;
            assert!(
                Gpt::parse(&header, &corrupt_array).is_err(),
                "array byte {position} -> {value:#04x}"
            );
            cases += 1;
        }
        corrupt_array[position] = array[position];
    }
    assert_eq!(cases, (BLOCK + 4 * entry::SIZE) as u32 * 255);
}

/// Handing the backup header to `Gpt::parse` is refused rather than half-accepted. It is a valid
/// header with a correct CRC describing the same disk, so nothing but the `my_lba` check tells it
/// apart. Getting this wrong means a recovery tool that reads the last block and believes it has
/// the primary.
#[test]
fn the_backup_header_is_not_a_primary() {
    let disk = build(&[Entry::new(types::CRICKER_DATA, PART, 2048, 4096)]).unwrap();
    assert_eq!(
        Gpt::parse(disk.backup(), &disk.array).err(),
        Some(Error::NotPrimary { my_lba: BLOCKS - 1 })
    );
    // It is still a valid header on its own terms, which is why the check has to be explicit.
    assert!(Header::decode(disk.backup()).is_ok());
}

#[test]
fn a_short_entry_array_is_a_caller_error_not_a_corrupt_disk() {
    let disk = build(&[Entry::new(types::CRICKER_DATA, PART, 2048, 4096)]).unwrap();
    assert_eq!(
        Gpt::parse(disk.header(), &disk.array[..ENTRY_ARRAY_BYTES - 1]).err(),
        Some(Error::EntryArrayLen {
            need: ENTRY_ARRAY_BYTES,
            have: ENTRY_ARRAY_BYTES - 1
        })
    );
    // Extra bytes are fine: a caller reading whole blocks usually has some.
    let mut padded = [0u8; ENTRY_ARRAY_BYTES + 512];
    padded[..ENTRY_ARRAY_BYTES].copy_from_slice(&disk.array);
    assert!(Gpt::parse(disk.header(), &padded).is_ok());
}

#[test]
fn a_header_that_is_not_a_header_says_so() {
    let disk = build(&[]).unwrap();
    let good: [u8; BLOCK] = disk.header().try_into().unwrap();
    let mut block = good;

    block[0] = b'X';
    assert_eq!(Header::decode(&block).err(), Some(Error::Signature));

    block = good;
    block[8..12].copy_from_slice(&0x0002_0000u32.to_le_bytes());
    assert_eq!(
        Header::decode(&block).err(),
        Some(Error::Revision(0x0002_0000))
    );

    block = good;
    block[12..16].copy_from_slice(&91u32.to_le_bytes());
    assert_eq!(Header::decode(&block).err(), Some(Error::HeaderSize(91)));

    block = good;
    block[20] = 1;
    assert_eq!(Header::decode(&block).err(), Some(Error::ReservedNotZero));

    // A buffer that is not one logical block.
    assert_eq!(
        Header::decode(&block[..100]).err(),
        Some(Error::BlockSize(100))
    );
    assert_eq!(Header::decode(&[0u8; 1024]).err(), Some(Error::Signature));
}

/// The reserved-tail check is the strictest thing this crate does and the likeliest to need
/// relaxing, so it gets a test that says out loud what it costs: a header whose CRC is perfectly
/// good is still refused if there is one stray byte after it in the block.
///
/// Both committed fixtures, from two unrelated tools, zero the tail, and UEFI 2.10 §5.3.2 requires
/// it. If a real disk ever trips this, the fix is to relax the check and record the disk that did
/// it, not to weaken the CRC.
#[test]
fn a_stray_byte_after_the_header_is_refused_even_with_a_valid_crc() {
    let disk = build(&[]).unwrap();
    let mut block: [u8; BLOCK] = disk.header().try_into().unwrap();
    assert!(Header::decode(&block).is_ok());
    block[BLOCK - 1] = 0xFF;
    assert_eq!(Header::decode(&block).err(), Some(Error::BlockTailNotZero));
}

#[test]
fn the_protective_mbr_round_trips_and_saturates() {
    let mut block = [0u8; BLOCK];
    mbr::write(&mut block, BLOCKS).unwrap();
    mbr::validate(&block, BLOCKS).unwrap();
    assert_eq!(block[510..], [0x55, 0xAA]);
    assert_eq!(block[450], mbr::PROTECTIVE_OS_TYPE);

    // A disk bigger than 32 bits of blocks: the size field saturates, and validation accepts the
    // saturated value on ANY disk, because at that point the field has stopped carrying
    // information and the alternative is refusing every disk over 2 TiB.
    let huge = 1u64 << 40;
    mbr::write(&mut block, huge).unwrap();
    assert_eq!(&block[458..462], &u32::MAX.to_le_bytes());
    mbr::validate(&block, huge).unwrap();
    mbr::validate(&block, BLOCKS).expect("0xFFFFFFFF is accepted everywhere, by construction");

    // An exact size against a different disk is wrong, though, which is what catches an MBR copied
    // from one disk to another.
    mbr::write(&mut block, BLOCKS).unwrap();
    assert_eq!(
        mbr::validate(&block, BLOCKS / 2).err(),
        Some(Error::Mbr(MbrProblem::SizeLba(BLOCKS as u32 - 1)))
    );

    mbr::write(&mut block, BLOCKS).unwrap();
    block[510] = 0;
    assert_eq!(
        mbr::validate(&block, BLOCKS).err(),
        Some(Error::Mbr(MbrProblem::Signature))
    );

    mbr::write(&mut block, BLOCKS).unwrap();
    block[450] = 0x83;
    assert_eq!(
        mbr::validate(&block, BLOCKS).err(),
        Some(Error::Mbr(MbrProblem::NoProtectiveRecord))
    );

    mbr::write(&mut block, BLOCKS).unwrap();
    block[454..458].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(
        mbr::validate(&block, BLOCKS).err(),
        Some(Error::Mbr(MbrProblem::StartLba(2)))
    );
}

/// Names are UTF-16, so anything outside the Basic Multilingual Plane costs two of the 36 code
/// units. The round trip has to survive that, and an entry off a hostile disk with an unpaired
/// surrogate has to come back as replacement characters rather than an error: the name is a label,
/// and refusing to list a partition because its cosmetic field is broken is the wrong trade.
#[test]
fn names_survive_utf16_and_hostile_bytes_do_not_break_reading() {
    let mut buf = [0u8; 4 * entry::NAME_UNITS];
    for text in [
        "",
        "cricker data",
        "übergrößenträger",
        "36 chars of ordinary label text here",
    ] {
        let e = Entry::new(types::CRICKER_DATA, PART, 10, 20)
            .with_name(text)
            .unwrap_or_else(|_| panic!("{text:?} should fit"));
        let n = e.name_utf8(&mut buf).unwrap();
        assert_eq!(core::str::from_utf8(&buf[..n]).unwrap(), text);
        // And through the bytes, which is where a length or endianness bug would show.
        let n = Entry::decode(&e.encode()).name_utf8(&mut buf).unwrap();
        assert_eq!(core::str::from_utf8(&buf[..n]).unwrap(), text);
    }

    // 36 code units fit and 37 do not; an emoji is a surrogate pair, so two units.
    let base = Entry::new(types::CRICKER_DATA, PART, 10, 20);
    assert!(base.with_name(&"a".repeat(36)).is_ok());
    assert_eq!(
        base.with_name(&"a".repeat(37)).err(),
        Some(Error::NameTooLong)
    );
    assert!(base.with_name(&"\u{1F600}".repeat(18)).is_ok());
    assert_eq!(
        base.with_name(&"\u{1F600}".repeat(19)).err(),
        Some(Error::NameTooLong)
    );

    let mut hostile = base;
    hostile.name[0] = 0xD800; // a high surrogate with nothing after it
    hostile.name[1] = b'x' as u16;
    let n = hostile.name_utf8(&mut buf).unwrap();
    assert_eq!(core::str::from_utf8(&buf[..n]).unwrap(), "\u{FFFD}x");

    // An output buffer that cannot hold the name is an error rather than a truncation.
    let named = base.with_name("cricker data").unwrap();
    assert_eq!(
        named.name_utf8(&mut [0u8; 4]).err(),
        Some(Error::NameTooLong)
    );
}

#[test]
fn an_entry_is_exactly_its_bytes() {
    let e = Entry {
        type_guid: types::LINUX_FILESYSTEM,
        unique_guid: PART,
        first_lba: 0x0102_0304_0506_0708,
        last_lba: 0x1112_1314_1516_1718,
        attributes: entry::ATTR_REQUIRED | entry::ATTR_LEGACY_BIOS_BOOTABLE,
        name: [0; entry::NAME_UNITS],
    };
    let bytes = e.encode();
    assert_eq!(bytes.len(), 128);
    assert_eq!(&bytes[32..40], &e.first_lba.to_le_bytes());
    assert_eq!(&bytes[40..48], &e.last_lba.to_le_bytes());
    assert_eq!(bytes[48], 0b101);
    assert_eq!(Entry::decode(&bytes), e);
    assert_eq!(Entry::decode(&[0u8; 128]), Entry::UNUSED);
    assert_eq!(e.blocks(), Some(e.last_lba - e.first_lba + 1));

    let backwards = Entry::new(types::CRICKER_DATA, PART, 20, 10);
    assert_eq!(
        backwards.blocks(),
        None,
        "not an empty partition, a broken one"
    );
    let one = Entry::new(types::CRICKER_DATA, PART, 20, 20);
    assert_eq!(one.blocks(), Some(1), "last_lba is inclusive");
}

#[test]
fn block_sizes_are_powers_of_two_between_512_and_4096() {
    assert!([512, 1024, 2048, 4096].into_iter().all(block_size_ok));
    assert!(
        ![0usize, 1, 256, 511, 513, 768, 8192]
            .into_iter()
            .any(block_size_ok)
    );
}

/// The sample the documentation examples run on is a real table, not a plausible-looking buffer.
#[test]
fn the_documentation_sample_is_a_valid_disk() {
    let disk = testing::sample_disk();
    let table = Gpt::parse(&disk[512..1024], &disk[1024..]).unwrap();
    table.check_protective_mbr(&disk[..512]).unwrap();
    assert_eq!(table.partitions().count(), 1);
}

// =================================================================================================
// The layout guards, boundary by boundary. Milestone 85's mutation run showed the suite could not
// tell `>` from `>=` in any of `parse`'s range checks: `real_disks.rs` corrupts one byte at a time,
// so every corrupt header dies at the CRC before the layout logic ever runs, and every table
// `build` makes is TIGHT (array against usable range, usable range against backup array), so the
// comparisons below were never exercised one side at a time. These tests forge headers with a
// VALID CRC and one layout lie each. One of the survivors was not a missing test but a missing `=`:
// `parse` accepted last_usable_lba equal to the backup array's first block. See
// notes/mutation-testing.md.

/// Recompute the header CRC after a patch, so the forgery reaches the layout checks.
fn reforge(block: &mut [u8]) {
    let hsize = u32::from_le_bytes(block[12..16].try_into().unwrap()) as usize;
    block[16..20].fill(0);
    let crc = gpt::crc::crc32(&block[..hsize]);
    block[16..20].copy_from_slice(&crc.to_le_bytes());
}

/// Parse a tight, empty table whose header has been patched and re-CRCed.
fn parse_patched(patch: impl FnOnce(&mut [u8])) -> Result<(), Error> {
    let disk = build(&[]).unwrap();
    let mut block: [u8; BLOCK] = disk.header().try_into().unwrap();
    patch(&mut block);
    reforge(&mut block);
    Gpt::parse(&block, &disk.array).map(|_| ())
}

fn set_u64(block: &mut [u8], offset: usize, value: u64) {
    block[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

// UEFI 2.10 §5.3.2 header field offsets.
const ALTERNATE_LBA: usize = 32;
const FIRST_USABLE_LBA: usize = 40;
const LAST_USABLE_LBA: usize = 48;

/// The tight table `build` makes, in numbers the tests below patch around: array at LBA 2..=33,
/// usable at 34..=131038, backup array at 131039..=131070, backup header at 131071.
const PRIMARY_END: u64 = 34;
const BACKUP_ARRAY_FIRST: u64 = BLOCKS - 33;

#[test]
fn a_one_block_usable_range_is_a_range_and_a_backwards_one_is_not() {
    // first == last is one usable block, which is legal.
    assert_eq!(
        parse_patched(|b| set_u64(b, LAST_USABLE_LBA, PRIMARY_END)).err(),
        None
    );
    // first > last is no range at all.
    assert_eq!(
        parse_patched(|b| set_u64(b, LAST_USABLE_LBA, PRIMARY_END - 1)).err(),
        Some(Error::UsableRange {
            first: PRIMARY_END,
            last: PRIMARY_END - 1
        })
    );
}

#[test]
fn the_usable_range_may_start_after_the_table_but_not_inside_it() {
    // Slack between the primary table and the usable range is legal; every table `build` makes is
    // tight, so without this a `>` here could rot into `<` unnoticed.
    assert_eq!(
        parse_patched(|b| set_u64(b, FIRST_USABLE_LBA, 2048)).err(),
        None
    );
    // A usable range that starts on the entry array's last block is an overlap.
    assert_eq!(
        parse_patched(|b| set_u64(b, FIRST_USABLE_LBA, PRIMARY_END - 1)).err(),
        Some(Error::TableOverlapsUsable)
    );
}

#[test]
fn a_disk_with_no_room_for_the_backup_is_refused() {
    // alternate_lba says the disk is 11 blocks; the backup array alone needs 33.
    assert_eq!(
        parse_patched(|b| {
            set_u64(b, ALTERNATE_LBA, 10);
            set_u64(b, FIRST_USABLE_LBA, PRIMARY_END);
            set_u64(b, LAST_USABLE_LBA, PRIMARY_END);
        })
        .err(),
        Some(Error::TableOverlapsUsable)
    );
}

/// The bug the mutation run found: `block_count - backup_reserved` is the backup array's first
/// block, and the check was `>`, so equality (one usable block INSIDE the backup array) parsed
/// clean. A partition placed on that block would overwrite the backup entry array.
#[test]
fn the_usable_range_stops_before_the_backup_array() {
    // The tight maximum, one below the backup array, is legal (this is what `create` emits).
    assert_eq!(
        parse_patched(|b| set_u64(b, LAST_USABLE_LBA, BACKUP_ARRAY_FIRST - 1)).err(),
        None
    );
    // Equal to the backup array's first block: refused, since the fix.
    assert_eq!(
        parse_patched(|b| set_u64(b, LAST_USABLE_LBA, BACKUP_ARRAY_FIRST)).err(),
        Some(Error::TableOverlapsUsable)
    );
    // Past it, likewise.
    assert_eq!(
        parse_patched(|b| set_u64(b, LAST_USABLE_LBA, BACKUP_ARRAY_FIRST + 1)).err(),
        Some(Error::TableOverlapsUsable)
    );
}

#[test]
fn the_entry_array_shape_guards_are_exact() {
    // Empty is not a shape: there is no entry count to derive.
    assert_eq!(
        Gpt::create(DISK, BLOCK, BLOCKS, &[], &mut []).err(),
        Some(Error::EntryArrayShape { have: 0 })
    );
    // Longer than an entry but not a whole number of them.
    assert_eq!(
        Gpt::create(DISK, BLOCK, BLOCKS, &[], &mut [0u8; 200]).err(),
        Some(Error::EntryArrayShape { have: 200 })
    );
    // Exactly one entry is the smallest legal array, and it holds exactly one partition.
    let one = Entry::new(types::CRICKER_DATA, PART, 2048, 4096);
    let mut array = [0u8; entry::SIZE];
    let table = Gpt::create(DISK, BLOCK, BLOCKS, &[one], &mut array).unwrap();
    assert_eq!(table.partitions().count(), 1);
}

/// `check_protective_mbr` is a thin wrapper over `mbr::validate`, and every other call in the
/// suite hands it a good block, so a body replaced with `Ok(())` passed (a milestone 85
/// survivor). One bad block through the wrapper is what pins the plumbing.
#[test]
fn a_wrong_mbr_is_refused_through_the_wrapper_too() {
    let disk = build(&[]).unwrap();
    let table = Gpt::parse(disk.header(), &disk.array).unwrap();
    let zeros = [0u8; BLOCK];
    assert!(table.check_protective_mbr(&zeros).is_err());
}

// The second mutation pass reached the modules the first never got to (entry, guid, header, the
// MBR validator), and its survivors have the same two shapes as the first pass's: boundaries
// never met exactly, and values every fixture happened to share. See notes/mutation-testing.md.

/// Every fixture and every built table uses 128-byte entries, so `check_entry_array`'s size guard
/// was only ever met at its own boundary. A 256-byte-entry table is legal and a 64-byte one is
/// not, and the array CRC does not care because both describe the same 16 KiB of zeros.
#[test]
fn entry_sizes_other_than_128_are_judged_not_assumed() {
    // 64 entries of 256 bytes: same 16 KiB, same CRC, legal.
    assert_eq!(
        parse_patched(|b| {
            b[80..84].copy_from_slice(&64u32.to_le_bytes());
            b[84..88].copy_from_slice(&256u32.to_le_bytes());
        })
        .err(),
        None
    );
    // 256 entries of 64 bytes: under the 128-byte floor.
    assert_eq!(
        parse_patched(|b| {
            b[80..84].copy_from_slice(&256u32.to_le_bytes());
            b[84..88].copy_from_slice(&64u32.to_le_bytes());
        })
        .err(),
        Some(Error::EntrySize(64))
    );
}

/// A header may claim every byte of its block: `header_size == block size` leaves no reserved
/// tail to check and is legal, and the bound's `>` was never met exactly.
#[test]
fn a_header_the_size_of_its_block_is_legal() {
    assert_eq!(
        parse_patched(|b| b[12..16].copy_from_slice(&512u32.to_le_bytes())).err(),
        None
    );
}

/// A partition may start on the first usable block. Every fixture starts partitions at 2048 with
/// usable space from 34, so `check_partitions`' lower bound was never met exactly either.
#[test]
fn a_partition_on_the_first_usable_block_is_inside_the_range() {
    let mut array = [0u8; ENTRY_ARRAY_BYTES];
    let part = Entry::new(types::CRICKER_DATA, PART, 34, 4096);
    let table = Gpt::create(DISK, BLOCK, BLOCKS, &[part], &mut array).unwrap();
    assert_eq!(table.partitions().next().unwrap().1.first_lba, 34);
}

/// The protective record may sit in any of the four MBR slots. Every tool we imaged puts it in
/// slot 0, where `i * 16` is zero under any arithmetic, so the record-offset math was untested
/// for every other slot.
#[test]
fn a_protective_record_in_a_later_slot_is_still_protective() {
    let disk = build(&[]).unwrap();
    let table = Gpt::parse(disk.header(), &disk.array).unwrap();
    let mut block = [0u8; BLOCK];
    block[..disk.mbr().len()].copy_from_slice(disk.mbr());
    // Move record 0 to record 2 (16 bytes each, table at offset 446), zeroing slot 0.
    let (a, b) = (446, 446 + 32);
    let record: [u8; 16] = block[a..a + 16].try_into().unwrap();
    block[a..a + 16].fill(0);
    block[b..b + 16].copy_from_slice(&record);
    table.check_protective_mbr(&block).unwrap();
}

/// Refusals speak: one formatted error, exact, so a Display replaced with `Ok(())` cannot pass.
#[test]
fn an_error_formats_to_its_own_words() {
    assert_eq!(
        Error::BlockSize(100).to_string(),
        "100 is not a logical block size (512..=4096, a power of two)"
    );
}

/// The attribute bits are the on-disk format sgdisk and firmware read; pin them. (`1 << 0` is
/// immune to shift-direction mutation by arithmetic; the others are not.)
#[test]
fn the_attribute_bits_are_the_disk_format() {
    assert_eq!(
        [
            entry::ATTR_REQUIRED,
            entry::ATTR_NO_BLOCK_IO,
            entry::ATTR_LEGACY_BIOS_BOOTABLE
        ],
        [1, 2, 4]
    );
}

/// A non-ASCII name survives the round trip. Every test name was ASCII, whose UTF-16 units have a
/// zero high byte, so a decode reading its two name bytes off by one produced the same units and
/// the whole name path looked healthy.
#[test]
fn a_name_beyond_ascii_round_trips() {
    let named = Entry::new(types::CRICKER_DATA, PART, 2048, 4096)
        .with_name("π¥")
        .unwrap();
    let back = Entry::decode(&named.encode());
    let mut buf = [0u8; 16];
    let n = back.name_utf8(&mut buf).unwrap();
    assert_eq!(&buf[..n], "π¥".as_bytes());
}

/// `name_utf8`'s bound, met exactly: a buffer the name exactly fills is enough, and one byte
/// under is `NameTooLong`, never a panic.
#[test]
fn an_exact_fit_name_buffer_is_enough() {
    let e = Entry::new(types::CRICKER_DATA, PART, 2048, 4096)
        .with_name("abc")
        .unwrap();
    let mut exact = [0u8; 3];
    assert_eq!(e.name_utf8(&mut exact), Ok(3));
    assert_eq!(&exact, b"abc");
    let mut short = [0u8; 2];
    assert_eq!(e.name_utf8(&mut short), Err(Error::NameTooLong));
}

/// A GUID displays as the string a person looks up, through Display and Debug both, and the
/// unused type has its name.
#[test]
fn a_guid_shows_itself_and_unused_has_a_name() {
    let g = Guid::from_fields(0x1234_5678, 0x9ABC, 0x4DEF, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(g.to_string(), "12345678-9ABC-4DEF-0102-030405060708");
    assert_eq!(format!("{g:?}"), "12345678-9ABC-4DEF-0102-030405060708");
    assert_eq!(types::name(types::UNUSED), Some("unused"));
}
