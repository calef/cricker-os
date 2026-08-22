//! **The milestone-32 filesystem wire protocols** (notes/fs-server.md).
//!
//! Two userspace protocols, one crate, so the four programs that speak them, the block server, the
//! FS server, the client, and the kernel-side tests, share one definition and cannot drift. The
//! kernel routes these words the way it routes any IPC (§10, §12): it never reads an opcode. Adding
//! one is a change to this crate and the note, not to the syscall surface. This is the same split
//! `line_editor::proto` makes for the terminal contract.
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
//! words. They travel in a region the two parties share, one per channel. For blk IPC that region
//! is [`blk::TRANSFER_BLOCKS`] contiguous pages ([`blk::TRANSFER_MAX`] bytes) and holds up to that
//! many filesystem blocks; for file IPC it is [`fs::TRANSFER_PAGES`] contiguous pages
//! ([`fs::TRANSFER_MAX`] bytes) and holds a name (on open) or file data (on read/write).
//!
//! **Both channels were one page until milestone 138**: the file channel through step 3, the blk
//! channel through step 4. Neither change touched the packing; see [`fs::TRANSFER_PAGES`] and
//! [`blk::TRANSFER_BLOCKS`] for what changed and, for the file channel, the one thing it does not
//! check.
//!
//! ## The error boundary
//!
//! Every reply's first word is an [`i64`]: **non-negative is a result, negative is an error**, and
//! the error value is exactly the negated errno ([`reply_err`]). RedoxFS's error type is
//! `syscall::error::Error { errno: i32 }` (the Linux numbers), so the FS server maps it to the wire
//! with `-(err.errno as i64)` at the serve loop and nowhere else, which is the "map the error type
//! once, at the server boundary" rule the roadmap sets. The client reads it back with [`reply_errno`].
//!
//! # Examples
//!
//! The error boundary is one word wide and has no flag bit, which is what lets a caller read a
//! length and an errno out of the same register without a mode to get wrong:
//!
//! ```
//! use fs_proto::{reply_err, reply_errno};
//!
//! // A READ that moved 4096 bytes, and a READ at end of file. Both are results.
//! assert_eq!(reply_errno(4096), None);
//! assert_eq!(reply_errno(0), None);
//!
//! // ENOENT, mapped once at the server's serve loop and nowhere else.
//! let wire = reply_err(2);
//! assert_eq!(wire, -2);
//! assert_eq!(reply_errno(wire), Some(2));
//! ```
//!
//! A request word packs the opcode, the handle and the length, so control needs no shared page at
//! all: the page is for the bytes.
//!
//! ```
//! use fs_proto::{fs, op};
//!
//! // `read(handle 3, 1024)`.
//! let w0 = fs::req(fs::READ, 3, 1024);
//! assert_eq!(op(w0), fs::READ); // the opcode field is the same one in both protocols
//! assert_eq!(fs::req_handle(w0), 3);
//! assert_eq!(fs::req_len(w0), 1024);
//!
//! // An OPEN sends handle ROOT and the *name's* length; the name itself is in the page.
//! let w0 = fs::req(fs::OPEN, fs::ROOT, b"report.txt".len() as u64);
//! assert_eq!(fs::req_handle(w0), fs::ROOT);
//! assert_eq!(fs::req_len(w0), 10);
//! ```
//!
//! A bulk request is the same word with a bigger length, which is the whole of milestone 138 step
//! 3: sixteen pages in one round trip instead of sixteen round trips, and no new opcode to learn.
//!
//! ```
//! use fs_proto::{PAGE, fs};
//!
//! // The largest read this contract permits, in one request.
//! let w0 = fs::req(fs::READ, 3, fs::TRANSFER_MAX as u64);
//! assert_eq!(fs::req_len(w0), fs::TRANSFER_MAX);
//! assert_eq!(fs::TRANSFER_MAX, fs::TRANSFER_PAGES * PAGE);
//!
//! // A client that shares only the first page keeps working untouched: it asks for one page,
//! // and one page is what the region behind its request has always been.
//! let w0 = fs::req(fs::READ, 3, PAGE as u64);
//! assert_eq!(fs::req_len(w0), PAGE);
//! ```
//!
//! [`dir::Rights`] is where the interesting claim is, and it is a claim about what is **absent**:
//! attenuation is the only way to make a non-root value, and it is a bitwise AND, so no chain of
//! calls can produce a child carrying more than its parent.
//!
//! ```
//! use fs_proto::dir::{self, Rights};
//!
//! // What a shell holds over the user's home directory.
//! let home = Rights::root(dir::ALL);
//!
//! // What it hands a log writer: somewhere to write, and no power to delete what is there.
//! let logs = home.attenuate(dir::READ | dir::WRITE | dir::CREATE);
//! assert!(logs.allows(dir::WRITE));
//! assert!(logs.denies_all(dir::REMOVE));
//!
//! // The log writer passing its authority on cannot widen it back, however it asks.
//! let narrower = logs.attenuate(dir::ALL);
//! assert_eq!(narrower.bits(), logs.bits());
//! assert!(narrower.denies_all(dir::REMOVE));
//! ```
//!
//! And [`grant`] is the shape that makes `run wc report.txt` mean what it says: `wc` is started
//! holding one file, read-only, with the name in two argument words rather than a frame, so a
//! per-file grant costs no extra mapping and the caretaker needs nothing mapped before it runs.
//!
//! ```
//! use fs_proto::grant;
//!
//! assert!(grant::fits(b"report.txt"));
//! let (lo, hi) = grant::pack_name(b"report.txt");
//! let spec = grant::spec(b"report.txt".len(), grant::READ);
//!
//! // What the caretaker unpacks from its three argument words.
//! let mut buf = [0u8; grant::MAX_NAME];
//! let n = grant::unpack_name(lo, hi, grant::spec_len(spec), &mut buf);
//! assert_eq!(&buf[..n], b"report.txt");
//!
//! // A read-only grant is not a policy that could have said yes, so a write gets EROFS.
//! assert!(!grant::writable(spec));
//! assert_eq!(grant::EROFS, 30);
//!
//! // A name too long to travel in two words is refused by the caller, not truncated on the wire.
//! assert!(!grant::fits(b"a-name-longer-than-sixteen-bytes"));
//! ```
//!
//! Name: recorded (milestone 46, and notes/naming.md's crate section). The wire contract was
//! spelled four ways (`fs_proto`, `gfx_proto`, `netproto`, `line_editor::proto`) for one concept;
//! `*_proto` won on 2026-07-30 under DECISIONS §39, and `script/lint` has checked it since. That
//! rule plus the service the stem names produces this name, which is the whole of what `recorded`
//! claims: calef ruled on the rule, and never on this crate.
//! `fs` is this tree's own word for the service, from `fs_server` through `fs_test_client`.

#![no_std]

/// One page, in bytes. Also one RedoxFS block (`redoxfs::BLOCK_SIZE`), so a block move is a page
/// move and one frame of the direct map holds exactly one block.
///
/// **This is the unit both channels are built from, and neither channel's whole size any more**:
/// a `blk` request moves up to [`blk::TRANSFER_BLOCKS`] of these (milestone 138 step 4) and a file
/// request moves up to [`fs::TRANSFER_PAGES`] (step 3). A file read or write larger than
/// [`fs::TRANSFER_MAX`], or a `Disk` call spanning more than [`blk::TRANSFER_MAX`], is still
/// chunked by the caller.
pub const PAGE: usize = 4096;

/// Where a request packs its opcode: bits 63:56 of the first `CALL` word, the same position
/// `line_editor::proto` uses, so the two contracts read alike.
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
/// the size. The unit of transfer is one filesystem block ([`PAGE`] bytes) as [`blk::SIZE`] and
/// [`blk::FLUSH`] see it; [`blk::READ`] and [`blk::WRITE`] may carry up to [`blk::TRANSFER_BLOCKS`]
/// of them in one request (milestone 138 step 4). The block server splits whatever it moves into
/// the 512-byte virtio sectors the device actually transfers, as a single descriptor spanning the
/// whole request rather than one per block.
pub mod blk {
    use super::PAGE;

    /// Read [`req_blocks`] filesystem blocks starting at `w1` from the disk into the shared
    /// region. Reply `r0` = 0 on success, or [`super::reply_err`] on failure; the bytes land at the
    /// start of the shared region, block `w1` first.
    pub const READ: u64 = 1;
    /// Write [`req_blocks`] filesystem blocks, now in the shared region, to the disk starting at
    /// `w1`. Reply `r0` = 0 on success, or [`super::reply_err`] on failure.
    pub const WRITE: u64 = 2;
    /// Ask the disk's size in bytes. `w1` ignored. Reply `r0` = the size (always non-negative here).
    pub const SIZE: u64 = 3;
    /// **Make everything the device has already acknowledged durable** (milestone 55): a
    /// `VIRTIO_BLK_T_FLUSH` the block server does not reply to until the device completes it.
    ///
    /// `w1` is 0 and is ignored. Reply `r0` = how many device flushes this block server has
    /// completed since it started, **including this one**, so a success is always `>= 1`; or
    /// [`super::reply_err`] on failure.
    ///
    /// # Why a count rather than a zero
    ///
    /// Every other verb here answers 0 for "done", and this one does not, because "the device was
    /// told" is a claim that a caller cannot otherwise check and that a stub would satisfy just as
    /// well. A count that **strictly increases across each successful call** is falsifiable: two
    /// syncs that return the same number mean the second one did not reach the device, which is
    /// exactly the lie this verb exists to make impossible. It is what milestone 55's gate
    /// observes, and it costs one `u64` the server was incrementing anyway.
    ///
    /// # Refusals, and why neither of them is a quiet success
    ///
    /// - **`EOPNOTSUPP` (95)** when the device did not offer `VIRTIO_BLK_F_FLUSH` (feature bit 9),
    ///   so no flush was issued and none can be. This is the case a silent zero would ruin: a
    ///   server that answered "done" here would be telling a backup client its data is on the
    ///   platter when nothing asked the platter anything.
    /// - **`EIO` (5)** when the device completed the request with a status other than
    ///   `VIRTIO_BLK_S_OK`, including `VIRTIO_BLK_S_UNSUPP` (2) from a device that negotiated the
    ///   feature and then refused the command anyway.
    ///
    /// Both travel to a file-service client unchanged through [`super::fs::SYNC`], which is the one
    /// place a block-protocol errno is visible above the FS server. See that verb for the argument.
    pub const FLUSH: u64 = 4;

    /// One filesystem block, in bytes: the transfer unit [`SIZE`] and [`FLUSH`] see and the unit
    /// [`req_blocks`] counts in. Equal to `redoxfs::BLOCK_SIZE`; asserted against it in the block
    /// server so a future RedoxFS bump that changed the block size would fail to build rather than
    /// silently corrupt.
    pub const BLOCK_SIZE: usize = PAGE;

    /// The number of 512-byte virtio sectors in one filesystem block (`BLOCK_SIZE / 512 = 8`).
    pub const SECTORS_PER_BLOCK: u64 = (BLOCK_SIZE as u64) / 512;

    /// **How many blocks the shared region spans, and the most one [`READ`] or [`WRITE`] may carry**
    /// (milestone 138 step 4). Chosen equal to `fs::TRANSFER_PAGES` on purpose: it is the same
    /// number for the same reason, and a full 64 KiB file request now fans out to exactly one blk
    /// request per RedoxFS `Disk::read_at`/`write_at` call up to this many contiguous blocks, rather
    /// than one per block.
    ///
    /// # Why this is the whole of the change, the same way step 3's was
    ///
    /// The request word had 56 bits below the opcode and used none of them; [`req`] packs the count
    /// there and nothing about the block index in the second word changes. **The shared region did
    /// limit a transfer to one block**, because the device DMAs straight into it and a request
    /// larger than the region would overrun a buffer nobody sized for it. Growing the region to
    /// [`TRANSFER_BLOCKS`] contiguous pages, and reading the count out of the request word instead
    /// of assuming one, is the entire mechanism: no new opcode, no descriptor beyond the one that
    /// already carried a whole block, only a longer one.
    ///
    /// # Compatibility, the same property step 3's channel has
    ///
    /// A caller that has never heard of this constant sends [`super::req`]`(op)`, whose low bits are
    /// all zero, which [`req_blocks`] reads back as **one** block: the pre-step-4 behaviour exactly.
    /// `disk_surveyor`, `disk_partitioner` and `mkfs` all call the block protocol this way and are
    /// unmodified by this step, the same compatibility step 3 gave `swish` and the caretakers.
    pub const TRANSFER_BLOCKS: usize = 16;

    /// The largest payload one [`READ`] or [`WRITE`] may carry, in bytes: the whole shared region
    /// ([`TRANSFER_BLOCKS`] blocks). The block server clamps every request's count to this.
    pub const TRANSFER_MAX: usize = TRANSFER_BLOCKS * BLOCK_SIZE;

    /// Pack a blk request's first word: opcode (bits 63:56, [`super::OP_SHIFT`]) and a block count
    /// (bits 7:0, stored as `blocks - 1` so the all-zero word every pre-step-4 caller already sends
    /// still decodes as one block). `blocks` must be in `1..=TRANSFER_BLOCKS`.
    pub const fn req(op: u64, blocks: usize) -> u64 {
        assert!(blocks >= 1 && blocks <= TRANSFER_BLOCKS);
        (op << super::OP_SHIFT) | (blocks as u64 - 1)
    }

    /// The block count of a blk request word: [`req`]'s inverse. A word built by
    /// [`super::req`]`(op)`, which every caller before this step sends, decodes as 1.
    pub const fn req_blocks(w0: u64) -> usize {
        ((w0 & 0xff) + 1) as usize
    }

    /// **The channel's arithmetic, checked by the build rather than by a test**, for
    /// `fs::TRANSFER_MAX`'s reason: this contract has no test harness at EL0, and a build is the
    /// only artifact that can be wrong about it there.
    const _: () = {
        assert!(TRANSFER_BLOCKS >= 1);
        assert!(TRANSFER_BLOCKS <= 256); // the 8-bit count field's range
        assert!(TRANSFER_MAX == TRANSFER_BLOCKS * BLOCK_SIZE);
        // `req`'s all-zero-low-bits compatibility case: a caller that never heard of `req_blocks`
        // sends `super::req(op)`, whose low bits are 0, and that must still mean one block.
        assert!(req_blocks(super::req(READ)) == 1);
    };
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
    ///   of `fs_server/tests/crash_consistency.rs`'s workload, so the sweep that cuts the device at
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

    /// **Read one extended attribute of the node a handle names** (milestone 57).
    ///
    /// The attribute's name is [`req_len`] bytes at the start of the shared page, the handle in
    /// [`req_handle`] is the **file or directory** the attribute is on, and the second word is 0.
    /// Reply `r0` = [`super::xattr::reply`], which packs the value's type code and its length; the
    /// value itself lands in the shared page. `ENODATA` if the attribute is not set.
    ///
    /// Needs [`super::dir::READ`] on the handle, refused with `EBADF`, which is exactly what
    /// [`READ`] needs and answers: an attribute is part of what a file *is*, so the direction the
    /// capability carries is the direction that governs it. See [`super::xattr`] for why there is no
    /// separate attribute right.
    pub const GETXATTR: u64 = 14;
    /// **Set one extended attribute on the node a handle names** (milestone 57).
    ///
    /// The only verb here that carries two payloads and a type: the name is [`req_len`] bytes at the
    /// start of the shared page and the value follows it back to back, with the value's length and
    /// its type code packed into the second word by [`super::xattr::spec`]. Reply `r0` = 0.
    ///
    /// Creating and replacing are one operation, unlike [`CREATE`]: an attribute has no handle and
    /// no identity of its own, so "it already had a value" is not a fact a caller can act on
    /// differently. Linux's `XATTR_CREATE`/`XATTR_REPLACE` flags are deliberately not offered, for
    /// DECISIONS §42's reason: they are emulable only racily above the store.
    ///
    /// Needs [`super::dir::WRITE`], refused with [`super::dir::EROFS`]. `ERANGE` for a name this
    /// store cannot hold, [`super::xattr::E2BIG`] for an over-long value, and
    /// [`super::xattr::ENOSPC`] when the node already holds [`super::xattr::MAX_COUNT`] attributes.
    pub const SETXATTR: u64 = 15;
    /// **List the attribute names on the node a handle names** (milestone 57).
    ///
    /// [`req_len`] and the second word are both 0. Reply `r0` = how many bytes of the shared page
    /// were filled with [`super::xattr::list`] records; 0 means the node has no attributes.
    ///
    /// There is **no cursor**, unlike [`READDIR`], and that is a property rather than an omission:
    /// [`super::xattr::MAX_COUNT`] names of [`super::xattr::MAX_NAME`] bytes fit one page exactly,
    /// so a listing is always complete in one reply and cannot be observed half-changed.
    ///
    /// Needs [`super::dir::READ`], refused with `EBADF`, for [`GETXATTR`]'s reason.
    pub const LISTXATTR: u64 = 16;
    /// **Remove one extended attribute from the node a handle names** (milestone 57).
    ///
    /// [`GETXATTR`]'s shape: the name is [`req_len`] bytes at the start of the shared page, the
    /// second word is 0. Reply `r0` = 0, or `ENODATA` if the attribute was not set. Removing an
    /// attribute that is not there is an **error, not a no-op**, because the caller asked about a
    /// specific thing and "it was not there" is the answer.
    ///
    /// Needs [`super::dir::WRITE`], refused with [`super::dir::EROFS`].
    pub const REMOVEXATTR: u64 = 17;

    /// **How much room the store behind this capability has** (milestone 54). POSIX's `statfs`, cut
    /// down to the three numbers a client actually decides with.
    ///
    /// [`req_len`] and the second word are both 0; [`req_handle`] is **any handle this server
    /// minted**, file or directory, [`ROOT`] included. Reply `r0` = how many bytes of the shared
    /// page were filled, which is [`super::statfs::LEN`], or an error; the record itself is
    /// [`super::statfs`]. `EBADF` for a handle the server did not mint.
    ///
    /// # Why any handle, and why no right
    ///
    /// The answer is a fact about the **storage behind the capability**, not about anything the
    /// capability names, and every handle in one server is on one image. So this takes a handle the
    /// way [`FSTAT`] and [`CLOSE`] do, for the same reason: holding one is the whole
    /// qualification, and a caller with no handle asks nothing. Demanding [`super::dir::READ`]
    /// would have made a write-only grant unable to find out whether its next write fits, which is
    /// the one question it has.
    ///
    /// The handle is still checked, and that is the capability part: a client that holds no
    /// endpoint into this server cannot ask, and one whose endpoint is a caretaker asks through the
    /// caretaker's own table. There is no ambient "how full is the disk" call.
    ///
    /// # BUGS
    ///
    /// - **It answers about the whole image, never about a subtree.** A holder of a narrow subtree
    ///   capability learns the volume's free space, which is more than its own namespace tells it.
    ///   There are no quotas in this filesystem, so there is no smaller number that would be true;
    ///   reporting the subtree's own usage as a "size" would be the silent degradation DECISIONS
    ///   §42 forbids. If quotas ever exist, this verb reports the quota and this note goes away.
    /// - **The number moves when somebody else writes, so it is a channel and not only a leak.**
    ///   The 2026-08-17 security audit's extension of the entry above, recorded-accepted. The entry
    ///   above states what a confined holder learns *once*; polling states what it learns *over
    ///   time*, and the two have different characters. Two programs confined to disjoint subtrees of
    ///   one image, sharing no capability and unable to name each other's names, can still
    ///   communicate: one writes and truncates to modulate the free count, the other polls `STATFS`.
    ///   Nobody has measured the bandwidth. It is bounded below by the store's allocation
    ///   granularity and above by how fast a client can `CALL` this server.
    ///
    ///   It is not fixable at this verb, which is why it is recorded rather than milestoned. A
    ///   confined writer has to be able to ask whether its next write fits, and any answer that is
    ///   true is an answer that moves when the volume moves. Per-subtree quotas would replace the
    ///   channel with a private number rather than merely narrow it, which is the second reason the
    ///   entry above wants them. Until then the honest statement is that **two subtrees on one
    ///   image are not isolated from each other's write volume**, and a deployment that needs that
    ///   isolation needs two images.
    /// - **The numbers are a snapshot with no lock behind it.** Two clients writing concurrently
    ///   both see a free count that was true when it was read; the store's own `ENOSPC` at write
    ///   time is the authority, and this is a forecast. That is what `statfs` is everywhere.
    pub const STATFS: u64 = 18;

