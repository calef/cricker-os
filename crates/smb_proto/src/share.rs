//! **The seam between the SMB state machine and whatever holds the files.**
//!
//! Milestone 54's block names the shape: the adapter holds one directory capability and one
//! network endpoint. [`Share`] is the directory-capability side of that seam, drawn so the
//! protocol machine ([`crate::server`]) never knows what backs it: the host tests and the QEMU
//! gate serve a [`FixtureShare`] (files baked into the binary), and the fs_proto-backed
//! implementation, which walks a real directory capability into the FS server, implements this
//! same trait in the server program where the IPC lives.
//!
//! The model is deliberately smaller than a filesystem: a **flat directory of files**, readable
//! always and writable when the backing says so. No subdirectories, no timestamps (everything
//! reports the epoch), no attributes beyond file-versus-directory. Each absence is a `BUGS`-grade
//! limitation of milestone 54's scope, not of the trait: growing the model is adding methods,
//! which is cheap next to the wire format.
//!
//! # What the write path changed here, and why (milestone 54's second half)
//!
//! Two contract changes, both forced by the same fact: **a writable share's listing moves.**
//!
//! - **A file is named by an opaque [`FileId`] the backing mints at open, not by its index in the
//!   listing.** The read-only trait let `Node::File(usize)` mean "the `i`th entry", which was
//!   sound only because nothing could reorder the directory. Create one file and every index
//!   after it shifts, so every open handle would silently start reading a different file. An
//!   identity the backing assigns is the fix, and it retires the re-open-per-request cost the
//!   read path's `BUGS` recorded: the fs-backed share now keeps the FS server's handle in the id.
//! - **Every fallible call carries an [`Error`]**, which the read path's `BUGS` named as missing:
//!   a refusal used to arrive on the wire as "no such file" and a failed read as EOF, so a client
//!   could not tell a capability refusal from an absence. A write path cannot afford that; a
//!   backup client that reads "your write succeeded, zero bytes" loses data quietly.
//!
//! # Read-only is a property of the share, checked before the backing is asked
//!
//! [`Share::writable`] has **no default**, so a backing cannot be written without saying which it
//! is, and [`crate::server`] refuses every mutating command on a share that answers `false`
//! *before* it calls the backing. That ordering is the point (the milestone's brief says so): a
//! read-only share must refuse at the protocol layer, with the status a client acts on, rather
//! than passing the request down and hoping the filesystem says no. The mutating methods here
//! then default to [`Error::ReadOnly`] as a second, independent line, so a backing that
//! implements nothing is read-only by construction rather than by remembering.

/// **What a share says when it cannot do what was asked.** The error channel the read-only trait
/// lacked; [`crate::server::status_for`] is the one place these become NT statuses, so a client's
/// retry logic keys on one mapping rather than on whatever each call site reached for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No such name in the share (and, for this flat model, a name that is a directory: see the
    /// crate `BUGS`, the share has no subdirectory nodes to hand back).
    NotFound,
    /// The name is already taken, and the request said create rather than create-or-open.
    Exists,
    /// The node is a directory and the operation is a file's.
    IsDirectory,
    /// Through this share, that cannot be done: the share is read-only, or the capability behind
    /// it is. Deliberately one variant rather than two, because the wire cannot tell a client
    /// anything useful about *which*, and DECISIONS §27's reason for `EROFS` over `EACCES`
    /// applies unchanged: there is no policy that could have said yes, only what the capability is.
    ReadOnly,
    /// The filesystem is full.
    NoSpace,
    /// The name is longer than [`crate::server::MAX_NAME`], which is the share's own bound rather
    /// than the filesystem's.
    NameTooLong,
    /// The backing failed in a way this model has no word for. Reported rather than swallowed:
    /// "something went wrong" on the wire beats a short read a client reads as EOF.
    Io,
}

/// **A file's identity, minted by the backing at open or create.** Opaque to the protocol machine,
/// which only ever hands one back where it got it. The fs-backed share makes it the FS server's
/// handle; the fixture makes it the index into its baked-in table. See the module header for why
/// this replaced a listing index.
pub type FileId = u64;

/// A node in the share: the root directory, or one open file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    /// The share's root, the one directory there is.
    Root,
    /// One open file, by the id its backing minted.
    File(FileId),
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

