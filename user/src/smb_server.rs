//! **The SMB adapter** (milestone 54): a network file service a Mac can mount.
//!
//! The roadmap block names the shape and this program is exactly it: an adapter holding **one
//! network endpoint and one share**, speaking SMB2 on the wire side and nothing but the `Share`
//! seam on the file side. All protocol logic lives in `smb_proto` (host-tested, rule 7); this
//! binary owns only the IO: listen and accept through the socket contract (milestone 107's
//! inbound half), reassemble the direct-TCP framing from `RECV`'s bounded chunks, hand each
//! message to `smb_proto::server::Connection`, and push the answer back out through `SEND`.
//!
//! The share has two backings behind the one `Share` seam, chosen by `arg2`, which also says
//! which **direction** the share is served in (the write path made that a separate question):
//!
//! - **The fs_proto-backed share** ([`FsShare`]), the milestone's real one: this program holds a
//!   directory capability into the FS server (the endpoint IS the capability, DECISIONS §27) and
//!   answers every `Share` question with `fs_proto` verbs, so a client of the mount is reading
//!   and writing RedoxFS bytes. This is what both the test boot and the serve boot wire when a
//!   RedoxFS disk is attached, both of them read-write.
//! - **The fixture** (`smb_proto::share::FIXTURE`), files baked into this binary: the no-disk
//!   fallback, kept because it lets the whole protocol path run and gate with no FS service in
//!   the boot, and because a wire bug is easier to hunt against a share that cannot be wrong.
//!   Read-only, and it is `Share`'s worked example of a backing that says so.
//!
//! All protocol code is indifferent to which one it is serving; see notes/smb.md.
//!
//! # Identity (milestone 54's last item)
//!
//! `arg2` also says **who may connect**, and [`CredentialAuthenticator`] is the answer that is not
//! "everyone": a session must present an NTLMv2 proof that milestone 65's credential service
//! accepts. This program holds one more endpoint for that and no key at all; read that type's doc
//! for what it can and cannot do, and `smb_proto::authenticator` for why the seam carries no key
//! material in either direction.
//!
//! **Where access control lives, and it is one place.** A [`Verdict`] decides whether a session
//! exists. It does not decide what the session may do: the share's rights are the rights of the one
//! directory capability in slot [`FS`], identical for every session, enforced by the FS server.
//! Per-user rights therefore mean per-user adapters, one directory capability each, which
//! milestone 47's rights split already expresses. A share whose rights lived both here and in the
//! grant would be a share whose rights can disagree with themselves.
//!
//! # Capability contract
//! - slot 0: the report endpoint (WRITE)
//! - slot 1: the `Stack` endpoint (WRITE), shared with the stack's other client if any
//! - slot 2: an untyped budget (to mint and delegate the shared frame)
//! - slot 3: the file-service endpoint (WRITE), the directory capability the share serves.
//!   Present only when `arg2` says fs-backed, along with [`FS_VA`] mapped to the whole file
//!   channel shared with the FS server (`fs::TRANSFER_MAX` bytes, not one page).
//! - slot 4: the credential service's **verify** endpoint (WRITE), present only when `arg2` says
//!   authenticated, along with [`CRED_VA`] mapped to the page shared with that service. Not the
//!   provision endpoint, which does not exist any more by the time this program runs.
//! - arg0: connections to serve before reporting OK; 0 means serve forever (the demo boot)
//! - arg1: the TCP port to listen on (445 in the demo boot; the test grants a neighbour of the
//!   echo gate's port instead, see the kernel test)
//! - arg2: which share, in which direction, and to whom. 0 is the fixture, 1 the fs_proto-backed
//!   share read-only, 2 read-write to guests, 3 read-write to a proven identity
//!   ([`SHARE_FIXTURE`] and its three neighbours). The write path split this from a flag because a
//!   **read-only view of the real filesystem** is a thing a boot should be able to wire and a
//!   boolean could not say; identity added a third question a boolean could not say either.
//!
//! # Socket ids
//!
//! The listener is sid 2 and the connection sid 3, **not** 0 and 1: in the combined inbound gate
//! this program shares one `Stack` endpoint with `socket_test_client`, whose exchange owns 0 and
//! 1, and a stack endpoint's socket table is per-endpoint (`socket_proto`'s BUGS: clients sharing an
//! endpoint are indistinguishable to the server). Two clients, four sids, no collision.
//!
//! # BUGS
//!
//! - **A listing still costs a walk.** `QUERY_DIRECTORY` re-walks `READDIR` from cursor 0 for
//!   each entry and pays an OPEN + FSTAT + CLOSE to learn its size, because `fs_proto`'s dirent
//!   records carry name and kind only. Reads and writes no longer pay it: the write path made
//!   the `Share` id the FS server's own handle, and milestone 55 made the transfer the whole file
//!   channel, so a 64 KiB read or write is **one** `fs::READ` or `fs::WRITE` and nothing else.
//! - **A handle is never reclaimed if a client vanishes mid-connection.** The FS server's handle
//!   table is per server, and this adapter closes what it opened only on CLOSE or when the
//!   `Connection` is dropped at end of connection; a connection torn down between a CREATE and
//!   its CLOSE leaks one FS handle for the life of the FS server. Bounded by `MAX_HANDLES` per
//!   connection, unbounded across connections, and the fix is the `Connection` telling the share
//!   what to release, which is a seam change rather than a line.
//! - **A path costs one descent per component, on every call.** `open("a\\b\\c")` is two
//!   `fs::OPENDIR`s and an `fs::OPEN`, and the two directory handles are opened and closed again
//!   for the next call. There is no cache because a cache would have to be invalidated by every
//!   other client of the same FS server, and this program cannot see them. Reads and writes are
//!   unaffected: they go through the handle CREATE minted.
//! - **A directory cannot be moved into another directory**, only renamed in place, because
//!   `fs_proto::fs::RENAME` refuses it (its doc argues the boundary). It arrives at the client as
//!   an unexpected-IO status rather than as something more useful, which is a gap worth naming: an
//!   `EINVAL` from that verb means two different things and this share cannot tell which.
//! - **Upper-case FS names are unreachable.** The wire folds names to lower-case ASCII before
//!   lookup, and the FS is case-sensitive, so only lower-case names on disk can be opened.
//! - **One connection at a time.** A second client connecting while one is served is refused by
//!   TCP (the listener's backlog is one, `socket_proto`'s BUGS) or waits for the re-arm. macOS uses
//!   one connection per mount.
//! - **A dropped connection costs a 15 s stall**: `RECV` on a dead connection runs into
//!   `net_stack`'s bounded wait before it reports failure, and only then does this server re-arm.
//!   A clean unmount (LOGOFF) is detected and costs nothing.
//! - **The demo boot still serves guests, and so the thing a person actually runs is still open to
//!   everyone who can reach the port.** `--features smb_serve` wires `SHARE_FS_READ_WRITE`, not
//!   [`SHARE_FS_AUTHENTICATED`], because there is no way to *tell* that boot a password: the only
//!   thing in the tree that provisions the credential store is a test program with a published
//!   fixture in it, and shipping a demo whose password Microsoft printed would be worse than a
//!   labelled guest share. The gate is authenticated; the demo says guest in its own banner. What
//!   closes this is a provisioning path, not a flag.
//! - **An authenticated share authenticates exactly one account**, because it is configured with one
//!   resource ([`CredentialAuthenticator::resource`], which also records why that configuration is
//!   in the wrong place). Several accounts mean several adapters today.
//! - **Sessions are not signed.** A proven session is proven at setup and unprotected afterwards, so
//!   an attacker on the path can inject into it. That is `smb_proto`'s scope note as much as this
//!   program's, and it is what `SessionBaseKey` exists for: the credential service publishes one and
//!   this adapter does not ask for it. Nothing here is worse than the guest share it replaces, and
//!   the honest reading is that identity buys authentication of the *client*, not integrity of the
//!   *stream*.
//! - **The server challenge is `now()`**, a clock this program holds rather than entropy it does not.
//!   Two connections in the same tick would repeat a challenge, and a repeated challenge is what
//!   makes a captured proof replayable. It is a real gap: the fix is an entropy capability, which is
//!   milestone 56's service and one more slot.
//! - The remaining protocol-level limitations (ASCII names, discarded timestamps) are `smb_proto`'s
//!   and are listed in that crate's header.
//!
//! Name: unrecorded. Provisional, minted by milestone 54's lane on 2026-08-15: the program is the
//! server half of SMB, and `fs_server` set the `<protocol>_server` shape. Expect ratification to
//! reconsider it beside `net_stack` (which serves the socket contract but is not `socket_server`).

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use abi::{endpoint, frame as fr, rights, untyped as ut};
use fs_proto::{dir, dirent, fs, xattr};
use smb_proto::authenticator::{Attempt, Authenticator, NoIdentity, Verdict};
use smb_proto::path::Path;
use smb_proto::server::Connection;
use smb_proto::share::{DirId, Entry, Error, FIXTURE, FileId, ROOT_DIR, Share, Volume};
use smb_proto::{MAX_MESSAGE, XPORT_LEN, xport_parse, xport_write};
use socket_proto::{
    DATA_MAX, LISTEN_DENIED, LISTEN_GRANTED, LISTEN_IN_USE, OFF_LEN, OFF_PAYLOAD, OP_ACCEPT,
    OP_ATTACH_FRAME, OP_CLOSE, OP_LISTEN, OP_RECV, OP_SEND, REP_ERR, REP_OK, req,
};
use user_rt::{call, exit, invoke, now, send};