    /// **Make everything this server has acknowledged durable** (milestone 55). The verb behind
    /// SMB2's `FLUSH`, and the reason macOS's `VOLUME_FULL_SYNC` bit is claimable at all.
    ///
    /// [`req_len`] and the second word are both 0; [`req_handle`] is **any handle this server
    /// minted**, file or directory, [`ROOT`] included, exactly as [`STATFS`] takes one. Reply `r0`
    /// = how many device flushes the storage behind this capability has completed since the block
    /// server started, **including this one**, so a success is always `>= 1`; or an error. `EBADF`
    /// for a handle the server did not mint.
    ///
    /// # What it actually promises, which is narrower than the name
    ///
    /// It promises that the **device** has been told, and that the reply waited for the device to
    /// say it was done. It does not promise anything about the filesystem's own state, because
    /// there is nothing to promise: the FS server puts every write through one RedoxFS transaction
    /// that commits to the header ring before it replies, so there is no dirty state above the
    /// block device at the moment a client could ask. This verb closes the layer below that one,
    /// where the block server previously issued no `VIRTIO_BLK_T_FLUSH` at all and the durability
    /// of the last acknowledged write was the device's word rather than ours.
    ///
    /// So the honest sentence is: **after a successful `SYNC`, every write this server has
    /// acknowledged is on the medium the device calls durable.** What "durable" means is still the
    /// device's definition, and a device that lies about its own flush is outside anything a
    /// protocol can check.
    ///
    /// # Rights: [`super::dir::WRITE`], refused with [`super::dir::EROFS`]
    ///
    /// This is the one place this contract departs from [`STATFS`]'s "a handle is the whole
    /// qualification", and the reason is that a sync is an **action on the device** rather than a
    /// fact about it. A holder that may not write has nothing outstanding to make durable, and
    /// letting it ask would hand a read-only capability a lever on every other client's write
    /// latency: a device flush stalls the queue for everyone on the image. Requiring the right
    /// costs nothing real, because anyone with something to sync wrote it first.
    ///
    /// `EROFS` rather than `EACCES` for the reason [`super::dir`] gives everywhere else: through
    /// this capability the storage does not take writes, so there is no policy that could have
    /// said yes.
    ///
    /// # The refusal that must stay loud
    ///
    /// When the device cannot flush, the block server's `EOPNOTSUPP` (95) is passed **through**
    /// this verb unchanged rather than mapped or swallowed. That is deliberate and it is the whole
    /// point of the verb existing: a `SYNC` that answered 0 on a device with no flush would be the
    /// silent degradation DECISIONS §42 forbids, and it would be that in the one place where the
    /// consequence is somebody's backup. A client that meets `EOPNOTSUPP` here knows the truth,
    /// which is that this storage cannot be made durable on demand.
    ///
    /// It is also the only errno on this contract that does not originate in the FS server or in
    /// RedoxFS. The FS server maps RedoxFS's error type once, at its serve loop; a blk errno is
    /// already a wire value when it arrives, and re-mapping it to `EIO` would throw away the one
    /// distinction the caller needs (cannot, versus tried and failed).
    ///
    /// # BUGS
    ///
    /// - **The count is a fact about the device, not about the caller**, so two clients of one
    ///   image see one number and each can watch the other sync. That is [`STATFS`]'s free-count
    ///   channel again, with the same answer: there is no smaller number that would be true, and
    ///   two subtrees on one image are not isolated from each other's IO. The bandwidth is lower
    ///   here than `STATFS`'s, because a sync costs a device round trip and a truncate does not.
    /// - **It syncs the whole device, never a subtree or a file.** A holder of one file makes the
    ///   entire image durable, including writes it cannot see and did not make. POSIX's `fsync` on
    ///   a descriptor suggests otherwise; nothing below here can honour that, because RedoxFS
    ///   commits a transaction across the image and the block layer moves blocks rather than
    ///   files. Naming it `SYNC` rather than `FSYNC` is the whole of what this note buys, and it
    ///   is on purpose.
    /// - **It does not fence.** A client that issues a write and a sync concurrently on two
    ///   handles gets no ordering guarantee between them; the sync covers what the server had
    ///   already acknowledged when it ran. There is no ordering primitive on this contract, and a
    ///   backup client's own write-then-flush is sequential, which is why this has not needed one.
    pub const SYNC: u64 = 19;

    /// **How many pages the file channel spans** (milestone 138 step 3). The client and the FS
    /// server share this many *contiguous* pages, not one, and a [`READ`] or [`WRITE`] may carry
    /// up to [`TRANSFER_MAX`] bytes through them in a single request.
    ///
    /// # Why this is the whole of the change
    ///
    /// The request word has carried a 40-bit length field, [`MAX_LEN`], since this contract was
    /// written, so nothing in the packing ever limited a transfer to 4096 bytes. **The page did.** Bulk rides in the
    /// region the two parties share, that region was one page, and so a request that asked for
    /// more would have had the server write past the end of it. Growing the region is therefore
    /// the entire multi-page transfer: no new opcode, no second reply word, no descriptor, and no
    /// change to a single packed field. Milestone 38's `BUGS` entry called the fix "a multi-page
    /// transfer on the contract"; this is what that turned out to cost.
    ///
    /// 16 pages, so [`TRANSFER_MAX`] is 64 KiB, which is the size milestone 38 measured ext4
    /// moving "for about what it charges for 4 KiB". It is a tuning number and it is the one place
    /// to change it.
    ///
    /// # What a party has to map, and the one thing that is not checked
    ///
    /// The FS server maps all [`TRANSFER_PAGES`] pages, because it must be able to serve any
    /// request this contract permits. **A client maps as many as it intends to use**, and asks for
    /// no more than it mapped. A client that only ever moves 4096 bytes therefore needs no change
    /// at all: it maps one page, sends `len` 4096, and cannot tell that the region behind the
    /// server grew. That is the compatibility property, and it is a real one rather than a
    /// courtesy: `swish` and the caretakers are unmodified by this step.
    ///
    /// **The foot gun, marked because it is rung four of `CLAUDE.md`'s ladder and not rung one:**
    /// nothing checks that a client asked for no more than it mapped. A client that sends
    /// `len = TRANSFER_MAX` with one page mapped gets a data abort on its own second page, and it
    /// gets it *after* the server has already done the work. This is the same agreement a client
    /// already has with its wiring about where the region lives at all (every FS client hardcodes
    /// its own `FILE_VA` and the kernel's wiring carries a comment saying it must match), one
    /// number wider rather than a new category. Making it unrepresentable needs the channel size
    /// to travel on the wire, which is a new concept on this contract and is not this step's.
    pub const TRANSFER_PAGES: usize = 16;

    /// **The largest payload one [`READ`] or [`WRITE`] may carry**, in bytes: the whole file
    /// channel ([`TRANSFER_PAGES`] pages). The FS server clamps every request's length to this,
    /// which is the one bound that keeps a request inside the region it shares.
    ///
    /// Setting [`TRANSFER_PAGES`] to 1 makes this [`super::PAGE`] again and reproduces the
    /// contract exactly as it stood before milestone 138 step 3, which is what
    /// `bench/transfer-size-sweep.sh` uses to measure a before against an after on one harness.
    pub const TRANSFER_MAX: usize = TRANSFER_PAGES * super::PAGE;

    /// The largest length or offset that fits the packing below (40 bits). Far above
    /// [`TRANSFER_MAX`], so the bit-packing has never been what bounds a transfer; the bound only
    /// guards the packing itself, and [`TRANSFER_MAX`] is what bounds a payload.
    pub const MAX_LEN: u64 = (1 << 40) - 1;

    /// **The channel's arithmetic, checked by the build rather than by a test**, because a test does
    /// not run in the EL0 build and a build is the only artifact that can be wrong about this.
    ///
    /// Both halves have a failure mode worth naming. A channel that is not a whole number of pages
    /// cannot be wired at all, since a mapping is per page. And a length the packing truncates would
    /// arrive at the server as a **smaller successful** read rather than as an error, which is
    /// DECISIONS §27's half-working write wearing a different hat.
    const _: () = {
        assert!(TRANSFER_PAGES >= 1);
        assert!(TRANSFER_MAX == TRANSFER_PAGES * super::PAGE);
        assert!(TRANSFER_MAX as u64 <= MAX_LEN);
    };
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
///   [`fs::OPENDIR`]) answers `ENOENT`. In this scope there is no such name, which is the same
///   sentence `fs_file_caretaker` says for the same reason: a holder must not be able to map what
///   it cannot reach.
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
            // The extended-attribute vocabulary (milestone 57). It renders here rather than in an
            // `xattr::explain` of its own for this module's own reason: the sentence a human reads
            // belongs next to the decision about what the errno means, and there is one such place.
            // Every line is still a fact about the thing or the capability.
            super::xattr::ENODATA => "that attribute is not set",
            super::xattr::ERANGE => "that is not a name an attribute can have",
            super::xattr::E2BIG => "that attribute value is too large to store",
            super::xattr::ENOSPC => "that already holds as many attributes as it can",
            super::xattr::ENOTSUP => "this capability does not carry extended attributes",
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

/// **The verb table: what each request word means, declared once instead of three times**
/// (milestone 61).
///
/// Three caretakers (`user/src/fs_file_caretaker.rs`, `fs_subtree_caretaker.rs`,
/// `fs_nameset_caretaker.rs`) proxy this same contract over a narrowed namespace, and each one used
/// to be a hand-written `match` over the opcode. Nothing made a `match` and the contract agree, so
/// the way that failed is that **a new verb was simply absent from a caretaker and the capability
/// silently was not there**. That is not a hypothetical: milestone 57 added the four
/// extended-attribute verbs, none of the three was taught them, and nothing failed. The programs
/// behind a per-file grant just could not read their own file's attributes.
///
/// This table inverts the failure mode. A verb declares its **argument shape** (does the word's
/// length field count a name, a payload, or nothing?) and the **rights the server will demand**, and
/// the caretakers dispatch off that instead of off a match arm somebody has to remember to add.
/// Adding a verb to [`fs`] without a row here is a **compile error** (see [`verb::TABLE`]'s const
/// assertions), which is the whole deliverable: forgetting now fails the build rather than producing
/// a capability that is quietly missing.
///
/// # What it shares, and the thing it must never share
///
/// **It shares the dispatch and never the attenuation.** The three caretakers narrow by three
/// different means and stay three programs, which is a refutation the roadmap records rather than a
/// coincidence:
///
/// - `fs_subtree_caretaker` performs **no checks at all**. One [`fs::OPENDIR`] at startup, the
///   server intersects the rights and mints a restricted handle, and everything afterwards is
///   reached through that handle. This table gives it its shape lookup and nothing else; it consults
///   no policy and no filter, because a branch here is a branch that can be wrong.
/// - `fs_nameset_caretaker` filters names, on **every** verb whose operand is a directory name, and
///   [`verb::Operand::Name`] is what tells it which those are. Before this table that was a
///   hand-listed set of arms, so a new name-taking verb would have arrived **unfiltered**, which is
///   the same failure one class more dangerous.
/// - `fs_file_caretaker` translates between two *protocols*, directory in and file out, so it needs
///   a per-verb answer and has one in [`verb::file_grant::POLICY`]. Its refusals keep their distinct
///   errnos deliberately: [`grant::ENOTDIR`] means the request does not mean anything through a file
///   capability, which is a different statement from a refusal, and flattening the two into
///   "allowed / not allowed" would destroy it.
///
/// # BUGS
///
/// - **A wrong row is wrong in three programs at once.** The mitigation is that it is pure data in a
///   host-testable crate, reachable by the tests below and by Kani, which a hand-written match in a
///   `no_std` binary is not.
/// - **It does not make the caretakers interchangeable**, and must not try to. Only the dispatch is
///   shared; what each attenuates to stays hand-written and stays in three separate address spaces.
pub mod verb {
    use super::{dir, fs};

    /// **What the length field of a request word counts**, which is the only thing about a request
    /// a proxy has to understand in order to forward it faithfully.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Operand {
        /// Nothing. The request names a handle and the length field is unused, so a caretaker
        /// forwards a length of **zero** rather than whatever the client happened to send. That is
        /// not tidiness: it means a client cannot smuggle a length into a verb that has none and
        /// have some later server give it a meaning.
        None,
        /// A **directory name**, `req_len` bytes at the start of the shared page, resolved under the
        /// request's handle. This is the operand a name filter applies to, and the only one:
        /// `fs_nameset_caretaker` asks its question exactly when this is the shape.
        Name,
        /// `req_len` bytes of **payload** that are not a directory name: file data
        /// ([`fs::READ`]/[`fs::WRITE`]), or an attribute name and value. Forwarded verbatim, and
        /// **never filtered**, because an attribute name is not a name in the namespace the
        /// capability narrows.
        Payload,
        /// Two directories and two names, [`fs::RENAME`] alone: the source is the request word's
        /// handle and length, the destination rides in the second word ([`fs::rename_dst`]), and
        /// both names sit back to back in the page. The only shape a proxy must rewrite rather than
        /// forward, because both handles are the client's.
        Rename,
    }

    /// One row: everything a proxy needs to know about one verb.
    #[derive(Clone, Copy, Debug)]
    pub struct Verb {
        /// The opcode. Rows are stored in opcode order and the ordering is asserted at compile time.
        pub op: u64,
        /// Its name, for a diagnostic and for a test's failure message. Costs nothing at runtime in
        /// the caretakers, which never read it.
        pub name: &'static str,
        /// What the length field counts.
        pub operand: Operand,
        /// Whether the **second word** carries something the server reads (an offset, a size, a
        /// cursor, a rights mask, an attribute spec). When it does not, a caretaker forwards zero,
        /// for [`Operand::None`]'s reason.
        pub carries_w1: bool,
        /// Whether a successful reply is a **new handle**, which a proxy that renumbers handles must
        /// install in its own table before handing back.
        pub mints_handle: bool,
        /// The [`dir`] rights the server demands, **all of them**. Zero where the verb needs none
        /// ([`fs::CLOSE`], [`fs::FSTAT`]) or where the requirement is "any of" rather than "all of"
        /// (see [`Verb::needs_any`]).
        pub needs_all: u64,
        /// The rights where **any one** of them will do: only [`fs::OPEN`], which takes
        /// [`dir::READ`] or [`dir::WRITE`]. Kept apart from [`Verb::needs_all`] because
        /// `Rights::allows` is deliberately "all of", and folding an any-of requirement into it
        /// would be a hole rather than a simplification.
        pub needs_any: u64,
    }

    impl Verb {
        /// Whether the length field means anything, so a caretaker knows to forward it.
        pub const fn carries_len(&self) -> bool {
            !matches!(self.operand, Operand::None)
        }

        /// Whether the operand is a name in the namespace a caretaker narrows. **This is what a
        /// name filter keys off**, and the reason it is a table field rather than a list inside
        /// `fs_nameset_caretaker`: a name-taking verb added to the contract is filtered from the
        /// moment its row exists, instead of from the moment somebody remembers.
        pub const fn takes_name(&self) -> bool {
            matches!(self.operand, Operand::Name | Operand::Rename)
        }

        /// Whether the verb **mutates**, which is the one question a direction-carrying grant asks.
        /// A read-only per-file grant refuses exactly these with [`super::grant::EROFS`], so a
        /// mutating verb added to the contract is refused by a read-only grant from its first day
        /// rather than forwarded because nobody updated a list.
        pub const fn mutates(&self) -> bool {
            self.needs_all & (dir::WRITE | dir::CREATE | dir::REMOVE) != 0
        }
    }

    /// A row, spelled once so the table below reads as data.
    const fn row(
        op: u64,
        name: &'static str,
        operand: Operand,
        carries_w1: bool,
        mints_handle: bool,
        needs_all: u64,
    ) -> Verb {
        Verb {
            op,
            name,
            operand,
            carries_w1,
            mints_handle,
            needs_all,
            needs_any: 0,
        }
    }

    /// The lowest opcode in the contract, and the first row's.
    pub const FIRST: u64 = fs::OPEN;
    /// The highest opcode in the contract, and the last row's. Raising it without adding a row is a
    /// compile error.
    pub const LAST: u64 = fs::STATFS;

    /// **Every verb the file-service contract carries, in opcode order.**
    ///
    /// The rights column is the server's, copied from the doc comment on each opcode in [`fs`] and
    /// checked against the server's behaviour by `fs_server`'s own tests. It is here because a proxy
    /// that knows a verb mutates can refuse it without having to know what mutating means.
    pub const TABLE: [Verb; (LAST - FIRST + 1) as usize] = [
        // OPEN's requirement is "READ or WRITE", the one any-of in the contract, so it is the one
        // row built by hand rather than by `row`.
        Verb {
            op: fs::OPEN,
            name: "OPEN",
            operand: Operand::Name,
            carries_w1: false,
            mints_handle: true,
            needs_all: 0,
            needs_any: dir::READ | dir::WRITE,
        },
        // The second word is an offset for both, which is why they carry it and TRUNCATE's size
        // rides there too: an offset-shaped quantity is not a payload length.
        row(fs::READ, "READ", Operand::Payload, true, false, dir::READ),
        row(
            fs::WRITE,
            "WRITE",
            Operand::Payload,
            true,
            false,
            dir::WRITE,
        ),
        row(fs::CLOSE, "CLOSE", Operand::None, false, false, 0),
        row(fs::FSTAT, "FSTAT", Operand::None, false, false, 0),
        row(
            fs::CREATE,
            "CREATE",
            Operand::Name,
            false,
            true,
            dir::CREATE,
        ),
        row(
            fs::TRUNCATE,
            "TRUNCATE",
            Operand::None,
            true,
            false,
            dir::WRITE,
        ),
        // OPENDIR and MKDIR carry the requested rights mask in the second word, which is what makes
        // them the two verbs that hand back *authority* rather than data.
        row(
            fs::OPENDIR,
            "OPENDIR",
            Operand::Name,
            true,
            true,
            dir::DESCEND,
        ),
        row(
            fs::READDIR,
            "READDIR",
            Operand::None,
            true,
            false,
            dir::ENUMERATE,
        ),
        row(
            fs::MKDIR,
            "MKDIR",
            Operand::Name,
            true,
            true,
            dir::CREATE | dir::DESCEND,
        ),
        // RENAME needs REMOVE on the source directory and CREATE on the destination. Both are in
        // `needs_all` because a proxy asks only "does this mutate", and it does, twice over.
        row(
            fs::RENAME,
            "RENAME",
            Operand::Rename,
            true,
            false,
            dir::REMOVE | dir::CREATE,
        ),
        row(
            fs::UNLINK,
            "UNLINK",
            Operand::Name,
            false,
            false,
            dir::REMOVE,
        ),
        row(fs::RMDIR, "RMDIR", Operand::Name, false, false, dir::REMOVE),
        // The extended-attribute verbs (milestone 57). Their operand is a **payload**, not a name:
        // an attribute name is not a name in the directory, so a name-set capability must not filter
        // it and a per-file capability must not compare it against the granted name. Getting that
        // wrong would have been the natural mistake here, which is why the distinction is a variant
        // of `Operand` rather than a comment.
        row(
            fs::GETXATTR,
            "GETXATTR",
            Operand::Payload,
            false,
            false,
            dir::READ,
        ),
        // The only attribute verb with a second word: the value's length and type code.
        row(
            fs::SETXATTR,
            "SETXATTR",
            Operand::Payload,
            true,
            false,
            dir::WRITE,
        ),
        row(
            fs::LISTXATTR,
            "LISTXATTR",
            Operand::None,
            false,
            false,
            dir::READ,
        ),
        row(
            fs::REMOVEXATTR,
            "REMOVEXATTR",
            Operand::Payload,
            false,
            false,
            dir::WRITE,
        ),
        // STATFS names a handle and nothing else: no operand, no second word, no right. The reply
        // fills the shared page, which costs a proxy nothing because the page is the one all three
        // parties already share (see `fs_subtree_caretaker`'s `PAGE_VA`).
        row(fs::STATFS, "STATFS", Operand::None, false, false, 0),
    ];

