//! **nifefs**: a filesystem so simple it fits in a comment.
//!
//! Read-only, flat (no directories), fixed-size everything. It exists to be *parsed*, not to be
//! good, in the same spirit as `crates/elf`: the point of milestone 9 is drivers and block I/O,
//! not filesystem design, so the on-disk format is the least thing that is still a real
//! filesystem.
//!
//! Pure logic, host-tested, no `std`. The kernel's disk tool writes an image with [`write_image`]
//! and the userspace filesystem server reads it with [`Fs::parse`], so **one definition of the
//! format serves both**, and a test writes an image and reads it back.
//!
//! # The layout
//!
//! ```text
//!   blocks 0..DIR_BLOCKS   the superblock and directory
//!     magic   "CRKR0002"   (8 bytes)
//!     count   u32 LE       how many files          <- HEADER_LEN = 12 bytes so far
//!     ...then `count` directory entries, each ENTRY_LEN = 40 bytes:
//!       name        NAME_LEN = 32 bytes, NUL-padded
//!       start_block u32 LE  where the file's data begins (an absolute block number)
//!       len         u32 LE  the file's length in bytes
//!
//!   blocks DIR_BLOCKS..     file data, each file block-aligned
//! ```
//!
//! The directory is [`DIR_BLOCKS`] blocks, holding the magic, the count, and up to [`MAX_FILES`]
//! entries. It grew from one block to two under DECISIONS §24 (interrupting the foreground
//! process), when the interactive system plus its demo programs passed fifteen files; to four on
//! 2026-07-30 (three lanes landing together made 32); and to six on 2026-08-01 when the entries
//! got wider. `start_block` is an absolute block number, so a
//! reader never needs to know `DIR_BLOCKS` to find data; only the writer places it.
//!
//! # Names
//!
//! A name is at most [`NAME_LEN`] bytes and is **not** NUL-terminated when it uses all of them, so a
//! reader compares against `NAME_LEN` bytes and stops at the first NUL if there is one.
//! [`write_image`] refuses a longer name rather than truncating it; see BUGS.
//!
//! # EXAMPLES
//!
//! Pack two files and read one back:
//!
//! ```
//! # use nifefs::{Fs, image_size, write_image};
//! let files: [(&str, &[u8]); 2] = [("motd", b"welcome\n"), ("os_primitives_benchmarker", b"\x7fELF")];
//! let mut img = vec![0u8; image_size(&files)];
//! write_image(&files, &mut img).unwrap();
//!
//! let fs = Fs::parse(&img).unwrap();
//! assert_eq!(fs.read("motd"), Some(&b"welcome\n"[..]));
//! assert_eq!(fs.len(), 2);
//! ```
//!
//! # BUGS
//!
//! - **A name longer than [`NAME_LEN`] is an error, not a truncation.** It used to be silently
//!   truncated, which is the worse failure: two long names that agree in their first `NAME_LEN`
//!   bytes become one entry, and `init` loads whichever program happened to be packed first. The
//!   build now stops with [`Error::NameTooLong`].
//! - **A name containing a NUL is an error too**, and for the same reason: the padding is NUL and
//!   every reader stops at the first one, so `"a\0b"` would be written and read back as `"a"`.
//!   [`Error::NameHasNul`]. This one survived the truncation fix because nobody thought to write a
//!   name with a NUL in it; `fuzz/fuzz_targets/nifefs_roundtrip` did, in under a minute.
//! - **A duplicate name is NOT an error, and the first one wins.** [`Fs::read`] returns the first
//!   entry whose name matches, so packing two files under one name silently hides the second. The
//!   disk tool builds its file list from a directory listing, where names are unique by
//!   construction, which is why this has never bitten; nothing in the format prevents it.
//! - **[`MAX_FILES`] is a ceiling that grows with the SUITE, not with the system**, so the cost is
//!   invisible to each branch that causes it and lands on whoever merges. Three lanes landing
//!   together on 2026-07-30 pushed the archive from 31 files to 32 and forced a directory resize.
//! - **A reader holding one block cannot see the whole directory.** The EL0 blk driver
//!   (`crates/virtio`) buffers block 0 only, so it can find the first
//!   [`ENTRIES_IN_FIRST_BLOCK`] files and no more. It is used on the tiny test disk, not the
//!   initrd, but the limit is real and nothing in the format announces it.
//! - **No directories, no writes, no permissions.** This is a boot archive. The read-write
//!   filesystem is the RedoxFS server in `fs_server/` (DECISIONS §34), which is a different job.
//!
//! Name: ratified 2026-08-01 as `crickerfs` (calef, milestone 63): the one run-together name kept
//! when that rule was deleted. Refused `cricker_fs` (`procfs` is the shape of a filesystem name
//! outside this project, and nobody writes `proc_fs`). Renamed `nifefs` with the OS (calef,
//! milestone 120, 2026-08-15); the run-together reasoning carried over unchanged.

