//! **The milestone-32 filesystem wire protocols** (notes/fs-server.md).
//!
//! Two userspace protocols, one crate, so the four programs that speak them, the block server, the
//! FS server, the client, and the kernel-side tests, share one definition and cannot drift. The
//! kernel routes these words the way it routes any IPC (§10, §12): it never reads an opcode. Adding
//! one is a change to this crate and the note, not to the syscall surface. This is the same split
//! `lineedit::proto` makes for the terminal contract.
//!
//! # The two protocols
//!
//! ```text
//!   disk ──virtio──►┌──────────────┐──blk IPC──►┌───────────┐──file IPC──► client
//!                   │ block server │            │ FS server │
//!                   └──────────────┘◄───────────└───────────┘◄──────────── (a granted
//!                        (owns the DMA            (owns RedoxFS +            directory cap)
//!                         confinement)             its own heap)
//! ```
//!
//! Nobody names anyone else (endpoint-only naming, notes/ipc-naming.md). The FS server holds "an
//! endpoint I read/write blocks on"; the client holds "an endpoint that opens and reads files under
//! the one directory this endpoint is bound to." Rewire the endpoints and neither side can tell.
//!
//! ## Control by message, bulk by shared page (DECISIONS §10)
//!
//! Both protocols are an endpoint `CALL`: the client sends two words and blocks until the server
//! replies through the one-shot Reply capability the kernel mints. Bulk bytes never ride in the
//! words. They travel in a page the two parties share, exactly [`PAGE`] bytes, one per channel. For
//! blk IPC the page holds one filesystem block; for file IPC it holds a name (on open) or file data
//! (on read/write).
//!
//! ## The error boundary
//!
//! Every reply's first word is an [`i64`]: **non-negative is a result, negative is an error**, and
//! the error value is exactly the negated errno ([`reply_err`]). RedoxFS's error type is
//! `syscall::error::Error { errno: i32 }` (the Linux numbers), so the FS server maps it to the wire
//! with `-(err.errno as i64)` at the serve loop and nowhere else, which is the "map the error type
//! once, at the server boundary" rule the roadmap sets. The client reads it back with [`reply_errno`].

#![no_std]

/// The shared-page size, in bytes. One RedoxFS block (`redoxfs::BLOCK_SIZE`) and one host page, so a
/// block move is a page move and the frame the kernel maps into both address spaces holds exactly
/// one unit of transfer. A read or write larger than this is chunked by the caller.
pub const PAGE: usize = 4096;

/// Where a request packs its opcode: bits 63:56 of the first `CALL` word, the same position
/// `lineedit::proto` uses, so the two contracts read alike.
pub const OP_SHIFT: u32 = 56;

/// Build a request's first word from just an opcode (the block protocol's shape: the operand, a
/// block index, rides in the second word).
pub const fn req(op: u64) -> u64 {
    op << OP_SHIFT
}

/// The opcode of any request word.
pub const fn op(w0: u64) -> u64 {
    w0 >> OP_SHIFT
}

/// Turn a redox/POSIX errno into a reply's first word: the negated number, so the client can invert
/// it. This is the ONE place the error convention is defined; the FS server calls it at its boundary.
pub const fn reply_err(errno: i32) -> i64 {
    -(errno as i64)
}

/// The errno behind an error reply, or `None` if the reply is a (non-negative) success. The inverse
/// of [`reply_err`], for a client turning a wire error back into an `ErrorKind`.
pub const fn reply_errno(r0: i64) -> Option<i32> {
    if r0 < 0 { Some((-r0) as i32) } else { None }
}

/// **The block-IPC protocol** (FS server → block server). Three synchronous methods shaped exactly
/// like the RedoxFS `Disk` trait the FS server implements over it: read a block, write a block, ask
/// the size. The unit of transfer is one filesystem block ([`PAGE`] bytes); the block server splits
/// each into the eight 512-byte virtio sectors the device actually moves.
pub mod blk {
    use super::PAGE;

    /// Read filesystem block `w1` from the disk into the shared page. Reply `r0` = 0 on success, or
    /// [`super::reply_err`] on failure; the [`PAGE`] bytes land in the shared page.
    pub const READ: u64 = 1;
    /// Write the [`PAGE`] bytes now in the shared page to filesystem block `w1`. Reply `r0` = 0 on
    /// success, or [`super::reply_err`] on failure.
    pub const WRITE: u64 = 2;
    /// Ask the disk's size in bytes. `w1` ignored. Reply `r0` = the size (always non-negative here).
    pub const SIZE: u64 = 3;

    /// One filesystem block, in bytes: the transfer unit and the shared-page size. Equal to
    /// `redoxfs::BLOCK_SIZE`; asserted against it in the block server so a future RedoxFS bump that
    /// changed the block size would fail to build rather than silently corrupt.
    pub const BLOCK_SIZE: usize = PAGE;

    /// The number of 512-byte virtio sectors in one filesystem block (`BLOCK_SIZE / 512 = 8`). The
    /// block server issues this many sector transfers per blk request.
    pub const SECTORS_PER_BLOCK: u64 = (BLOCK_SIZE as u64) / 512;
}

/// **The file-service protocol** (client → FS server). The contract is capability-shaped from birth
/// (notes/fs-server.md): the endpoint a client holds IS the directory capability, bound in the
/// server to one directory node, and every name in [`fs::OPEN`] is resolved relative to it. There is no
/// global namespace and no absolute path; a client with no such endpoint can open nothing, and the
/// refusal is "no such capability" (it holds no endpoint), never a permission check. An [`fs::OPEN`]
/// hands back a **handle**, a small integer the server issues and validates against this session's
/// table; a handle is likewise a capability, meaningless to forge because the server only honors the
/// ones it minted.
pub mod fs {
    /// **The session's bound directory, which is always handle 0.**
    ///
    /// The server installs its bound directory in the handle table before it serves anything, so the
    /// directory a client's endpoint designates is an ordinary directory handle like any other and
    /// every name-taking verb resolves against a handle rather than against a hidden field. That is
    /// Plan 9's answer in one number: `/` is the root of *your* namespace, and two clients on two
    /// endpoints both say `0` and mean different directories.
    ///
    /// It is 0 because every client already sent 0 in the handle field of an [`OPEN`], which the
    /// server ignored. Making 0 mean exactly what those clients meant costs no wire change; file
    /// handles now start at 1.
    pub const ROOT: u64 = 0;

