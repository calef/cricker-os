//! **The SMB adapter** (milestone 54): a network file service a Mac can mount.
//!
//! The roadmap block names the shape and this program is exactly it: an adapter holding **one
//! network endpoint and one share**, speaking SMB2 on the wire side and nothing but the `Share`
//! seam on the file side. All protocol logic lives in `smb_proto` (host-tested, rule 7); this
//! binary owns only the IO: listen and accept through the socket contract (milestone 107's
//! inbound half), reassemble the direct-TCP framing from `RECV`'s bounded chunks, hand each
//! message to `smb_proto::server::Connection`, and push the answer back out through `SEND`.
//!
//! The share has two backings behind the one `Share` seam, chosen by `arg2`:
//!
//! - **The fs_proto-backed share** ([`FsShare`]), the milestone's real one: this program holds a
//!   directory capability into the FS server (the endpoint IS the capability, DECISIONS §27) and
//!   answers every `Share` question with `fs_proto` verbs, so a client of the mount is reading
//!   RedoxFS bytes. This is what both the test boot and the serve boot wire when a RedoxFS disk
//!   is attached.
//! - **The fixture** (`smb_proto::share::FIXTURE`), files baked into this binary: the no-disk
//!   fallback, kept because it lets the whole protocol path run and gate with no FS service in
//!   the boot, and because a wire bug is easier to hunt against a share that cannot be wrong.
//!   Read-only, and it is `Share`'s worked example of a backing that says so.
//!
//! All protocol code is indifferent to which one it is serving; see notes/smb.md.
//!
//! # Capability contract
//! - slot 0: the report endpoint (WRITE)
//! - slot 1: the `Stack` endpoint (WRITE), shared with the stack's other client if any
//! - slot 2: an untyped budget (to mint and delegate the shared frame)
//! - slot 3: the file-service endpoint (WRITE), the directory capability the share serves.
//!   Present only when `arg2` says fs-backed, along with [`FS_VA`] mapped to the page shared
//!   with the FS server.
//! - arg0: connections to serve before reporting OK; 0 means serve forever (the demo boot)
//! - arg1: the TCP port to listen on (445 in the demo boot; the test grants a neighbour of the
//!   echo gate's port instead, see the kernel test)
//! - arg2: which share, and in which direction. 0 is the fixture, 1 the fs_proto-backed share
//!   read-only, 2 the fs_proto-backed share read-write ([`SHARE_FIXTURE`] and its two
//!   neighbours). The write path split this from a flag, because a **read-only view of the real
//!   filesystem** is a thing a boot should be able to wire and a boolean could not say.
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
//!   the `Share` id the FS server's own handle, so a 64 KiB transfer is sixteen page-sized
//!   `fs::READ`s and nothing else.
//! - **A handle is never reclaimed if a client vanishes mid-connection.** The FS server's handle
//!   table is per server, and this adapter closes what it opened only on CLOSE or when the
//!   `Connection` is dropped at end of connection; a connection torn down between a CREATE and
//!   its CLOSE leaks one FS handle for the life of the FS server. Bounded by `MAX_HANDLES` per
//!   connection, unbounded across connections, and the fix is the `Connection` telling the share
//!   what to release, which is a seam change rather than a line.
//! - **Free space is nominal**, and Time Machine will care: see `smb_proto`'s BUGS and
//!   `smb_proto::share::NOMINAL_VOLUME_BYTES`. `fs_proto` has no `statfs` verb.
//! - **Subdirectories are listed but not enterable**: the share model is flat (`share.rs`), so a
//!   directory shows in a listing with the directory attribute and answers `NOT_FOUND` when
//!   opened. Writing therefore cannot create one either: `MKDIR` is a verb this share does not
//!   offer at any disposition.
//! - **Upper-case FS names are unreachable.** The wire folds names to lower-case ASCII before
//!   lookup, and the FS is case-sensitive, so only lower-case names on disk can be opened.
//! - **One connection at a time.** A second client connecting while one is served is refused by
//!   TCP (the listener's backlog is one, `socket_proto`'s BUGS) or waits for the re-arm. macOS uses
//!   one connection per mount.
//! - **A dropped connection costs a 15 s stall**: `RECV` on a dead connection runs into
//!   `net_stack`'s bounded wait before it reports failure, and only then does this server re-arm.
//!   A clean unmount (LOGOFF) is detected and costs nothing.
//! - The protocol-level limitations (guest-only auth, read-only, ASCII names) are `smb_proto`'s
//!   and are listed in that crate's header.
//!
//! Name: unrecorded. Provisional, minted by milestone 54's lane on 2026-08-15: the program is the
//! server half of SMB, and `fs_server` set the `<protocol>_server` shape. Expect ratification to
//! reconsider it beside `net_stack` (which serves the socket contract but is not `socket_server`).