#![no_std]

/// The block size, and the alignment of everything.
pub const BLOCK: usize = 512;

/// Superblock magic. Version in the last four bytes, so a format change is legible.
///
/// **Bumped `0001` to `0002` on 2026-08-01, when [`NAME_LEN`] went from 24 to 32.** The rule the
/// milestone-24 change established is the one being followed here, not overturned: bump when a
/// reader can tell. That change moved `DIR_BLOCKS` only, and `start_block` is absolute, so no
/// reader could tell and bumping would have broken the blk driver's hardcoded check for nothing.
/// A wider entry is the opposite case. A reader still striding 32 bytes finds a plausible name at
/// the wrong offset and a start block cut out of the middle of one, and returns the wrong file
/// instead of an error. The version byte exists to turn exactly that silence into
/// [`Error::BadMagic`], and it is what makes a stale image left in `target/` fail loudly.
pub const MAGIC: [u8; 8] = *b"CRKR0002";

/// How many blocks the superblock-and-directory occupies. File data starts after it.
pub const DIR_BLOCKS: usize = 6;

/// The magic plus the count, before the first entry.
pub const HEADER_LEN: usize = 12;

/// The longest archive name, in bytes. A name that uses all of them has no NUL terminator.
///
/// **32 since 2026-08-01, up from 24** (milestone 63's prerequisite). The names in this system are
/// decided by what a program *does* (CLAUDE.md's naming tenet), and 24 had started deciding them
/// instead: `fs_subtree_caretaker` is 20 bytes and `sub_server_supervisor` is 21, so a fourth
/// qualifier would not have fit on either, and `os_primitives_benchmarker` is 25 and did not fit at
/// all. 32 clears the longest settled name by 7 bytes.
///
/// **Not larger**, because the cost is per entry and the benefit is speculative: every extra 8 bytes
/// of name is 8 bytes off every directory entry in every image, and no name anyone has argued for
/// comes near 32. The next raise is as cheap as this one was (there is no data migration; every
/// image regenerates from this crate), so buying headroom now buys nothing.
pub const NAME_LEN: usize = 32;

/// One directory entry: the name, then `start_block` and `len` as `u32`s.
pub const ENTRY_LEN: usize = NAME_LEN + 8;

/// How many entries fit **entirely inside block 0**, which is the bound for a reader that has only
/// one block buffered. 12 at `NAME_LEN = 32`.
///
/// Entries are packed from [`HEADER_LEN`] with no per-block padding, so an entry may straddle a
/// block boundary. That costs nothing to a reader holding the whole image (the kernel, the FS
/// server, `xtask`), and it is the whole limit for one that does not: the EL0 blk driver reads
/// block 0 into a 512-byte DMA buffer and walks the directory there. Without this bound it would
/// read the entry after the last complete one out of whatever follows the buffer.
pub const ENTRIES_IN_FIRST_BLOCK: usize = (BLOCK - HEADER_LEN) / ENTRY_LEN;

