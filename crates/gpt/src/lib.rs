//! **The GUID Partition Table**: reading one, writing one, and refusing a bad one.
//!
//! (The three letters are the disk format from UEFI 2.10 §5.3, which predates the machine-learning
//! sense of them by about fifteen years. There are no language models in here.)
//!
//! A partition table is the map that says "the filesystem starts at block 2048 and stops at block
//! 32767". Milestone 57 needs it in both directions: **reading** one is how a kernel finds a
//! partition on a disk it has never seen, and there is no way around that; **writing** one is the
//! `parted mkpart` equivalent, the first of the three steps in calef's router setup that this OS
//! has no answer to.
//!
//! # This crate does no I/O, on purpose
//!
//! Nothing here reads or writes a block device. Every function takes a byte buffer the caller
//! filled and returns bytes the caller will place. That is the same discipline `dtb` and `elf`
//! follow, and the reason is not purity: it is that the whole crate then compiles for the host and
//! its tests run in milliseconds against real disks made by real tools, instead of inside a QEMU
//! boot (DECISIONS §7, §14).
//!
//! It also allocates nothing and has no `unsafe`. The entry array is a caller-supplied buffer, so a
//! kernel can hand it a stack array and a userspace tool can hand it a `Vec`.
//!
//! [`span`] does not break that rule and is worth saying why: it computes *where* to read, and
//! arithmetic between two block sizes is exactly the kind of thing this crate is for. A GPT counts
//! in 512-byte logical blocks; the block service a nife program holds moves 4096 bytes per
//! request. Somebody has to reconcile those, and doing it in a driver means doing it three times.
//!
//! # What a GPT actually is, in the order the bytes appear
//!
//! ```text
//!   LBA 0     protective MBR   one fake "the whole disk is in use" record, type 0xEE
//!   LBA 1     primary header   92 bytes: signature, geometry, two CRC32s, disk GUID
//!   LBA 2..   entry array      128 entries x 128 bytes = 16 KiB = 32 blocks
//!   ...       the partitions themselves, between first_usable_lba and last_usable_lba
//!   LBA n-33  backup array     a second copy of the same 16 KiB
//!   LBA n-1   backup header    the same header, with my_lba and alternate_lba swapped
//! ```
//!
//! Four things carry the integrity of that: the header CRC and the array CRC, each stored twice.
//! **A GPT is a format that can tell you it is broken**, which is exactly why every check in here
//! is an error and none of them is a warning. A table that half-validates is a table you are about
//! to write a filesystem on top of.
//!
//! # The layering, and why the errors are where they are
//!
//! - [`header::Header::decode`] judges one block on its own terms: is this a GPT header at all?
//! - [`entry::Entry::decode`] judges nothing. It is **total**, every bit pattern decodes, and the
//!   round trip is the identity. There is no such thing as a malformed entry, only an entry that
//!   makes no sense on a particular disk.
//! - [`Gpt::parse`] is where the disk shows up, and therefore where every geometry rule lives: the
//!   array CRC, the usable range, partitions that run off the end, partitions that overlap.
//! - [`Gpt::check_backup`] compares the two copies. A disk whose halves disagree has had something
//!   happen to it and is not to be trusted.
//!
//! # Using it
//!
//! Reading, given a way to fetch blocks:
//!
//! ```
//! # use gpt::{Gpt, guid::types};
//! # let disk = gpt::testing::sample_disk();
//! # let (header_block, entry_array) = (&disk[512..1024], &disk[1024..1024 + 16384]);
//! let table = Gpt::parse(header_block, entry_array)?;
//! for (index, part) in table.partitions() {
//!     let mut label = [0u8; 4 * gpt::entry::NAME_UNITS];
//!     let n = part.name_utf8(&mut label)?;
//!     println!(
//!         "{}: {} blocks of {}, {:?}",
//!         index + 1,
//!         part.blocks().unwrap(),
//!         types::name(part.type_guid).unwrap_or("unknown"),
//!         core::str::from_utf8(&label[..n]).unwrap(),
//!     );
//! }
//! # Ok::<(), gpt::Error>(())
//! ```
//!
//! Writing, which is four block ranges the caller then puts on the disk:
//!
//! ```
//! # use gpt::{Entry, Gpt, guid::{Guid, types}};
//! # let disk_guid = Guid::ZERO;
//! # let part_guid = Guid::ZERO;
//! let mut array = [0u8; gpt::ENTRY_ARRAY_BYTES];
//! let table = Gpt::create(
//!     disk_guid,
//!     512,
//!     131_072, // a 64 MiB disk
//!     &[Entry::new(types::NIFE_DATA, part_guid, 2048, 131_038).with_name("nife data")?],
//!     &mut array,
//! )?;
//!
//! let mut block = [0u8; 512];
//! table.write_protective_mbr(&mut block)?; // to LBA 0
//! table.write_primary_header(&mut block)?; // to LBA 1
//! table.write_backup_header(&mut block)?;  // to table.backup_header_lba()
//! let bytes = table.entry_array();          // to LBA 2 and to table.backup_entry_lba()
//! # Ok::<(), gpt::Error>(())
//! ```
//!
//! See notes/gpt.md for the format walk-through and the surprises the fixtures pinned.
//!
//! Name: ratified 2026-08-01 (calef, the naming tenet in CLAUDE.md). Named in the group of standard
//! terms that are already right and must not be touched, because a name a reader knows from outside
//! this project costs nothing to learn and renaming it would destroy the recognition the tenet
//! exists to buy.

#![no_std]
// milestone 68's ratchet is workspace-wide (§107); this crate opts out until its 23-item
// worklist (notes/doc-coverage.md) is burned down.
#![allow(missing_docs)]

pub mod crc;
pub mod entry;
pub mod guid;
pub mod header;
pub mod span;

use crc::crc32;
pub use entry::Entry;
pub use guid::Guid;
pub use header::Header;

/// The smallest logical block a GPT disk can have, and what almost every disk reports.
pub const MIN_BLOCK_SIZE: usize = 512;

/// The largest logical block this crate handles. 4096 is what a native-4K drive reports, and the
/// header still occupies exactly one block, so the only thing that changes is the LBA arithmetic.
pub const MAX_BLOCK_SIZE: usize = 4096;

