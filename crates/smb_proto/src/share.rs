//! **The seam between the SMB state machine and whatever holds the files.**
//!
//! Milestone 54's block names the shape: the adapter holds one directory capability and one
//! network endpoint. [`Share`] is the directory-capability side of that seam, drawn so the
//! protocol machine ([`crate::server`]) never knows what backs it: the host tests and the QEMU
//! gate serve a [`FixtureShare`] (files baked into the binary), and the fs_proto-backed
//! implementation, which walks a real directory capability into the FS server, implements this
//! same trait in the server program where the IPC lives.
//!
//! The model is deliberately smaller than a filesystem: a **tree of directories and files**,
//! readable always and writable when the backing says so. No timestamps (everything reports the
//! epoch), no attributes beyond file-versus-directory, no links. Each absence is a `BUGS`-grade
//! limitation of milestone 54's scope, not of the trait: growing the model is adding methods,
//! which is cheap next to the wire format.
//!
//! # What subdirectories changed here, and why (milestone 54's third act)
//!
//! The share was **flat** until this: every method took a bare name, [`Node`] had one directory in
//! it (`Root`), and `smb_proto::server` refused any name containing a separator. A Time Machine
//! sparsebundle is a directory of band files, so the flat model cannot hold the thing milestone 55
//! exists to serve. Three contract changes, and each one is the smallest that makes a tree
//! expressible:
//!
//! - **Every name-taking method takes a [`Path`] instead of a `&[u8]`.** Not a formality: a `Path`
//!   cannot be constructed without going through [`crate::path::Path::parse`], so a backing cannot
//!   be handed `..` even by an implementation that forgot to check. The traversal refusal is a wire
//!   decision and lives at the wire (see that module's header).
//! - **A directory is an opened object with an id**, [`DirId`], exactly as a file is. [`ROOT_DIR`]
//!   is the share root, which the backing neither mints nor closes, mirroring `fs_proto`'s
//!   `fs::ROOT` because the fs-backed share makes them literally the same number.
//! - **[`Share::entry`] takes the directory to list.** It listed "the share" before, which was
//!   expressible only because there was one directory.
//!
//! [`Share::mkdir`] and [`Share::rmdir`] are the two new verbs, and they are separate from
//! [`Share::create`] and [`Share::remove`] rather than folded into them for `fs_proto`'s reason: a
//! call that removes whatever it finds is how one word destroys a subtree, and this contract does
//! not offer it at any name.
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

use crate::path::Path;

/// **What a share says when it cannot do what was asked.** The error channel the read-only trait
/// lacked; [`crate::server::status_for`] is the one place these become NT statuses, so a client's
/// retry logic keys on one mapping rather than on whatever each call site reached for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No such name in the directory the path's parent names.
    NotFound,
    /// A component of the path before the last is not there, or is not a directory. Distinct from
    /// [`Error::NotFound`] because it becomes a different NT status: a client that meets "path not
    /// found" knows to create the parent, and one that meets "name not found" knows the parent is
    /// there. The flat share could not tell them apart because it had one directory.
    PathNotFound,
    /// The node is a file and the operation is a directory's: listing it, descending into it, or
    /// `RMDIR`. The mirror of [`Error::IsDirectory`], and it exists for the same reason `fs_proto`
    /// has both.
    NotDirectory,
    /// The directory is not empty, and this contract removes only empty ones. The recursion lives
    /// in the client, one refusable step at a time, exactly as `fs_proto::fs::RMDIR` argues.
    NotEmpty,
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

/// **A directory's identity**, minted by the backing at [`Share::open_dir`] or [`Share::mkdir`] and
/// released by [`Share::close_dir`]. The same shape as [`FileId`] and deliberately a separate type
/// alias: they are two namespaces, and the fs-backed share draws both from the FS server's one
/// handle table only because that server issues one number space for both kinds.
pub type DirId = u64;

/// **The share's root directory.** Never minted, never closed, and always openable: it is the
/// directory the share *is*, so a backing that has nothing else still has this.
///
/// Zero because `fs_proto::fs::ROOT` is zero, which makes the fs-backed share's mapping the
/// identity function rather than an off-by-one waiting to happen.
pub const ROOT_DIR: DirId = 0;

