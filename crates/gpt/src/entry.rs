//! A partition entry: 128 bytes saying what a partition is, where it starts, and where it stops.
//!
//! # Decoding an entry cannot fail
//!
//! [`Entry::decode`] is **total**: every one of the 2^1024 bit patterns is a valid `Entry` value,
//! and the round trip back through [`Entry::encode`] returns the bytes it started with. That is a
//! deliberate layering, the same one `ntp_proto` makes: decoding judges nothing, so it needs no
//! error type, and every rule about whether an entry makes *sense* on a particular disk lives in
//! [`crate::Gpt`] where the disk's geometry is known.
//!
//! # The name is UTF-16, and the name is optional
//!
//! 36 UTF-16LE code units, null-padded, and it is a label for humans. Nothing in the format depends
//! on it and nothing should: partitions are found by type GUID or unique GUID.
//!
//! Worth knowing before writing code that relies on names: **macOS does not write them.** Partition
//! a disk image with `diskutil partitionDisk ... HFS+ MACDATA`, give the volume a name, and the GPT
//! entry's 72 name bytes are still all zero, because on a Mac the volume name lives in the
//! filesystem. The committed Apple fixture pins that behaviour. `sgdisk`, `parted` and Linux tools
//! do write it. So a partition browser that keys off the GPT name shows blanks on half the disks in
//! the world, and that is not a bug in the browser.

use crate::Error;
use crate::guid::Guid;

/// Bytes per partition entry, and the only size anything writes. The spec permits larger (any
/// multiple of 8 that is at least 128) so entries can grow; this crate reads a larger entry by
/// decoding its first 128 bytes and ignoring the rest, which is what a reader that does not know a
/// later revision can honestly do.
pub const SIZE: usize = 128;

/// UTF-16 code units in the partition name field. 72 bytes.
pub const NAME_UNITS: usize = 36;

/// Attribute bit 0: the platform must not alter this partition. Firmware sets it on partitions it
/// needs (an OEM recovery partition, for instance), and a partition editor is expected to refuse to
/// touch one.
pub const ATTR_REQUIRED: u64 = 1 << 0;

/// Attribute bit 1: no block-IO protocol. UEFI firmware will not expose this partition as a
/// filesystem.
pub const ATTR_NO_BLOCK_IO: u64 = 1 << 1;

/// Attribute bit 2: legacy BIOS bootable. What `sgdisk -A n:set:2` sets, and what a BIOS-emulating
/// bootloader looks for in place of the MBR active flag.
pub const ATTR_LEGACY_BIOS_BOOTABLE: u64 = 1 << 2;

mod at {
    pub const TYPE_GUID: usize = 0;
    pub const UNIQUE_GUID: usize = 16;
    pub const FIRST_LBA: usize = 32;
    pub const LAST_LBA: usize = 40;
    pub const ATTRIBUTES: usize = 48;
    pub const NAME: usize = 56;
}

/// One partition entry, decoded.
///
/// Also the type used to *describe* a partition to [`crate::Gpt::create`]: what you want written
/// and what you read back are the same thing, so there is no second "spec" struct to keep in sync.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    /// What the partition is for. All zeros means the entry is unused; see
    /// [`crate::guid::types`] for the ones this crate names.
    pub type_guid: Guid,
    /// Names this partition, uniquely, forever. Generated once when the partition is created.
    pub unique_guid: Guid,
    /// First block of the partition.
    pub first_lba: u64,
    /// Last block of the partition, **inclusive**. A one-block partition has
    /// `first_lba == last_lba`, and the off-by-one here is the classic way to lose a sector.
    pub last_lba: u64,
    /// The attribute bits. See `ATTR_*`; bits 48 to 63 are reserved for the partition type's own
    /// use and mean whatever that type says they mean.
    pub attributes: u64,
    /// The label, as UTF-16 code units, null-padded. Use [`Entry::name_utf8`] to read it and
    /// [`Entry::with_name`] to set it rather than touching this directly.
    pub name: [u16; NAME_UNITS],
}

impl Entry {
    /// An unused entry: all zeros, which is what an empty slot in the array looks like on disk.
    pub const UNUSED: Entry = Entry {
        type_guid: Guid::ZERO,
        unique_guid: Guid::ZERO,
        first_lba: 0,
        last_lba: 0,
        attributes: 0,
        name: [0; NAME_UNITS],
    };

    /// A used entry covering `first_lba ..= last_lba`, with no attributes and no name.
    ///
    /// Both GUIDs are the caller's: this crate has no randomness and will not invent a unique GUID
    /// for you. That is the honest boundary (a GUID that is not random is not unique), and milestone
    /// 55's entropy work is where the caller gets one.
    pub const fn new(type_guid: Guid, unique_guid: Guid, first_lba: u64, last_lba: u64) -> Entry {
        Entry {
            type_guid,
            unique_guid,
            first_lba,
            last_lba,
            attributes: 0,
            name: [0; NAME_UNITS],
        }
    }