const REPORT: u64 = 0;
const STACK: u64 = 1;
const UNTYPED: u64 = 2;
/// The file-service endpoint: the share's whole authority to the filesystem (see the module
/// header; present only in the fs-backed wiring).
const FS: u64 = 3;
/// **The credential service's verify endpoint** (milestone 65), present only in the authenticated
/// wiring. It is the whole of this program's authority over identity, and what it is *not* is the
/// point: there is no message on it that yields a key, so revoking it ends the ability to
/// authenticate and no compromise of this process yields the ability to forge.
const CRED: u64 = 4;

/// Where the **base** of the channel shared with the FS server is mapped (a name out, file bytes
/// and directory listings back). Must match the kernel-side wiring in
/// `kernel/src/user/virtio_service.rs`.
///
/// **It is [`fs::TRANSFER_MAX`] bytes wide, not one page** (milestone 55, on milestone 138 step 3's
/// contract), and the kernel's wiring maps every page of it here. This program is a client that
/// uses all of it: [`FsShare::read`] and [`FsShare::write`] ask for up to the whole channel in one
/// request, which is the only reason an SMB client's 64 KiB transfer is one trip through the FS
/// server rather than sixteen. A client may not ask for more than it mapped and nothing checks that
/// it did not (`fs_proto::fs::TRANSFER_PAGES`' marked foot gun), so the two sides agreeing is a
/// property of those two files reading the same constant, not of anything at runtime.
///
/// Everything else this program puts here (a name, a `READDIR` page, a `statfs` record, a rename's
/// two names) still lives in the **first page**, which is the FS server's own clamp discipline: a
/// reply whose length the server chooses stays inside one page.
const FS_VA: u64 = 0x0000_0000_00B0_0000;

/// Where the page shared with the **credential** service is mapped. A different frame from
/// [`FS_VA`]'s and from the network one, because it is shared with a different process; must match
/// the kernel-side wiring like every VA here.
const CRED_VA: u64 = 0x0000_0000_00C0_0000;

/// **What `arg2` says the share is.** Four values rather than a flag: the write path made "which
/// backing" and "which direction" two separate questions, and identity made "who may connect" a
/// third. A boolean answering all three would have made a read-only real share unwireable and an
/// authenticated one unsayable. The seams are what enforce these; this is only how the boot says
/// which it wants.
const SHARE_FIXTURE: u64 = 0;
const SHARE_FS_READ_ONLY: u64 = 1;
const SHARE_FS_READ_WRITE: u64 = 2;
/// The fs-backed share, read-write, **and no guests**: a session must present an NTLMv2 proof that
/// the credential service accepts. Needs slot [`CRED`] and the page at [`CRED_VA`].
const SHARE_FS_AUTHENTICATED: u64 = 3;