/// The primary header always lives at LBA 1, immediately after the protective MBR. Not a
/// convention: the spec fixes it, because it is the one address a reader can know before it has
/// read anything.
pub const PRIMARY_HEADER_LBA: u64 = 1;

/// Entries in a table written by [`Gpt::create`] with a [`ENTRY_ARRAY_BYTES`]-sized buffer, and
/// what every tool in the world writes.
pub const DEFAULT_ENTRY_COUNT: u32 = 128;

/// 16 KiB: `DEFAULT_ENTRY_COUNT * entry::SIZE`, and the minimum array size UEFI 2.10 §5.3.3
/// requires of a *conformant writer*.
///
/// [`Gpt::create`] will build a smaller array if handed a smaller buffer, and [`Gpt::parse`] will
/// read one, because refusing a table we can read perfectly well would be the wrong kind of strict.
/// Anything writing a real disk should pass a buffer of exactly this size.
pub const ENTRY_ARRAY_BYTES: usize = DEFAULT_ENTRY_COUNT as usize * entry::SIZE;

/// True if `len` is a logical block size this crate accepts: a power of two from
/// [`MIN_BLOCK_SIZE`] to [`MAX_BLOCK_SIZE`].
///
/// Block size is never a parameter in this crate's API. A caller passes the block it read, and the
/// slice's own length is the block size, which removes a whole class of "we told it 512 and handed
/// it 4096" bug.
pub const fn block_size_ok(len: usize) -> bool {
    len >= MIN_BLOCK_SIZE && len <= MAX_BLOCK_SIZE && len.is_power_of_two()
}

/// Everything this crate refuses, and why.
///
/// One variant per rule, carrying the numbers that make the failure diagnosable, because "invalid
/// GPT" is useless to somebody holding the only copy of their data.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// A buffer offered as one logical block is not a legal block size. See [`block_size_ok`].
    BlockSize(usize),
    /// The eight bytes at offset 0 are not `EFI PART`. Usually means "this disk has no GPT",
    /// which is a normal answer rather than a corruption.
    Signature,
    /// A revision other than [`header::REVISION`]. Refused rather than guessed at.
    Revision(u32),
    /// `HeaderSize` outside `92 ..= block_size`.
    HeaderSize(u32),
    /// The reserved `u32` at offset 20 is not zero.
    ReservedNotZero,
    /// A non-zero byte after the header in its block, which UEFI 2.10 §5.3.2 forbids.
    BlockTailNotZero,
    /// The header's own CRC does not match its bytes. Something rewrote part of the header.
    HeaderCrc { stored: u32, computed: u32 },
    /// `SizeOfPartitionEntry` is under 128 or is not a multiple of 8.
    EntrySize(u32),
    /// `NumberOfPartitionEntries` is zero, or so large that the array cannot be addressed.
    EntryCount(u32),
    /// The caller supplied fewer entry-array bytes than the header says the array has. Not a
    /// corrupt disk: a caller that read too few blocks.
    EntryArrayLen { need: usize, have: usize },
    /// [`Gpt::create`] was handed an entry-array buffer that is not a whole number of 128-byte
    /// entries, or is empty. The buffer's size *is* `NumberOfPartitionEntries`, so it cannot be a
    /// fractional entry.
    EntryArrayShape { have: usize },
    /// The entry array's CRC does not match the header's record of it.
    EntryArrayCrc { stored: u32, computed: u32 },
    /// [`Gpt::parse`] was given a header that says it lives somewhere other than LBA 1. A backup
    /// header goes to [`Gpt::check_backup`], which knows what to compare it against.
    NotPrimary { my_lba: u64 },
    /// `FirstUsableLBA` is above `LastUsableLBA`, so the disk has no usable blocks at all.
    UsableRange { first: u64, last: u64 },
    /// The table does not fit outside the usable range: either the primary array runs into
    /// `FirstUsableLBA`, or `LastUsableLBA` runs into the backup array or header. A partition
    /// placed in that space would be overwritten by the next table update.
    TableOverlapsUsable,
    /// A used entry with `last_lba < first_lba`, which is not a partition.
    PartitionRange { index: usize },
    /// A used entry that starts below `FirstUsableLBA` or ends above `LastUsableLBA`. This is the
    /// check that catches a table copied onto a smaller disk.
    PartitionOutsideUsable { index: usize },
    /// Two used entries claim a block in common. Zero-indexed, and always reported with the lower
    /// index first.
    PartitionsOverlap { a: usize, b: usize },
    /// The backup header disagrees with the primary about something they must agree on.
    Backup(BackupMismatch),
    /// The protective MBR at LBA 0 is wrong. See [`MbrProblem`].
    Mbr(MbrProblem),
    /// [`Gpt::create`] was asked to build a table on a disk with no room for the tables themselves.
    DiskTooSmall { blocks: u64, need: u64 },
    /// More partitions than the entry array has room for.
    TooManyPartitions { given: usize, room: u32 },
    /// A partition name that does not fit in 36 UTF-16 code units, or an output buffer too small
    /// to receive one.
    NameTooLong,
}

/// Which field the backup header disagreed with the primary on.
///
/// Every one of these is a field the two copies must hold identically, so a mismatch means one of
/// them was written by something that did not update the other. That is usually a resize or a
/// clone-to-a-different-disk, and it is exactly the state where guessing which copy is right does
/// real damage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BackupMismatch {
    /// The backup does not think it lives where the primary says it does.
    MyLba,
    /// The backup does not point back at the primary.
    AlternateLba,
    /// Different disk GUIDs: these are two different disks' tables.
    DiskGuid,
    FirstUsableLba,
    LastUsableLba,
    EntryCount,
    EntrySize,
    /// The two copies of the entry array are not the same array.
    EntryArrayCrc,
    /// The backup's array is not immediately below the backup header, where it must be.
    EntryArrayLba,
}

/// What is wrong with the protective MBR.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MbrProblem {
    /// The `0x55AA` at offset 510 is missing. Not an MBR at all.
    Signature,
    /// No partition record of type `0xEE`. A disk with a real MBR partition table and no GPT.
    NoProtectiveRecord,
    /// A second non-empty record beside the protective one: a **hybrid MBR**, where the same disk
    /// is described twice and the two descriptions can disagree. Refused deliberately; see
    /// [`mbr::validate`].
    ExtraRecord { index: usize },
    /// The protective record does not start at LBA 1, so it does not cover the GPT it protects.
    StartLba(u32),
    /// The protective record's size does not cover the disk. Its whole job is to make a
    /// GPT-unaware tool see a full disk.
    SizeLba(u32),
}