    /// Resolve the name in the shared page (its length is [`req_len`] of the request word) under the
    /// directory handle in [`req_handle`] ([`ROOT`] for the endpoint's bound directory). Second word
    /// 0. Reply `r0` = a handle (≥ 0) or an error.
    ///
    /// Needs [`super::dir::READ`] or [`super::dir::WRITE`] on that directory, and the file handle it
    /// returns **inherits both bits** from it, so what you may do to the file was decided when the
    /// directory was granted. Without either the answer is `ENOENT`: in this scope there is no such
    /// name. `EISDIR` if the name is a directory ([`OPENDIR`] is the verb for that).
    pub const OPEN: u64 = 1;
    /// Read up to [`req_len`] bytes from the handle ([`req_handle`]) at offset `w1` into the shared
    /// page. Reply `r0` = bytes read (≥ 0, 0 at EOF) into the shared page, or an error.
    pub const READ: u64 = 2;
    /// Write [`req_len`] bytes from the shared page to the handle at offset `w1`. Reply `r0` = bytes
    /// written (≥ 0), or an error.
    pub const WRITE: u64 = 3;
    /// Close the handle ([`req_handle`]). Reply `r0` = 0, or an error if the handle was not open.
    pub const CLOSE: u64 = 4;
    /// The current size in bytes of the handle's file. Reply `r0` = size (≥ 0), or an error.
    pub const FSTAT: u64 = 5;
    /// Create the name in the shared page (length is [`req_len`]) under the directory handle in
    /// [`req_handle`] ([`ROOT`] for the endpoint's bound directory) and open it. Second word 0.
    /// Reply `r0` = a handle (≥ 0), or an error.
    ///
    /// Needs [`super::dir::CREATE`] on that directory; without it the answer is
    /// [`super::dir::EROFS`], because through this capability the directory takes no new names.
    ///
    /// **`EEXIST` if the name already exists, and nothing is modified.** Create is create, not
    /// create-or-open: a caller that wants either must ask for both and say which it got. The
    /// alternative makes a partly-working write read as a working one, which is the failure §27
    /// records. Shares [`OPEN`]'s shape exactly, so a client that can open can create.
    pub const CREATE: u64 = 6;
    /// Set the size of the handle's ([`req_handle`]) file to exactly `w1` bytes. [`req_len`] is 0;
    /// the size rides in the second word because it is an offset-shaped quantity, not a payload
    /// length. Reply `r0` = 0, or an error.
    ///
    /// POSIX `ftruncate` in **both** directions: shrinking discards the bytes past the new size,
    /// growing extends with zeroes. The shrink is the point. Without it a write shorter than the file
    /// leaves the old tail in place, so a caller replacing a file's contents gets a longer file than
    /// it wrote, and a write that half-works reads as a write that failed (DECISIONS §27, four times
    /// corrected). Truncating to the current size is a no-op, which matters because `std::fs::write`
    /// truncates unconditionally.
    pub const TRUNCATE: u64 = 7;
    /// **Descend: resolve the name in the shared page under the directory handle in [`req_handle`]
    /// and hand back a handle to the child directory, carrying rights.** The requested rights ride
    /// in the second word as a [`super::dir`] mask. Reply `r0` = the new directory handle (≥ 0), or
    /// an error.
    ///
    /// This is the first verb in this contract that hands back **authority** rather than data, which
    /// is why its rules are stated here rather than left to the implementation:
    ///
    /// - The child's rights are `parent & requested`, computed by
    ///   [`super::dir::Rights::attenuate`], which is the only constructor for a non-root rights set
    ///   and cannot widen. A child of a child is attenuated again, so **no descendant of a directory
    ///   capability can carry a right the capability did not have**, at any depth.
    /// - If that intersection is not what was asked for, the request is refused with
    ///   [`super::dir::EPERM`] rather than quietly granted the smaller set. The refusal is a
    ///   courtesy, not the safety property: delete it and the intersection above still holds.
    /// - It needs [`super::dir::DESCEND`] on the parent. Without it the answer is `ENOENT`, so a
    ///   holder that may not walk into a subtree cannot learn that the subtree is there.
    /// - `ENOTDIR` if the name is a file. `EINVAL` if the name is not a single component, which is
    ///   what keeps `..` from meaning anything.
    pub const OPENDIR: u64 = 8;
    /// **Enumerate a directory handle.** [`req_handle`] is the directory, the second word is a
    /// **cursor**: the index of the first entry to return, 0 to start. The reply's `r0` is the
    /// number of bytes written into the shared page, encoded as [`super::dirent`] records; `r0` = 0
    /// means the cursor is past the end. [`req_len`] is ignored.
    ///
    /// Needs [`super::dir::ENUMERATE`], and its absence is [`super::dir::EPERM`], **not an empty
    /// listing**. An empty listing would be a lie about the directory rather than a fact about the
    /// capability, and DECISIONS §42's rule is that a verb which is not offered fails loudly instead
    /// of degrading silently.
    ///
    /// Entries come back sorted by name so the cursor means the same thing across calls; a directory
    /// changed between two calls of one enumeration can therefore repeat or skip a name, which is
    /// the ordinary readdir caveat and is recorded rather than fixed.
    pub const READDIR: u64 = 9;
    /// **Make a child directory and hand back a capability to it**, which is why it lives here
    /// rather than beside [`CREATE`]: `mkdir` is descend-with-creation, and milestone 47's
    /// instruction is that the two be designed together rather than separately.
    ///
    /// Shares [`OPENDIR`]'s shape exactly (the name in the shared page, the requested rights in the
    /// second word, a directory handle in the reply) and differs in two things: it needs
    /// [`super::dir::CREATE`] as well as [`super::dir::DESCEND`], and it answers `EEXIST` if the
    /// name is already there rather than opening what it found. Create is create, for the reason
    /// [`CREATE`] gives.
    ///
    /// The new directory's rights are attenuated from the parent's exactly as [`OPENDIR`]'s are, so
    /// a program cannot mint itself more authority by *making* a directory than by finding one.
    pub const MKDIR: u64 = 10;
    /// **Move a name from one directory to another, or to a different name in the same one.**
    ///
    /// The only verb in this contract that names **two** directories, so it is the only one whose
    /// second word is not a scalar: the source is [`req_handle`]/[`req_len`] of the request word as
    /// usual, and the destination rides in the second word packed by [`rename_dst`], which uses the
    /// same two fields. Both names are in the shared page, the **source first**, back to back, so
    /// the source occupies `[0, req_len)` and the destination `[req_len, req_len + dst_len)`.
    /// Reply `r0` = 0, or an error.
    ///
    /// # The two atomicities, stated apart (DECISIONS §42)
    ///
    /// - **Concurrency-atomic: yes.** No observer sees the intermediate state where the name exists
    ///   in both places or neither. The FS server runs one request to completion before it receives
    ///   the next, so inside the server there is no concurrent observer to see anything.
    /// - **Crash-atomic: yes.** The whole rename runs in one RedoxFS transaction and reaches the
    ///   platter through one commit in the header ring, so a fresh mount finds the old name or the
    ///   new one and never both or neither. This is measured, not argued: it is the last operation
    ///   of `fs-server/tests/crash_consistency.rs`'s workload, so the sweep that cuts the device at
    ///   every write in that workload cuts inside this one too.
    ///
    /// The temp-file-then-rename idiom needs both, which is why §42 states them separately: saying
    /// "atomic" and letting the reader assume the stronger one is the mistake POSIX made.
    ///
    /// # Rights, and what is deliberately not offered
    ///
    /// Needs [`super::dir::REMOVE`] on the **source** directory (its name goes away) and
    /// [`super::dir::CREATE`] on the **destination** (a name appears). Both are mutating rights, so
    /// a capability lacking either answers [`super::dir::EROFS`]: through this capability that
    /// directory does not give names up, or does not take new ones. Within one directory a rename
    /// needs both on that one directory. This is the rung that made `REMOVE` real: until this verb
    /// existed nothing on the wire consulted it, and a right nothing enforces is a right the
    /// contract is lying about.
    ///
    /// - **`renameat2`'s `EXCHANGE` and `NOREPLACE` are not offered** (§42). They work on ext4,
    ///   btrfs, XFS, f2fs and tmpfs and nowhere else, and emulating `NOREPLACE` with link-then-
    ///   unlink is racy, so offering them would make behaviour backend-specific.
    /// - **Cross-filesystem move is not this verb** (§42). It is copy-then-unlink, a different
    ///   object with a different identity, non-atomic by nature. It cannot be reached here anyway:
    ///   both handles are minted by one server bound to one image.
    /// - **A directory can be renamed in place but not moved into another directory**, and the
    ///   answer is `EINVAL`. POSIX's guard against making a directory its own descendant is a
    ///   path-prefix test, and this contract has no paths to take a prefix of; the equivalent is an
    ///   ancestry walk in the server, which is real recursion in a process whose stack is measured
    ///   at three quarters used. Renaming within one directory cannot create a cycle, because the
    ///   node's parent does not change, and that is the boundary the refusal is drawn at. §42's rule
    ///   is that a verb which is not offered fails loudly rather than degrading, and this does.
    ///
    /// If the destination name exists it is **replaced**, provided it is the same kind as the
    /// source: a file over a directory is `EISDIR` and a directory over a file is `ENOTDIR`, POSIX's
    /// own answers, and a non-empty destination directory is `ENOTEMPTY`. The kinds are checked here
    /// rather than left to the backend, because §42's whole point is that an offered verb means one
    /// thing everywhere. Renaming a name onto itself is a successful no-op.
    pub const RENAME: u64 = 11;
    /// **Remove a name from a directory. This is `unlink`, and it is not revocation.**
    ///
    /// The name in the shared page (length is [`req_len`]) is taken out of the directory in
    /// [`req_handle`]. Second word 0. Reply `r0` = 0, or an error.
    ///
    /// # The two operations Unix conflated, and which one this is
    ///
    /// Milestone 47's instruction is that `rm` stop meaning both at once, so this verb means exactly
    /// one of them and says which:
    ///
    /// - **Unlink (this verb).** The *name* goes away. A handle already open on that file **keeps
    ///   reading**, and the bytes are still there, exactly as POSIX promises; the object dies when
    ///   the last name and the last handle are both gone. Atomic replace and the temp-file idiom
    ///   both depend on this, which is why it is worth having rather than an accident to be tidied
    ///   up. The FS server makes it true by registering an open node with the engine's usage table,
    ///   so removing the last link **queues** the node rather than freeing it (`release_node`).
    /// - **Revoke.** The object dies and every capability to it goes stale. Not offered here, and
    ///   the absence is deliberate rather than an oversight: it would mean invalidating handles the
    ///   server minted for clients it cannot enumerate (the handle table is per *server*, and the
    ///   server does not track which client holds which handle). §13 revokes frames and §16 revokes
    ///   objects because the kernel owns those tables; nothing owns this one that way yet. A verb
    ///   that removed a name and *called itself* revocation would be the silent degradation §42
    ///   forbids, so `rm` unlinks and says so.
    ///
    /// # Rights, and the kind check
    ///
    /// Needs [`super::dir::REMOVE`] on the directory, refused with [`super::dir::EROFS`]: through
    /// this capability that directory does not give names up. The right is checked **before the name
    /// is resolved**, so a capability that may not remove cannot use this verb to find out whether a
    /// name is there.
    ///
    /// **A directory is refused with [`super::dir::EISDIR`]**, POSIX's own answer. [`RMDIR`] is the
    /// verb for a directory, and it takes only an **empty** one, so no single call on this contract
    /// can take a subtree away. "Remove this name whatever it is" is how `rm` destroys a tree for
    /// someone who typed one word, and this contract does not offer it at any opcode.
    pub const UNLINK: u64 = 12;
    /// **Remove an empty directory. Unix's `rmdir(2)`, and empty-only is the whole safety
    /// property.**
    ///
    /// [`UNLINK`]'s shape exactly: the name in the shared page (length is [`req_len`]), resolved
    /// under the directory handle in [`req_handle`], second word 0, reply `r0` = 0 or an error. It
    /// hands nothing back, which is what separates removing a name from destroying an object.
    ///
    /// # Empty-only, and why that is not a limitation to be lifted
    ///
    /// A directory with anything in it is [`super::dir::ENOTEMPTY`], refused rather than emptied.
    /// **The recursion in `rm -r` lives in userspace** (`user/src/rm.rs`), as a loop of individually
    /// safe single-step operations: enumerate, unlink the files, remove the empty directories from
    /// the bottom up. Each step is one request this server runs to completion, and each step needs
    /// the rights for it at *that* level, so a recursive removal stops exactly where the
    /// capabilities stop. A `RMDIR` that removed whatever it found would put the whole subtree
    /// behind one call, and no capability check afterwards could undo that.
    ///
    /// This also means an interrupted `rm -r` leaves a **partial tree**, which is what Unix does
    /// too. There is no transaction spanning requests, and adding one would mean the server holding
    /// a transaction open across receives, which breaks the run-one-request-to-completion property
    /// the concurrency atomicity of [`RENAME`] rests on.
    ///
    /// # Rights, the kind check, and what this is not
    ///
    /// Needs [`super::dir::REMOVE`] on the parent, refused with [`super::dir::EROFS`] and checked
    /// **before the name is resolved**, so a capability that may not remove cannot use this verb to
    /// find out whether a name is there. A **file** is `ENOTDIR`, POSIX's own answer for `rmdir` on
    /// one, and the mirror of [`UNLINK`]'s `EISDIR`: neither verb will remove the kind the other one
    /// is for.
    ///
    /// **It is not revocation**, for [`UNLINK`]'s reason and not a lesser version of it: the FS
    /// server's handle table is per *server*, so handles cannot be invalidated for clients the
    /// server cannot enumerate. A holder of a directory handle whose name this verb removes keeps a
    /// handle onto a node the engine has freed, and its next request through it fails (`ENOENT`)
    /// rather than reaching something else. See the BUGS section of notes/rm-recursion.md.
    pub const RMDIR: u64 = 13;

    /// The largest length or offset that fits the packing below (40 bits). Far above [`super::PAGE`],
    /// so a single request never carries more than one page of payload regardless; the bound only
    /// guards the bit-packing.
    pub const MAX_LEN: u64 = (1 << 40) - 1;
    /// The largest handle the packing can carry (16 bits). The server's table is far smaller.
    pub const MAX_HANDLE: u64 = 0xffff;