/// See the module header on why these are 2 and 3.
const LISTEN_SID: u64 = 2;
const CONN_SID: u64 = 3;

/// Where the shared frame is mapped in this process.
const FRAME_VA: u64 = 0x0000_0000_00A0_0000;

/// The success word the kernel test asserts, and the "listening" word the serve-forever boot
/// prints on. Distinct stage codes (0xE1xx, disjoint from `socket_test_client`'s 0xE0xx) name
/// what failed.
const OK: u64 = 1;

/// How many times an `ACCEPT` that timed out (nobody connected within `net_stack`'s bounded wait)
/// is retried before the test role gives up. The host prober connects every 100 ms, so one
/// timeout already means something is wrong; four means it is not transient.
const ACCEPT_TRIES: u32 = 4;

/// The inbound reassembly buffer: one full message plus one `RECV`'s worth of the next.
const RX_CAP: usize = MAX_MESSAGE + XPORT_LEN + DATA_MAX;

/// In `.bss`, not on the two-page stack: the reassembly and response buffers are ~64 KiB each.
static mut RX: [u8; RX_CAP] = [0; RX_CAP];
static mut TX: [u8; XPORT_LEN + MAX_MESSAGE] = [0; XPORT_LEN + MAX_MESSAGE];

/// One thread per address space (DECISIONS §33), so there is no second reference. Taking the raw
/// pointer first is what `static_mut_refs` asks for.
fn rx() -> &'static mut [u8] {
    let p = &raw mut RX;
    // SAFETY: see above.
    unsafe { &mut *p }
}
/// Same reasoning as [`rx()`].
fn tx() -> &'static mut [u8] {
    let p = &raw mut TX;
    // SAFETY: see above.
    unsafe { &mut *p }
}

fn w8(va: u64, v: u8) {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::write_volatile(va as *mut u8, v) }
}
fn r8(va: u64) -> u8 {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::read_volatile(va as *const u8) }
}

// ============================================================================================
// The fs_proto-backed share: milestone 54's second act. Everything below and nothing above
// touches the FS endpoint; the protocol machine sees only the `Share` trait.
// ============================================================================================

/// One READDIR reply's records, copied out of the shared page before parsing. In `.bss` (the
/// six-page stack cannot hold a page-sized buffer), single-threaded like [`RX`].
static mut DIR: [u8; fs_proto::PAGE] = [0; fs_proto::PAGE];

/// The name of the entry the last listing walk resolved; what an [`Entry`]'s `name` borrows.
/// **Valid only until the next `Share` call**: the next walk overwrites it. That is sound for
/// `smb_proto::server`, which copies every entry into its response before asking for the next
/// one, and it is the price of a share with no allocator; a consumer that held two entries at
/// once would need this to become a table.
static mut NAME: [u8; 255] = [0; 255];

/// Copy `bytes` to the start of the FS-shared page (a name to resolve).
fn fs_put(bytes: &[u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        w8(FS_VA + i as u64, b);
    }
}

/// Ask the FS server to close `handle`. Nothing to do on failure: the handle is the server's
/// bookkeeping, and a close that failed left nothing this program holds.
fn fs_close(handle: u64) {
    let _ = call(FS, fs::req(fs::CLOSE, handle, 0), 0);
}

/// One `READDIR` page of directory `dir` starting at entry `cursor`, copied into [`DIR`] and
/// returned as records ready for [`dirent::iter`]. Empty at the end of the listing, and on an FS
/// refusal (the module BUGS: errors degrade to absence).
fn fs_readdir(dir: u64, cursor: u64) -> &'static [u8] {
    let (r0, _) = call(FS, fs::req(fs::READDIR, dir, 0), cursor);
    if (r0 as i64) <= 0 {
        return &[];
    }
    let n = (r0 as usize).min(fs_proto::PAGE);
    let p = &raw mut DIR;
    // SAFETY: one thread per address space (DECISIONS §33), and the previous reply's slice is
    // dead before this borrow: every caller consumes one page before asking for the next.
    let d = unsafe { &mut *p };
    for (i, b) in d.iter_mut().take(n).enumerate() {
        *b = r8(FS_VA + i as u64);
    }
    &d[..n]
}

/// Turn an FS reply's negated errno into the share model's word for it. The one place the
/// translation happens, so a client meets one status per condition (`smb_proto`'s `status_for` is
/// the other half of the same chain). An errno this share has no word for is [`Error::Io`] rather
/// than silence, which is the whole point of the trait having an error channel at all.
fn fs_error(r0: u64) -> Error {
    match fs_proto::reply_errno(r0 as i64) {
        Some(2) => Error::NotFound,                // ENOENT
        Some(17) => Error::Exists,                 // EEXIST
        Some(dir::EISDIR) => Error::IsDirectory,   // 21
        Some(dir::ENOTDIR) => Error::NotDirectory, // 20
        Some(dir::ENOTEMPTY) => Error::NotEmpty,   // 39
        Some(dir::EROFS) | Some(dir::EPERM) => Error::ReadOnly,
        Some(xattr::ENOSPC) => Error::NoSpace, // 28
        Some(36) => Error::NameTooLong,        // ENAMETOOLONG
        _ => Error::Io,
    }
}

/// Walk the listing of directory `dir` to entry `index`, leaving its name in [`NAME`]. Returns the
/// name's length and whether the entry is a directory.
fn fs_nth(dir: u64, index: usize) -> Option<(usize, bool)> {
    let mut cursor = 0usize;
    loop {
        let page = fs_readdir(dir, cursor as u64);
        if page.is_empty() {
            return None;
        }
        let mut in_page = 0usize;
        for (nm, is_dir) in dirent::iter(page) {
            if cursor + in_page == index {
                let p = &raw mut NAME;
                // SAFETY: single-threaded; no `Entry` borrowing NAME is live across a Share
                // call (see NAME's doc).
                let buf = unsafe { &mut *p };
                let l = nm.len().min(buf.len());
                buf[..l].copy_from_slice(&nm[..l]);
                return Some((l, is_dir));
            }
            in_page += 1;
        }
        if in_page == 0 {
            return None; // records that fit no page would loop forever; treat as the end
        }
        cursor += in_page;
    }
}