/// A parsed or freshly built GUID Partition Table.
///
/// Borrows the entry array rather than copying it: 16 KiB is too much to move around a kernel, and
/// the caller already has the bytes.
#[derive(Clone, Copy, Debug)]
pub struct Gpt<'a> {
    header: Header,
    /// Exactly `entry_count * entry_size` bytes, the span the array CRC covers. Trimmed on the way
    /// in so nothing downstream has to remember to.
    entries: &'a [u8],
    block_size: usize,
}

impl<'a> Gpt<'a> {
    /// Parse a primary GPT.
    ///
    /// `header_block` is the single block read from LBA 1; its length is taken as the disk's
    /// logical block size. `entry_array` is the bytes read starting at the header's
    /// `PartitionEntryLBA`, and may be longer than the array (a caller reading whole blocks will
    /// usually have a few spare).
    ///
    /// Everything is checked, and any failure is an error: the header's own structure and CRC (via
    /// [`Header::decode`]), the entry array's CRC, that the table fits outside the usable range,
    /// that every used partition lies inside it, and that no two partitions overlap.
    ///
    /// **This does not look at the backup.** Pass it to [`Gpt::check_backup`], which is separate
    /// because reading the last block of the disk is a second I/O the caller may not want to do
    /// before it knows the primary is sound.
    pub fn parse(header_block: &[u8], entry_array: &'a [u8]) -> Result<Gpt<'a>, Error> {
        let block_size = header_block.len();
        let header = Header::decode(header_block)?;

        if header.my_lba != PRIMARY_HEADER_LBA {
            return Err(Error::NotPrimary {
                my_lba: header.my_lba,
            });
        }

        let entries = check_entry_array(&header, entry_array)?;
        let array_blocks = array_blocks(&header, block_size)?;

        // The disk's size, as the primary header understands it: the backup header is on the last
        // block, so there is one more block than its address.
        let block_count = header
            .alternate_lba
            .checked_add(1)
            .ok_or(Error::TableOverlapsUsable)?;

        if header.first_usable_lba > header.last_usable_lba {
            return Err(Error::UsableRange {
                first: header.first_usable_lba,
                last: header.last_usable_lba,
            });
        }
        // The primary table (MBR, header, array) must end before the usable range starts, and the
        // usable range must end before the backup array starts. Written as additions on the low
        // side and subtractions guarded by comparison on the high side, because a hostile header
        // can put any u64 in any of these fields.
        let primary_end = header
            .entry_array_lba
            .checked_add(array_blocks)
            .ok_or(Error::TableOverlapsUsable)?;
        let backup_reserved = array_blocks
            .checked_add(1)
            .ok_or(Error::TableOverlapsUsable)?;
        // `>=`, not `>`: `block_count - backup_reserved` IS the backup array's first block (it is
        // what `backup_entry_lba` computes), so a last_usable_lba equal to it puts one usable
        // block inside the backup array. The `>` this shipped with accepted that header, and no
        // test noticed until milestone 85's mutation run reported `> with >=` as a survivor: the
        // mutant was the fix. See notes/mutation-testing.md.
        if header.entry_array_lba <= PRIMARY_HEADER_LBA
            || primary_end > header.first_usable_lba
            || block_count < backup_reserved
            || header.last_usable_lba >= block_count - backup_reserved
        {
            return Err(Error::TableOverlapsUsable);
        }

        check_partitions(entries, header.entry_size as usize, &header)?;

        Ok(Gpt {
            header,
            entries,
            block_size,
        })
    }

    /// Build a new table. The `mkpart` half of the milestone.
    ///
    /// `entry_array` is the caller's buffer for the entry array and must be a whole number of
    /// entries; its size sets `NumberOfPartitionEntries`, so pass `[0u8; `[`ENTRY_ARRAY_BYTES`]`]`
    /// for the 128 entries every real disk has. It is overwritten completely, unused entries
    /// included, because a leftover entry from a previous table is a partition that comes back from
    /// the dead.
    ///
    /// The partitions are checked exactly as [`Gpt::parse`] checks a disk's: inside the usable
    /// range, non-empty, non-overlapping. **Placement is the caller's job**, deliberately: alignment
    /// (the 2048-block, 1 MiB convention that keeps a partition from straddling an SSD erase block)
    /// is policy, and a format crate that silently moved a partition would be doing policy behind
    /// the caller's back.
    pub fn create(
        disk_guid: Guid,
        block_size: usize,
        block_count: u64,
        partitions: &[Entry],
        entry_array: &'a mut [u8],
    ) -> Result<Gpt<'a>, Error> {
        if !block_size_ok(block_size) {
            return Err(Error::BlockSize(block_size));
        }
        if entry_array.len() < entry::SIZE || !entry_array.len().is_multiple_of(entry::SIZE) {
            return Err(Error::EntryArrayShape {
                have: entry_array.len(),
            });
        }
        let entry_count = (entry_array.len() / entry::SIZE) as u32;
        if partitions.len() > entry_count as usize {
            return Err(Error::TooManyPartitions {
                given: partitions.len(),
                room: entry_count,
            });
        }

        let array_bytes = entry_array.len() as u64;
        let array_blocks = array_bytes.div_ceil(block_size as u64);
        // LBA 0 is the protective MBR, LBA 1 the header, then the array; the backup is the array
        // again and then the header, at the very end. Everything between is usable.
        let first_usable = PRIMARY_HEADER_LBA + 1 + array_blocks;
        // The smallest disk this table fits on: the primary table, one usable block, the backup
        // array and the backup header.
        let need = first_usable + array_blocks + 2;
        if block_count < need {
            return Err(Error::DiskTooSmall {
                blocks: block_count,
                need,
            });
        }
        let last_usable = block_count - array_blocks - 2;

        entry_array.fill(0);
        for (i, part) in partitions.iter().enumerate() {
            entry_array[i * entry::SIZE..(i + 1) * entry::SIZE].copy_from_slice(&part.encode());
        }

        let header = Header {
            my_lba: PRIMARY_HEADER_LBA,
            alternate_lba: block_count - 1,
            first_usable_lba: first_usable,
            last_usable_lba: last_usable,
            disk_guid,
            entry_array_lba: PRIMARY_HEADER_LBA + 1,
            entry_count,
            entry_size: entry::SIZE_U32,
            entry_array_crc: crc32(entry_array),
        };

        check_partitions(entry_array, entry::SIZE, &header)?;

        Ok(Gpt {
            header,
            entries: entry_array,
            block_size,
        })
    }

