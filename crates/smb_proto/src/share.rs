//! **The seam between the SMB state machine and whatever holds the files.**
//!
//! Milestone 54's block names the shape: the adapter holds one directory capability and one
//! network endpoint. [`Share`] is the directory-capability side of that seam, drawn so the
//! protocol machine ([`crate::server`]) never knows what backs it: the host tests and the QEMU
//! gate serve a [`FixtureShare`] (files baked into the binary), and the fs_proto-backed
//! implementation, which walks a real directory capability into the FS server, implements this
//! same trait in the server program where the IPC lives. That backend is the milestone's recorded
//! remaining piece; the trait is here so landing it changes no protocol code.
//!
//! The model is deliberately smaller than a filesystem: a flat, read-only directory of files. No
//! subdirectories, no timestamps (everything reports the epoch), no attributes beyond
//! file-versus-directory. Each absence is a `BUGS`-grade limitation of milestone 54's scope, not
//! of the trait: growing the model is adding methods, which is cheap next to the wire format.

/// A node in the share: the root directory or one of its files, named by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    /// The share's root, the one directory there is.
    Root,
    /// The `i`th file of the share, in listing order.
    File(usize),
}

/// What a directory listing says about one entry.
pub struct Entry<'a> {
    /// The entry's name, ASCII (see the crate BUGS on names).
    pub name: &'a [u8],
    /// Its size in bytes (0 for directories).
    pub size: u64,
    /// Directory or file: the one attribute this model carries.
    pub is_dir: bool,
}

/// What the SMB server asks of whatever holds the files.
pub trait Share {
    /// Resolve a lower-cased ASCII name (no path separators; the share is flat) to a node.
    fn lookup(&self, name: &[u8]) -> Option<Node>;
    /// The `index`th entry of the root, or `None` past the end. Index 0 upward, stable across a
    /// connection: `QUERY_DIRECTORY` resumes by index.
    fn entry(&self, index: usize) -> Option<Entry<'_>>;
    /// A file's size in bytes.
    fn size(&self, file: usize) -> u64;
    /// Read from a file at `offset` into `out`, returning the bytes produced (short at EOF, 0 at
    /// or past it).
    fn read(&self, file: usize, offset: u64, out: &mut [u8]) -> usize;
}

/// A share whose files are baked into the binary: the fixture the tests and the QEMU demo serve.
pub struct FixtureShare {
    /// `(name, contents)` pairs; names lower-case ASCII, listing order.
    pub files: &'static [(&'static [u8], &'static [u8])],
}

/// The fixture every test and the QEMU boot serve, so the host prober, the host tests, and the
/// notes' worked example all read the same bytes. One file's contents are asserted end to end;
/// the second exists so a directory listing has more than one row.
pub const FIXTURE: FixtureShare = FixtureShare {
    files: &[
        (b"hello.txt", b"nife serves SMB\n"),
        (
            b"readme.md",
            b"This share is served by nife's smb_server through the socket contract.\n\
              It is read-only and everything in it is baked into the server binary;\n\
              the fs_proto-backed share is milestone 54's recorded remaining piece.\n",
        ),
    ],
};

impl Share for FixtureShare {
    fn lookup(&self, name: &[u8]) -> Option<Node> {
        if name.is_empty() {
            return Some(Node::Root);
        }
        self.files
            .iter()
            .position(|(n, _)| *n == name)
            .map(Node::File)
    }

    fn entry(&self, index: usize) -> Option<Entry<'_>> {
        let (name, body) = self.files.get(index)?;
        Some(Entry {
            name,
            size: body.len() as u64,
            is_dir: false,
        })
    }

    fn size(&self, file: usize) -> u64 {
        self.files.get(file).map_or(0, |(_, b)| b.len() as u64)
    }

    fn read(&self, file: usize, offset: u64, out: &mut [u8]) -> usize {
        let Some((_, body)) = self.files.get(file) else {
            return 0;
        };
        let off = offset.min(body.len() as u64) as usize;
        let n = (body.len() - off).min(out.len());
        out[..n].copy_from_slice(&body[off..off + n]);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_resolves_the_root_the_files_and_nothing_else() {
        assert_eq!(FIXTURE.lookup(b""), Some(Node::Root));
        assert_eq!(FIXTURE.lookup(b"hello.txt"), Some(Node::File(0)));
        assert_eq!(FIXTURE.lookup(b"readme.md"), Some(Node::File(1)));
        assert_eq!(FIXTURE.lookup(b"absent"), None);
    }

    #[test]
    fn reads_are_bounded_by_eof_and_by_the_buffer() {
        let mut buf = [0u8; 8];
        assert_eq!(FIXTURE.read(0, 0, &mut buf), 8);
        assert_eq!(&buf, b"nife ser");
        let n = FIXTURE.read(0, 8, &mut buf);
        assert_eq!(&buf[..n], b"ves SMB\n");
        assert_eq!(FIXTURE.read(0, FIXTURE.size(0), &mut buf), 0, "at EOF");
        assert_eq!(FIXTURE.read(0, u64::MAX, &mut buf), 0, "far past EOF");
    }

    #[test]
    fn the_listing_ends_and_matches_lookup() {
        let mut n = 0;
        while let Some(e) = FIXTURE.entry(n) {
            assert_eq!(FIXTURE.lookup(e.name), Some(Node::File(n)));
            n += 1;
        }
        assert_eq!(n, FIXTURE.files.len());
    }
}