/// **A nominal volume capacity, in bytes, and a lie the reader is warned about.**
///
/// `FileFsSizeInformation` and `FileFsFullSizeInformation` make a client decide whether to start
/// writing, and macOS will not write to a volume reporting zero free space. This share has no way
/// to ask: `fs_proto` carries no `statfs` verb, so nothing between the SMB adapter and RedoxFS
/// knows the image's real size or its free blocks. Rather than report a truth it does not have,
/// the share reports this figure and the crate `BUGS` says plainly that it is nominal: a write
/// past the real end of the image fails with `STATUS_DISK_FULL` from the filesystem, at the time
/// of the write, instead of being predicted. A `statfs` verb is the recorded fix and is a
/// `fs_proto` contract change, which is not a lane's to mint (AGENTS.md).
pub const NOMINAL_VOLUME_BYTES: u64 = 64 * 1024 * 1024;

/// What the SMB server asks of whatever holds the files.
///
/// The read half is required; the write half defaults to [`Error::ReadOnly`] so that a read-only
/// backing implements only what it can do. [`Share::writable`] has no default on purpose (see the
/// module header): a backing must state its direction.
pub trait Share {
    /// **May this share be changed at all?** Consulted by [`crate::server`] before any mutating
    /// command reaches the backing, and reflected on the wire in `TREE_CONNECT`'s maximal access
    /// and in the volume's `READ_ONLY_VOLUME` attribute, which is what makes macOS refuse a write
    /// client-side before it costs a round trip.
    fn writable(&self) -> bool;

    /// Open an existing file by its lower-cased ASCII name (no path separators; the share is
    /// flat), minting a [`FileId`] the caller must eventually [`Share::close`].
    fn open(&self, name: &[u8]) -> Result<FileId, Error>;

    /// The `index`th entry of the root, or `None` past the end. Index 0 upward, stable across a
    /// connection so `QUERY_DIRECTORY` can resume by index. The name borrows the share and is
    /// valid only until the next call on it (the fs-backed share resolves into one buffer).
    fn entry(&self, index: usize) -> Option<Entry<'_>>;

    /// The current size, in bytes, of an open file.
    fn size(&self, file: FileId) -> u64;

    /// Read from an open file at `offset` into `out`. `Ok(0)` is end of file; anything the
    /// backing refuses is an [`Error`], which is the distinction the read-only trait could not
    /// make.
    fn read(&self, file: FileId, offset: u64, out: &mut [u8]) -> Result<usize, Error>;

    /// Release a [`FileId`]. Infallible by design: a close that failed leaves the caller holding
    /// nothing it could do differently, and the id is the backing's bookkeeping.
    fn close(&self, file: FileId);

    /// Create a new file and open it. **Create is create**, matching `fs_proto::fs::CREATE`: an
    /// existing name is [`Error::Exists`] and nothing is modified, so the caller decides what
    /// create-or-open means rather than inheriting somebody's guess.
    fn create(&self, _name: &[u8]) -> Result<FileId, Error> {
        Err(Error::ReadOnly)
    }

    /// Write `data` to an open file at `offset`, returning the bytes taken (which may be short;
    /// the caller loops). A zero-length write is a legal no-op.
    fn write(&self, _file: FileId, _offset: u64, _data: &[u8]) -> Result<usize, Error> {
        Err(Error::ReadOnly)
    }

    /// Set an open file's size exactly, in both directions (`ftruncate`): shrinking discards the
    /// tail, growing extends with zeroes. The shrink is what makes "replace this file's contents"
    /// mean what a client thinks it means.
    fn truncate(&self, _file: FileId, _size: u64) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    /// Move a name to another name in the same (flat) share, replacing the destination if it
    /// exists. `SET_INFO`'s `FileRenameInformation`, which is how a client moves a file and how
    /// the temp-file-then-rename idiom lands.
    fn rename(&self, _from: &[u8], _to: &[u8]) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    /// Take a name out of the share. This is `unlink`: a file another handle still has open keeps
    /// reading (`fs_proto::fs::UNLINK` says so at length), which is what the delete-on-close and
    /// atomic-replace idioms rest on.
    fn remove(&self, _name: &[u8]) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
}