#![no_std]
#![no_main]

use abi::{endpoint, frame as fr, rights, untyped as ut};
use fs_proto::{dir, dirent, fs, xattr};
use smb_proto::server::Connection;
use smb_proto::share::{Entry, Error, FIXTURE, FileId, Share};
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

/// Where the page shared with the FS server is mapped (a name out, file bytes and directory
/// listings back). Must match the kernel-side wiring in `kernel/src/user/virtio_service.rs`.
const FS_VA: u64 = 0x0000_0000_00B0_0000;

/// **What `arg2` says the share is.** Three values rather than a flag, because the write path
/// made "which backing" and "which direction" two separate questions, and a boolean answering
/// both would have made a read-only real share unwireable. The `Share` seam is what enforces the
/// direction; this is only how the boot says which it wants.
const SHARE_FIXTURE: u64 = 0;
const SHARE_FS_READ_ONLY: u64 = 1;
const SHARE_FS_READ_WRITE: u64 = 2;

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
/// two-extra-page stack cannot hold a page-sized buffer), single-threaded like [`RX`].
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

/// One `READDIR` page of the share directory starting at entry `cursor`, copied into [`DIR`]
/// and returned as records ready for [`dirent::iter`]. Empty at the end of the listing, and on
/// an FS refusal (the module BUGS: errors degrade to absence).
fn fs_readdir(cursor: u64) -> &'static [u8] {
    let (r0, _) = call(FS, fs::req(fs::READDIR, fs::ROOT, 0), cursor);
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
        Some(2) => Error::NotFound,              // ENOENT
        Some(17) => Error::Exists,               // EEXIST
        Some(dir::EISDIR) => Error::IsDirectory, // 21
        Some(dir::EROFS) | Some(dir::EPERM) => Error::ReadOnly,
        Some(xattr::ENOSPC) => Error::NoSpace, // 28
        Some(36) => Error::NameTooLong,        // ENAMETOOLONG
        _ => Error::Io,
    }
}