/// The name [`fs_nth`] just resolved, as the slice an [`Entry`] carries.
fn fs_name(len: usize) -> &'static [u8] {
    let p = &raw const NAME;
    // SAFETY: single-threaded, and stable until the next listing walk (NAME's doc).
    unsafe { &(&*p)[..len] }
}

/// **The share backed by the FS server**: every question answered with `fs_proto` verbs over
/// the directory capability in slot [`FS`], so what the mount reads and writes is RedoxFS.
///
/// **The [`FileId`] is the FS server's own handle**, which is the write path's contribution and
/// two fixes in one. It retires the open-per-request cost the read path recorded (a 64 KiB READ
/// cost a listing walk, an OPEN, sixteen reads and a CLOSE; it now costs sixteen reads), and it
/// makes a handle survive a directory that moves under it, which is what a writable share does
/// every time a client creates a file.
///
/// **A [`DirId`] is the FS server's handle too**, and [`ROOT_DIR`] is `fs::ROOT` because both are
/// zero: the share's root directory and the endpoint's bound directory are the same object, so the
/// mapping between the two id spaces is the identity function and there is nothing to get wrong.
///
/// `writable` comes from the boot (`arg2`), not from probing the capability: the protocol layer
/// consults it before it asks anything, so a read-only share is read-only whatever the directory
/// capability behind it would have permitted. A share wired writable over a capability that
/// lacks `dir::WRITE` still refuses, one layer down, with the FS server's own `EROFS`.
///
/// Name: provisional, minted by this milestone's lane on 2026-08-15 (`FixtureShare` set the
/// `<what backs it>Share` shape).
struct FsShare {
    writable: bool,
}

/// A directory handle held for the duration of one `Share` call, and the rule for giving it back.
///
/// A path walk opens one FS handle per component and must close every one of them, including on
/// the error paths, or the FS server's table grows for the life of the boot. Pairing the handle
/// with "did I open it" is what lets one `close` call at the end of every method be correct for
/// both the root (which nobody opened) and a descent (which somebody did).
struct Descent {
    handle: u64,
    owned: bool,
}

impl Descent {
    /// Give the handle back if it was ours. Calling this twice is harmless; not calling it is the
    /// leak, so every method that makes one ends with it on every path.
    fn close(self) {
        if self.owned {
            fs_close(self.handle);
        }
    }
}

impl FsShare {
    /// **The rights this share asks for when it descends.** Exactly what it will use and no more:
    /// `fs_proto::fs::OPENDIR` refuses with `EPERM` when the intersection with the parent's rights
    /// is smaller than the request, so asking for `dir::ALL` on a read-only share would fail on a
    /// capability that was correctly narrowed.
    ///
    /// A share wired read-write over a capability that lacks one of these gets that `EPERM`, which
    /// `fs_error` reports as [`Error::ReadOnly`]: through this capability, that is what the
    /// directory is. The refusal arrives at the descent rather than at the write, which is earlier
    /// and therefore better.
    fn descend_rights(&self) -> u64 {
        if self.writable {
            dir::ALL
        } else {
            dir::ENUMERATE | dir::READ | dir::DESCEND
        }
    }

    /// **Walk to the directory `path` names**, one `fs::OPENDIR` per component.
    ///
    /// The cost is the recorded one: a path N components deep costs N descents per call, because
    /// this share holds no cache and a cache would have to be invalidated by every other client of
    /// the same FS server. See the module BUGS.
    fn descend(&self, path: Path<'_>) -> Result<Descent, Error> {
        let mut at = Descent {
            handle: fs::ROOT,
            owned: false,
        };
        for comp in path.components() {
            if comp.len() > fs_proto::PAGE {
                at.close();
                return Err(Error::NameTooLong);
            }
            fs_put(comp);
            let (r0, _) = call(
                FS,
                fs::req(fs::OPENDIR, at.handle, comp.len() as u64),
                self.descend_rights(),
            );
            at.close();
            if (r0 as i64) < 0 {
                // Every component but the last is a *path* component, so a failure walking one is
                // "path not found" rather than "name not found": that is what tells a client to
                // make the parent first. `NotDirectory` survives as itself, being more specific.
                return Err(match fs_error(r0) {
                    Error::NotFound => Error::PathNotFound,
                    e => e,
                });
            }
            at = Descent {
                handle: r0,
                owned: true,
            };
        }
        Ok(at)
    }

    /// Walk to the **parent** of `path` and hand back the parent's handle with the leaf name. The
    /// shape every name-taking verb needs: `fs_proto` resolves a single component under a handle,
    /// never a path.
    fn parent_of<'p>(&self, path: Path<'p>) -> Result<(Descent, &'p [u8]), Error> {
        if path.is_root() {
            return Err(Error::IsDirectory);
        }
        let name = path.name();
        if name.len() > fs_proto::PAGE {
            return Err(Error::NameTooLong);
        }
        Ok((self.descend(path.parent())?, name))
    }

    /// One name-taking verb under the path's parent, with the parent closed on every path out.
    /// `w1` is the request's second word (a rights mask for the descending verbs, 0 otherwise).
    fn at_parent(&self, path: Path<'_>, op: u64, w1: u64) -> Result<u64, Error> {
        let (parent, name) = self.parent_of(path)?;
        fs_put(name);
        let (r0, _) = call(FS, fs::req(op, parent.handle, name.len() as u64), w1);
        parent.close();
        if (r0 as i64) < 0 {
            return Err(fs_error(r0));
        }
        Ok(r0)
    }
}

impl Share for FsShare {
    fn writable(&self) -> bool {
        self.writable
    }

