//! **The FS-server core: RedoxFS behind a capability-shaped file service** (milestone 32 phase 2).
//!
//! This is the sans-IO heart of the FS server, in the `line_editor` spirit: the filesystem logic
//! with no IPC and no device in it, so it is host-tested in milliseconds against a real RedoxFS
//! image and only the wiring needs QEMU. The EL0 binary (this crate's `el0` build) wraps a
//! [`Server`] in the block-IPC [`redoxfs::Disk`] and the file-service serve loop; everything the
//! filesystem actually *does* lives here.
//!
//! # The contract, capability-shaped from birth
//!
//! A [`Server`] is opened **bound to one directory** ([`Server::open`] binds the RedoxFS root).
//! Every name a client presents is resolved *under a directory handle*: there is no absolute path,
//! no `..` escape, no global namespace. In the running system the client reaches this server only by
//! holding an endpoint the server reads on, and that endpoint IS the directory capability; a client
//! without it can open nothing. A successful open returns a **handle**, a small integer this server
//! minted and will validate against its own table, which is itself a capability: forging one is
//! meaningless because the server honors only the handles it issued. See notes/fs-server.md.
//!
//! # The directory capability (milestone 47)
//!
//! The bound directory is [`fs_proto::fs::ROOT`], handle 0, an ordinary entry in the same table, so
//! every name-taking verb names a directory rather than resolving against a hidden field. A
//! directory handle carries a [`fs_proto::dir::Rights`] set, [`Server::open_dir`] hands back a
//! handle to a child, and a child's rights are the parent's intersected with what was asked for.
//! **That intersection is the only constructor for a non-root rights set**, so no descendant can
//! carry a right its ancestor lacked, at any depth, without a check anyone could forget. A file
//! handle inherits its directory's read and write bits, so what may be done to a file was decided
//! when the directory was granted. See notes/dir-capability.md.
//!
//! **What this table is not.** It is per *server*, not per client, so two clients sharing one
//! endpoint share these handles and a rights-carrying handle attenuates only its holder. The handle
//! is the authority; the **endpoint** is the boundary. Confining a program to a subtree is
//! therefore still a caretaker process holding the wider endpoint
//! (`user/src/fs_subtree_caretaker.rs`), for exactly the reason `fs_file_caretaker` is one.
//!
//! # The error boundary
//!
//! Every method here returns `syscall::error::Result`, RedoxFS's own error type, unmapped. The
//! translation to the wire (a negated errno, `fs_proto::reply_err`) happens once, in the serve loop
//! at the process boundary, and nowhere else. Keeping the core in RedoxFS's error vocabulary is what
//! makes that rule enforceable: there is no ABI type in here to leak.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

// Crash injection (milestone 37). Host-only: it exists to damage an in-memory image the way a dying
// machine damages a platter, and the EL0 binary has no business carrying it. Gated on `hosttest`
// rather than `test` so an integration test in `tests/` can reach it.
#[cfg(feature = "hosttest")]
pub mod crash;

use alloc::vec::Vec;

use fs_proto::dir::{self, Rights};
use fs_proto::xattr;
use redoxfs::{Disk, FileSystem, Node, Transaction, TreePtr};
use syscall::error::{
    EBADF, EEXIST, EINVAL, EIO, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY, EPERM, EROFS, Error, Result,
};

/// What one handle names, and what may be done through it.
///
/// The rights ride on the handle rather than on the session because that is what makes descending
/// mean anything: a child directory handle is a *different* authority from its parent, held at the
/// same time by the same client, and a file opened under it inherits its direction. A session-wide
/// rights field could not express any of that.
#[derive(Clone, Copy)]
enum Entry {
    /// A directory. Its rights are the full ladder.
    Dir(TreePtr<Node>, Rights),
    /// A file, carrying the [`dir::READ`]/[`dir::WRITE`] bits of the directory it was opened under.
    /// The other rungs are meaningless on a file and are simply never asked about.
    File(TreePtr<Node>, Rights),
}

/// A file service over one RedoxFS image on one [`Disk`]. Generic over the disk so the host tests
/// drive a `DiskMemory`/`DiskFile` and the EL0 binary drives the block-IPC client, with identical
/// filesystem code between them.
pub struct Server<D: Disk> {
    fs: FileSystem<D>,
    /// The open-handle table. `handles[h]` is what a handle names, or `None` for a freed slot; the
    /// index is the handle. A capability the server issues and checks, never trusts.
    ///
    /// **Slot 0 is the bound directory** ([`fs_proto::fs::ROOT`]), installed by [`Server::open`]
    /// before anything is served and never closeable. It exists so the directory an endpoint
    /// designates is an ordinary handle: every name resolves under a handle, and there is no
    /// separate "the current directory" field for a verb to forget to consult.
    handles: Vec<Option<Entry>>,
    /// A monotonically increasing stand-in for wall-clock seconds, so a write advances the node's
    /// mtime deterministically. The server has no RTC; the value only needs to move forward, and the
    /// host tests assert on file *contents*, not timestamps.
    clock: u64,
}

impl<D: Disk> Server<D> {
    /// Open an existing RedoxFS image and bind the service to its root directory.
    ///
    /// **What recovers a crashed image, precisely** (a correction, milestone 37). This used to say
    /// that `cleanup: true` "replays the header ring to the newest consistent generation". It does
    /// not, and the distinction matters because it names the wrong mechanism. `FileSystem::open`
    /// scans all 256 header-ring slots **unconditionally**, keeps the newest one whose seahash still
    /// checks out, and ignores the rest; that scan is the crash recovery, and it happens whatever
    /// `cleanup` is. What `cleanup: true` adds is a tidy-up on top: release the nodes nothing points
    /// at any more, and commit. The server passes it because a mount should not leak, not because
    /// the recovery depends on it.
    ///
    /// The recovery is measured rather than described: `tests/crash_consistency.rs` cuts the device
    /// at every point in a workload, tears the write it was in the middle of, and mounts the result
    /// through this function. See [`crate::crash`] and DECISIONS §34.
    ///
    /// The server never opens with the creation APIs: those are std-gated and stay host-side (the
    /// server opens, it never makes).
    pub fn open(disk: D) -> Result<Self> {
        let fs = FileSystem::open(disk, None, None, true)?;
        Ok(Self {
            fs,
            // Handle 0 is the bound directory, and it carries every right: this server IS the whole
            // filesystem's authority, and narrowing happens at the endpoint above it, never here.
            // `Rights::root` is the only constructor that invents a rights set, and this is its
            // only caller.
            handles: alloc::vec![Some(Entry::Dir(TreePtr::root(), Rights::root(dir::ALL)))],
            clock: 1,
        })
    }

    /// Resolve `name` under the **bound directory** and return a handle for it: the shorthand for
    /// [`Server::open_file_at`] with [`fs_proto::fs::ROOT`], which is what every caller that predates
    /// directory handles means.
    pub fn open_file(&mut self, name: &str) -> Result<u32> {
        self.open_file_at(fs_proto::fs::ROOT as u32, name)
    }

    /// Resolve `name` under the directory `handle` names and return a file handle for it.
    ///
    /// The name is a single component resolved in that directory; it is not a path, which is the
    /// whole point (no walk, no escape). `EINVAL` for anything that is not one component,
    /// [`dir::EISDIR`] if the name is a directory ([`Server::open_dir`] is the verb for that), and
    /// `ENOENT` if there is no such entry **or** if the directory carries neither [`dir::READ`] nor
    /// [`dir::WRITE`]: a holder that may not reach a name must not learn that the name is there.
    ///
    /// The file handle **inherits** the directory's read and write bits, which is what makes
    /// milestone 47's "a program handed a directory to write logs into" mean something: the
    /// direction was decided when the directory was granted, and the file cannot widen it.
    pub fn open_file_at(&mut self, handle: u32, name: &str) -> Result<u32> {
        check_component(name)?;
        let (parent, rights) = self.dir_at(handle)?;
        if rights.denies_all(dir::READ | dir::WRITE) {
            return Err(Error::new(syscall::error::ENOENT));
        }
        let ptr = self.fs.tx(|tx| {
            let node = tx.find_node(parent, name)?;
            if node.data().is_dir() {
                return Err(Error::new(EISDIR));
            }
            // Register the node as open with the engine's usage table. This is what makes
            // `unlink` an unlink: without it, removing the last name frees the node immediately and
            // this handle would then read a deallocated one. See `Server::unlink`.
            tx.on_open_node(node.ptr())?;
            Ok(node.ptr())
        })?;
        Ok(self.install(Entry::File(ptr, rights.attenuate(dir::READ | dir::WRITE))))
    }

    /// **Descend: resolve `name` under the directory `handle` names and hand back a handle to the
    /// child directory, carrying `requested` rights** (milestone 47's keystone).
    ///
    /// The child's rights are `parent & requested`, and [`Rights::attenuate`] is the only way this
    /// crate makes a rights set that is not a root's, so **a child can never carry a right its
    /// parent lacked** and no descendant of it can either. The `EPERM` below is not what enforces
    /// that: delete it and the intersection still holds. What it does is refuse to hand back
    /// *less* than was asked for without saying so, which is DECISIONS §42's rule against silent
    /// degradation.
    ///
    /// Needs [`dir::DESCEND`] on the parent, and its absence answers `ENOENT` rather than a
    /// refusal, so a holder that may not walk in cannot map what is there.
    pub fn open_dir(&mut self, handle: u32, name: &str, requested: u64) -> Result<u32> {
        let (ptr, rights) = self.resolve_child_dir(handle, name, dir::DESCEND)?;
        let granted = rights.attenuate(requested);
        if granted != Rights::root(requested) {
            return Err(Error::new(EPERM));
        }
        Ok(self.install(Entry::Dir(ptr, granted)))
    }

    /// **Make a child directory and hand back a capability to it**: `mkdir` as descend-with-creation,
    /// which is why it lives beside [`Server::open_dir`] and shares its rights arithmetic exactly.
    ///
    /// Needs [`dir::CREATE`] *and* [`dir::DESCEND`] on the parent: making a directory you could not
    /// have walked into would be a way to mint a capability out of a right that was withheld.
    /// `EEXIST` if the name is already there, and nothing is modified, for the reason
    /// [`Server::create_file`] gives.
    pub fn make_dir(&mut self, handle: u32, name: &str, requested: u64) -> Result<u32> {
        check_component(name)?;
        let (parent, rights) = self.dir_at(handle)?;
        if !rights.allows(dir::CREATE) {
            return Err(Error::new(EROFS));
        }
        if !rights.allows(dir::DESCEND) {
            return Err(Error::new(EPERM));
        }
        let granted = rights.attenuate(requested);
        if granted != Rights::root(requested) {
            return Err(Error::new(EPERM));
        }
        self.clock += 1;
        let now = self.clock;
        let node = self.fs.tx(|tx| {
            if tx.find_node(parent, name).is_ok() {
                return Err(Error::new(EEXIST));
            }
            tx.create_node(parent, name, Node::MODE_DIR | 0o755, now, 0)
        })?;
        Ok(self.install(Entry::Dir(node.ptr(), granted)))
    }

    /// **Enumerate the directory `handle` names**, filling `out` with [`fs_proto::dirent`] records
    /// and returning how many bytes were used. `cursor` is the index of the first entry to return;
    /// 0 bytes back means the cursor is past the end.
    ///
    /// Needs [`dir::ENUMERATE`], and its absence is [`dir::EPERM`] rather than an empty listing.
    /// An empty listing would be a statement about the *directory*, which would be false; the true
    /// statement is about the capability, and DECISIONS §42 says a verb that is not offered fails
    /// loudly instead of degrading into a plausible lie.
    ///
    /// Entries are sorted by name so the cursor means the same thing across calls. **The honest
    /// caveat**: the directory is re-read per call, so a name added or removed between two calls of
    /// one walk can be seen twice or missed. That is readdir's usual caveat and it is recorded
    /// rather than fixed, because fixing it means holding a snapshot per client and this table is
    /// not per client.
    pub fn read_dir(&mut self, handle: u32, cursor: u32, out: &mut [u8]) -> Result<usize> {
        let (ptr, rights) = self.dir_at(handle)?;
        if !rights.allows(dir::ENUMERATE) {
            return Err(Error::new(EPERM));
        }
        self.fs.tx(|tx| {
            let mut children = Vec::new();
            tx.child_nodes(ptr, &mut children)?;
            // **The attribute store is not part of the namespace** (milestone 57). Filtered here,
            // before the cursor is applied, so the cursor still counts what the client can see; a
            // filter applied after `skip` would make one entry vanish at whichever offset the store
            // happened to sort to. `check_component` refuses the name on the way in, so this is the
            // other half of the same rule rather than a second one.
            children.retain(|e| e.name() != Some(xattr::STORE_DIR));
            children.sort_unstable_by(|a, b| a.name().unwrap_or("").cmp(b.name().unwrap_or("")));
            let mut used = 0;
            for entry in children.iter().skip(cursor as usize) {
                let Some(name) = entry.name() else { continue };
                let child: redoxfs::TreeData<Node> = tx.read_tree(entry.node_ptr())?;
                match fs_proto::dirent::encode(
                    &mut out[used..],
                    name.as_bytes(),
                    child.data().is_dir(),
                ) {
                    Some(n) => used += n,
                    // A record is never split: stop before one that will not fit and let the
                    // caller's next cursor start here. That is what makes "the buffer filled" and
                    // "the directory ended" distinguishable rather than a guess.
                    None => break,
                }
            }
            Ok(used)
        })
    }

    /// Read up to `buf.len()` bytes from `handle` at `offset`, returning the count (0 at EOF). Passes
    /// atime 0 so a read never advances the node's access time (never *newer* than what is stored),
    /// which keeps reads from triggering a copy-on-write of the node on every call.
    ///
    /// `EBADF` if the handle was not opened for reading, which is POSIX's own answer for reading a
    /// write-only descriptor and is a fact about the handle rather than a policy about the file.
    pub fn read(&mut self, handle: u32, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let ptr = self.file_at(handle, dir::READ, EBADF)?;
        self.fs.tx(|tx| tx.read_node(ptr, offset, buf, 0, 0))
    }

    /// Write `data` to `handle` at `offset`, returning the count. Advances the internal clock so the
    /// node's mtime moves forward; the data persists regardless of the timestamp.
    ///
    /// [`dir::EROFS`] without [`dir::WRITE`], the same word and the same argument
    /// `fs_file_caretaker` uses: through this capability the file is read-only, and there is no
    /// policy that could have said yes.
    pub fn write(&mut self, handle: u32, offset: u64, data: &[u8]) -> Result<usize> {
        let ptr = self.file_at(handle, dir::WRITE, EROFS)?;
        self.clock += 1;
        let now = self.clock;
        self.fs.tx(|tx| tx.write_node(ptr, offset, data, now, 0))
    }

    /// The current size, in bytes, of the file a handle names. Allowed through any file handle: a
    /// holder that may write but not read still has to be able to find the end.
    pub fn fstat(&mut self, handle: u32) -> Result<u64> {
        let ptr = self.file_at(handle, 0, EBADF)?;
        self.fs.tx(|tx| Ok(tx.read_tree(ptr)?.data().size()))
    }

    /// Create `name` under the bound directory and return a handle for it (milestone 31 phase 2).
    ///
    /// **Semantics, stated because the absence of these verbs cost a day.** If `name` does not exist
    /// it is created empty and opened. If it *does* exist this returns `EEXIST` and creates nothing:
    /// create is create, not create-or-open, so a caller that wants either has to say which it wants.
    /// The alternative (silently opening an existing file) is what makes a partly-working write look
    /// like a working one, which is the exact failure mode §27 records.
    ///
    /// Resolved under the bound directory like every other name, so this cannot create outside the
    /// granted subtree, and `check_component` rejects a path rather than walking one.
    pub fn create_file(&mut self, name: &str) -> Result<u32> {
        self.create_file_at(fs_proto::fs::ROOT as u32, name)
    }

    /// [`Server::create_file`], under the directory `handle` names. Needs [`dir::CREATE`] on it;
    /// without it the answer is [`dir::EROFS`], because through this capability that directory takes
    /// no new names.
    pub fn create_file_at(&mut self, handle: u32, name: &str) -> Result<u32> {
        check_component(name)?;
        let (parent, rights) = self.dir_at(handle)?;
        if !rights.allows(dir::CREATE) {
            return Err(Error::new(EROFS));
        }
        // Existence is checked inside the same transaction as the create, so two clients racing on
        // one name cannot both be told they created it.
        self.clock += 1;
        let now = self.clock;
        let ptr = self.fs.tx(|tx| {
            if tx.find_node(parent, name).is_ok() {
                return Err(Error::new(EEXIST));
            }
            let node = tx.create_node(parent, name, Node::MODE_FILE | 0o644, now, 0)?;
            // Registered as open exactly as `open_file_at` registers, and for its reason: a file
            // created and then unlinked with its handle still open is the temp-file idiom.
            tx.on_open_node(node.ptr())?;
            Ok(node.ptr())
        })?;
        Ok(self.install(Entry::File(ptr, rights.attenuate(dir::READ | dir::WRITE))))
    }

    /// Set the size of the file a handle names to exactly `size` bytes (milestone 31 phase 2).
    ///
    /// **Both directions, deliberately.** Shrinking discards the bytes past `size`; growing extends
    /// with zeroes. That is POSIX `ftruncate` semantics and it is what makes a shorter write able to
    /// *replace* a file's contents rather than leave the old tail behind. §27 records what the absence
    /// of this cost: a 61-byte write over a 64-byte file left a 64-byte file, three investigations read
    /// the resulting assertion failure as a write failure, and the engine was never at fault.
    ///
    /// **What is guaranteed if the machine dies mid-truncate: untested (milestone 37).** RedoxFS is
    /// copy-on-write and commits through a header ring, and the server always mounts with the
    /// `cleanup: true` replay that walks back to the newest *consistent* generation, so the engine's
    /// design says a torn truncate leaves either the old size or the new one. We have not injected a
    /// torn or dropped write to check, so that is a design claim and not a measurement. Truncate and
    /// create are the first verbs that change a file's *shape* rather than its bytes, which makes them
    /// the most interesting cases for milestone 37 to attack.
    pub fn truncate(&mut self, handle: u32, size: u64) -> Result<()> {
        // A truncate carries no bytes, so a guard that only covered `write` would leave a way to
        // destroy a file just as thoroughly. It takes the same right and answers the same word.
        let ptr = self.file_at(handle, dir::WRITE, EROFS)?;
        self.clock += 1;
        let now = self.clock;
        self.fs.tx(|tx| tx.truncate_node(ptr, size, now, 0))
    }