    /// Pack a file request's first word: opcode (bits 63:56), handle (bits 55:40), length (bits
    /// 39:0). [`OPEN`] and [`CREATE`] pass handle 0 and length = the name length;
    /// [`READ`]/[`WRITE`] pass the handle and the byte count; [`CLOSE`]/[`FSTAT`]/[`TRUNCATE`] pass
    /// the handle and length 0 ([`TRUNCATE`]'s new size rides in the second word).
    pub const fn req(op: u64, handle: u64, len: u64) -> u64 {
        (op << super::OP_SHIFT) | ((handle & MAX_HANDLE) << 40) | (len & MAX_LEN)
    }
    /// The handle of a file request word.
    pub const fn req_handle(w0: u64) -> u64 {
        (w0 >> 40) & MAX_HANDLE
    }
    /// The length/count of a file request word.
    pub const fn req_len(w0: u64) -> usize {
        (w0 & MAX_LEN) as usize
    }

    /// Pack [`RENAME`]'s **destination** half into the second word: the same handle and length
    /// fields [`req`] uses, minus the opcode, because a rename names two directories and one word
    /// cannot carry both. Read back with [`dst_handle`] and [`dst_len`].
    pub const fn rename_dst(handle: u64, len: u64) -> u64 {
        ((handle & MAX_HANDLE) << 40) | (len & MAX_LEN)
    }
    /// The destination directory handle of a [`RENAME`]'s second word.
    pub const fn dst_handle(w1: u64) -> u64 {
        (w1 >> 40) & MAX_HANDLE
    }
    /// The destination name's length, in bytes, of a [`RENAME`]'s second word.
    pub const fn dst_len(w1: u64) -> usize {
        (w1 & MAX_LEN) as usize
    }
}

/// **What a directory capability carries** (milestone 47): the rights ladder, and the one
/// constructor that makes attenuation monotonic.
///
/// A directory capability was one authority until this module existed, which meant that handing a
/// program somewhere to write its logs also handed it the power to delete what was already there.
/// Milestone 47's answer is that a directory is **six separable rights**, and the answer to "can a
/// child ever carry more than its parent" is *no, by construction*: [`dir::Rights::attenuate`] is a
/// bitwise AND with the parent, it is the only way to make a non-root [`dir::Rights`], and `a & b`
/// is a subset of `a` for every `b`. There is no code path that widens, so there is nothing to
/// forget.
///
/// # The three refusals, and why they are three
///
/// The errno a missing right answers is a design decision, not a detail, because it decides what the
/// holder *learns*:
///
/// - **A naming right** ([`dir::READ`]/[`dir::WRITE`] for [`fs::OPEN`], [`dir::DESCEND`] for
///   [`fs::OPENDIR`]) answers `ENOENT`. In this scope there is no such name, which is the
///   same sentence `fwarden` says for the same reason: a holder must not be able to map what it
///   cannot reach.
/// - **A mutating right** ([`dir::CREATE`], [`dir::REMOVE`], and [`dir::WRITE`] on a file handle)
///   answers [`dir::EROFS`]. Through this capability that directory is read-only. `EACCES` was
///   rejected here for the reason DECISIONS §27 rejected it for files: it implies a policy that
///   could have said yes, and there is no policy, only what the capability is.
/// - **[`dir::ENUMERATE`]** answers [`dir::EPERM`], and it is the one that cannot use either of the
///   other two. "No such name" makes no sense (you hold the directory), and an empty listing would
///   be a statement about the *directory* rather than about the capability, which is exactly the
///   silent degradation DECISIONS §42 forbids.
pub mod dir {
    /// List the names in it ([`super::fs::READDIR`]). Separable because enumeration is the right
    /// globbing and tab completion consume, and "you may open the file I named" should not imply
    /// "you may find out what else is in there".
    pub const ENUMERATE: u64 = 1 << 0;
    /// Open a name in it for reading, and read a file handle obtained through it.
    pub const READ: u64 = 1 << 1;
    /// Open a name in it for writing, and write or truncate a file handle obtained through it.
    /// Separate from [`READ`] because milestone 47's motivating case is a directory a program may
    /// append to and not read.
    pub const WRITE: u64 = 1 << 2;
    /// Make a new name in it ([`super::fs::CREATE`]).
    pub const CREATE: u64 = 1 << 3;
    /// Take a name out of it. This is the right the log-writing case exists to withhold: [`WRITE`]
    /// and [`CREATE`] without [`REMOVE`] is "add to this, destroy nothing", which is milestone 47's
    /// motivating sentence. Three verbs consult it: [`super::fs::RENAME`], whose source name goes
    /// away, [`super::fs::UNLINK`], which is `rm`'s, and [`super::fs::RMDIR`], which takes an empty
    /// directory's name out of its parent.
    pub const REMOVE: u64 = 1 << 4;
    /// Walk into a child directory ([`super::fs::OPENDIR`]).
    ///
    /// **Separate from [`READ`] on purpose**, and this is the rung milestone 47 did not name. If
    /// descending came with reading, then granting a directory would silently grant its whole
    /// subtree, transitively and to any depth, and how much authority a grant carried would be
    /// decided by the shape of the tree rather than by the grant. That is ambient authority
    /// reintroduced by recursion, which is the thing this milestone exists to refuse.
    pub const DESCEND: u64 = 1 << 5;

    /// Every right this contract defines. The mount binds its root with exactly this; nothing below
    /// the root can ever be constructed with more.
    pub const ALL: u64 = ENUMERATE | READ | WRITE | CREATE | REMOVE | DESCEND;

    /// **What a recursive removal needs, at every level** (milestone 47's `rm -r`): [`ENUMERATE`] to
    /// see what is there, [`DESCEND`] to walk into it, [`REMOVE`] to take names out. Named here
    /// because the `rm` program asks for exactly this on every descent and the wiring that grants it
    /// hands over exactly this, so the two cannot drift into a capability nobody meant.
    ///
    /// **It carries no [`READ`] and no [`WRITE`]**, which is not an oversight but the interesting
    /// part: a program handed this can destroy the subtree and cannot read one byte of it. On Unix
    /// the same `rm` could read every file it is about to unlink, because the authority came from
    /// the process rather than from the command.
    pub const REMOVE_TREE: u64 = ENUMERATE | DESCEND | REMOVE;

    /// `EROFS`: a mutating right the capability does not carry. The same number and the same
    /// argument as [`super::grant::EROFS`].
    pub const EROFS: i32 = 30;
    /// `EPERM`: [`ENUMERATE`] withheld. The only refusal here that must be loud rather than
    /// concealing, because concealment would mean lying about the directory.
    pub const EPERM: i32 = 1;
    /// `EISDIR`: [`super::fs::OPEN`] of a name that is a directory. [`super::fs::OPENDIR`] is the
    /// verb, and answering with a useless file handle instead is how a caller ends up reading a
    /// directory's raw bytes and believing them.
    pub const EISDIR: i32 = 21;
    /// `ENOTDIR`: [`super::fs::OPENDIR`] of a name that is a file, or a directory verb aimed at a
    /// file handle. Same number and same argument as [`super::grant::ENOTDIR`].
    pub const ENOTDIR: i32 = 20;
    /// `EINVAL`: a name that is not a single component, and [`super::fs::RENAME`]'s refusal to move
    /// a **directory** between two directories (the verb's own doc gives the cycle argument).
    pub const EINVAL: i32 = 22;
    /// `ENOTEMPTY`: [`super::fs::RENAME`] over a destination directory that still has entries in it,
    /// and [`super::fs::RMDIR`] of one. On `RMDIR` it is the safety property rather than an
    /// inconvenience: a verb that emptied what it found would put a whole subtree behind one call.
    pub const ENOTEMPTY: i32 = 39;

    /// **What a refusal from this contract means, in one sentence a shell can print.**
    ///
    /// The errno a verb answers is part of this module's design rather than an implementation
    /// detail, because it decides what the holder *learns* (see this module's header). So the
    /// sentence that renders it belongs here too, next to the decision, instead of in each program
    /// that has to explain itself to a human. Every line is a statement about **the capability or
    /// the thing**, never about a policy: there is no policy here to have said yes.
    ///
    /// `EACCES` is deliberately absent: nothing in this contract answers it (DECISIONS §27, §47).
    pub const fn explain(errno: i32) -> &'static str {
        match errno {
            // ENOENT. Two very different situations, one sentence, and that is the point: a holder
            // that may not reach a name must not be able to tell "it is not there" from "you may
            // not name it". The sentence is true either way.
            2 => "no such name in this directory",
            EPERM => "this capability does not carry that right",
            EROFS => "through this capability that directory is read-only",
            EISDIR => "that is a directory",
            ENOTDIR => "that is not a directory",
            // EEXIST. Create is create, not create-or-open, so this is a fact about the directory.
            17 => "that name is already there",
            ENOTEMPTY => "that directory is not empty",
            // EBADF, EMFILE: facts about a handle rather than about a capability.
            9 => "no such handle",
            24 => "too many handles open at once",
            EINVAL => "that is not a name this contract can resolve",
            _ => "the filesystem refused",
        }
    }

    /// **A rights set on one directory or file handle.** Opaque on purpose: the only ways to make
    /// one are [`Rights::root`], which the mount calls once for the directory the endpoint is bound
    /// to, and [`Rights::attenuate`], which cannot widen.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Rights(u64);

    impl Rights {
        /// The rights the endpoint's bound directory carries. This is the only place a rights set is
        /// made out of thin air, and the only caller is the code that binds a server to a directory.
        pub const fn root(mask: u64) -> Self {
            Rights(mask & ALL)
        }

        /// **The child's rights: what the parent has, intersected with what was asked for.**
        ///
        /// The whole of milestone 47's monotonicity property is this one `&`. It is a total
        /// function with no failure mode, which is the point: a caller cannot ask for a right the
        /// parent lacks and receive it, because there is no branch here to get wrong. The server
        /// separately *refuses* a request whose intersection came up short, so a caller is never
        /// silently given less than it asked for, but that refusal is about telling the truth and
        /// this line is about the property.
        pub const fn attenuate(self, requested: u64) -> Self {
            Rights(self.0 & requested)
        }

        /// Whether this set carries **every** right in `needed`. Written as "all of", not "any of",
        /// because an operation that needs two rights and is allowed by either is a hole.
        pub const fn allows(self, needed: u64) -> bool {
            self.0 & needed == needed
        }

        /// Whether it carries none of `any`. The negative form, for the "neither read nor write"
        /// test [`super::fs::OPEN`] makes.
        pub const fn denies_all(self, any: u64) -> bool {
            self.0 & any == 0
        }

        /// The raw mask, for reporting it (a test's verdict, or what `caps` would print). Not a way
        /// back into a [`Rights`]: there is no constructor that takes this.
        pub const fn bits(self) -> u64 {
            self.0
        }
    }

    /// Machine-checked, because "a child can never exceed its parent" is the claim this whole module
    /// exists to make and a test can only try the masks somebody thought of.
    #[cfg(kani)]
    mod proofs {
        use super::*;

        /// **Attenuation never widens, for every parent and every request.** `allows` is the only
        /// question the server asks a rights set, so the property is stated in its terms: anything
        /// the child permits, the parent permitted.
        #[kani::proof]
        fn attenuate_never_widens() {
            let parent = Rights(kani::any());
            let requested: u64 = kani::any();
            let needed: u64 = kani::any();
            let child = parent.attenuate(requested);
            assert!(!child.allows(needed) || parent.allows(needed));
        }

        /// And it never widens **at any depth**, which is the property a tree walk depends on: two
        /// descents are still bounded by the root. `attenuate` is idempotent-shaped rather than
        /// merely monotone, and a proof is cheaper than trusting that AND is associative in code
        /// somebody may later rewrite.
        #[kani::proof]
        fn a_grandchild_is_bounded_by_the_root() {
            let root = Rights(kani::any());
            let a: u64 = kani::any();
            let b: u64 = kani::any();
            let needed: u64 = kani::any();
            let grandchild = root.attenuate(a).attenuate(b);
            assert!(!grandchild.allows(needed) || root.allows(needed));
        }

        /// A root is bounded by [`ALL`], so a caller that invents a mask with unknown bits set
        /// cannot smuggle one in and have some later version of this module give it a meaning.
        #[kani::proof]
        fn a_root_carries_nothing_undefined() {
            let mask: u64 = kani::any();
            assert!(Rights::root(mask).bits() & !ALL == 0);
        }
    }
}