    /// Check the backup copy against this one.
    ///
    /// `backup_block` is the last block of the disk; `backup_entries` is read from
    /// [`Gpt::backup_entry_lba`]. Every field the two headers must share is compared, and the two
    /// arrays are compared by CRC.
    ///
    /// **Comparing by CRC rather than byte for byte is deliberate.** The header's array CRC is what
    /// the format itself uses to bind a header to its array, so two arrays that CRC alike and
    /// differ in bytes would mean a CRC-32 collision, which is a stronger adversary than this
    /// format defends against anywhere else. It also means a caller can check the backup without
    /// holding both 16 KiB arrays at once.
    pub fn check_backup(&self, backup_block: &[u8], backup_entries: &[u8]) -> Result<(), Error> {
        let backup = Header::decode(backup_block)?;

        let mismatch = |m: BackupMismatch| Err(Error::Backup(m));
        if backup.my_lba != self.header.alternate_lba {
            return mismatch(BackupMismatch::MyLba);
        }
        if backup.alternate_lba != self.header.my_lba {
            return mismatch(BackupMismatch::AlternateLba);
        }
        if backup.disk_guid != self.header.disk_guid {
            return mismatch(BackupMismatch::DiskGuid);
        }
        if backup.first_usable_lba != self.header.first_usable_lba {
            return mismatch(BackupMismatch::FirstUsableLba);
        }
        if backup.last_usable_lba != self.header.last_usable_lba {
            return mismatch(BackupMismatch::LastUsableLba);
        }
        if backup.entry_count != self.header.entry_count {
            return mismatch(BackupMismatch::EntryCount);
        }
        if backup.entry_size != self.header.entry_size {
            return mismatch(BackupMismatch::EntrySize);
        }
        if backup.entry_array_crc != self.header.entry_array_crc {
            return mismatch(BackupMismatch::EntryArrayCrc);
        }
        if backup.entry_array_lba != self.backup_entry_lba() {
            return mismatch(BackupMismatch::EntryArrayLba);
        }

        // Validates the bytes the caller actually read, which is the point: the field comparison
        // above only proves the two headers agree, not that the backup array on the disk is intact.
        check_entry_array(&backup, backup_entries)?;
        Ok(())
    }

    /// The decoded primary header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The GUID naming this disk.
    pub fn disk_guid(&self) -> Guid {
        self.header.disk_guid
    }

    /// The logical block size, taken from the length of the header block.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// How many blocks the disk has, according to the header (`alternate_lba + 1`).
    pub fn block_count(&self) -> u64 {
        self.header.alternate_lba + 1
    }

    /// First block a partition may occupy.
    pub fn first_usable_lba(&self) -> u64 {
        self.header.first_usable_lba
    }

    /// Last block a partition may occupy, inclusive.
    pub fn last_usable_lba(&self) -> u64 {
        self.header.last_usable_lba
    }

    /// Where the backup header goes: the last block of the disk.
    pub fn backup_header_lba(&self) -> u64 {
        self.header.alternate_lba
    }

    /// Where the primary entry array goes.
    pub fn primary_entry_lba(&self) -> u64 {
        self.header.entry_array_lba
    }

    /// Where the backup entry array goes: immediately below the backup header.
    pub fn backup_entry_lba(&self) -> u64 {
        self.header.alternate_lba - self.entry_array_blocks()
    }

    /// How many blocks the entry array occupies. 32 for the usual 16 KiB array on a 512-byte disk,
    /// 4 on a 4K-native one.
    pub fn entry_array_blocks(&self) -> u64 {
        (self.entries.len() as u64).div_ceil(self.block_size as u64)
    }

    /// How many entries the array holds, used or not.
    pub fn entry_count(&self) -> u32 {
        self.header.entry_count
    }

    /// The entry array bytes, exactly `entry_count * entry_size` of them: what to write at
    /// [`Gpt::primary_entry_lba`] and again at [`Gpt::backup_entry_lba`]. Both copies are the same
    /// bytes, which is why there is one accessor.
    pub fn entry_array(&self) -> &'a [u8] {
        self.entries
    }

    /// Entry `index`, used or not. `None` past the end of the array.
    pub fn entry(&self, index: usize) -> Option<Entry> {
        let start = index.checked_mul(self.header.entry_size as usize)?;
        let end = start.checked_add(entry::SIZE)?;
        let bytes: &[u8; entry::SIZE] = self.entries.get(start..end)?.try_into().ok()?;
        Some(Entry::decode(bytes))
    }

    /// The used partitions, with their zero-based index in the array.
    ///
    /// The index matters and is not just a counter: it is what `sgdisk` and `parted` print as the
    /// partition *number* (plus one), and what Linux turns into `/dev/sda1`. A table may have a
    /// hole, an unused entry 1 between used entries 0 and 2, and the numbering survives it.
    pub fn partitions(&self) -> impl Iterator<Item = (usize, Entry)> + '_ {
        (0..self.header.entry_count as usize)
            .filter_map(|i| self.entry(i).map(|e| (i, e)))
            .filter(|(_, e)| e.is_used())
    }

    /// Check the block read from LBA 0 is a protective MBR for this disk.
    ///
    /// Separate from [`Gpt::parse`] for the same reason [`Gpt::check_backup`] is: it is a different
    /// block, and a caller that has only read LBA 1 and the array should not be forced to read
    /// another. See [`mbr`] for what "protective" means and why a hybrid MBR is refused.
    pub fn check_protective_mbr(&self, block: &[u8]) -> Result<(), Error> {
        mbr::validate(block, self.block_count())
    }

    /// Write the protective MBR for this disk into `block` (destined for LBA 0).
    pub fn write_protective_mbr(&self, block: &mut [u8]) -> Result<(), Error> {
        mbr::write(block, self.block_count())
    }

    /// Write the primary header into `block` (destined for LBA 1).
    pub fn write_primary_header(&self, block: &mut [u8]) -> Result<(), Error> {
        self.header.encode_into(block)
    }

    /// Write the backup header into `block` (destined for [`Gpt::backup_header_lba`]).
    ///
    /// The same header with the two LBA fields swapped and the array pointer moved. Everything else
    /// is identical, which is what [`Gpt::check_backup`] relies on.
    pub fn write_backup_header(&self, block: &mut [u8]) -> Result<(), Error> {
        Header {
            my_lba: self.header.alternate_lba,
            alternate_lba: self.header.my_lba,
            entry_array_lba: self.backup_entry_lba(),
            ..self.header
        }
        .encode_into(block)
    }
}