    /// **Move `src` under the directory `src_dir` names to `dst` under the directory `dst_dir`
    /// names** (milestone 47). The verb `mv` is, and the rung that gave [`dir::REMOVE`] something to
    /// enforce.
    ///
    /// # The two atomicities, which POSIX taught everyone to conflate (DECISIONS §42)
    ///
    /// - **Concurrency-atomic.** Nothing observes the state where the name is in both places or
    ///   neither, and the reason is structural rather than a lock: the serve loop runs one request
    ///   to completion before it receives the next, so inside this server there is no concurrent
    ///   observer at all.
    /// - **Crash-atomic.** Everything below happens inside one `fs.tx`, which reaches the platter
    ///   through one commit in RedoxFS's header ring, so a fresh mount finds the old name or the new
    ///   one. Measured rather than asserted: this is the last operation of the workload in
    ///   `tests/crash_consistency.rs`, whose sweep cuts the device at every write the workload
    ///   makes, so the cut lands inside this transaction too.
    ///
    /// # Rights
    ///
    /// [`dir::REMOVE`] on the source (its name goes away) and [`dir::CREATE`] on the destination (a
    /// name appears), each refused with [`dir::EROFS`]. Within one directory a rename needs both on
    /// that one. **Checked before anything is resolved**, so a capability that may not move a name
    /// cannot use this verb to find out whether one is there.
    ///
    /// # What it refuses, and why each refusal is the contract's rather than the backend's
    ///
    /// - A **directory** may be renamed in place but not moved between directories: `EINVAL`. See
    ///   [`fs_proto::fs::RENAME`] for the cycle argument. Keeping the parent unchanged is what makes
    ///   the in-place case provably safe without an ancestry walk.
    /// - Replacing a destination of a **different kind** is `EISDIR` (file over directory) or
    ///   `ENOTDIR` (directory over file). RedoxFS's own `rename_node` allows both, because it passes
    ///   the *destination's* type to `remove_node` and so never compares the two; these two lines
    ///   are ours, at our boundary, for §42's reason that an offered verb must mean one thing on
    ///   every backend.
    /// - A non-empty destination directory is `ENOTEMPTY`, from the engine.
    /// - Either name that is not a single component is `EINVAL`, so `..` means nothing here either.
    pub fn rename(&mut self, src_dir: u32, src: &str, dst_dir: u32, dst: &str) -> Result<()> {
        check_component(src)?;
        check_component(dst)?;
        let (src_parent, src_rights) = self.dir_at(src_dir)?;
        let (dst_parent, dst_rights) = self.dir_at(dst_dir)?;
        if !src_rights.allows(dir::REMOVE) || !dst_rights.allows(dir::CREATE) {
            return Err(Error::new(EROFS));
        }
        let same_dir = src_parent.id() == dst_parent.id();
        self.fs.tx(|tx| {
            let node = tx.find_node(src_parent, src)?;
            if node.data().is_dir() && !same_dir {
                return Err(Error::new(EINVAL));
            }
            // The node a replaced destination names, when this rename is its last link. Its
            // extended attributes have to go with it, in **this** transaction: `rename_node` calls
            // `remove_node` on the destination and throws the freed id away, so nothing else here
            // ever learns that a node died, and a leaked blob would be inherited by whatever node
            // the engine hands that id to next. Attributes keyed on a node are free across a rename
            // and dangerous across a reuse, which are two halves of the same property.
            let mut replaced = None;
            if let Ok(existing) = tx.find_node(dst_parent, dst) {
                // A no-op rename is a success, not a self-replacement: `remove_node` would take the
                // only link and `link_node` would then have nothing to point at.
                if existing.id() == node.id() {
                    return Ok(());
                }
                if existing.data().is_dir() != node.data().is_dir() {
                    return Err(Error::new(if existing.data().is_dir() {
                        EISDIR
                    } else {
                        ENOTDIR
                    }));
                }
                if existing.data().links() == 1 {
                    replaced = Some(existing.id());
                }
            }
            tx.rename_node(src_parent, src, dst_parent, dst)?;
            // The source keeps its node and therefore keeps its attributes, with no code here
            // knowing a rename happened. That is the whole reason the store keys on a node rather
            // than on a path, and it is what an AppleDouble sidecar gets wrong.
            if let Some(id) = replaced {
                purge_attrs(tx, id)?;
            }
            Ok(())
        })
    }

    /// **Remove `name` from the directory `handle` names: `rm`'s verb, and it is `unlink` rather
    /// than revocation** (milestone 47's commands).
    ///
    /// Needs [`dir::REMOVE`], refused with [`dir::EROFS`], and the right is checked **before the
    /// name is resolved**, so a capability that may not remove cannot use this verb to discover
    /// whether a name is there. `ENOENT` if there is no such entry, `EINVAL` if it is not a single
    /// component, and [`dir::EISDIR`] for a directory: `rm` takes a name of a file away, and this
    /// contract has no `RMDIR`, which is stated where a caller meets it
    /// ([`fs_proto::fs::UNLINK`]) rather than approximated by removing whatever is found.
    ///
    /// # What "unlink, not revoke" costs, and where it is paid
    ///
    /// The name goes and the object stays while anyone still holds a handle on it, which is POSIX's
    /// promise and the thing atomic-replace depends on. It is not free and it is not automatic:
    /// RedoxFS frees a node the moment its last link goes **unless the node is registered as open**
    /// in its usage table, in which case `remove_node` queues it on the release list and
    /// `on_close_node` collects it later. So [`Server::open_file_at`] and [`Server::create_file_at`]
    /// register, [`Server::close`] deregisters, and *that* pairing is what makes this verb an
    /// unlink. Without it the read that follows an unlink reads a deallocated node, and `rm` would
    /// be a revoke wearing an unlink's name, which is exactly the conflation §42 forbids.
    ///
    /// **BUGS.** A handle that is never closed pins the node for the life of the server, exactly as
    /// a leaked fd does on Unix, and this handle table is per *server* and outlives its clients: a
    /// confined program that dies mid-session leaves its files on the release list forever. The fix
    /// is per-client handle ownership, which the note records as the same missing thing that makes
    /// revocation unavailable.
    pub fn unlink(&mut self, handle: u32, name: &str) -> Result<()> {
        check_component(name)?;
        let (parent, rights) = self.dir_at(handle)?;
        if !rights.allows(dir::REMOVE) {
            return Err(Error::new(EROFS));
        }
        // `remove_node` compares the node's kind against the mode it is given, so `EISDIR` for a
        // directory is the engine's own answer here and matches POSIX. Contrast `rename`, where the
        // kind check had to be ours because the engine never compares the two.
        //
        // It also answers `Some(id)` **exactly when the node's last link went**, which is precisely
        // when its extended attributes must go too, and `None` when a link remains. So the purge
        // asks the engine rather than guessing, and it happens inside the same transaction as the
        // removal: a crash cannot leave a freed node's attributes behind for the next node to
        // inherit that id (milestone 57, DECISIONS §34).
        self.fs.tx(|tx| {
            if let Some(id) = tx.remove_node(parent, name, Node::MODE_FILE)? {
                purge_attrs(tx, id)?;
            }
            Ok(())
        })
    }

    /// **Remove an empty directory `name` from the directory `handle` names: `rmdir(2)`, and
    /// empty-only is the entire safety property** (milestone 47's `rm -r`).
    ///
    /// Needs [`dir::REMOVE`] on the parent, refused with [`dir::EROFS`], checked **before the name
    /// is resolved** for [`Server::unlink`]'s reason. `ENOTEMPTY` if anything is still in it,
    /// `ENOTDIR` for a file (POSIX's own answer, and the mirror of `unlink`'s `EISDIR`), `EINVAL`
    /// for anything that is not a single component, `ENOENT` if there is no such name.
    ///
    /// # Why this verb is safe to offer and a recursive one is not
    ///
    /// `mkdir` shipped in the commands lane with nothing that removed what it made, and the
    /// objection recorded there was that "a verb that removes whatever it finds is how one word
    /// takes a subtree away". That is right about a *recursive* verb and is not right about Unix's,
    /// which is the point: **no single call on this contract can take a subtree away.** The
    /// recursion lives in userspace (`user/src/rm.rs`) as a loop of individually safe steps, each of
    /// which needs the rights for it at its own level, so a walk stops exactly where the
    /// capabilities stop rather than where a check in here remembered to look.
    ///
    /// # It is not revocation, and here is what that costs
    ///
    /// [`Server::unlink`] pairs with the engine's usage table so an unlinked file survives for
    /// whoever holds a handle. **Directory handles are not registered that way**, so a client
    /// holding a handle to the directory this call removes is left naming a node the engine has
    /// freed, and its next request through that handle fails (`ENOENT`) rather than reaching
    /// something else. Removing a name is still not revoking a capability: the handle table is per
    /// *server* and this server cannot enumerate the clients holding handles, so it could not
    /// invalidate them if it wanted to. Recorded rather than fixed, because registering directory
    /// handles is the same change as per-client handle ownership, which is what revocation needs.
    pub fn rmdir(&mut self, handle: u32, name: &str) -> Result<()> {
        check_component(name)?;
        let (parent, rights) = self.dir_at(handle)?;
        if !rights.allows(dir::REMOVE) {
            return Err(Error::new(EROFS));
        }
        // `remove_node` compares the node's kind against the mode it is given, so `ENOTDIR` for a
        // file is the engine's own answer, and it refuses a non-empty directory with `ENOTEMPTY`
        // from inside the same transaction that would have removed it. Both are POSIX's words for
        // `rmdir`, which is why neither is checked again out here: a second opinion that could
        // disagree with the engine is worse than one that cannot.
        //
        // A directory carries extended attributes exactly as a file does, so it is purged exactly as
        // a file is. See [`Server::unlink`] for why the engine's `Some(id)` is what decides it.
        self.fs.tx(|tx| {
            if let Some(id) = tx.remove_node(parent, name, Node::MODE_DIR)? {
                purge_attrs(tx, id)?;
            }
            Ok(())
        })
    }

    // --- Extended attributes (milestone 57, DECISIONS §34's 2026-07-31 amendment) ---
    //
    // The engine has none of this. The four methods below are a **layer**: a store the server keeps
    // in the image, keyed on the node's `TreePtr` id, published as `fs_proto::xattr::store`. What
    // makes the layer legitimate here and worthless on Linux is that nothing can bypass it, because
    // every path to these bytes goes through `fs_proto`. See notes/xattr.md.

    /// **Read one extended attribute of the node `handle` names**, returning its type code and the
    /// number of bytes written into `out`.
    ///
    /// Needs [`dir::READ`], refused with `EBADF`, which is what [`Server::read`] needs and answers.
    /// `ENODATA` if the attribute is not set: the only error that means absence, which is the
    /// discipline milestone 37's harness learned the hard way (notes/fs-server.md).
    ///
    /// **`ERANGE` if `out` is shorter than the value**, never a short read. POSIX's own answer, and
    /// DECISIONS §42's rule: a value clipped to fit would hand back a file that looked intact and
    /// was not. In the running system `out` is the shared page, which is larger than
    /// [`xattr::MAX_VALUE`] by construction, so this cannot fire on the wire.
    pub fn get_xattr(&mut self, handle: u32, name: &[u8], out: &mut [u8]) -> Result<(u32, usize)> {
        if !xattr::valid_name(name) {
            return Err(Error::new(xattr::ERANGE));
        }
        let ptr = self.node_at(handle, dir::READ, EBADF)?;
        self.fs.tx(|tx| {
            let blob = read_attrs(tx, ptr.id())?;
            let (kind, value) = xattr::store::get(&blob, name).ok_or(Error::new(xattr::ENODATA))?;
            if out.len() < value.len() {
                return Err(Error::new(xattr::ERANGE));
            }
            out[..value.len()].copy_from_slice(value);
            Ok((kind, value.len()))
        })
    }

    /// **Set one extended attribute on the node `handle` names.** Creating and replacing are one
    /// operation ([`fs_proto::fs::SETXATTR`] says why).
    ///
    /// Needs [`dir::WRITE`], refused with [`dir::EROFS`], which is what [`Server::write`] needs and
    /// answers. The three ceilings answer their own words ([`xattr::ERANGE`], [`xattr::E2BIG`],
    /// [`xattr::ENOSPC`]) so a caller can tell which one it hit.
    ///
    /// **Crash-atomic with the file's own bytes**, because the read, the rewrite and the store
    /// write all happen inside one `fs.tx`, which reaches the platter as one commit in RedoxFS's
    /// header ring. That was the check that had to pass before this layer was viable at all
    /// (DECISIONS §34), and it is why an attribute cannot survive a crash that lost the file or
    /// vice versa.
    pub fn set_xattr(&mut self, handle: u32, name: &[u8], kind: u32, value: &[u8]) -> Result<()> {
        // Checked before anything is allocated or opened, so a client cannot make the server size a
        // buffer from a length it invented. `store::set` checks again, and that is the copy the host
        // tests exercise; this one is the boundary.
        if !xattr::valid_name(name) {
            return Err(Error::new(xattr::ERANGE));
        }
        if value.len() > xattr::MAX_VALUE {
            return Err(Error::new(xattr::E2BIG));
        }
        let ptr = self.node_at(handle, dir::WRITE, EROFS)?;
        self.clock += 1;
        let now = self.clock;
        self.fs.tx(|tx| {
            let blob = read_attrs(tx, ptr.id())?;
            // Replacing can only shrink, so the old blob plus one whole new record is always enough
            // and `store::set` never has to refuse for want of room.
            let mut out =
                alloc::vec![0u8; blob.len() + xattr::store::record_len(name.len(), value.len())];
            let n = xattr::store::set(&blob, name, kind, value, &mut out).map_err(Error::new)?;
            write_attrs(tx, ptr.id(), &out[..n], now)
        })
    }

    /// **List the attribute names on the node `handle` names**, filling `out` with
    /// [`xattr::list`] records and returning the bytes used. Needs [`dir::READ`], refused with
    /// `EBADF`.
    ///
    /// No cursor, unlike [`Server::read_dir`]: a complete listing fits one page by construction
    /// ([`xattr::MAX_COUNT`]), so it cannot be observed half-changed the way a cursored directory
    /// walk can.
    pub fn list_xattr(&mut self, handle: u32, out: &mut [u8]) -> Result<usize> {
        let ptr = self.node_at(handle, dir::READ, EBADF)?;
        self.fs
            .tx(|tx| Ok(xattr::store::list(&read_attrs(tx, ptr.id())?, out)))
    }

    /// **Remove one extended attribute from the node `handle` names.** Needs [`dir::WRITE`],
    /// refused with [`dir::EROFS`]. `ENODATA` if it was not set, rather than a quiet success: the
    /// caller asked about one specific thing.
    pub fn remove_xattr(&mut self, handle: u32, name: &[u8]) -> Result<()> {
        if !xattr::valid_name(name) {
            return Err(Error::new(xattr::ERANGE));
        }
        let ptr = self.node_at(handle, dir::WRITE, EROFS)?;
        self.clock += 1;
        let now = self.clock;
        self.fs.tx(|tx| {
            let blob = read_attrs(tx, ptr.id())?;
            let mut out = alloc::vec![0u8; blob.len()];
            let n = xattr::store::remove(&blob, name, &mut out).map_err(Error::new)?;
            write_attrs(tx, ptr.id(), &out[..n], now)
        })
    }

    /// Take the disk back out of the server, **dropping the mount without unmounting**, which is
    /// exactly how a dying process leaves it: no flush, no clean shutdown, whatever is on the platter
    /// is the whole story. Milestone 37's harness uses it to get the recorded write log back after a
    /// workload; the crash tests then reconstruct the platter from that log alone.
    pub fn into_disk(self) -> D {
        self.fs.disk
    }

    /// Release a handle. `EBADF` if it was not open.
    ///
    /// **The bound directory cannot be closed**, and the refusal is `EINVAL` rather than `EBADF`:
    /// handle 0 is not something the client opened, it is the root of its namespace, and a client
    /// that could close it could make every later request in the session fail.
    ///
    /// Closing a **file** handle is also what finally frees a node whose last name was unlinked
    /// while this handle was open (see [`Server::unlink`]). Nothing else collects it, so a close is
    /// not merely bookkeeping in a table this server owns.
    pub fn close(&mut self, handle: u32) -> Result<()> {
        if handle as u64 == fs_proto::fs::ROOT {
            return Err(Error::new(EINVAL));
        }
        let entry = self.entry(handle)?;
        self.handles[handle as usize] = None;
        if let Entry::File(ptr, _) = entry {
            self.fs.tx(|tx| tx.on_close_node(ptr))?;
        }
        Ok(())
    }

    /// **How much room the image has** ([`fs_proto::fs::STATFS`], milestone 54), as
    /// `(block_size, total_blocks, free_blocks)`.
    ///
    /// The handle is validated and then not used, which looks odd and is the point: it is the
    /// qualification rather than the subject. Holding any handle this server minted is what
    /// entitles a caller to ask, and the answer is about the one image behind all of them, so
    /// `EBADF` for a handle that was never minted is the whole of the check. No right is demanded;
    /// [`fs_proto::fs::STATFS`] argues that at length.
    ///
    /// **Where the two numbers come from, and why one is not read inside a transaction.**
    /// `header.size()` is the filesystem's size in bytes as the newest committed header records it,
    /// and `allocator.free()` counts free level-0 blocks in the *committed* allocator. Both are
    /// read outside `fs.tx`, deliberately: opening a transaction to answer a question changes
    /// nothing and would put a commit in the path of a read-only verb. RedoxFS's own `clone`
    /// computes free space from exactly this pair.
    ///
    /// A transaction in flight elsewhere would move `free`, and there is none: this server runs one
    /// request to completion before it receives the next, which is the same property
    /// [`fs_proto::fs::RENAME`]'s concurrency atomicity rests on.
    pub fn statfs(&mut self, handle: u32) -> Result<(u64, u64, u64)> {
        self.entry(handle)?;
        let block = redoxfs::BLOCK_SIZE;
        // Integer division, so a size that is not a whole number of blocks reports the blocks that
        // exist rather than a fraction the caller would round the wrong way.
        Ok((
            block,
            self.fs.header.size() / block,
            self.fs.allocator().free(),
        ))
    }

    /// **May this handle ask for the storage to be made durable?** (milestone 55). `Ok(())` when it
    /// may; the caller then does the IO.
    ///
    /// This is the permission half of `fs_proto::fs::SYNC` and **it performs no flush**, which the
    /// name is chosen to say. The flush is a `fs_proto::blk::FLUSH` to the block server, so it lives
    /// in the binary with the rest of the IO; RedoxFS's `Disk` trait has no sync method and this
    /// crate does not modify vendored code to add one. A method here called `sync` would be the
    /// exact kind of claim-beyond-the-mechanism milestone 55 exists to remove.
    ///
    /// `node_at` with [`dir::WRITE`], so it takes a handle of **either kind** the
    /// way `STATFS` does (the answer is about the storage, not about the node), and refuses with
    /// [`dir::EROFS`] the way every mutating verb does. The rights argument is in
    /// `fs_proto::fs::SYNC`'s own documentation: a sync is an action on the device rather than a
    /// fact about it, and a read-only holder has nothing outstanding to make durable.
    ///
    /// Name: provisional, minted by milestone 55's durability lane on 2026-08-18.
    pub fn sync_permitted(&self, handle: u32) -> Result<()> {
        self.node_at(handle, dir::WRITE, EROFS).map(|_| ())
    }

    /// Install an entry in the handle table, reusing a freed slot before growing. Returns the
    /// handle. Slot 0 is never free, so a minted handle is never 0 and never shadows the root.
    fn install(&mut self, entry: Entry) -> u32 {
        if let Some(i) = self.handles.iter().position(|s| s.is_none()) {
            self.handles[i] = Some(entry);
            i as u32
        } else {
            self.handles.push(Some(entry));
            (self.handles.len() - 1) as u32
        }
    }

    /// What a handle names, or `EBADF`. Every operation reaches the table through here or through
    /// one of the two typed wrappers below, so a forged or stale handle is refused in one place.
    fn entry(&self, handle: u32) -> Result<Entry> {
        self.handles
            .get(handle as usize)
            .copied()
            .flatten()
            .ok_or(Error::new(EBADF))
    }

    /// The directory a handle names and the rights it carries. [`dir::ENOTDIR`] for a file handle,
    /// which is a fact about the handle rather than a refusal of the request.
    fn dir_at(&self, handle: u32) -> Result<(TreePtr<Node>, Rights)> {
        match self.entry(handle)? {
            Entry::Dir(ptr, rights) => Ok((ptr, rights)),
            Entry::File(..) => Err(Error::new(ENOTDIR)),
        }
    }

    /// The file a handle names, once its rights carry `needed`. `refusal` is the errno to answer
    /// when they do not, which differs by direction on purpose: a read through a write-only handle
    /// is `EBADF` (POSIX's answer, a fact about the handle) and a write through a read-only one is
    /// [`dir::EROFS`] (`fs_file_caretaker`'s answer, a fact about the capability).
    fn file_at(&self, handle: u32, needed: u64, refusal: i32) -> Result<TreePtr<Node>> {
        match self.entry(handle)? {
            Entry::File(ptr, rights) if rights.allows(needed) => Ok(ptr),
            Entry::File(..) => Err(Error::new(refusal)),
            Entry::Dir(..) => Err(Error::new(EISDIR)),
        }
    }