    fn open(&self, path: Path<'_>) -> Result<FileId, Error> {
        self.at_parent(path, fs::OPEN, 0)
    }

    fn open_dir(&self, path: Path<'_>) -> Result<DirId, Error> {
        if path.is_root() {
            return Ok(ROOT_DIR);
        }
        // Not `at_parent`: the last component is a directory here, so the whole path is a descent
        // and the walk already does exactly this. Going through the parent would open the same
        // handle twice.
        let (parent, name) = self.parent_of(path)?;
        fs_put(name);
        let (r0, _) = call(
            FS,
            fs::req(fs::OPENDIR, parent.handle, name.len() as u64),
            self.descend_rights(),
        );
        parent.close();
        if (r0 as i64) < 0 {
            return Err(fs_error(r0));
        }
        Ok(r0)
    }

    fn close_dir(&self, dir: DirId) {
        // The bound directory is not something this program opened, and closing it would take the
        // whole share away from every later request. `smb_proto` never asks, and this is the
        // second line rather than the first.
        if dir != ROOT_DIR {
            fs_close(dir);
        }
    }

    fn create(&self, path: Path<'_>) -> Result<FileId, Error> {
        self.at_parent(path, fs::CREATE, 0)
    }

    fn mkdir(&self, path: Path<'_>) -> Result<DirId, Error> {
        self.at_parent(path, fs::MKDIR, self.descend_rights())
    }