/// **How [`fs::READDIR`] packs a directory listing into the shared page.**
///
/// One record per entry: a flags byte, a length byte, then that many bytes of name. Length-prefixed
/// rather than NUL-terminated because a name is bytes to this contract and a terminator inside one
/// would silently truncate it, and one byte because RedoxFS's own limit on a directory entry is well
/// under 255.
///
/// The reply's `r0` is how many bytes of the page the server filled, so a client iterates until it
/// has consumed exactly that many. A record is never split across replies: the server stops before
/// one that would not fit and the cursor picks up there, so "the page was full" and "the directory
/// ended" are told apart by `r0` rather than by guessing.
pub mod dirent {
    /// The entry is a directory, so [`super::fs::OPENDIR`] is the verb for it rather than
    /// [`super::fs::OPEN`]. Carried because a listing whose reader has to open every name to find
    /// out what it is turns one enumeration into N opens, and because completion wants to know.
    pub const IS_DIR: u8 = 1 << 0;

    /// Bytes one record takes: two for the header, plus the name.
    pub const fn record_len(name_len: usize) -> usize {
        2 + name_len
    }

    /// Write one record at the start of `out`, returning its length, or `None` if it does not fit or
    /// the name is too long to encode.
    pub fn encode(out: &mut [u8], name: &[u8], is_dir: bool) -> Option<usize> {
        let n = record_len(name.len());
        if name.len() > u8::MAX as usize || out.len() < n {
            return None;
        }
        out[0] = if is_dir { IS_DIR } else { 0 };
        out[1] = name.len() as u8;
        out[2..n].copy_from_slice(name);
        Some(n)
    }

    /// Walk the records in `buf` (exactly the `r0` bytes a [`fs::READDIR`] reply filled). Stops at
    /// the first record that runs off the end, which is what a truncated or corrupt reply looks like
    /// and which must not be read as a name.
    ///
    /// [`fs::READDIR`]: super::fs::READDIR
    pub fn iter(buf: &[u8]) -> Entries<'_> {
        Entries { buf, at: 0 }
    }

    /// The iterator [`iter`] returns: `(name, is_dir)` per entry.
    pub struct Entries<'a> {
        buf: &'a [u8],
        at: usize,
    }

    impl<'a> Iterator for Entries<'a> {
        type Item = (&'a [u8], bool);

        fn next(&mut self) -> Option<Self::Item> {
            let head = self.buf.get(self.at..self.at + 2)?;
            let (flags, len) = (head[0], head[1] as usize);
            let name = self.buf.get(self.at + 2..self.at + 2 + len)?;
            self.at += record_len(len);
            Some((name, flags & IS_DIR != 0))
        }
    }
}

/// **The per-file grant** (milestone 31 phase 2): what it means to hold one file rather than a
/// directory, and how the narrowing travels.
///
/// A directory capability lets its holder name anything in the bound directory. `run wc report.txt`
/// must hand over less than that: **one file, in one direction, and nothing else**. The narrowing is
/// done by an attenuator, `user/src/fwarden.rs`, which holds the directory capability, opens exactly
/// the granted name once at startup, and then serves the *same* [`fs`] protocol on its own endpoint
/// with three rules:
///
/// 1. **[`fs::OPEN`] answers only the granted name.** Any other name is `ENOENT`, because in this
///    scope there is no such name; nothing consulted a permission. The holder cannot enumerate, and
///    cannot discover what else exists.
/// 2. **[`fs::CREATE`] is `ENOTDIR`.** A file capability is not a directory, so "create a name in it"
///    is not a request that means anything, which is a better answer than a permission refusal.
/// 3. **[`fs::WRITE`] and [`fs::TRUNCATE`] are [`grant::EROFS`] without [`grant::WRITE`].** The
///    capability is read-only; there is no policy to consult and no way to widen it from inside.
///
/// The attenuator pattern is Mark Miller's caretaker, and putting it in a separate process is what
/// makes the claim checkable: the confined program holds an endpoint to the warden and nothing that
/// names the FS server, so it cannot route around the narrowing even in principle. Open-by-path
/// still exists only inside a server (DECISIONS §27); the warden just serves a server whose entire
/// namespace is one name.
pub mod grant {
    /// The granted directions, packed into the warden's spec word. Read alone is the common case
    /// (`run wc report.txt`); write implies read, since a writer that cannot read back is a shape
    /// nothing has asked for.
    pub const READ: u64 = 1 << 0;
    pub const WRITE: u64 = 1 << 1;

    /// The longest granted name, in bytes. The name rides in **two `START` argument words** rather
    /// than a frame, so a per-file grant costs no extra page and no extra mapping, and the warden
    /// needs nothing mapped before it runs. Sixteen bytes is short, and deliberately so: it is a
    /// demonstrator's limit, not a filesystem's, and lifting it means giving the warden a frame,
    /// which is a change to the wiring and not to this contract.
    pub const MAX_NAME: usize = 16;

    /// `EROFS`, the reply to a write through a read-only grant. Chosen over `EACCES` on purpose:
    /// `EACCES` is the Unix answer, "you were denied", which implies a policy that could have said
    /// yes. There is no policy here. The capability carries one direction, so the honest statement
    /// is about the thing itself, and `std::io::ErrorKind::ReadOnlyFilesystem` is what a caller sees.
    pub const EROFS: i32 = 30;

    /// `ENOTDIR`, the reply to a [`super::fs::CREATE`] through a file grant. "This is a file, not a
    /// directory" is a fact about what the holder has, not a refusal of what it asked.
    pub const ENOTDIR: i32 = 20;

    /// The handle the warden mints for the one file it serves. Fixed, because there is exactly one:
    /// a holder that guesses a different number gets `EBADF` from the same check every other handle
    /// goes through.
    pub const HANDLE: u64 = 0;

    /// Pack a granted name into the two argument words the warden is started with. Names shorter than
    /// [`MAX_NAME`] are zero-padded; longer ones are refused by the caller (see [`fits`]).
    pub const fn pack_name(name: &[u8]) -> (u64, u64) {
        let mut lo = [0u8; 8];
        let mut hi = [0u8; 8];
        let mut i = 0;
        while i < name.len() && i < 8 {
            lo[i] = name[i];
            i += 1;
        }
        while i < name.len() && i < MAX_NAME {
            hi[i - 8] = name[i];
            i += 1;
        }
        (u64::from_le_bytes(lo), u64::from_le_bytes(hi))
    }

    /// Unpack a granted name into `buf` (at least [`MAX_NAME`] bytes) and return its length.
    pub fn unpack_name(lo: u64, hi: u64, len: usize, buf: &mut [u8; MAX_NAME]) -> usize {
        buf[..8].copy_from_slice(&lo.to_le_bytes());
        buf[8..].copy_from_slice(&hi.to_le_bytes());
        len.min(MAX_NAME)
    }

    /// Whether a name can travel as a per-file grant at all.
    pub const fn fits(name: &[u8]) -> bool {
        !name.is_empty() && name.len() <= MAX_NAME
    }

    /// Pack the name length and the granted rights into the warden's third argument word.
    pub const fn spec(len: usize, rights: u64) -> u64 {
        ((len as u64) & 0xff) | (rights << 8)
    }

    /// The granted name's length from a spec word.
    pub const fn spec_len(w: u64) -> usize {
        (w & 0xff) as usize
    }

    /// The granted rights from a spec word.
    pub const fn spec_rights(w: u64) -> u64 {
        w >> 8
    }

    /// Whether a spec word grants writing.
    pub const fn writable(w: u64) -> bool {
        spec_rights(w) & WRITE != 0
    }
}

/// The phase-2 end-to-end test fixture, in one place so the three programs that touch it agree: the
/// host build (`cargo xtask`, which writes these into the RedoxFS image with the host tool), the
/// client (which reads and writes them through the FS server), and the kernel test (which asserts
/// the client's report). Not part of either wire protocol; a shared constant, like a test vector.
pub mod fixture {
    /// A file the image ships with; the client reads it back through a granted directory capability.
    pub const MOTD_NAME: &str = "motd";
    /// Its exact contents. Longer than eight bytes so the report's head word is a real prefix.
    pub const MOTD: &[u8] =
        b"redoxfs served this file to an EL0 client through a capability handle\n";

    /// A file the image ships with (with placeholder contents) so the client can open it and write.
    pub const SCRATCH_NAME: &str = "scratch";
    /// Placeholder contents the host tool writes; overwritten by the client's write test.
    pub const SCRATCH_INIT: &[u8] = b"(placeholder overwritten by the fs-server write test)";
    /// What the client writes to `scratch` and reads back; the host tool re-reads it after the run
    /// to prove the write reached the on-disk image and the filesystem is still consistent.
    pub const WRITE_PATTERN: &[u8] =
        b"CRKFS_WRITE_OK: this round-tripped through the RedoxFS image\n";

    /// The client's success sentinel, sent as the report's second word; the head of [`MOTD`] is the
    /// first. Any other value (or silence) fails the test.
    pub const SUCCESS: u64 = 0xF11E_600D;