/// The most files an archive can hold: the directory blocks, past the header, in whole entries.
/// **76 at `DIR_BLOCKS = 6` and `ENTRY_LEN = 40`.**
///
/// **`DIR_BLOCKS` moved from 4 to 6 with the wider entry, on purpose.** Widening alone would have
/// dropped the ceiling from 63 files to 50, and the riscv64 initrd holds **exactly 50 files today**,
/// so the next program added to it would have failed the build. This ceiling is crossed by lanes
/// that cannot see each other, which is how it was crossed last time. 6 blocks put it at 76 and cost
/// 1 KB of image, once, in a multi-megabyte initrd.
///
/// **It costs no kernel stack, which is new.** It used to: [`Fs`] copied every entry into a fixed
/// `[Entry; MAX_FILES]` and is a stack local on the boot and spawn paths, so 63 entries was ~2 KB of
/// stack and raising the limit overflowed a 4-page kernel stack the day it was raised. That array is
/// gone (see [`Fs`]), so the only price of a bigger directory now is image bytes.
pub const MAX_FILES: usize = (DIR_BLOCKS * BLOCK - HEADER_LEN) / ENTRY_LEN;

/// Why [`Fs::parse`] refused an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The first eight bytes are not the archive magic.
    BadMagic,
    /// The directory claims more entries than [`MAX_FILES`] allows.
    TooManyFiles,
    /// A name longer than [`NAME_LEN`] bytes. Refused rather than truncated: two names agreeing in
    /// their first `NAME_LEN` bytes would otherwise become one entry, and the loader would fetch
    /// whichever program was packed first. See the crate's BUGS section.
    NameTooLong,
    /// A name containing a NUL byte, which the format cannot represent.
    ///
    /// **The same failure as [`Error::NameTooLong`], one byte wide instead of thirty-three.** A name
    /// shorter than [`NAME_LEN`] is NUL-padded on disk and every reader stops at the first NUL, so a
    /// name with one inside it comes back cut at that point: `"a\0b"` is written and reads back as
    /// `"a"`, and `read("a\0b")` finds nothing at all. Two names agreeing up to their first NUL
    /// become one entry, which is exactly the collision `NameTooLong` was introduced to stop.
    ///
    /// Refused rather than mangled, for the reason the crate's BUGS section gives for the other one:
    /// an archive is a mapping, and a writer that silently changes a key has lost data.
    ///
    /// Found by `fuzz/fuzz_targets/nifefs_roundtrip` on 2026-08-02, in under a minute, from the
    /// one-file input `[("\0", [])]`. The round-trip property is what saw it; no totality proof
    /// could have, because nothing panicked.
    NameHasNul,
    /// A file's data runs past the end of the image.
    OutOfBounds,
    /// The image is smaller than the directory span (`DIR_BLOCKS` blocks).
    Truncated,
}

/// One file: a name, and where its bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// NUL-padded on disk; use [`name_str`](Self::name_str) or [`name_eq`](Self::name_eq) rather
    /// than reading this directly.
    pub name: [u8; NAME_LEN],
    /// The block where this file's data begins.
    pub start_block: u32,
    /// The file's length in bytes.
    pub len: u32,
}

impl Entry {
    /// The name as a `&str` up to the first NUL, if it is valid UTF-8.
    pub fn name_str(&self) -> Option<&str> {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        core::str::from_utf8(&self.name[..end]).ok()
    }

    /// Whether this entry's name is exactly `name`.
    pub fn name_eq(&self, name: &str) -> bool {
        self.name_str() == Some(name)
    }
}

/// A parsed superblock. Borrows the whole image so file lookups can return slices into it.
///
/// **Holds no entry array, deliberately, and that is what made the 2026-08-01 raise cheap.** It used
/// to copy every directory entry into a fixed `[Entry; MAX_FILES]`, which made [`MAX_FILES`] a charge
/// against the *kernel stack*: `Fs` is a stack local in the boot and spawn paths, and raising the
/// limit from 31 to 63 entries on 2026-07-30 overflowed a 4-page kernel stack immediately, faulting
/// on the guard page while parsing the initrd.
/// Entries are decoded from the borrowed image on demand instead, which costs a few instructions per
/// lookup on a path that runs a handful of times per boot, and retires the stack cost and the ceiling
/// together. The image is already borrowed for exactly this reason.
pub struct Fs<'a> {
    image: &'a [u8],
    count: usize,
}