    /// **The row for an opcode, or `None` if the contract does not carry it.**
    ///
    /// A caretaker's whole dispatch is this lookup plus the fields it returns, so an opcode a client
    /// invents is refused at one site rather than falling through a match's catch-all.
    pub const fn of(op: u64) -> Option<&'static Verb> {
        if op < FIRST || op > LAST {
            return None;
        }
        Some(&TABLE[(op - FIRST) as usize])
    }

    // **The compile-time half of the deliverable.** Adding an opcode to `fs` and forgetting a row
    // here fails the build instead of producing a caretaker that silently does not offer it. A
    // runtime test cannot do this job: the three caretakers are `no_std` binaries and a test that
    // has to boot them would find the gap long after the commit that made it.
    const _: () = assert!(
        TABLE.len() == (LAST - FIRST + 1) as usize,
        "every opcode between verb::FIRST and verb::LAST needs a row in verb::TABLE"
    );
    const _: () = {
        let mut i = 0;
        while i < TABLE.len() {
            assert!(
                TABLE[i].op == FIRST + i as u64,
                "verb::TABLE is stored in opcode order and indexed by it; a row is out of place"
            );
            i += 1;
        }
    };

    /// **What a per-file grant answers for each verb** (`user/src/fs_file_caretaker.rs`).
    ///
    /// This one caretaker needs a policy and the other two do not, and the asymmetry is the design
    /// rather than an accident. `fs_subtree_caretaker` and `fs_nameset_caretaker` serve the
    /// *directory* protocol their client speaks, so every verb means what it always meant and the
    /// attenuation lives elsewhere (in the handle the server minted, and in a name filter). A file
    /// capability is a **different protocol**: it has no namespace to enumerate, nothing to create a
    /// name in, and exactly one handle. So "which verbs exist" is part of what the capability *is*,
    /// and that belongs in a table somebody wrote on purpose.
    pub mod file_grant {
        use super::super::grant;

        /// What the caretaker does with one verb.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum Policy {
            /// **Answered here, from what this process holds.** [`super::fs::OPEN`] resolves the one
            /// granted name; [`super::fs::CLOSE`] is a truthful no-op, because the caretaker owns
            /// the underlying handle for its whole life and a client closing "its" handle must not
            /// be able to make the next request fail.
            Local,
            /// **Forwarded on the handle the caretaker opened at startup**, with the caller's handle
            /// substituted. A mutating verb ([`super::Verb::mutates`]) is refused with
            /// [`grant::EROFS`] when the grant carries no write direction, and the refusal is
            /// table-driven rather than listed, so a mutating verb added to the contract is refused
            /// by a read-only grant from the day its row exists.
            Forward,
            /// **Not offered, and the errno says why.** [`grant::ENOTDIR`] where the request does
            /// not *mean* anything through a file capability, which is a stronger and more useful
            /// statement than a refusal: a refusal implies a policy that could have said yes, and
            /// there is no policy here, only what the capability is.
            Refused(i32),
        }

        /// The policy for each verb, indexed exactly as [`super::TABLE`] is.
        ///
        /// # BUGS
        ///
        /// Every directory verb answers `ENOTDIR`, the same statement `CREATE` makes: a file
        /// capability is not a directory, so the request does not mean anything here. They
        /// answered `EBADF` until 2026-08-01, inherited from the catch-all this table replaced,
        /// which shared one arm with "you named a handle I never minted". Writing the rows down is
        /// what made the conflation visible, and the fix is that the two words now mean one thing
        /// each: `EBADF` is about the caller's handle, `ENOTDIR` is about the request.
        pub const POLICY: [Policy; super::TABLE.len()] = [
            Policy::Local,                   // OPEN: the one granted name, anything else ENOENT.
            Policy::Forward,                 // READ
            Policy::Forward,                 // WRITE, EROFS without the direction
            Policy::Local,                   // CLOSE
            Policy::Forward,                 // FSTAT
            Policy::Refused(grant::ENOTDIR), // CREATE: a file capability is not a directory.
            Policy::Forward,                 // TRUNCATE, EROFS without the direction
            // **All six say the same thing `CREATE` says**, and for the same reason: a file
            // capability is not a directory, so "list it", "make a name in it" or "take a name out
            // of it" do not *mean* anything here. There is no policy that could have said yes.
            //
            // They answered `EBADF` until 2026-08-01, inherited from the catch-all this table
            // replaced, which shared one arm with "you named a handle I never minted". Two
            // different statements came out as one word: one is about the request, the other about
            // the caller's handle. Writing the rows down is what made the conflation visible.
            Policy::Refused(grant::ENOTDIR), // OPENDIR
            Policy::Refused(grant::ENOTDIR), // READDIR
            Policy::Refused(grant::ENOTDIR), // MKDIR
            Policy::Refused(grant::ENOTDIR), // RENAME
            Policy::Refused(grant::ENOTDIR), // UNLINK
            Policy::Refused(grant::ENOTDIR), // RMDIR
            // **The milestone-61 change.** All four used to answer `super::super::xattr::ENOTSUP`
            // here, so a
            // program behind a per-file grant could not read its own file's attributes. They are
            // forwarded now, and the two that mutate are refused with `EROFS` through a read-only
            // grant by `Policy::Forward`'s own rule, which is the same rule `WRITE` and `TRUNCATE`
            // already obeyed. An attribute is part of what a file *is* (see `xattr`'s header), so a
            // capability that may read the file may read what is attached to it.
            Policy::Forward, // GETXATTR
            Policy::Forward, // SETXATTR, EROFS without the direction
            Policy::Forward, // LISTXATTR
            Policy::Forward, // REMOVEXATTR, EROFS without the direction
            // STATFS is forwarded rather than refused, and it is the one directory-shaped question
            // that means something through a file capability: a program granted one file to write
            // still has to know whether its next write fits. The server takes any handle it minted,
            // so the caretaker's substituted file handle answers it.
            Policy::Forward, // STATFS
        ];

        /// `EBADF`: no such handle, and only that since 2026-08-01 (see [`POLICY`]'s BUGS).
        pub const EBADF: i32 = 9;

        /// The policy for an opcode, or `None` for an opcode the contract does not carry.
        pub const fn of(op: u64) -> Option<Policy> {
            if op < super::FIRST || op > super::LAST {
                return None;
            }
            Some(POLICY[(op - super::FIRST) as usize])
        }

        // A policy per verb, checked where a missing one is cheapest to find.
        const _: () = assert!(
            POLICY.len() == super::TABLE.len(),
            "every verb needs a per-file-grant policy; add the row next to the verb's"
        );
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

/// **How [`fs::STATFS`] packs the volume's numbers into the shared page** (milestone 54).
///
/// Three little-endian `u64`s, in this order: the allocation unit in bytes, the volume's size in
/// those units, and how many of them are free. Nothing else, because nothing else changes a
/// caller's decision: a client asks this to find out whether what it is about to write will fit,
/// and macOS asks it to size a Time Machine sparsebundle against the volume it is landing on.
///
/// # The three choices worth arguing with
///
/// **A record in the page rather than a packed reply word.** A reply carries one `i64`, and two
/// 64-bit counts do not fit in one however they are packed. Every other verb that answers with more
/// than a number ([`fs::READDIR`], [`fs::LISTXATTR`]) fills the page and returns its length, so this
/// is the contract's existing shape rather than a new one.
///
/// **The unit is a field, not [`PAGE`].** They are the same number today (4096) and they are not the
/// same *thing*: one is the filesystem's allocation granularity and the other is the IPC transfer
/// unit. A client that assumed they were equal would silently mis-size a volume the day a
/// filesystem with 64 KiB records is served through the same contract.
///
/// **The reply's length is the version.** `r0` is [`statfs::LEN`] today; a future field extends the
/// record and raises it, and a client written against this version reads its prefix correctly
/// because it checks `r0 >= LEN` for the fields it knows rather than `r0 == LEN`.
/// [`statfs::decode`] is written that way on purpose. There is deliberately **no version word**:
/// the length already is one, and a second one would be a thing to keep in sync.
///
/// # BUGS
///
/// - **Free space is a forecast, not a reservation.** See [`fs::STATFS`]'s own BUGS: the store's
///   `ENOSPC` at write time is the authority.
/// - **There is no inode count.** POSIX `statfs` carries `f_files`/`f_ffree`, and SMB's volume
///   classes carry neither, so nothing above this contract has asked. Adding them extends the
///   record by the paragraph above rather than changing it.
pub mod statfs {
    /// Bytes one record takes. Also the value [`super::fs::STATFS`] answers with on success; see
    /// the module header on why the length is the version.
    pub const LEN: usize = 24;

    /// Write a record at the start of `out`, returning its length, or `None` if it does not fit.
    pub fn encode(
        out: &mut [u8],
        block_size: u64,
        total_blocks: u64,
        free_blocks: u64,
    ) -> Option<usize> {
        if out.len() < LEN {
            return None;
        }
        out[0..8].copy_from_slice(&block_size.to_le_bytes());
        out[8..16].copy_from_slice(&total_blocks.to_le_bytes());
        out[16..24].copy_from_slice(&free_blocks.to_le_bytes());
        Some(LEN)
    }

    /// Read a record from the start of `buf` (the `r0` bytes a [`fs::STATFS`] reply filled), as
    /// `(block_size, total_blocks, free_blocks)`. `None` if the reply is shorter than this version
    /// of the record, which is what a truncated reply looks like and must not be read as zeroes.
    ///
    /// [`fs::STATFS`]: super::fs::STATFS
    pub fn decode(buf: &[u8]) -> Option<(u64, u64, u64)> {
        if buf.len() < LEN {
            return None;
        }
        let w = |at: usize| u64::from_le_bytes(buf[at..at + 8].try_into().unwrap());
        Some((w(0), w(8), w(16)))
    }

    /// The volume's size in bytes, saturating rather than wrapping: the numbers come off a disk and
    /// a corrupt pair must not become a small plausible answer.
    pub const fn total_bytes(block_size: u64, total_blocks: u64) -> u64 {
        block_size.saturating_mul(total_blocks)
    }

    /// The free bytes, saturating for [`total_bytes`]'s reason.
    pub const fn free_bytes(block_size: u64, free_blocks: u64) -> u64 {
        block_size.saturating_mul(free_blocks)
    }
}

/// **The per-file grant** (milestone 31 phase 2): what it means to hold one file rather than a
/// directory, and how the narrowing travels.
///
/// A directory capability lets its holder name anything in the bound directory. `run wc report.txt`
/// must hand over less than that: **one file, in one direction, and nothing else**. The narrowing
/// is done by an attenuator, `user/src/fs_file_caretaker.rs`, which holds the directory capability,
/// opens exactly the granted name once at startup, and then serves the *same* [`fs`] protocol on
/// its own endpoint with three rules:
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
/// makes the claim checkable: the confined program holds an endpoint to the caretaker and nothing
/// that names the FS server, so it cannot route around the narrowing even in principle.
/// Open-by-path still exists only inside a server (DECISIONS §27); the caretaker just serves a
/// server whose entire namespace is one name.
pub mod grant {
    /// The granted directions, packed into the caretaker's spec word. Read alone is the common case
    /// (`run wc report.txt`); write implies read, since a writer that cannot read back is a shape
    /// nothing has asked for.
    pub const READ: u64 = 1 << 0;
    pub const WRITE: u64 = 1 << 1;

    /// The longest granted name, in bytes. The name rides in **two `START` argument words** rather
    /// than a frame, so a per-file grant costs no extra page and no extra mapping, and the
    /// caretaker needs nothing mapped before it runs. Sixteen bytes is short, and deliberately so:
    /// it is a demonstrator's limit, not a filesystem's, and lifting it means giving the caretaker
    /// a frame, which is a change to the wiring and not to this contract.
    pub const MAX_NAME: usize = 16;

    /// `EROFS`, the reply to a write through a read-only grant. Chosen over `EACCES` on purpose:
    /// `EACCES` is the Unix answer, "you were denied", which implies a policy that could have said
    /// yes. There is no policy here. The capability carries one direction, so the honest statement
    /// is about the thing itself, and `std::io::ErrorKind::ReadOnlyFilesystem` is what a caller sees.
    pub const EROFS: i32 = 30;

    /// `ENOTDIR`, the reply to a [`super::fs::CREATE`] through a file grant. "This is a file, not a
    /// directory" is a fact about what the holder has, not a refusal of what it asked.
    pub const ENOTDIR: i32 = 20;

    /// The handle the caretaker mints for the one file it serves. Fixed, because there is exactly
    /// one: a holder that guesses a different number gets `EBADF` from the same check every other
    /// handle goes through.
    pub const HANDLE: u64 = 0;

    /// Pack a granted name into the two argument words the caretaker is started with. Names shorter
    /// than [`MAX_NAME`] are zero-padded; longer ones are refused by the caller (see [`fits`]).
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

    /// Pack the name length and the granted rights into the caretaker's third argument word.
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

    /// **A grant whose operand is the holder's whole namespace**, spelled as a name of zero length.
    ///
    /// A per-file or per-directory grant names one thing, and [`fits`] refuses an empty name, so a
    /// length of zero cannot mean a name and is free to mean something else. It means the grant
    /// carries a **set** ([`super::nameset`]), which does not fit in two argument words, so the
    /// program is told "act on everything you can see" and reads the set by enumerating the
    /// capability it was handed. That is not a loophole: the namespace of a set capability *is* the
    /// set, so enumerating it reveals exactly what the command line already showed.
    pub const WHOLE_NAMESPACE: usize = 0;
}

/// **A directory capability attenuated to a set of names** (milestone 47's globbing lane).
///
/// [`grant`] narrows a directory down to one name, which is what `wc report.txt` and `rm old.txt`
/// designate. `rm *.txt` designates more than one and fewer than all, and the roadmap's decided
/// answer is a directory capability attenuated to **the names that matched**. `fs_file_caretaker`
/// already serves a namespace of exactly one name; this is the same shape with a wider namespace,
/// served by `user/src/fs_nameset_caretaker.rs`.
///
/// # Why the set rides in a frame and the single name does not
///
/// One name fits in two `START` argument words, which is why a per-file grant costs no page. A set
/// does not fit in any number of registers, so it is written into a frame the wiring maps
/// **read-only** into the caretaker. That is the honest place for `ARG_MAX` to reappear: it is not
/// a buffer limit any more, it is the size of a capability, and [`nameset::MAX_NAMES`] is where the
/// ceiling is declared.
///
/// # The encoding, and why each name carries its type
///
/// A record is one header byte then the name: bit 7 of the header is [`nameset::IS_DIR`] and the low seven
/// are the length (a name is at most [`grant::MAX_NAME`] bytes, so seven bits is room to spare). A
/// zero header terminates the set, which is why a name may not be empty.
///
/// The type bit is carried because the caretaker **answers `READDIR` from the set itself** rather
/// than filtering the server's listing: the set is the namespace, so listing it needs no round trip
/// and no cursor translation. The type is what the directory said at the moment the grant was
/// planned, which is the same "resolved at grant time" rule every other part of a grant follows: a
/// `cd` after the fact cannot change what an already-planned grant means, and neither can a
/// `mkdir`.
pub mod nameset {
    use super::grant;

    /// **The most names one set grant carries**, and the point where `ARG_MAX` becomes a capability
    /// limit rather than a buffer limit.
    ///
    /// The number is set by what the two ends can hold **by value** with no allocator, and it was
    /// **measured rather than chosen**: at sixteen names the shell ran off the bottom of its stack
    /// planning one grant, because a set travels by value through four frames a debug build does not
    /// collapse. Eight names of sixteen bytes is 152 bytes per copy, and the shell fits.
    ///
    /// There is a second reason, and it is the one that decides the *shape* of the failure: `caps rm
    /// *.txt` prints the set before anything runs, and a grant nobody can read is a grant nobody
    /// checked. Exceeding this is a **refusal at the prompt** (`grant_plan::Refusal::TooManyNames`), never
    /// a truncation, because a glob that quietly granted a prefix of what it matched would be the
    /// worst outcome this lane could produce.
    pub const MAX_NAMES: usize = 8;

    /// Bytes an encoded set occupies at most: a header and a name each, plus the terminator.
    pub const BYTES: usize = MAX_NAMES * (1 + grant::MAX_NAME) + 1;

    /// Bit 7 of a record's header: this name was a directory when the grant was planned.
    pub const IS_DIR: u8 = 1 << 7;

    /// The low bits of a record's header: the name's length. Zero terminates the set.
    const LEN: u8 = 0x7f;

    /// Write `names` into `out`, returning the bytes used, or `None` if the set does not fit, a name
    /// is empty or too long, or there are more than [`MAX_NAMES`] of them.
    ///
    /// **`None` rather than a short write**, for [`MAX_NAMES`]'s reason: the caller is building a
    /// capability, and a truncated capability is a different capability.
    pub fn encode(names: &[(&[u8], bool)], out: &mut [u8]) -> Option<usize> {
        if names.len() > MAX_NAMES {
            return None;
        }
        let mut n = 0;
        for &(name, is_dir) in names {
            if name.is_empty() || name.len() > grant::MAX_NAME {
                return None;
            }
            let end = n + 1 + name.len();
            if end + 1 > out.len() {
                return None;
            }
            out[n] = name.len() as u8 | if is_dir { IS_DIR } else { 0 };
            out[n + 1..end].copy_from_slice(name);
            n = end;
        }
        *out.get_mut(n)? = 0;
        Some(n + 1)
    }

    /// Walk an encoded set: `(name, is_dir)` per record. Stops at the terminator, and at the first
    /// record that runs off the end, which is what a truncated buffer looks like and must not be
    /// read as a name.
    pub fn iter(buf: &[u8]) -> Names<'_> {
        Names { buf, at: 0 }
    }

    /// Whether `name` is in the set. **This is the whole of the attenuation**: the caretaker asks
    /// it once per name-taking request against its granted directory, and a name that is not in the
    /// set is `ENOENT`, because in that scope there is no such name.
    pub fn contains(buf: &[u8], name: &[u8]) -> bool {
        iter(buf).any(|(n, _)| n == name)
    }

    /// How many names the set holds.
    pub fn count(buf: &[u8]) -> usize {
        iter(buf).count()
    }

    /// The iterator [`iter`] returns.
    pub struct Names<'a> {
        buf: &'a [u8],
        at: usize,
    }

    impl<'a> Iterator for Names<'a> {
        type Item = (&'a [u8], bool);

        fn next(&mut self) -> Option<Self::Item> {
            let head = *self.buf.get(self.at)?;
            let len = (head & LEN) as usize;
            if len == 0 {
                return None;
            }
            let name = self.buf.get(self.at + 1..self.at + 1 + len)?;
            self.at += 1 + len;
            Some((name, head & IS_DIR != 0))
        }
    }
}

/// **Extended attributes: the limits, the encodings, and the store the FS server keeps them in**
/// (milestone 57, DECISIONS §34's 2026-07-31 amendment).
///
/// An extended attribute is a named byte string attached to a file or a directory. Samba stores
/// Apple's Time Machine metadata in them (`streams_xattr`), which is why they exist here at all, and
/// milestone 55's backup target is the deliverable on the other end.
///
/// # It is a layer, and the contract is why that is safe
///
/// RedoxFS has none of this. The decision was to build it **above** the engine rather than to fork
/// the on-disk format, and the argument that settled it was reversibility: this module is the
/// contract, so whether an attribute lives in a node's on-disk structure or in a store the server
/// manages is invisible to every client. Moving the implementation later changes nothing above this
/// line.
///
/// On Linux a layer like this would be worthless, because anything can open the file directly and
/// walk around it. **Here nothing can.** Every path to the disk goes through [`fs`], so a layer above
/// the filesystem is as authoritative as the filesystem.
///
/// # There is no attribute right, and that is deliberate
///
/// [`fs::GETXATTR`] and [`fs::LISTXATTR`] need [`dir::READ`]; [`fs::SETXATTR`] and
/// [`fs::REMOVEXATTR`] need [`dir::WRITE`]. No seventh rung was added to the ladder. An attribute is
/// part of what a file *is*, not a separate object with its own authority, so a capability that may
/// read a file may read what is attached to it and a capability that may not write it may not
/// change them. Adding a rung would also mean every existing grant silently gained or lost a right
/// depending on which way the default fell, which is the kind of change milestone 47's monotonicity
/// property exists to make impossible.
///
/// # The type code exists so indexing is not foreclosed
///
/// Every attribute carries a `u32` **kind**, and this layer never interprets it: it is stored,
/// returned, and otherwise ignored. [`xattr::RAW`] is what a POSIX-style client writes, and it is what
/// Samba will write.
///
/// The reason to carry a field nothing reads is `design/haiku-bfs-and-packages.md`. BFS made
/// attributes **typed and indexed**, with live queries over them, and that design's author went on to
/// build Spotlight; it is the ambitious version of this feature and it is a real destination. A store
/// of untyped blobs cannot be indexed later without a format migration and a wire break. A store that
/// round-trips a type code can. The cost of not foreclosing it is four bytes per record and one
/// packed word, paid now, once.
///
/// **BUGS.** The kind is 31 bits, not 32, because it rides in the sign-protected half of a reply word
/// (see [`xattr::reply`]). BFS-style four-character codes are ASCII and fit; a code with its top bit set does
/// not, and [`fs::SETXATTR`] answers `EINVAL` rather than truncating it.
pub mod xattr {
    /// The longest attribute **name**, in bytes. 255, which is Linux's `XATTR_NAME_MAX`, chosen so
    /// that nothing Samba writes is refused for its name: `user.org.netatalk.Metadata` and
    /// `user.com.apple.metadata:_kMDItemUserTags` are both well inside it, and a limit that refused
    /// a real client's name would be discovered on the board rather than here. It is also exactly
    /// what one length byte holds, which is what makes [`list`]'s records a byte cheaper than
    /// [`super::dirent`]'s.
    pub const MAX_NAME: usize = 255;

    /// The longest attribute **value**, in bytes.
    ///
    /// Three kibibytes, and the bound that sets it is the transport: [`fs::SETXATTR`] carries a name
    /// and a value in one [`super::PAGE`], so `MAX_NAME + MAX_VALUE` must fit 4096 with room that a
    /// reader can see is room. It is far above what the application needs (Apple's `AFP_AfpInfo` is
    /// 60 bytes, a `FinderInfo` blob is 32) and far below what Samba's `streams_xattr` can be asked
    /// to hold, because that feature stores whole alternate data streams as attributes.
    ///
    /// **Corrected 2026-08-18.** This said `AFP_AfpInfo` was 402 bytes, which conflated two
    /// different blobs. `AFP_INFO_SIZE` is `0x3c`, 60, in Samba's `source3/include/MacExtensions.h`,
    /// and Apple's own SMB client carries it as `uint8_t afpinfo[60]`. The 402 is the
    /// AppleDouble-shaped buffer on Samba's *netatalk* metadata path, `uint8_t ad_data[402]` in
    /// `vfs_fruit.c`. The sizing argument above survives the correction with more room, not less;
    /// see §99 for what does and does not fit here.
    ///
    /// **BUGS.** A resource fork larger than this is refused with [`E2BIG`], loudly, per DECISIONS
    /// §42: an attribute store that silently truncated a value would hand back a file that looked
    /// intact and was not. Lifting the limit means chunking the transfer across requests, which the
    /// contract does not do for anything today.
    pub const MAX_VALUE: usize = 3072;

    /// The most attributes one file or directory may carry.
    ///
    /// Sixteen, and it is the number that makes [`fs::LISTXATTR`] complete in one reply: sixteen
    /// names of [`MAX_NAME`] bytes plus their length prefixes is 4096 bytes, exactly one
    /// [`super::PAGE`]. A cursor would otherwise be needed, and a cursored listing can be observed
    /// half-changed, which is a worse property than a ceiling. Samba with `fruit` uses a handful per
    /// file.
    ///
    /// **BUGS.** The seventeenth attribute is [`ENOSPC`], not an eviction.
    pub const MAX_COUNT: usize = 16;

    /// The largest **kind** code, [`i32::MAX`] as a `u32`. See this module's header: the kind rides
    /// in a reply word whose sign bit means "error", so bit 31 is not available to it.
    pub const MAX_KIND: u32 = i32::MAX as u32;

    /// The kind a POSIX-style client writes: an uninterpreted byte string. Zero, so a client that
    /// knows nothing about kinds writes the right one by writing nothing.
    pub const RAW: u32 = 0;