/// A node in the share: an open directory, or one open file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    /// One open directory, by the id its backing minted. [`ROOT_DIR`] is the share's own root.
    Dir(DirId),
    /// One open file, by the id its backing minted.
    File(FileId),
}

impl Node {
    /// The share's root directory as a node, which is what a CREATE of the share itself opens.
    pub const ROOT: Node = Node::Dir(ROOT_DIR);

    /// Is this a directory? The one attribute the model carries, asked at every response that
    /// reports file attributes.
    pub const fn is_dir(&self) -> bool {
        matches!(self, Node::Dir(_))
    }
}

/// **What a volume reports about itself**: the answer to `fs_proto::fs::STATFS`, carried through
/// the seam so `FileFsSizeInformation` and `FileFsFullSizeInformation` can stop being a constant.
///
/// A backing that cannot ask answers `None` from [`Share::statfs`] and the protocol layer falls
/// back to [`NOMINAL_VOLUME_BYTES`], which is what the fixture does: it has no volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Volume {
    /// The allocation unit in bytes. Reported to the client as its own unit rather than converted,
    /// so the client's arithmetic lands on real boundaries.
    pub block_size: u64,
    /// The volume's size in those units.
    pub total_blocks: u64,
    /// How many of them are free.
    pub free_blocks: u64,
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