/// Trim the caller's buffer to the array the header describes, and check its CRC.
///
/// Shared by [`Gpt::parse`] and [`Gpt::check_backup`] so that the backup's array is held to the
/// same standard as the primary's, which is the whole point of having a backup.
fn check_entry_array<'a>(header: &Header, supplied: &'a [u8]) -> Result<&'a [u8], Error> {
    if header.entry_size < entry::SIZE_U32 || !header.entry_size.is_multiple_of(8) {
        return Err(Error::EntrySize(header.entry_size));
    }
    if header.entry_count == 0 {
        return Err(Error::EntryCount(header.entry_count));
    }
    let need = header
        .entry_array_bytes()
        .ok_or(Error::EntryCount(header.entry_count))?;
    let entries = supplied.get(..need).ok_or(Error::EntryArrayLen {
        need,
        have: supplied.len(),
    })?;

    let computed = crc32(entries);
    if computed != header.entry_array_crc {
        return Err(Error::EntryArrayCrc {
            stored: header.entry_array_crc,
            computed,
        });
    }
    Ok(entries)
}

/// How many blocks the entry array occupies, from a header and a block size.
fn array_blocks(header: &Header, block_size: usize) -> Result<u64, Error> {
    let bytes = header
        .entry_array_bytes()
        .ok_or(Error::EntryCount(header.entry_count))? as u64;
    Ok(bytes.div_ceil(block_size as u64))
}

/// Every rule about where partitions may sit, applied to an entry array.
///
/// The overlap check is the quadratic one and it is deliberately not clever: 128 entries is 8,128
/// comparisons, which is nothing, and a sort would need a scratch buffer this crate does not have.
fn check_partitions(entries: &[u8], entry_size: usize, header: &Header) -> Result<(), Error> {
    let count = entries.len() / entry_size;
    let at = |i: usize| -> Entry {
        let start = i * entry_size;
        Entry::decode(entries[start..start + entry::SIZE].try_into().unwrap())
    };

    for i in 0..count {
        let e = at(i);
        if !e.is_used() {
            continue;
        }
        if e.last_lba < e.first_lba {
            return Err(Error::PartitionRange { index: i });
        }
        if e.first_lba < header.first_usable_lba || e.last_lba > header.last_usable_lba {
            return Err(Error::PartitionOutsideUsable { index: i });
        }
        for j in (i + 1)..count {
            if e.overlaps(&at(j)) {
                return Err(Error::PartitionsOverlap { a: i, b: j });
            }
        }
    }
    Ok(())
}

/// The protective MBR at LBA 0.
///
/// # Why a GPT disk starts with a lie
///
/// The first block of a GPT disk holds a classic MBR partition table describing **one** partition,
/// of type `0xEE`, covering the whole disk. Nothing ever mounts it. It exists so that a tool old
/// enough to not know about GPT sees a disk that is entirely spoken for, and therefore does not
/// helpfully offer to initialise it. The failure mode it defends against is a partition editor from
/// 2001 deciding a 4 TB disk is empty.
///
/// It is genuinely a *protective* device rather than a description, which is why the checks here
/// are about the two fields that do the protecting (start at LBA 1, cover the disk) and not about
/// the CHS fields, which are vestigial: `sgdisk` writes the real cylinder/head/sector of the last
/// block, macOS writes `FE FF FF`, and both are correct because nothing reads them.
pub mod mbr {
    use crate::{Error, MbrProblem, block_size_ok};

    /// The OS type byte that makes a record protective. Not a filesystem, a keep-out sign.
    pub const PROTECTIVE_OS_TYPE: u8 = 0xEE;

    /// Where the four 16-byte partition records start.
    const RECORDS: usize = 446;

    /// Where the `0x55AA` signature sits, in every MBR since 1983.
    const SIGNATURE: usize = 510;

    /// Field offsets inside one 16-byte record.
    const OS_TYPE: usize = 4;
    const START_LBA: usize = 8;
    const SIZE_LBA: usize = 12;

