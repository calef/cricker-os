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
//!   block 0            the superblock
//!     magic   "CRKR0001"   (8 bytes)
//!     count   u32 LE       how many files
//!     ...then `count` directory entries, each 32 bytes:
//!       name        24 bytes, NUL-padded
//!       start_block u32 LE  where the file's data begins
//!       len         u32 LE  the file's length in bytes
//!
//!   block 1..           file data, each file block-aligned
//! ```
//!
//! A 512-byte block holds the magic, the count, and up to 15 entries, which is all the files this
//! filesystem will ever need to hold to make its point.

#![no_std]

/// The block size, and the alignment of everything.
pub const BLOCK: usize = 512;

/// Superblock magic. Version in the last four bytes, so a format change is legible.
pub const MAGIC: [u8; 8] = *b"CRKR0001";

const NAME_LEN: usize = 24;
const ENTRY_LEN: usize = 32;
const MAX_FILES: usize = (BLOCK - 12) / ENTRY_LEN; // 15

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

    let mut block = 1u32; // data starts after the superblock
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
    let mut blocks = 1usize;
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
        let files: [(&str, &[u8]); 2] = [
            ("motd", b"welcome to cricker-os\n"),
            ("empty", b""),
        ];
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
        assert_eq!(after.start_block, 3, "big took blocks 1-2, after should be at 3");
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
        let files: vec::Vec<(&str, &[u8])> = (0..16).map(|_| ("f", data)).collect();
        let mut img = vec![0u8; image_size(&files) + BLOCK];
        assert_eq!(write_image(&files, &mut img).err(), Some(Error::TooManyFiles));
    }
}