    /// `ENODATA`: the named attribute is not set on this node. The only error that means absence,
    /// which is the discipline `ENOENT` earns in [`dir::explain`]: an instrument that reads any
    /// failure as "not there" reports filesystems that never existed (notes/fs-server.md).
    pub const ENODATA: i32 = 61;
    /// `ERANGE`: the name is empty, longer than [`MAX_NAME`], or holds a NUL. A fact about the name,
    /// not about the capability.
    pub const ERANGE: i32 = 34;
    /// `E2BIG`: the value is longer than [`MAX_VALUE`].
    pub const E2BIG: i32 = 7;
    /// `ENOSPC`: the node already holds [`MAX_COUNT`] attributes and this would be a new one.
    pub const ENOSPC: i32 = 28;
    /// `EOPNOTSUPP`: **this capability does not offer extended attributes at all.**
    ///
    /// DECISIONS §42's rule is that a backend which cannot meet a verb's guarantee does not offer
    /// the verb, and that the refusal is loud. The caretakers (`fs_file_caretaker`,
    /// `fs_subtree_caretaker`, `fs_nameset_caretaker`) answer this: they serve this same protocol
    /// over a narrowed namespace and do not forward attribute requests, so a program behind one
    /// learns that its capability does not carry attributes rather than that its request was
    /// malformed. `EINVAL` would have been the silent degradation, because it reads as "you sent
    /// nonsense" rather than "this does not do that".
    pub const ENOTSUP: i32 = 95;

    /// **The reserved name the attribute store lives under**, in the image root.
    ///
    /// It is refused by every name-taking verb and skipped by [`fs::READDIR`], in **every**
    /// directory rather than only in the root, so the rule a client has to remember is one sentence
    /// instead of a location-dependent one. A store a client could name would be part of the
    /// namespace, and then "the attributes of a file" would be reachable as ordinary bytes by
    /// anything holding the directory.
    ///
    /// **BUGS.** You cannot create a file called `.nife-attrs` anywhere on a nife
    /// filesystem, at any rights. The refusal is `EINVAL`, the same one `..` gets, because the name
    /// is not expressible rather than not permitted.
    ///
    /// Recovery **does** see it. `redoxfs_host extract` and upstream's FUSE mount show this
    /// directory as ordinary data, so a backup carries the attributes out with the files. That is a
    /// decision and not a leak (notes/host-recovery.md); the store is unnameable through the
    /// *contract*, and the contract is what confines a client.
    pub const STORE_DIR: &str = ".nife-attrs";

    /// Pack [`fs::SETXATTR`]'s second word: the value's type code in the high 32 bits, its length in
    /// the low 32. The name's length rides in the request word's own length field, as every other
    /// verb's payload length does, so the two lengths never share a field.
    pub const fn spec(kind: u32, value_len: u64) -> u64 {
        ((kind as u64) << 32) | (value_len & 0xffff_ffff)
    }
    /// The type code from a [`fs::SETXATTR`] second word.
    pub const fn spec_kind(w1: u64) -> u32 {
        (w1 >> 32) as u32
    }
    /// The value length from a [`fs::SETXATTR`] second word.
    pub const fn spec_value_len(w1: u64) -> usize {
        (w1 & 0xffff_ffff) as usize
    }

    /// Pack [`fs::GETXATTR`]'s reply: the kind in the high bits, the value's length in the low 32.
    ///
    /// **It is an [`i64`] and it must stay non-negative**, because this whole protocol's error
    /// boundary is "negative is a negated errno" ([`super::reply_err`]). That is what costs the kind
    /// its 32nd bit, and it is the right trade: a separate reply word does not exist, and a header
    /// in front of the value would make every client copy the value twice.
    pub const fn reply(kind: u32, value_len: usize) -> i64 {
        (((kind & MAX_KIND) as i64) << 32) | (value_len as i64 & 0xffff_ffff)
    }
    /// The kind of a [`fs::GETXATTR`] reply. Only meaningful when the reply is non-negative.
    pub const fn reply_kind(r0: i64) -> u32 {
        ((r0 >> 32) as u32) & MAX_KIND
    }
    /// The value's length, in bytes, of a [`fs::GETXATTR`] reply.
    pub const fn reply_value_len(r0: i64) -> usize {
        (r0 as u64 & 0xffff_ffff) as usize
    }

    /// Whether `name` is a name this store can hold: non-empty, at most [`MAX_NAME`] bytes, and free
    /// of NUL.
    ///
    /// **Bytes, not UTF-8, and no namespace prefix is required.** Linux insists on
    /// `user.`/`security.`/`trusted.` because those namespaces mean different privilege; there is no
    /// privilege here to mean, so requiring the prefix would be inventing a policy to enforce. NUL is
    /// the one byte refused, because every client that will ever write one of these names (Samba, and
    /// anything speaking the POSIX API) hands it over as a C string, and a name with a NUL in it
    /// would come back shorter than it went out.
    pub const fn valid_name(name: &[u8]) -> bool {
        if name.is_empty() || name.len() > MAX_NAME {
            return false;
        }
        let mut i = 0;
        while i < name.len() {
            if name[i] == 0 {
                return false;
            }
            i += 1;
        }
        true
    }

    /// **How [`fs::LISTXATTR`] packs names into the shared page**: one length byte, then that many
    /// bytes of name, repeated. No flags byte and no type code, because a listing answers "what is
    /// attached" and [`fs::GETXATTR`] answers "what is it"; carrying the kind here would double the
    /// record header to save a round trip nobody has asked for.
    ///
    /// The reply's `r0` is how many bytes were filled, so a client iterates until it has consumed
    /// exactly that many, exactly as [`super::dirent`] works.
    pub mod list {
        /// Bytes one record takes: one for the length, plus the name.
        pub const fn record_len(name_len: usize) -> usize {
            1 + name_len
        }

        /// The most bytes a complete listing can occupy: every attribute a node may hold, at the
        /// longest name each. Equal to [`super::super::PAGE`], which is what lets
        /// [`super::super::fs::LISTXATTR`] answer without a cursor.
        pub const MAX_BYTES: usize = super::MAX_COUNT * record_len(super::MAX_NAME);

        /// Write one record at the start of `out`, returning its length, or `None` if it does not
        /// fit or the name is not one this store holds.
        pub fn encode(out: &mut [u8], name: &[u8]) -> Option<usize> {
            let n = record_len(name.len());
            if !super::valid_name(name) || out.len() < n {
                return None;
            }
            out[0] = name.len() as u8;
            out[1..n].copy_from_slice(name);
            Some(n)
        }

        /// Walk the records in `buf` (exactly the `r0` bytes a [`super::super::fs::LISTXATTR`] reply
        /// filled). Stops at a zero length byte and at the first record that runs off the end, which
        /// is what a truncated reply looks like and must not be read as a name.
        pub fn iter(buf: &[u8]) -> Names<'_> {
            Names { buf, at: 0 }
        }