impl<'a> Fs<'a> {
    /// Validate the whole directory up front: magic, entry count, and every entry's data bounds.
    /// A returned `Fs` has nothing left to check; decoding an entry and every lookup built on it
    /// trust this pass completely.
    pub fn parse(image: &'a [u8]) -> Result<Self, Error> {
        // The directory blocks must be present before any entry offset (up to HEADER_LEN +
        // MAX_FILES*ENTRY_LEN, which sits inside DIR_BLOCKS) is read; this guard is what makes
        // those reads sound.
        if image.len() < DIR_BLOCKS * BLOCK {
            return Err(Error::Truncated);
        }
        if image[0..8] != MAGIC {
            return Err(Error::BadMagic);
        }

        let count = u32le(image, 8) as usize;
        if count > MAX_FILES {
            return Err(Error::TooManyFiles);
        }

        // Validate every entry now, not while reading: a server should reject a corrupt image once,
        // up front, rather than discover it mid-request. Decoding into a local costs no stack that
        // survives the loop, which is the whole point of not keeping the array.
        for i in 0..count {
            let e = Self::entry_at(image, i);
            let start = e.start_block as usize * BLOCK;
            let end = start
                .checked_add(e.len as usize)
                .ok_or(Error::OutOfBounds)?;
            if end > image.len() {
                return Err(Error::OutOfBounds);
            }
        }

        Ok(Fs { image, count })
    }

    /// How many files the directory holds.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the directory holds no files.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Decode entry `i` out of the directory. Caller guarantees `i < count`, which every caller here
    /// does by construction; `parse` has already bounds-checked the directory span.
    fn entry_at(image: &[u8], i: usize) -> Entry {
        let off = HEADER_LEN + i * ENTRY_LEN;
        let mut name = [0u8; NAME_LEN];
        name.copy_from_slice(&image[off..off + NAME_LEN]);
        Entry {
            name,
            start_block: u32le(image, off + NAME_LEN),
            len: u32le(image, off + NAME_LEN + 4),
        }
    }

    /// The directory, decoded lazily. One `Entry` exists at a time rather than [`MAX_FILES`] of them.
    pub fn entries(&self) -> impl Iterator<Item = Entry> + '_ {
        (0..self.count).map(|i| Self::entry_at(self.image, i))
    }

    /// The bytes of a file, by name.
    pub fn read(&self, name: &str) -> Option<&'a [u8]> {
        let e = self.entries().find(|e| e.name_eq(name))?;
        let start = e.start_block as usize * BLOCK;
        Some(&self.image[start..start + e.len as usize])
    }
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Write a nifefs image containing `files` (name, contents) into `out`, returning the number
/// of bytes written. `out` must be large enough; the disk tool sizes it from [`image_size`].
///
/// Not `no_std`-hostile, but only the disk-building tool calls it, on the host.
pub fn write_image(files: &[(&str, &[u8])], out: &mut [u8]) -> Result<usize, Error> {
    if files.len() > MAX_FILES {
        return Err(Error::TooManyFiles);
    }
    // Every name is checked before a byte is written, so a rejected archive is not a half-written
    // one. This used to truncate instead; see Error::NameTooLong for why that was the worse answer.
    for (name, _) in files {
        if name.len() > NAME_LEN {
            return Err(Error::NameTooLong);
        }
        // The same argument as NameTooLong, one byte wide (found by fuzzing, 2026-08-02): a name
        // holding a NUL is a name the format cannot store, because the padding IS a NUL and every
        // reader stops at the first one. Writing it produced an entry `read` could never match.
        if name.as_bytes().contains(&0) {
            return Err(Error::NameHasNul);
        }
    }

    for b in out.iter_mut() {
        *b = 0;
    }
    out[0..8].copy_from_slice(&MAGIC);
    out[8..HEADER_LEN].copy_from_slice(&(files.len() as u32).to_le_bytes());

    let mut block = DIR_BLOCKS as u32; // data starts after the superblock and directory
    for (i, (name, data)) in files.iter().enumerate() {
        let off = HEADER_LEN + i * ENTRY_LEN;
        let n = name.len(); // checked against NAME_LEN above
        out[off..off + n].copy_from_slice(&name.as_bytes()[..n]);
        out[off + NAME_LEN..off + NAME_LEN + 4].copy_from_slice(&block.to_le_bytes());
        out[off + NAME_LEN + 4..off + NAME_LEN + 8]
            .copy_from_slice(&(data.len() as u32).to_le_bytes());

        let start = block as usize * BLOCK;
        let end = start + data.len();
        if end > out.len() {
            return Err(Error::OutOfBounds);
        }
        out[start..end].copy_from_slice(data);

        let blocks = data.len().div_ceil(BLOCK).max(1) as u32;
        block += blocks;
    }

    Ok(block as usize * BLOCK)
}