/// A share whose files are baked into the binary: the fixture the tests and the QEMU demo serve.
/// **Read-only**, and it is the trait's worked example of a backing that says so and then
/// implements nothing else.
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
              the fs_proto-backed share is what a real mount reads.\n",
        ),
    ],
};

impl Share for FixtureShare {
    fn writable(&self) -> bool {
        false
    }

    fn open(&self, name: &[u8]) -> Result<FileId, Error> {
        self.files
            .iter()
            .position(|(n, _)| *n == name)
            .map(|i| i as FileId)
            .ok_or(Error::NotFound)
    }

    fn entry(&self, index: usize) -> Option<Entry<'_>> {
        let (name, body) = self.files.get(index)?;
        Some(Entry {
            name,
            size: body.len() as u64,
            is_dir: false,
        })
    }

    fn size(&self, file: FileId) -> u64 {
        self.files
            .get(file as usize)
            .map_or(0, |(_, b)| b.len() as u64)
    }

    fn read(&self, file: FileId, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        let Some((_, body)) = self.files.get(file as usize) else {
            return Err(Error::NotFound);
        };
        let off = offset.min(body.len() as u64) as usize;
        let n = (body.len() - off).min(out.len());
        out[..n].copy_from_slice(&body[off..off + n]);
        Ok(n)
    }

    fn close(&self, _file: FileId) {}
}

/// **A writable share in memory**, for the host tests only.
///
/// It exists because the write path needs something to write *to* that is not a filesystem: the
/// fs-backed share lives in `smb_server` where the IPC is, and is reachable only from a booted
/// guest. This is the same argument [`FixtureShare`] makes for the read path, one direction over:
/// a backing that cannot be wrong, so a failing test is a protocol bug.
///
/// The interior mutability is `RefCell` rather than a lock because the trait takes `&self` (the
/// protocol machine holds no mutable share) and the tests are single-threaded. A real backing
/// mutates through IPC and needs no cell at all.
#[cfg(test)]
pub struct MemoryShare {
    files: core::cell::RefCell<Vec<(Vec<u8>, Vec<u8>)>>,
}

#[cfg(test)]
impl MemoryShare {
    /// A share holding `files`, in listing order.
    pub fn new(files: &[(&[u8], &[u8])]) -> Self {
        Self {
            files: core::cell::RefCell::new(
                files
                    .iter()
                    .map(|(n, b)| (n.to_vec(), b.to_vec()))
                    .collect(),
            ),
        }
    }

    /// What the share holds under `name`, for a test asserting a write landed. `None` if the name
    /// is gone, which is what a delete has to be checked against.
    pub fn contents(&self, name: &[u8]) -> Option<Vec<u8>> {
        self.files
            .borrow()
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.clone())
    }

    /// A removed file keeps its slot (so no live id shifts) and loses its name, so the empty name
    /// is never a match: it is the tombstone rather than a name a client could ask for.
    fn index(&self, name: &[u8]) -> Option<usize> {
        if name.is_empty() {
            return None;
        }
        self.files.borrow().iter().position(|(n, _)| n == name)
    }
}

/// The id is a **generation counter**, not an index, on purpose: a test that created a file and
/// then wrote through a handle minted before it would pass against an index-keyed share and fail
/// against a real one. Ids here are `slot + 1` of a stable side table, so nothing shifts.
#[cfg(test)]
impl Share for MemoryShare {
    fn writable(&self) -> bool {
        true
    }

    fn open(&self, name: &[u8]) -> Result<FileId, Error> {
        self.index(name)
            .map(|i| i as FileId + 1)
            .ok_or(Error::NotFound)
    }