        /// The iterator [`iter`] returns.
        pub struct Names<'a> {
            buf: &'a [u8],
            at: usize,
        }

        impl<'a> Iterator for Names<'a> {
            type Item = &'a [u8];

            fn next(&mut self) -> Option<Self::Item> {
                let len = *self.buf.get(self.at)? as usize;
                if len == 0 {
                    return None;
                }
                let name = self.buf.get(self.at + 1..self.at + 1 + len)?;
                self.at += record_len(len);
                Some(name)
            }
        }
    }

    /// **The store's on-disk record format, and the whole of the layer's semantics as pure
    /// functions.**
    ///
    /// One node's attributes are one blob: a sequence of records, each a [`store::RECORD_HEADER`]-byte
    /// header (a name length, a `u32` kind, a `u16` value length, all little-endian) followed by the
    /// name and then the value. The FS server keeps one such blob per node in a file named for the
    /// node's `TreePtr` id ([`store::file_name`]) inside [`STORE_DIR`].
    ///
    /// **Why the format is published here rather than hidden in the server.** Two reasons, and the
    /// first is the honest one: every interesting rule of this feature (what replaces what, when a
    /// limit is hit, what a truncated blob means) lives in [`store::set`], [`store::remove`] and [`store::get`], which are
    /// pure functions over a byte slice and therefore host-tested in milliseconds instead of under an
    /// emulator. The second is recovery: a person holding a damaged image and the `redoxfs_host`
    /// tool can read the store, because the format is written down next to the code that makes it.
    ///
    /// **Keyed on the node, which is what makes rename free.** A rename changes a directory entry and
    /// not the node, so attributes follow a file across one without any code knowing a rename
    /// happened. AppleDouble sidecars get exactly this wrong, and so would any path-keyed store; the
    /// correctness is only available inside the FS server, which is an argument *for* the layer
    /// rather than a consolation for it.
    pub mod store {
        use super::{E2BIG, ENODATA, ENOSPC, ERANGE, MAX_COUNT, MAX_KIND, MAX_VALUE, valid_name};

        /// A record's fixed header: one byte of name length, four of kind, two of value length.
        pub const RECORD_HEADER: usize = 1 + 4 + 2;

        /// Bytes one record occupies.
        pub const fn record_len(name_len: usize, value_len: usize) -> usize {
            RECORD_HEADER + name_len + value_len
        }

        /// The largest a node's whole blob can be: [`MAX_COUNT`] records at the widest each. This is
        /// the ceiling on what one node costs the store, and the reason the count limit exists at
        /// all rather than being left to the disk.
        pub const MAX_BYTES: usize = MAX_COUNT * record_len(super::MAX_NAME, MAX_VALUE);

        /// The name of the file that holds a node's blob: its `TreePtr` id in lowercase hex, always
        /// eight characters, so the store directory's entries sort and read predictably in a
        /// recovery listing.
        pub const fn file_name(node_id: u32) -> [u8; 8] {
            let mut out = [b'0'; 8];
            let mut i = 0;
            while i < 8 {
                let nibble = ((node_id >> (28 - 4 * i)) & 0xf) as u8;
                out[i] = if nibble < 10 {
                    b'0' + nibble
                } else {
                    b'a' + nibble - 10
                };
                i += 1;
            }
            out
        }

        /// Walk a blob's records: `(name, kind, value)` each.
        ///
        /// Stops at a zero name length and at the first record that runs off the end. Both are what a
        /// **truncated or zero-padded** blob looks like, and neither may be read as an attribute: a
        /// torn write that left half a record behind must produce fewer attributes, never a value
        /// with somebody else's bytes on the end of it.
        pub fn iter(blob: &[u8]) -> Records<'_> {
            Records { blob, at: 0 }
        }

        /// How many attributes a blob holds.
        pub fn count(blob: &[u8]) -> usize {
            iter(blob).count()
        }

        /// The value and kind of one attribute, or `None` if it is not set.
        pub fn get<'a>(blob: &'a [u8], name: &[u8]) -> Option<(u32, &'a [u8])> {
            iter(blob)
                .find(|(n, ..)| *n == name)
                .map(|(_, k, v)| (k, v))
        }

        /// Write the [`super::list`] records for every attribute in `blob` into `out`, returning the
        /// bytes used. A complete listing always fits [`super::list::MAX_BYTES`]; a shorter `out`
        /// stops before the record that will not fit rather than splitting one.
        pub fn list(blob: &[u8], out: &mut [u8]) -> usize {
            let mut used = 0;
            for (name, ..) in iter(blob) {
                match super::list::encode(&mut out[used..], name) {
                    Some(n) => used += n,
                    None => break,
                }
            }
            used
        }

        /// **Set `name` to `value`, producing the new blob in `out` and returning its length.**
        ///
        /// An existing attribute is replaced **in place**, so the order a client sees from
        /// [`super::super::fs::LISTXATTR`] does not shuffle when a value changes; a new one is
        /// appended. Create and replace are one operation, for the reason
        /// [`super::super::fs::SETXATTR`] gives.
        ///
        /// `out` must hold `blob.len() + record_len(name.len(), value.len())`, which is always
        /// enough because replacing can only shrink the result. A shorter one is [`ENOSPC`], which a
        /// correctly sized caller never sees.
        ///
        /// Errors are errnos rather than a bespoke type, because the FS server's rule is that
        /// nothing below the serve loop speaks the wire's vocabulary and everything speaks the
        /// engine's.
        pub fn set(
            blob: &[u8],
            name: &[u8],
            kind: u32,
            value: &[u8],
            out: &mut [u8],
        ) -> Result<usize, i32> {
            if !valid_name(name) {
                return Err(ERANGE);
            }
            if value.len() > MAX_VALUE {
                return Err(E2BIG);
            }
            if kind > MAX_KIND {
                return Err(syscall_einval());
            }
            let mut used = 0;
            let mut replaced = false;
            for (n, k, v) in iter(blob) {
                let (n, k, v) = if n == name {
                    replaced = true;
                    (name, kind, value)
                } else {
                    (n, k, v)
                };
                used += write_record(&mut out[used..], n, k, v).ok_or(ENOSPC)?;
            }
            if !replaced {
                if count(blob) >= MAX_COUNT {
                    return Err(ENOSPC);
                }
                used += write_record(&mut out[used..], name, kind, value).ok_or(ENOSPC)?;
            }
            Ok(used)
        }

        /// **Remove `name`, producing the new blob in `out` and returning its length.** [`ENODATA`]
        /// if it was not set: the caller asked about one specific thing, so "it was not there" is the
        /// answer rather than a silent success. `out` must be at least `blob.len()`.
        pub fn remove(blob: &[u8], name: &[u8], out: &mut [u8]) -> Result<usize, i32> {
            if !valid_name(name) {
                return Err(ERANGE);
            }
            let mut used = 0;
            let mut found = false;
            for (n, k, v) in iter(blob) {
                if n == name {
                    found = true;
                    continue;
                }
                used += write_record(&mut out[used..], n, k, v).ok_or(ENOSPC)?;
            }
            if found { Ok(used) } else { Err(ENODATA) }
        }

        /// `EINVAL`, spelled here so this module needs no dependency on the error crate. The kind
        /// field is the only thing that answers it: a code with bit 31 set cannot ride home in a
        /// reply word, and truncating it would hand back a type that was never written.
        const fn syscall_einval() -> i32 {
            22
        }

        /// Lay one record at the start of `out`; `None` if it does not fit.
        fn write_record(out: &mut [u8], name: &[u8], kind: u32, value: &[u8]) -> Option<usize> {
            let n = record_len(name.len(), value.len());
            if out.len() < n || name.len() > super::MAX_NAME || value.len() > MAX_VALUE {
                return None;
            }
            out[0] = name.len() as u8;
            out[1..5].copy_from_slice(&kind.to_le_bytes());
            out[5..7].copy_from_slice(&(value.len() as u16).to_le_bytes());
            out[RECORD_HEADER..RECORD_HEADER + name.len()].copy_from_slice(name);
            out[RECORD_HEADER + name.len()..n].copy_from_slice(value);
            Some(n)
        }

        /// The iterator [`iter`] returns.
        pub struct Records<'a> {
            blob: &'a [u8],
            at: usize,
        }

        impl<'a> Iterator for Records<'a> {
            type Item = (&'a [u8], u32, &'a [u8]);

            fn next(&mut self) -> Option<Self::Item> {
                let head = self.blob.get(self.at..self.at + RECORD_HEADER)?;
                let name_len = head[0] as usize;
                if name_len == 0 {
                    return None;
                }
                let kind = u32::from_le_bytes([head[1], head[2], head[3], head[4]]);
                let value_len = u16::from_le_bytes([head[5], head[6]]) as usize;
                let body = self.at + RECORD_HEADER;
                let name = self.blob.get(body..body + name_len)?;
                let value = self
                    .blob
                    .get(body + name_len..body + name_len + value_len)?;
                self.at += record_len(name_len, value_len);
                Some((name, kind, value))
            }
        }
    }

    use super::{dir, fs};

    /// A [`fs::SETXATTR`] carries a name and a value in one page, so the two limits together must
    /// leave the transport room. Checked at compile time because both sides are constants: a change
    /// that broke it should fail the build rather than wait for a request that happens to be wide.
    const _: () = assert!(MAX_NAME + MAX_VALUE <= super::PAGE);
    /// And a complete listing must fit one page, which is what lets [`fs::LISTXATTR`] have no cursor.
    const _: () = assert!(list::MAX_BYTES <= super::PAGE);
    /// The four verbs must not collide with the thirteen that were already on the wire.
    const _: () = assert!(fs::GETXATTR > fs::RMDIR && fs::REMOVEXATTR <= 0xff);
    /// The rights these verbs consult are the ones [`fs::READ`] and [`fs::WRITE`] consult; nothing
    /// here invents a rung, so a mask this module needs is a mask the ladder already defines.
    const _: () = assert!((dir::READ | dir::WRITE) & !dir::ALL == 0);
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
    pub const SCRATCH_INIT: &[u8] = b"(placeholder overwritten by the fs_server write test)";
    /// What the client writes to `scratch` and reads back; the host tool re-reads it after the run
    /// to prove the write reached the on-disk image and the filesystem is still consistent.
    pub const WRITE_PATTERN: &[u8] =
        b"CRKFS_WRITE_OK: this round-tripped through the RedoxFS image\n";

    /// The client's success sentinel, sent as the report's second word; the head of [`MOTD`] is the
    /// first. Any other value (or silence) fails the test.
    pub const SUCCESS: u64 = 0xF11E_600D;

    /// **The file the SMB gate reads off the real filesystem** (milestone 54). The combined
    /// inbound boot creates it through the FS server (`fs_test_client`'s seed role) *before* the
    /// SMB adapter serves, and xtask's SMB prober then opens it by this name over the wire and
    /// asserts these exact bytes came back, which is what proves the served bytes crossed
    /// RedoxFS -> `fs_proto` -> the `Share` seam -> SMB2 -> TCP rather than a fixture baked into
    /// the server binary. One constant, three readers (the seeding client, the kernel test, the
    /// host prober), zero second copies of the expected contents.
    ///
    /// Lower-case on purpose: the SMB server folds wire names to lower-case ASCII before lookup
    /// (`smb_proto`'s BUGS), so an upper-case name here would be unreachable over the mount.
    pub const SMB_SEED_NAME: &str = "smb_seed.txt";
    /// Its exact contents. What the seed role writes and the SMB prober must read back.
    pub const SMB_SEED: &[u8] =
        b"these bytes crossed RedoxFS, fs_proto, and SMB2 on their way to you\n";

    /// **The file the SMB gate writes, in the other direction** (milestone 54's write path). The
    /// host's SMB prober creates it over the wire, writes [`SMB_WROTE`] plus a tail, shortens it
    /// with `SET_INFO`, and closes; a *different* in-guest process (`fs_test_client`'s verify
    /// role, holding a directory capability and nothing that names the network) then reads it
    /// back through the FS server and reports what it found.
    ///
    /// Two readers, one constant, no second copy of the expectation, exactly as [`SMB_SEED`]
    /// does for the read direction. Lower-case for the same reason: the wire folds names to
    /// lower-case ASCII before lookup.
    pub const SMB_WROTE_NAME: &str = "smb_wrote.txt";
    /// Its exact contents after the prober has written and shortened it.
    pub const SMB_WROTE: &[u8] =
        b"these bytes crossed SMB2, fs_proto, and RedoxFS on their way in\n";

    /// **The directory the SMB gate makes** (milestone 54's subdirectory half). The host's prober
    /// creates it over the wire with `FILE_DIRECTORY_FILE`, puts [`SMB_NESTED`] inside it, and the
    /// same in-guest verify role checks that a *directory* is what landed.
    ///
    /// That last check is the one worth having: a share that ignored the separator would create a
    /// file literally called `tm_bands\band0`, which reads back as a missing directory and a
    /// missing file, and is exactly what the flat share did. `.sparsebundle` is not in the name on
    /// purpose, because nothing here is Time Machine specific yet; the *shape* is what milestone 55
    /// needs.
    pub const SMB_DIR_NAME: &str = "tm_bands";
    /// The file the gate writes inside [`SMB_DIR_NAME`], by its name alone: the verify role opens
    /// the directory and then the name under it, which is the path walk done from the other side.
    pub const SMB_NESTED_NAME: &str = "band0";
    /// Its exact contents. Distinct from [`SMB_WROTE`] so that a share which wrote both files to
    /// the same place cannot pass by accident.
    pub const SMB_NESTED: &[u8] = b"a band file, written one directory down over SMB2\n";

    /// **What the verify role found**, sent as the report's second word. A classification rather
    /// than a pass/fail, in [`escape`]'s spirit and for the same reason: "the file is not there"
    /// and "the file is there and wrong" are different bugs, and a bare failure names neither.
    pub mod smb_wrote {
        /// The file holds exactly [`super::SMB_WROTE`]. The only passing verdict.
        pub const EXACT: u64 = 1;
        /// No such name: the write never reached the filesystem at all.
        pub const ABSENT: u64 = 2;
        /// The name is there and the length is wrong, which points at the truncate leg
        /// (`SET_INFO` / `FileEndOfFileInformation`) rather than at the write.
        pub const WRONG_SIZE: u64 = 3;
        /// The right length and the wrong bytes: an offset or a chunking bug in the write path.
        pub const WRONG_BYTES: u64 = 4;
        /// **The subdirectory is not there** ([`super::SMB_DIR_NAME`]), so the client's `mkdir`
        /// never reached the filesystem. Its own verdict rather than folded into [`ABSENT`],
        /// because "the directory was not made" and "the directory was made and the file in it was
        /// not" are different bugs in different halves of the path walk.
        pub const DIR_ABSENT: u64 = 5;
        /// The subdirectory is there and is not a directory, which means the adapter created a
        /// *file* whose name happens to contain a separator: the exact failure a path-aware share
        /// exists to prevent, and the one a flat share would produce.
        pub const DIR_IS_A_FILE: u64 = 6;
        /// The directory is there and the file inside it is not.
        pub const NESTED_ABSENT: u64 = 7;
        /// The nested file is there and holds the wrong bytes.
        pub const NESTED_WRONG: u64 = 8;
    }

    /// **What the durability witness found** (milestone 55), sent as the report's third word by the
    /// same in-guest role that reports [`smb_wrote`] in the second.
    ///
    /// It is a classification for [`smb_wrote`]'s reason, and the classes here are further apart
    /// than that module's: "this machine's device cannot flush" and "the flush never left the SMB
    /// server" are a hardware fact and a wiring bug, and a run that confused them would send
    /// somebody chasing the wrong half of the stack.
    ///
    /// The witness asks [`crate::fs::SYNC`] twice and reads the count each answer carries.
    /// **Two independent things are being checked**, which is why one verdict is not enough:
    ///
    /// 1. The count was **already non-zero** when the witness first asked. Nothing in this process
    ///    had synced yet, so the only thing that can have moved it is the SMB server answering the
    ///    host prober's `FLUSH` over TCP. That is the end-to-end leg, and it is the one that cannot
    ///    be faked from inside the guest.
    /// 2. The count **strictly increased** between the two calls. That is what separates a real
    ///    device round trip from a server that answers a constant, and it is the check that would
    ///    have caught the gap this milestone exists to close.
    pub mod durability {
        /// Both checks passed: something above this process had already flushed the device, and
        /// this process's own sync moved the count again. The only passing verdict.
        pub const DURABLE: u64 = 1;
        /// The count was zero when the witness first asked, so **no flush ever reached the
        /// device** before it ran. The SMB `FLUSH` command is not reaching `fs_proto::fs::SYNC`,
        /// which is precisely the state milestone 55 shipped `VOLUME_FULL_SYNC` in.
        pub const NEVER_FLUSHED: u64 = 2;
        /// [`crate::fs::SYNC`] answered `EOPNOTSUPP`: this device offers no
        /// `VIRTIO_BLK_F_FLUSH`, so the claim cannot be backed here at all. **Not a bug in this
        /// tree**, and it must not be reported as one; it is the honest answer for a device that
        /// does not do the thing, and a run that met it should say so and stop rather than pass.
        pub const NO_DEVICE_FLUSH: u64 = 3;
        /// The first sync was refused for some other reason; the errno rides in the report's
        /// remaining room where a caller has one, and a bare occurrence of this verdict means the
        /// verb is wired wrong rather than unsupported.
        pub const REFUSED: u64 = 4;
        /// Two syncs, the same count: the second one did not reach the device. A server answering
        /// a cached or constant yes lands exactly here, which is the failure this witness is
        /// shaped around.
        pub const NOT_ADVANCING: u64 = 5;
    }

    /// **The file milestone 38's throughput benchmark writes and reads back.** Not shipped in the
    /// image: the throughput role creates it (or truncates it, if a previous run left it there),
    /// which is deliberate rather than incidental, because the sequential-write phase *is* the
    /// creation and a file the image already carried would have been laid out by the host tool
    /// rather than by the server under test.
    ///
    /// **Provisional name** (2026-08-18, milestone 38): calef names files a reader meets, and this
    /// one is met by anyone who lists the bench image after a run.
    pub const THROUGHPUT_NAME: &str = "throughput";

    /// **The shape of milestone 38's throughput measurement**, shared by the client that performs
    /// it (`user/src/fs_test_client.rs`, the throughput role) and the bench boot that names its
    /// results (`kernel/src/bench.rs`). Two programs agree on these, so they are a crate and not a
    /// constant written twice (CLAUDE.md rule 7).
    pub mod throughput {
        /// **The transfer unit: the whole file channel, which is the most a single `fs_proto`
        /// request can carry.** Still not a tuning choice *here*: it is
        /// [`super::super::fs::TRANSFER_MAX`], so this phase always measures the protocol's own
        /// ceiling and the tuning lives at the one constant that sets that ceiling.
        ///
        /// It was 4096 until milestone 138 step 3, because the region a request moved bytes
        /// through was one page. A throughput figure here is therefore still "how fast can this
        /// architecture move one request's worth", and what changed is how much that is. A
        /// comparison against a system whose client may pass a differently sized buffer is
        /// comparing two different things, and **that caveat did not go away when the number got
        /// bigger**; see notes/benchmarks.md.
        pub const UNIT: usize = super::super::fs::TRANSFER_MAX;

        /// **Bytes moved per phase, held constant across transfer sizes on purpose.** 1 MiB, which
        /// is small for a throughput benchmark and is bounded by the fixture image (16 MiB) rather
        /// than chosen for statistics: the write phases have to fit beside everything else the
        /// image carries, and RedoxFS is copy-on-write, so a rewritten block consumes fresh space
        /// until the old one is freed.
        ///
        /// It is the **bytes** that are pinned rather than the transfer count, because milestone
        /// 138 step 3 made the transfer size a variable and a benchmark that kept the count would
        /// have moved sixteen times as much data at 64 KiB as at 4 KiB. Two runs of this phase move
        /// the same file however the requests are shaped, which is what makes a before and an after
        /// comparable.
        pub const TOTAL: u64 = 1024 * 1024;

        /// Transfers per phase: [`TOTAL`] divided by [`UNIT`]. 256 at one page, 16 at sixteen.
        pub const BLOCKS: u64 = TOTAL / UNIT as u64;

        /// **The phase moves exactly [`TOTAL`] bytes at every transfer size**, which is what makes a
        /// before and an after on `bench/transfer-size-sweep.sh` a comparison rather than two
        /// numbers, and what the sequential phase's `FSTAT` check against the file size depends on.
        /// A build is where this has to hold, because the phase runs at EL0 where no test does.
        const _: () = {
            assert!(BLOCKS >= 1);
            assert!(BLOCKS * UNIT as u64 == TOTAL);
            assert!(TOTAL.is_multiple_of(RECORD));
        };

        /// Untimed transfers before each phase: the `OPEN`, the first cold block, and whatever the
        /// engine touches on its first walk of the file.
        pub const WARMUP: u64 = 8;

        /// Phase tags, sent as the report's third word so the bench boot names a result rather than
        /// counting messages. The order they are performed in is the order they are listed, and it
        /// is forced: the write phase is what creates the file the read phases read.
        pub const SEQ_WRITE: u64 = 1;
        /// Sequential read of the file the write phase left behind.
        pub const SEQ_READ: u64 = 2;
        /// Read at page-aligned offsets drawn from a fixed-seed PRNG, so a run is reproducible.
        pub const RAND_READ: u64 = 3;
        /// Write at those same pseudo-random offsets, in place, over a file that already exists.
        pub const RAND_WRITE: u64 = 4;
        /// **A read of the first page of a record.** It was added to confirm a model of where the
        /// milliseconds go, and it refuted it, which is why it is still here.
        ///
        /// The prediction was that a read at the *start* of a record would be much cheaper than one
        /// in the middle: `read_node_inner` asks for the record at
        /// `BlockLevel::for_bytes(offset_within_record + len)`, which is level 0 at the start and
        /// level 5 at the end. **It measures the same as the other two read phases, to within 3%.**
        /// The reason is one level down: `read_record` reads the block the pointer *stores*, and
        /// only then checks whether that is as large as the level asked for. A fully written record
        /// is stored at its node's record level, so **a read of an ordinary file fetches the whole
        /// record**, whatever offset it asked for.
        ///
        /// That is a stronger result than the one it replaced, and it is what made the arithmetic in
        /// notes/benchmarks.md close: at milestone 38's record level a 4 KiB read was 32 block
        /// reads, flat. **Milestone 138 step 1 took the record level to 1**, so it is now two, and
        /// the mechanism this phase measures is unchanged: the whole record still comes back.
        pub const RECORD_READ: u64 = 6;

        /// **A record boundary**, which is the property [`RECORD_READ`] needs and the only one:
        /// the phase reads at multiples of this, and it is meaningful exactly when every multiple
        /// of it is also a multiple of the store's record size.
        ///
        /// 128 KiB satisfies that at every RedoxFS record level from 0 to 5, because each of those
        /// sizes divides it. That is why the value did not move when milestone 138 took the record
        /// level from 5 to 1: keeping it makes every figure this phase has ever produced comparable
        /// with every other, which a value tracking the record size would have destroyed.
        ///
        /// **This is the store's number, not the protocol's**, and it used to say so and then add
        /// that nothing checked it, calling itself "the one soft spot in this module". It is checked
        /// now: `fs_server` sees both this crate and the vendored engine, and asserts at compile
        /// time that this value is a whole number of records. See `fs_server/src/lib.rs`.
        pub const RECORD: u64 = 128 * 1024;

        /// **The cost of producing one page of payload**, with no filesystem in it at all.
        ///
        /// It is measured and reported because the two write phases pay it inside their timed
        /// window and cannot avoid it. RedoxFS compresses a record with lz4 and skips a write whose
        /// record is byte-identical to what is already there, so a benchmark that sends the same
        /// page twice measures the short circuit and one that sends 32 identical pages into one
        /// 128 KiB record measures lz4. The payload therefore has to be fresh and incompressible
        /// per write, which costs 512 stores, and this row is how much. Subtract it to get the
        /// filesystem's own share.
        pub const PAYLOAD_FILL: u64 = 5;
        /// How many phase reports the bench boot waits for.
        pub const PHASES: usize = 6;

        /// **The client role the bench boot spawns**, which is the one thing here that two
        /// *binaries* have to agree on rather than two modules: the kernel passes it to
        /// `fs_service::start` and `fs_test_client` matches on it. The other eight roles are
        /// literals with comments on both sides, which is what this one would have been.
        pub const ROLE: u64 = 9;

        /// The name of a phase tag, for the line the bench boot prints. `None` for anything else,
        /// which is a client that reported a tag this table does not know.
        pub const fn name(tag: u64) -> Option<&'static str> {
            match tag {
                SEQ_WRITE => Some("fs_seq_write"),
                SEQ_READ => Some("fs_seq_read"),
                RAND_READ => Some("fs_rand_read"),
                RAND_WRITE => Some("fs_rand_write"),
                RECORD_READ => Some("fs_record_read"),
                PAYLOAD_FILL => Some("fs_payload_fill"),
                _ => None,
            }
        }
    }

    /// The FS server's readiness sentinel: sent once, after it has opened the RedoxFS image over blk
    /// IPC and before it serves clients. The test waits for it, so a hang in `open` (the blk path)
    /// is distinguishable from a hang in the serve/client path, and a booted-but-empty run is caught.
    pub const READY: u64 = 0xF5_0BEEF5;

    /// **A caretaker's descent was refused**, sent on the same endpoint [`READY`] would have gone
    /// out on, with the errno in the second word. The caretaker then exits without serving.
    ///
    /// It exists because of who waits for the handshake. Under `kernel::user::fs_service` the waiter
    /// is a test, and a caretaker that panicked instead of answering cost that test a 60-second
    /// watchdog and a legible failure. Under `system_initializer` the waiter is **init**, which is
    /// the spawn service for the whole prompt and has no other thread: a caretaker that panicked
    /// there would park init in `RECV` forever and the machine would never take another command.
    /// A grant naming a directory that is not there is an ordinary thing to type, so the honest
    /// answer has to be a message rather than a corpse.
    ///
    /// **Provisional name** (2026-08-17, milestone 31 phase 3): it names the one step that can fail
    /// before a caretaker serves anything, which is the single `OPENDIR` its whole attenuation lives
    /// in.
    pub const DESCENT_REFUSED: u64 = 0xF5_0DEAD5;

    /// **The attacker's report: a bitmap of what got through**, not a pass/fail. Each bit says one
    /// specific thing happened, so the test asserts an *expected set* rather than "zero", and a
    /// failure names itself. That shape is what lets one attacker serve as its own negative control:
    /// run against a read-only grant every bit must be clear, and run against a read/write grant of
    /// the same shape the two write bits must be **set**, which is what proves the read-only
    /// refusals were a narrowed capability rather than a caretaker that refuses everything.
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
        /// **Its extended-attribute write was accepted.** Expected only with `grant::WRITE`, and it
        /// is `WROTE`'s twin for the same reason `TRUNCATED` is: an attribute is a way to change a
        /// file that carries no file bytes, so a direction check that only covered `WRITE` and
        /// `TRUNCATE` would leave one open. Added by milestone 61 with the forwarding it guards.
        pub const WROTE_ATTR: u64 = 1 << 6;
        /// **Reading its own file's attributes failed**, so the capability does not carry what
        /// milestone 61 says it carries. A control bit in [`GRANTED_READ_FAILED`]'s shape rather
        /// than an escape, and the one that catches a regression to the old behaviour: before
        /// milestone 61 the caretaker answered `EOPNOTSUPP` to all four attribute verbs, so this
        /// bit would be set in **both** runs.
        pub const GRANTED_ATTRS_FAILED: u64 = 1 << 7;
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
        /// [`INNER`] and [`SECRET`] **as absolute paths**, which the navigating witness types to
        /// ask the same two questions with a leading slash (milestone 47's namespace half).
        ///
        /// Constants rather than bytes built at the call site because the witness runs on two
        /// stack pages and has been one page short before now: a static string costs it nothing.
        pub const ABS_INNER: &str = "/inner";
        /// [`SECRET`] as an absolute path. See [`ABS_INNER`].
        pub const ABS_SECRET: &str = "/secret";
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

        /// **The directory milestone 50's redirection witness works in**, a sibling of [`SUB`] for
        /// the same reason that one has siblings: a shell that writes files needs somewhere its
        /// writes cannot be mistaken for another test's, and the root is shared with every test in
        /// the boot.
        ///
        /// ```text
        ///   /redir      alpha  beta        <- the redirection witness is granted this
        /// ```
        ///
        /// Two files rather than none, so `ls > out.txt` writes a listing with something in it and
        /// the `wc` that counts it is counting more than the name of its own output file.
        pub const REDIR: &str = "redir";
        pub const REDIR_ONE: &str = "alpha";
        pub const REDIR_TWO: &str = "beta";
        pub const REDIR_BODY: &[u8] = b"CRK50: a file the redirection witness lists\n";

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
        /// A plain file beside the doomed tree, removed by a plain `rm` with no options. It is what
        /// makes **silence on success** falsifiable: that run must report a zero status and print
        /// nothing at all, and `rm(1)`'s `-v` exists precisely because the default prints nothing.
        pub const RM_SOLO: &str = "rm-solo";
        /// A name that is **not** on the image. `rm` of it is a diagnostic and a non-zero exit;
        /// `rm -f` of it is silence and a zero one, which is the whole of what `-f` means.
        pub const RM_MISSING: &str = "rm-nothing";

        /// **The directory the globbing lane's set grant is planned in**, a sibling of [`RMTREE`]
        /// so a capability to one is provably not a capability to the other.
        ///
        /// ```text
        ///   /globset            gl-one.txt  gl-two.txt  gl-three.log  gl-dir/
        ///   /globset/gl-dir     gl-inner
        /// ```
        ///
        /// [`GLOB_PATTERN`] matches exactly [`GLOB_ONE`] and [`GLOB_TWO`]. The other two are the
        /// controls, and they are two different controls: [`GLOB_MISS`] is a **file that exists,
        /// sits one directory entry away, and is not in the set**, which is what a set-attenuated
        /// capability must not be able to name; [`GLOB_DIR`] is a directory, so the type bit
        /// [`super::super::nameset::IS_DIR`] carries has something to be wrong about.
        pub const GLOBSET: &str = "globset";
        /// The pattern the shell expands, and the whole of what `rm *.txt` designates here. Pinned
        /// against the matcher by a host test, so the set the kernel test states literally is
        /// provably the expansion rather than a second opinion about it.
        pub const GLOB_PATTERN: &[u8] = b"gl-*.txt";
        pub const GLOB_ONE: &str = "gl-one.txt";
        pub const GLOB_TWO: &str = "gl-two.txt";
        /// A file in the same directory that the pattern does **not** match. It is the escape the
        /// nameset caretaker is attacked with: it exists, and the caretaker one hop up could remove
        /// it.
        pub const GLOB_MISS: &str = "gl-three.log";
        /// A directory in the same directory, not matched either. It is why a set record carries a
        /// type bit at all.
        pub const GLOB_DIR: &str = "gl-dir";
        /// A file inside [`GLOB_DIR`], so the unmatched directory is not empty and a run that
        /// removed it would have had to walk it.
        pub const GLOB_INNER: &str = "gl-inner";
        pub const GLOB_BODY: &[u8] = b"CRK47-GLOB: a name a pattern either matched or did not\n";
        /// A pattern that matches **nothing** here, so the empty-expansion refusal has something to
        /// be about. Same prefix as the others deliberately: it fails on the extension alone, which
        /// is the case a matcher that gave up early would get wrong.
        pub const GLOB_NOMATCH: &[u8] = b"gl-*.rs";
        /// A line with no pattern in it, echoed to check that globbing did not quietly turn `echo`
        /// into a word-splitter. Its two consecutive spaces are the whole point.
        pub const GLOB_PLAIN: &[u8] = b"two  spaces";

        /// **Milestone 109's batching tree**: a directory holding more matching names than one
        /// grant can carry, so `xargs` has something at the prompt to be about.
        ///
        /// A sibling of [`GLOBSET`] rather than an extension of it, deliberately: the globbing
        /// lane's tests assert that `gl-*.txt` matches exactly two names, and adding files there to
        /// make this one's point would make that one's point by accident.
        pub const GLOBMANY: &str = "globmany";
        /// The pattern, matching every one of [`MANY_NAMES`] and not [`MANY_MISS`]. Pinned against
        /// the matcher by a host test, the way [`GLOB_PATTERN`] is.
        pub const MANY_PATTERN: &[u8] = b"m-*.txt";
        /// **Eleven names: one full batch and a short one.** Eleven rather than sixteen because the
        /// second batch has to be *short*, or a sweep that always ran full batches would pass. They
        /// are zero-padded so their byte order is their numeric order, which makes the batch
        /// boundary something a reader can check by eye.
        pub const MANY_NAMES: [&str; 11] = [
            "m-00.txt", "m-01.txt", "m-02.txt", "m-03.txt", "m-04.txt", "m-05.txt", "m-06.txt",
            "m-07.txt", "m-08.txt", "m-09.txt", "m-10.txt",
        ];
        /// **The first batch, as the prompt prints it**: the eight smallest matches, space
        /// separated. Spelled out rather than derived, because a gate that computes its own expected
        /// answer from the same rule it is testing checks nothing.
        pub const MANY_FIRST_BATCH: &str =
            "m-00.txt m-01.txt m-02.txt m-03.txt m-04.txt m-05.txt m-06.txt m-07.txt";
        /// **The second batch**, which is the short one, and which the first batch's watermark
        /// decides. A sweep resuming from a directory *cursor* rather than from a name would print
        /// something else here the moment anything in the directory moved.
        pub const MANY_SECOND_BATCH: &str = "m-08.txt m-09.txt m-10.txt";
        /// A file in the same directory the pattern does not match, so "the batches are the match"
        /// is a claim about *which* names rather than about how many.
        pub const MANY_MISS: &str = "m-keep.log";
        pub const MANY_BODY: &[u8] = b"CRK109-XARGS: a name in a match too large to hand over\n";

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
        pub const ROOT_ENTRIES: [&str; 8] = [
            GLOBMANY,
            GLOBSET,
            super::MOTD_NAME,
            OTHER,
            REDIR,
            RMTREE,
            super::SCRATCH_NAME,
            SUB,
        ];
    }

    /// **The directory attacker's report** (milestone 47), a bitmap for the same reason the per-file
    /// attacker's is one: the test asserts an *expected set*, so the read-only run and the wide run
    /// are each other's control and a caretaker that refused everything fails one of them.
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
        /// control bit: without it every refusal above is equally consistent with a caretaker that
        /// answers no to everything, or a grant that reaches nothing at all.
        pub const OPENED_ITS_OWN: u64 = 1 << 13;
        /// **The thing it should be able to do failed**, so nothing above was proven. A capability
        /// that reaches nothing is trivially unescapable.
        pub const GRANTED_ACCESS_FAILED: u64 = 1 << 11;
        /// **It listed the extended attributes of the file inside its grant** (milestone 61).
        /// Expected wherever [`super::super::dir::READ`] is held, which is every configuration that
        /// can open the file at all: an attribute is part of what a file *is*, so a capability that
        /// may read the file may read what is attached to it. Before milestone 61 the caretaker
        /// answered `EOPNOTSUPP` and this bit was never set.
        pub const READ_ATTRS: u64 = 1 << 14;
        /// **It set an extended attribute on that file and read it back with its type code.**
        /// Expected only with [`super::super::dir::WRITE`], which is [`WROTE`]'s rule applied to the
        /// third way of changing a file.
        ///
        /// The interesting part is *who* enforces it: `fs_subtree_caretaker` performs no check, so
        /// this bit is a statement about the handle the FS server minted from the caretaker's one
        /// `OPENDIR`. A file handle inherits `READ`/`WRITE` from the directory it was opened
        /// through, so a read-only subtree grant is refused by the server rather than by a branch in
        /// the caretaker, which is exactly that program's design property holding under a verb it
        /// was never taught.
        pub const SET_AN_ATTR: u64 = 1 << 15;
        /// **It reached a name in the granted directory that the grant's *set* does not carry**
        /// (milestone 61's name-set witness). Never allowed.
        ///
        /// A set capability's whole property is that `rm *.txt` cannot reach `notes.md`. `rm` asks
        /// that question with [`super::super::dir::REMOVE`] and nothing else; this asks it with
        /// `READ`, because the milestone taught `fs_nameset_caretaker` four verbs that read and it
        /// would be a poor trade to close an attribute gap by opening a naming one.
        pub const REACHED_AN_UNMATCHED_NAME: u64 = 1 << 16;
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
        /// **`/` named this shell's own root**: from one level down, `cd /` came back to the root
        /// and `pwd` rendered `/` again. Plan 9's answer rather than DOS's, and the round trip
        /// `pwd` had always implied: the path it prints is one you can type back.
        ///
        /// It replaced an `ABSOLUTE_REFUSED` bit on 2026-08-18. That bit was true of a system with
        /// no namespace to root a path in, and rooting one in the holder's own namespace grants
        /// nothing, which [`ABSOLUTE_REACHED_INNER`] and [`ABSOLUTE_REACHED_SECRET`] are the
        /// measurement of rather than the claim.
        pub const ABSOLUTE_IS_MY_ROOT: u64 = 1 << 3;
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
        /// It opened [`super::tree::INNER`] **by absolute path**, `/inner`, which exists only in
        /// [`super::tree::SUB`].
        ///
        /// The pair with [`ABSOLUTE_REACHED_SECRET`] is why absolute paths cost nothing here: the
        /// same two probes as [`REACHED_INNER`] and [`REACHED_SECRET`], asked with a leading slash,
        /// and each shell reaches exactly the one its own root contains. A `/` rooted in a global
        /// namespace would make both shells reach both.
        pub const ABSOLUTE_REACHED_INNER: u64 = 1 << 20;
        /// It opened [`super::tree::SECRET`] by absolute path, `/secret`, which exists only in
        /// [`super::tree::OTHER`].
        pub const ABSOLUTE_REACHED_SECRET: u64 = 1 << 21;
        /// **`cd /..` was refused and nothing moved.** The negative control on the syntax: an
        /// absolute path meets the same wall `..` does, because your root is the only root there
        /// is and there is no level above it to name.
        pub const ABSOLUTE_CLAMPED_AT_ROOT: u64 = 1 << 22;
    }

    /// **What the globbing witness reports** (milestone 47's globbing lane): a bitmap, for the same
    /// reason every other witness here reports one, and with the same vacuity control at the end.
    ///
    /// The shell it runs in holds a real directory capability served by a real
    /// `fs_subtree_caretaker`, so what is under test is the prompt's own expansion path over a real
    /// `READDIR`, not a reimplementation of it.
    pub mod globscape {
        /// `echo` expanded the pattern into a non-empty set. The precondition for everything else:
        /// a shell that expanded nothing agrees with a grant of nothing, perfectly and uselessly.
        pub const EXPANDED: u64 = 1 << 0;
        /// **The names `echo` printed and the names the grant carries are the same bytes.** The
        /// headline: the expansion you see is the authority that moves.
        pub const AGREED: u64 = 1 << 1;
        /// The expansion contains neither [`super::tree::GLOB_MISS`] nor [`super::tree::GLOB_DIR`],
        /// which exist in the same directory and are one entry away. Without this, [`AGREED`] is
        /// equally true of a shell that expanded a pattern to everything it could see.
        pub const EXCLUDED_A_STRANGER: u64 = 1 << 2;
        /// A pattern that matched nothing was **refused**, rather than printed as itself. zsh's
        /// answer, and the model's: an empty expansion is an empty grant.
        pub const NO_MATCH_REFUSED: u64 = 1 << 3;
        /// A pattern in a leading path component was refused. Selecting directories to walk is an
        /// authority question, so it is not one a matcher answers (notes/glob.md).
        pub const PATTERN_IN_PATH_REFUSED: u64 = 1 << 4;
        /// A line with no pattern in it came out of `echo` **byte for byte**, spacing included.
        /// Globbing is not allowed to quietly turn `echo` into a word-splitter.
        pub const TEXT_UNTOUCHED: u64 = 1 << 5;
        /// Something that should have worked did not, so nothing above proves anything. Always a
        /// failure.
        pub const GLOB_FAILED: u64 = 1 << 6;
    }

    /// **What the `rm` program reports, and how a diagnostic is told from the verdict**
    /// (milestone 47's `rm -r`, `user/src/rm.rs`).
    ///
    /// **Where these two words travel** (corrected 2026-08-17). `rm` declares the sink contract
    /// (`grant_plan::OutputSpec::Bytes`), so its report is framed text and then `sink_proto`'s
    /// `OP_EOF`; the status and the removal count ride in the two words that end-of-stream message
    /// leaves free. They used to ride on [`VERDICT`], a word the sink contract has no meaning for,
    /// which no reader but a guest test could have decoded.
    ///
    /// `rm` is a **program**, not a shell builtin, so it holds a report endpoint and nothing that
    /// names a terminal. Two kinds of message go out on it, and they are told apart by the first
    /// word, which is what makes "silent on success" checkable from the far end:
    ///
    /// - **A line of text**, framed exactly as every std program's stdout is (`w0` = the byte count,
    ///   at most 16, and the bytes little-endian in `w1`|`w2`). Diagnostics always go out this way;
    ///   `-v` adds one line per name removed.
    /// - **The verdict**, once, last: `w0` = [`crate::fixture::VERDICT`], `w1` = the exit status, `w2` = how
    ///   many names were removed.
    ///
    /// A byte count is at most 16 and [`crate::fixture::VERDICT`] is far larger, so the two never collide. A
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

    /// **What the extended-attribute witness reports** (milestone 57), a bitmap for the reason every
    /// other witness here reports one: the kernel test asserts an *exact* set, so a client that could
    /// do nothing and a client that could do everything both fail, and a failure names itself.
    ///
    /// It rides the third word of the FS client's proof report, beside the `motd` head and the
    /// success sentinel, because the attribute layer is served by the same FS server over the same
    /// endpoint and a second boot of the whole three-process stack would prove nothing new.
    pub mod attrs {
        /// An attribute was set and read back with **its type code and its bytes**. The type code
        /// half is what makes this more than a byte-string round trip: a layer that dropped the kind
        /// would pass every other bit here.
        pub const SET_AND_READ_BACK: u64 = 1 << 0;
        /// The listing named it, and named nothing else. "Nothing else" is the half that catches a
        /// store keyed on the wrong node, which would show up as somebody else's attributes.
        pub const LISTED: u64 = 1 << 1;
        /// **The attribute followed its file across a rename.** The headline property, and the one
        /// an AppleDouble sidecar gets wrong. It works because the store keys on the node and a
        /// rename changes a directory entry, so nothing in the rename path knows attributes exist.
        pub const SURVIVED_RENAME: u64 = 1 << 2;
        /// After a remove, reading it answers `ENODATA`. Without this, [`SET_AND_READ_BACK`] is
        /// equally consistent with a store that never forgets anything.
        pub const GONE_AFTER_REMOVE: u64 = 1 << 3;
        /// **A file recreated at a name whose previous occupant had attributes has none.** The
        /// on-device half of "deletion is in the same transaction as the file's": a leaked blob is
        /// not wasted space, it is somebody else's metadata on a file that never asked for it.
        pub const GONE_AFTER_UNLINK: u64 = 1 << 4;
        /// A value past [`super::super::xattr::MAX_VALUE`] was refused with
        /// [`super::super::xattr::E2BIG`], loudly, rather than stored short (DECISIONS §42).
        pub const OVERSIZE_REFUSED: u64 = 1 << 5;
        /// The store's directory could not be opened, created, or descended into. A client that
        /// could name it could read any file's attributes as ordinary bytes.
        pub const STORE_UNNAMEABLE: u64 = 1 << 6;
        /// And a full enumeration of the root did not name it, which is the other half: what cannot
        /// be named is also not listed. Reported only if the enumeration ran to the end and returned
        /// something, so an empty listing cannot pass as a clean one.
        pub const STORE_UNLISTED: u64 = 1 << 7;
        /// **Something that should have worked did not**, so nothing above proves anything. The
        /// control bit, in the shape every other witness here uses.
        pub const ATTRS_FAILED: u64 = 1 << 8;

        /// What a correct run reports, stated once so the two ISA legs' assertions cannot drift.
        pub const EXPECTED: u64 = SET_AND_READ_BACK
            | LISTED
            | SURVIVED_RENAME
            | GONE_AFTER_REMOVE
            | GONE_AFTER_UNLINK
            | OVERSIZE_REFUSED
            | STORE_UNNAMEABLE
            | STORE_UNLISTED;

        /// The file the witness makes, sets an attribute on, and renames. Its own name, so nothing
        /// it leaves behind can be confused with a fixture the post-run host check pins.
        pub const PROBE: &str = "attr-probe";
        /// What it renames [`PROBE`] to. The attribute has to be readable through this name.
        pub const PROBE_MOVED: &str = "attr-moved";
        /// The attribute the witness writes, spelled the way Samba spells one.
        pub const NAME: &[u8] = b"user.com.apple.metadata";
        /// Its value: opaque bytes, which is exactly what `streams_xattr` stores.
        pub const VALUE: &[u8] = b"CRK57: an opaque byte string\n";
        /// Its type code, `'CSTR'` in BFS's spelling. Non-zero on purpose: a layer that silently
        /// wrote [`super::super::xattr::RAW`] would pass a test whose fixture used the default.
        pub const KIND: u32 = 0x4353_5452;
    }

    /// **The on-device crash test's vocabulary** (milestone 37, DECISIONS §34 condition 1).
    ///
    /// The host sweep in `fs_server/tests/crash_consistency.rs` proves the property exhaustively
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

    /// **The blank disk milestone 57's write half partitions and formats**, and the layout the two
    /// programs and the post-run host check all have to agree about.
    ///
    /// Everything here is on **its own disk**, regenerated every run, for milestone 37's reason: a
    /// test that writes a partition table cannot be pointed at a fixture other tests read, or every
    /// one of their results becomes a function of whether this one ran first (DECISIONS §27).
    ///
    /// The disk arrives as 64 MiB of zeros with no table on it at all, which is what makes the
    /// refusal cases checkable: a program that was denied entropy and wrote nothing leaves a disk
    /// that still parses as nothing.
    ///
    /// ```text
    ///   LBA      2048..10239    an EFI system partition (nothing formats it; it is here so the
    ///                           table has more than one entry and more than one unique GUID)
    ///   LBA     10240..30719    a Linux filesystem partition, likewise
    ///   LBA     30720..129023   the nife data partition: 48 MiB, 4096-aligned at both ends,
    ///                           and the one `mkfs` formats
    /// ```
    pub mod blank {
        /// The disk's size in 512-byte logical blocks. 64 MiB, matching the `sgdisk` fixture disk so
        /// the two are comparable at a glance.
        pub const DISK_BLOCKS: u64 = 131_072;
        /// The logical block size the table counts in. Every disk this project has met uses it, and
        /// nothing on the blk wire reports it (`disk_surveyor`'s BUGS).
        pub const LBA: u64 = 512;

        /// The EFI system partition: first and last logical block, inclusive. 2048 is the 1 MiB
        /// alignment every real tool uses.
        pub const ESP: (u64, u64) = (2048, 10239);
        /// The Linux filesystem partition.
        pub const LINUX: (u64, u64) = (10240, 30719);
        /// **The nife data partition**, the one that gets a filesystem. Its start and its
        /// length are both whole multiples of RedoxFS's 4096-byte block, which `mkfs` requires
        /// and refuses without: a filesystem whose blocks straddle the partition's first byte would
        /// read fine through the offset disk that made it and be unreadable by anything else.
        pub const DATA: (u64, u64) = (30720, 129_023);

        /// How many partitions the table carries. The post-run host check asserts on it.
        pub const PARTITIONS: usize = 3;

        /// The name written into every entry's 36 UTF-16 units, in order.
        pub const NAMES: [&str; PARTITIONS] = ["nife boot", "nife root", "nife data"];

        /// The file `mkfs` creates on the filesystem it just made, so the host tool has
        /// something to read back. An empty filesystem that opens proves the header; a file the
        /// guest wrote proves the tree, the allocator and the root node too.
        pub const MADE_NAME: &str = "made-on-target";
        /// Its contents, checked byte for byte on the host after the run.
        pub const MADE_BODY: &[u8] = b"CRK57: this filesystem was created by nife on the target\n";
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The crate is `no_std`, so its own tests have to say where the allocating types come from. They
    // are here only to build fixtures; nothing under test allocates.
    extern crate std;
    use std::vec::Vec;
    use std::{format, vec};

    /// **Every verb in the contract has a row, and the row agrees with the opcode's own doc.**
    ///
    /// The compile-time assertions in [`verb`] already prove the table is complete and in order, so
    /// this test proves the part a `const` cannot reach: that the rows say the *right* thing. It is
    /// the mitigation for the table's own BUGS entry, which is that one wrong row is wrong in three
    /// programs at once.
    #[test]
    fn every_verb_has_a_row_that_says_what_its_words_mean() {
        for op in verb::FIRST..=verb::LAST {
            let v = verb::of(op).unwrap_or_else(|| panic!("opcode {op} has no row"));
            assert_eq!(v.op, op, "{} is filed under the wrong opcode", v.name);
            assert!(!v.name.is_empty(), "opcode {op} has no name");
            // A verb cannot state its rights two ways at once: `Rights::allows` is "all of", so an
            // any-of requirement folded into `needs_all` would demand both and refuse a capability
            // the server would have accepted.
            assert!(
                v.needs_all == 0 || v.needs_any == 0,
                "{} states its rights twice",
                v.name
            );
            assert_eq!(
                v.needs_all & !dir::ALL,
                0,
                "{} names a right this contract does not define",
                v.name
            );
            assert_eq!(
                v.needs_any & !dir::ALL,
                0,
                "{} names a right this contract does not define",
                v.name
            );
        }
        // The one any-of in the contract, pinned so that folding it into `needs_all` fails here.
        let open = verb::of(fs::OPEN).unwrap();
        assert_eq!(open.needs_any, dir::READ | dir::WRITE);
        assert_eq!(open.needs_all, 0);

        // The four verbs that hand back authority, which is what a renumbering proxy must install.
        let mints: Vec<&str> = (verb::FIRST..=verb::LAST)
            .filter_map(verb::of)
            .filter(|v| v.mints_handle)
            .map(|v| v.name)
            .collect();
        assert_eq!(mints, ["OPEN", "CREATE", "OPENDIR", "MKDIR"]);
    }

    /// **The `statfs` record survives a round trip and refuses a short one.**
    ///
    /// The refusal is the half worth having: a truncated reply read as zeroes would tell a client
    /// the volume is empty and zero bytes big, which is a lie in the direction that loses data.
    #[test]
    fn the_statfs_record_round_trips_and_a_short_reply_is_not_zeroes() {
        let mut page = [0u8; 64];
        assert_eq!(
            statfs::encode(&mut page, 4096, 16384, 9001),
            Some(statfs::LEN)
        );
        assert_eq!(statfs::decode(&page), Some((4096, 16384, 9001)));
        // Exactly the record is enough; one byte less is not readable at all.
        assert_eq!(
            statfs::decode(&page[..statfs::LEN]),
            Some((4096, 16384, 9001))
        );
        assert_eq!(statfs::decode(&page[..statfs::LEN - 1]), None);
        assert_eq!(statfs::decode(&[]), None);
        // A page too small to hold the record refuses rather than writing a partial one.
        let mut tiny = [0u8; statfs::LEN - 1];
        assert_eq!(statfs::encode(&mut tiny, 4096, 1, 1), None);

        // The length is the version: a longer reply is a later version, and this version reads its
        // own prefix out of it rather than refusing.
        let mut longer = [0u8; statfs::LEN + 8];
        statfs::encode(&mut longer, 512, 2, 1).unwrap();
        longer[statfs::LEN..].copy_from_slice(&7u64.to_le_bytes());
        assert_eq!(statfs::decode(&longer), Some((512, 2, 1)));

        assert_eq!(statfs::total_bytes(4096, 16384), 64 * 1024 * 1024);
        assert_eq!(statfs::free_bytes(4096, 0), 0);
        // Saturating, because these numbers come off a disk: a corrupt pair must stay obviously
        // wrong instead of wrapping into a small plausible answer.
        assert_eq!(statfs::total_bytes(u64::MAX, 2), u64::MAX);
    }

    /// **`STATFS` asks for no right and mints no handle**, which is the whole of its row and the
    /// part a caretaker acts on. Spelled out rather than derived, for [`verb::TABLE`]'s reason: a
    /// row that grew a rights requirement would silently make a write-only grant unable to ask
    /// whether its next write fits.
    #[test]
    fn statfs_names_a_handle_and_nothing_else() {
        let v = verb::of(fs::STATFS).unwrap();
        assert_eq!(v.name, "STATFS");
        assert_eq!(v.needs_all, 0);
        assert_eq!(v.needs_any, 0);
        assert!(!v.carries_len(), "STATFS's length field counts nothing");
        assert!(!v.carries_w1, "STATFS has no second word");
        assert!(!v.mints_handle);
        assert!(!v.mutates(), "asking how full a disk is changes nothing");
        assert!(!v.takes_name(), "a name filter has nothing to filter here");
        // Forwarded through a per-file grant: see the policy row's own comment.
        assert_eq!(
            verb::file_grant::of(fs::STATFS),
            Some(verb::file_grant::Policy::Forward)
        );
    }

    /// **A name filter must see every verb that names a directory entry, and no others.**
    ///
    /// This is the row that decides a security property rather than a formatting one:
    /// `fs_nameset_caretaker` asks "is this name in the set" exactly when `takes_name` is true, so a
    /// name-taking verb whose row says otherwise walks straight past the filter. The list is spelled
    /// out rather than derived, because deriving it from the field it is checking would assert
    /// nothing.
    #[test]
    fn the_filter_sees_exactly_the_verbs_that_name_a_directory_entry() {
        let filtered: Vec<&str> = (verb::FIRST..=verb::LAST)
            .filter_map(verb::of)
            .filter(|v| v.takes_name())
            .map(|v| v.name)
            .collect();
        assert_eq!(
            filtered,
            [
                "OPEN", "CREATE", "OPENDIR", "MKDIR", "RENAME", "UNLINK", "RMDIR"
            ],
            "a verb that resolves a name in the granted directory is missing from the filter, or a \
             verb that does not resolve one is being filtered"
        );
        // The attribute verbs carry a name in the page and it is NOT a directory name, which is the
        // distinction `Operand::Payload` exists to make. Filtering it would refuse a program its own
        // file's attributes on the grounds that the attribute is not in the directory.
        for op in [fs::GETXATTR, fs::SETXATTR, fs::LISTXATTR, fs::REMOVEXATTR] {
            assert!(
                !verb::of(op).unwrap().takes_name(),
                "an attribute name is not a name in the namespace a capability narrows"
            );
        }
    }

    /// **Every mutating verb is refused by a read-only per-file grant**, which is what makes the
    /// attribute forwarding safe: `SETXATTR` and `REMOVEXATTR` mutate, so a read grant must not pass
    /// them on, and `fs_file_caretaker` decides that from `mutates()` rather than from a list of
    /// verbs somebody maintains.
    #[test]
    fn a_read_only_file_grant_refuses_every_verb_that_mutates() {
        use verb::file_grant::Policy;
        let mutating: Vec<&str> = (verb::FIRST..=verb::LAST)
            .filter_map(verb::of)
            .filter(|v| v.mutates())
            .map(|v| v.name)
            .collect();
        assert_eq!(
            mutating,
            [
                "WRITE",
                "CREATE",
                "TRUNCATE",
                "MKDIR",
                "RENAME",
                "UNLINK",
                "RMDIR",
                "SETXATTR",
                "REMOVEXATTR",
            ]
        );
        // Nothing that mutates may be answered locally: `Forward` is the only policy that consults
        // the grant's direction, and `Refused` never reaches the server at all.
        for op in verb::FIRST..=verb::LAST {
            let v = verb::of(op).unwrap();
            let p = verb::file_grant::of(op).unwrap();
            if v.mutates() {
                assert!(
                    matches!(p, Policy::Forward | Policy::Refused(_)),
                    "{} mutates and must be refused or direction-gated, never answered locally",
                    v.name
                );
            }
        }
    }

    /// **The four attribute verbs are forwarded through a per-file grant** (milestone 61's gap).
    ///
    /// Pinned as its own test because the old behaviour was uniform and defensible: all four
    /// answered `EOPNOTSUPP`, which was honest about a capability that did not carry them. What
    /// makes forwarding right is `xattr`'s own rule, that an attribute is part of what a file *is*,
    /// so a capability that may read the file may read what is attached to it. A future edit that
    /// quietly reverted this would pass every other test here.
    #[test]
    fn a_per_file_grant_carries_its_files_attributes() {
        use verb::file_grant::Policy;
        for op in [fs::GETXATTR, fs::SETXATTR, fs::LISTXATTR, fs::REMOVEXATTR] {
            assert_eq!(
                verb::file_grant::of(op),
                Some(Policy::Forward),
                "{} is not forwarded, so a program behind a per-file grant cannot reach its own \
                 file's attributes",
                verb::of(op).unwrap().name
            );
        }
        // The read half must be reachable through a read-only grant, which is the case that
        // motivated the milestone: `wc report.txt` should be able to ask what is attached to it.
        assert!(!verb::of(fs::GETXATTR).unwrap().mutates());
        assert!(!verb::of(fs::LISTXATTR).unwrap().mutates());
    }

    /// **A file capability is not a directory, and the refusals still say which kind of "no" it is.**
    ///
    /// The distinction the table was most at risk of flattening. `CREATE` answers `ENOTDIR` because
    /// the request does not mean anything here, not `EACCES` and not a generic "denied"; the other
    /// directory verbs answer it too, since 2026-08-01. All seven are pinned, so a change to any of
    /// them is a change somebody made on purpose, and `EBADF` stays available to mean the one thing
    /// it should: the caller named a handle this program never minted.
    #[test]
    fn a_file_grants_refusals_keep_their_separate_meanings() {
        use verb::file_grant::Policy;
        assert_eq!(
            verb::file_grant::of(fs::CREATE),
            Some(Policy::Refused(grant::ENOTDIR)),
            "CREATE must say `this is not a directory`, never `you were denied`"
        );
        // Every directory verb answers what CREATE answers. `EBADF` would be a claim about the
        // caller's handle; these are refusals about the REQUEST, which a file capability cannot
        // give meaning to. The two must stay distinguishable, because `EBADF` is also what a
        // forged handle gets, and a client cannot act on a word that means both.
        for op in [
            fs::OPENDIR,
            fs::READDIR,
            fs::MKDIR,
            fs::RENAME,
            fs::UNLINK,
            fs::RMDIR,
        ] {
            assert_eq!(
                verb::file_grant::of(op),
                Some(Policy::Refused(grant::ENOTDIR)),
                "a directory verb through a file grant is ENOTDIR, not EBADF",
            );
        }
        // The two the caretaker answers out of what it holds rather than by asking the server.
        assert_eq!(verb::file_grant::of(fs::OPEN), Some(Policy::Local));
        assert_eq!(verb::file_grant::of(fs::CLOSE), Some(Policy::Local));
        // An opcode the contract does not carry has no row and no policy, in both tables at once, so
        // a caretaker's one refusal site covers a forged opcode as well as a forged handle.
        assert!(verb::of(verb::LAST + 1).is_none());
        assert!(verb::file_grant::of(verb::LAST + 1).is_none());
        assert!(verb::of(0).is_none(), "0 is not a verb");
    }

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

    /// **milestone 138 step 4**: a caller that packs a block count survives the round trip, and a
    /// caller that never heard of one (every caller through step 3, via the plain [`req`]) still
    /// decodes as exactly one block, which is the whole of the compatibility claim
    /// `blk::TRANSFER_BLOCKS`'s doc makes.
    #[test]
    fn blk_request_carries_a_block_count_and_defaults_to_one() {
        for blocks in [1usize, 2, blk::TRANSFER_BLOCKS] {
            let w0 = blk::req(blk::READ, blocks);
            assert_eq!(op(w0), blk::READ);
            assert_eq!(blk::req_blocks(w0), blocks);
        }
        // A pre-step-4 caller's word (the plain, one-argument `req`) decodes as one block.
        assert_eq!(blk::req_blocks(req(blk::READ)), 1);
        // `blk::req(op, 1)` and `req(op)` are wire-identical, which is the compatibility property
        // stated: a caller that upgrades to naming one block explicitly changes nothing on the wire.
        assert_eq!(blk::req(blk::READ, 1), req(blk::READ));
    }

    #[test]
    #[should_panic]
    fn blk_request_refuses_a_count_of_zero() {
        blk::req(blk::READ, 0);
    }

    #[test]
    #[should_panic]
    fn blk_request_refuses_a_count_past_the_transfer_limit() {
        blk::req(blk::READ, blk::TRANSFER_BLOCKS + 1);
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
            xattr::ENODATA,
            xattr::ERANGE,
            xattr::E2BIG,
            xattr::ENOSPC,
            xattr::ENOTSUP,
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

    /// **Every errno this contract names has its own sentence.** The test above proves the
    /// sentences are facts rather than policies; this one proves the map is not degenerate. The
    /// errno is a *decision* about what the holder learns, so two errnos sharing a sentence, or one
    /// falling through to the fallback, would print a true-sounding wrong explanation at the one
    /// place a human meets the refusal. Pairwise distinctness, with the fallback in the set, makes
    /// either failure visible.
    #[test]
    fn every_explained_errno_has_its_own_sentence() {
        let fallback = dir::explain(-1);
        assert_eq!(
            fallback,
            dir::explain(0),
            "the fallback must not depend on the number"
        );
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(fallback);
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
            xattr::ENODATA,
            xattr::ERANGE,
            xattr::E2BIG,
            xattr::ENOSPC,
            xattr::ENOTSUP,
        ] {
            assert!(
                seen.insert(dir::explain(errno)),
                "errno {errno} shares its sentence with another, or fell through to the fallback"
            );
        }
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

    /// **`pack_name` stops at the limit even when the caller did not.** [`grant::fits`] is the
    /// caller's check and `pack_name` does not repeat it, so it must stay total on input it was
    /// told not to receive: a 17-byte name packs as its 16-byte prefix rather than indexing `hi`
    /// off its end.
    #[test]
    fn pack_name_stops_at_the_limit_even_when_the_caller_did_not() {
        assert!(!grant::fits(b"sixteen-bytes!!!X"));
        assert_eq!(
            grant::pack_name(b"sixteen-bytes!!!X"),
            grant::pack_name(b"sixteen-bytes!!!"),
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
            WROTE_ATTR,
            GRANTED_ATTRS_FAILED,
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
            ("GETXATTR", fs::GETXATTR),
            ("SETXATTR", fs::SETXATTR),
            ("LISTXATTR", fs::LISTXATTR),
            ("REMOVEXATTR", fs::REMOVEXATTR),
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

    /// **A 255-byte name fills the length byte exactly, and 256 is refused.** The boundary is
    /// load-bearing because `encode` writes `name.len() as u8`: a check that let 256 through would
    /// write a length byte of 0, and the far side would read an empty name followed by 256 bytes of
    /// somebody else's records.
    #[test]
    fn a_dirent_name_fills_the_length_byte_exactly_or_is_refused() {
        let mut page = [0u8; 512];
        let longest = [b'n'; 255];
        let n = dirent::encode(&mut page, &longest, false).expect("255 fits a length byte");
        assert_eq!(n, dirent::record_len(255));
        let (name, is_dir) = dirent::iter(&page[..n]).next().expect("readable");
        assert_eq!(name, &longest[..]);
        assert!(!is_dir);
        let over = [b'n'; 256];
        assert_eq!(
            dirent::encode(&mut page, &over, false),
            None,
            "a length byte cannot say 256"
        );
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
            READ_ATTRS,
            SET_AN_ATTR,
            REACHED_AN_UNMATCHED_NAME,
        ];
        let mut seen = 0u64;
        for b in bits {
            assert_ne!(b, 0, "zero is the pass; it cannot also be a breach");
            assert_eq!(seen & b, 0, "two escapes share a bit");
            seen |= b;
        }
    }

    /// **The absolute forms of the two probe names are the relative ones with a slash on the
    /// front**, and nothing else. Written down because they are two constants that must agree, and
    /// a drift between them would not fail: the witness would simply open nothing and report a
    /// clear bit, which reads exactly like the escape it is looking for having been prevented.
    #[test]
    fn the_absolute_probe_names_are_the_relative_ones_rooted() {
        use fixture::tree::*;
        assert_eq!(ABS_INNER, format!("/{INNER}"));
        assert_eq!(ABS_SECRET, format!("/{SECRET}"));
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
            ABSOLUTE_IS_MY_ROOT,
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
            ABSOLUTE_REACHED_INNER,
            ABSOLUTE_REACHED_SECRET,
            ABSOLUTE_CLAMPED_AT_ROOT,
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
            SUB, INNER, DEEPER, LEAF, OTHER, SECRET, MADE, MADE_DIR, MOVED, NAV_KEPT, NAV_GONE,
            NAV_DIR, NAV_EMPTY, NAV_INSIDE,
            // The `rm -r` subtree. Its names ride in a grant too: the program is *started* with the
            // one name it is to remove, packed the way every other grant is.
            RMTREE, RM_KEEP, RM_DOOMED, RM_ONE, RM_TWO, RM_NESTED, RM_LEAF, RM_SOLO, RM_MISSING,
        ] {
            assert!(
                grant::fits(name.as_bytes()),
                "{name} cannot ride in a grant"
            );
        }
        // The post-run host check identifies the attacker's creations by PREFIX, because a run
        // index is appended to each. A fixture name that started with one of those prefixes would
        // be read as an escape, or would hide one.
        let probes = [
            MADE, MADE_DIR, MOVED, NAV_KEPT, NAV_GONE, NAV_DIR, NAV_EMPTY,
        ];
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
        for staged in [
            RM_KEEP, RM_DOOMED, RM_ONE, RM_TWO, RM_NESTED, RM_LEAF, RM_SOLO,
        ] {
            assert_ne!(
                staged, RM_MISSING,
                "the name `rm -f` is pointed at must not be one the fixture stages",
            );
        }
        assert_ne!(RMTREE, SUB, "the rm grant must not be the shells' subtree");
    }

    /// **A verdict word cannot be mistaken for a line of text**, which every witness in this fixture
    /// rests on: a receiver reads "the first message is the verdict" as "this run printed nothing".
    ///
    /// **`rm` no longer rides this word** (2026-08-17). It declares the sink contract, so it ends its
    /// stream with `sink_proto::eof()` and its status and count ride in the two words that message
    /// leaves free; the same property is pinned over there by `eof_is_not_a_byte_count`. What is
    /// checked here is the word the attackers and the shell witnesses still use.
    #[test]
    fn a_text_frame_and_a_verdict_are_told_apart_by_the_first_word() {
        // A frame's first word is a byte count into the two payload words, so 16 is its ceiling.
        // Checked at COMPILE time rather than here: both sides are constants, so a change that broke
        // it should fail the build rather than wait for someone to run the suite. Same discipline as
        // the clock service's step-bound assertion (§43).
        const _: () = assert!(
            fixture::VERDICT > 16,
            "a verdict word that could be a byte count would make a diagnostic read as a verdict",
        );
        // And the caretakers' two answers to a descent are told apart the same way, which is what
        // lets init read one word and know whether it holds a capability or a refusal.
        const _: () = assert!(
            fixture::READY != fixture::DESCENT_REFUSED,
            "a caretaker's two possible answers must not be the same word",
        );
        // And a non-zero status is the errno, so the two ends agree about what failed rather than
        // only that something did.
        assert_eq!(fixture::rm::status(dir::EROFS), dir::EROFS as u64);
        assert_ne!(fixture::rm::status(dir::EROFS), fixture::rm::OK);
        assert_eq!(
            fixture::rm::OK,
            0,
            "success is zero, as every shell expects"
        );
    }

    /// **The set the globbing lane grants is exactly what the pattern matches**, checked against the
    /// matcher rather than asserted twice.
    ///
    /// The kernel test states its set literally (a `[(&[u8], bool); 2]` it hands to
    /// [`nameset::encode`]), which is the only way to build a grant without an enumeration. That
    /// literal is only trustworthy if it *is* the expansion, so this pins it: the pattern matches the
    /// two names in the set and neither of the two controls, over the fixture as staged.
    #[test]
    fn the_glob_fixture_is_matched_by_the_pattern_it_is_staged_for() {
        use fixture::tree;
        let p = tree::GLOB_PATTERN;
        for name in [tree::GLOB_ONE, tree::GLOB_TWO] {
            assert!(
                glob::matches(p, name.as_bytes()),
                "{name} is in the granted set and the pattern does not match it",
            );
        }
        for name in [tree::GLOB_MISS, tree::GLOB_DIR, tree::GLOB_INNER] {
            assert!(
                !glob::matches(p, name.as_bytes()),
                "{name} is a control and the pattern matches it, so it proves nothing",
            );
        }
        // Every name in the set has to be able to travel in one, or the grant cannot be built.
        for name in [
            tree::GLOB_ONE,
            tree::GLOB_TWO,
            tree::GLOB_MISS,
            tree::GLOB_DIR,
        ] {
            assert!(grant::fits(name.as_bytes()), "{name} does not fit a grant");
        }
    }

    /// **The batching fixture is over the bound, and the two batches printed in the gate are the
    /// ones the rule produces** (milestone 109).
    ///
    /// `script/shell-check` asserts two literal strings at a real prompt, and a literal is only
    /// worth asserting if it is the answer rather than a second opinion about it. So this pins all
    /// three halves: the pattern matches every name and not the control, there are more names than
    /// one grant can carry, and the batch boundary falls where [`fixture::tree::MANY_FIRST_BATCH`]
    /// says it does.
    #[test]
    fn the_batching_fixture_is_over_the_bound_and_splits_where_the_gate_says() {
        use fixture::tree;
        let p = tree::MANY_PATTERN;
        for name in tree::MANY_NAMES {
            assert!(
                glob::matches(p, name.as_bytes()),
                "{name} is staged for the sweep and the pattern does not match it",
            );
            assert!(grant::fits(name.as_bytes()), "{name} does not fit a grant");
        }
        assert!(
            !glob::matches(p, tree::MANY_MISS.as_bytes()),
            "the control matches, so 'the batches are the match' proves nothing",
        );
        assert!(
            tree::MANY_NAMES.len() > nameset::MAX_NAMES,
            "the fixture fits in one grant, so nothing here is batched",
        );
        // The names are staged in sorted order, which is what lets the two batches below be read off
        // the array rather than sorted at the assertion.
        assert!(tree::MANY_NAMES.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(
            tree::MANY_FIRST_BATCH,
            tree::MANY_NAMES[..nameset::MAX_NAMES].join(" "),
        );
        assert_eq!(
            tree::MANY_SECOND_BATCH,
            tree::MANY_NAMES[nameset::MAX_NAMES..].join(" "),
        );
    }

    /// The set encoding round-trips, carries the type bit, and **refuses rather than truncates**.
    ///
    /// The refusals are the half worth testing: `encode` is building a capability, and a capability
    /// that quietly held fewer names than the command matched is the exact failure this lane is
    /// supposed to make impossible.
    #[test]
    fn a_name_set_round_trips_and_refuses_rather_than_truncating() {
        let mut buf = [0u8; nameset::BYTES];
        let names: [(&[u8], bool); 3] = [(b"one.txt", false), (b"logs", true), (b"z", false)];
        let n = nameset::encode(&names, &mut buf).expect("three short names must fit");
        assert_eq!(nameset::count(&buf[..n]), 3);
        let want: [(&[u8], bool); 3] = [(b"one.txt", false), (b"logs", true), (b"z", false)];
        for (got, want) in nameset::iter(&buf[..n]).zip(want) {
            assert_eq!(got, want, "the set did not come back as it went in");
        }
        assert!(nameset::contains(&buf[..n], b"logs"));
        assert!(
            !nameset::contains(&buf[..n], b"log"),
            "a prefix is a different name; the caretaker's whole check is this comparison",
        );

        // A full set fits exactly, and one more name does not: BYTES is a bound that is met, not a
        // guess with slack in it.
        let full: [(&[u8], bool); nameset::MAX_NAMES] =
            [(b"sixteen-bytes!!!", false); nameset::MAX_NAMES];
        assert_eq!(
            nameset::encode(&full, &mut buf),
            Some(nameset::BYTES),
            "the widest set this contract carries must fit its own buffer exactly",
        );
        let over: [(&[u8], bool); nameset::MAX_NAMES + 1] = [(b"a", false); nameset::MAX_NAMES + 1];
        assert_eq!(nameset::encode(&over, &mut buf), None, "one name too many");
        assert_eq!(
            nameset::encode(&[(b"seventeen-bytes!!", false)], &mut buf),
            None,
            "a name that cannot travel in a grant cannot travel in a set either",
        );
        assert_eq!(
            nameset::encode(&[(b"", false)], &mut buf),
            None,
            "an empty name is the terminator, so it cannot also be a name",
        );
        // And a buffer one byte short refuses rather than dropping the terminator, which would make
        // the reader run off the end of the set and into whatever followed it.
        let mut tight = [0u8; 8];
        assert_eq!(nameset::encode(&[(b"one.txt", false)], &mut tight), None);
        // The fit check runs before the record's first byte is laid, so a single-record refusal
        // leaves the buffer clean. (Refusing a later record can leave earlier ones behind; the
        // `None` is what the caller must honour either way.) Without this, a refusal could still
        // have scribbled most of a capability into a buffer somebody reuses.
        assert_eq!(
            tight, [0u8; 8],
            "a refused encode left partial capability bytes behind"
        );
        // A buffer too short for even the record is the same refusal, not a slice panic: the one
        // check covers the record and the terminator together.
        let mut shorter = [0u8; 7];
        assert_eq!(nameset::encode(&[(b"one.txt", false)], &mut shorter), None);
    }

    // --- Extended attributes (milestone 57) ---

    /// Scratch big enough for any blob this store can hold, so a test never fails on its own buffer.
    fn scratch() -> Vec<u8> {
        vec![0u8; xattr::store::MAX_BYTES]
    }

    /// **A name and a value survive the store round trip, at every length the limits allow**,
    /// including the two that a `u8` length byte and a `u16` value length are most likely to get
    /// wrong. A value read back short is a file that looks intact and is not, which is the exact
    /// failure DECISIONS §42 says must never be silent.
    #[test]
    fn an_attribute_round_trips_at_the_edges_of_both_limits() {
        use xattr::store;
        let widest_name = vec![b'n'; xattr::MAX_NAME];
        let widest_value = vec![0xABu8; xattr::MAX_VALUE];
        for (name, value) in [
            (&b"user.DOSATTRIB"[..], &b"0x20"[..]),
            (&b"a"[..], &b""[..]),
            (&widest_name[..], &widest_value[..]),
            (
                &b"user.com.apple.metadata:_kMDItemUserTags"[..],
                &[0u8; 1][..],
            ),
        ] {
            let mut out = scratch();
            let n = store::set(&[], name, xattr::RAW, value, &mut out).expect("set");
            assert_eq!(store::count(&out[..n]), 1);
            let (kind, got) = store::get(&out[..n], name).expect("the name we just wrote");
            assert_eq!(kind, xattr::RAW);
            assert_eq!(got, value, "the value did not come back as it went in");
        }
    }

    /// **The kind is carried and never interpreted**, which is the whole of "do not foreclose
    /// indexing" (`design/haiku-bfs-and-packages.md`). A BFS-style four-character code has to survive
    /// the store and the reply word, and a code with bit 31 set has to be refused rather than
    /// truncated into a type nobody wrote.
    #[test]
    fn a_type_code_survives_the_store_and_the_reply_word() {
        use xattr::store;
        // 'CSTR' and 'LONG', BFS's own spellings, plus the largest code the reply word can carry.
        for kind in [xattr::RAW, 0x4353_5452, 0x4C4F_4E47, xattr::MAX_KIND] {
            let mut out = scratch();
            let n = store::set(&[], b"typed", kind, b"body", &mut out).expect("set");
            assert_eq!(store::get(&out[..n], b"typed").unwrap().0, kind);

            let r = xattr::reply(kind, 4);
            assert!(r >= 0, "a reply word must never read as an error");
            assert_eq!(xattr::reply_kind(r), kind, "kind through the reply word");
            assert_eq!(
                xattr::reply_value_len(r),
                4,
                "length through the reply word"
            );
        }
        // Bit 31 is not the store's to give away: refuse it where it is written.
        let mut out = scratch();
        assert_eq!(
            store::set(&[], b"toobig", xattr::MAX_KIND + 1, b"x", &mut out),
            Err(22),
            "a kind that cannot ride home must be refused, not truncated",
        );
    }

    /// **Every limit refuses rather than truncating**, and each with its own errno, so a caller can
    /// tell "your name is wrong" from "your value is too big" from "this file is full". One errno for
    /// all three would make the honest failure §42 demands unactionable.
    #[test]
    fn each_limit_answers_with_its_own_word_and_nothing_is_clipped() {
        use xattr::store;
        let mut out = scratch();
        assert_eq!(
            store::set(&[], b"", xattr::RAW, b"v", &mut out),
            Err(xattr::ERANGE)
        );
        assert_eq!(
            store::set(
                &[],
                &vec![b'x'; xattr::MAX_NAME + 1],
                xattr::RAW,
                b"v",
                &mut out
            ),
            Err(xattr::ERANGE),
        );
        assert_eq!(
            store::set(&[], b"has\0nul", xattr::RAW, b"v", &mut out),
            Err(xattr::ERANGE),
            "a NUL would come back as a shorter name than went out",
        );
        assert_eq!(
            store::set(
                &[],
                b"big",
                xattr::RAW,
                &vec![0u8; xattr::MAX_VALUE + 1],
                &mut out
            ),
            Err(xattr::E2BIG),
        );

        // Fill a node to its ceiling, then ask for one more.
        let mut blob: Vec<u8> = Vec::new();
        for i in 0..xattr::MAX_COUNT {
            let mut next = scratch();
            let n = store::set(
                &blob,
                format!("a{i}").as_bytes(),
                xattr::RAW,
                b"v",
                &mut next,
            )
            .expect("within the count");
            blob = next[..n].to_vec();
        }
        assert_eq!(store::count(&blob), xattr::MAX_COUNT);
        assert_eq!(
            store::set(&blob, b"one-too-many", xattr::RAW, b"v", &mut out),
            Err(xattr::ENOSPC),
        );
        // And the ceiling is on NEW names only: replacing one of the sixteen still works, which is
        // what keeps a full file usable rather than frozen.
        let n =
            store::set(&blob, b"a0", xattr::RAW, b"replaced", &mut out).expect("replace at cap");
        assert_eq!(store::count(&out[..n]), xattr::MAX_COUNT);
        assert_eq!(store::get(&out[..n], b"a0").unwrap().1, b"replaced");
    }

    /// A replacement lands **in place**, so a listing does not shuffle when a value changes, and a
    /// new name appends. A client that walked a listing while updating values would otherwise see
    /// names move under it for no reason it could observe.
    #[test]
    fn a_replacement_keeps_its_position_and_a_new_name_appends() {
        use xattr::store;
        let (mut blob, mut out) = (Vec::new(), scratch());
        for name in [&b"one"[..], b"two", b"three"] {
            let n = store::set(&blob, name, xattr::RAW, b"v1", &mut out).unwrap();
            blob = out[..n].to_vec();
        }
        let names = |b: &[u8]| store::iter(b).map(|(n, ..)| n.to_vec()).collect::<Vec<_>>();
        assert_eq!(
            names(&blob),
            [b"one".to_vec(), b"two".to_vec(), b"three".to_vec()]
        );

        // A longer value in the middle: the order holds and only that record changes.
        let n = store::set(&blob, b"two", 7, b"a much longer value", &mut out).unwrap();
        assert_eq!(
            names(&out[..n]),
            [b"one".to_vec(), b"two".to_vec(), b"three".to_vec()]
        );
        assert_eq!(
            store::get(&out[..n], b"two"),
            Some((7, &b"a much longer value"[..]))
        );
        assert_eq!(
            store::get(&out[..n], b"one"),
            Some((xattr::RAW, &b"v1"[..]))
        );
    }

    /// Removing takes exactly one attribute and leaves the rest byte for byte, and removing what is
    /// not there is [`xattr::ENODATA`] rather than a quiet success. The caller asked about one
    /// specific thing.
    #[test]
    fn removing_takes_one_attribute_and_says_so_when_there_is_none() {
        use xattr::store;
        let (mut blob, mut out) = (Vec::new(), scratch());
        for name in [&b"keep-a"[..], b"drop", b"keep-b"] {
            let n = store::set(&blob, name, xattr::RAW, name, &mut out).unwrap();
            blob = out[..n].to_vec();
        }
        let n = store::remove(&blob, b"drop", &mut out).expect("remove");
        assert_eq!(store::count(&out[..n]), 2);
        assert_eq!(store::get(&out[..n], b"drop"), None);
        assert_eq!(
            store::get(&out[..n], b"keep-a"),
            Some((xattr::RAW, &b"keep-a"[..]))
        );
        assert_eq!(
            store::get(&out[..n], b"keep-b"),
            Some((xattr::RAW, &b"keep-b"[..]))
        );
        assert_eq!(
            store::remove(&out[..n], b"drop", &mut scratch()),
            Err(xattr::ENODATA)
        );
        // The last one out leaves an empty blob, which is how the server knows to delete the file.
        let mut b = out[..n].to_vec();
        for name in [&b"keep-a"[..], b"keep-b"] {
            let m = store::remove(&b, name, &mut out).unwrap();
            b = out[..m].to_vec();
        }
        assert_eq!(b.len(), 0);
    }

    /// **A truncated or zero-padded blob yields fewer attributes, never a torn one.** This is the
    /// property that matters after a crash: RedoxFS gives us whole transactions, but the reader must
    /// not be the weak link, and a value that came back with the next record's bytes on the end of it
    /// would be corruption the checksums could not see.
    #[test]
    fn a_torn_blob_reads_back_short_rather_than_wrong() {
        use xattr::store;
        let mut out = scratch();
        let mut blob = Vec::new();
        for name in [&b"one"[..], b"two"] {
            let n = store::set(&blob, name, xattr::RAW, b"value", &mut out).unwrap();
            blob = out[..n].to_vec();
        }
        let whole = store::count(&blob);
        assert_eq!(whole, 2);
        for cut in 0..blob.len() {
            let seen = store::iter(&blob[..cut]).count();
            assert!(
                seen < whole,
                "a cut at {cut} produced a whole second record"
            );
            for (name, _, value) in store::iter(&blob[..cut]) {
                assert!(
                    !name.is_empty() && value == b"value",
                    "a torn record was read as data"
                );
            }
        }
        // A zero-padded tail (what a partially written block leaves) stops the walk rather than
        // producing empty-named attributes.
        let mut padded = blob.clone();
        padded.extend_from_slice(&[0u8; 64]);
        assert_eq!(store::count(&padded), whole);
    }

    /// A listing round-trips through the page encoding, and the widest listing this contract permits
    /// fits one page **exactly**, which is what lets `LISTXATTR` answer without a cursor. If that
    /// stops being true, the verb needs a cursor and the note needs rewriting.
    #[test]
    fn the_widest_listing_fits_one_page_and_round_trips() {
        use xattr::store;
        assert_eq!(
            xattr::list::MAX_BYTES,
            PAGE,
            "the no-cursor property is this equality"
        );

        let mut blob = Vec::new();
        let mut out = scratch();
        let names: Vec<Vec<u8>> = (0..xattr::MAX_COUNT)
            .map(|i| {
                let mut n = vec![b'a' + i as u8; xattr::MAX_NAME];
                n[0] = b'0' + i as u8;
                n
            })
            .collect();
        for name in &names {
            let n = store::set(&blob, name, xattr::RAW, b"", &mut out).unwrap();
            blob = out[..n].to_vec();
        }
        let mut page = [0u8; PAGE];
        let used = store::list(&blob, &mut page);
        assert_eq!(used, PAGE, "sixteen widest names fill the page and no more");
        let back: Vec<Vec<u8>> = xattr::list::iter(&page[..used])
            .map(|n| n.to_vec())
            .collect();
        assert_eq!(back, names, "the listing did not come back as it went in");

        // A short buffer stops before the record that will not fit rather than splitting a name.
        let mut tiny = [0u8; 300];
        let n = store::list(&blob, &mut tiny);
        assert_eq!(xattr::list::iter(&tiny[..n]).count(), 1);
    }

    /// The two packed words carry their fields without bleeding, at the extremes. A value length
    /// that spilled into the kind would hand a caller somebody else's type; a kind that spilled into
    /// the length would hand it somebody else's bytes.
    #[test]
    fn the_attribute_words_carry_a_kind_and_a_length_without_bleeding() {
        for &(kind, len) in &[
            (xattr::RAW, 0usize),
            (1, xattr::MAX_VALUE),
            (xattr::MAX_KIND, xattr::MAX_VALUE),
            (0xFFFF_FFFF, 1), // a caller that ignores MAX_KIND: the reply masks rather than lies
        ] {
            let w1 = xattr::spec(kind, len as u64);
            assert_eq!(xattr::spec_kind(w1), kind, "kind through the request word");
            assert_eq!(
                xattr::spec_value_len(w1),
                len,
                "length through the request word"
            );
            let r = xattr::reply(kind, len);
            assert!(r >= 0);
            assert_eq!(xattr::reply_kind(r), kind & xattr::MAX_KIND);
            assert_eq!(xattr::reply_value_len(r), len);
        }
    }

    /// The attribute witness's bits must not overlap, for the reason every other witness's must not:
    /// the kernel test asserts an exact set, so two outcomes on one bit make a wrong verdict read as
    /// a right one. `EXPECTED` is pinned against them here so the two ISA legs cannot assert
    /// something the bits no longer spell.
    #[test]
    fn the_attribute_witness_bits_are_distinct_and_its_expectation_matches_them() {
        use fixture::attrs::*;
        let bits = [
            SET_AND_READ_BACK,
            LISTED,
            SURVIVED_RENAME,
            GONE_AFTER_REMOVE,
            GONE_AFTER_UNLINK,
            OVERSIZE_REFUSED,
            STORE_UNNAMEABLE,
            STORE_UNLISTED,
            ATTRS_FAILED,
        ];
        let mut seen = 0u64;
        for b in bits {
            assert_ne!(
                b, 0,
                "zero is the empty report; it cannot also be an outcome"
            );
            assert_eq!(seen & b, 0, "two attribute outcomes share a bit");
            seen |= b;
        }
        assert_eq!(
            EXPECTED,
            seen & !ATTRS_FAILED,
            "a correct run reports every outcome and no failure",
        );
        // The witness's fixture has to be usable: two distinct names it can create and rename
        // between, and a name and value the store will actually accept.
        assert_ne!(PROBE, PROBE_MOVED);
        assert!(grant::fits(PROBE.as_bytes()) && grant::fits(PROBE_MOVED.as_bytes()));
        assert!(xattr::valid_name(NAME));
        assert!(VALUE.len() <= xattr::MAX_VALUE);
        // Checked at COMPILE time: both sides are constants, so a fixture that fell back to the
        // default kind should fail the build rather than wait for the suite. The same discipline
        // `a_text_frame_and_a_verdict_are_told_apart_by_the_first_word` uses.
        const _: () = assert!(
            fixture::attrs::KIND != xattr::RAW,
            "a default kind would hide a layer that dropped it",
        );
    }

    /// The store's file names are the node ids, and two nodes never share one. That equality is what
    /// makes rename free and what makes a leaked blob dangerous, so it is pinned rather than assumed.
    #[test]
    fn a_store_file_is_named_for_exactly_one_node() {
        use xattr::store::file_name;
        assert_eq!(&file_name(0), b"00000000");
        assert_eq!(&file_name(1), b"00000001");
        assert_eq!(&file_name(0xdead_beef), b"deadbeef");
        assert_eq!(&file_name(u32::MAX), b"ffffffff");
        let mut seen = std::collections::BTreeSet::new();
        for id in [0u32, 1, 2, 16, 255, 4096, 0x0100_0000, u32::MAX] {
            assert!(
                seen.insert(file_name(id)),
                "two node ids share a store file"
            );
        }
    }

    /// **One record's exact bytes, as the format doc publishes them.** Recovery tooling reads this
    /// layout without this crate, so the bytes are the contract, not the code: a one-byte name
    /// length, four bytes of kind and two of value length, little-endian, then the name, then the
    /// value. The round-trip tests cannot pin this, because a writer and reader that agreed on a
    /// wrong header width would pass them together.
    #[test]
    fn an_attribute_record_lays_out_exactly_as_published() {
        use xattr::store;
        assert_eq!(store::RECORD_HEADER, 7);
        let mut out = scratch();
        let n = store::set(&[], b"ab", 0x0102_0304, b"xyz", &mut out).expect("set");
        assert_eq!(n, store::record_len(2, 3));
        assert_eq!(
            &out[..n],
            &[
                2, 0x04, 0x03, 0x02, 0x01, 3, 0, b'a', b'b', b'x', b'y', b'z'
            ],
        );
    }

    /// **An exactly-sized buffer is enough, and a short one refuses rather than crashes.** `set`'s
    /// doc sizes the caller's buffer at `blob.len() + record_len(...)`, so "exactly enough" has to
    /// mean enough; and the FS server hands `set` a buffer it sized itself, so a one-byte error
    /// there must come back [`xattr::ENOSPC`] instead of tearing down the serve loop on a slice
    /// bound.
    #[test]
    fn a_record_that_exactly_fits_is_written_and_a_short_buffer_refuses() {
        use xattr::store;
        let n = store::record_len(1, 1);
        let mut exact = vec![0u8; n];
        assert_eq!(store::set(&[], b"n", xattr::RAW, b"v", &mut exact), Ok(n));
        let mut short = vec![0u8; n - 1];
        assert_eq!(
            store::set(&[], b"n", xattr::RAW, b"v", &mut short),
            Err(xattr::ENOSPC),
        );
    }
    // The tests from here down each pin something milestone 85's mutation run showed nothing
    // pinned. The equivalences (every `|` over disjoint masked fields, where `^` cannot differ)
    // are recorded in notes/mutation-testing.md rather than tested.

    /// **A record wider than the contract is refused when a blob is rewritten, not carried
    /// forward.**
    ///
    /// [`xattr::store::set`] and [`xattr::store::remove`] re-emit every record they did not touch,
    /// and those lengths come off the **blob** rather than from a caller the entry points
    /// bounds-checked. A record's value length is a `u16`, so a torn or forged blob reaches 65535
    /// where [`xattr::MAX_VALUE`] is 3072, and the only thing standing between that and a rewritten
    /// blob nobody can read back is `write_record`'s own limit.
    ///
    /// Nothing exercised it before, because every other test here builds its blobs through `set`,
    /// whose `E2BIG` fires first. The mutation run found it as a `||` that could become `&&` with
    /// the whole suite still green.
    #[test]
    fn a_record_wider_than_the_contract_is_refused_when_a_blob_is_rewritten() {
        use xattr::store::{self, RECORD_HEADER};
        let over = xattr::MAX_VALUE + 1;
        let mut blob = vec![0u8; store::record_len(1, over)];
        blob[0] = 1;
        blob[5..7].copy_from_slice(&(over as u16).to_le_bytes());
        blob[RECORD_HEADER] = b'a';
        // The walker has to hand the oversized record over, or the rewrite path is never reached
        // and the refusals below would be about an empty blob instead.
        assert_eq!(store::iter(&blob).count(), 1);

        let mut out = vec![0u8; blob.len() + store::record_len(1, 1)];
        assert_eq!(
            store::set(&blob, b"b", xattr::RAW, b"v", &mut out),
            Err(xattr::ENOSPC),
        );
        assert_eq!(store::remove(&blob, b"b", &mut out), Err(xattr::ENOSPC));
    }

    /// The directory rights and the two named bundles, as numbers. `ALL` and `REMOVE_TREE` are
    /// built with `|`, and a mutant that made one an `&` collapsed the bundle to zero: a mount
    /// whose root carries nothing, with every test still green.
    #[test]
    fn the_rights_are_the_bits_they_say() {
        assert_eq!(
            [
                dir::ENUMERATE,
                dir::READ,
                dir::WRITE,
                dir::CREATE,
                dir::REMOVE,
                dir::DESCEND
            ],
            [1, 2, 4, 8, 16, 32]
        );
        assert_eq!(dir::ALL, 63);
        assert_eq!(dir::REMOVE_TREE, 1 | 16 | 32);
    }

    /// The two Verb predicates the caretakers key off, pinned per verb rather than by example. A
    /// `carries_len` that answered true (or false) for everything, and a `mutates` that lost a
    /// bit, would misroute requests in three programs at once, which is the table's own BUGS
    /// entry.
    #[test]
    fn the_verb_predicates_partition_the_table_as_documented() {
        // Operand::None: the length field means nothing.
        let no_len = [
            "CLOSE",
            "FSTAT",
            "TRUNCATE",
            "READDIR",
            "LISTXATTR",
            "STATFS",
        ];
        // needs_all intersects WRITE | CREATE | REMOVE.
        let mutating = [
            "WRITE",
            "CREATE",
            "TRUNCATE",
            "MKDIR",
            "RENAME",
            "UNLINK",
            "RMDIR",
            "SETXATTR",
            "REMOVEXATTR",
        ];
        for v in &verb::TABLE {
            assert_eq!(
                v.carries_len(),
                !no_len.contains(&v.name),
                "{}: carries_len",
                v.name
            );
            assert_eq!(
                v.mutates(),
                mutating.contains(&v.name),
                "{}: mutates",
                v.name
            );
        }
    }

    /// A directory entry at both of its limits: the longest encodable name in an exact-fit
    /// buffer, one byte over either limit refused, and the flag byte pinned.
    #[test]
    fn a_dirent_encodes_at_its_limits_and_not_past_them() {
        let name = vec![b'n'; 255];
        let mut out = vec![0u8; dirent::record_len(255)];
        assert_eq!(dirent::encode(&mut out, &name, true), Some(257));
        assert_eq!(out[0], dirent::IS_DIR);
        assert_eq!(out[1], 255);
        let mut short = vec![0u8; 256];
        assert_eq!(
            dirent::encode(&mut short, &name, true),
            None,
            "a buffer one byte short"
        );
        let long = vec![b'n'; 256];
        let mut out = vec![0u8; dirent::record_len(256)];
        assert_eq!(
            dirent::encode(&mut out, &long, false),
            None,
            "a name one byte too long"
        );
    }

    /// The per-file grant words, byte for byte: the rights bits, the spec word, and a name packed
    /// across the two argument words. The seventeen-byte case is the one that matters: the copy
    /// loop's bound is `i < MAX_NAME`, and `<=` there is an out-of-bounds write the caller's
    /// `fits` check never reaches.
    #[test]
    fn grant_words_pack_exactly() {
        assert_eq!([grant::READ, grant::WRITE], [1, 2]);
        let w = grant::spec(5, grant::WRITE);
        assert_eq!(w, 5 | (2 << 8));
        assert_eq!(grant::spec_len(w), 5);
        assert_eq!(grant::spec_rights(w), grant::WRITE);

        let (lo, hi) = grant::pack_name(b"abcdefghij");
        assert_eq!(lo, u64::from_le_bytes(*b"abcdefgh"));
        assert_eq!(hi, u64::from_le_bytes(*b"ij\0\0\0\0\0\0"));
        // Seventeen bytes: the packer stops at MAX_NAME rather than indexing past hi.
        let (lo17, hi17) = grant::pack_name(b"abcdefghijklmnopq");
        assert_eq!(lo17, u64::from_le_bytes(*b"abcdefgh"));
        assert_eq!(hi17, u64::from_le_bytes(*b"ijklmnop"));
    }

    /// A name set, byte for byte, in an exact-fit buffer: the length-and-flag byte per record
    /// (`IS_DIR` rides bit 7, above the 16-byte length), the terminator, and one byte less
    /// refused. The record cursor is `n + 1 + name.len()`, and either `+` rotting turns the
    /// layout into overlapping records with every length still adding up.
    #[test]
    fn a_nameset_lays_out_byte_for_byte() {
        let names: [(&[u8], bool); 2] = [(b"ab", false), (b"cde", true)];
        let mut out = [0u8; 8];
        assert_eq!(nameset::encode(&names, &mut out), Some(8));
        assert_eq!(
            out,
            [2, b'a', b'b', 3 | nameset::IS_DIR, b'c', b'd', b'e', 0]
        );
        let mut short = [0u8; 7];
        assert_eq!(nameset::encode(&names, &mut short), None);
    }

    /// Every witness verdict module is distinct single bits. Both ends of each QEMU test build
    /// their words from these same constants, so a shifted-to-zero bit is invisible at runtime:
    /// the claim it carried silently stops being checked. Distinct powers of two here is the
    /// whole defence.
    #[test]
    fn the_witness_bits_are_distinct_single_bits() {
        fn all_distinct_bits(bits: &[u64]) {
            for (i, &a) in bits.iter().enumerate() {
                assert!(a.is_power_of_two(), "bit {i} is {a:#x}, not a single bit");
                for &b in &bits[i + 1..] {
                    assert_ne!(a, b, "two claims share bit {a:#x}");
                }
            }
        }
        all_distinct_bits(&[
            fixture::escape::SECOND_FILE,
            fixture::escape::WROTE,
            fixture::escape::TRUNCATED,
            fixture::escape::CREATED,
            fixture::escape::FORGED_HANDLE,
            fixture::escape::GRANTED_READ_FAILED,
            fixture::escape::WROTE_ATTR,
            fixture::escape::GRANTED_ATTRS_FAILED,
        ]);
        all_distinct_bits(&[
            fixture::dirscape::REACHED_PARENT,
            fixture::dirscape::REACHED_SIBLING,
            fixture::dirscape::WALKED_UP,
            fixture::dirscape::WIDENED,
            fixture::dirscape::ENUMERATED,
            fixture::dirscape::DESCENDED,
            fixture::dirscape::CREATED,
            fixture::dirscape::WROTE,
            fixture::dirscape::RENAMED,
            fixture::dirscape::ENUMERATED_A_STRANGER,
            fixture::dirscape::FORGED_HANDLE,
            fixture::dirscape::GRANTED_ACCESS_FAILED,
            fixture::dirscape::MADE_A_DIR,
            fixture::dirscape::OPENED_ITS_OWN,
            fixture::dirscape::READ_ATTRS,
            fixture::dirscape::SET_AN_ATTR,
            fixture::dirscape::REACHED_AN_UNMATCHED_NAME,
        ]);
        all_distinct_bits(&[
            fixture::navscape::PWD_IS_ROOT,
            fixture::navscape::CLAMPED_AT_ROOT,
            fixture::navscape::WALKED_UP,
            fixture::navscape::ABSOLUTE_IS_MY_ROOT,
            fixture::navscape::REACHED_INNER,
            fixture::navscape::REACHED_SECRET,
            fixture::navscape::LISTED,
            fixture::navscape::SAW_INNER,
            fixture::navscape::SAW_SECRET,
            fixture::navscape::DESCENDED,
            fixture::navscape::RETURNED,
            fixture::navscape::CREATED,
            fixture::navscape::MADE_DIR,
            fixture::navscape::UNLINKED,
            fixture::navscape::HOLDER_KEPT_READING,
            fixture::navscape::NAME_GONE_AFTER_UNLINK,
            fixture::navscape::UNLINK_REFUSED_A_DIRECTORY,
            fixture::navscape::NAVIGATION_FAILED,
            fixture::navscape::RMDIR_REFUSED_NON_EMPTY,
            fixture::navscape::RMDIR_REMOVED_EMPTY,
            fixture::navscape::ABSOLUTE_REACHED_INNER,
            fixture::navscape::ABSOLUTE_REACHED_SECRET,
            fixture::navscape::ABSOLUTE_CLAMPED_AT_ROOT,
        ]);
        all_distinct_bits(&[
            fixture::globscape::EXPANDED,
            fixture::globscape::AGREED,
            fixture::globscape::EXCLUDED_A_STRANGER,
            fixture::globscape::NO_MATCH_REFUSED,
            fixture::globscape::PATTERN_IN_PATH_REFUSED,
            fixture::globscape::TEXT_UNTOUCHED,
            fixture::globscape::GLOB_FAILED,
        ]);
        all_distinct_bits(&[
            fixture::attrs::SET_AND_READ_BACK,
            fixture::attrs::LISTED,
            fixture::attrs::SURVIVED_RENAME,
            fixture::attrs::GONE_AFTER_REMOVE,
            fixture::attrs::GONE_AFTER_UNLINK,
            fixture::attrs::OVERSIZE_REFUSED,
            fixture::attrs::STORE_UNNAMEABLE,
            fixture::attrs::STORE_UNLISTED,
            fixture::attrs::ATTRS_FAILED,
        ]);
        assert_eq!(
            fixture::attrs::EXPECTED,
            255,
            "the eight required claims, bits 0 through 7"
        );
    }
    /// **A single-page request still means one page**, which is the compatibility claim
    /// [`fs::TRANSFER_PAGES`] makes in prose, checked through the packing rather than beside it.
    ///
    /// The arithmetic the channel has to satisfy is a `const` block next to the constants
    /// themselves, because a build is the only artifact that can be wrong about it and a test does
    /// not run in the EL0 build. What is left here is the round trip, which needs the packing.
    #[test]
    fn a_single_page_request_still_means_one_page() {
        let w0 = fs::req(fs::READ, 1, PAGE as u64);
        assert_eq!(fs::req_len(w0), PAGE);
        assert_eq!(fs::req_handle(w0), 1);
        assert_eq!(op(w0), fs::READ);

        // And the largest the channel permits survives the same 40-bit field.
        let w0 = fs::req(fs::WRITE, 1, fs::TRANSFER_MAX as u64);
        assert_eq!(fs::req_len(w0), fs::TRANSFER_MAX);
        assert_eq!(op(w0), fs::WRITE);
    }
}