    /// Write a protective MBR for a disk of `block_count` blocks.
    ///
    /// The size field is 32 bits, so on a disk larger than 2 TiB (at 512-byte blocks) it cannot
    /// hold the truth and the convention is `0xFFFFFFFF`. That is the one place this format admits
    /// it is a fiction.
    pub fn write(block: &mut [u8], block_count: u64) -> Result<(), Error> {
        if !block_size_ok(block.len()) {
            return Err(Error::BlockSize(block.len()));
        }
        block.fill(0);
        let r = &mut block[RECORDS..RECORDS + 16];
        r[OS_TYPE] = PROTECTIVE_OS_TYPE;
        // The CHS fields stay at the maximum this crate can express: 0x000200 is "cylinder 0,
        // head 0, sector 2", which is LBA 1, and 0xFFFFFF is the standard "beyond CHS" marker.
        r[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
        r[5..8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
        r[START_LBA..START_LBA + 4].copy_from_slice(&1u32.to_le_bytes());
        r[SIZE_LBA..SIZE_LBA + 4].copy_from_slice(&protective_size(block_count).to_le_bytes());
        block[SIGNATURE] = 0x55;
        block[SIGNATURE + 1] = 0xAA;
        Ok(())
    }

    /// Check the block at LBA 0 is a protective MBR for a disk of `block_count` blocks.
    ///
    /// A **hybrid MBR**, where records 2 to 4 also describe partitions so that a GPT-unaware system
    /// can boot from them, is rejected ([`MbrProblem::ExtraRecord`]). That is a real thing people
    /// do (Boot Camp made a generation of them) and it is also the format's worst failure mode: two
    /// descriptions of one disk, maintained by two different tools, silently diverging. Refusing it
    /// is a decision rather than an omission, and the error names it so the message can say so.
    pub fn validate(block: &[u8], block_count: u64) -> Result<(), Error> {
        if !block_size_ok(block.len()) {
            return Err(Error::BlockSize(block.len()));
        }
        if block[SIGNATURE] != 0x55 || block[SIGNATURE + 1] != 0xAA {
            return Err(Error::Mbr(MbrProblem::Signature));
        }

        // Find the protective record first, then complain about anything beside it. Done in that
        // order because the two failures mean different things and the message has to be the right
        // one: a disk with a real MBR partition table and no 0xEE record is "not a GPT disk", while
        // a 0xEE record with company is a hybrid MBR.
        let os_type = |i: usize| block[RECORDS + i * 16 + OS_TYPE];
        let Some(i) = (0..4).find(|&i| os_type(i) == PROTECTIVE_OS_TYPE) else {
            return Err(Error::Mbr(MbrProblem::NoProtectiveRecord));
        };
        if let Some(extra) = (0..4).find(|&j| j != i && os_type(j) != 0x00) {
            return Err(Error::Mbr(MbrProblem::ExtraRecord { index: extra }));
        }
        let r = &block[RECORDS + i * 16..RECORDS + (i + 1) * 16];

        let start = u32::from_le_bytes(r[START_LBA..START_LBA + 4].try_into().unwrap());
        if start != 1 {
            return Err(Error::Mbr(MbrProblem::StartLba(start)));
        }
        let size = u32::from_le_bytes(r[SIZE_LBA..SIZE_LBA + 4].try_into().unwrap());
        // Either the exact remainder of the disk or the saturated marker. Both are in the wild:
        // sgdisk and macOS both write the exact value on a small disk, and everybody writes
        // 0xFFFFFFFF once the disk is too big to describe.
        if size != protective_size(block_count) && size != u32::MAX {
            return Err(Error::Mbr(MbrProblem::SizeLba(size)));
        }
        Ok(())
    }

    /// Every block but LBA 0, saturated to what 32 bits can hold.
    fn protective_size(block_count: u64) -> u32 {
        u32::try_from(block_count.saturating_sub(1)).unwrap_or(u32::MAX)
    }
}

/// Sample tables, for this crate's own doc examples and for anything downstream that wants a GPT
/// without a disk.
///
/// Not behind `#[cfg(test)]`: rustdoc examples compile against the public API, so a helper only the
/// tests can see is a helper the documentation cannot use.
pub mod testing {
    use crate::guid::types;
    use crate::{ENTRY_ARRAY_BYTES, Entry, Gpt, Guid};

    /// A 64 MiB, 512-byte-block disk carrying one nife data partition, as a contiguous
    /// buffer: LBA 0 is at offset 0, so slicing by `lba * 512` gets any block.
    ///
    /// Only the first 34 blocks are real; the rest is zeros. Enough for the primary table, which is
    /// what a doc example needs.
    pub fn sample_disk() -> [u8; 512 * 34] {
        let mut disk = [0u8; 512 * 34];
        let mut array = [0u8; ENTRY_ARRAY_BYTES];
        let part = Entry::new(
            types::NIFE_DATA,
            Guid::from_bytes([0x11; 16]),
            2048,
            131_038,
        )
        .with_name("nife data")
        .expect("twelve characters fit");
        let table = Gpt::create(
            Guid::from_bytes([0x22; 16]),
            512,
            131_072,
            &[part],
            &mut array,
        )
        .expect("a 64 MiB disk has room for one partition");

        let (mbr, rest) = disk.split_at_mut(512);
        let (header, entries) = rest.split_at_mut(512);
        table.write_protective_mbr(mbr).expect("512 is a block");
        table.write_primary_header(header).expect("512 is a block");
        entries[..ENTRY_ARRAY_BYTES].copy_from_slice(table.entry_array());
        disk
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::BlockSize(n) => write!(
                f,
                "{n} is not a logical block size (512..=4096, a power of two)"
            ),
            Error::Signature => f.write_str("no EFI PART signature: this is not a GPT header"),
            Error::Revision(r) => write!(f, "GPT revision {r:#010x} is not 1.0"),
            Error::HeaderSize(n) => write!(f, "header size {n} is outside 92..=block size"),
            Error::ReservedNotZero => {
                f.write_str("the reserved word at header offset 20 is not zero")
            }
            Error::BlockTailNotZero => {
                f.write_str("the reserved tail of the header block is not zero")
            }
            Error::HeaderCrc { stored, computed } => {
                write!(f, "header CRC {stored:#010x}, computed {computed:#010x}")
            }
            Error::EntrySize(n) => write!(f, "entry size {n} is under 128 or not a multiple of 8"),
            Error::EntryCount(n) => write!(f, "entry count {n} is unusable"),
            Error::EntryArrayLen { need, have } => {
                write!(f, "entry array needs {need} bytes, {have} supplied")
            }
            Error::EntryArrayShape { have } => {
                write!(f, "{have} bytes is not a whole number of 128-byte entries")
            }
            Error::EntryArrayCrc { stored, computed } => {
                write!(
                    f,
                    "entry array CRC {stored:#010x}, computed {computed:#010x}"
                )
            }
            Error::NotPrimary { my_lba } => {
                write!(f, "header says it lives at LBA {my_lba}, not 1")
            }
            Error::UsableRange { first, last } => {
                write!(f, "usable range {first}..={last} is empty")
            }
            Error::TableOverlapsUsable => f.write_str("the table and the usable range overlap"),
            Error::PartitionRange { index } => write!(f, "partition {index} ends before it starts"),
            Error::PartitionOutsideUsable { index } => {
                write!(f, "partition {index} is outside the usable range")
            }
            Error::PartitionsOverlap { a, b } => write!(f, "partitions {a} and {b} overlap"),
            Error::Backup(m) => write!(f, "the backup header disagrees about {m:?}"),
            Error::Mbr(p) => write!(f, "the protective MBR is wrong: {p:?}"),
            Error::DiskTooSmall { blocks, need } => {
                write!(f, "{blocks} blocks is too small for a GPT, {need} needed")
            }
            Error::TooManyPartitions { given, room } => {
                write!(f, "{given} partitions, room for {room}")
            }
            Error::NameTooLong => f.write_str("a partition name does not fit"),
        }
    }
}

// =================================================================================================
// Machine-checked proofs (Kani). Behind `#[cfg(kani)]`, so an ordinary build or test never sees
// them; only `cargo kani` (script/verify) sets that cfg. See notes/verification.md and notes/gpt.md.
//
// The division of labour with the tests is deliberate and is the one `ntp_proto` argued for:
// **where a domain is small enough to count, the tests count it, exhaustively, and that is a
// stronger result than any bounded solver run.** Every single-byte corruption of a real 512-byte
// header block is enumerated in tests/real_disks.rs; every value of every byte of a small table is
// enumerated in tests/table.rs.
//
// What enumeration cannot do is quantify over the *table*. Those tests say "every corruption of
// this table is caught"; the harnesses below say "for every buffer there is, a changed byte changes
// the CRC", which is the general fact the tests are instances of. That, and the round trips over
// 2^1024 entries and 2^128 GUIDs, is what a model checker is for.
// =================================================================================================
#[cfg(kani)]
mod verification {
    use guid::types;