    /// The node a handle names, **file or directory alike**, once its rights carry `needed`.
    ///
    /// The only place in this file that does not care which kind a handle is, and extended
    /// attributes are the only thing that does not care: a directory carries them exactly as a file
    /// does, so a verb that refused one kind would be inventing a distinction the feature has not
    /// got. `refusal` differs by direction for [`Server::file_at`]'s reason.
    fn node_at(&self, handle: u32, needed: u64, refusal: i32) -> Result<TreePtr<Node>> {
        let (ptr, rights) = match self.entry(handle)? {
            Entry::Dir(ptr, rights) | Entry::File(ptr, rights) => (ptr, rights),
        };
        if rights.allows(needed) {
            Ok(ptr)
        } else {
            Err(Error::new(refusal))
        }
    }

    /// Resolve `name` under the directory `handle` names, requiring `needed` on the parent, and
    /// return the child **directory** with the parent's rights for the caller to attenuate.
    ///
    /// The missing-right answer is `ENOENT`, not a refusal: this is the naming rung of the ladder,
    /// and a holder that may not descend must not be able to learn what is below.
    fn resolve_child_dir(
        &mut self,
        handle: u32,
        name: &str,
        needed: u64,
    ) -> Result<(TreePtr<Node>, Rights)> {
        check_component(name)?;
        let (parent, rights) = self.dir_at(handle)?;
        if !rights.allows(needed) {
            return Err(Error::new(syscall::error::ENOENT));
        }
        let node = self.fs.tx(|tx| tx.find_node(parent, name))?;
        if !node.data().is_dir() {
            return Err(Error::new(ENOTDIR));
        }
        Ok((node.ptr(), rights))
    }
}

/// Reject anything that is not a single path component, with `EINVAL`.
///
/// **This is our rule, enforced at our boundary, and it was previously true only by accident.** The
/// contract has always said a name is a single component resolved under the bound directory, with no
/// absolute path and no `..` escape (§27, and this module's own header). RedoxFS does not enforce that:
/// its `check_name` rejects only `:`, over-long names, and duplicates, so `/`, `.` and `..` pass
/// straight through. Nothing walked paths, so nothing escaped, which made the invariant hold by the
/// absence of a walker rather than by a check.
///
/// Adding `CREATE` (milestone 31 phase 2) turned that from a latent oddity into something a client
/// could *write*: `create_file("../escape")` created a directory entry literally named `../escape` in
/// the bound directory. Still not a traversal, and still a landmine, because the moment anything walks
/// paths (a per-directory grant binding a subtree, the host tool, or the image mounted through Redox's
/// FUSE driver) that entry means something it should never have been able to mean. A found-and-fixed
/// gap, not a theoretical one: the test that caught it asserted a property the docs already claimed.
///
/// Deliberately checked here rather than patched into the vendored engine: it is a rule of *our*
/// contract, not a bug in RedoxFS, whose callers are free to name entries whatever they like.
/// **And the attribute store's name is reserved, everywhere** (milestone 57). A store a client could
/// name would be part of the namespace, and then "the attributes of a file" would be reachable as
/// ordinary bytes by anything holding the directory they live in, which is precisely what DECISIONS
/// §34's amendment says to get right. It is refused in every directory rather than only in the root
/// so that the rule a client has to remember is one sentence instead of a location-dependent one,
/// and `EINVAL` is the answer because the name is not expressible here rather than not permitted.
/// [`Server::read_dir`] is the other half: what cannot be named is also not listed.
fn check_component(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name == xattr::STORE_DIR
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.contains('\0')
    {
        return Err(Error::new(EINVAL));
    }
    Ok(())
}

/// The name of the file holding one node's attribute blob: its `TreePtr` id in hex. ASCII by
/// construction, so the conversion cannot fail; the error arm exists so a future change to
/// [`xattr::store::file_name`] cannot silently alias two nodes onto one blob.
fn store_file(id: u32) -> Result<([u8; 8], usize)> {
    let bytes = xattr::store::file_name(id);
    if core::str::from_utf8(&bytes).is_err() {
        return Err(Error::new(EIO));
    }
    Ok((bytes, 8))
}

/// The attribute store's directory in the image root, or `None` if nothing has ever set an
/// attribute on this filesystem.
///
/// **`ENOENT` is the only error read as absence**, and every other one is propagated. A dropped
/// write to the root's tree block makes a lookup answer `EIO`, and reading that as "there are no
/// attributes" is exactly the false negative that made milestone 37's harness report filesystems
/// that never existed (notes/fs-server.md). Here it would silently hand a client an empty attribute
/// set for a file that has some.
fn find_store<D: Disk>(tx: &mut Transaction<D>) -> Result<Option<TreePtr<Node>>> {
    match tx.find_node(TreePtr::root(), xattr::STORE_DIR) {
        Ok(node) => Ok(Some(node.ptr())),
        Err(e) if e.errno == ENOENT => Ok(None),
        Err(e) => Err(e),
    }
}

/// One node's attribute blob, or empty if it has none. Never partial: a blob longer than the layer
/// can hold is `EIO`, because the alternative is reading whatever is in the first
/// [`xattr::store::MAX_BYTES`] of a file this layer did not write.
fn read_attrs<D: Disk>(tx: &mut Transaction<D>, node_id: u32) -> Result<Vec<u8>> {
    let Some(dir_ptr) = find_store(tx)? else {
        return Ok(Vec::new());
    };
    let (bytes, n) = store_file(node_id)?;
    let name = core::str::from_utf8(&bytes[..n]).map_err(|_| Error::new(EIO))?;
    let node = match tx.find_node(dir_ptr, name) {
        Ok(node) => node,
        Err(e) if e.errno == ENOENT => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let size = node.data().size() as usize;
    if size > xattr::store::MAX_BYTES {
        return Err(Error::new(EIO));
    }
    let mut blob = alloc::vec![0u8; size];
    let read = tx.read_node(node.ptr(), 0, &mut blob, 0, 0)?;
    blob.truncate(read);
    Ok(blob)
}

/// Replace one node's attribute blob. An **empty** blob removes the store file rather than leaving a
/// zero-length one behind, so "no file" and "no attributes" stay the same statement and the store
/// does not accumulate an entry per node that ever held an attribute.
fn write_attrs<D: Disk>(
    tx: &mut Transaction<D>,
    node_id: u32,
    blob: &[u8],
    now: u64,
) -> Result<()> {
    if blob.is_empty() {
        return purge_attrs(tx, node_id);
    }
    let (bytes, n) = store_file(node_id)?;
    let name = core::str::from_utf8(&bytes[..n]).map_err(|_| Error::new(EIO))?;
    let dir_ptr = match find_store(tx)? {
        Some(ptr) => ptr,
        // 0o700 rather than 0o755: the mode is decoration here (this contract has no uid to check
        // it against), and the narrower one is what a recovery tool's listing should suggest.
        None => tx
            .create_node(
                TreePtr::root(),
                xattr::STORE_DIR,
                Node::MODE_DIR | 0o700,
                now,
                0,
            )?
            .ptr(),
    };
    let ptr = match tx.find_node(dir_ptr, name) {
        Ok(node) => node.ptr(),
        Err(e) if e.errno == ENOENT => tx
            .create_node(dir_ptr, name, Node::MODE_FILE | 0o600, now, 0)?
            .ptr(),
        Err(e) => return Err(e),
    };
    tx.write_node(ptr, 0, blob, now, 0)?;
    // **Then shrink to exactly the new length.** A write does not truncate, so a blob that got
    // shorter would leave the previous one's tail behind and the reader would walk records nobody
    // wrote. That is DECISIONS §27's four-times-corrected failure met again, and here it would
    // present as corrupted attributes rather than as a longer file.
    tx.truncate_node(ptr, blob.len() as u64, now, 0)?;
    Ok(())
}

/// Take one node's attributes away. Called from [`Server::unlink`], [`Server::rmdir`] and
/// [`Server::rename`] **inside the same transaction as the removal**, which is the whole of why a
/// freed node id cannot hand its attributes to the next node issued that id.
///
/// A node with no store file is not an error: most nodes have no attributes.
fn purge_attrs<D: Disk>(tx: &mut Transaction<D>, node_id: u32) -> Result<()> {
    let Some(dir_ptr) = find_store(tx)? else {
        return Ok(());
    };
    let (bytes, n) = store_file(node_id)?;
    let name = core::str::from_utf8(&bytes[..n]).map_err(|_| Error::new(EIO))?;
    match tx.remove_node(dir_ptr, name, Node::MODE_FILE) {
        // Only when a blob really went, so the cost falls on attribute removals rather than on
        // every unlink on the filesystem.
        Ok(_) => drop_store_if_empty(tx),
        Err(e) if e.errno == ENOENT => Ok(()),
        Err(e) => Err(e),
    }
}

/// **Take the store directory away with the last blob in it**, so a filesystem that holds no
/// attributes is indistinguishable from one that never held any.
///
/// This used to be a recorded limitation (notes/xattr.md), on the grounds that noticing an empty
/// directory costs a walk. It does not: `remove_node` with `MODE_DIR` **already** refuses a
/// directory that still has entries, with `ENOTEMPTY`, so the engine's own check is the emptiness
/// test and there is nothing to enumerate. Asking it and accepting the refusal costs one lookup on
/// the removals that emptied a blob, inside the transaction that emptied it.
///
/// The reason to bother is the recovery side rather than the byte. `redoxfs_host extract` copies the
/// store out with the tree, so a leftover empty `.nife-attrs` would land in somebody's recovered
/// Documents folder as a directory nothing explains. A store that disappears when the last attribute
/// does never gets there.
///
/// `ENOENT` is tolerated for the same reason it is everywhere else here (another transaction in the
/// same commit may have taken it), and every other error is propagated, because a store that failed
/// to be removed for an unknown reason is not a store known to be gone.
fn drop_store_if_empty<D: Disk>(tx: &mut Transaction<D>) -> Result<()> {
    match tx.remove_node(TreePtr::root(), xattr::STORE_DIR, Node::MODE_DIR) {
        Ok(_) => Ok(()),
        Err(e) if e.errno == ENOTEMPTY || e.errno == ENOENT => Ok(()),
        Err(e) => Err(e),
    }
}

/// One filesystem block, in bytes (RedoxFS's `BLOCK_SIZE`): the unit [`BlockIo`] transfers.
pub const BLOCK: usize = 4096;

/// **The one place in this tree that sees both the wire protocol and the store**, which is what
/// makes the two assertions below possible at all: `fs_proto` deliberately depends on nothing, and
/// the vendored engine has never heard of it. Milestone 138 wrote them because it moved the record
/// level, and `fs_proto::fixture::throughput::RECORD`'s own comment named itself the soft spot a
/// change like that would break silently.
///
/// They are `const` assertions rather than tests so that they fire in the EL0 build too, where no
/// test harness runs. A build is the only artifact that can be wrong about this.
const _: () = {
    // `BLOCK` is the transfer unit for a store whose block size is `BLOCK_SIZE`. If they ever
    // disagree, every chunked read and write in this file is off by a factor.
    assert!(BLOCK as u64 == redoxfs::BLOCK_SIZE);

    // The record-aligned throughput phase reads at multiples of `RECORD`, and that means what it
    // claims only while every such multiple is also a record boundary. See that constant's comment.
    assert!(
        fs_proto::fixture::throughput::RECORD
            .is_multiple_of(redoxfs::BLOCK_SIZE << redoxfs::RECORD_LEVEL)
    );

    // A file this build creates must be readable by this build. The creation level and the ceiling
    // the two `BlockTrait::empty` guards enforce are separate constants since milestone 138, and
    // this is what stops them being separated in the wrong direction.
    assert!(redoxfs::RECORD_LEVEL <= redoxfs::RECORD_LEVEL_MAX);

    // **The file channel is a whole number of blocks** (milestone 138 step 3). A `READ` or `WRITE`
    // now carries up to `fs::TRANSFER_MAX` bytes straight into the shared region, and RedoxFS reads
    // and writes it a record at a time from there; a channel that ended part way through a block
    // would make the last chunk of every full-channel request a partial one, which is the shape the
    // repeat-write bug had. Nothing else in the tree can check this: `fs_proto` has never heard of
    // the store and the store has never heard of `fs_proto`.
    assert!(fs_proto::fs::TRANSFER_MAX.is_multiple_of(BLOCK));
    assert!(fs_proto::fs::TRANSFER_MAX >= BLOCK);
};

/// A block-granular transport: read or write one whole filesystem block, or report the disk size.
/// The EL0 binary implements this over blk IPC; a host test implements it over a `Vec`.
///
/// This exists so the **chunking** that bridges RedoxFS's byte-addressed, arbitrary-length `Disk`
/// calls to whole-block transfers is written once and host-tested, instead of living only in the EL0
/// binary where no host test can reach it. That gap is what let the repeat-write bug hide.
pub trait BlockIo {
    /// Read filesystem block `block` into `buf` (exactly [`BLOCK`] bytes).
    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK]) -> Result<()>;
    /// Write `buf` (exactly [`BLOCK`] bytes) to filesystem block `block`.
    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK]) -> Result<()>;
    /// The disk size, in bytes.
    fn size_bytes(&mut self) -> Result<u64>;
}

/// A RedoxFS [`Disk`] over a [`BlockIo`]. RedoxFS calls `read_at`/`write_at` with a block index and a
/// buffer that may be shorter than a block (a compressed record), exactly a block, or many blocks
/// long (an uncompressed 128 KiB record), so this splits each call into whole-block transfers.
///
/// A write whose final chunk is short is **read-modify-written**: the block is read first, the chunk
/// overwrites its front, and the whole block goes back. That preserves the bytes past the chunk,
/// which is what a byte-addressed disk (`DiskFile`, where `write_at` writes exactly the buffer) does,
/// and RedoxFS relies on it for compressed records.
pub struct BlockDisk<T>(pub T);

impl<T: BlockIo> Disk for BlockDisk<T> {
    unsafe fn read_at(&mut self, block: u64, buffer: &mut [u8]) -> Result<usize> {
        let mut scratch = [0u8; BLOCK];
        for (i, chunk) in buffer.chunks_mut(BLOCK).enumerate() {
            self.0.read_block(block + i as u64, &mut scratch)?;
            chunk.copy_from_slice(&scratch[..chunk.len()]);
        }
        Ok(buffer.len())
    }

    unsafe fn write_at(&mut self, block: u64, buffer: &[u8]) -> Result<usize> {
        let mut scratch = [0u8; BLOCK];
        for (i, chunk) in buffer.chunks(BLOCK).enumerate() {
            let b = block + i as u64;
            if chunk.len() < BLOCK {
                self.0.read_block(b, &mut scratch)?;
            }
            scratch[..chunk.len()].copy_from_slice(chunk);
            self.0.write_block(b, &scratch)?;
        }
        Ok(buffer.len())
    }

    fn size(&mut self) -> Result<u64> {
        self.0.size_bytes()
    }
}

/// **A direct-mapped, write-through cache of single-block reads** (milestone 138 step 2). Every
/// slot holds at most one `(block number, block bytes)` pair, chosen by `block % capacity`; a
/// collision simply evicts, which is safe because a miss just falls through to the disk.
///
/// **Why this and not an LRU.** An LRU would hit more often at the same slot count, at the cost of
/// bookkeeping (a recency list, a pointer to update on every hit) this cache does not need to be
/// correct: [`BlockCache::get`] and [`BlockCache::insert`] are each one array index and one
/// comparison. The working set this cache exists for is small and stable rather than large and
/// shifting: one open file's tree spine is five blocks, so a capacity a few times that comfortably
/// holds it without needing to rank which block is "least recently used", and the fewer-moving-parts
/// option wins when it costs nothing to reach.
struct BlockCache {
    slots: Vec<Option<(u64, [u8; BLOCK])>>,
}

impl BlockCache {
    /// `capacity` slots, empty. `capacity` must be at least 1; the caller picks it against a
    /// process's own heap budget, which this type has no way to see.
    fn new(capacity: usize) -> Self {
        let mut slots = Vec::with_capacity(capacity);
        slots.resize(capacity, None);
        Self { slots }
    }

    fn slot(&self, block: u64) -> usize {
        (block as usize) % self.slots.len()
    }

    /// The cached bytes for `block`, or `None` on a miss (never cached, or evicted by a collision).
    fn get(&self, block: u64) -> Option<&[u8; BLOCK]> {
        match &self.slots[self.slot(block)] {
            Some((b, data)) if *b == block => Some(data),
            _ => None,
        }
    }

    /// Record `block`'s current bytes, replacing whatever the slot held (its own entry to refresh,
    /// or a different block's to evict). `data` must be exactly [`BLOCK`] bytes.
    fn insert(&mut self, block: u64, data: &[u8]) {
        let i = self.slot(block);
        let mut arr = [0u8; BLOCK];
        arr.copy_from_slice(data);
        self.slots[i] = Some((block, arr));
    }

    /// Drop `block`'s entry if it is the one occupying its slot. A no-op if the slot holds a
    /// different block (already evicted) or nothing.
    fn invalidate(&mut self, block: u64) {
        let i = self.slot(block);
        if matches!(&self.slots[i], Some((b, _)) if *b == block) {
            self.slots[i] = None;
        }
    }
}

/// **A [`Disk`] wrapping another, caching single-block reads** (milestone 138 step 2). Built to
/// close the residual steps 1, 3 and 4 all left standing: `Transaction::read_tree_and_addr`'s
/// four-level tree walk plus the target node, five single-block reads issued fresh on every
/// `Server::read`/`write`/`fstat`/... call even when the last call resolved the very same node
/// (notes/fs-server.md, "the same five blocks every time"). Nothing below [`CachedDisk`] changed to
/// make this true; it was always true of `Transaction::read_tree`, and steps 1, 3 and 4 all left it
/// alone because none of them touch how a node is found, only what is done once it is.
///
/// # Why single blocks only, and why write-through rather than read-through-and-forget
///
/// **Only a `buffer.len() == BLOCK` read consults the cache.** A record body ([`BlockDisk`] and the
/// EL0 binary's `IpcDisk` both call `Disk::read_at`/`write_at` with a multi-block buffer for one) is
/// already the batched call step 4 built, and caching a whole record would spend far more of this
/// process's small heap budget for far less repetition: a record is read once per access, not five
/// times per access the way the tree spine is. Bypassing the cache for anything wider than one
/// block keeps the memory this type owns proportional to the metadata working set it targets, not
/// to the data moving through it.
///
/// **A write updates the cache rather than only invalidating it**, once the write to the inner disk
/// has actually succeeded (never before: caching bytes a write has not been confirmed to have
/// landed would make the cache lie about a write that failed partway). A short final chunk (the
/// same case [`BlockDisk`]'s doc names) still touches one whole block on the platter through a
/// read-modify-write below this type, and this type was not given the merged result, so it
/// invalidates that block's slot instead of caching a guess.
///
/// # What makes it safe against RedoxFS's copy-on-write allocator
///
/// RedoxFS never rewrites a live block in place: a change always lands at a **freshly allocated**
/// address and the old one is only deallocated, never immediately reused, until a later allocation
/// claims it (`Transaction::allocate`/`deallocate`). So a cache entry for an address can only ever
/// go stale by that same address being **written**, and this type's [`Disk::write_at`] is the only
/// path a write reaches the platter through; there is no way for the bytes at a cached address to
/// change without this type observing it and updating or invalidating the entry in the same call.
/// This is the property that makes a bare write-through cache correct here without a generation
/// counter or a fencing scheme: one process, one `Disk` impl, every write funnelled through it.
pub struct CachedDisk<D> {
    inner: D,
    cache: BlockCache,
}