/// **The volume figure a backing with no volume reports**, and it is nominal.
///
/// `FileFsSizeInformation` and `FileFsFullSizeInformation` make a client decide whether to start
/// writing, and macOS will not write to a volume reporting zero free space. A backing that can ask
/// its store answers [`Share::statfs`] with a [`Volume`] and this constant is not consulted; the
/// fs-backed share does exactly that as of milestone 54's `fs_proto::fs::STATFS`.
///
/// What is left is the case with no honest answer: [`FixtureShare`] is files baked into a binary
/// and has no volume at all. Reporting this figure for it is a stated approximation rather than a
/// silent one, and the crate `BUGS` names it where a reader meets the feature.
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

    /// Open an existing **file** at `path`, minting a [`FileId`] the caller must eventually
    /// [`Share::close`]. [`Error::IsDirectory`] if the path names a directory;
    /// [`Share::open_dir`] is the call for that.
    fn open(&self, path: Path<'_>) -> Result<FileId, Error>;

    /// **Open an existing directory**, minting a [`DirId`] the caller must eventually
    /// [`Share::close_dir`]. [`Error::NotDirectory`] if the path names a file.
    ///
    /// The root is [`ROOT_DIR`] and needs no backing call, so the protocol layer answers it
    /// without asking; a backing may still be asked for it and must answer [`ROOT_DIR`].
    ///
    /// Required rather than defaulted, unlike the write half: a share with no directories to
    /// descend into is still a share whose root can be listed, and a default returning
    /// [`Error::NotFound`] would let a backing forget to implement descent and look like an empty
    /// tree instead of an unfinished one.
    fn open_dir(&self, path: Path<'_>) -> Result<DirId, Error>;

    /// Release a [`DirId`]. Infallible for [`Share::close`]'s reason, and never called with
    /// [`ROOT_DIR`]: the root is not something anyone opened.
    fn close_dir(&self, dir: DirId);

    /// The `index`th entry of `dir`, or `None` past the end. Index 0 upward, stable across a
    /// connection so `QUERY_DIRECTORY` can resume by index. The name borrows the share and is
    /// valid only until the next call on it (the fs-backed share resolves into one buffer).
    fn entry(&self, dir: DirId, index: usize) -> Option<Entry<'_>>;

    /// **What the volume behind this share reports**, or `None` when the backing cannot ask. The
    /// protocol layer falls back to [`NOMINAL_VOLUME_BYTES`] on `None`, and says so in the crate
    /// BUGS; a backing that can ask must, because macOS sizes a Time Machine sparsebundle against
    /// this answer.
    ///
    /// Defaulted to `None` rather than required: a backing with no volume (the fixture is baked
    /// into a binary) has no honest number, and inventing one is what this method exists to stop.
    fn statfs(&self) -> Option<Volume> {
        None
    }

    /// The current size, in bytes, of an open file.
    fn size(&self, file: FileId) -> u64;

    /// Read from an open file at `offset` into `out`. `Ok(0)` is end of file; anything the
    /// backing refuses is an [`Error`], which is the distinction the read-only trait could not
    /// make.
    fn read(&self, file: FileId, offset: u64, out: &mut [u8]) -> Result<usize, Error>;

    /// Release a [`FileId`]. Infallible by design: a close that failed leaves the caller holding
    /// nothing it could do differently, and the id is the backing's bookkeeping.
    fn close(&self, file: FileId);

    /// Create a new file at `path` and open it. **Create is create**, matching
    /// `fs_proto::fs::CREATE`: an existing name is [`Error::Exists`] and nothing is modified, so
    /// the caller decides what create-or-open means rather than inheriting somebody's guess. A
    /// missing parent directory is [`Error::PathNotFound`]; this call makes one name, never a
    /// chain of them.
    fn create(&self, _path: Path<'_>) -> Result<FileId, Error> {
        Err(Error::ReadOnly)
    }

    /// **Make a child directory and open it**, matching `fs_proto::fs::MKDIR` down to its
    /// refusals: [`Error::Exists`] for a name already taken and [`Error::PathNotFound`] for a
    /// missing parent. One level per call, for [`Share::create`]'s reason.
    ///
    /// It hands back a [`DirId`] rather than nothing because SMB's CREATE with
    /// `FILE_DIRECTORY_FILE` both creates and opens in one message, and a create-then-open pair
    /// would be two chances to lose the race with another client.
    fn mkdir(&self, _path: Path<'_>) -> Result<DirId, Error> {
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

    /// Move a path to another path, replacing the destination if it exists.  `SET_INFO`'s
    /// `FileRenameInformation`, which is how a client moves a file and how the
    /// temp-file-then-rename idiom lands.
    ///
    /// **A directory may be renamed in place but not moved into another directory**, and the
    /// answer is [`Error::Io`]'s status via the caller. That is `fs_proto::fs::RENAME`'s own
    /// boundary, drawn there because the guard against making a directory its own descendant is an
    /// ancestry walk in a server whose stack is measured at three quarters used. The limitation
    /// surfaces here rather than being hidden, per DECISIONS §42.
    fn rename(&self, _from: Path<'_>, _to: Path<'_>) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    /// Take a **file's** name out of the share. This is `unlink`: a file another handle still has
    /// open keeps reading (`fs_proto::fs::UNLINK` says so at length), which is what the
    /// delete-on-close and atomic-replace idioms rest on. A directory is [`Error::IsDirectory`].
    fn remove(&self, _path: Path<'_>) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    /// Remove an **empty** directory, `fs_proto::fs::RMDIR`. A non-empty one is
    /// [`Error::NotEmpty`], refused rather than emptied, and a file is [`Error::NotDirectory`].
    ///
    /// Empty-only is the safety property rather than a limitation: `rm -r`'s recursion belongs in
    /// whoever is deleting, as a loop of individually refusable steps, so a removal stops exactly
    /// where the capabilities stop. A call that removed whatever it found would put a subtree
    /// behind one message and no check afterwards could undo it.
    fn rmdir(&self, _path: Path<'_>) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
}

/// **A share whose tree is baked into the binary**: the fixture the tests and the QEMU demo serve.
/// **Read-only**, and it is the trait's worked example of a backing that says so and then
/// implements nothing else.
///
/// It holds two flat tables rather than a tree of nodes, and that is the whole trick: a directory's
/// [`DirId`] is its index in [`FixtureShare::dirs`], and a listing is a filter over both tables for
/// the entries whose parent path matches. Linear per call and irrelevant at this size; what it buys
/// is that the fixture cannot be wrong in an interesting way, which is why a failing protocol test
/// is a protocol bug.
pub struct FixtureShare {
    /// Directory paths, indexed by [`DirId`]. **Index 0 must be the root**, the empty path.
    pub dirs: &'static [&'static [u8]],
    /// `(path, contents)` pairs; paths lower-case ASCII with `\` separators, listing order.
    pub files: &'static [(&'static [u8], &'static [u8])],
}

/// The fixture every test and the QEMU boot serve, so the host prober, the host tests, and the
/// notes' worked example all read the same bytes.
///
/// One file's contents are asserted end to end; the second exists so a directory listing has more
/// than one row; and **`bands\` exists so nesting is served by the no-disk boot too**, which is
/// what makes a subdirectory bug findable without a RedoxFS image attached. Its name is the
/// sparsebundle's, because that is the shape milestone 55 has to carry.
pub const FIXTURE: FixtureShare = FixtureShare {
    dirs: &[b"", b"bands"],
    files: &[
        (b"hello.txt", b"nife serves SMB\n"),
        (
            b"readme.md",
            b"This share is served by nife's smb_server through the socket contract.\n\
              It is read-only and everything in it is baked into the server binary;\n\
              the fs_proto-backed share is what a real mount reads.\n",
        ),
        (b"bands\\0", b"a band file, one directory down\n"),
    ],
};

impl FixtureShare {
    /// The path a [`DirId`] names, or `None` for an id this fixture never minted.
    fn dir_path(&self, dir: DirId) -> Option<&'static [u8]> {
        self.dirs.get(dir as usize).copied()
    }

    /// The last component of a path: the name a listing shows.
    fn leaf(path: &'static [u8]) -> &'static [u8] {
        match path.iter().rposition(|&b| b == crate::path::SEP) {
            Some(i) => &path[i + 1..],
            None => path,
        }
    }

    /// Does the path's parent directory exist? What separates [`Error::PathNotFound`] from
    /// [`Error::NotFound`], which a flat share could not tell apart because it had one directory.
    fn parent_exists(&self, path: Path<'_>) -> bool {
        let parent = path.parent();
        parent.is_root() || self.dirs.iter().any(|d| *d == parent.as_bytes())
    }

    /// Is `path` directly inside `parent` (not merely below it)? The listing's membership test, and
    /// the reason `bands\0` does not show up in the root's listing.
    fn child_of(path: &[u8], parent: &[u8]) -> bool {
        match path.iter().rposition(|&b| b == crate::path::SEP) {
            Some(i) => &path[..i] == parent,
            None => parent.is_empty(),
        }
    }
}

impl Share for FixtureShare {
    fn writable(&self) -> bool {
        false
    }

    fn open(&self, path: Path<'_>) -> Result<FileId, Error> {
        if self.dirs.iter().any(|d| *d == path.as_bytes()) {
            return Err(Error::IsDirectory);
        }
        if !self.parent_exists(path) {
            return Err(Error::PathNotFound);
        }
        self.files
            .iter()
            .position(|(p, _)| *p == path.as_bytes())
            .map(|i| i as FileId)
            .ok_or(Error::NotFound)
    }

    fn open_dir(&self, path: Path<'_>) -> Result<DirId, Error> {
        if self.files.iter().any(|(p, _)| *p == path.as_bytes()) {
            return Err(Error::NotDirectory);
        }
        if !self.parent_exists(path) {
            return Err(Error::PathNotFound);
        }
        self.dirs
            .iter()
            .position(|d| *d == path.as_bytes())
            .map(|i| i as DirId)
            .ok_or(Error::NotFound)
    }

    fn close_dir(&self, _dir: DirId) {}

    fn entry(&self, dir: DirId, index: usize) -> Option<Entry<'_>> {
        let parent = self.dir_path(dir)?;
        // Directories first, then files, each in table order, so an index means the same thing on
        // every call of one enumeration. The root itself is never its own child.
        let dirs = self
            .dirs
            .iter()
            .skip(1)
            .filter(|d| Self::child_of(d, parent))
            .map(|d| Entry {
                name: Self::leaf(d),
                size: 0,
                is_dir: true,
            });
        let files = self
            .files
            .iter()
            .filter(|(p, _)| Self::child_of(p, parent))
            .map(|(p, body)| Entry {
                name: Self::leaf(p),
                size: body.len() as u64,
                is_dir: false,
            });
        dirs.chain(files).nth(index)
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
///
/// **Paths are leaked**, which is what lets a listing hand out a name that outlives the `RefCell`
/// borrow it came from. That is a test fixture's licence and nothing else's: the alternative is
/// `entry` returning `None` forever, which is what this share did before subdirectories existed
/// and which left "create a directory, then list it" provable nowhere.
#[cfg(test)]
pub struct MemoryShare {
    /// `(path, contents)`; `None` contents means the node is a directory. A slot is never removed,
    /// so no live id moves; a removed node's path is cleared and the empty path never matches.
    nodes: core::cell::RefCell<Vec<MemoryNode>>,
}

/// One node of a [`MemoryShare`]: its path, and its bytes unless it is a directory.
#[cfg(test)]
type MemoryNode = (&'static [u8], Option<Vec<u8>>);

#[cfg(test)]
impl MemoryShare {
    /// A share holding `files` (flat or nested paths) plus `dirs`, in listing order. Every parent
    /// a path needs must be in `dirs`: this fixture makes no directory it was not told to.
    pub fn new(dirs: &[&[u8]], files: &[(&[u8], &[u8])]) -> Self {
        let mut nodes: Vec<MemoryNode> = Vec::new();
        for d in dirs {
            nodes.push((Self::leak(d), None));
        }
        for (p, b) in files {
            nodes.push((Self::leak(p), Some(b.to_vec())));
        }
        Self {
            nodes: core::cell::RefCell::new(nodes),
        }
    }

    /// See the struct doc: a test fixture's leak, so a listing can hand out a stable name.
    fn leak(p: &[u8]) -> &'static [u8] {
        Box::leak(p.to_vec().into_boxed_slice())
    }

    /// What the share holds under `path`, for a test asserting a write landed. `None` if the name
    /// is gone, which is what a delete has to be checked against.
    pub fn contents(&self, path: &[u8]) -> Option<Vec<u8>> {
        let i = self.index(path)?;
        self.nodes.borrow()[i].1.clone()
    }

    /// Does the share hold a directory at `path`?
    pub fn has_dir(&self, path: &[u8]) -> bool {
        self.index(path)
            .is_some_and(|i| self.nodes.borrow()[i].1.is_none())
    }

    /// The slot a path names. A removed node keeps its slot and loses its name, so the empty path
    /// is never a match: it is the tombstone rather than a name a client could ask for.
    fn index(&self, path: &[u8]) -> Option<usize> {
        if path.is_empty() {
            return None;
        }
        self.nodes.borrow().iter().position(|(p, _)| *p == path)
    }

    /// The slot a [`DirId`] or [`FileId`] names. Ids are `slot + 1` so that 0 stays [`ROOT_DIR`].
    fn slot(id: u64) -> usize {
        id as usize - 1
    }

    /// The path a directory id names, [`ROOT_DIR`] included.
    fn dir_path(&self, dir: DirId) -> Option<&'static [u8]> {
        if dir == ROOT_DIR {
            return Some(b"");
        }
        let nodes = self.nodes.borrow();
        let (path, body) = nodes.get(Self::slot(dir))?;
        body.is_none().then_some(*path)
    }

    /// The parent of `path` must exist and be a directory, or the caller answers
    /// [`Error::PathNotFound`]. One level per call: this fixture makes no directory chains.
    fn parent_exists(&self, path: Path<'_>) -> bool {
        let parent = path.parent();
        parent.is_root()
            || self
                .index(parent.as_bytes())
                .is_some_and(|i| self.nodes.borrow()[i].1.is_none())
    }

    /// Does anything live directly inside `path`? What `rmdir`'s empty-only refusal asks.
    fn has_children(&self, path: &[u8]) -> bool {
        self.nodes
            .borrow()
            .iter()
            .any(|(p, _)| !p.is_empty() && FixtureShare::child_of(p, path))
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

    fn open(&self, path: Path<'_>) -> Result<FileId, Error> {
        if !self.parent_exists(path) {
            return Err(Error::PathNotFound);
        }
        let i = self.index(path.as_bytes()).ok_or(Error::NotFound)?;
        if self.nodes.borrow()[i].1.is_none() {
            return Err(Error::IsDirectory);
        }
        Ok(i as FileId + 1)
    }

    fn open_dir(&self, path: Path<'_>) -> Result<DirId, Error> {
        if path.is_root() {
            return Ok(ROOT_DIR);
        }
        if !self.parent_exists(path) {
            return Err(Error::PathNotFound);
        }
        let i = self.index(path.as_bytes()).ok_or(Error::NotFound)?;
        if self.nodes.borrow()[i].1.is_some() {
            return Err(Error::NotDirectory);
        }
        Ok(i as DirId + 1)
    }

    fn close_dir(&self, _dir: DirId) {}

    fn entry(&self, dir: DirId, index: usize) -> Option<Entry<'_>> {
        let parent = self.dir_path(dir)?;
        let nodes = self.nodes.borrow();
        let (path, body) = nodes
            .iter()
            .filter(|(p, _)| !p.is_empty() && FixtureShare::child_of(p, parent))
            .nth(index)?;
        Some(Entry {
            name: FixtureShare::leaf(path),
            size: body.as_ref().map_or(0, |b| b.len() as u64),
            is_dir: body.is_none(),
        })
    }

    fn statfs(&self) -> Option<Volume> {
        // A made-up but *moving* volume: the free count falls as the share fills, so a test can
        // tell a real answer from a constant, which is the whole distinction STATFS exists to make.
        let used: u64 = self
            .nodes
            .borrow()
            .iter()
            .map(|(_, b)| b.as_ref().map_or(0, |b| b.len() as u64).div_ceil(512))
            .sum();
        Some(Volume {
            block_size: 512,
            total_blocks: 2048,
            free_blocks: 2048 - used.min(2048),
        })
    }

    fn size(&self, file: FileId) -> u64 {
        self.nodes
            .borrow()
            .get(Self::slot(file))
            .and_then(|(_, b)| b.as_ref())
            .map_or(0, |b| b.len() as u64)
    }

    fn read(&self, file: FileId, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        let nodes = self.nodes.borrow();
        let (_, body) = nodes.get(Self::slot(file)).ok_or(Error::NotFound)?;
        let body = body.as_ref().ok_or(Error::IsDirectory)?;
        let off = offset.min(body.len() as u64) as usize;
        let n = (body.len() - off).min(out.len());
        out[..n].copy_from_slice(&body[off..off + n]);
        Ok(n)
    }

    fn close(&self, _file: FileId) {}

    fn create(&self, path: Path<'_>) -> Result<FileId, Error> {
        if !self.parent_exists(path) {
            return Err(Error::PathNotFound);
        }
        if self.index(path.as_bytes()).is_some() {
            return Err(Error::Exists);
        }
        let mut nodes = self.nodes.borrow_mut();
        nodes.push((Self::leak(path.as_bytes()), Some(Vec::new())));
        Ok(nodes.len() as FileId)
    }

    fn mkdir(&self, path: Path<'_>) -> Result<DirId, Error> {
        if !self.parent_exists(path) {
            return Err(Error::PathNotFound);
        }
        if self.index(path.as_bytes()).is_some() {
            return Err(Error::Exists);
        }
        let mut nodes = self.nodes.borrow_mut();
        nodes.push((Self::leak(path.as_bytes()), None));
        Ok(nodes.len() as DirId)
    }

    fn write(&self, file: FileId, offset: u64, data: &[u8]) -> Result<usize, Error> {
        let mut nodes = self.nodes.borrow_mut();
        let (_, body) = nodes.get_mut(Self::slot(file)).ok_or(Error::NotFound)?;
        let body = body.as_mut().ok_or(Error::IsDirectory)?;
        let off = offset as usize;
        if body.len() < off + data.len() {
            body.resize(off + data.len(), 0);
        }
        body[off..off + data.len()].copy_from_slice(data);
        Ok(data.len())
    }

    fn truncate(&self, file: FileId, size: u64) -> Result<(), Error> {
        let mut nodes = self.nodes.borrow_mut();
        let (_, body) = nodes.get_mut(Self::slot(file)).ok_or(Error::NotFound)?;
        body.as_mut()
            .ok_or(Error::IsDirectory)?
            .resize(size as usize, 0);
        Ok(())
    }

    fn rename(&self, from: Path<'_>, to: Path<'_>) -> Result<(), Error> {
        if !self.parent_exists(to) {
            return Err(Error::PathNotFound);
        }
        let src = self.index(from.as_bytes()).ok_or(Error::NotFound)?;
        let dst = self.index(to.as_bytes());
        let leaked = Self::leak(to.as_bytes());
        let mut nodes = self.nodes.borrow_mut();
        if let Some(dst) = dst {
            // Replace, matching fs_proto::fs::RENAME. The slot is emptied rather than removed so
            // no other node's id moves, which is exactly the property this share exists to hold.
            nodes[dst].0 = b"";
        }
        nodes[src].0 = leaked;
        Ok(())
    }

    fn remove(&self, path: Path<'_>) -> Result<(), Error> {
        let i = self.index(path.as_bytes()).ok_or(Error::NotFound)?;
        let mut nodes = self.nodes.borrow_mut();
        if nodes[i].1.is_none() {
            return Err(Error::IsDirectory);
        }
        nodes[i].0 = b"";
        Ok(())
    }

    fn rmdir(&self, path: Path<'_>) -> Result<(), Error> {
        let i = self.index(path.as_bytes()).ok_or(Error::NotFound)?;
        if self.nodes.borrow()[i].1.is_some() {
            return Err(Error::NotDirectory);
        }
        if self.has_children(path.as_bytes()) {
            return Err(Error::NotEmpty);
        }
        self.nodes.borrow_mut()[i].0 = b"";
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path a test spells inline. Panics on a malformed one, which is what a test wants: a
    /// fixture that cannot be parsed is a bug in the test, not a case under test.
    fn p(raw: &[u8]) -> Path<'_> {
        Path::parse(raw).expect("the test spelled a malformed path")
    }

    #[test]
    fn open_resolves_the_files_and_nothing_else() {
        assert_eq!(FIXTURE.open(p(b"hello.txt")), Ok(0));
        assert_eq!(FIXTURE.open(p(b"readme.md")), Ok(1));
        assert_eq!(FIXTURE.open(p(b"bands\\0")), Ok(2));
        assert_eq!(FIXTURE.open(p(b"absent")), Err(Error::NotFound));
        // A directory is not openable as a file, and the mirror holds too.
        assert_eq!(FIXTURE.open(p(b"bands")), Err(Error::IsDirectory));
        assert_eq!(FIXTURE.open_dir(p(b"hello.txt")), Err(Error::NotDirectory));
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

    /// **A listing shows what is directly inside a directory and nothing below it.** That is the
    /// one property a flat listing could not have had, and getting it wrong would show a client
    /// `bands\0` in the share root, which no client can then open.
    #[test]
    fn a_listing_shows_one_level_and_the_root_does_not_show_grandchildren() {
        let root = FIXTURE.open_dir(Path::ROOT).unwrap();
        assert_eq!(root, ROOT_DIR, "the root is the id every backing agrees on");
        let mut rows = Vec::new();
        let mut i = 0;
        while let Some(e) = FIXTURE.entry(root, i) {
            rows.push((e.name.to_vec(), e.is_dir));
            i += 1;
        }
        assert_eq!(
            rows,
            [
                (b"bands".to_vec(), true),
                (b"hello.txt".to_vec(), false),
                (b"readme.md".to_vec(), false),
            ],
            "the root lists its own children, directories first, and not `bands\\0`",
        );

        let bands = FIXTURE.open_dir(p(b"bands")).unwrap();
        let e = FIXTURE.entry(bands, 0).expect("bands holds one file");
        assert_eq!(e.name, b"0", "a listing shows the leaf, not the whole path");
        assert!(!e.is_dir);
        assert_eq!(e.size, FIXTURE.size(FIXTURE.open(p(b"bands\\0")).unwrap()));
        assert!(FIXTURE.entry(bands, 1).is_none(), "and then it ends");

        // An id nothing minted lists nothing rather than panicking or aliasing a real directory.
        assert!(FIXTURE.entry(99, 0).is_none());
    }

    /// The default write half is a refusal, so a backing that implements nothing cannot be
    /// written by accident. This is the second of the two lines the module header describes; the
    /// first (the protocol layer refusing before it asks) is proven in `server`'s tests.
    #[test]
    fn a_share_that_implements_nothing_refuses_every_mutation() {
        assert!(!FIXTURE.writable());
        assert_eq!(FIXTURE.create(p(b"new.txt")), Err(Error::ReadOnly));
        assert_eq!(FIXTURE.mkdir(p(b"new")), Err(Error::ReadOnly));
        assert_eq!(FIXTURE.write(0, 0, b"x"), Err(Error::ReadOnly));
        assert_eq!(FIXTURE.truncate(0, 0), Err(Error::ReadOnly));
        assert_eq!(
            FIXTURE.rename(p(b"hello.txt"), p(b"other.txt")),
            Err(Error::ReadOnly)
        );
        assert_eq!(FIXTURE.remove(p(b"hello.txt")), Err(Error::ReadOnly));
        assert_eq!(FIXTURE.rmdir(p(b"bands")), Err(Error::ReadOnly));
    }

    /// **A backing with no volume says so**, and the protocol layer is what decides what to report
    /// instead. Inventing a number here is exactly what `NOMINAL_VOLUME_BYTES`'s doc argues
    /// against.
    #[test]
    fn a_share_with_no_volume_answers_none_rather_than_guessing() {
        assert_eq!(FIXTURE.statfs(), None);
        let m = MemoryShare::new(&[], &[]);
        let v = m.statfs().expect("a share that can ask, answers");
        assert_eq!(v.free_blocks, v.total_blocks, "an empty share is all free");
        m.create(p(b"big")).unwrap();
        m.write(1, 0, &[0u8; 4096]).unwrap();
        assert!(
            m.statfs().unwrap().free_blocks < v.free_blocks,
            "free space must move, or it is the constant this verb replaces",
        );
    }

    /// **Making a directory and then listing it**, which is the sequence a client performs and
    /// which nothing could prove while the writable fixture had no listing at all.
    #[test]
    fn a_made_directory_holds_what_is_put_in_it() {
        let m = MemoryShare::new(&[], &[]);
        let dir = m.mkdir(p(b"backup")).unwrap();
        assert!(m.has_dir(b"backup"));
        assert_eq!(
            m.mkdir(p(b"backup")),
            Err(Error::Exists),
            "create is create"
        );

        // One level per call: a chain is the client's job, and the refusal says which half failed.
        assert_eq!(
            m.create(p(b"nowhere\\file")),
            Err(Error::PathNotFound),
            "a missing parent is PathNotFound, not NotFound",
        );

        let f = m.create(p(b"backup\\band0")).unwrap();
        m.write(f, 0, b"payload").unwrap();
        let e = m.entry(dir, 0).expect("the made directory lists its child");
        assert_eq!(e.name, b"band0");
        assert_eq!(e.size, 7);
        assert!(m.entry(dir, 1).is_none());
        // And the root shows the directory, not the file inside it.
        let root: Vec<_> = (0..)
            .map_while(|i| m.entry(ROOT_DIR, i).map(|e| (e.name.to_vec(), e.is_dir)))
            .collect();
        assert_eq!(root, [(b"backup".to_vec(), true)]);

        // rmdir is empty-only, and the refusal is a fact about the directory.
        assert_eq!(m.rmdir(p(b"backup")), Err(Error::NotEmpty));
        assert_eq!(m.remove(p(b"backup")), Err(Error::IsDirectory));
        assert_eq!(m.rmdir(p(b"backup\\band0")), Err(Error::NotDirectory));
        m.remove(p(b"backup\\band0")).unwrap();
        m.rmdir(p(b"backup")).unwrap();
        assert!(!m.has_dir(b"backup"));
    }

    /// A handle minted before the share changed keeps naming what it named. The write path's
    /// property, restated for directories: creating a file must not move a live `DirId`.
    #[test]
    fn an_id_survives_the_share_growing_under_it() {
        let m = MemoryShare::new(&[b"a"], &[(b"a\\one", b"1")]);
        let dir = m.open_dir(p(b"a")).unwrap();
        let file = m.open(p(b"a\\one")).unwrap();
        m.create(p(b"a\\two")).unwrap();
        m.mkdir(p(b"b")).unwrap();
        assert_eq!(m.size(file), 1, "the file id still names the same file");
        assert_eq!(
            m.entry(dir, 0).unwrap().name,
            b"one",
            "the directory id still names the same directory",
        );
    }
}
