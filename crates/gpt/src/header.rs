//! The GPT header: 92 bytes at LBA 1, and the same 92 bytes again at the last LBA.
//!
//! Everything about where the table lives is in here, which is why there are two copies. Lose the
//! header and the entry array is 16 KiB of unlabelled structs.
//!
//! # The layering this module commits to
//!
//! [`Header::decode`] answers **"are these bytes a GPT header?"** and nothing else: signature,
//! revision, header size, the reserved word, the CRC over the header's own bytes, and the reserved
//! tail of the block. It has no idea how big the disk is, where the partitions are, or whether it
//! is looking at the primary or the backup, so it cannot judge any of that.
//!
//! Every judgement that needs the *disk* is in [`crate::Gpt`]. The split matters because the backup
//! header has to be decodable on its own terms before anything can compare it against the primary,
//! and because it puts every geometry rule in one function instead of scattering them.

use crate::crc::crc32_pieces;
use crate::guid::Guid;
use crate::{Error, block_size_ok};

/// `EFI PART`, the eight bytes at offset 0 of every GPT header.
pub const SIGNATURE: [u8; 8] = *b"EFI PART";

/// GPT revision 1.0, encoded as `0x00010000` (major in the high 16 bits). The only revision that
/// has ever existed, and the only one this crate accepts: a revision we do not know is a header
/// whose fields might not be where we think they are.
pub const REVISION: u32 = 0x0001_0000;

/// The size of the defined part of the header, in bytes, and the number of bytes the header CRC
/// covers. A header may declare a *larger* size (the spec reserves the right to grow it), and this
/// crate rejects that rather than guessing at fields it does not know: see [`Header::decode`].
pub const HEADER_SIZE: u32 = 92;

/// Field offsets within the header, in the order UEFI 2.10 Table 5.5 lists them. Written out
/// because a run of `u64::from_le_bytes` calls with literal indices is unreadable and one
/// transposed digit is a silent bug.
mod at {
    pub const SIGNATURE: usize = 0;
    pub const REVISION: usize = 8;
    pub const HEADER_SIZE: usize = 12;
    pub const HEADER_CRC: usize = 16;
    pub const RESERVED: usize = 20;
    pub const MY_LBA: usize = 24;
    pub const ALTERNATE_LBA: usize = 32;
    pub const FIRST_USABLE_LBA: usize = 40;
    pub const LAST_USABLE_LBA: usize = 48;
    pub const DISK_GUID: usize = 56;
    pub const ENTRY_ARRAY_LBA: usize = 72;
    pub const ENTRY_COUNT: usize = 80;
    pub const ENTRY_SIZE: usize = 84;
    pub const ENTRY_ARRAY_CRC: usize = 88;
}

/// A decoded GPT header.
///
/// Field for field with the on-disk structure, minus the four that are not information: the
/// signature and revision are constants once [`Header::decode`] has accepted, the reserved word is
/// zero, and the header CRC is recomputed on write rather than stored (a struct that carried a
/// stale CRC would be a footgun with no upside).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// The LBA this header was read from, according to the header. 1 for a primary, the last block
    /// of the disk for a backup. Checked against where it actually came from by [`crate::Gpt`],
    /// which is what makes a header that was copied to the wrong place detectable.
    pub my_lba: u64,
    /// Where the *other* copy of this header lives.
    pub alternate_lba: u64,
    /// The first block a partition may occupy. Everything below it is table.
    pub first_usable_lba: u64,
    /// The last block a partition may occupy, inclusive. Everything above it is backup table.
    pub last_usable_lba: u64,
    /// Names the disk, not any partition on it.
    pub disk_guid: Guid,
    /// Where this header's copy of the partition entry array starts.
    pub entry_array_lba: u64,
    /// How many entries the array has room for, used or not. 128 in practice, always.
    pub entry_count: u32,
    /// Bytes per entry. 128 in practice; the spec allows any multiple of 8 that is at least 128, so
    /// that entries can grow.
    pub entry_size: u32,
    /// CRC-32 over `entry_count * entry_size` bytes of the entry array.
    pub entry_array_crc: u32,
}

impl Header {
    /// Decode one block into a header, checking everything that can be checked without knowing what
    /// disk it came from.
    ///
    /// `block` must be exactly one block (512 to 4096 bytes, a power of two); its length **is** the
    /// block size, which is why no separate parameter is needed. Rejects:
    ///
    /// - a bad block size, a wrong signature, or a revision other than [`REVISION`];
    /// - a declared header size outside `92 ..= block_size`, which is the spec's own bound. A
    ///   header that declares *more* than 92 is accepted and the extra bytes are covered by the CRC
    ///   and otherwise ignored, which is what the spec asks of a reader that does not know a later
    ///   revision's fields. In practice nothing writes anything but 92, and a revision that grew
    ///   the header would have to change [`REVISION`] too, which this crate refuses outright;
    /// - a non-zero reserved word at offset 20, or a non-zero byte anywhere after the header in the
    ///   block. UEFI 2.10 §5.3.2 requires both to be zero. This is the strictest check in the crate
    ///   and the one most likely to need relaxing if a real disk ever trips it; see notes/gpt.md;
    /// - a header CRC that does not match. Computed the way the spec defines it, over `header_size`
    ///   bytes with the CRC field itself taken as zero.
    pub fn decode(block: &[u8]) -> Result<Header, Error> {
        if !block_size_ok(block.len()) {
            return Err(Error::BlockSize(block.len()));
        }
        if block[at::SIGNATURE..at::SIGNATURE + 8] != SIGNATURE {
            return Err(Error::Signature);
        }
        let revision = le32(block, at::REVISION);
        if revision != REVISION {
            return Err(Error::Revision(revision));
        }
        let header_size = le32(block, at::HEADER_SIZE);
        if header_size < HEADER_SIZE || header_size as usize > block.len() {
            return Err(Error::HeaderSize(header_size));
        }
        if le32(block, at::RESERVED) != 0 {
            return Err(Error::ReservedNotZero);
        }
        if block[header_size as usize..].iter().any(|&b| b != 0) {
            return Err(Error::BlockTailNotZero);
        }

        let stored = le32(block, at::HEADER_CRC);
        let computed = header_crc(&block[..header_size as usize]);
        if stored != computed {
            return Err(Error::HeaderCrc { stored, computed });
        }

        Ok(Header::decode_fields(block))
    }

