//! **crickerfs**: a filesystem so simple it fits in a comment.
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
//!   block 0..          the directory (as many 512-byte blocks as the entries need)
//!     magic   "CRKR0001"   (8 bytes)
//!     count   u32 LE       how many files
//!     ...then `count` directory entries, each 32 bytes:
//!       name        24 bytes, NUL-padded
//!       start_block u32 LE  where the file's data begins
//!       len         u32 LE  the file's length in bytes
//!
//!   then                file data, each file block-aligned, starting at the block after the
//!                       directory (block 1 for the common one-block directory)
//! ```
//!
//! Up to fifteen entries fit one 512-byte block; more spill into a second, and file data starts
//! after the directory rather than at a fixed block 1. So an image at or under fifteen files is
//! byte-identical to the original single-block format, and a larger one (the riscv initrd, whose
//! per-role driver binaries push it past fifteen) just pushes its data out by a block.

#![no_std]

/// The block size, and the alignment of everything.
pub const BLOCK: usize = 512;

/// Superblock magic. Version in the last four bytes, so a format change is legible.
pub const MAGIC: [u8; 8] = *b"CRKR0001";

const NAME_LEN: usize = 24;
const ENTRY_LEN: usize = 32;

/// The most files an image may hold. The directory (the magic, the count, then `count` 32-byte
/// entries) now spans **as many blocks as it needs**, and file data starts at the block after it,
/// so the count is no longer capped at the fifteen entries that fit one 512-byte block. Two blocks
/// (31 files) is ample for every image this kernel builds: the largest is the riscv initrd, whose
/// per-role driver binaries (aarch64 folds them into one `hello`) push it to ~17. Raising this is a
/// one-line change, paid in the parsed [`Fs`]'s on-stack entry array. Nothing persists a crickerfs
/// image across a rebuild (every initrd and the crickerfs disk are regenerated), so the multi-block
/// directory needs no on-disk migration; and for any image at or under the old fifteen-file cap the
/// layout is byte-identical (a one-block directory, data at block 1), so nothing regressed.
const MAX_FILES: usize = 31;

/// How many 512-byte blocks the directory occupies for `count` files. File data starts at the block
/// after it. One block for up to fifteen files, which is what keeps small images byte-identical to
/// the pre-multi-block format.
const fn dir_blocks(count: usize) -> usize {
    // `12 + count*ENTRY_LEN` is at least 12, so `div_ceil` is at least 1; no `.max(1)` needed (and
    // `usize::max` is not const on this toolchain).
    (12 + count * ENTRY_LEN).div_ceil(BLOCK)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    BadMagic,
    TooManyFiles,
    /// A file's data runs past the end of the image.
    OutOfBounds,
    /// The image is smaller than one block.
    Truncated,
}

/// One file: a name, and where its bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub name: [u8; NAME_LEN],
    pub start_block: u32,
    pub len: u32,
}

impl Entry {
    /// The name as a `&str` up to the first NUL, if it is valid UTF-8.
    pub fn name_str(&self) -> Option<&str> {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        core::str::from_utf8(&self.name[..end]).ok()
    }

    pub fn name_eq(&self, name: &str) -> bool {
        self.name_str() == Some(name)
    }
}

/// A parsed superblock. Borrows the whole image so file lookups can return slices into it.
pub struct Fs<'a> {
    image: &'a [u8],
    entries: [Entry; MAX_FILES],
    count: usize,
}

impl<'a> Fs<'a> {
    pub fn parse(image: &'a [u8]) -> Result<Self, Error> {
        if image.len() < BLOCK {
            return Err(Error::Truncated);
        }
        if image[0..8] != MAGIC {
            return Err(Error::BadMagic);
        }

        let count = u32le(image, 8) as usize;
        if count > MAX_FILES {
            return Err(Error::TooManyFiles);
        }
        // The directory may now span more than one block, so make sure it fits the image before
        // indexing entries (a corrupt count must not read past the buffer).
        if 12 + count * ENTRY_LEN > image.len() {
            return Err(Error::OutOfBounds);
        }

        let mut entries = [Entry {
            name: [0; NAME_LEN],
            start_block: 0,
            len: 0,
        }; MAX_FILES];

        for (i, e) in entries.iter_mut().enumerate().take(count) {
            let off = 12 + i * ENTRY_LEN;
            e.name.copy_from_slice(&image[off..off + NAME_LEN]);
            e.start_block = u32le(image, off + NAME_LEN);
            e.len = u32le(image, off + NAME_LEN + 4);

            // Validate now, not while reading: a server should reject a corrupt image once, up
            // front, not discover it mid-request.
            let start = e.start_block as usize * BLOCK;
            let end = start
                .checked_add(e.len as usize)
                .ok_or(Error::OutOfBounds)?;
            if end > image.len() {
                return Err(Error::OutOfBounds);
            }
        }

        Ok(Fs {
            image,
            entries,
            count,
        })
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries[..self.count]
    }