    /// The FS server's readiness sentinel: sent once, after it has opened the RedoxFS image over blk
    /// IPC and before it serves clients. The test waits for it, so a hang in `open` (the blk path)
    /// is distinguishable from a hang in the serve/client path, and a booted-but-empty run is caught.
    pub const READY: u64 = 0xF5_0BEEF5;

    /// **The attacker's report: a bitmap of what got through**, not a pass/fail. Each bit says one
    /// specific thing happened, so the test asserts an *expected set* rather than "zero", and a
    /// failure names itself. That shape is what lets one attacker serve as its own negative control:
    /// run against a read-only grant every bit must be clear, and run against a read/write grant of
    /// the same shape the two write bits must be **set**, which is what proves the read-only
    /// refusals were a narrowed capability rather than a warden that refuses everything.
    pub mod escape {
        /// It opened a file the grant does not designate. Never allowed.
        pub const SECOND_FILE: u64 = 1 << 0;
        /// Its write to the granted file was accepted. Expected only with `grant::WRITE`.
        pub const WROTE: u64 = 1 << 1;
        /// Its truncate of the granted file was accepted. Expected only with `grant::WRITE`; a
        /// separate bit because a truncate carries no bytes, so a guard that only covered `WRITE`
        /// would leave a way to destroy a file just as thoroughly.
        pub const TRUNCATED: u64 = 1 << 2;
        /// It created a file through a file capability. Never allowed: a file is not a directory.
        pub const CREATED: u64 = 1 << 3;
        /// It reached a file with a handle it was never given. Never allowed.
        pub const FORGED_HANDLE: u64 = 1 << 4;
        /// The thing it *should* be able to do failed, so nothing above was actually proven. A
        /// capability that reaches nothing is trivially unescapable, and a test that only checked
        /// the refusals would pass against one.
        pub const GRANTED_READ_FAILED: u64 = 1 << 5;
    }

    /// The attacker's report leads with this so a silent client (a trapped one) cannot be mistaken
    /// for a clean verdict of zero.
    pub const VERDICT: u64 = 0xE5_CA9E00;

    /// **The subtree milestone 47's directory capability is measured against.**
    ///
    /// The shape matters more than the names. `sub` is what gets granted; `other` is its sibling and
    /// exists so "it cannot reach a sibling" is a claim about a directory that is really there and
    /// that the process one hop up the chain really can open, the same reason the per-file
    /// attacker's neighbour is a real file. `deeper` is inside the grant, so a second descent has
    /// somewhere to go and [`super::dir::DESCEND`] has something to withhold.
    ///
    /// ```text
    ///   /            motd  scratch  sub/  other/
    ///   /sub         inner  deeper/          <- the granted capability is here
    ///   /sub/deeper  leaf
    ///   /other       secret                  <- never reachable from the grant
    /// ```
    pub mod tree {
        /// The directory a dir grant designates.
        pub const SUB: &str = "sub";
        /// A file inside it. Pinned by the post-run host check, so nothing may damage it.
        pub const INNER: &str = "inner";
        pub const INNER_BODY: &[u8] = b"CRK47-INNER: a file inside the granted subtree\n";
        /// A directory inside the grant: what a second descent descends into.
        pub const DEEPER: &str = "deeper";
        /// A file inside that, reachable only with two descents.
        pub const LEAF: &str = "leaf";
        pub const LEAF_BODY: &[u8] = b"CRK47-LEAF: two descents below the granted directory\n";
        /// The granted directory's **sibling**. A capability to [`SUB`] must not reach it.
        pub const OTHER: &str = "other";
        /// A file in the sibling, pinned by the post-run host check.
        pub const SECRET: &str = "secret";
        pub const SECRET_BODY: &[u8] = b"CRK47-SECRET: in a sibling of the granted directory\n";

        /// The name the writable attacker creates inside its grant, with a run index appended so
        /// three runs sharing one image do not collide. It stays on the image afterwards, which is
        /// deliberate: the post-run host check asserts a name with this **prefix** is in [`SUB`] and
        /// **not** in the root, which is the escape it is looking for.
        pub const MADE: &str = "made-by-atk";
        /// What the attacker writes into [`MADE`]; read straight back, because "the server accepted
        /// my write" and "my write landed" are different claims.
        pub const MADE_BODY: &[u8] = b"CRK47: written through a directory capability\n";
        /// The directory the writable attacker makes inside its grant, to prove `MKDIR` mints a
        /// capability and that the capability it mints is not wider than the one that made it.
        pub const MADE_DIR: &str = "dir-by-atk";
        /// What the attacker renames [`MADE`] to, if its capability carries
        /// [`super::super::dir::REMOVE`]. The post-run host check looks for a name with this prefix
        /// in [`SUB`] **and** a [`MADE`] one that was not renamed, which is `REMOVE` being enforced
        /// witnessed from outside the guest: one capability moved a name and another could not.
        pub const MOVED: &str = "moved-by-atk";

        /// The file a navigating shell creates and **leaves** in its root, so that [`NAV_GONE`]'s
        /// absence afterwards is a fact about `unlink` rather than about a shell that created
        /// nothing. Run-indexed like the names above, for the same reason.
        pub const NAV_KEPT: &str = "nav-kept";
        /// The file a navigating shell creates, opens, writes, and then **unlinks while its own
        /// handle is still open**. Two claims ride on it: in-guest, the read through that handle
        /// still returns [`NAV_BODY`] (unlink removes a name, not an object); on the host, this name
        /// is not in the directory afterwards (the removal reached the platter).
        pub const NAV_GONE: &str = "nav-gone";
        /// What goes into [`NAV_GONE`] before it is unlinked, and what must still be readable
        /// through the open handle after.
        pub const NAV_BODY: &[u8] = b"CRK47-NAV: a name can go while the bytes stay\n";
        /// The directory a navigating shell makes with `mkdir` and leaves behind. It stays because
        /// the shell never empties it: [`super::super::fs::UNLINK`] refuses a directory and
        /// [`super::super::fs::RMDIR`] refuses a non-empty one, so a directory with a name in it
        /// takes two deliberate steps to remove and neither of them is one word.
        pub const NAV_DIR: &str = "nav-dir";
        /// The directory a navigating shell makes, puts [`NAV_INSIDE`] in, and then removes with
        /// `RMDIR` **after** taking that name back out. Two claims ride on it: in-guest, the first
        /// `RMDIR` is refused with `ENOTEMPTY` and the second succeeds; on the host, this name is
        /// not in the directory afterwards, so the removal reached the platter.
        pub const NAV_EMPTY: &str = "nav-empty";
        /// The one name inside [`NAV_EMPTY`], which is what makes the first `RMDIR` a refusal. It is
        /// unlinked before the second, so nothing of it survives either.
        pub const NAV_INSIDE: &str = "in";

        /// **The subtree the `rm` program is granted** (milestone 47's `rm -r`), a sibling of
        /// [`SUB`] so a capability to one is provably not a capability to the other.
        ///
        /// ```text
        ///   /rmtree                 rm-keep  rm-doomed/     <- the grant is here
        ///   /rmtree/rm-doomed       rm-one  rm-two  rm-nested/
        ///   /rmtree/rm-doomed/rm-nested   rm-leaf
        /// ```
        ///
        /// [`RM_KEEP`] is the control that makes the removal a statement about what `rm` was
        /// **told** rather than about what it could reach: it sits beside the doomed tree, inside
        /// the same grant, and the same capability could have taken it. It is still there
        /// afterwards because nothing named it.
        pub const RMTREE: &str = "rmtree";
        /// A file beside the doomed tree, inside the grant. Nothing names it, so nothing removes it.
        pub const RM_KEEP: &str = "rm-keep";
        pub const RM_KEEP_BODY: &[u8] = b"CRK47-RM: inside the grant, and never named\n";
        /// The directory `rm -r` is pointed at.
        pub const RM_DOOMED: &str = "rm-doomed";
        /// Two files directly inside it, so the walk has something to unlink at the top level.
        pub const RM_ONE: &str = "rm-one";
        pub const RM_TWO: &str = "rm-two";
        /// A directory inside it, so the walk has to recurse and `RMDIR` has to run bottom-up.
        pub const RM_NESTED: &str = "rm-nested";
        /// The file at the bottom: the last thing unlinked and the deepest the walk goes.
        pub const RM_LEAF: &str = "rm-leaf";
        pub const RM_BODY: &[u8] = b"CRK47-RM: a file `rm -r` is expected to take away\n";
        /// A name that is **not** on the image. `rm` of it is a diagnostic and a non-zero exit;
        /// `rm -f` of it is silence and a zero one, which is the whole of what `-f` means.
        pub const RM_MISSING: &str = "rm-nothing";

        /// **The names the image root must still carry after a run**, checked by the post-run host
        /// tool: this is the half of the assertion made from *outside* the confined program, and no
        /// in-guest verdict could have reported it. A capability granted on [`SUB`] can remove
        /// nothing above itself, so a name missing here escaped.
        ///
        /// **Containment, not equality**, and the reason is worth stating where the constant is:
        /// the root is shared with every other test in the boot (the `std::fs` test creates
        /// `made-by-std` in it), so an exact comparison would couple this milestone's gate to what
        /// unrelated tests happen to write. The upward-escape half is checked against [`MADE`] and
        /// [`MADE_DIR`] instead, which are names only the attacker writes.
        pub const ROOT_ENTRIES: [&str; 5] =
            [super::MOTD_NAME, OTHER, RMTREE, super::SCRATCH_NAME, SUB];
    }