    /// The same entry with a label. `Err(`[`Error::NameTooLong`]`)` if the string does not fit in
    /// [`NAME_UNITS`] UTF-16 code units, which is 36 for text in the Basic Multilingual Plane and
    /// fewer once emoji get involved (each is a surrogate pair, so two units).
    pub fn with_name(mut self, name: &str) -> Result<Entry, Error> {
        self.name = [0; NAME_UNITS];
        for (n, unit) in name.encode_utf16().enumerate() {
            if n == NAME_UNITS {
                return Err(Error::NameTooLong);
            }
            self.name[n] = unit;
        }
        Ok(self)
    }

    /// True when the type GUID is not all zeros, which is the *only* thing that marks an entry
    /// live. An entry with LBAs set and a zero type is unused, and a partition editor that reads
    /// the LBAs first will hallucinate partitions on a freshly wiped disk.
    pub const fn is_used(&self) -> bool {
        !self.type_guid.is_zero()
    }

    /// How many blocks the partition covers. `last_lba` is inclusive, hence the `+ 1`.
    ///
    /// `None` when `last_lba < first_lba`, which is a malformed entry rather than an empty
    /// partition. [`crate::Gpt`] rejects those, so a partition obtained from a parsed table always
    /// has a size.
    pub const fn blocks(&self) -> Option<u64> {
        if self.last_lba < self.first_lba {
            None
        } else {
            Some(self.last_lba - self.first_lba + 1)
        }
    }

    /// The label as UTF-8, written into `out`, returning how many bytes were written.
    ///
    /// `Err(`[`Error::NameTooLong`]`)` if `out` is too small; 4 bytes per code unit
    /// (`4 * NAME_UNITS` = 144) is always enough. An unpaired surrogate, which a hostile or
    /// corrupt entry can carry, becomes U+FFFD rather than an error: the name is a label, and
    /// refusing to list a partition because its cosmetic field is malformed would be the wrong
    /// trade.
    pub fn name_utf8(&self, out: &mut [u8]) -> Result<usize, Error> {
        let units = self.name.iter().position(|&u| u == 0).unwrap_or(NAME_UNITS);
        let mut written = 0;
        for ch in char::decode_utf16(self.name[..units].iter().copied()) {
            let ch = ch.unwrap_or(char::REPLACEMENT_CHARACTER);
            let len = ch.len_utf8();
            if written + len > out.len() {
                return Err(Error::NameTooLong);
            }
            ch.encode_utf8(&mut out[written..]);
            written += len;
        }
        Ok(written)
    }

    /// Decode 128 bytes. Total: there is no bit pattern this rejects, and none it loses.
    pub fn decode(bytes: &[u8; SIZE]) -> Entry {
        let mut name = [0u16; NAME_UNITS];
        for (i, unit) in name.iter_mut().enumerate() {
            let o = at::NAME + i * 2;
            *unit = u16::from_le_bytes([bytes[o], bytes[o + 1]]);
        }
        Entry {
            type_guid: Guid::from_bytes(
                bytes[at::TYPE_GUID..at::TYPE_GUID + 16].try_into().unwrap(),
            ),
            unique_guid: Guid::from_bytes(
                bytes[at::UNIQUE_GUID..at::UNIQUE_GUID + 16]
                    .try_into()
                    .unwrap(),
            ),
            first_lba: le64(bytes, at::FIRST_LBA),
            last_lba: le64(bytes, at::LAST_LBA),
            attributes: le64(bytes, at::ATTRIBUTES),
            name,
        }
    }

    /// The 128 bytes this entry is on disk. Inverse of [`Entry::decode`], proved.
    pub fn encode(&self) -> [u8; SIZE] {
        let mut bytes = [0u8; SIZE];
        bytes[at::TYPE_GUID..at::TYPE_GUID + 16].copy_from_slice(&self.type_guid.to_bytes());
        bytes[at::UNIQUE_GUID..at::UNIQUE_GUID + 16].copy_from_slice(&self.unique_guid.to_bytes());
        bytes[at::FIRST_LBA..at::FIRST_LBA + 8].copy_from_slice(&self.first_lba.to_le_bytes());
        bytes[at::LAST_LBA..at::LAST_LBA + 8].copy_from_slice(&self.last_lba.to_le_bytes());
        bytes[at::ATTRIBUTES..at::ATTRIBUTES + 8].copy_from_slice(&self.attributes.to_le_bytes());
        for (i, unit) in self.name.iter().enumerate() {
            let o = at::NAME + i * 2;
            bytes[o..o + 2].copy_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    /// True when this entry and `other` claim a block in common. Both must be used; an unused entry
    /// overlaps nothing, whatever its LBA fields say.
    ///
    /// Half-open comparison on inclusive ranges, which is where this goes wrong if written in a
    /// hurry: two partitions where one ends exactly where the next begins **do** overlap, because
    /// `last_lba` is the last block *in* the partition.
    pub const fn overlaps(&self, other: &Entry) -> bool {
        self.is_used()
            && other.is_used()
            && self.first_lba <= other.last_lba
            && other.first_lba <= self.last_lba
    }
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

/// [`SIZE`] as the `u32` the header field wants, so the cast appears once.
pub(crate) const SIZE_U32: u32 = SIZE as u32;