    /// The bytes of a file, by name.
    pub fn read(&self, name: &str) -> Option<&'a [u8]> {
        let e = self.entries().iter().find(|e| e.name_eq(name))?;
        let start = e.start_block as usize * BLOCK;
        Some(&self.image[start..start + e.len as usize])
    }
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Write a crickerfs image containing `files` (name, contents) into `out`, returning the number
/// of bytes written. `out` must be large enough; the disk tool sizes it from [`image_size`].
///
/// Not `no_std`-hostile, but only the disk-building tool calls it, on the host.
pub fn write_image(files: &[(&str, &[u8])], out: &mut [u8]) -> Result<usize, Error> {
    if files.len() > MAX_FILES {
        return Err(Error::TooManyFiles);
    }

    for b in out.iter_mut() {
        *b = 0;
    }
    out[0..8].copy_from_slice(&MAGIC);
    out[8..12].copy_from_slice(&(files.len() as u32).to_le_bytes());

    let mut block = dir_blocks(files.len()) as u32; // data starts after the (multi-block) directory
    for (i, (name, data)) in files.iter().enumerate() {
        let off = 12 + i * ENTRY_LEN;
        let n = name.len().min(NAME_LEN);
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
    let mut blocks = dir_blocks(files.len());
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
        let files: [(&str, &[u8]); 2] = [("motd", b"welcome to cricker-os\n"), ("empty", b"")];
        let mut img = vec![0u8; image_size(&files)];
        let n = write_image(&files, &mut img).unwrap();
        assert_eq!(n, img.len());

        let fs = Fs::parse(&img).unwrap();
        assert_eq!(fs.len(), 2);
        assert_eq!(fs.read("motd"), Some(&b"welcome to cricker-os\n"[..]));
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
        let after = fs.entries().iter().find(|e| e.name_eq("after")).unwrap();
        assert_eq!(
            after.start_block, 3,
            "big took blocks 1-2, after should be at 3"
        );
        assert_eq!(fs.read("big").unwrap().len(), 600);
    }

    #[test]
    fn bad_magic_is_refused() {
        let img = vec![0u8; BLOCK];
        assert_eq!(Fs::parse(&img).err(), Some(Error::BadMagic));
    }

    #[test]
    fn a_truncated_image_is_refused() {
        assert_eq!(Fs::parse(&[0u8; 10]).err(), Some(Error::Truncated));
    }

    #[test]
    fn a_file_pointing_past_the_end_is_refused() {
        let mut img = vec![0u8; BLOCK];
        img[0..8].copy_from_slice(&MAGIC);
        img[8..12].copy_from_slice(&1u32.to_le_bytes());
        // one entry, start_block 100 (way past a one-block image)
        let off = 12;
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

    /// **A directory that spills past one block round-trips.** More than fifteen files forces the
    /// directory into a second block and file data to start after it; every file must still read
    /// back byte-for-byte. This is the case the riscv initrd hit (its per-role driver binaries push
    /// it past fifteen), and the regression guard for the multi-block directory.
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
            files.len() > (BLOCK - 12) / ENTRY_LEN,
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
/// - the **truncation guard** for every under-one-block image.
///
/// What is deliberately NOT proved: whole-parse totality (the wall above; the in-bounds indexing it
/// would add is `u32le`/`copy_from_slice` over the directory, at offsets below `12 + count *
/// ENTRY_LEN`, which the directory-fits guard (`12 + count * ENTRY_LEN > image.len()` returns
/// `OutOfBounds`) bounds inside the image). The directory may now span more than one block, so that
/// explicit guard, not the one-block `image.len() < BLOCK` truncation guard, is what makes the
/// directory reads sound; the truncation guard still refuses the degenerate under-one-block image.
/// Also not proved: `name_str` (its panic surface is a slice bounded by `position()`, and its UTF-8
/// half is `core`'s validator, which costs CBMC minutes to re-prove and is trusted everywhere else).
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

    /// **A short image is always `Truncated`, never indexed**: for any image under one block,
    /// parse refuses before touching a byte past the length check.
    #[kani::proof]
    fn a_short_image_is_refused_not_indexed() {
        const SHORT: usize = BLOCK - 1;
        let image: [u8; SHORT] = kani::any();
        assert_eq!(Fs::parse(&image).err(), Some(Error::Truncated));
    }
}
