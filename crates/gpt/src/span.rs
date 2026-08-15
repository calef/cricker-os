//! **Where to read**: a byte range of a disk, restated in the transfer unit a block device
//! actually moves.
//!
//! This is still no I/O (nothing here touches a device); it is the arithmetic between two block
//! sizes, and it exists because on nife those two sizes are never the same number.
//!
//! A GPT is written in *logical* blocks, almost always 512 bytes: the primary header is at LBA 1,
//! the entry array at LBA 2, the backup header at the last LBA. The block service a program holds
//! moves one **filesystem block** per request, 4096 bytes (`fs_proto::blk::BLOCK_SIZE`), because
//! that is what RedoxFS reads and what makes a mount's device round trips affordable. So "read LBA
//! 1" is really "read transfer block 0 and take bytes 512..1024 of it", and "read the 33 blocks of
//! the backup table" straddles a boundary that depends on the disk's size.
//!
//! Getting that wrong is an off-by-one that reads a plausible-looking buffer full of the wrong
//! bytes, and the CRC then says the table is broken when the table is fine. So it is one function
//! with its own tests rather than three open-coded divisions in a driver.
//!
//! ```
//! # use gpt::span::Span;
//! // The primary header: LBA 1 of a 512-byte-block disk, fetched 4096 bytes at a time.
//! let s = Span::covering(512, 512, 4096).unwrap();
//! assert_eq!((s.first_block, s.blocks, s.offset), (0, 1, 512));
//!
//! // The whole 16 KiB entry array, starting at LBA 2: five transfer blocks, and the bytes we
//! // want begin 1024 into the first of them.
//! let s = Span::covering(2 * 512, 16384, 4096).unwrap();
//! assert_eq!((s.first_block, s.blocks, s.offset), (0, 5, 1024));
//! ```

/// A run of consecutive transfer blocks that contains a wanted byte range, plus where the range
/// starts inside it.
///
/// The contract, and the only two things a caller needs: read [`Span::blocks`] blocks starting at
/// [`Span::first_block`] into one buffer, and the bytes asked for are at [`Span::offset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// The first transfer block to read.
    pub first_block: u64,
    /// How many consecutive transfer blocks to read.
    pub blocks: u64,
    /// Where the wanted range begins inside the buffer those blocks fill.
    pub offset: usize,
}

impl Span {
    /// The blocks covering `len` bytes at `byte_offset`, when the device moves `transfer` bytes at
    /// a time.
    ///
    /// `None` for a `transfer` of zero, a `len` of zero (nothing to read is not a read), or any
    /// arithmetic that would leave a `u64`. Refusing rather than saturating is the same rule
    /// [`crate::Header::entry_array_bytes`] follows: a saturated length means reading a shorter
    /// buffer than was asked for and then believing what is in it.
    pub const fn covering(byte_offset: u64, len: u64, transfer: u64) -> Option<Span> {
        if transfer == 0 || len == 0 {
            return None;
        }
        let first_block = byte_offset / transfer;
        let offset = byte_offset % transfer;
        // The last byte wanted, relative to the start of `first_block`.
        let Some(last) = offset.checked_add(len - 1) else {
            return None;
        };
        let blocks = last / transfer + 1;
        // The read must still name a block: `first_block + blocks` is the one past the end.
        if first_block.checked_add(blocks).is_none() {
            return None;
        }
        Some(Span {
            first_block,
            blocks,
            offset: offset as usize,
        })
    }

    /// How many bytes the buffer must hold: `blocks * transfer`. `None` on overflow, and on a
    /// 32-bit host when the answer does not fit a `usize`.
    pub const fn buffer_bytes(&self, transfer: u64) -> Option<usize> {
        match self.blocks.checked_mul(transfer) {
            Some(n) if n <= usize::MAX as u64 => Some(n as usize),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three reads a GPT reader actually makes on the disk this project tests against: a 64 MiB
    /// disk, 512-byte logical blocks, fetched in 4096-byte filesystem blocks.
    #[test]
    fn the_three_reads_a_gpt_reader_makes() {
        const T: u64 = 4096;
        const BLOCKS: u64 = 131_072; // 64 MiB of 512-byte blocks

        // The protective MBR and the primary header both live in transfer block 0.
        assert_eq!(
            Span::covering(0, 512, T).unwrap(),
            Span {
                first_block: 0,
                blocks: 1,
                offset: 0
            },
        );
        assert_eq!(
            Span::covering(512, 512, T).unwrap(),
            Span {
                first_block: 0,
                blocks: 1,
                offset: 512
            },
        );

        // The primary entry array: LBA 2, 16 KiB. It starts 1024 bytes into block 0 and ends
        // 17408 bytes in, so five blocks (20480 bytes) cover it.
        let array = Span::covering(2 * 512, 16384, T).unwrap();
        assert_eq!(
            array,
            Span {
                first_block: 0,
                blocks: 5,
                offset: 1024
            },
        );
        assert_eq!(array.buffer_bytes(T), Some(20480));

        // The backup half, which is the one worth having a function for: 33 logical blocks
        // (the array plus the header) ending on the last block of the disk. It does not begin on
        // a transfer boundary and nothing about the number 33 makes that obvious.
        let backup = Span::covering((BLOCKS - 33) * 512, 33 * 512, T).unwrap();
        assert_eq!(backup.first_block, (BLOCKS - 33) * 512 / T);
        assert_eq!(backup.offset, ((BLOCKS - 33) * 512 % T) as usize);
        // The last byte of the disk is inside the run.
        let end = (backup.first_block + backup.blocks) * T;
        assert!(end >= BLOCKS * 512, "{end} must reach the end of the disk");
    }

    /// The property that matters, stated as a property: the run starts at or before the range and
    /// ends at or after it, and `offset` lands on the first wanted byte.
    #[test]
    fn a_span_always_contains_what_was_asked_for() {
        for transfer in [512u64, 1024, 4096] {
            for byte_offset in [0u64, 1, 511, 512, 4095, 4096, 4097, 1 << 20, (1 << 20) + 7] {
                for len in [1u64, 2, 511, 512, 513, 4096, 16384, 16896] {
                    let s = Span::covering(byte_offset, len, transfer).unwrap();
                    let start = s.first_block * transfer;
                    let end = start + s.blocks * transfer;
                    assert!(start <= byte_offset, "{s:?} starts after the range");
                    assert!(end >= byte_offset + len, "{s:?} ends before the range");
                    assert_eq!(start + s.offset as u64, byte_offset, "{s:?} mislocates it");
                    // And it is not wasteful: dropping the last block would lose bytes.
                    assert!(
                        end - transfer < byte_offset + len,
                        "{s:?} reads a spare block"
                    );
                }
            }
        }
    }

    #[test]
    fn the_refusals() {
        assert_eq!(Span::covering(0, 512, 0), None, "a zero transfer unit");
        assert_eq!(Span::covering(0, 0, 4096), None, "a zero-length read");
        assert_eq!(
            Span::covering(u64::MAX, u64::MAX, 4096),
            None,
            "a range that leaves the disk's address space",
        );
        // A read of the very last byte a u64 can name is still fine.
        assert!(Span::covering(u64::MAX, 1, 4096).is_some());
    }
}