/// How many bytes an image holding `files` needs.
pub fn image_size(files: &[(&str, &[u8])]) -> usize {
    let mut blocks = DIR_BLOCKS;
    for (_, data) in files {
        blocks += data.len().div_ceil(BLOCK).max(1);
    }
    blocks * BLOCK
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;

    #[test]
    fn write_then_read_round_trips() {
        let files: [(&str, &[u8]); 2] = [("motd", b"welcome to nife\n"), ("empty", b"")];
        let mut img = vec![0u8; image_size(&files)];
        let n = write_image(&files, &mut img).unwrap();
        assert_eq!(n, img.len());

        let fs = Fs::parse(&img).unwrap();
        assert_eq!(fs.len(), 2);
        assert_eq!(fs.read("motd"), Some(&b"welcome to nife\n"[..]));
        assert_eq!(fs.read("empty"), Some(&b""[..]));
        assert_eq!(fs.read("nope"), None);
    }

    #[test]
    fn files_are_block_aligned() {
        // A file longer than one block pushes the next file to a later block.
        let big = vec![0x41u8; 600]; // > 512, so 2 blocks
        let files: [(&str, &[u8]); 2] = [("big", &big), ("after", b"x")];
        let mut img = vec![0u8; image_size(&files)];
        write_image(&files, &mut img).unwrap();

        let fs = Fs::parse(&img).unwrap();
        let after = fs.entries().find(|e| e.name_eq("after")).unwrap();
        assert_eq!(
            after.start_block as usize,
            DIR_BLOCKS + 2,
            "big took the two blocks after the directory, after should follow"
        );
        assert_eq!(fs.read("big").unwrap().len(), 600);
    }

    /// **The two derived capacity constants are the values their docs claim.** Nothing else pins
    /// them: every other test compares an image against the same constant it was built from, so an
    /// operator slip in the formula moves both sides together and no round trip notices. The values
    /// are load-bearing outside this crate (the EL0 blk driver stops at `ENTRIES_IN_FIRST_BLOCK`;
    /// the initrd build fails at `MAX_FILES`), so a wrong one is a wrong bound in another program.
    /// A deliberate resize of `DIR_BLOCKS` or `ENTRY_LEN` updates this test in the same commit.
    #[test]
    fn the_capacity_constants_are_the_documented_values() {
        // (BLOCK - HEADER_LEN) / ENTRY_LEN = (512 - 12) / 40 = 12. The doc comment says 12.
        assert_eq!(ENTRIES_IN_FIRST_BLOCK, 12);
        // (DIR_BLOCKS * BLOCK - HEADER_LEN) / ENTRY_LEN = (3072 - 12) / 40 = 3060 / 40 = 76.
        assert_eq!(MAX_FILES, 76);
    }

    #[test]
    fn bad_magic_is_refused() {
        // A full-directory-sized image so the truncation guard passes and the magic check rejects it.
        let img = vec![0u8; DIR_BLOCKS * BLOCK];
        assert_eq!(Fs::parse(&img).err(), Some(Error::BadMagic));
    }

    #[test]
    fn a_truncated_image_is_refused() {
        assert_eq!(Fs::parse(&[0u8; 10]).err(), Some(Error::Truncated));
    }

    /// **One byte under the directory span is `Truncated`, at the exact boundary.** The guard must
    /// compare against the full `DIR_BLOCKS * BLOCK` = 3072 bytes; the 10-byte test above is under
    /// every plausible mis-computation of that span (a `+` slip gives 518), so only a
    /// just-under-the-line image proves the multiplication. A guard that let 3071 bytes through
    /// would report `BadMagic` here instead, and would let entry reads run off a short image.
    #[test]
    fn an_image_one_byte_under_the_directory_span_is_truncated() {
        let img = vec![0u8; DIR_BLOCKS * BLOCK - 1];
        assert_eq!(Fs::parse(&img).err(), Some(Error::Truncated));
    }

    /// **A file whose last byte is the image's last byte is legal, in the writer and the reader.**
    /// Both bounds checks are `end > len`, and every other test ends its data short of a block
    /// boundary, so the accept side of the exact-fit case was untested in both directions: a check
    /// hardened to `>=` would refuse a valid archive whose final file is block-sized. One block of
    /// data after a 6-block directory ends at byte 3584 == image length exactly.
    #[test]
    fn a_file_ending_exactly_at_the_image_end_round_trips() {
        let body = vec![0x5au8; BLOCK];
        let files: [(&str, &[u8]); 1] = [("exact", &body)];
        let mut img = vec![0u8; image_size(&files)];
        let n = write_image(&files, &mut img).unwrap();
        assert_eq!(n, img.len());
        let fs = Fs::parse(&img).unwrap();
        assert_eq!(fs.read("exact"), Some(&body[..]));
    }

    /// `is_empty` had no caller in the tests at all, so all three of its mutations (constant true,
    /// constant false, inverted comparison) were invisible. Both sides pin it to `len() == 0`.
    #[test]
    fn an_empty_archive_is_empty_and_a_packed_one_is_not() {
        let none: [(&str, &[u8]); 0] = [];
        let mut img = vec![0u8; image_size(&none)];
        write_image(&none, &mut img).unwrap();
        let fs = Fs::parse(&img).unwrap();
        assert!(fs.is_empty());
        assert_eq!(fs.len(), 0);

        let files: [(&str, &[u8]); 1] = [("motd", b"hi")];
        let mut img = vec![0u8; image_size(&files)];
        write_image(&files, &mut img).unwrap();
        assert!(!Fs::parse(&img).unwrap().is_empty());
    }

    #[test]
    fn a_file_pointing_past_the_end_is_refused() {
        let mut img = vec![0u8; DIR_BLOCKS * BLOCK];
        img[0..8].copy_from_slice(&MAGIC);
        img[8..12].copy_from_slice(&1u32.to_le_bytes());
        // one entry, start_block 100 (way past the image)
        let off = HEADER_LEN;
        img[off + NAME_LEN..off + NAME_LEN + 4].copy_from_slice(&100u32.to_le_bytes());
        img[off + NAME_LEN + 4..off + NAME_LEN + 8].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(Fs::parse(&img).err(), Some(Error::OutOfBounds));
    }

    #[test]
    fn too_many_files_is_refused() {
        let data: &[u8] = b"x";
        let files: vec::Vec<(&str, &[u8])> = (0..MAX_FILES + 1).map(|_| ("f", data)).collect();
        let mut img = vec![0u8; image_size(&files) + BLOCK];
        assert_eq!(
            write_image(&files, &mut img).err(),
            Some(Error::TooManyFiles)
        );
    }

    /// **The longest settled program name fits, and one byte more is refused.** The limit exists to
    /// bound a name, and a name is what the reader will actually collide with, so the test names the
    /// real one: `os_primitives_benchmarker` (25 bytes) is what forced the raise from 24.
    #[test]
    fn a_name_is_bounded_by_name_len_and_a_longer_one_is_refused() {
        let data: &[u8] = b"y";
        let longest = "os_primitives_benchmarker";
        assert!(longest.len() <= NAME_LEN, "the name that forced the raise");

        // A name using every byte has no NUL terminator, which is the case name_str must handle.
        let full = "x".repeat(NAME_LEN);
        let files: [(&str, &[u8]); 2] = [(longest, data), (full.as_str(), data)];
        let mut img = vec![0u8; image_size(&files)];
        write_image(&files, &mut img).unwrap();
        let fs = Fs::parse(&img).unwrap();
        assert_eq!(fs.read(longest), Some(data));
        assert_eq!(fs.read(&full), Some(data));

        // One byte over is an error, not a truncation: truncating would have merged two names.
        let over = "x".repeat(NAME_LEN + 1);
        let files: [(&str, &[u8]); 1] = [(over.as_str(), data)];
        let mut img = vec![0u8; image_size(&files)];
        assert_eq!(
            write_image(&files, &mut img).err(),
            Some(Error::NameTooLong)
        );
    }

    /// **A name with a NUL in it is refused, not stored and then unfindable.**
    ///
    /// The regression for the bug `fuzz/fuzz_targets/nifefs_roundtrip` found on 2026-08-02, whose
    /// minimal input was a single file named `"\0"` with no contents. That name was accepted, packed
    /// into an entry that decoded back as the empty string, and then `read("\0")` answered `None`:
    /// data written and not readable, with nothing panicking and no test noticing. Every name below
    /// is one the writer used to take.
    ///
    /// It is the same failure as the truncation bug the crate already documents, which is the point.
    /// A round-trip property catches the whole family; an example test catches the example.
    #[test]
    fn a_name_with_a_nul_in_it_is_refused() {
        let data: &[u8] = b"z";
        for name in ["\0", "a\0b", "init\0", "\0\0"] {
            let files: [(&str, &[u8]); 1] = [(name, data)];
            let mut img = vec![0u8; image_size(&files)];
            assert_eq!(
                write_image(&files, &mut img).err(),
                Some(Error::NameHasNul),
                "{name:?} cannot be stored, so it must not be accepted"
            );
        }

        // The empty name is NOT in that list and is deliberately still legal: it is representable
        // (an all-NUL name field), it reads back as itself, and refusing it would be a policy this
        // format has no reason to hold.
        let files: [(&str, &[u8]); 1] = [("", data)];
        let mut img = vec![0u8; image_size(&files)];
        write_image(&files, &mut img).unwrap();
        assert_eq!(Fs::parse(&img).unwrap().read(""), Some(data));
    }

    /// **`MAX_FILES` costs no kernel stack, and this is what keeps it that way.** `Fs` is a stack
    /// local on the kernel's boot and spawn paths; when it held a `[Entry; MAX_FILES]`, raising the
    /// limit overflowed a 4-page kernel stack the same day. Reintroducing the array would make this
    /// fail rather than making a boot fault on a guard page.
    #[test]
    fn fs_does_not_grow_with_max_files() {
        assert_eq!(
            core::mem::size_of::<Fs>(),
            core::mem::size_of::<&[u8]>() + core::mem::size_of::<usize>(),
            "Fs is a borrowed image and a count, nothing per-entry"
        );
        assert_eq!(core::mem::size_of::<Entry>(), ENTRY_LEN);
    }

    #[test]
    fn a_full_directory_round_trips() {
        // Exactly MAX_FILES files must write and parse back, so the two-block directory's last entry
        // (offset 12 + (MAX_FILES-1)*32) is honored, not silently past the header's reach.
        let data: &[u8] = b"z";
        let names: vec::Vec<std::string::String> =
            (0..MAX_FILES).map(|i| std::format!("f{i}")).collect();
        let files: vec::Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), data)).collect();
        let mut img = vec![0u8; image_size(&files)];
        write_image(&files, &mut img).unwrap();
        let fs = Fs::parse(&img).unwrap();
        assert_eq!(fs.len(), MAX_FILES);
        assert_eq!(fs.read(&names[MAX_FILES - 1]), Some(data));
    }

    /// **A directory that spills past one block round-trips, every file with distinct contents.**
    /// `a_full_directory_round_trips` above proves the capacity boundary but writes identical bytes
    /// to every file, so a directory that mixed up `start_block`s would still read the same byte back
    /// and pass. This one gives every file its own contents and name and checks each reads back
    /// byte-for-byte, the integrity guard for the multi-block directory. It is the case the riscv
    /// initrd hit (its per-role driver binaries push it past fifteen).
    #[test]
    fn a_directory_spanning_two_blocks_round_trips() {
        // Twenty files, each with distinct contents longer than a name so a shifted or truncated
        // directory could not accidentally match.
        let bodies: vec::Vec<vec::Vec<u8>> = (0..20u8)
            .map(|i| vec![i; (BLOCK + i as usize) % (2 * BLOCK) + 1])
            .collect();
        let names: vec::Vec<vec::Vec<u8>> = (0..20u8)
            .map(|i| std::format!("file{i}").into_bytes())
            .collect();
        let files: vec::Vec<(&str, &[u8])> = (0..20)
            .map(|i| {
                (
                    core::str::from_utf8(&names[i]).unwrap(),
                    bodies[i].as_slice(),
                )
            })
            .collect();
        assert!(
            files.len() > ENTRIES_IN_FIRST_BLOCK,
            "test must exceed one block"
        );

        let mut img = vec![0u8; image_size(&files)];
        let n = write_image(&files, &mut img).expect("write");
        assert_eq!(n, img.len());

        let fs = Fs::parse(&img).expect("parse");
        assert_eq!(fs.len(), 20);
        for (i, body) in bodies.iter().enumerate() {
            let name = core::str::from_utf8(&names[i]).unwrap();
            assert_eq!(
                fs.read(name),
                Some(body.as_slice()),
                "file {name} mismatched"
            );
        }
    }
}