impl<D> CachedDisk<D> {
    /// Wrap `inner`, caching up to `capacity` distinct single-block reads. `capacity` is the
    /// caller's call, against its own heap budget: milestone 37's crash-test FS servers run a
    /// fraction of the ordinary heap and must pick a smaller number than the ordinary server does.
    pub fn new(inner: D, capacity: usize) -> Self {
        Self {
            inner,
            cache: BlockCache::new(capacity),
        }
    }
}

impl<D: Disk> Disk for CachedDisk<D> {
    unsafe fn read_at(&mut self, block: u64, buffer: &mut [u8]) -> Result<usize> {
        if buffer.len() == BLOCK {
            if let Some(cached) = self.cache.get(block) {
                buffer.copy_from_slice(cached);
                return Ok(BLOCK);
            }
            // SAFETY: forwarded from this method's own caller, on a buffer of the same shape.
            let n = unsafe { self.inner.read_at(block, buffer)? };
            if n == BLOCK {
                self.cache.insert(block, buffer);
            }
            return Ok(n);
        }
        // A record body or larger: already batched by step 4, and not this cache's target. See the
        // type's own doc for why caching it would cost more than it is worth.
        unsafe { self.inner.read_at(block, buffer) }
    }

    unsafe fn write_at(&mut self, block: u64, buffer: &[u8]) -> Result<usize> {
        // SAFETY: forwarded from this method's own caller, on a buffer of the same shape.
        let n = unsafe { self.inner.write_at(block, buffer)? };
        // Only the bytes the inner disk confirmed writing are cached; a short count from a
        // hypothetical partial write must not be papered over with an entry that claims more
        // landed than did.
        let mut off = 0;
        let mut b = block;
        while off < n {
            let chunk = (n - off).min(BLOCK);
            if chunk == BLOCK {
                self.cache.insert(b, &buffer[off..off + BLOCK]);
            } else {
                // A short final chunk: the whole-block bytes that actually landed are a merge
                // (read-modify-write) this type never saw, so it cannot vouch for a cached copy.
                self.cache.invalidate(b);
            }
            off += chunk;
            b += 1;
        }
        Ok(n)
    }

    fn size(&mut self) -> Result<u64> {
        self.inner.size()
    }
}

#[cfg(test)]
mod tests {
    use redoxfs::DiskMemory;

    use super::*;