    fn entry(&self, dir: DirId, index: usize) -> Option<Entry<'_>> {
        let (len, is_dir) = fs_nth(dir, index)?;
        let size = if is_dir {
            0
        } else {
            // OPEN + FSTAT + CLOSE per entry: the listing records carry no size (fs_proto's
            // dirent is name and kind only). This is the one path that still pays per entry,
            // because a listing is a walk by nature.
            fs_put(fs_name(len));
            let (h, _) = call(FS, fs::req(fs::OPEN, dir, len as u64), 0);
            if (h as i64) < 0 {
                0
            } else {
                let (s, _) = call(FS, fs::req(fs::FSTAT, h, 0), 0);
                fs_close(h);
                if (s as i64) < 0 { 0 } else { s }
            }
        };
        Some(Entry {
            name: fs_name(len),
            size,
            is_dir,
        })
    }

    /// **What the image reports** (`fs_proto::fs::STATFS`, milestone 54). Asked on the bound
    /// directory, which every wiring of this program holds; the verb needs no right, so a share
    /// over a narrowed capability answers this as well as one over the root.
    ///
    /// `None` on a refusal rather than a guess: the protocol layer's fallback is a stated nominal
    /// figure, and a wrong number here would be an unstated one.
    fn statfs(&self) -> Option<Volume> {
        let (r0, _) = call(FS, fs::req(fs::STATFS, fs::ROOT, 0), 0);
        if (r0 as i64) < 0 {
            return None;
        }
        let n = (r0 as usize).min(fs_proto::PAGE);
        let mut rec = [0u8; fs_proto::statfs::LEN];
        for (i, b) in rec.iter_mut().enumerate().take(n) {
            *b = r8(FS_VA + i as u64);
        }
        let (block_size, total_blocks, free_blocks) = fs_proto::statfs::decode(&rec[..n])?;
        Some(Volume {
            block_size,
            total_blocks,
            free_blocks,
        })
    }

    /// **Make the image durable** (`fs_proto::fs::SYNC`, milestone 55). Asked on the bound
    /// directory for [`FsShare::statfs`]'s reason: the answer is about the storage behind this
    /// capability rather than about any node in it, and `fs::ROOT` is the handle every wiring of
    /// this program holds.
    ///
    /// **A refusal is returned, never smoothed over.** This is the one `Share` method where a
    /// convenient `Ok(())` would be a lie with a consequence: SMB2's `FLUSH` is what a backup
    /// client waits on before it believes its data is safe, and this server tells macOS it honours
    /// a full sync (`smb_proto::apple::VOLUME_FULL_SYNC`). `EOPNOTSUPP` from a device with no
    /// `VIRTIO_BLK_F_FLUSH` arrives here as [`Error::Io`], which the protocol layer turns into
    /// `STATUS_UNEXPECTED_IO_ERROR`, and a client that meets that knows not to trust the write.
    ///
    /// The count `fs::SYNC` answers with is discarded here, because SMB's FLUSH response has
    /// nowhere to carry it and no client would read it. It is not wasted: it is what the gate's
    /// in-guest witness reads to prove each sync was a fresh device round trip rather than a
    /// constant (`fs_proto::fixture::durability`).
    fn sync(&self) -> Result<(), Error> {
        let (r0, _) = call(FS, fs::req(fs::SYNC, fs::ROOT, 0), 0);
        if (r0 as i64) < 0 {
            return Err(fs_error(r0));
        }
        Ok(())
    }

    fn size(&self, file: FileId) -> u64 {
        let (s, _) = call(FS, fs::req(fs::FSTAT, file, 0), 0);
        if (s as i64) < 0 { 0 } else { s }
    }

    fn close(&self, file: FileId) {
        fs_close(file);
    }

    /// **One SMB read is one `fs::READ`**, for every size a client may ask for (milestone 55).
    ///
    /// The loop is still a loop and still has to be, because `out` is whatever the protocol layer
    /// sized and the contract's ceiling is [`fs::TRANSFER_MAX`]; what changed is that the two
    /// numbers now meet. `smb_proto::MAX_TRANSACT` is 64 KiB, which is the `MaxReadSize` this
    /// server negotiates and the largest read any client may therefore issue, and `TRANSFER_MAX` is
    /// 64 KiB as well, so the loop runs **once** for the largest transfer on the wire. It ran
    /// sixteen times before, and that, rather than anything in the FS server, is why milestone 138
    /// step 3's 5.67x sequential read did not reach a mounted share.
    ///
    /// The ceiling is read from the contract rather than spelled here on purpose: a future change
    /// to `fs::TRANSFER_PAGES` reaches this program without anyone editing it, and a second
    /// hardcoded 65536 in the tree would be a number that can disagree with the region behind it.
    fn read(&self, file: FileId, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        let mut done = 0usize;
        while done < out.len() {
            let want = (out.len() - done).min(fs::TRANSFER_MAX);
            let (r0, _) = call(
                FS,
                fs::req(fs::READ, file, want as u64),
                offset + done as u64,
            );
            if (r0 as i64) < 0 {
                // A refusal partway through a multi-page read is still a refusal, not a short
                // read: reporting the bytes so far would be the silent truncation the trait's
                // error channel exists to stop.
                return Err(fs_error(r0));
            }
            let got = (r0 as usize).min(want);
            if got == 0 {
                break; // end of file
            }
            for i in 0..got {
                out[done + i] = r8(FS_VA + i as u64);
            }
            done += got;
            if got < want {
                break; // a short read is EOF; asking again would answer 0 anyway
            }
        }
        Ok(done)
    }

    /// **One SMB write is one `fs::WRITE`**, on [`FsShare::read`]'s reasoning and with more riding
    /// on it: a backup is writes, and milestone 138 step 1 measured a write's fixed term at 690 us
    /// per request against 87% of the request. Paying it once per 64 KiB instead of once per 4 KiB
    /// is the whole of what milestone 55 takes from that milestone.
    ///
    /// This is the client-chosen length, which is what entitles it to the whole channel:
    /// `fs_proto`'s serve loop clamps `READ` and `WRITE` to `fs::TRANSFER_MAX` and everything whose
    /// length the **server** picks to one page, and this share sits on the correct side of that
    /// split by only ever growing these two.
    fn write(&self, file: FileId, offset: u64, data: &[u8]) -> Result<usize, Error> {
        let mut done = 0usize;
        while done < data.len() {
            let chunk = (data.len() - done).min(fs::TRANSFER_MAX);
            for (i, &b) in data[done..done + chunk].iter().enumerate() {
                w8(FS_VA + i as u64, b);
            }
            let (r0, _) = call(
                FS,
                fs::req(fs::WRITE, file, chunk as u64),
                offset + done as u64,
            );
            if (r0 as i64) < 0 {
                // Bytes already written stay written; the caller is told how far it got by the
                // error rather than by a count, and a client retries from its own offset.
                return if done == 0 {
                    Err(fs_error(r0))
                } else {
                    Ok(done)
                };
            }
            let took = (r0 as usize).min(chunk);
            done += took;
            if took < chunk {
                break; // a short write is the filesystem's word for "no more room here"
            }
        }
        Ok(done)
    }

    fn truncate(&self, file: FileId, size: u64) -> Result<(), Error> {
        let (r0, _) = call(FS, fs::req(fs::TRUNCATE, file, 0), size);
        if (r0 as i64) < 0 {
            return Err(fs_error(r0));
        }
        Ok(())
    }

    /// **The only verb that names two directories**, so it is the only one that holds two descents
    /// at once and the only one whose second word is a packed pair rather than a scalar.
    ///
    /// `fs_proto::fs::RENAME` refuses moving a *directory* into another directory with `EINVAL`
    /// (its own doc argues the boundary: the cycle guard is an ancestry walk in a server whose
    /// stack is measured at three quarters used). That arrives here as [`Error::Io`] and reaches
    /// the client as an unexpected-IO status, which is honest and unhelpful; the case a client
    /// actually performs, renaming within one directory, works.
    fn rename(&self, from: Path<'_>, to: Path<'_>) -> Result<(), Error> {
        let (src_name, dst_name) = (from.name(), to.name());
        if src_name.len() + dst_name.len() > fs_proto::PAGE {
            return Err(Error::NameTooLong);
        }
        let src = self.descend(from.parent())?;
        let dst = match self.descend(to.parent()) {
            Ok(d) => d,
            Err(e) => {
                src.close();
                return Err(e);
            }
        };
        // Source first, destination back to back: fs_proto::fs::RENAME's page layout.
        fs_put(src_name);
        for (i, &b) in dst_name.iter().enumerate() {
            w8(FS_VA + (src_name.len() + i) as u64, b);
        }
        let (r0, _) = call(
            FS,
            fs::req(fs::RENAME, src.handle, src_name.len() as u64),
            fs::rename_dst(dst.handle, dst_name.len() as u64),
        );
        src.close();
        dst.close();
        if (r0 as i64) < 0 {
            return Err(fs_error(r0));
        }
        Ok(())
    }

    fn remove(&self, path: Path<'_>) -> Result<(), Error> {
        self.at_parent(path, fs::UNLINK, 0).map(|_| ())
    }

    fn rmdir(&self, path: Path<'_>) -> Result<(), Error> {
        self.at_parent(path, fs::RMDIR, 0).map(|_| ())
    }
}

// ============================================================================================
// Identity (milestone 54's last item). Everything below and nothing above touches the credential
// endpoint; the protocol machine sees only the `Authenticator` trait.
// ============================================================================================

/// **The share's authenticator: one endpoint, no key.**
///
/// The whole implementation is "put the public parts of the client's claim in a page, `CALL`, and
/// believe the answer". That is the shape milestone 65 built the credential service for, and the
/// kernel suite has asserted since then that a program in exactly this position authenticates a
/// session without ever holding the key
/// (`an_smb_server_authenticates_a_session_without_ever_holding_the_key`). This is that program,
/// arrived at from the other direction.
///
/// **What this process can do, exhaustively.** Ask whether a proof matches the key stored under one
/// resource. It cannot read that key (`cred_proto` has no message that returns one), cannot write it
/// (the provision endpoint was deleted at both ends before this process existed), and cannot ask
/// about any other resource (the name is [`Self::resource`], not a wire field). Compromising it
/// yields an oracle that answers questions its own clients were already asking, and revoking the
/// endpoint ends even that. Samba's `smbd` opens the password database instead, so compromising it
/// leaks every hash: crackable offline, reusable wherever the password was reused.
///
/// **The session key is not asked for, not read, and not left lying about.**
/// `cred_proto::verify::NTLM_PROOF` publishes [MS-NLMP] §4.2.4.1.2's `SessionBaseKey` in the shared
/// page on a match, because a server that *signs* needs it. This one does not sign, so it never calls
/// `cred_proto::session_key` and wipes the page itself the moment the reply lands (the service's own
/// wipe covers a *refusal*, not a match). That is asserted from outside by the kernel test that looks
/// at the frame afterwards, which is the check this process could not make about itself.
///
/// Name: provisional, minted by milestone 54's lane on 2026-08-17, following `FsShare`'s
/// `<what backs it><what it is>` shape.
struct CredentialAuthenticator;