/// Walk the listing to entry `index`, leaving its name in [`NAME`]. Returns the name's length
/// and whether the entry is a directory.
fn fs_nth(index: usize) -> Option<(usize, bool)> {
    let mut cursor = 0usize;
    loop {
        let page = fs_readdir(cursor as u64);
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

impl Share for FsShare {
    fn writable(&self) -> bool {
        self.writable
    }

    fn open(&self, name: &[u8]) -> Result<FileId, Error> {
        if name.len() > fs_proto::PAGE {
            return Err(Error::NameTooLong);
        }
        fs_put(name);
        let (r0, _) = call(FS, fs::req(fs::OPEN, fs::ROOT, name.len() as u64), 0);
        if (r0 as i64) < 0 {
            // A directory is `EISDIR`, which this flat share reports as absence: it has no
            // subdirectory node to hand back (module BUGS), so "there is nothing here you can
            // open" is the true sentence.
            return Err(match fs_error(r0) {
                Error::IsDirectory => Error::NotFound,
                e => e,
            });
        }
        Ok(r0)
    }

    fn create(&self, name: &[u8]) -> Result<FileId, Error> {
        if name.len() > fs_proto::PAGE {
            return Err(Error::NameTooLong);
        }
        fs_put(name);
        let (r0, _) = call(FS, fs::req(fs::CREATE, fs::ROOT, name.len() as u64), 0);
        if (r0 as i64) < 0 {
            return Err(fs_error(r0));
        }
        Ok(r0)
    }

    fn entry(&self, index: usize) -> Option<Entry<'_>> {
        let (len, is_dir) = fs_nth(index)?;
        let size = if is_dir {
            0
        } else {
            // OPEN + FSTAT + CLOSE per entry: the listing records carry no size (fs_proto's
            // dirent is name and kind only). This is the one path that still pays per entry,
            // because a listing is a walk by nature.
            fs_put(fs_name(len));
            let (h, _) = call(FS, fs::req(fs::OPEN, fs::ROOT, len as u64), 0);
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

    fn size(&self, file: FileId) -> u64 {
        let (s, _) = call(FS, fs::req(fs::FSTAT, file, 0), 0);
        if (s as i64) < 0 { 0 } else { s }
    }

    fn close(&self, file: FileId) {
        fs_close(file);
    }

    fn read(&self, file: FileId, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        let mut done = 0usize;
        while done < out.len() {
            let want = (out.len() - done).min(fs_proto::PAGE);
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

    fn write(&self, file: FileId, offset: u64, data: &[u8]) -> Result<usize, Error> {
        let mut done = 0usize;
        while done < data.len() {
            let chunk = (data.len() - done).min(fs_proto::PAGE);
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

    fn rename(&self, from: &[u8], to: &[u8]) -> Result<(), Error> {
        if from.len() + to.len() > fs_proto::PAGE {
            return Err(Error::NameTooLong);
        }
        // Source first, destination back to back: fs_proto::fs::RENAME's page layout.
        fs_put(from);
        for (i, &b) in to.iter().enumerate() {
            w8(FS_VA + (from.len() + i) as u64, b);
        }
        let (r0, _) = call(
            FS,
            fs::req(fs::RENAME, fs::ROOT, from.len() as u64),
            fs::rename_dst(fs::ROOT, to.len() as u64),
        );
        if (r0 as i64) < 0 {
            return Err(fs_error(r0));
        }
        Ok(())
    }

    fn remove(&self, name: &[u8]) -> Result<(), Error> {
        if name.len() > fs_proto::PAGE {
            return Err(Error::NameTooLong);
        }
        fs_put(name);
        let (r0, _) = call(FS, fs::req(fs::UNLINK, fs::ROOT, name.len() as u64), 0);
        if (r0 as i64) < 0 {
            return Err(fs_error(r0));
        }
        Ok(())
    }
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
fn serve_connection(share: &impl Share) -> bool {
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
        let Some(n) = conn.handle(&rx()[XPORT_LEN..total], &mut out[XPORT_LEN..], share) else {
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
        SHARE_FIXTURE => serve(rounds, &FIXTURE),
        SHARE_FS_READ_ONLY => serve(rounds, &FsShare { writable: false }),
        SHARE_FS_READ_WRITE => serve(rounds, &FsShare { writable: true }),
        _ => done(0xE130), // an arg2 nobody defined: a wiring bug, named rather than guessed
    }
}

/// The accept loops, over whichever share the boot wired.
fn serve(rounds: u64, share: &impl Share) -> ! {
    if rounds == 0 {
        // The serve-forever boot: say we are listening, then serve until the machine stops.
        send(REPORT, OK, 0, 0);
        loop {
            if call(STACK, req(OP_ACCEPT, LISTEN_SID), CONN_SID).0 == REP_OK {
                serve_connection(share);
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
        let served = serve_connection(share);
        let _ = call(STACK, req(OP_CLOSE, CONN_SID), 0);
        if !served {
            done(0xE121); // a connection arrived but no SMB message was answered on it
        }
        left -= 1;
    }
    let _ = call(STACK, req(OP_CLOSE, LISTEN_SID), 0);
    done(OK);
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `brk` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem));
    };
    #[cfg(target_arch = "riscv64")]
    // SAFETY: `ebreak` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