/// Machine-checked proofs (`script/verify`; notes/verification.md).
///
/// The initrd is parsed by the KERNEL (kernel/src/user.rs `program`), which puts this parser
/// inside the TCB on externally-supplied bytes, the same position the dtb parser is in. And the
/// same wall: the first attempt proved the WHOLE parse over a symbolic image, on the theory
/// that a 15-entry bounded loop is tractable; CBMC disagreed (20+ CPU-minutes and climbing), so
/// as in dtb the proof is decomposed to what converges and carries the weight:
///
/// - the **validation-implies-safe-read** arithmetic, for every entry value and image length
///   (no symbolic arrays; this is the kernel-facing guarantee), and
/// - the **truncation guard** for every image under the directory span.
///
/// What is deliberately NOT proved: whole-parse totality (the wall above; the in-bounds
/// indexing it would add is `u32le`/`copy_from_slice` at offsets statically under one block,
/// which the `image.len() < DIR_BLOCKS * BLOCK` guard, proved below, makes sound), and `name_str` (its panic
/// surface is a slice bounded by `position()`, and its UTF-8 half is `core`'s validator, which
/// costs CBMC minutes to re-prove and is trusted everywhere else in the system).
#[cfg(kani)]
mod verification {
    use super::*;

    /// **Parse's validation is sufficient for `read`: proved as arithmetic, for every entry.**
    /// For ANY `start_block`, `len`, and image length: if the exact check `parse` performs
    /// accepts the entry (`start_block * BLOCK`, `checked_add(len)`, `end <= image_len`), then
    /// the slice bounds `read` computes satisfy `start <= end <= image_len`, so the indexing
    /// cannot panic and the returned bytes lie inside the image. This is the kernel-facing
    /// guarantee, free of any bound on the image size.
    #[kani::proof]
    fn the_validation_implies_reads_slice_is_in_bounds() {
        let start_block: u32 = kani::any();
        let len: u32 = kani::any();
        let image_len: usize = kani::any();

        // Exactly parse's acceptance condition, no more.
        let start = start_block as usize * BLOCK;
        let Some(end) = start.checked_add(len as usize) else {
            return; // parse refuses: OutOfBounds
        };
        if end > image_len {
            return; // parse refuses: OutOfBounds
        }

        // Exactly read's slice arithmetic.
        let r_start = start_block as usize * BLOCK;
        let r_end = r_start + len as usize; // cannot overflow: equals `end` above
        assert!(r_start <= r_end);
        assert!(r_end <= image_len, "read could index past the image");
    }

    /// **A short image is always `Truncated`, never indexed**: for any image under the directory
    /// span, parse refuses before touching a byte past the length check (which is what keeps the
    /// entry reads, up to offset 12 + `MAX_FILES`*32 inside `DIR_BLOCKS`, in bounds).
    #[kani::proof]
    fn a_short_image_is_refused_not_indexed() {
        const SHORT: usize = DIR_BLOCKS * BLOCK - 1;
        let image: [u8; SHORT] = kani::any();
        assert_eq!(Fs::parse(&image).err(), Some(Error::Truncated));
    }
}