    /// **The directory attacker's report** (milestone 47), a bitmap for the same reason the per-file
    /// attacker's is one: the test asserts an *expected set*, so the read-only run and the wide run
    /// are each other's control and a warden that refused everything fails one of them.
    pub mod dirscape {
        /// It opened a file that exists only in the granted directory's **parent**. Never allowed:
        /// this is "cannot reach its parent".
        pub const REACHED_PARENT: u64 = 1 << 0;
        /// It descended into, or opened anything in, the granted directory's **sibling**. Never
        /// allowed: this is "cannot reach a sibling".
        pub const REACHED_SIBLING: u64 = 1 << 1;
        /// `..` resolved to something. Never allowed, at any rights.
        pub const WALKED_UP: u64 = 1 << 2;
        /// **It asked for a right its capability did not carry, and got it.** Never allowed, and
        /// this is the bit that answers "can a child's rights exceed its parent's".
        pub const WIDENED: u64 = 1 << 3;
        /// It enumerated the granted directory. Expected only with [`super::super::dir::ENUMERATE`].
        pub const ENUMERATED: u64 = 1 << 4;
        /// It descended one level inside the grant. Expected only with
        /// [`super::super::dir::DESCEND`].
        pub const DESCENDED: u64 = 1 << 5;
        /// It created a name inside the grant. Expected only with [`super::super::dir::CREATE`].
        pub const CREATED: u64 = 1 << 6;
        /// It made a **directory** inside the grant and got a capability to it. Expected only with
        /// [`super::super::dir::CREATE`] and [`super::super::dir::DESCEND`] together.
        pub const MADE_A_DIR: u64 = 1 << 12;
        /// Its write to a file inside the grant was accepted **and read back**. Expected only with
        /// [`super::super::dir::WRITE`].
        pub const WROTE: u64 = 1 << 7;
        /// **It renamed a name inside its grant.** Expected only with
        /// [`super::super::dir::REMOVE`] and [`super::super::dir::CREATE`] together, which is what
        /// makes this the bit that gives the `REMOVE` rung something to fail.
        pub const RENAMED: u64 = 1 << 8;
        /// An enumeration it was allowed to make returned a name that is not in the granted
        /// directory. Never allowed: a listing is a rendering of authority, so a name from outside
        /// the grant appearing in it is an escape even though nothing was opened.
        pub const ENUMERATED_A_STRANGER: u64 = 1 << 9;
        /// It reached something with a handle it was never given. Never allowed.
        pub const FORGED_HANDLE: u64 = 1 << 10;
        /// **It opened the file inside its own grant**, which it is supposed to be able to do. The
        /// control bit: without it every refusal above is equally consistent with a warden that
        /// answers no to everything, or a grant that reaches nothing at all.
        pub const OPENED_ITS_OWN: u64 = 1 << 13;
        /// **The thing it should be able to do failed**, so nothing above was proven. A capability
        /// that reaches nothing is trivially unescapable.
        pub const GRANTED_ACCESS_FAILED: u64 = 1 << 11;
    }

    /// **What a navigating shell reports** (milestone 47's commands: `cd`, `pwd`, `ls`, `mkdir`,
    /// `rm`).
    ///
    /// A bitmap for the same reason the two attackers' are: the kernel test asserts an *exact* set,
    /// so a shell that could do nothing and a shell that could do everything both fail, and the two
    /// shells the headline test runs are each other's control. The shell running this script is told
    /// **nothing about which subtree it was rooted in** beyond a run index; it tries to name one file
    /// from each of two different roots and reports which it reached, so "two shells cannot name each
    /// other's files" is read off the *pair* of reports rather than claimed by either.
    pub mod navscape {
        /// `pwd` rendered `/` before anything moved. The root of your namespace is the root of the
        /// only namespace you have, which is the whole of "pwd is relative to your root".
        pub const PWD_IS_ROOT: u64 = 1 << 0;
        /// `cd ..` at the root was refused **and nothing moved**. This is `..` clamping: the shell
        /// descends from what it holds and has nothing above it to pop.
        pub const CLAMPED_AT_ROOT: u64 = 1 << 1;
        /// `cd ..` at the root moved it somewhere. Never allowed, at any rights.
        pub const WALKED_UP: u64 = 1 << 2;
        /// An absolute path was refused as unnameable, with no request made. The refusal is that the
        /// name **cannot be expressed** (there is no namespace to root it in), not that a permission
        /// was checked, which is why the `std` PAL answers `InvalidFilename` and not
        /// `PermissionDenied`.
        pub const ABSOLUTE_REFUSED: u64 = 1 << 3;
        /// It opened [`super::tree::INNER`], which exists only in [`super::tree::SUB`].
        pub const REACHED_INNER: u64 = 1 << 4;
        /// It opened [`super::tree::SECRET`], which exists only in [`super::tree::OTHER`].
        pub const REACHED_SECRET: u64 = 1 << 5;
        /// `ls` of its own root returned a listing.
        pub const LISTED: u64 = 1 << 6;
        /// That listing named [`super::tree::INNER`].
        pub const SAW_INNER: u64 = 1 << 7;
        /// That listing named [`super::tree::SECRET`]. A listing is a rendering of authority, so the
        /// pair of these two bits is the property stated in what the two shells can *see* as well as
        /// in what they can open.
        pub const SAW_SECRET: u64 = 1 << 8;
        /// `cd` into a child directory worked **and `pwd` then rendered the child's path**, which is
        /// the half that proves the shell's own position tracked the capability it moved to.
        pub const DESCENDED: u64 = 1 << 9;
        /// `cd ..` from there came back, and `pwd` rendered `/` again.
        pub const RETURNED: u64 = 1 << 10;
        /// It created both of its files with the shell's `create` path.
        pub const CREATED: u64 = 1 << 11;
        /// `mkdir` made a directory in its root.
        pub const MADE_DIR: u64 = 1 << 12;
        /// `rm` removed the name it had made.
        pub const UNLINKED: u64 = 1 << 13;
        /// **After that `rm`, a read through the handle it still held returned the bytes.** The
        /// unlink/revoke split, made a measurement: the name went and the object did not.
        pub const HOLDER_KEPT_READING: u64 = 1 << 14;
        /// And the name it removed no longer opens. Without this, the bit above is equally
        /// consistent with an `rm` that did nothing at all.
        pub const NAME_GONE_AFTER_UNLINK: u64 = 1 << 15;
        /// `UNLINK` of a **directory** was refused. It unlinks the name of a file; a verb that
        /// removed whatever it found is how one word takes a subtree away.
        pub const UNLINK_REFUSED_A_DIRECTORY: u64 = 1 << 16;
        /// **Something that should have worked did not**, so the refusals above prove nothing. The
        /// control bit, in the shape both attackers already use.
        pub const NAVIGATION_FAILED: u64 = 1 << 17;
        /// `RMDIR` of a directory with a name still in it was refused (`ENOTEMPTY`). The other half
        /// of "no single call on this contract takes a subtree away": `UNLINK` will not remove a
        /// directory at all, and `RMDIR` will not remove one that holds anything.
        pub const RMDIR_REFUSED_NON_EMPTY: u64 = 1 << 18;
        /// And once the name inside it was taken out, `RMDIR` removed it. Without this, the bit
        /// above is equally true of a `RMDIR` that refuses everything.
        pub const RMDIR_REMOVED_EMPTY: u64 = 1 << 19;
    }

    /// **What the `rm` program reports, and how a diagnostic is told from the verdict**
    /// (milestone 47's `rm -r`, `user/src/rm.rs`).
    ///
    /// `rm` is a **program**, not a shell builtin, so it holds a report endpoint and nothing that
    /// names a terminal. Two kinds of message go out on it, and they are told apart by the first
    /// word, which is what makes "silent on success" checkable from the far end:
    ///
    /// - **A line of text**, framed exactly as every std program's stdout is (`w0` = the byte count,
    ///   at most 16, and the bytes little-endian in `w1`|`w2`). Diagnostics always go out this way;
    ///   `-v` adds one line per name removed.
    /// - **The verdict**, once, last: `w0` = [`super::VERDICT`], `w1` = the exit status, `w2` = how
    ///   many names were removed.
    ///
    /// A byte count is at most 16 and [`super::VERDICT`] is far larger, so the two never collide. A
    /// receiver that takes the verdict as its **first** message therefore knows the run printed
    /// nothing, which is `rm(1)`'s default behaviour asserted rather than assumed: a `SEND` blocks
    /// until somebody receives it, so a line that was emitted cannot be missed by looking later.
    pub mod rm {
        /// The exit status of a run in which everything named was removed. `rm(1)`: "exits 0 if all
        /// of the named files or file hierarchies were removed".
        pub const OK: u64 = 0;

        /// A non-zero exit status **is the errno of the first failure**, rather than a generic 1.
        ///
        /// `rm(1)` promises only "a value >0", so this is a choice, and it is the one that costs
        /// nothing and says more: a test that expected `EROFS` and got `ENOENT` has learned that the
        /// walk stopped somewhere else than it thought. The [`super::super::dir::explain`] sentence
        /// for that number is what the program prints as its diagnostic, so the status word and the
        /// text a human reads come from the same decision.
        pub const fn status(errno: i32) -> u64 {
            errno as u64
        }
    }

    /// **The on-device crash test's vocabulary** (milestone 37, DECISIONS §34 condition 1).
    ///
    /// The host sweep in `fs-server/tests/crash_consistency.rs` proves the property exhaustively
    /// against a reconstructed platter. This is the other half: one crash driven all the way through
    /// the real stack, on its own disk, so the recovery is a real FS-server process mounting a real
    /// image that a real virtio write left half finished.
    ///
    /// **Two payloads of the same length, deliberately.** The contract's `WRITE` does not truncate
    /// (DECISIONS §27, four times corrected), so a shorter second payload would leave the first
    /// one's tail behind and "the file is exactly A or exactly B" would stop being a question with
    /// an answer. Equal lengths make the recovered file unambiguous, which is the whole assertion.
    pub mod crash {
        /// The file the crash driver writes. Its own name on its own disk, so nothing this test does
        /// can be confused with the shared fixture disk's `scratch`.
        pub const NAME: &str = "cut";
        /// What the image ships with. The driver's first write is **acknowledged** before the crash,
        /// so recovering this would mean an acknowledged write vanished, and the test says so.
        pub const INITIAL: &[u8] =
            b"CRK37-INITIAL: the value the host tool wrote before this boot ran";
        /// Payload A: written and acknowledged, and then the server is killed. It must survive.
        pub const A: &[u8] = b"CRK37-PAYLOAD-A: acknowledged before the kill, must be whole after";
        /// Payload B: the write the FS server dies in the middle of. Whole or absent, never partial.
        pub const B: &[u8] = b"CRK37-PAYLOAD-B: the write the server was killed in the middle of!";

        /// The FS server's "I am about to die" word, sent on its readiness endpoint immediately
        /// before it traps. It is what lets the kernel test start the recovery mount at a defined
        /// moment rather than guessing, and it is also the evidence that the *injector* killed the
        /// server rather than something else having gone wrong.
        pub const CUT: u64 = 0x0C07_DEAD;