    /// The field half of [`Header::decode`]: read the nine fields out of a block, judging nothing.
    ///
    /// Total on any slice of at least [`HEADER_SIZE`] bytes, and the exact inverse of
    /// [`Header::encode_fields`]. Split out from `decode` so the **layout** can be proved
    /// separately from the **integrity**, which is what keeps the layout proof affordable: a
    /// harness that went through the CRC would have to reason about a 92-byte polynomial over
    /// symbolic bytes, which costs minutes of solver time to prove a fact about byte offsets. See
    /// `verification::a_headers_fields_survive_the_round_trip` and notes/gpt.md.
    pub(crate) fn decode_fields(block: &[u8]) -> Header {
        Header {
            my_lba: le64(block, at::MY_LBA),
            alternate_lba: le64(block, at::ALTERNATE_LBA),
            first_usable_lba: le64(block, at::FIRST_USABLE_LBA),
            last_usable_lba: le64(block, at::LAST_USABLE_LBA),
            disk_guid: Guid::from_bytes(
                block[at::DISK_GUID..at::DISK_GUID + 16].try_into().unwrap(),
            ),
            entry_array_lba: le64(block, at::ENTRY_ARRAY_LBA),
            entry_count: le32(block, at::ENTRY_COUNT),
            entry_size: le32(block, at::ENTRY_SIZE),
            entry_array_crc: le32(block, at::ENTRY_ARRAY_CRC),
        }
    }

    /// Write this header into one block, zeroing the block first and computing the header CRC last.
    ///
    /// Always emits a 92-byte header, so `decode(encode(h)) == h` for every header this crate can
    /// hold. The block is zeroed rather than overwritten in place because the reserved tail must be
    /// zero and a caller handing us a buffer with a previous header in it is the normal case.
    pub fn encode_into(&self, block: &mut [u8]) -> Result<(), Error> {
        if !block_size_ok(block.len()) {
            return Err(Error::BlockSize(block.len()));
        }
        self.encode_fields(block);
        let crc = header_crc(&block[..HEADER_SIZE as usize]);
        put32(block, at::HEADER_CRC, crc);
        Ok(())
    }

    /// The field half of [`Header::encode_into`]: everything except the header's own CRC, which is
    /// left as the four zero bytes the CRC is defined to be computed over.
    ///
    /// The split is what makes the round trip provable in two independent pieces, and the two
    /// pieces compose for a reason that is three lines of source rather than an assumption: the
    /// only thing `encode_into` adds is four bytes at offset 16, the only thing `decode` adds is
    /// checks, and **the CRC field is not one of the nine fields**, so the two halves cannot
    /// interfere with each other.
    pub(crate) fn encode_fields(&self, block: &mut [u8]) {
        block.fill(0);
        block[at::SIGNATURE..at::SIGNATURE + 8].copy_from_slice(&SIGNATURE);
        put32(block, at::REVISION, REVISION);
        put32(block, at::HEADER_SIZE, HEADER_SIZE);
        // at::HEADER_CRC and at::RESERVED stay zero: the first is filled in by the caller after
        // the CRC is computed, the second is reserved and must stay zero on disk.
        put64(block, at::MY_LBA, self.my_lba);
        put64(block, at::ALTERNATE_LBA, self.alternate_lba);
        put64(block, at::FIRST_USABLE_LBA, self.first_usable_lba);
        put64(block, at::LAST_USABLE_LBA, self.last_usable_lba);
        block[at::DISK_GUID..at::DISK_GUID + 16].copy_from_slice(&self.disk_guid.to_bytes());
        put64(block, at::ENTRY_ARRAY_LBA, self.entry_array_lba);
        put32(block, at::ENTRY_COUNT, self.entry_count);
        put32(block, at::ENTRY_SIZE, self.entry_size);
        put32(block, at::ENTRY_ARRAY_CRC, self.entry_array_crc);
    }

    /// The number of bytes of entry array this header describes: `entry_count * entry_size`, the
    /// exact span the array CRC covers.
    ///
    /// `None` on overflow, which a hostile header can cause with two `u32`s (`0xFFFFFFFF` each
    /// multiplies out past `usize` on a 32-bit target). Returning an `Option` rather than
    /// saturating keeps the caller from silently CRC-ing a shorter buffer than the header claimed.
    pub fn entry_array_bytes(&self) -> Option<usize> {
        usize::try_from(self.entry_count as u64 * self.entry_size as u64).ok()
    }
}

/// The header CRC, computed the way the spec defines it: over `header_size` bytes, with the four
/// bytes of the CRC field itself treated as zero.
///
/// The self-referential field is the fiddly part of every CRC-protected header format. The obvious
/// way is to copy the header, zero four bytes and CRC the copy, which puts a block-sized buffer on
/// a kernel stack to change four bytes. Instead the CRC is taken in three pieces, which is the same
/// computation and no buffer.
fn header_crc(header: &[u8]) -> u32 {
    crc32_pieces(&[
        &header[..at::HEADER_CRC],
        &[0, 0, 0, 0],
        &header[at::HEADER_CRC + 4..],
    ])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}