    fn entry(&self, _index: usize) -> Option<Entry<'_>> {
        // The listing is not what this share is for, and an `Entry` borrows the share, which a
        // `RefCell` cannot hand out. `QUERY_DIRECTORY` is proven against `FIXTURE`.
        None
    }

    fn size(&self, file: FileId) -> u64 {
        self.files
            .borrow()
            .get(file as usize - 1)
            .map_or(0, |(_, b)| b.len() as u64)
    }

    fn read(&self, file: FileId, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        let files = self.files.borrow();
        let (_, body) = files.get(file as usize - 1).ok_or(Error::NotFound)?;
        let off = offset.min(body.len() as u64) as usize;
        let n = (body.len() - off).min(out.len());
        out[..n].copy_from_slice(&body[off..off + n]);
        Ok(n)
    }

    fn close(&self, _file: FileId) {}

    fn create(&self, name: &[u8]) -> Result<FileId, Error> {
        if self.index(name).is_some() {
            return Err(Error::Exists);
        }
        let mut files = self.files.borrow_mut();
        files.push((name.to_vec(), Vec::new()));
        Ok(files.len() as FileId)
    }

    fn write(&self, file: FileId, offset: u64, data: &[u8]) -> Result<usize, Error> {
        let mut files = self.files.borrow_mut();
        let (_, body) = files.get_mut(file as usize - 1).ok_or(Error::NotFound)?;
        let off = offset as usize;
        if body.len() < off + data.len() {
            body.resize(off + data.len(), 0);
        }
        body[off..off + data.len()].copy_from_slice(data);
        Ok(data.len())
    }

    fn truncate(&self, file: FileId, size: u64) -> Result<(), Error> {
        let mut files = self.files.borrow_mut();
        let (_, body) = files.get_mut(file as usize - 1).ok_or(Error::NotFound)?;
        body.resize(size as usize, 0);
        Ok(())
    }

    fn rename(&self, from: &[u8], to: &[u8]) -> Result<(), Error> {
        let src = self.index(from).ok_or(Error::NotFound)?;
        let dst = self.index(to);
        let mut files = self.files.borrow_mut();
        if let Some(dst) = dst {
            // Replace, matching fs_proto::fs::RENAME. The slot is emptied rather than removed so
            // no other file's id moves, which is exactly the property this share exists to hold.
            files[dst].0.clear();
        }
        files[src].0 = to.to_vec();
        Ok(())
    }

    fn remove(&self, name: &[u8]) -> Result<(), Error> {
        let i = self.index(name).ok_or(Error::NotFound)?;
        self.files.borrow_mut()[i].0.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_resolves_the_files_and_nothing_else() {
        assert_eq!(FIXTURE.open(b"hello.txt"), Ok(0));
        assert_eq!(FIXTURE.open(b"readme.md"), Ok(1));
        assert_eq!(FIXTURE.open(b"absent"), Err(Error::NotFound));
    }

    #[test]
    fn reads_are_bounded_by_eof_and_by_the_buffer() {
        let mut buf = [0u8; 8];
        assert_eq!(FIXTURE.read(0, 0, &mut buf), Ok(8));
        assert_eq!(&buf, b"nife ser");
        let n = FIXTURE.read(0, 8, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"ves SMB\n");
        assert_eq!(FIXTURE.read(0, FIXTURE.size(0), &mut buf), Ok(0), "at EOF");
        assert_eq!(FIXTURE.read(0, u64::MAX, &mut buf), Ok(0), "far past EOF");
    }

    #[test]
    fn the_listing_ends_and_matches_open() {
        let mut n = 0;
        while let Some(e) = FIXTURE.entry(n) {
            assert_eq!(FIXTURE.open(e.name), Ok(n as FileId));
            n += 1;
        }
        assert_eq!(n, FIXTURE.files.len());
    }

    /// The default write half is a refusal, so a backing that implements nothing cannot be
    /// written by accident. This is the second of the two lines the module header describes; the
    /// first (the protocol layer refusing before it asks) is proven in `server`'s tests.
    #[test]
    fn a_share_that_implements_nothing_refuses_every_mutation() {
        assert!(!FIXTURE.writable());
        assert_eq!(FIXTURE.create(b"new.txt"), Err(Error::ReadOnly));
        assert_eq!(FIXTURE.write(0, 0, b"x"), Err(Error::ReadOnly));
        assert_eq!(FIXTURE.truncate(0, 0), Err(Error::ReadOnly));
        assert_eq!(
            FIXTURE.rename(b"hello.txt", b"other.txt"),
            Err(Error::ReadOnly)
        );
        assert_eq!(FIXTURE.remove(b"hello.txt"), Err(Error::ReadOnly));
    }
}