impl CredentialAuthenticator {
    /// **The resource whose key this share's sessions are proved against.**
    ///
    /// # BUGS
    ///
    /// **This names a test fixture, and a deployment must not.** It is
    /// `cred_proto::fixture::SMB_RESOURCE`, which is [MS-NLMP] §4.2.1's published account, because
    /// the only boot that wires [`SHARE_FS_AUTHENTICATED`] today is the gate and the store it talks
    /// to is provisioned by a test program. A real share's resource is somebody's and arrives
    /// through a provisioning path that does not exist yet.
    ///
    /// **The right fix is not a configuration string, it is a narrower capability.** A request that
    /// names its resource is this program choosing which record to ask about, which is one authority
    /// more than it needs; the endpoint should *be* the credential for one resource, so the name is
    /// implied and unforgeable. That is DECISIONS §27's argument ("the endpoint IS the capability")
    /// applied to `cred_proto`, and it is a change to a contract two programs agree on, so it is
    /// calef's rather than this lane's. Until then this constant is the exception, and it says so.
    const fn resource() -> &'static [u8] {
        cred_proto::fixture::SMB_RESOURCE
    }
}

impl Authenticator for CredentialAuthenticator {
    fn authenticate(&self, a: &Attempt<'_>) -> Verdict {
        // The blob is the client's and is bounded by the contract, not by hope: a longer one cannot
        // be laid out, and `place_ntlm_proof` says so rather than truncating into a wrong answer.
        if a.blob.len() > cred_proto::MAX_BLOB {
            return Verdict::Refused;
        }
        // SAFETY: the wiring mapped one page read/write at CRED_VA before this program ran, shared
        // with the credential service and with nothing else. One thread per address space
        // (DECISIONS §33), so there is no second borrow.
        let page = unsafe { core::slice::from_raw_parts_mut(CRED_VA as *mut u8, cred_proto::PAGE) };
        let Some(w0) =
            cred_proto::place_ntlm_proof(page, Self::resource(), a.challenge, a.blob, a.proof)
        else {
            // A request the contract will not build is a refusal, not a guess. Nothing was sent.
            return Verdict::Refused;
        };
        let (r0, _) = call(CRED, w0, 0);
        // **Wipe the page before looking at the answer, and the reason is the whole point of this
        // type.** On a match the service *publishes* [MS-NLMP] §4.2.4.1.2's `SessionBaseKey` here,
        // because a server that signs needs it; this one does not sign, does not read it, and must
        // not let it sit in a frame two processes map for the rest of the connection. The verdict
        // rides in `r0`, a register, so nothing here needs the page at all.
        //
        // `cred_proto::wipe`'s own doc asks a client that keeps running to do this. The gate found
        // out the hard way: the first version of this function skipped it, and the kernel's look at
        // the frame afterwards found a live session key at `SESSION_KEY_OFF` (`0xbb`, not
        // §4.2.4's `0x8d..`, because the challenge is this connection's rather than the published
        // one). Left in the record because the failure was the assertion doing its job: a key that
        // outlives the exchange it belongs to is exactly what milestone 65's frame check is for.
        cred_proto::wipe(page);
        // `authenticated` collapses every failure mode to false, which is the safe direction and is
        // in the contract precisely so no caller has to remember which codes were the good ones.
        if cred_proto::authenticated(r0) {
            Verdict::Authenticated
        } else {
            Verdict::Refused
        }
    }

    // `anonymous` is not overridden: the trait's default refuses, which is the whole difference
    // between this share and a guest one. Spelled as a comment rather than as a method that returns
    // the default, because a method here would have to be read to find out it changed nothing.
}

/// Report `code` and stop. One-shot roles must exit, not spin (see `socket_test_client`).
fn done(code: u64) -> ! {
    send(REPORT, code, 0, 0);
    exit();
}

/// Mint a frame from our untyped, map it writable, and delegate it to socket `sid`.
fn attach_frame(sid: u64) {
    // SAFETY: `svc`. RETYPE returns the new frame capability's slot, or a negative error.
    let frame = unsafe { invoke(UNTYPED, ut::RETYPE, 0, 0, 0) };
    if frame < 0 {
        done(0xE101);
    }
    // SAFETY: `svc`. Map it writable; page tables come from our untyped.
    if unsafe { invoke(frame as u64, fr::MAP, FRAME_VA, 1, UNTYPED) } < 0 {
        done(0xE102);
    }
    // SAFETY: `svc`. Delegate it (narrowed to read/write) with the ATTACH request.
    if unsafe {
        invoke(
            STACK,
            endpoint::SEND_CAP,
            frame as u64,
            rights::READ | rights::WRITE,
            req(OP_ATTACH_FRAME, sid),
        )
    } < 0
    {
        done(0xE103);
    }
}

/// Send `buf` on the connection, chunked through the shared frame. False if the connection died.
fn send_all(buf: &[u8]) -> bool {
    let mut off = 0usize;
    let mut stalls = 0u32;
    while off < buf.len() {
        let chunk = (buf.len() - off).min(DATA_MAX);
        for (i, &b) in buf[off..off + chunk].iter().enumerate() {
            w8(FRAME_VA + OFF_PAYLOAD + i as u64, b);
        }
        let (sent, _) = call(STACK, req(OP_SEND, CONN_SID), chunk as u64);
        if sent == REP_ERR || sent as usize > chunk {
            return false;
        }
        if sent == 0 {
            // The peer's window or the socket buffer is full; `net_stack` polls the interface on
            // every call, so retrying drives progress. Bounded, so a dead peer cannot hold us.
            stalls += 1;
            if stalls > 10_000 {
                return false;
            }
        } else {
            stalls = 0;
        }
        off += sent as usize;
    }
    true
}