        /// What the verifier found in [`NAME`] after the recovery mount, sent as its report's second
        /// word. Anything else, including silence, fails.
        pub const SAW_A: u64 = 0x0037_00A0;
        pub const SAW_B: u64 = 0x0037_00B0;
        /// The file held something that was never one of the two payloads: a partial write, the
        /// pre-boot contents, or a length nobody asked for. This is the failure the test exists for.
        pub const SAW_NEITHER: u64 = 0x0037_00FF;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The crash fixture's two payloads must be the same length**, and the millisecond test that
    /// says so is not pedantry: `WRITE` does not truncate, so the moment B is shorter than A the
    /// recovered file is A's tail with B's head on it, and "exactly A or exactly B" becomes a
    /// question with no answer. That is DECISIONS §27's four-times-corrected failure, and it cost a
    /// day when it was discovered from the far end instead of pinned here.
    #[test]
    fn the_crash_payloads_replace_each_other_completely() {
        use fixture::crash;
        assert_eq!(
            crash::A.len(),
            crash::B.len(),
            "a shorter payload leaves the other's tail behind, so the verifier could see neither",
        );
        assert!(
            crash::INITIAL.len() <= crash::A.len(),
            "the image's initial contents must not outlive the first write either",
        );
        assert_ne!(crash::A, crash::B);
        assert_ne!(crash::A, crash::INITIAL);
    }

    #[test]
    fn block_request_roundtrips() {
        let w0 = req(blk::READ);
        assert_eq!(op(w0), blk::READ);
        // The block index rides in the second word untouched, so any u64 survives.
        for block in [0u64, 1, 42, u64::MAX] {
            let (rw0, rw1) = (req(blk::WRITE), block);
            assert_eq!(op(rw0), blk::WRITE);
            assert_eq!(rw1, block);
        }
    }

    #[test]
    fn file_request_packs_and_unpacks() {
        // Every field survives the pack, at its extremes, without bleeding into another.
        for &(o, h, l) in &[
            (fs::OPEN, 0u64, 7u64),
            (fs::READ, fs::MAX_HANDLE, fs::MAX_LEN),
            (fs::WRITE, 1, 4096),
            (fs::CLOSE, 65535, 0),
            (fs::FSTAT, 3, 0),
            (fs::CREATE, 0, 11),
            (fs::TRUNCATE, 42, 0),
        ] {
            let w = fs::req(o, h, l);
            assert_eq!(op(w), o, "opcode");
            assert_eq!(fs::req_handle(w), h, "handle");
            assert_eq!(fs::req_len(w) as u64, l, "len");
        }
    }

    #[test]
    fn a_handle_never_collides_with_the_opcode_or_length() {
        // A big handle must not spill into the opcode field, and a max length must not spill into
        // the handle: the READ path packs all three and reads them back independently.
        let w = fs::req(fs::READ, fs::MAX_HANDLE, fs::MAX_LEN);
        assert_eq!(op(w), fs::READ);
        assert_eq!(fs::req_handle(w), fs::MAX_HANDLE);
        assert_eq!(fs::req_len(w) as u64, fs::MAX_LEN);
    }

    #[test]
    fn every_opcode_is_distinct_and_fits_its_field() {
        // Two verbs sharing a number is a wire bug the packing tests cannot see: each would pack and
        // unpack perfectly and mean the wrong thing. Cheap to assert, so assert it.
        let ops = [
            ("OPEN", fs::OPEN),
            ("READ", fs::READ),
            ("WRITE", fs::WRITE),
            ("CLOSE", fs::CLOSE),
            ("FSTAT", fs::FSTAT),
            ("CREATE", fs::CREATE),
            ("TRUNCATE", fs::TRUNCATE),
        ];
        for (i, (na, a)) in ops.iter().enumerate() {
            assert!(*a <= 0xff, "{na} does not fit the 8-bit opcode field");
            assert_ne!(
                *a, 0,
                "0 is not a verb: a zeroed word must not decode as one"
            );
            for (nb, b) in &ops[i + 1..] {
                assert_ne!(a, b, "{na} and {nb} share an opcode");
            }
        }
        // The blk protocol is a separate namespace on a separate endpoint, so its numbers may and do
        // overlap. Asserted so nobody "fixes" the overlap and renumbers a live wire.
        assert_eq!(
            blk::READ,
            fs::OPEN,
            "the two protocols are deliberately independent"
        );
    }

    /// **A refusal explains itself as a fact, never as a policy.** The sentences are part of §47's
    /// design (the errno decides what the holder learns), so they are pinned here rather than left
    /// to drift in whichever program prints them. The negative half is the load-bearing one: not one
    /// of these lines may read like Unix's "permission denied", because there is no policy in this
    /// contract that could have said yes.
    #[test]
    fn every_refusal_explains_itself_without_implying_a_policy() {
        for errno in [
            2,
            dir::EPERM,
            dir::EROFS,
            dir::EISDIR,
            dir::ENOTDIR,
            17,
            dir::ENOTEMPTY,
            9,
            24,
            dir::EINVAL,
            // An errno this contract does not name still has to render as something a human can
            // read, rather than as an empty line or a panic.
            5,
        ] {
            let s = dir::explain(errno);
            assert!(!s.is_empty(), "errno {errno} renders as nothing");
            assert!(
                !s.contains("denied") && !s.contains("permission"),
                "errno {errno} explains itself as a policy: {s:?}",
            );
        }
        // The two rungs whose whole design is that they answer different words must not collapse
        // into one sentence, or the distinction is invisible where a human meets it.
        assert_ne!(dir::explain(dir::EROFS), dir::explain(dir::EPERM));
        assert_ne!(dir::explain(2), dir::explain(dir::EPERM));
    }

    #[test]
    fn errors_are_negated_errnos_and_invert() {
        // The boundary convention: -errno on the wire, invertible, and successes read as no error.
        assert_eq!(reply_err(2), -2); // ENOENT
        assert_eq!(reply_err(5), -5); // EIO
        assert_eq!(reply_errno(reply_err(9)), Some(9)); // EBADF round trips
        assert_eq!(reply_errno(0), None);
        assert_eq!(reply_errno(4096), None); // a byte count is a success, not an error
    }

    #[test]
    fn a_granted_name_survives_the_two_argument_words() {
        // The name rides in START arguments rather than a frame, so a per-file grant costs no page.
        // Every length up to the limit has to come back exactly, including the 8-byte boundary where
        // it splits across the two words.
        for name in [
            &b"a"[..],
            b"motd",
            b"scratch",
            b"12345678",
            b"123456789",
            b"sixteen-bytes!!!",
        ] {
            assert!(grant::fits(name), "{name:?} should fit");
            let (lo, hi) = grant::pack_name(name);
            let mut buf = [0u8; grant::MAX_NAME];
            let n = grant::unpack_name(lo, hi, name.len(), &mut buf);
            assert_eq!(&buf[..n], name, "{name:?} did not survive the round trip");
        }
        assert!(!grant::fits(b""), "an empty name designates nothing");
        assert!(
            !grant::fits(b"seventeen-bytes!!"),
            "a name past the limit must be refused where it is packed, not truncated silently",
        );
    }

    #[test]
    fn a_grant_spec_carries_its_length_and_its_direction_apart() {
        // A read grant and a write grant of the same name differ only here, so the two fields must
        // not bleed: a 16-byte name must not look like a write bit, and write must not lengthen it.
        let ro = grant::spec(16, grant::READ);
        let rw = grant::spec(16, grant::READ | grant::WRITE);
        assert_eq!(grant::spec_len(ro), 16);
        assert_eq!(grant::spec_len(rw), 16);
        assert!(!grant::writable(ro));
        assert!(grant::writable(rw));
        // And a zero-rights spec grants nothing, rather than defaulting to something.
        assert!(!grant::writable(grant::spec(4, 0)));
    }

    #[test]
    fn the_escape_bits_are_distinct() {
        // The attacker reports a bitmap and the test asserts an expected set, so two outcomes
        // sharing a bit would hide one of them and make a wrong verdict read as a right one.
        use fixture::escape::*;
        let bits = [
            SECOND_FILE,
            WROTE,
            TRUNCATED,
            CREATED,
            FORGED_HANDLE,
            GRANTED_READ_FAILED,
        ];
        let mut seen = 0u64;
        for b in bits {
            assert_ne!(b, 0, "zero is the pass; it cannot also be a breach");
            assert_eq!(seen & b, 0, "two escapes share a bit");
            seen |= b;
        }
    }

    #[test]
    fn a_filesystem_block_is_eight_sectors() {
        assert_eq!(blk::BLOCK_SIZE, PAGE);
        assert_eq!(blk::SECTORS_PER_BLOCK, 8);
    }

    // --- The directory capability (milestone 47) ---

    /// **A child never carries a right its parent lacked**, over every single right and every
    /// request, at one level and at two. The Kani harness proves this for every mask; this is the
    /// millisecond version that runs in `cargo test`, and it exists because the proofs are behind a
    /// cfg an ordinary build never sets.
    #[test]
    fn attenuation_is_monotonic_at_every_depth() {
        use dir::*;
        let every = [ENUMERATE, READ, WRITE, CREATE, REMOVE, DESCEND];
        for &held in &every {
            let parent = Rights::root(held);
            // Asking for everything gets exactly what the parent had, never more.
            assert_eq!(parent.attenuate(ALL), parent);
            for &wanted in &every {
                let child = parent.attenuate(wanted);
                for &probe in &every {
                    assert!(
                        !child.allows(probe) || parent.allows(probe),
                        "a child carried {probe:#x} that its parent ({held:#x}) did not",
                    );
                }
                // And a grandchild that asks for everything is still bounded by the root.
                let grand = child.attenuate(ALL);
                for &probe in &every {
                    assert!(!grand.allows(probe) || parent.allows(probe));
                }
            }
        }
    }

    /// `allows` is "all of", not "any of". An operation needing two rights that a capability with
    /// one of them could perform is a hole, and the two spellings differ only in an `==`.
    #[test]
    fn allows_means_all_of_them_not_any_of_them() {
        use dir::*;
        let r = Rights::root(CREATE);
        assert!(r.allows(CREATE));
        assert!(!r.allows(CREATE | REMOVE), "a rename needs both");
        assert!(Rights::root(CREATE | REMOVE).allows(CREATE | REMOVE));
        // The empty request is vacuously allowed, which is what makes a verb needing no right work.
        assert!(Rights::root(0).allows(0));
        assert!(Rights::root(0).denies_all(ALL));
        assert!(!Rights::root(READ).denies_all(READ | WRITE));
    }

    /// A root cannot be built with bits this contract has not defined. Otherwise a caller could set
    /// bit 60 today and have some later version of `dir` give it a meaning it was never granted.
    #[test]
    fn undefined_rights_bits_cannot_be_smuggled_into_a_root() {
        assert_eq!(dir::Rights::root(u64::MAX).bits(), dir::ALL);
        assert_eq!(dir::ALL.count_ones(), 6, "six rungs on the ladder");
    }