    use super::*;

    /// The buffer length the CRC harnesses quantify over. **Measured, not guessed**; the timings
    /// are in notes/gpt.md. CRC-32 is a shift-and-xor over every bit of the input, so the formula
    /// grows with the byte count and the solver time grows with the formula.
    ///
    /// What the bound buys is not "CRC-32 is correct for short inputs". It is the *structure*: that
    /// the table-driven implementation and the bitwise definition agree, and that a changed byte
    /// always changes the result. Both are properties of the algorithm rather than of the length,
    /// and the lengths that matter in practice (92 bytes of header, 16 KiB of array) are covered
    /// exhaustively by the tests, over bytes real tools wrote.
    const PROVED_BYTES: usize = 8;

    /// **The fast CRC is the CRC.** For every 8-byte input, the 256-entry table and the bitwise
    /// definition give the same answer.
    ///
    /// The table is derived by `const` evaluation from the polynomial, which is a second place a
    /// bug can hide and one no amount of testing against real disks would find: a wrong table that
    /// happened to be self-consistent would round-trip our own output perfectly.
    #[kani::proof]
    #[kani::unwind(9)]
    fn crc32_matches_its_bitwise_definition() {
        let bytes: [u8; PROVED_BYTES] = kani::any();
        assert_eq!(crc::crc32(&bytes), crc::crc32_bitwise(&bytes));
    }

    /// **Changing one byte always changes the CRC.** For every buffer, every position in it, and
    /// every value that byte could change to.
    ///
    /// This is what the whole format's integrity rests on, and the general form of the exhaustive
    /// mutation sweeps in the tests: those enumerate every corruption of *one* table, this
    /// quantifies over the table. It is true for a reason (an error burst shorter than the 32-bit
    /// polynomial is always detected) and the reason is exactly the sort of thing worth checking
    /// rather than citing.
    #[kani::proof]
    #[kani::unwind(9)]
    fn a_single_byte_change_always_changes_the_crc() {
        let mut bytes: [u8; PROVED_BYTES] = kani::any();
        let index: usize = kani::any();
        kani::assume(index < PROVED_BYTES);
        let replacement: u8 = kani::any();
        kani::assume(replacement != bytes[index]);

        let before = crc::crc32(&bytes);
        bytes[index] = replacement;
        assert_ne!(crc::crc32(&bytes), before);
    }

    /// **A partition entry survives the round trip, on every one of the 2^1024 bit patterns.**
    ///
    /// Not "on a well-formed entry": there is no such qualifier, because [`Entry::decode`] judges
    /// nothing. That is what makes the property provable over every input at all, and it is why
    /// every rule about whether an entry makes sense lives in [`Gpt`] instead.
    #[kani::proof]
    #[kani::unwind(129)]
    fn an_entry_survives_the_round_trip() {
        let bytes: [u8; entry::SIZE] = kani::any();
        assert_eq!(Entry::decode(&bytes).encode(), bytes);
    }

    /// **A GUID survives the mixed-endian round trip**, for all 2^128 of them, out through the
    /// printed form and back.
    ///
    /// The swapping is the one place in this crate where a transposition produces something that
    /// looks entirely reasonable and matches nothing.
    #[kani::proof]
    #[kani::unwind(37)]
    fn a_guid_survives_printing_and_parsing() {
        let g = Guid::from_bytes(kani::any());
        assert_eq!(Guid::try_from_ascii(&g.to_ascii()), Some(g));
    }

    /// **The version-4 stamp lands where a reader looks, and touches nothing else**, for all 2^128
    /// inputs.
    ///
    /// Two claims in one, and the second is the one a test cannot make convincingly: the printed
    /// form says version 4 and a `10` variant, *and* the other 122 bits are exactly the bytes the
    /// caller supplied. A stamp that quietly normalised more than the format reserves would be
    /// discarding entropy the caller paid a round trip to a service for, and it would look correct
    /// in every sample anyone thought to write down.
    #[kani::proof]
    #[kani::unwind(37)]
    fn stamping_reserves_six_bits_and_keeps_the_other_hundred_and_twenty_two() {
        let raw: [u8; 16] = kani::any();
        let stamped = Guid::v4_from_random(raw).to_bytes();
        let text = Guid::v4_from_random(raw).to_ascii();
        assert_eq!(text[14], b'4');
        assert!(matches!(text[19], b'8' | b'9' | b'A' | b'B'));
        for i in 0..16 {
            if i != 7 && i != 8 {
                assert_eq!(stamped[i], raw[i]);
            }
        }
        assert_eq!(stamped[7] & 0x0f, raw[7] & 0x0f);
        assert_eq!(stamped[8] & 0x3f, raw[8] & 0x3f);
    }