    /// A [`BlockIo`] over an in-memory image, so the exact chunking the EL0 binary uses is exercised
    /// on the host. Byte for byte it must behave like `DiskMemory`.
    struct VecIo(Vec<u8>);
    impl BlockIo for VecIo {
        fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK]) -> Result<()> {
            let off = block as usize * BLOCK;
            buf.copy_from_slice(&self.0[off..off + BLOCK]);
            Ok(())
        }
        fn write_block(&mut self, block: u64, buf: &[u8; BLOCK]) -> Result<()> {
            let off = block as usize * BLOCK;
            self.0[off..off + BLOCK].copy_from_slice(buf);
            Ok(())
        }
        fn size_bytes(&mut self) -> Result<u64> {
            Ok(self.0.len() as u64)
        }
    }

    /// **A sync is a write-side authority, and a read-only capability may not ask for one**
    /// (milestone 55).
    ///
    /// The rights question here is not obvious and the answer is argued at `fs_proto::fs::SYNC`,
    /// so what this pins is the choice rather than a mechanism: a sync is an *action on the
    /// device*, and a device flush stalls the queue for every client of the image. Letting a
    /// read-only holder ask would hand it a lever on everybody else's write latency for no
    /// purpose, because a holder with nothing written has nothing to make durable.
    ///
    /// It also pins the shape: **either kind of handle**, file or directory, because the answer is
    /// about the storage rather than about the node, which is `STATFS`'s rule and the reason this
    /// goes through `node_at` rather than `file_at`.
    #[test]
    fn a_sync_needs_write_and_takes_a_handle_of_either_kind() {
        let mut fs = FileSystem::create(BlockDisk(VecIo(vec![0u8; 16 * 1024 * 1024])), None, 0, 0)
            .expect("mkfs through BlockDisk");
        fs.tx(|tx| {
            tx.create_node(TreePtr::root(), "scratch", Node::MODE_FILE | 0o644, 0, 0)?;
            tx.create_node(TreePtr::root(), "sub", Node::MODE_DIR | 0o755, 0, 0)?;
            Ok(())
        })
        .expect("populate");
        let image = fs.disk.0.0;
        let mut srv = Server::open(BlockDisk(VecIo(image))).expect("Server::open");

        // The bound directory carries every right, so it may ask.
        assert!(srv.sync_permitted(fs_proto::fs::ROOT as u32).is_ok());

        // A file handle opened through it inherits READ | WRITE, so it may too: the verb is not
        // about the file, and refusing a file handle would be inventing a distinction.
        let file = srv.open_file("scratch").expect("open scratch");
        assert!(srv.sync_permitted(file).is_ok());

        // A directory capability narrowed to reading may not. EROFS rather than EACCES, because
        // through this capability the storage takes no writes and there is no policy that could
        // have said yes.
        let ro = srv
            .open_dir(
                fs_proto::fs::ROOT as u32,
                "sub",
                dir::ENUMERATE | dir::READ | dir::DESCEND,
            )
            .expect("open sub read-only");
        assert_eq!(
            srv.sync_permitted(ro).unwrap_err().errno,
            dir::EROFS,
            "a read-only capability must not be able to make the whole device flush"
        );

        // And a handle nobody minted is EBADF, which is the check that makes the right meaningful:
        // a forged handle must not be a way around it.
        assert_eq!(srv.sync_permitted(9999).unwrap_err().errno, EBADF);
    }

    /// **Repeat writes through the EL0 binary's exact chunking.** The device's `IpcDisk` and this
    /// `BlockDisk` split `Disk` calls the same way, so if the chunking is what breaks a repeat write
    /// (the partial-block read-modify-write is the suspicious part, and compression plus a growing
    /// allocator log exercise it more on later writes), it breaks here, on the host, in milliseconds.
    #[test]
    fn repeat_writes_through_the_block_chunking_do_not_loop() {
        let mut fs = FileSystem::create(BlockDisk(VecIo(vec![0u8; 16 * 1024 * 1024])), None, 0, 0)
            .expect("mkfs through BlockDisk");
        fs.tx(|tx| {
            let ptr = tx
                .create_node(TreePtr::root(), "scratch", Node::MODE_FILE | 0o644, 0, 0)?
                .ptr();
            tx.write_node(ptr, 0, b"(placeholder)", 0, 0)?;
            Ok(())
        })
        .expect("populate");
        let image = fs.disk.0.0;

        let mut srv = Server::open(BlockDisk(VecIo(image))).expect("Server::open");
        let h = srv.open_file("scratch").expect("open scratch");
        let mut buf = [0u8; 128];
        for pass in 1..=3u8 {
            let payload = repeat_write_payload(pass);
            assert_eq!(srv.write(h, 0, &payload).unwrap(), payload.len());
            let n = srv.read(h, 0, &mut buf).unwrap();
            assert_eq!(&buf[..n], &payload[..], "pass {pass} through the chunking");
        }
    }

    /// Repeat writes of a **multi-record** payload, so the chunking's multi-block path and the
    /// compressed-record partial tail both get exercised across generations.
    ///
    /// **The length is derived from the record size rather than written down**, and milestone 138 is
    /// why. This used to say `160 * 1024`, chosen because it was 1.25 of the 128 KiB record of the
    /// day; at the 8 KiB record that milestone moved to, the same constant is exactly twenty whole
    /// records and the partial tail this test exists to reach **stops existing**, silently, with the
    /// test still passing. Two whole records plus one block is that shape at any level, so the
    /// coverage cannot be lost by a constant moving somewhere else.
    #[test]
    fn repeat_record_sized_writes_through_the_chunking_do_not_loop() {
        let mut fs = FileSystem::create(BlockDisk(VecIo(vec![0u8; 32 * 1024 * 1024])), None, 0, 0)
            .expect("mkfs");
        fs.tx(|tx| {
            tx.create_node(TreePtr::root(), "big", Node::MODE_FILE | 0o644, 0, 0)?;
            Ok(())
        })
        .expect("populate");
        let image = fs.disk.0.0;

        let mut srv = Server::open(BlockDisk(VecIo(image))).expect("open");
        let h = srv.open_file("big").expect("open big");
        let len = 2 * (redoxfs::BLOCK_SIZE << redoxfs::RECORD_LEVEL) as usize + BLOCK;
        for pass in 1..=3u8 {
            // Position-dependent and pass-dependent, and deliberately not compressible to a tiny
            // size, so the record path does real work each pass.
            let data: Vec<u8> = (0..len)
                .map(|i| ((i as u32).wrapping_mul(2_654_435_761) >> 16) as u8 ^ pass)
                .collect();
            assert_eq!(srv.write(h, 0, &data).unwrap(), len);
            let mut back = vec![0u8; len];
            let n = srv.read(h, 0, &mut back).unwrap();
            assert_eq!(n, len, "pass {pass} short read");
            assert!(back == data, "pass {pass} record data mismatch");
        }
    }

    /// Build a 16 MiB RedoxFS image in memory with the given files in its root (using the std
    /// creation APIs the host tool uses), then reopen it through a fresh [`Server`], exactly as the
    /// running system does: the host tool makes the image, the server only ever opens it.
    fn server_with(files: &[(&str, &[u8])]) -> Server<DiskMemory> {
        Server::open(image_with(files)).expect("open")
    }

    /// The disk behind [`server_with`], as a standalone [`DiskMemory`], so a test can open it, drop
    /// the server, and reopen the same disk to prove persistence across a close.
    fn image_with(files: &[(&str, &[u8])]) -> DiskMemory {
        let disk = DiskMemory::new(16 * 1024 * 1024);
        let mut fs = FileSystem::create(disk, None, 0, 0).expect("create");
        fs.tx(|tx| {
            for (name, data) in files {
                let ptr = tx
                    .create_node(TreePtr::root(), name, Node::MODE_FILE | 0o644, 0, 0)?
                    .ptr();
                tx.write_node(ptr, 0, data, 0, 0)?;
            }
            Ok(())
        })
        .expect("populate");
        fs.disk
    }

    #[test]
    fn opens_and_reads_a_file_the_host_tool_wrote() {
        let mut srv = server_with(&[("motd", b"hello from redoxfs\n")]);
        let h = srv.open_file("motd").expect("open motd");
        let mut buf = [0u8; 64];
        let n = srv.read(h, 0, &mut buf).expect("read");
        assert_eq!(&buf[..n], b"hello from redoxfs\n");
    }

    #[test]
    fn a_missing_name_is_enoent_and_names_do_not_walk() {
        let mut srv = server_with(&[("present", b"x")]);
        assert_eq!(
            srv.open_file("absent").unwrap_err().errno,
            syscall::error::ENOENT
        );
        // A name is one component resolved in the bound directory, never a path: a slash does not
        // descend, so "present/nope" finds nothing rather than walking anywhere.
        assert!(srv.open_file("present/nope").is_err());
    }

    #[test]
    fn a_write_persists_and_reads_back() {
        let mut srv = server_with(&[("scratch", &[0u8; 32])]);
        let h = srv.open_file("scratch").unwrap();
        let payload = b"CRKWRIT1 and then some position-dependent bytes here";
        let n = srv.write(h, 0, payload).unwrap();
        assert_eq!(n, payload.len());
        let mut buf = [0u8; 128];
        let m = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(&buf[..m], payload);
        assert_eq!(srv.fstat(h).unwrap(), payload.len() as u64);
    }

    /// The payload for pass `n` of the repeat-write test: a fixed 64 bytes, tagged with the pass and
    /// position-dependent after that, so every pass has the same length (no truncation confusion) and
    /// a stale or shifted read cannot match. Shared with the on-device client through
    /// `fs_proto::fixture` where that matters.
    fn repeat_write_payload(pass: u8) -> [u8; 64] {
        let mut p = [0u8; 64];
        p[..8].copy_from_slice(b"CRKRPT__");
        p[7] = b'0' + pass;
        for (i, b) in p.iter_mut().enumerate().skip(8) {
            *b = (i as u8).wrapping_mul(31) ^ pass;
        }
        p
    }

    /// **The repeat-write gate** (fix/redoxfs-repeat-write). A first write to a pristine block
    /// worked all along; a write to a block that has ALREADY been written is what loops on device.
    /// The old gate never saw it because `mkredoxfs` rewrites the target to a placeholder before
    /// every run, so every gated write was a first write and the bug hid behind the harness.
    ///
    /// This writes the same file TWICE in one run, which is the honest reproduction: it depends on
    /// nothing left over from a previous invocation. It is also the decisive host-vs-device
    /// comparison: if this loops, the bug is reachable with no nife runtime at all (upstream or
    /// our chunking); if it passes, the divergence is ours, in the device I/O path.
    #[test]
    fn a_second_write_to_the_same_block_does_not_loop() {
        let mut srv = server_with(&[("scratch", b"(placeholder)")]);
        let h = srv.open_file("scratch").unwrap();

        // Equal-length, position-dependent payloads: each write fully replaces the last (a shorter
        // write at offset 0 does not truncate, which is correct filesystem behaviour but would muddy
        // the comparison), and a buffer that came back shifted or stale cannot match.
        let mut buf = [0u8; 128];
        for pass in 1..=3u8 {
            let payload = repeat_write_payload(pass);
            assert_eq!(
                srv.write(h, 0, &payload).unwrap(),
                payload.len(),
                "write {pass} was short"
            );
            let n = srv.read(h, 0, &mut buf).unwrap();
            assert_eq!(&buf[..n], &payload[..], "write {pass} did not read back");
        }
    }

    /// **The real trigger** (fix/redoxfs-repeat-write): write, drop the mount WITHOUT a clean
    /// unmount, reopen, and write again. Every existing test stopped short of this. The one that
    /// looked closest, `a_write_survives_a_full_close_and_reopen`, only READS after the reopen.
    ///
    /// This is exactly what the gate does across its two ISA legs: `mkredoxfs` runs once, the aarch64
    /// leg mounts and writes (a first write, which works), the process dies without unmounting, and
    /// then the riscv leg mounts the same image and writes again. That second boot's write is what
    /// fails, which is why the bug looked like "repeat write" and why one leg passing hid it.
    #[test]
    fn a_write_after_a_reopen_of_a_previously_written_image_does_not_loop() {
        let mut disk = image_with(&[("scratch", b"(placeholder)")]);

        // Boot 1: mount, write, and drop the mount the way a dying process does (no unmount).
        {
            let mut srv = Server::open(disk).expect("open 1");
            let h = srv.open_file("scratch").expect("open scratch 1");
            let p1 = repeat_write_payload(1);
            assert_eq!(srv.write(h, 0, &p1).unwrap(), p1.len());
            disk = srv.fs.disk; // take the disk back; no unmount, as on device
        }

        // Boot 2: mount the image boot 1 wrote, and write again.
        let mut srv = Server::open(disk).expect("open 2");
        let h = srv.open_file("scratch").expect("open scratch 2");
        let p2 = repeat_write_payload(2);
        assert_eq!(srv.write(h, 0, &p2).unwrap(), p2.len(), "boot-2 write");
        let mut buf = [0u8; 128];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], &p2[..], "boot-2 write did not read back");
    }

    /// **The mechanism behind the "cross-boot second-mount write failure", and it is not a filesystem
    /// bug at all** (fix/redoxfs-second-mount). There is no TRUNCATE verb, so a write shorter than the
    /// file leaves the previous write's tail in place. That is correct behaviour, and it is what the
    /// contract offers today, but it is sharp: a test that writes N bytes and then compares a
    /// *whole-file* read against those N bytes passes only if the file was not already longer.
    ///
    /// That is exactly what happened. One boot's client left a 64-byte payload in `scratch`; the next
    /// boot's `std::fs` test wrote its 61-byte pattern and asserted the whole file equalled it, got 64
    /// bytes back, and panicked in the write block. It looked like "a second mount of a used image
    /// fails its write", and three rounds chased a filesystem bug that was never there. The write
    /// succeeded every time.
    ///
    /// The byte counts here are the real ones so the arithmetic is legible rather than abstract.
    #[test]
    fn a_shorter_write_does_not_truncate_and_that_is_what_broke_across_boots() {
        let mut srv = server_with(&[("scratch", b"(placeholder)")]);
        let h = srv.open_file("scratch").unwrap();

        // Boot 1's client wrote 64 bytes.
        let long = [b'L'; 64];
        assert_eq!(srv.write(h, 0, &long).unwrap(), 64);
        assert_eq!(srv.fstat(h).unwrap(), 64);

        // Boot 2's std::fs wrote its 61-byte pattern over it. The write itself is fine.
        let short = [b'S'; 61];
        assert_eq!(srv.write(h, 0, &short).unwrap(), 61);

        // And the file is STILL 64 bytes, because nothing truncates it.
        assert_eq!(
            srv.fstat(h).unwrap(),
            64,
            "a shorter write must not truncate; TRUNCATE is the verb that fixes this, not WRITE"
        );
        let mut buf = [0u8; 128];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(n, 64, "a whole-file read returns the OLD length");
        assert_eq!(&buf[..61], &short[..], "the new bytes landed");
        assert_eq!(
            &buf[61..64],
            b"LLL",
            "and the longer write's tail survives, which is what failed the comparison"
        );
    }

    #[test]
    fn a_write_survives_a_full_close_and_reopen() {
        // Persistence across a full close/reopen is what the on-disk image buys, and it is the
        // property the kill-mid-write test leans on: a reopen sees the committed write.
        let disk = image_with(&[("f", b"")]);
        let mut srv = Server::open(disk).unwrap();
        let h = srv.open_file("f").unwrap();
        srv.write(h, 0, b"persisted").unwrap();
        let disk = srv.fs.disk; // the FileSystem committed on each tx; take the disk back
        let mut srv = Server::open(disk).unwrap();
        let h = srv.open_file("f").unwrap();
        let mut buf = [0u8; 16];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"persisted");
    }

    #[test]
    fn a_forged_or_closed_handle_is_ebadf() {
        let mut srv = server_with(&[("f", b"data")]);
        let h = srv.open_file("f").unwrap();
        let mut buf = [0u8; 8];
        assert_eq!(srv.read(999, 0, &mut buf).unwrap_err().errno, EBADF);
        srv.close(h).unwrap();
        assert_eq!(srv.read(h, 0, &mut buf).unwrap_err().errno, EBADF);
        assert_eq!(srv.close(h).unwrap_err().errno, EBADF);
    }

    #[test]
    fn handles_are_reused_after_close() {
        let mut srv = server_with(&[("a", b"1"), ("b", b"2")]);
        let h0 = srv.open_file("a").unwrap();
        assert_ne!(
            h0, 0,
            "slot 0 is the bound directory and is never handed out"
        );
        srv.close(h0).unwrap();
        let h1 = srv.open_file("b").unwrap();
        assert_eq!(h0, h1, "a freed slot is reused before growing the table");
    }

    // --- The directory capability (milestone 47) ---

    /// Build an image with a subtree: files in the root, then `sub/` holding `inner` and `deeper/`,
    /// and a sibling `other/` holding `secret`. The same shape `fs_proto::fixture::tree` describes
    /// and the device fixture carries, so the host tests and the guest tests attack one geometry.
    fn image_with_tree() -> DiskMemory {
        let disk = DiskMemory::new(16 * 1024 * 1024);
        let mut fs = FileSystem::create(disk, None, 0, 0).expect("create");
        fs.tx(|tx| {
            let file = |tx: &mut redoxfs::Transaction<DiskMemory>,
                        parent: TreePtr<Node>,
                        name: &str,
                        body: &[u8]| {
                let ptr = tx
                    .create_node(parent, name, Node::MODE_FILE | 0o644, 0, 0)?
                    .ptr();
                tx.write_node(ptr, 0, body, 0, 0)?;
                Ok::<(), Error>(())
            };
            file(tx, TreePtr::root(), "motd", b"root motd")?;
            let sub = tx
                .create_node(TreePtr::root(), "sub", Node::MODE_DIR | 0o755, 0, 0)?
                .ptr();
            file(tx, sub, "inner", b"inside the grant")?;
            let deeper = tx
                .create_node(sub, "deeper", Node::MODE_DIR | 0o755, 0, 0)?
                .ptr();
            file(tx, deeper, "leaf", b"two levels down")?;
            let other = tx
                .create_node(TreePtr::root(), "other", Node::MODE_DIR | 0o755, 0, 0)?
                .ptr();
            file(tx, other, "secret", b"in the sibling")?;
            Ok(())
        })
        .expect("populate the tree");
        fs.disk
    }

    fn server_with_tree() -> Server<DiskMemory> {
        Server::open(image_with_tree()).expect("open")
    }

    /// Collect a whole directory listing through `read_dir`, driving the cursor the way a client
    /// does, so the cursor and the "a record is never split" rule are exercised rather than assumed.
    fn list(
        srv: &mut Server<DiskMemory>,
        handle: u32,
        buf_len: usize,
    ) -> Result<Vec<(String, bool)>> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; buf_len];
        let mut cursor = 0u32;
        // Bounded so a cursor that fails to advance fails the test instead of hanging it.
        for _ in 0..64 {
            let n = srv.read_dir(handle, cursor, &mut buf)?;
            if n == 0 {
                return Ok(out);
            }
            for (name, is_dir) in fs_proto::dirent::iter(&buf[..n]) {
                out.push((String::from_utf8(name.to_vec()).unwrap(), is_dir));
                cursor += 1;
            }
        }
        panic!("read_dir never reported the end of the directory");
    }

    /// **The keystone: descending hands back a capability, and it is a capability to that directory
    /// and nothing else.** A handle to `sub` opens `inner` and does not open `motd`, which is in the
    /// parent, or `secret`, which is in a sibling. Neither is a permission failure: under `sub`
    /// there is no such name.
    #[test]
    fn a_child_directory_handle_names_only_what_is_in_it() {
        let mut srv = server_with_tree();
        let sub = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL)
            .unwrap();

        let inner = srv.open_file_at(sub, "inner").expect("its own file opens");
        let mut buf = [0u8; 64];
        let n = srv.read(inner, 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"inside the grant");

        for absent in ["motd", "other", "secret"] {
            assert_eq!(
                srv.open_file_at(sub, absent).err().map(|e| e.errno),
                Some(syscall::error::ENOENT),
                "{absent} is not under `sub`, so naming it must find nothing",
            );
        }
        // And the same for descending: the sibling directory is real and is not reachable from here.
        assert_eq!(
            srv.open_dir(sub, "other", dir::ALL).err().map(|e| e.errno),
            Some(syscall::error::ENOENT),
        );
        // `..` never resolves, at any rights, because it is not a component this contract accepts.
        assert_eq!(
            srv.open_dir(sub, "..", dir::ALL).err().map(|e| e.errno),
            Some(EINVAL),
        );
    }

    /// **A child's rights can never exceed its parent's**, and asking for more is refused rather
    /// than quietly granted less. Both halves matter: the refusal is the truthfulness, and the
    /// attenuation underneath it is the safety.
    #[test]
    fn a_child_directory_cannot_carry_a_right_the_parent_lacked() {
        let mut srv = server_with_tree();
        // A parent that may descend and read, and may not create, write or remove.
        let narrow = dir::DESCEND | dir::READ | dir::ENUMERATE;
        let sub = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", narrow)
            .unwrap();

        assert_eq!(
            srv.open_dir(sub, "deeper", narrow | dir::CREATE)
                .err()
                .map(|e| e.errno),
            Some(EPERM),
            "asking for a right the parent lacks must be refused, not silently narrowed",
        );
        // Asking for what the parent has, or less, works, and the child is bounded either way.
        let deeper = srv.open_dir(sub, "deeper", narrow).unwrap();
        assert_eq!(
            srv.create_file_at(deeper, "smuggled")
                .err()
                .map(|e| e.errno),
            Some(EROFS),
            "a grandchild must not have gained the create right the root grant withheld",
        );
        // And the parent itself still cannot create, so the child did not come from a wider parent.
        assert_eq!(
            srv.create_file_at(sub, "smuggled").err().map(|e| e.errno),
            Some(EROFS),
        );
    }

    /// The five rungs are separable, each gating exactly its own verb. Written as a sweep rather
    /// than five tests because the interesting failure is a verb that answers to the *wrong* right,
    /// and that only shows up when every combination is tried against every verb.
    #[test]
    fn each_rung_of_the_ladder_gates_exactly_its_own_verb() {
        // `withheld` is what the failing capability lacks and `needed` is what the passing one has.
        // They differ for `open`, which takes READ **or** WRITE, so withholding only READ would
        // leave it reachable and the sweep would be asserting the wrong thing (it did, first time).
        for &(withheld, needed, ok, refusal) in &[
            (dir::ENUMERATE, dir::ENUMERATE, "enumerate", EPERM),
            (
                dir::READ | dir::WRITE,
                dir::READ,
                "open",
                syscall::error::ENOENT,
            ),
            (dir::CREATE, dir::CREATE, "create", EROFS),
            (
                dir::DESCEND,
                dir::DESCEND,
                "descend",
                syscall::error::ENOENT,
            ),
            // `rm`'s rung. Until `unlink` existed only `rename` consulted REMOVE, and a rename
            // needs CREATE as well, so this is the first probe that isolates this one.
            (dir::REMOVE, dir::REMOVE, "unlink", EROFS),
        ] {
            let mut srv = server_with_tree();
            // Everything except the rung under test, so a verb that consults the wrong bit passes
            // when it should fail.
            let without = srv
                .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL & !withheld)
                .unwrap();
            let with = srv
                .open_dir(fs_proto::fs::ROOT as u32, "sub", needed | dir::READ)
                .unwrap();

            let attempt = |srv: &mut Server<DiskMemory>, h: u32| -> Result<()> {
                match ok {
                    "enumerate" => srv.read_dir(h, 0, &mut [0u8; 256]).map(|_| ()),
                    "open" => srv.open_file_at(h, "inner").map(|_| ()),
                    "create" => srv.create_file_at(h, "made-by-the-sweep").map(|_| ()),
                    "unlink" => srv.unlink(h, "inner"),
                    _ => srv.open_dir(h, "deeper", 0).map(|_| ()),
                }
            };
            assert_eq!(
                attempt(&mut srv, without).err().map(|e| e.errno),
                Some(refusal),
                "{ok} was allowed by a capability that lacked its own right",
            );
            assert!(
                attempt(&mut srv, with).is_ok(),
                "{ok} was refused by a capability that carried its right, so the refusal above \
                 proves nothing",
            );
        }
    }

    /// **`unlink` removes a name and not an object**, which is the half of `rm` Unix conflated and
    /// milestone 47 insists on separating. Three claims in one test because they are one property:
    /// the name goes, the holder keeps reading, and the bytes it reads are still the right ones.
    ///
    /// This is the test that decided the implementation. RedoxFS frees a node the instant its last
    /// link goes, so the first version of this verb was a *revoke*: the read below returned garbage.
    /// Registering an open node with the engine's usage table is what turns it into an unlink, and
    /// deleting that one line turns this test red.
    #[test]
    fn unlink_takes_the_name_and_leaves_the_object_for_whoever_holds_it() {
        let mut srv = server_with_tree();
        let sub = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL)
            .unwrap();

        let h = srv.create_file_at(sub, "doomed").unwrap();
        let body = b"the bytes outlive the name";
        assert_eq!(srv.write(h, 0, body).unwrap(), body.len());

        srv.unlink(sub, "doomed").expect("unlink");

        // The name is gone: from an open, and from a listing.
        assert_eq!(
            srv.open_file_at(sub, "doomed").err().map(|e| e.errno),
            Some(syscall::error::ENOENT),
            "the name must be gone the moment it is unlinked",
        );
        let names: Vec<String> = list(&mut srv, sub, 256)
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(!names.contains(&"doomed".to_string()), "listing: {names:?}");

        // And the object is not: the handle opened before the unlink still reads its bytes.
        let mut buf = [0u8; 64];
        let n = srv.read(h, 0, &mut buf).expect("a holder keeps reading");
        assert_eq!(
            &buf[..n],
            body,
            "unlink removed the object, not just the name: this verb is a revoke wearing an \
             unlink's name",
        );

        // The close is what finally collects it, and it must not fail on a node whose name is gone.
        srv.close(h).expect("close after unlink");

        // The same property through the OTHER door. `create` and `open` register the node
        // separately, so a test that only creates would leave half the verb unproven; deleting the
        // registration in `open_file_at` alone left this test green the first time it was run.
        let h = srv
            .open_file_at(sub, "inner")
            .expect("open an existing file");
        srv.unlink(sub, "inner").expect("unlink what was opened");
        let n = srv
            .read(h, 0, &mut buf)
            .expect("an opener keeps reading too");
        assert_eq!(&buf[..n], b"inside the grant");
        srv.close(h).unwrap();
    }

    /// **`RMDIR` takes an empty directory and refuses a non-empty one**, which is the whole safety
    /// property: `mkdir` now has an inverse, and no single call on this contract can take a subtree
    /// away.
    ///
    /// The pair is what makes it mean anything. A `rmdir` that refused everything would satisfy
    /// "non-empty is refused" perfectly, so the same directory is emptied by hand and removed
    /// through the same call, and the name really is gone from the listing afterwards.
    #[test]
    fn rmdir_removes_an_empty_directory_and_refuses_one_with_a_name_in_it() {
        let mut srv = server_with_tree();
        let sub = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL)
            .unwrap();

        // `deeper` holds `leaf`, so this is exactly the subtree-in-one-word case.
        assert_eq!(
            srv.rmdir(sub, "deeper").err().map(|e| e.errno),
            Some(syscall::error::ENOTEMPTY),
            "a `rmdir` that emptied what it found would put a whole subtree behind one call",
        );
        let names: Vec<String> = list(&mut srv, sub, 256)
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(
            names.contains(&"deeper".to_string()),
            "the refused directory is still there: {names:?}",
        );

        // Empty it the way `rm -r` does: from the bottom, one safe step at a time.
        let deeper = srv.open_dir(sub, "deeper", dir::ALL).unwrap();
        srv.unlink(deeper, "leaf").expect("unlink the leaf");
        srv.close(deeper).expect("close before removing the name");
        srv.rmdir(sub, "deeper").expect("an empty directory goes");

        let names: Vec<String> = list(&mut srv, sub, 256)
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(!names.contains(&"deeper".to_string()), "listing: {names:?}");
        assert_eq!(
            srv.open_dir(sub, "deeper", dir::ALL).err().map(|e| e.errno),
            Some(syscall::error::ENOENT),
            "the name must be gone from the parent, not merely absent from a listing",
        );
    }

    /// `RMDIR`'s refusals, each of which is a fact about the thing or about the capability.
    ///
    /// A **file** is `ENOTDIR`, the mirror of `unlink`'s `EISDIR`: neither verb removes the kind the
    /// other one is for, so there is no spelling of "remove this name whatever it is". And the right
    /// is checked before the name is resolved, so a capability that may not remove cannot use this
    /// verb as an existence oracle either.
    #[test]
    fn rmdir_refuses_a_file_a_missing_name_a_path_and_a_capability_without_remove() {
        let mut srv = server_with_tree();
        let sub = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL)
            .unwrap();

        assert_eq!(
            srv.rmdir(sub, "inner").err().map(|e| e.errno),
            Some(ENOTDIR),
            "`rmdir` of a file must be refused: `unlink` is the verb for one",
        );
        assert_eq!(
            srv.rmdir(sub, "never-existed").err().map(|e| e.errno),
            Some(syscall::error::ENOENT),
        );
        for path in ["..", "a/b", "."] {
            assert_eq!(
                srv.rmdir(sub, path).err().map(|e| e.errno),
                Some(EINVAL),
                "`{path}` is not a component, so it cannot name a directory to remove",
            );
        }

        let ro = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL & !dir::REMOVE)
            .unwrap();
        assert_eq!(srv.rmdir(ro, "deeper").err().map(|e| e.errno), Some(EROFS));
        assert_eq!(
            srv.rmdir(ro, "never-existed").err().map(|e| e.errno),
            Some(EROFS),
            "a name that is there and one that is not must answer the same word",
        );
    }

    /// **The whole of `rm -r`, driven against the core**: walk, unlink the files, remove the empty
    /// directories bottom-up. It is here because that is the shape `user/src/rm.rs` implements over
    /// IPC, and proving the *order* is safe belongs next to the verbs rather than only in a guest
    /// test that takes an emulator to run.
    ///
    /// The load-bearing assertion is the second one: **the bottom-up order is forced.** Removing
    /// `deeper` before its `leaf` is `ENOTEMPTY`, so a walk that got the order wrong cannot pass by
    /// accident, and there is no call that would have let it.
    #[test]
    fn a_recursive_removal_is_a_loop_of_individually_safe_steps() {
        let mut srv = server_with_tree();
        let root = fs_proto::fs::ROOT as u32;

        assert_eq!(
            srv.rmdir(root, "sub").err().map(|e| e.errno),
            Some(syscall::error::ENOTEMPTY),
            "top-down is refused at the first step, which is what makes the walk necessary",
        );

        // Depth first, exactly as the program does it.
        let sub = srv.open_dir(root, "sub", dir::ALL).unwrap();
        let deeper = srv.open_dir(sub, "deeper", dir::ALL).unwrap();
        for (name, is_dir) in list(&mut srv, deeper, 256).unwrap() {
            assert!(!is_dir, "the fixture's bottom level holds only files");
            srv.unlink(deeper, &name).expect("unlink at the bottom");
        }
        srv.close(deeper).unwrap();
        srv.rmdir(sub, "deeper").expect("now it is empty");
        for (name, is_dir) in list(&mut srv, sub, 256).unwrap() {
            assert!(!is_dir, "`deeper` was the only directory in `sub`");
            srv.unlink(sub, &name).expect("unlink one level up");
        }
        srv.close(sub).unwrap();
        srv.rmdir(root, "sub").expect("and the top goes last");

        let names: Vec<String> = list(&mut srv, root, 256)
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(!names.contains(&"sub".to_string()), "root holds {names:?}");
        // And nothing outside the walk moved. The walk removes what it enumerated and nothing else,
        // which is the property the guest test then makes about a *capability* rather than a loop.
        assert!(names.contains(&"other".to_string()));
        assert!(names.contains(&"motd".to_string()));
    }

    /// The refusals, which are what stop `rm` from meaning more than it says. A directory is
    /// `EISDIR` ([`Server::rmdir`] is the verb for one, and it takes only an empty one), a
    /// missing name is `ENOENT`, and a path is `EINVAL` exactly as it is everywhere else here.
    #[test]
    fn unlink_refuses_a_directory_a_missing_name_and_a_path() {
        let mut srv = server_with_tree();
        let sub = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL)
            .unwrap();

        assert_eq!(
            srv.unlink(sub, "deeper").err().map(|e| e.errno),
            Some(EISDIR),
            "`rm` of a directory must be refused, not silently recursive",
        );
        assert_eq!(
            srv.unlink(sub, "never-existed").err().map(|e| e.errno),
            Some(syscall::error::ENOENT),
        );
        for path in ["..", "a/b", "."] {
            assert_eq!(
                srv.unlink(sub, path).err().map(|e| e.errno),
                Some(EINVAL),
                "`{path}` is not a component, so it cannot name anything to remove",
            );
        }
        // The right is checked BEFORE the name is resolved, so a capability that may not remove
        // cannot use this verb as an existence oracle: a name that is there and one that is not
        // must answer the same word.
        let ro = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL & !dir::REMOVE)
            .unwrap();
        assert_eq!(srv.unlink(ro, "inner").err().map(|e| e.errno), Some(EROFS),);
        assert_eq!(
            srv.unlink(ro, "never-existed").err().map(|e| e.errno),
            Some(EROFS),
        );
    }

    /// **A file handle inherits its directory's direction**, which is the "open (read versus write)"
    /// rung. A directory granted for writing yields a file that writes and, deliberately, does not
    /// read; the reverse yields one that reads and refuses to write or truncate.
    #[test]
    fn a_file_handle_carries_the_direction_of_the_directory_it_came_from() {
        let mut srv = server_with_tree();

        let ro = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::DESCEND | dir::READ)
            .unwrap();
        let h = srv.open_file_at(ro, "inner").unwrap();
        let mut buf = [0u8; 32];
        assert!(srv.read(h, 0, &mut buf).is_ok());
        assert_eq!(
            srv.write(h, 0, b"x").err().map(|e| e.errno),
            Some(EROFS),
            "a file opened under a read-only directory must not write",
        );
        assert_eq!(
            srv.truncate(h, 0).err().map(|e| e.errno),
            Some(EROFS),
            "and must not truncate either: a truncate is a write that carries no bytes",
        );

        let wo = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::DESCEND | dir::WRITE)
            .unwrap();
        let h = srv.open_file_at(wo, "inner").unwrap();
        assert!(srv.write(h, 0, b"appended by a write-only grant").is_ok());
        assert_eq!(
            srv.read(h, 0, &mut buf).err().map(|e| e.errno),
            Some(EBADF),
            "a write-only handle is not a readable one, which is POSIX's own answer",
        );
        assert!(srv.fstat(h).is_ok(), "but it can still find the end");
    }

    /// A listing is a rendering of authority: it holds exactly the granted directory's names, never
    /// the parent's or the sibling's, and it says which are directories so a reader need not open
    /// every one to find out.
    #[test]
    fn a_listing_holds_exactly_the_names_in_that_directory() {
        let mut srv = server_with_tree();
        let sub = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL)
            .unwrap();

        let mut got = list(&mut srv, sub, 256).expect("enumerate");
        got.sort();
        assert_eq!(
            got,
            vec![("deeper".to_string(), true), ("inner".to_string(), false)],
            "the listing must be the granted directory's names and their kinds",
        );

        // The root's own listing has the sibling in it, which is what makes the assertion above a
        // statement about the capability rather than about an empty directory.
        let mut root = list(&mut srv, fs_proto::fs::ROOT as u32, 256).expect("enumerate the root");
        root.sort();
        assert!(root.iter().any(|(n, d)| n == "other" && *d));
    }

    /// **A listing bigger than the buffer comes back across several calls, whole records only.**
    /// The cursor is what makes that work, and a buffer sized to cut a record in half is exactly the
    /// case that would otherwise hand a client half a name.
    #[test]
    fn a_listing_larger_than_the_buffer_is_paged_without_tearing_a_name() {
        let mut srv = server_with_tree();
        let root = fs_proto::fs::ROOT as u32;
        for i in 0..12 {
            srv.create_file_at(root, &format!("entry-{i:02}-with-a-longish-name"))
                .unwrap();
        }
        let whole = list(&mut srv, root, 4096).expect("one big listing");
        // Ten bytes fits one short record and no long one, so most calls return one entry and the
        // walk only terminates if the cursor advances correctly.
        let paged = list(&mut srv, root, 40).expect("paged listing");
        assert_eq!(paged, whole, "paging changed the listing");
        assert_eq!(whole.len(), 15, "3 fixture entries plus the 12 made here");
    }

    /// `mkdir` is descend-with-creation: it mints a directory node and hands back a capability to it,
    /// attenuated exactly as a descent is, so making a directory is not a way to mint authority.
    #[test]
    fn mkdir_returns_a_capability_no_wider_than_the_one_that_made_it() {
        let mut srv = server_with_tree();
        // No REMOVE, so nothing made below may remove either.
        let held = dir::DESCEND | dir::CREATE | dir::READ | dir::WRITE | dir::ENUMERATE;
        let sub = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", held)
            .unwrap();

        let made = srv.make_dir(sub, "logs", held).expect("mkdir");
        assert_eq!(
            srv.make_dir(sub, "logs", held).err().map(|e| e.errno),
            Some(EEXIST),
            "mkdir is create, not create-or-open",
        );
        // The capability it handed back really is a directory capability: it takes names.
        let f = srv.create_file_at(made, "today").expect("create in it");
        assert_eq!(srv.write(f, 0, b"a log line\n").unwrap(), 11);
        assert_eq!(
            srv.make_dir(sub, "wider", dir::ALL).err().map(|e| e.errno),
            Some(EPERM),
            "and it cannot be made wider than the directory it was made in",
        );

        // The new directory shows up in its parent's listing, as a directory.
        let mut got = list(&mut srv, sub, 512).unwrap();
        got.sort();
        assert!(got.contains(&("logs".to_string(), true)));
    }

    /// A directory capability granted without `CREATE` is the milestone's motivating sentence: a
    /// program may write into what is there and may not make or destroy anything.
    #[test]
    fn a_write_grant_without_create_can_write_and_cannot_make_a_name() {
        let mut srv = server_with_tree();
        let logs = srv
            .open_dir(
                fs_proto::fs::ROOT as u32,
                "sub",
                dir::DESCEND | dir::READ | dir::WRITE,
            )
            .unwrap();
        let h = srv.open_file_at(logs, "inner").unwrap();
        assert!(srv.write(h, 0, b"the program's own bytes").is_ok());
        assert_eq!(
            srv.create_file_at(logs, "new").err().map(|e| e.errno),
            Some(EROFS),
        );
        assert_eq!(
            srv.make_dir(logs, "new", 0).err().map(|e| e.errno),
            Some(EROFS),
        );
    }

    /// The two typed handles do not answer for each other, and the root cannot be closed. Each of
    /// these is a way for a client to make the server confuse one kind of authority for another.
    #[test]
    fn a_directory_handle_and_a_file_handle_refuse_each_others_verbs() {
        let mut srv = server_with_tree();
        let root = fs_proto::fs::ROOT as u32;
        let sub = srv.open_dir(root, "sub", dir::ALL).unwrap();
        let file = srv.open_file_at(sub, "inner").unwrap();
        let mut buf = [0u8; 32];

        assert_eq!(
            srv.read(sub, 0, &mut buf).err().map(|e| e.errno),
            Some(EISDIR)
        );
        assert_eq!(srv.write(sub, 0, b"x").err().map(|e| e.errno), Some(EISDIR));
        assert_eq!(srv.truncate(sub, 0).err().map(|e| e.errno), Some(EISDIR));
        assert_eq!(
            srv.open_file_at(file, "inner").err().map(|e| e.errno),
            Some(ENOTDIR),
        );
        assert_eq!(
            srv.read_dir(file, 0, &mut buf).err().map(|e| e.errno),
            Some(ENOTDIR),
        );
        assert_eq!(
            srv.open_dir(file, "deeper", 0).err().map(|e| e.errno),
            Some(ENOTDIR),
        );
        // Opening a directory as a file is EISDIR, not a handle to bytes that mean nothing.
        assert_eq!(
            srv.open_file_at(root, "sub").err().map(|e| e.errno),
            Some(EISDIR),
        );
        assert_eq!(
            srv.close(root).err().map(|e| e.errno),
            Some(EINVAL),
            "the root of a namespace is not something the client opened",
        );
        // And a forged handle is still EBADF, from the same one check every verb goes through.
        for h in [99u32, 1000] {
            assert_eq!(
                srv.read_dir(h, 0, &mut buf).err().map(|e| e.errno),
                Some(EBADF)
            );
            assert_eq!(
                srv.open_dir(h, "sub", 0).err().map(|e| e.errno),
                Some(EBADF)
            );
        }
    }

    // --- RENAME (milestone 47, DECISIONS §42's verb) ---

    /// **A rename moves the name and the bytes follow it**, within a directory and across two of
    /// them. The cross-directory case is the one POSIX mandates and the one an application's
    /// temp-file-then-rename idiom needs, so it is not enough for the same-directory case to work.
    #[test]
    fn a_rename_moves_a_name_within_a_directory_and_between_two() {
        let mut srv = server_with_tree();
        let root = fs_proto::fs::ROOT as u32;
        let sub = srv.open_dir(root, "sub", dir::ALL).unwrap();
        let deeper = srv.open_dir(sub, "deeper", dir::ALL).unwrap();

        // Within one directory: `sub/inner` becomes `sub/renamed`, contents intact.
        srv.rename(sub, "inner", sub, "renamed").expect("rename");
        assert_eq!(
            srv.open_file_at(sub, "inner").err().map(|e| e.errno),
            Some(syscall::error::ENOENT),
            "the old name must be gone, not merely shadowed",
        );
        let h = srv.open_file_at(sub, "renamed").expect("the new name");
        let mut buf = [0u8; 64];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"inside the grant");

        // Across two directories, which is the same-filesystem move POSIX mandates be atomic.
        srv.rename(sub, "renamed", deeper, "moved").expect("move");
        assert_eq!(
            srv.open_file_at(sub, "renamed").err().map(|e| e.errno),
            Some(syscall::error::ENOENT),
        );
        let h = srv.open_file_at(deeper, "moved").expect("the moved name");
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(
            &buf[..n],
            b"inside the grant",
            "the bytes followed the name"
        );
    }

    /// **The `REMOVE` rung, which had no verb until this one.** A capability that may create and
    /// write and not remove cannot move a name out; one that may remove and not create cannot move a
    /// name in. Both answer `EROFS`, and the pair is the point: a rename is two authorities and
    /// either one missing is enough.
    #[test]
    fn a_rename_needs_remove_where_it_takes_from_and_create_where_it_puts() {
        let mut srv = server_with_tree();
        let root = fs_proto::fs::ROOT as u32;
        // Everything but REMOVE: milestone 47's motivating sentence, "add to this, destroy nothing".
        let append = srv.open_dir(root, "sub", dir::ALL & !dir::REMOVE).unwrap();
        assert_eq!(
            srv.rename(append, "inner", append, "elsewhere")
                .err()
                .map(|e| e.errno),
            Some(EROFS),
            "a capability that cannot remove cannot rename away from itself either",
        );
        // And the refusal is about the capability, not about the name: it is the same answer for a
        // name that is not there, so this verb cannot be used to probe for one.
        assert_eq!(
            srv.rename(append, "absent", append, "x")
                .err()
                .map(|e| e.errno),
            Some(EROFS),
        );

        // Everything but CREATE: it may give a name up and has nowhere to put one.
        let mut srv = server_with_tree();
        let taker = srv.open_dir(root, "sub", dir::ALL & !dir::CREATE).unwrap();
        assert_eq!(
            srv.rename(taker, "inner", taker, "elsewhere")
                .err()
                .map(|e| e.errno),
            Some(EROFS),
        );

        // With both, the same request through the same code succeeds, so the refusals above are
        // statements about the rights rather than about a verb that never works.
        let mut srv = server_with_tree();
        let full = srv.open_dir(root, "sub", dir::ALL).unwrap();
        srv.rename(full, "inner", full, "elsewhere")
            .expect("REMOVE and CREATE together must be enough");
    }

    /// **Cross-directory rights are checked per directory, not once**, which is the case a
    /// single-capability check would get wrong: taking a name out of a directory you may remove
    /// from and putting it into one you may not create in is two different refusals.
    #[test]
    fn a_cross_directory_rename_consults_both_directories_rights() {
        let mut srv = server_with_tree();
        let root = fs_proto::fs::ROOT as u32;
        let src = srv.open_dir(root, "sub", dir::ALL).unwrap();
        // A destination with no CREATE. Reached by descending twice so it is a genuinely different
        // capability rather than the same handle spelled differently.
        let dst = srv
            .open_dir(src, "deeper", dir::ALL & !dir::CREATE)
            .unwrap();
        assert_eq!(
            srv.rename(src, "inner", dst, "landed")
                .err()
                .map(|e| e.errno),
            Some(EROFS),
            "the destination's missing CREATE must refuse, even though the source may remove",
        );
        // The source is untouched by the refusal: a half-done rename is the failure this verb exists
        // to make impossible.
        assert!(srv.open_file_at(src, "inner").is_ok());

        let wide = srv.open_dir(src, "deeper", dir::ALL).unwrap();
        srv.rename(src, "inner", wide, "landed").expect("rename");
        assert!(srv.open_file_at(wide, "landed").is_ok());
    }

    /// A rename **replaces** an existing destination of the same kind, which is what the temp-file
    /// idiom depends on, and refuses one of a different kind with POSIX's own two answers.
    #[test]
    fn a_rename_replaces_a_file_and_refuses_to_cross_kinds() {
        let mut srv = server_with_tree();
        let root = fs_proto::fs::ROOT as u32;
        let sub = srv.open_dir(root, "sub", dir::ALL).unwrap();
        let tmp = srv.create_file_at(sub, "tmp").unwrap();
        srv.write(tmp, 0, b"the new contents").unwrap();

        srv.rename(sub, "tmp", sub, "inner").expect("replace");
        let h = srv.open_file_at(sub, "inner").unwrap();
        let mut buf = [0u8; 64];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(
            &buf[..n],
            b"the new contents",
            "the destination must hold the source's bytes, whole",
        );
        assert_eq!(
            srv.open_file_at(sub, "tmp").err().map(|e| e.errno),
            Some(syscall::error::ENOENT),
        );

        // A file over a directory, and a directory over a file. RedoxFS's rename_node allows both
        // (it passes the destination's own type to remove_node, so it never compares the two);
        // these are our checks, so the verb means one thing regardless of backend.
        assert_eq!(
            srv.rename(sub, "inner", sub, "deeper")
                .err()
                .map(|e| e.errno),
            Some(EISDIR),
        );
        assert_eq!(
            srv.rename(sub, "deeper", sub, "inner")
                .err()
                .map(|e| e.errno),
            Some(ENOTDIR),
        );
        // And a non-empty destination directory survives, which is the engine's ENOTEMPTY.
        let d = srv.make_dir(sub, "empty-dir", dir::ALL).unwrap();
        let _ = d;
        assert_eq!(
            srv.rename(sub, "empty-dir", sub, "deeper")
                .err()
                .map(|e| e.errno),
            Some(syscall::error::ENOTEMPTY),
        );
    }

    /// **A directory renames in place and does not move between directories**, and the refusal is
    /// the contract's rather than an accident: moving a directory into one of its own descendants is
    /// how a tree becomes a detached cycle, POSIX guards it with a path-prefix test, and this
    /// contract has no paths to take a prefix of. Renaming inside one directory cannot create a
    /// cycle because the parent does not change, which is exactly where the line is drawn.
    #[test]
    fn a_directory_renames_in_place_and_refuses_to_move_between_directories() {
        let mut srv = server_with_tree();
        let root = fs_proto::fs::ROOT as u32;
        let sub = srv.open_dir(root, "sub", dir::ALL).unwrap();

        srv.rename(sub, "deeper", sub, "renamed-dir")
            .expect("a directory renames in place");
        let d = srv
            .open_dir(sub, "renamed-dir", dir::ALL)
            .expect("and it is still a directory, with its contents");
        assert!(srv.open_file_at(d, "leaf").is_ok());

        // The cycle this refusal prevents, spelled out: `sub` moved into something inside `sub`.
        assert_eq!(
            srv.rename(root, "sub", d, "sub").err().map(|e| e.errno),
            Some(EINVAL),
            "moving a directory between directories is not offered, and says so",
        );
        // Even a harmless-looking move (out of the grant's reach entirely) takes the same answer,
        // because the rule is about the verb and not about which pair happens to be safe.
        assert_eq!(
            srv.rename(sub, "renamed-dir", root, "promoted")
                .err()
                .map(|e| e.errno),
            Some(EINVAL),
        );
    }

    /// A name is a single component on both halves, a self-rename is a no-op success, and a rename
    /// through a file handle is `ENOTDIR`. Each of these is a way to make the server treat one kind
    /// of thing as another.
    #[test]
    fn a_rename_refuses_what_every_other_name_taking_verb_refuses() {
        let mut srv = server_with_tree();
        let root = fs_proto::fs::ROOT as u32;
        let sub = srv.open_dir(root, "sub", dir::ALL).unwrap();

        for bad in ["..", ".", "", "a/b", "/abs", "with:colon"] {
            assert_eq!(
                srv.rename(sub, bad, sub, "x").err().map(|e| e.errno),
                Some(EINVAL),
                "a source named {bad:?} must be refused as EINVAL",
            );
            assert_eq!(
                srv.rename(sub, "inner", sub, bad).err().map(|e| e.errno),
                Some(EINVAL),
                "a destination named {bad:?} must be refused as EINVAL",
            );
        }
        // Renaming a name onto itself changes nothing and succeeds, which is POSIX's answer and the
        // one that matters: the alternative unlinks the only link and then has nothing to relink.
        srv.rename(sub, "inner", sub, "inner").expect("a no-op");
        assert!(srv.open_file_at(sub, "inner").is_ok());

        let f = srv.open_file_at(sub, "inner").unwrap();
        assert_eq!(
            srv.rename(f, "inner", sub, "x").err().map(|e| e.errno),
            Some(ENOTDIR),
        );
        assert_eq!(
            srv.rename(sub, "inner", f, "x").err().map(|e| e.errno),
            Some(ENOTDIR),
        );
        assert_eq!(
            srv.rename(999, "inner", sub, "x").err().map(|e| e.errno),
            Some(EBADF),
        );
    }

    /// A rename is on the platter, not only in the handle table: drop the mount the way a dying
    /// process does and reopen. Persistence is the half a cache cannot fake, and it is what the
    /// crash sweep in `tests/crash_consistency.rs` then cuts into.
    #[test]
    fn a_rename_survives_a_reopen() {
        let mut disk = image_with_tree();
        {
            let mut srv = Server::open(disk).expect("open");
            let sub = srv
                .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL)
                .unwrap();
            srv.rename(sub, "inner", sub, "survivor").unwrap();
            disk = srv.fs.disk;
        }
        let mut srv = Server::open(disk).expect("reopen");
        let sub = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL)
            .unwrap();
        assert_eq!(
            srv.open_file_at(sub, "inner").err().map(|e| e.errno),
            Some(syscall::error::ENOENT),
        );
        let h = srv.open_file_at(sub, "survivor").expect("the new name");
        let mut buf = [0u8; 64];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"inside the grant");
    }

    /// Everything a directory capability makes survives the mount, which is the half a cache cannot
    /// fake: the descent, the created directory, and the file inside it are all on the platter.
    #[test]
    fn a_subtree_built_through_directory_handles_survives_a_reopen() {
        let mut disk = image_with_tree();
        {
            let mut srv = Server::open(disk).expect("open");
            let sub = srv
                .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL)
                .unwrap();
            let made = srv.make_dir(sub, "made", dir::ALL).unwrap();
            let f = srv.create_file_at(made, "note").unwrap();
            srv.write(f, 0, b"written two levels down").unwrap();
            disk = srv.fs.disk; // no unmount, as a dying process leaves it
        }
        let mut srv = Server::open(disk).expect("reopen");
        let sub = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL)
            .unwrap();
        let made = srv
            .open_dir(sub, "made", dir::ALL)
            .expect("the made directory");
        let f = srv.open_file_at(made, "note").expect("the file in it");
        let mut buf = [0u8; 64];
        let n = srv.read(f, 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"written two levels down");
    }
    // --- STATFS (milestone 54) ---

    /// **The volume's numbers are the image's, and writing to it moves the free count.**
    ///
    /// The moving part is what makes this a test rather than a tautology: a `statfs` that returned
    /// a constant would pass every assertion about units and totals, and it is exactly the constant
    /// this verb exists to replace (`smb_proto::share::NOMINAL_VOLUME_BYTES`). So the free count is
    /// read, a megabyte is written, and it must have gone down.
    #[test]
    fn statfs_reports_the_image_and_a_write_takes_space_out_of_it() {
        let mut srv = server_with(&[("small", b"x")]);

        let (block, total, free) = srv.statfs(fs_proto::fs::ROOT as u32).unwrap();
        assert_eq!(
            block,
            redoxfs::BLOCK_SIZE,
            "the unit is RedoxFS's own block"
        );
        // `server_with` builds a 16 MiB DiskMemory; the filesystem is that minus the reservation
        // RedoxFS makes at the front, so the assertion is a range rather than an equality.
        let bytes = fs_proto::statfs::total_bytes(block, total);
        assert!(
            (8 * 1024 * 1024..=16 * 1024 * 1024).contains(&bytes),
            "a 16 MiB image reported {bytes} bytes",
        );
        assert!(free > 0 && free < total, "free {free} of {total} blocks");

        let h = srv.create_file("big").unwrap();
        srv.write(h, 0, &[7u8; 1024 * 1024]).unwrap();
        let (_, after_total, after_free) = srv.statfs(fs_proto::fs::ROOT as u32).unwrap();
        assert_eq!(after_total, total, "the image did not change size");
        assert!(
            after_free < free,
            "a megabyte was written and free went {free} -> {after_free}",
        );

        // Any handle qualifies, file or directory, because the answer is about the image behind
        // both. A handle nobody minted does not.
        assert_eq!(srv.statfs(h).unwrap().0, block);
        assert_eq!(srv.statfs(9999).err().map(|e| e.errno), Some(EBADF));
    }

    /// **No right is demanded**, which is the row `fs_proto::verb::TABLE` carries and the thing a
    /// narrowed capability depends on: a grant that may write but not read still has to be able to
    /// find out whether its next write fits.
    #[test]
    fn statfs_answers_through_a_capability_that_carries_nothing() {
        let mut srv = server_with_tree();
        let root = fs_proto::fs::ROOT as u32;
        // A child directory holding one right, and not one of the ones a read would need.
        let narrow = srv.open_dir(root, "sub", dir::DESCEND).unwrap();
        assert!(
            srv.read_dir(narrow, 0, &mut [0u8; 64]).is_err(),
            "the fixture is only meaningful if this capability cannot enumerate",
        );
        assert_eq!(
            srv.statfs(narrow).unwrap().0,
            redoxfs::BLOCK_SIZE,
            "STATFS asks for no right; see fs_proto::fs::STATFS",
        );
    }

    // --- CREATE and TRUNCATE (milestone 31 phase 2) ---

    #[test]
    fn create_makes_an_empty_file_and_refuses_an_existing_name() {
        let mut srv = server_with(&[("already", b"here")]);

        let h = srv.create_file("fresh").unwrap();
        assert_eq!(srv.fstat(h).unwrap(), 0, "a created file starts empty");

        // Create is create, not create-or-open. Silently opening an existing file is how a
        // partly-working write comes to look like a working one (the §27 failure mode).
        assert_eq!(
            srv.create_file("already").err().map(|e| e.errno),
            Some(EEXIST),
            "creating over an existing name must refuse, and must not truncate it either",
        );
        let h2 = srv.open_file("already").unwrap();
        assert_eq!(
            srv.fstat(h2).unwrap(),
            4,
            "the refused create left the file alone"
        );

        // And the refusal did not half-create anything: the name still resolves to the old file.
        let mut buf = [0u8; 8];
        assert_eq!(srv.read(h2, 0, &mut buf).unwrap(), 4);
        assert_eq!(&buf[..4], b"here");
    }

    #[test]
    fn truncate_shrinks_and_discards_the_tail_which_is_the_bug_it_exists_to_fix() {
        // The exact byte counts from DECISIONS §27: a 64-byte payload from one boot, a 61-byte
        // write from the next, and the three-byte tail that made a good write look like a failure.
        let mut srv = server_with(&[("scratch", b"(placeholder)")]);
        let h = srv.open_file("scratch").unwrap();
        assert_eq!(srv.write(h, 0, &[b'L'; 64]).unwrap(), 64);

        let short = [b'S'; 61];
        assert_eq!(srv.write(h, 0, &short).unwrap(), 61);
        srv.truncate(h, short.len() as u64).unwrap();

        assert_eq!(srv.fstat(h).unwrap(), 61, "truncate sets the size exactly");
        let mut buf = [0u8; 128];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(n, 61, "and a whole-file read returns only what was written");
        assert_eq!(&buf[..61], &short[..], "with the new bytes intact");
    }

    #[test]
    fn truncate_grows_with_zeroes_and_is_idempotent() {
        let mut srv = server_with(&[("f", b"abc")]);
        let h = srv.open_file("f").unwrap();

        srv.truncate(h, 8).unwrap();
        assert_eq!(srv.fstat(h).unwrap(), 8);
        let mut buf = [0xffu8; 16];
        assert_eq!(srv.read(h, 0, &mut buf).unwrap(), 8);
        assert_eq!(&buf[..3], b"abc", "the original bytes survive a grow");
        assert_eq!(
            &buf[3..8],
            &[0, 0, 0, 0, 0],
            "and the extension reads as zeroes"
        );

        // Truncating to the current size changes nothing, which matters because std::fs::write
        // opens-creates-truncates unconditionally and would otherwise churn the tree every call.
        srv.truncate(h, 8).unwrap();
        assert_eq!(srv.fstat(h).unwrap(), 8);

        srv.truncate(h, 0).unwrap();
        assert_eq!(
            srv.fstat(h).unwrap(),
            0,
            "and shrinking to empty is allowed"
        );
        assert_eq!(srv.read(h, 0, &mut buf).unwrap(), 0);
    }

    #[test]
    fn create_write_truncate_all_survive_a_close_and_reopen() {
        // The half a cache cannot fake: drop the mount entirely and reopen the same disk. The disk is
        // taken back out of the server rather than cloned, which is also how a dying process leaves
        // it: no unmount, exactly as the cross-boot test does.
        let mut disk = image_with(&[]);
        let payload = b"created, written, and truncated to exactly this";

        {
            let mut srv = Server::open(disk).expect("open");
            let h = srv.create_file("made").unwrap();
            assert_eq!(srv.write(h, 0, payload).unwrap(), payload.len());
            assert_eq!(srv.write(h, 0, &[b'X'; 200]).unwrap(), 200);
            assert_eq!(srv.write(h, 0, payload).unwrap(), payload.len());
            srv.truncate(h, payload.len() as u64).unwrap();
            disk = srv.fs.disk;
        }

        let mut srv = Server::open(disk).expect("reopen");
        let h = srv
            .open_file("made")
            .expect("the created name survived the reopen");
        assert_eq!(srv.fstat(h).unwrap(), payload.len() as u64);
        let mut buf = [0u8; 512];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(
            &buf[..n],
            payload,
            "the truncated contents are what persisted"
        );
    }

    #[test]
    fn create_and_truncate_refuse_what_every_other_verb_refuses() {
        let mut srv = server_with(&[("f", b"x")]);

        // A forged handle is EBADF in exactly one place, and the new verb goes through it too.
        assert_eq!(
            srv.truncate(99, 0).err().map(|e| e.errno),
            Some(EBADF),
            "truncate must validate the handle like every other operation",
        );

        // A name is a single component, not a path, and both verbs enforce it. RedoxFS does NOT:
        // its check_name passes `/`, `.` and `..` through, so before this check create_file("../escape")
        // happily made an entry literally named `../escape`. Not a traversal, because nothing walks
        // paths, and a landmine for the first thing that does.
        for bad in [
            "../escape",
            "sub/deep",
            "/absolute",
            ".",
            "..",
            "",
            "with:colon",
            "back\\slash",
        ] {
            assert_eq!(
                srv.create_file(bad).err().map(|e| e.errno),
                Some(EINVAL),
                "create must refuse {bad:?} as EINVAL: a name is not a path",
            );
            assert_eq!(
                srv.open_file(bad).err().map(|e| e.errno),
                Some(EINVAL),
                "and open must refuse {bad:?} the same way, not fall through to ENOENT",
            );
        }
    }
    /// **What `std::fs::write` actually does, twice, with a shorter payload the second time.**
    ///
    /// Written to chase a livelock: on device this sequence tripped the 90 s per-test ceiling with two
    /// userspace threads spinning. If the loop is in the filesystem it shows up here in milliseconds;
    /// if this passes, the fault is above the core (the wire, the PAL, or the client's own loops) and
    /// that is worth knowing before reading any of them.
    ///
    /// `std::fs::write` is `create(true).truncate(true)`, so each call is: open, or create if absent;
    /// truncate to 0; write. The second call is the interesting one, because it truncates a file that
    /// already has bytes and then writes fewer than it had.
    #[test]
    fn the_std_fs_write_sequence_twice_does_not_loop() {
        let mut srv = server_with(&[]);
        let long = b"the first write, deliberately the longer of the two";
        let short = b"the second write, shorter";
        assert!(short.len() < long.len());

        // Call one: the name is absent, so create, then truncate an already-empty file, then write.
        let h = srv.create_file("made-by-std").unwrap();
        srv.truncate(h, 0).unwrap();
        assert_eq!(srv.write(h, 0, long).unwrap(), long.len());
        srv.close(h).unwrap();

        // Call two: the name exists, so open, truncate away 51 bytes, then write 25.
        let h = srv.open_file("made-by-std").unwrap();
        srv.truncate(h, 0).unwrap();
        assert_eq!(
            srv.fstat(h).unwrap(),
            0,
            "truncate to 0 must empty the file"
        );
        assert_eq!(srv.write(h, 0, short).unwrap(), short.len());
        assert_eq!(srv.fstat(h).unwrap(), short.len() as u64);
        srv.close(h).unwrap();

        // And the read-back `std::fs::read` does: open, fstat for the size hint, read to EOF. The
        // read loop must terminate, which means a read at or past the size returns 0.
        let h = srv.open_file("made-by-std").unwrap();
        let size = srv.fstat(h).unwrap() as usize;
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        let mut off = 0u64;
        // Bounded so a livelock fails the test instead of hanging it: EOF must arrive well inside this.
        for _ in 0..16 {
            let n = srv.read(h, off, &mut buf).unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
            off += n as u64;
        }
        assert_eq!(
            got.len(),
            size,
            "the read loop did not reach EOF in 16 reads"
        );
        assert_eq!(
            &got[..],
            short,
            "the shorter write did not replace the contents"
        );
        assert_eq!(
            srv.read(h, off, &mut buf).unwrap(),
            0,
            "a read at EOF must return 0, or every read_to_end spins",
        );
    }
    /// **Create into a directory the image already populated**, which is what the device does and what
    /// the empty-root tests above do not.
    ///
    /// The device image is built by the host tool with `motd` and `scratch` already in the root, and
    /// `std::fs::write` then adds a third name. On device that hangs: three FS-server instances were
    /// caught spinning, one in `create_node` and two in `find_node_inner`, i.e. in the DIRECTORY code
    /// rather than the allocator. Every earlier host test created into an *empty* root, so none of
    /// them shaped the directory the way the real image does. If the fault is directory-shape
    /// dependent it belongs here, in milliseconds.
    #[test]
    fn create_into_a_populated_root_and_look_it_up() {
        // The same names and contents the shipped image carries, so the root's shape matches.
        let mut srv = server_with(&[
            (
                "motd",
                b"redoxfs served this file to an EL0 client through a capability handle\n",
            ),
            (
                "scratch",
                b"(placeholder overwritten by the fs_server write test)",
            ),
        ]);

        let h = srv.create_file("made-by-std").unwrap();
        assert_eq!(srv.write(h, 0, b"first").unwrap(), 5);
        srv.close(h).unwrap();

        // The lookups that spin on device: the new name, and crucially the pre-existing ones, since a
        // corrupted directory would take every scan down with it, not just the new entry's.
        for name in ["made-by-std", "motd", "scratch"] {
            let h = srv
                .open_file(name)
                .unwrap_or_else(|e| panic!("{name} no longer resolves: {e:?}"));
            srv.close(h).unwrap();
        }
        assert!(
            srv.open_file("definitely-absent").is_err(),
            "a miss must still terminate and report, not scan forever",
        );
    }
    /// **CREATE through the EL0 binary's exact block chunking**, which is the one path none of the
    /// other create tests take.
    ///
    /// Every test above drives `DiskMemory`, whose `write_at` writes exactly the buffer it is given.
    /// The device drives `IpcDisk` behind [`BlockDisk`], which splits every call into whole-block
    /// transfers and READ-MODIFY-WRITES a short final chunk. `create_node` writes a fresh node block
    /// and extends a directory, so it is exactly the shape that would expose a chunking fault, and on
    /// device it hangs with FS servers spinning in `create_node` and `find_node_inner`.
    ///
    /// Bounded on purpose: the point is that this terminates, so a livelock fails the run rather than
    /// wedging the suite.
    #[test]
    fn create_through_the_block_chunking_does_not_loop() {
        let mut fs = FileSystem::create(BlockDisk(VecIo(vec![0u8; 16 * 1024 * 1024])), None, 0, 0)
            .expect("mkfs through BlockDisk");
        // The root the shipped image has when std::fs::write adds a third name to it.
        fs.tx(|tx| {
            for (name, body) in [
                (
                    "motd",
                    &b"redoxfs served this file to an EL0 client through a capability handle\n"[..],
                ),
                (
                    "scratch",
                    &b"(placeholder overwritten by the fs_server write test)"[..],
                ),
            ] {
                let ptr = tx
                    .create_node(TreePtr::root(), name, Node::MODE_FILE | 0o644, 0, 0)?
                    .ptr();
                tx.write_node(ptr, 0, body, 0, 0)?;
            }
            Ok(())
        })
        .expect("populate");
        let image = fs.disk.0.0;

        let mut srv = Server::open(BlockDisk(VecIo(image))).expect("Server::open");

        // What std::fs::write does: create, truncate, write; then again, shorter.
        let h = srv
            .create_file("made-by-std")
            .expect("create through chunking");
        srv.truncate(h, 0).unwrap();
        let long = b"the first write, deliberately the longer of the two";
        assert_eq!(srv.write(h, 0, long).unwrap(), long.len());
        srv.close(h).unwrap();

        let h = srv
            .open_file("made-by-std")
            .expect("reopen through chunking");
        srv.truncate(h, 0).unwrap();
        let short = b"the second write, shorter";
        assert_eq!(srv.write(h, 0, short).unwrap(), short.len());
        srv.close(h).unwrap();

        // The lookups that spin on device, including the pre-existing names: a directory damaged by
        // the create would take every scan down with it, not only the new entry's.
        for name in ["made-by-std", "motd", "scratch"] {
            let h = srv.open_file(name).unwrap_or_else(|e| {
                panic!("{name} no longer resolves after a chunked create: {e:?}")
            });
            srv.close(h).unwrap();
        }

        let h = srv.open_file("made-by-std").unwrap();
        let mut buf = [0u8; 256];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(
            &buf[..n],
            short,
            "the chunked create/truncate/write round trip lost bytes"
        );
    }

    // --- Extended attributes (milestone 57) ---

    /// The node id behind a name, so a test can talk about the thing the store keys on rather than
    /// about the name that happens to point at it. This is the whole trick of the layer, so the
    /// tests have to be able to see it.
    fn node_id_of(srv: &mut Server<DiskMemory>, parent: TreePtr<Node>, name: &str) -> u32 {
        srv.fs
            .tx(|tx| Ok(tx.find_node(parent, name)?.id()))
            .unwrap_or_else(|e| panic!("no node behind {name}: {e:?}"))
    }

    /// Whether the store still holds a blob for `node_id`. Reaches past the contract deliberately:
    /// a client cannot name the store, so a test that could only use the contract could not tell
    /// "the attributes were purged" from "the attributes are unreachable".
    fn store_holds(srv: &mut Server<DiskMemory>, node_id: u32) -> bool {
        srv.fs
            .tx(|tx| {
                let Some(dir) = find_store(tx)? else {
                    return Ok(false);
                };
                let bytes = xattr::store::file_name(node_id);
                let name = core::str::from_utf8(&bytes).expect("hex is ASCII");
                Ok(tx.find_node(dir, name).is_ok())
            })
            .unwrap()
    }

    /// The attribute names on a handle, through the contract, in the order the store holds them.
    fn attr_names(srv: &mut Server<DiskMemory>, handle: u32) -> Vec<Vec<u8>> {
        let mut page = [0u8; 4096];
        let n = srv.list_xattr(handle, &mut page).expect("list");
        fs_proto::xattr::list::iter(&page[..n])
            .map(|s| s.to_vec())
            .collect()
    }

    /// One attribute's value through the contract.
    fn attr(srv: &mut Server<DiskMemory>, handle: u32, name: &[u8]) -> Result<(u32, Vec<u8>)> {
        let mut page = [0u8; 4096];
        let (kind, n) = srv.get_xattr(handle, name, &mut page)?;
        Ok((kind, page[..n].to_vec()))
    }

    /// **An attribute is on the disk, not in the server.** Set it, drop the mount the way a dying
    /// process does, mount the same image again, and read it back. Without this the whole layer
    /// could be a map in a `Server` and every other test here would still pass.
    #[test]
    fn an_attribute_survives_a_mount_it_was_not_written_in() {
        let disk = image_with(&[("motd", b"hello")]);
        let mut srv = Server::open(disk).unwrap();
        let h = srv.open_file("motd").unwrap();
        srv.set_xattr(h, b"user.DOSATTRIB", xattr::RAW, b"0x20")
            .expect("set");
        srv.set_xattr(h, b"user.com.apple.FinderInfo", 0x4353_5452, &[7u8; 32])
            .expect("set a typed one");
        let disk = srv.fs.disk; // no unmount, exactly as on device

        let mut srv = Server::open(disk).unwrap();
        let h = srv.open_file("motd").unwrap();
        assert_eq!(
            attr(&mut srv, h, b"user.DOSATTRIB").unwrap(),
            (xattr::RAW, b"0x20".to_vec()),
        );
        assert_eq!(
            attr(&mut srv, h, b"user.com.apple.FinderInfo").unwrap(),
            (0x4353_5452, vec![7u8; 32]),
            "the type code has to survive the platter too, or indexing is foreclosed after all",
        );
        assert_eq!(
            attr_names(&mut srv, h),
            [
                b"user.DOSATTRIB".to_vec(),
                b"user.com.apple.FinderInfo".to_vec()
            ],
        );
    }

    /// **The headline: a rename carries the attributes, and nothing in the rename path knows they
    /// exist.**
    ///
    /// The store keys on the node's `TreePtr`, and a rename changes a directory entry rather than a
    /// node, so this works by construction. It is the correctness property that is only available
    /// inside the FS server, and the one AppleDouble sidecars (and any path-keyed store) get wrong,
    /// so it is pinned rather than reasoned about. The node id is asserted unchanged as well, since
    /// that equality is *why* it holds.
    #[test]
    fn a_rename_carries_the_attributes_because_the_store_keys_on_the_node() {
        let mut srv = server_with_tree();
        let h = srv.open_file("motd").unwrap();
        srv.set_xattr(h, b"user.tag", 9, b"follow me").unwrap();
        let before = node_id_of(&mut srv, TreePtr::root(), "motd");

        // Rename within the root, then into a subdirectory: neither touches the node.
        srv.rename(
            fs_proto::fs::ROOT as u32,
            "motd",
            fs_proto::fs::ROOT as u32,
            "motd2",
        )
        .expect("rename in place");
        let sub = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::ALL)
            .unwrap();
        srv.rename(fs_proto::fs::ROOT as u32, "motd2", sub, "moved")
            .expect("rename across directories");

        let after = node_id_of(&mut srv, TreePtr::root(), "sub");
        assert_ne!(
            after, before,
            "sanity: the subdirectory is a different node"
        );
        let moved = srv
            .open_file_at(sub, "moved")
            .expect("open at its new name");
        assert_eq!(
            attr(&mut srv, moved, b"user.tag").unwrap(),
            (9, b"follow me".to_vec()),
            "the attribute did not follow the file across two renames",
        );
        assert!(
            store_holds(&mut srv, before),
            "and it is still the same node's blob"
        );
    }

    /// **Unlink takes the attributes with the file, in the same transaction**, and a node id issued
    /// again afterwards inherits nothing.
    ///
    /// This is the correctness bug wearing a housekeeping costume that DECISIONS §34's amendment
    /// names: a leaked blob is not wasted space, it is somebody else's metadata attached to a file
    /// that never asked for it. The reuse half is the one that makes it a bug rather than a leak, so
    /// it is tested by actually provoking the reuse.
    #[test]
    fn unlink_takes_the_attributes_and_a_reused_node_inherits_nothing() {
        let mut srv = server_with_tree();
        let h = srv.create_file("doomed").unwrap();
        srv.set_xattr(h, b"user.secret", xattr::RAW, b"not yours")
            .unwrap();
        let doomed = node_id_of(&mut srv, TreePtr::root(), "doomed");
        assert!(store_holds(&mut srv, doomed));

        srv.close(h).unwrap(); // an open handle pins the node; close first so the id is really freed
        srv.unlink(fs_proto::fs::ROOT as u32, "doomed").unwrap();
        assert!(
            !store_holds(&mut srv, doomed),
            "the blob outlived the file it belonged to",
        );

        // **Provoke the reuse rather than reasoning about it.** RedoxFS releases a freed node id, so
        // a fresh create gets it back, and today the very first one does. The loop is bounded and
        // the miss is asserted rather than skipped: without a reuse the interesting half of this
        // test is vacuous, and a silent skip is how a test stops proving anything without saying so.
        // If a future engine stops recycling ids promptly, widen the bound; do not delete the
        // assertion.
        let mut reused = None;
        for i in 0..8 {
            let name = format!("fresh{i}");
            let nh = srv.create_file(&name).unwrap();
            if node_id_of(&mut srv, TreePtr::root(), &name) == doomed {
                reused = Some(nh);
                break;
            }
        }
        let nh =
            reused.expect("no create reused the freed node id, so the reuse case went untested");
        assert!(
            attr_names(&mut srv, nh).is_empty(),
            "a new file was handed the previous occupant's attributes",
        );
        assert_eq!(
            attr(&mut srv, nh, b"user.secret").unwrap_err().errno,
            xattr::ENODATA,
        );
    }

    /// A rename that **replaces** a destination takes the replaced file's attributes too. RedoxFS's
    /// `rename_node` removes that node and throws the freed id away, so this is the one removal the
    /// engine cannot tell us about and the server has to notice for itself.
    #[test]
    fn a_rename_over_a_file_takes_the_replaced_files_attributes() {
        let mut srv = server_with_tree();
        let victim = srv.create_file("victim").unwrap();
        srv.set_xattr(victim, b"user.doomed", xattr::RAW, b"x")
            .unwrap();
        srv.close(victim).unwrap();
        let victim_id = node_id_of(&mut srv, TreePtr::root(), "victim");

        let winner = srv.create_file("winner").unwrap();
        srv.set_xattr(winner, b"user.kept", xattr::RAW, b"y")
            .unwrap();
        let winner_id = node_id_of(&mut srv, TreePtr::root(), "winner");

        srv.rename(
            fs_proto::fs::ROOT as u32,
            "winner",
            fs_proto::fs::ROOT as u32,
            "victim",
        )
        .expect("replace");

        assert!(
            !store_holds(&mut srv, victim_id),
            "the replaced file's attributes outlived it",
        );
        assert!(
            store_holds(&mut srv, winner_id),
            "and the survivor kept its own"
        );
        let h = srv.open_file("victim").unwrap();
        assert_eq!(attr_names(&mut srv, h), [b"user.kept".to_vec()]);
    }

    /// `rmdir` purges exactly as `unlink` does, because a directory carries attributes exactly as a
    /// file does. A verb that purged one kind and not the other would leak on whichever half its
    /// author was not thinking about.
    #[test]
    fn a_directory_carries_attributes_and_rmdir_takes_them_with_it() {
        let mut srv = server_with_tree();
        let d = srv
            .make_dir(fs_proto::fs::ROOT as u32, "attrdir", dir::ALL)
            .unwrap();
        srv.set_xattr(d, b"user.on-a-directory", 3, b"yes").unwrap();
        let id = node_id_of(&mut srv, TreePtr::root(), "attrdir");
        assert_eq!(
            attr(&mut srv, d, b"user.on-a-directory").unwrap(),
            (3, b"yes".to_vec()),
        );

        srv.rmdir(fs_proto::fs::ROOT as u32, "attrdir").unwrap();
        assert!(
            !store_holds(&mut srv, id),
            "an empty directory left its attributes behind"
        );
    }

    /// **The last attribute takes the store directory with it**, so a filesystem holding no
    /// attributes is byte-for-byte a filesystem that never held any.
    ///
    /// This closes a recorded limitation rather than adding a feature, and the reason it was worth
    /// closing is on the recovery side: `redoxfs_host extract` copies the store out with the tree,
    /// so a leftover empty `.nife-attrs` would appear in somebody's recovered files as a
    /// directory nothing explains.
    ///
    /// Both ways of emptying it are exercised, because they reach the same place by different paths:
    /// `remove_xattr` writes an empty blob and `unlink` purges one, and only one of them goes
    /// through `write_attrs`. The middle assertion is the one that keeps this honest: while a second
    /// node still has attributes, the directory must **stay**, or the test would pass just as well
    /// against an implementation that removed the store every time.
    #[test]
    fn the_last_attribute_takes_the_store_with_it() {
        /// Whether the store directory exists at all. Reaches past the contract on purpose: no
        /// client can name it, so nothing inside the contract can tell "gone" from "hidden".
        fn store_exists(srv: &mut Server<DiskMemory>) -> bool {
            srv.fs.tx(|tx| Ok(find_store(tx)?.is_some())).unwrap()
        }

        let mut srv = server_with_tree();
        assert!(
            !store_exists(&mut srv),
            "a fresh image must not carry a store before anything sets an attribute",
        );

        let motd = srv.open_file("motd").unwrap();
        let second = srv.create_file("second").unwrap();
        srv.set_xattr(motd, b"user.a", xattr::RAW, b"1").unwrap();
        srv.set_xattr(second, b"user.b", xattr::RAW, b"2").unwrap();
        assert!(store_exists(&mut srv), "setting one must create the store");

        srv.remove_xattr(motd, b"user.a").unwrap();
        assert!(
            store_exists(&mut srv),
            "the store must stay while another node still has attributes",
        );

        srv.remove_xattr(second, b"user.b").unwrap();
        assert!(
            !store_exists(&mut srv),
            "the last attribute must take the store directory with it",
        );

        // And again by the other route: `unlink` purges a blob without going through `write_attrs`,
        // so the two paths need separate evidence.
        srv.set_xattr(motd, b"user.c", xattr::RAW, b"3").unwrap();
        assert!(store_exists(&mut srv));
        srv.unlink(fs_proto::fs::ROOT as u32, "motd").unwrap();
        srv.close(motd).unwrap();
        assert!(
            !store_exists(&mut srv),
            "unlinking the last node with attributes must take the store too",
        );
    }

    /// **The store is not in the namespace.** It cannot be listed, opened, descended into, created,
    /// renamed or removed, in any directory, at full rights. If a client could name it, "the
    /// attributes of a file" would be reachable as ordinary bytes by anything holding the directory.
    #[test]
    fn the_attribute_store_is_not_in_the_namespace() {
        let mut srv = server_with_tree();
        let h = srv.open_file("motd").unwrap();
        srv.set_xattr(h, b"user.x", xattr::RAW, b"1").unwrap();

        // It is really there: this test would be vacuous against a filesystem with no store at all.
        assert!(
            srv.fs.tx(|tx| Ok(find_store(tx)?.is_some())).unwrap(),
            "nothing was created, so nothing is being hidden",
        );

        let root = fs_proto::fs::ROOT as u32;
        let listed = list(&mut srv, root, 4096).unwrap();
        assert!(
            !listed.iter().any(|(n, _)| n == xattr::STORE_DIR),
            "a listing named the attribute store: {listed:?}",
        );
        // Every name-taking verb, at every rights this contract can carry, and the answer is always
        // the same one `..` gets: the name is not expressible here.
        assert_eq!(
            srv.open_file_at(root, xattr::STORE_DIR).unwrap_err().errno,
            EINVAL
        );
        assert_eq!(
            srv.open_dir(root, xattr::STORE_DIR, dir::ALL)
                .unwrap_err()
                .errno,
            EINVAL,
        );
        assert_eq!(
            srv.create_file_at(root, xattr::STORE_DIR)
                .unwrap_err()
                .errno,
            EINVAL
        );
        assert_eq!(
            srv.make_dir(root, xattr::STORE_DIR, dir::ALL)
                .unwrap_err()
                .errno,
            EINVAL,
        );
        assert_eq!(
            srv.unlink(root, xattr::STORE_DIR).unwrap_err().errno,
            EINVAL
        );
        assert_eq!(srv.rmdir(root, xattr::STORE_DIR).unwrap_err().errno, EINVAL);
        assert_eq!(
            srv.rename(root, "motd", root, xattr::STORE_DIR)
                .unwrap_err()
                .errno,
            EINVAL,
        );
        // And it is reserved in a *subdirectory* too, so the rule is one sentence rather than a
        // location-dependent one.
        let sub = srv.open_dir(root, "sub", dir::ALL).unwrap();
        assert_eq!(
            srv.create_file_at(sub, xattr::STORE_DIR).unwrap_err().errno,
            EINVAL
        );
    }

    /// **An attribute takes the direction the capability carries**, and no new rung was added to the
    /// ladder to say so: reading one needs what reading the file needs, changing one needs what
    /// writing the file needs, and each is refused with the word that verb already answers.
    #[test]
    fn attributes_take_the_direction_the_capability_already_carries() {
        let mut srv = server_with_tree();

        // A read-only directory, so the file handle under it inherits read and not write.
        let ro = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::READ | dir::DESCEND)
            .unwrap();
        let h = srv.open_file_at(ro, "inner").unwrap();
        let mut page = [0u8; 4096];
        assert_eq!(
            srv.list_xattr(h, &mut page).unwrap(),
            0,
            "reading is allowed"
        );
        assert_eq!(
            srv.set_xattr(h, b"user.x", xattr::RAW, b"1")
                .unwrap_err()
                .errno,
            EROFS,
            "through this capability the file is read-only, and so are its attributes",
        );
        assert_eq!(srv.remove_xattr(h, b"user.x").unwrap_err().errno, EROFS);

        // And the mirror: a write-only directory's file handle may set and may not read, which is
        // `EBADF` because it is a fact about the handle rather than about the capability.
        let wo = srv
            .open_dir(fs_proto::fs::ROOT as u32, "sub", dir::WRITE | dir::DESCEND)
            .unwrap();
        let h = srv.open_file_at(wo, "inner").unwrap();
        srv.set_xattr(h, b"user.x", xattr::RAW, b"1")
            .expect("writing is allowed");
        assert_eq!(
            srv.get_xattr(h, b"user.x", &mut page).unwrap_err().errno,
            EBADF
        );
        assert_eq!(srv.list_xattr(h, &mut page).unwrap_err().errno, EBADF);
        // A handle that was never minted is EBADF here as it is everywhere else.
        assert_eq!(srv.list_xattr(999, &mut page).unwrap_err().errno, EBADF);
    }

    /// **Every ceiling refuses at the server boundary with its own word**, so a client can act on
    /// which one it hit. DECISIONS §42's rule is that an offered verb fails loudly rather than
    /// degrading; a store that clipped a value would hand back a file that looked intact and was not.
    #[test]
    fn every_ceiling_refuses_loudly_and_with_its_own_word() {
        let mut srv = server_with_tree();
        let h = srv.open_file("motd").unwrap();
        let mut page = [0u8; 4096];

        assert_eq!(
            srv.set_xattr(h, b"", xattr::RAW, b"v").unwrap_err().errno,
            xattr::ERANGE,
        );
        assert_eq!(
            srv.set_xattr(h, &vec![b'n'; xattr::MAX_NAME + 1], xattr::RAW, b"v")
                .unwrap_err()
                .errno,
            xattr::ERANGE,
        );
        assert_eq!(
            srv.set_xattr(h, b"user.big", xattr::RAW, &vec![0u8; xattr::MAX_VALUE + 1])
                .unwrap_err()
                .errno,
            xattr::E2BIG,
        );
        // The widest value the contract allows must actually work, or the limit above is a limit on
        // nothing and the refusal proves only that the check exists.
        srv.set_xattr(
            h,
            b"user.widest",
            xattr::RAW,
            &vec![0xCDu8; xattr::MAX_VALUE],
        )
        .expect("the widest permitted value");
        assert_eq!(
            attr(&mut srv, h, b"user.widest").unwrap().1.len(),
            xattr::MAX_VALUE,
        );

        // Fill the node and ask for one more.
        for i in 1..xattr::MAX_COUNT {
            srv.set_xattr(h, format!("user.a{i}").as_bytes(), xattr::RAW, b"v")
                .unwrap();
        }
        assert_eq!(
            srv.set_xattr(h, b"user.overflow", xattr::RAW, b"v")
                .unwrap_err()
                .errno,
            xattr::ENOSPC,
        );
        // Replacing at the cap still works: the ceiling is on new names, not on a full file.
        srv.set_xattr(h, b"user.a1", xattr::RAW, b"replaced")
            .unwrap();

        // Absence is ENODATA, from both verbs, and is the only error that means it.
        assert_eq!(
            srv.get_xattr(h, b"user.never-set", &mut page)
                .unwrap_err()
                .errno,
            xattr::ENODATA,
        );
        assert_eq!(
            srv.remove_xattr(h, b"user.never-set").unwrap_err().errno,
            xattr::ENODATA,
        );
        // And a buffer too small for the value is ERANGE, never a short read.
        let mut tiny = [0u8; 4];
        assert_eq!(
            srv.get_xattr(h, b"user.widest", &mut tiny)
                .unwrap_err()
                .errno,
            xattr::ERANGE,
        );
    }

    /// **A shorter blob does not leave the longer one's tail behind.** The store file is written and
    /// then truncated to length, because a write does not truncate; without the truncate the reader
    /// walks records nobody wrote. That is DECISIONS §27's four-times-corrected failure met in a
    /// place where it would present as corrupted metadata rather than as a longer file.
    #[test]
    fn a_shrinking_attribute_set_does_not_leave_a_tail() {
        let mut srv = server_with_tree();
        let h = srv.open_file("motd").unwrap();
        for i in 0..6 {
            srv.set_xattr(h, format!("user.n{i}").as_bytes(), 1, &[b'x'; 200])
                .unwrap();
        }
        assert_eq!(attr_names(&mut srv, h).len(), 6);

        // Take five of them away and shrink the sixth's value: every byte of the old blob is past
        // the end of the new one.
        for i in 0..5 {
            srv.remove_xattr(h, format!("user.n{i}").as_bytes())
                .unwrap();
        }
        srv.set_xattr(h, b"user.n5", 1, b"tiny").unwrap();

        let disk = srv.fs.disk;
        let mut srv = Server::open(disk).unwrap();
        let h = srv.open_file("motd").unwrap();
        assert_eq!(attr_names(&mut srv, h), [b"user.n5".to_vec()]);
        assert_eq!(
            attr(&mut srv, h, b"user.n5").unwrap(),
            (1, b"tiny".to_vec())
        );

        // And the last one out takes the store file with it, so the store does not accumulate an
        // entry per node that ever held an attribute.
        let id = node_id_of(&mut srv, TreePtr::root(), "motd");
        srv.remove_xattr(h, b"user.n5").unwrap();
        assert!(
            !store_holds(&mut srv, id),
            "an emptied blob left its file behind"
        );
        assert!(attr_names(&mut srv, h).is_empty());
    }

    /// **[`CachedDisk`] tests (milestone 138 step 2).** A [`Disk`] over a `Vec<u8>` that counts every
    /// `read_at`/`write_at` call it receives, so a test can assert "the cache answered this one"
    /// rather than merely "the answer was right", which every other disk fake here already checks by
    /// construction.
    struct CountingDisk {
        image: Vec<u8>,
        reads: usize,
        writes: usize,
    }

    impl CountingDisk {
        fn new(mib: usize) -> Self {
            Self {
                image: vec![0u8; mib * 1024 * 1024],
                reads: 0,
                writes: 0,
            }
        }
    }

    impl Disk for CountingDisk {
        unsafe fn read_at(&mut self, block: u64, buffer: &mut [u8]) -> Result<usize> {
            self.reads += 1;
            let off = block as usize * BLOCK;
            buffer.copy_from_slice(&self.image[off..off + buffer.len()]);
            Ok(buffer.len())
        }

        unsafe fn write_at(&mut self, block: u64, buffer: &[u8]) -> Result<usize> {
            self.writes += 1;
            let off = block as usize * BLOCK;
            self.image[off..off + buffer.len()].copy_from_slice(buffer);
            Ok(buffer.len())
        }

        fn size(&mut self) -> Result<u64> {
            Ok(self.image.len() as u64)
        }
    }

    /// A repeat read of the same block is answered from the cache: the inner disk sees it once.
    #[test]
    fn a_repeated_single_block_read_hits_the_cache() {
        let mut disk = CachedDisk::new(CountingDisk::new(1), 8);
        let mut buf = [0u8; BLOCK];
        for _ in 0..5 {
            unsafe { disk.read_at(3, &mut buf) }.unwrap();
        }
        assert_eq!(
            disk.inner.reads, 1,
            "four of the five reads should have hit"
        );
    }

    /// **The property the whole type exists for**: a write to a cached block is never served stale
    /// afterwards. This is the regression test a caching bug would fail without ever touching the
    /// device layer, which is the point of building it host-side.
    #[test]
    fn a_write_through_the_cache_is_read_back_fresh() {
        let mut disk = CachedDisk::new(CountingDisk::new(1), 8);
        let mut buf = [0u8; BLOCK];
        unsafe { disk.read_at(3, &mut buf) }.unwrap(); // populate the cache
        assert_eq!(buf, [0u8; BLOCK]);

        let new_bytes = [7u8; BLOCK];
        unsafe { disk.write_at(3, &new_bytes) }.unwrap();

        let mut buf2 = [0u8; BLOCK];
        unsafe { disk.read_at(3, &mut buf2) }.unwrap();
        assert_eq!(buf2, new_bytes, "a stale cache entry would fail this");
        // And the read that mattered was answered from the cache, not the disk: the write updated
        // the entry rather than merely invalidating it.
        assert_eq!(
            disk.inner.reads, 1,
            "only the first, cold read should have reached the disk"
        );
    }

    /// A write shorter than a block invalidates rather than caching a guess, because this type never
    /// sees the read-modify-write merge that lands underneath it.
    #[test]
    fn a_short_write_invalidates_rather_than_caching_a_guess() {
        let mut disk = CachedDisk::new(CountingDisk::new(1), 8);
        let mut buf = [0u8; BLOCK];
        unsafe { disk.read_at(3, &mut buf) }.unwrap(); // populate the cache
        // A short write: the inner disk's contract is to report exactly what it wrote, and a real
        // Disk::write_at that read-modify-writes still reports the full chunk length it was asked
        // for (see BlockDisk::write_at), so simulate the case a Disk implementation reporting a
        // genuinely short count would produce, e.g. a write cut off by the device.
        struct ShortWrite(CountingDisk);
        impl Disk for ShortWrite {
            unsafe fn read_at(&mut self, block: u64, buffer: &mut [u8]) -> Result<usize> {
                unsafe { self.0.read_at(block, buffer) }
            }
            unsafe fn write_at(&mut self, block: u64, buffer: &[u8]) -> Result<usize> {
                unsafe { self.0.write_at(block, &buffer[..BLOCK / 2]) }
            }
            fn size(&mut self) -> Result<u64> {
                self.0.size()
            }
        }
        let mut disk = CachedDisk::new(ShortWrite(CountingDisk::new(1)), 8);
        unsafe { disk.read_at(3, &mut buf) }.unwrap();
        let new_bytes = [7u8; BLOCK];
        unsafe { disk.write_at(3, &new_bytes) }.unwrap();
        // The entry must be gone: a subsequent read has to reach the disk again rather than answer
        // from a cache that only ever saw half the new bytes.
        let reads_before = disk.inner.0.reads;
        let mut buf2 = [0u8; BLOCK];
        unsafe { disk.read_at(3, &mut buf2) }.unwrap();
        assert_eq!(
            disk.inner.0.reads,
            reads_before + 1,
            "a short write must invalidate, not cache a partial guess"
        );
    }

    /// A collision (two block numbers sharing one slot) evicts rather than corrupting: the newer
    /// block reads back correctly and the older one, now a miss, still reads back correctly too
    /// (from the inner disk, since the cache no longer has it).
    #[test]
    fn a_slot_collision_evicts_rather_than_corrupts() {
        let mut disk = CachedDisk::new(CountingDisk::new(1), 4); // capacity 4: blocks 0 and 4 collide
        let mut a = [0u8; BLOCK];
        let mut b = [0u8; BLOCK];
        unsafe { disk.write_at(0, &[1u8; BLOCK]) }.unwrap();
        unsafe { disk.write_at(4, &[2u8; BLOCK]) }.unwrap(); // evicts block 0's entry
        unsafe { disk.read_at(0, &mut a) }.unwrap(); // a miss now: must come from the disk, not block 4's cached bytes
        unsafe { disk.read_at(4, &mut b) }.unwrap();
        assert_eq!(
            a, [1u8; BLOCK],
            "block 0 must read its own bytes, not block 4's"
        );
        assert_eq!(b, [2u8; BLOCK]);
    }

    /// A multi-block read (a record body) bypasses the cache entirely: it always reaches the disk,
    /// and it never populates or consults an entry, so it cannot collide with the single-block
    /// working set the cache exists for.
    #[test]
    fn a_multi_block_read_bypasses_the_cache() {
        let mut disk = CachedDisk::new(CountingDisk::new(1), 8);
        let mut buf = [0u8; BLOCK * 2];
        for _ in 0..3 {
            unsafe { disk.read_at(0, &mut buf) }.unwrap();
        }
        assert_eq!(
            disk.inner.reads, 3,
            "every multi-block read must reach the disk; none of them should have been a cache hit"
        );
    }

    /// [`CachedDisk`] over a real image behaves exactly as the uncached path does: same bytes back,
    /// through the ordinary `Server` API rather than through `Disk` directly.
    #[test]
    fn cached_disk_reads_the_same_bytes_as_uncached() {
        let mut fs = FileSystem::create(BlockDisk(VecIo(vec![0u8; 16 * 1024 * 1024])), None, 0, 0)
            .expect("mkfs through BlockDisk");
        fs.tx(|tx| {
            let ptr = tx
                .create_node(TreePtr::root(), "f", Node::MODE_FILE | 0o644, 0, 0)?
                .ptr();
            tx.write_node(ptr, 0, b"cached-disk-content", 0, 0)?;
            Ok(())
        })
        .expect("populate");
        let image = fs.disk.0.0;

        let mut plain = Server::open(BlockDisk(VecIo(image.clone()))).expect("open, uncached");
        let mut cached =
            Server::open(CachedDisk::new(BlockDisk(VecIo(image)), 8)).expect("open, cached");

        let mut buf_a = [0u8; 32];
        let mut buf_b = [0u8; 32];
        let ha = plain.open_file("f").unwrap();
        let hb = cached.open_file("f").unwrap();
        // Read twice through the cached path, so a caching bug would have a chance to show up.
        for _ in 0..2 {
            let na = plain.read(ha, 0, &mut buf_a).unwrap();
            let nb = cached.read(hb, 0, &mut buf_b).unwrap();
            assert_eq!(na, nb);
            assert_eq!(buf_a[..na], buf_b[..nb]);
        }
        assert_eq!(&buf_a[..19], b"cached-disk-content");
    }
}