/// Receive once into `rx[at..]`, returning the new fill level, or `None` when the connection is
/// gone (closed or `net_stack`'s bounded wait expired).
fn recv_into(at: usize) -> Option<usize> {
    let (n, _) = call(STACK, req(OP_RECV, CONN_SID), 0);
    if n == REP_ERR || n == 0 {
        return None;
    }
    let n = (n as usize).min(DATA_MAX);
    let buf = rx();
    if at + n > buf.len() {
        return None; // a peer overrunning the reassembly buffer is a broken peer
    }
    for i in 0..n {
        buf[at + i] = r8(FRAME_VA + OFF_PAYLOAD + i as u64);
    }
    // The frame's own length field is net_stack's word for the same count; trust the reply word.
    let _ = OFF_LEN;
    Some(at + n)
}

/// Serve one accepted connection until the client logs off or the connection dies. Returns true
/// if at least one SMB message was answered (what "served" means for the test's rounds).
fn serve_connection(share: &impl Share, auth: &impl Authenticator) -> bool {
    // The NTLMSSP server challenge, and it must differ per connection or a captured proof replays.
    // `now()` is the clock the adapter holds; see the module BUGS on what that is worth.
    let mut conn = Connection::new(now().to_le_bytes());
    let mut fill = 0usize;
    let mut served = false;
    loop {
        // Assemble one transport frame: 4 bytes of length, then the message.
        while fill < XPORT_LEN {
            match recv_into(fill) {
                Some(f) => fill = f,
                None => return served,
            }
        }
        let hdr = [rx()[0], rx()[1], rx()[2], rx()[3]];
        let Some(mlen) = xport_parse(&hdr) else {
            // Not direct-TCP SMB2 framing: drop the connection rather than guess.
            return served;
        };
        let total = XPORT_LEN + mlen;
        while fill < total {
            match recv_into(fill) {
                Some(f) => fill = f,
                None => return served,
            }
        }

        let logoff = is_logoff(&rx()[XPORT_LEN..total]);
        let out = tx();
        let Some(n) = conn.handle(&rx()[XPORT_LEN..total], &mut out[XPORT_LEN..], share, auth)
        else {
            return served; // not SMB2: drop the connection
        };
        xport_write(out, n);
        if !send_all(&tx()[..XPORT_LEN + n]) {
            return served;
        }
        served = true;

        // A pipelined next request may already be buffered; keep it.
        rx().copy_within(total..fill, 0);
        fill -= total;

        if logoff {
            // A clean unmount. Ending the connection here (rather than waiting for the peer's
            // close) is what spares the 15 s RECV stall the module BUGS describe.
            return served;
        }
    }
}

/// Is this message (or the first element of a compound) a LOGOFF?
fn is_logoff(msg: &[u8]) -> bool {
    smb_proto::is_smb2(msg) && smb_proto::r16(msg, smb_proto::H_COMMAND) == smb_proto::CMD_LOGOFF
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(rounds: u64, port: u64, fs_backed: u64) -> ! {
    // Bind the granted port. The grant is whoever spawned us saying which port this service may
    // claim; a refusal here is a wiring bug, named per outcome.
    match call(STACK, req(OP_LISTEN, LISTEN_SID), port).0 {
        LISTEN_GRANTED => {}
        LISTEN_DENIED => done(0xE110),
        LISTEN_IN_USE => done(0xE111),
        _ => done(0xE112),
    }
    attach_frame(CONN_SID);

    // Which backing this boot wired, and in which direction (the module header's contract).
    // Dispatched once, here, so the serve loops stay monomorphic over the trait and no protocol
    // code asks again.
    match fs_backed {
        SHARE_FIXTURE => serve(rounds, &FIXTURE, &NoIdentity),
        SHARE_FS_READ_ONLY => serve(rounds, &FsShare { writable: false }, &NoIdentity),
        SHARE_FS_READ_WRITE => serve(rounds, &FsShare { writable: true }, &NoIdentity),
        // The only mode that admits nobody by default. It is a separate arm rather than a flag on
        // the one above so that a boot has to *say* it wants identity, and so that a reader of a
        // `Spawn` literal can see which it got.
        SHARE_FS_AUTHENTICATED => serve(
            rounds,
            &FsShare { writable: true },
            &CredentialAuthenticator,
        ),
        _ => done(0xE130), // an arg2 nobody defined: a wiring bug, named rather than guessed
    }
}

/// The accept loops, over whichever share and authenticator the boot wired.
fn serve(rounds: u64, share: &impl Share, auth: &impl Authenticator) -> ! {
    if rounds == 0 {
        // The serve-forever boot: say we are listening, then serve until the machine stops.
        send(REPORT, OK, 0, 0);
        loop {
            if call(STACK, req(OP_ACCEPT, LISTEN_SID), CONN_SID).0 == REP_OK {
                serve_connection(share, auth);
                let _ = call(STACK, req(OP_CLOSE, CONN_SID), 0);
            }
        }
    }

    let mut left = rounds;
    while left > 0 {
        let mut accepted = false;
        for _ in 0..ACCEPT_TRIES {
            if call(STACK, req(OP_ACCEPT, LISTEN_SID), CONN_SID).0 == REP_OK {
                accepted = true;
                break;
            }
        }
        if !accepted {
            done(0xE120); // nobody connected: is the runner's SMB hostfwd there, and the prober?
        }
        let served = serve_connection(share, auth);
        let _ = call(STACK, req(OP_CLOSE, CONN_SID), 0);
        if !served {
            done(0xE121); // a connection arrived but no SMB message was answered on it
        }
        left -= 1;
    }
    let _ = call(STACK, req(OP_CLOSE, LISTEN_SID), 0);
    done(OK);
}

user_rt::panic_handler!();