    /// **A header's fields survive the round trip**, for every value of every one of the nine.
    ///
    /// This is the header's *layout* proved: nine fields at nine offsets in two integer widths, and
    /// not a bit lost. The integrity half is proved by the two CRC harnesses above and, at the
    /// lengths that actually occur, exhaustively by the tests.
    ///
    /// **Why the split, and why the halves compose.** Going through `encode_into`/`decode` instead
    /// puts a 92-byte CRC-32 over symbolic bytes into the formula twice. That was **measured at
    /// over 274 seconds and still unfinished**, against a `script/verify` budget of roughly three
    /// minutes per harness, because a table-driven CRC becomes a 256-way multiplexer per byte once
    /// the state goes symbolic and ninety-two of those is a large formula to prove a fact about
    /// byte offsets. The two halves compose for a reason visible in three lines of `header.rs`
    /// rather than by assumption: everything `encode_into` adds is four bytes at offset 16,
    /// everything `decode` adds is checks, and the CRC field is not one of the nine fields, so
    /// neither half can disturb the other.
    #[kani::proof]
    #[kani::unwind(513)]
    fn a_headers_fields_survive_the_round_trip() {
        let header = Header {
            my_lba: kani::any(),
            alternate_lba: kani::any(),
            first_usable_lba: kani::any(),
            last_usable_lba: kani::any(),
            disk_guid: Guid::from_bytes(kani::any()),
            entry_array_lba: kani::any(),
            entry_count: kani::any(),
            entry_size: kani::any(),
            entry_array_crc: kani::any(),
        };
        let mut block = [0u8; MIN_BLOCK_SIZE];
        header.encode_fields(&mut block);
        assert_eq!(Header::decode_fields(&block), header);
    }

    /// **The other direction: no two header fields share a byte, and none is unclaimed.** For every
    /// possible 512-byte block, decoding the fields and re-encoding them reproduces every byte of
    /// the field region (offsets 24 to 92) exactly.
    ///
    /// The round trip above would still pass if two fields read the same offset and one of them
    /// were never written back; this is the direction that catches it, because a byte no field
    /// claims comes back zero and a byte two fields claim comes back holding one of them. Together
    /// they say the nine fields **partition** the header's field region.
    #[kani::proof]
    #[kani::unwind(513)]
    fn the_header_fields_partition_the_block() {
        let block: [u8; MIN_BLOCK_SIZE] = kani::any();
        let mut round_tripped = [0u8; MIN_BLOCK_SIZE];
        Header::decode_fields(&block).encode_fields(&mut round_tripped);
        assert_eq!(round_tripped[24..92], block[24..92]);
    }

    /// **Two partitions that share a block are always caught**, for every pair of ranges.
    ///
    /// Pure integer comparison, so this one is complete rather than bounded, and it is the property
    /// most easily got wrong by hand: `last_lba` is inclusive, so ranges that merely touch do
    /// overlap. Stated as the *definition* (the intersection is non-empty) rather than as the
    /// implementation, so the proof is not the code compared with itself.
    #[kani::proof]
    fn overlap_is_exactly_sharing_a_block() {
        let a = Entry::new(
            types::LINUX_FILESYSTEM,
            Guid::ZERO,
            kani::any(),
            kani::any(),
        );
        let b = Entry::new(types::NIFE_DATA, Guid::ZERO, kani::any(), kani::any());
        kani::assume(a.first_lba <= a.last_lba && b.first_lba <= b.last_lba);

        let shared = a.first_lba.max(b.first_lba) <= a.last_lba.min(b.last_lba);
        assert_eq!(a.overlaps(&b), shared);
        assert_eq!(a.overlaps(&b), b.overlaps(&a), "overlap is symmetric");

        // An unused entry overlaps nothing, whatever its bounds say.
        let mut ghost = a;
        ghost.type_guid = Guid::ZERO;
        assert!(!ghost.overlaps(&b));
    }

    /// **The writer never lays out a table its own reader would reject**, for every disk size and
    /// every partition placement.
    ///
    /// Every geometry rule [`Gpt::parse`] enforces, asserted on what [`Gpt::create`] produced. A
    /// writer that emitted a table its own reader refuses would be the worst outcome this crate
    /// has, because it would be discovered on somebody's disk rather than in this file, and the
    /// dimension where that could hide is the one enumerated tests cover worst: an unusual disk
    /// size, where the arithmetic that places the backup table runs out of room.
    ///
    /// Stated over the *geometry* rather than over the bytes, deliberately. A byte-level
    /// create-then-parse harness has four symbolic CRC-32 chains in it and did not finish inside
    /// the budget (see `a_headers_fields_survive_the_round_trip` for the measurement). The
    /// byte-level identity is proved instead where it can be proved *completely*: `Gpt::create`
    /// reproduces `sgdisk`'s own 512-byte header, backup header and 16 KiB array exactly, in
    /// `tests/real_disks.rs`. This harness supplies what that test cannot, which is every other
    /// disk size.
    ///
    /// `create` is allowed to fail and the harness then returns; what is proved is that whenever it
    /// succeeds, the result satisfies the reader's rules.
    #[kani::proof]
    fn create_never_lays_out_a_table_parse_would_reject() {
        let part = Entry::new(
            types::NIFE_DATA,
            Guid::from_bytes([7; 16]),
            kani::any(),
            kani::any(),
        );
        let blocks: u64 = kani::any();

        let mut array = [0u8; entry::SIZE];
        let Ok(table) = Gpt::create(
            Guid::from_bytes([3; 16]),
            MIN_BLOCK_SIZE,
            blocks,
            &[part],
            &mut array,
        ) else {
            return;
        };

        let h = table.header();
        let span = table.entry_array_blocks();
        assert_eq!(table.block_count(), blocks);
        assert_eq!(h.my_lba, PRIMARY_HEADER_LBA);

        // The four inequalities Gpt::parse checks, in the same order.
        assert!(h.first_usable_lba <= h.last_usable_lba);
        assert!(h.entry_array_lba > PRIMARY_HEADER_LBA);
        assert!(h.entry_array_lba + span <= h.first_usable_lba);
        assert!(h.last_usable_lba + span < h.alternate_lba);

        // And the partition it was given is inside the usable range it chose, or create refused.
        assert!(part.first_lba >= h.first_usable_lba);
        assert!(part.last_lba <= h.last_usable_lba);
        assert!(part.first_lba <= part.last_lba);

        // The backup table lands exactly below the backup header, which is what check_backup
        // compares the backup's own entry_array_lba against.
        assert_eq!(table.backup_entry_lba() + span, table.backup_header_lba());
    }
}