    /// The three new verbs must not collide with the seven that were already on the wire, and
    /// `ROOT` must stay 0 because every client that ever sent an `OPEN` sent 0 in that field.
    #[test]
    fn the_directory_verbs_are_distinct_from_every_other_one() {
        let ops = [
            ("OPEN", fs::OPEN),
            ("READ", fs::READ),
            ("WRITE", fs::WRITE),
            ("CLOSE", fs::CLOSE),
            ("FSTAT", fs::FSTAT),
            ("CREATE", fs::CREATE),
            ("TRUNCATE", fs::TRUNCATE),
            ("OPENDIR", fs::OPENDIR),
            ("READDIR", fs::READDIR),
            ("MKDIR", fs::MKDIR),
            ("RENAME", fs::RENAME),
            ("UNLINK", fs::UNLINK),
            ("RMDIR", fs::RMDIR),
        ];
        for (i, (na, a)) in ops.iter().enumerate() {
            assert!(*a <= 0xff, "{na} does not fit the 8-bit opcode field");
            assert_ne!(*a, 0, "0 is not a verb");
            for (nb, b) in &ops[i + 1..] {
                assert_ne!(a, b, "{na} and {nb} share an opcode");
            }
        }
        assert_eq!(fs::ROOT, 0, "every existing client sends 0 and means this");
    }

    /// **A rename names two directories, so its second word is a packed pair rather than a scalar**,
    /// and the two halves must not bleed into each other or a rename lands somewhere nobody asked
    /// for. The destination fields use the same bit positions as the request word's, which is the
    /// point of reusing them, so this also pins that they stay in step.
    #[test]
    fn a_rename_carries_two_directories_and_two_lengths_without_them_bleeding() {
        for &(sh, sl, dh, dl) in &[
            (fs::ROOT, 3u64, fs::ROOT, 4u64),
            (1, 16, 2, 1),
            (fs::MAX_HANDLE, fs::MAX_LEN, fs::MAX_HANDLE, fs::MAX_LEN),
            (fs::MAX_HANDLE, 1, 0, fs::MAX_LEN),
        ] {
            let w0 = fs::req(fs::RENAME, sh, sl);
            let w1 = fs::rename_dst(dh, dl);
            assert_eq!(op(w0), fs::RENAME);
            assert_eq!(fs::req_handle(w0), sh, "source handle");
            assert_eq!(fs::req_len(w0) as u64, sl, "source name length");
            assert_eq!(fs::dst_handle(w1), dh, "destination handle");
            assert_eq!(fs::dst_len(w1) as u64, dl, "destination name length");
        }
        // A destination in the same directory is the ordinary rename, and it must not be confusable
        // with "no destination given": handle 0 IS a directory (the bound one), so a zero second
        // word means "rename inside your root", which is exactly what a plain `mv a b` wants.
        assert_eq!(fs::dst_handle(0), fs::ROOT);
    }

    /// A listing round-trips: every name comes back byte for byte, with its kind, in order.
    #[test]
    fn a_directory_listing_round_trips_through_the_page() {
        let entries: [(&[u8], bool); 4] = [
            (b"a", false),
            (b"deeper", true),
            (b"inner", false),
            (b"a-name-that-is-quite-a-lot-longer-than-the-others", false),
        ];
        let mut page = [0u8; 128];
        let mut at = 0;
        for (name, is_dir) in entries {
            at += dirent::encode(&mut page[at..], name, is_dir).expect("encode");
        }
        let mut seen = 0;
        for (i, (name, is_dir)) in dirent::iter(&page[..at]).enumerate() {
            assert_eq!(name, entries[i].0, "entry {i}'s name");
            assert_eq!(is_dir, entries[i].1, "entry {i}'s kind");
            seen += 1;
        }
        assert_eq!(seen, entries.len());
    }

    /// **A record is never split**, and a truncated buffer yields the entries that are whole rather
    /// than a fragment of a name. A reader that returned half a name would let a client act on a
    /// name that was never in the directory.
    #[test]
    fn a_listing_that_does_not_fit_encodes_nothing_and_reads_back_nothing_partial() {
        let mut tiny = [0u8; 4];
        assert_eq!(dirent::encode(&mut tiny, b"ab", false), Some(4));
        assert_eq!(
            dirent::encode(&mut tiny, b"abc", false),
            None,
            "a record that does not fit must be refused, not clipped",
        );

        // A buffer cut in the middle of a name yields only the whole records before it.
        let mut page = [0u8; 32];
        let n = dirent::encode(&mut page, b"one", false).unwrap()
            + dirent::encode(&mut page[5..], b"two", false).unwrap();
        for cut in 0..n {
            let seen = dirent::iter(&page[..cut]).count();
            assert!(seen <= 1, "a cut at {cut} produced a torn entry");
        }
        assert_eq!(dirent::iter(&page[..n]).count(), 2);
    }

    /// The escape bits must not overlap, for the reason the per-file ones must not: the test asserts
    /// an expected set, so two outcomes on one bit make a wrong verdict read as a right one.
    #[test]
    fn the_directory_escape_bits_are_distinct() {
        use fixture::dirscape::*;
        let bits = [
            REACHED_PARENT,
            REACHED_SIBLING,
            WALKED_UP,
            WIDENED,
            ENUMERATED,
            DESCENDED,
            CREATED,
            WROTE,
            RENAMED,
            MADE_A_DIR,
            ENUMERATED_A_STRANGER,
            FORGED_HANDLE,
            OPENED_ITS_OWN,
            GRANTED_ACCESS_FAILED,
        ];
        let mut seen = 0u64;
        for b in bits {
            assert_ne!(b, 0, "zero is the pass; it cannot also be a breach");
            assert_eq!(seen & b, 0, "two escapes share a bit");
            seen |= b;
        }
    }

    /// The navigating shell's bits, for the reason above, and one more that is specific to them:
    /// the headline test reads a property off **two** reports, so a bit that meant two things would
    /// let one shell's success stand in for the other's failure.
    #[test]
    fn the_navigation_bits_are_distinct() {
        use fixture::navscape::*;
        let bits = [
            PWD_IS_ROOT,
            CLAMPED_AT_ROOT,
            WALKED_UP,
            ABSOLUTE_REFUSED,
            REACHED_INNER,
            REACHED_SECRET,
            LISTED,
            SAW_INNER,
            SAW_SECRET,
            DESCENDED,
            RETURNED,
            CREATED,
            MADE_DIR,
            UNLINKED,
            HOLDER_KEPT_READING,
            NAME_GONE_AFTER_UNLINK,
            UNLINK_REFUSED_A_DIRECTORY,
            NAVIGATION_FAILED,
            RMDIR_REFUSED_NON_EMPTY,
            RMDIR_REMOVED_EMPTY,
        ];
        let mut seen = 0u64;
        for b in bits {
            assert_ne!(
                b, 0,
                "zero is the empty report; it cannot also be an outcome"
            );
            assert_eq!(seen & b, 0, "two navigation outcomes share a bit");
            seen |= b;
        }
    }

    /// The fixture's names must all fit a grant's two argument words, and must be distinct: a
    /// sibling that happened to be spelled like the granted directory would make the confinement
    /// test pass for the wrong reason.
    #[test]
    fn the_subtree_fixture_is_grantable_and_unambiguous() {
        use fixture::tree::*;
        for name in [
            SUB,
            INNER,
            DEEPER,
            LEAF,
            OTHER,
            SECRET,
            MADE,
            MADE_DIR,
            MOVED,
            NAV_KEPT,
            NAV_GONE,
            NAV_DIR,
            NAV_EMPTY,
            NAV_INSIDE,
            // The `rm -r` subtree. Its names ride in a grant too: the program is *started* with the
            // one name it is to remove, packed the way every other grant is.
            RMTREE,
            RM_KEEP,
            RM_DOOMED,
            RM_ONE,
            RM_TWO,
            RM_NESTED,
            RM_LEAF,
            RM_MISSING,
        ] {
            assert!(
                grant::fits(name.as_bytes()),
                "{name} cannot ride in a grant"
            );
        }
        // The post-run host check identifies the attacker's creations by PREFIX, because a run
        // index is appended to each. A fixture name that started with one of those prefixes would
        // be read as an escape, or would hide one.
        let probes = [MADE, MADE_DIR, MOVED, NAV_KEPT, NAV_GONE, NAV_DIR, NAV_EMPTY];
        for fixture in ROOT_ENTRIES
            .iter()
            .chain(&[INNER, DEEPER, LEAF, SECRET, RM_KEEP, RM_DOOMED])
        {
            for probe in probes {
                assert!(
                    !fixture.starts_with(probe),
                    "{fixture} collides with the prefix `{probe}` the confinement check searches for",
                );
            }
        }
        // And the three probes must not shadow each other, because the check counts them apart: a
        // rename that looked like a creation would make "REMOVE was enforced" unfalsifiable.
        for (i, a) in probes.iter().enumerate() {
            for b in &probes[i + 1..] {
                assert!(
                    !a.starts_with(b) && !b.starts_with(a),
                    "`{a}` and `{b}` cannot be told apart by prefix",
                );
            }
        }
        assert_ne!(SUB, OTHER, "the sibling must be a different directory");
        // The `rm -r` fixture's own shape. `RM_MISSING` is the load-bearing one: the whole of `-f`
        // is that a name which is not there is not an error, so a fixture that accidentally shipped
        // it would make the `-f` run and the plain run indistinguishable.
        for staged in [RM_KEEP, RM_DOOMED, RM_ONE, RM_TWO, RM_NESTED, RM_LEAF] {
            assert_ne!(
                staged, RM_MISSING,
                "the name `rm -f` is pointed at must not be one the fixture stages",
            );
        }
        assert_ne!(RMTREE, SUB, "the rm grant must not be the shells' subtree");
    }

    /// **A verdict word cannot be mistaken for a line of text**, which is the one thing the `rm`
    /// program's report channel rests on: its receiver reads "the first message is the verdict" as
    /// "the run printed nothing", and `rm(1)`'s default is to print nothing on success.
    #[test]
    fn a_text_frame_and_a_verdict_are_told_apart_by_the_first_word() {
        // A frame's first word is a byte count into the two payload words, so 16 is its ceiling.
        assert!(
            fixture::VERDICT > 16,
            "a verdict word that could be a byte count would make a diagnostic read as a verdict",
        );
        // And a non-zero status is the errno, so the two ends agree about what failed rather than
        // only that something did.
        assert_eq!(fixture::rm::status(dir::EROFS), dir::EROFS as u64);
        assert_ne!(fixture::rm::status(dir::EROFS), fixture::rm::OK);
        assert_eq!(fixture::rm::OK, 0, "success is zero, as every shell expects");
    }
}
