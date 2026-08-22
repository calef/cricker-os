#![cfg_attr(not(test), no_std)]
// milestone 68's ratchet is workspace-wide (§107); this crate opts out until its 52-item
// worklist (notes/doc-coverage.md) is burned down.
#![allow(missing_docs)]
//! **The SMB2 wire format, and the server logic that speaks it** (milestone 54).
//!
//! macOS mounts SMB natively, and milestone 55 (a Time Machine target) requires SMB regardless, so
//! SMB is the one network file protocol this tree carries (design/roadmap/54-network-file-service.md
//! settles it). This crate is the whole of the wire: framing, header, every request parse and
//! response build, NTLMSSP and the minimal SPNEGO wrapping macOS puts around it, and the
//! per-connection state machine ([`server::Connection`]) that turns one inbound SMB2 message into
//! one response. The `smb_server` role in `user/src/smb_server.rs` owns only the IO: sockets in,
//! bytes to this crate, bytes out.
//!
//! That split is rule 7 and the tree's whole method: every byte offset in this protocol is a thing
//! two programs must agree on (the expensive-to-reverse category), so it lives here, host-tested,
//! with the client-side builders ([`client`]) in the same crate so the tests and xtask's host-side
//! prober exercise the same constants the server answers with. No party keeps its own copy.
//!
//! # Scope, honestly
//!
//! The dialect is **SMB 2.1 only** (`DIALECT_0210`), chosen deliberately: 2.0.2 predates features
//! macOS wants, and the 3.x family drags in signing enforcement, encryption, and
//! `VALIDATE_NEGOTIATE_INFO`, none of which a first mount needs. Sessions are **guest only**: the
//! server answers the NTLMSSP exchange so a client can complete it, and then admits everyone as
//! guest without checking anything, which is what "anonymous first, identity later" means
//! concretely (the `cred`/`ntlm` machinery from milestone 65 is the recorded next step; nothing
//! here stores or checks a secret, per DECISIONS §79's constraints on credential material). The
//! share is a **tree** of directories and files, readable always and writable when its backing says
//! so ([`share::Share::writable`]); a read-only share refuses every mutating command **at this
//! layer**, before the backing is asked. See the `BUGS` section at the bottom of this header.
//!
//! **Paths are parsed here, once, at the edge** ([`path`]). A traversal is a wire attack, so `..`
//! is refused where the bytes arrive rather than wherever a backing happens to look at it, and the
//! [`share::Share`] seam takes a [`path::Path`] that cannot be constructed without that check.
//!
//! **Apple's extensions arrive through the one door SMB2 has for them** ([`create_context`], the
//! chain a CREATE hangs its extensions off, and [`apple`], which is what the `AAPL` tag in that
//! chain means). That is milestone 55's first piece: macOS mounts a plain share happily and will
//! not offer one as a Time Machine destination without this exchange. Every claim it makes is a
//! claim a client acts on, so [`apple`]'s header lists each bit and why it is set or left clear,
//! and its `BUGS` is honest about the one that reaches past what the stack backs.
//!
//! # Examples
//!
//! A whole mount, on the host, in microseconds. This is what the `smb_server` role does with a
//! socket underneath and nothing else: bytes in, [`server::Connection::handle`], bytes out.
//!
//! ```
//! use smb_proto::{
//!     H_SESSION_ID, H_STATUS, H_TREE_ID, MAX_MESSAGE, STATUS_SUCCESS, authenticator::NoIdentity,
//!     client, r32, r64, server::Connection, share::FIXTURE,
//! };
//!
//! // One request through the state machine, the way a socket loop would drive it.
//! fn exchange(c: &mut Connection, request: &[u8]) -> Vec<u8> {
//!     let mut out = vec![0u8; MAX_MESSAGE];
//!     let n = c
//!         .handle(request, &mut out, &FIXTURE, &NoIdentity)
//!         .expect("SMB2 in, SMB2 out");
//!     out.truncate(n);
//!     out
//! }
//!
//! // The challenge is the connection's, and a real server takes it from the entropy service.
//! let mut c = Connection::new([0xA5; 8]);
//!
//! let resp = exchange(&mut c, &client::negotiate(1));
//! assert_eq!(client::negotiate_dialect(&resp), smb_proto::DIALECT_0210);
//!
//! // Two round trips: the client asks, the server challenges, the client answers. Nothing checks
//! // the answer, which is what "guest only" means and why it is the first entry under BUGS.
//! let resp = exchange(&mut c, &client::session_setup_negotiate(2));
//! let sid = r64(&resp, H_SESSION_ID);
//! let resp = exchange(&mut c, &client::session_setup_authenticate(3, sid));
//! assert_eq!(r32(&resp, H_STATUS), STATUS_SUCCESS);
//!
//! let resp = exchange(&mut c, &client::tree_connect(4, sid, b"\\\\10.0.2.15\\share"));
//! let tid = r32(&resp, H_TREE_ID);
//!
//! // Open a file and read it.
//! let resp = exchange(&mut c, &client::create(5, sid, tid, b"hello.txt"));
//! let fid = client::create_file_id(&resp);
//! let resp = exchange(&mut c, &client::read(6, sid, tid, &fid, 0, 64));
//! assert_eq!(client::read_data(&resp), b"nife serves SMB\n");
//!
//! // A read at end of file is END_OF_FILE, not an empty success. That distinction is what ends a
//! // client's read loop, so getting it wrong is a hang rather than a wrong answer.
//! let resp = exchange(&mut c, &client::read(7, sid, tid, &fid, 16, 64));
//! assert_eq!(r32(&resp, H_STATUS), smb_proto::STATUS_END_OF_FILE);
//! ```
//!
//! The other thing worth showing is [`path`], because it is where a wire attack dies. A Time Machine
//! sparsebundle is a directory of band files, so the share has to be a tree, and the moment it is a
//! tree a client can try to walk out of it:
//!
//! ```
//! use smb_proto::path::{Path, PathError};
//!
//! // What a Mac writing a backup actually sends.
//! let p = Path::parse(b"corinne.sparsebundle\\bands\\1a2b").unwrap();
//! assert_eq!(p.depth(), 3);
//! assert_eq!(p.name(), b"1a2b");
//! assert_eq!(p.parent().as_bytes(), b"corinne.sparsebundle\\bands");
//!
//! // `..` is refused, not resolved: the capability argument is that there is no "above" to
//! // resolve to, and refusing stays correct if a component is a symlink one day.
//! assert_eq!(Path::parse(b"bands\\..\\..\\etc\\passwd"), Err(PathError::Traversal));
//!
//! // `.` is refused too, for a different reason: accepting it would give one file two names, and
//! // this share's handles carry their path as their identity.
//! assert_eq!(Path::parse(b"bands\\.\\0"), Err(PathError::Traversal));
//!
//! // A forward slash is not this protocol's separator, so accepting it would let two clients
//! // spell one file two ways.
//! assert_eq!(Path::parse(b"bands/0"), Err(PathError::Separator));
//!
//! // But a trailing separator is stripped, because clients really do send `dir\`.
//! assert_eq!(Path::parse(b"bands\\").unwrap().as_bytes(), b"bands");
//!
//! // The root is the one path with no components, and its parent is itself.
//! assert!(Path::ROOT.is_root());
//! assert_eq!(Path::ROOT.parent(), Path::ROOT);
//! ```
//!
//! # BUGS
//!
//! - **Guest only, and guest means everyone.** Every AUTHENTICATE is accepted and flagged as a
//!   guest session. There is no user database, no proof check, and no signing. Do not put anything
//!   on a share this serves that the local network may not read, **and now that writes exist,
//!   nothing you would mind the local network changing.**
//! - **Free space is nominal only when the backing has no volume to ask.** A backing that can
//!   answer [`share::Share::statfs`] is reported verbatim (the fs-backed share does, through
//!   `fs_proto::fs::STATFS`); [`share::FixtureShare`] is files baked into a binary and has no
//!   volume, so it falls back to [`share::NOMINAL_VOLUME_BYTES`]. The number is a **forecast**
//!   either way: a write past the real end of the image still fails with [`STATUS_DISK_FULL`] at
//!   the write, because free space is not a reservation.
//! - **A read-only share reports zero free space**, whatever its backing says. That matches
//!   `READ_ONLY_VOLUME` one field over, and a client that believed the real count would try a write
//!   this server refuses anyway.
//! - **Timestamps on a directory are the epoch too**, for the file case's reason below, so a
//!   listing sorted by date is sorted by nothing.
//! - **Nothing enforces a depth limit**, only the total path length ([`path::MAX_PATH`]). A deep
//!   path costs the fs-backed share one descent per component, so it is slow rather than refused.
//! - **`SET_INFO`'s `FileRenameInformation` ignores a nonzero `RootDirectory`** and answers
//!   [`STATUS_NOT_SUPPORTED`]. The destination is a share-relative path, which is what every client
//!   sends.
//! - **Timestamps are accepted and discarded.** `SET_INFO`'s `FileBasicInformation` succeeds and
//!   changes nothing, because this server holds no clock capability and `fs_proto`'s `FSTAT`
//!   carries no times. A client that sets a modification time and reads it back gets the epoch.
//! - **`FileAllocationInformation` is a no-op**, deliberately: preallocation is a hint, and
//!   turning it into a truncate would zero-extend a file a client was about to write into.
//! - **No credit or message-id accounting.** Credits granted are whatever was asked (at least 1),
//!   and message ids are echoed, never validated. A well-behaved client is fine; a hostile one can
//!   replay. The listener re-arms between connections, so one bad connection costs one connection.
//! - **ASCII names only.** SMB names are UTF-16LE; this crate matches names whose code units are
//!   all ASCII and treats anything else as not found. Same shape as `ntlm`'s uppercasing bug: a
//!   wrong answer rather than a crash, named here where the reader meets it.
//! - **`ReplaceIfExists = 0` is ignored on a rename**, so a rename always replaces a destination of
//!   the same kind. `fs_proto::fs::RENAME` offers no way to refuse a collision (its own doc gives
//!   §42's reason for declining `renameat2`'s `NOREPLACE`), so a client that asked for the rename
//!   to fail gets a silent overwrite. The wrong direction to fail in, and the fix is in the
//!   filesystem contract rather than here.
//! - **No share modes, no oplocks, no leases.** `ShareAccess` is parsed off the wire and never
//!   consulted, so nothing is ever a sharing violation. That is why `fruit:posix_rename` needed no
//!   work; it is also why two clients writing one file get whatever the filesystem gives them.
//! - **One connection served at a time** (the `smb_server` role's shape, recorded here because the
//!   protocol allows more): the socket contract is synchronous and `MAX_SOCKETS` is 4, so this is
//!   a per-connection state machine, not a multiplexer. macOS opens one connection per mount.
//!
//! Name: unrecorded. Provisional, minted by milestone 54's lane on 2026-08-15, following the
//! `fs_proto`/`socket_proto`/`gfx_proto` pattern (a protocol contract crate takes the protocol's
//! standard name plus the `_proto` suffix). `smb` is a term of art in the family the naming tenet
//! says are already right (`elf`, `pci`, `ntlm`).

pub mod apple;
pub mod authenticator;
pub mod client;
pub mod create_context;
pub mod ntlmssp;
pub mod path;
pub mod server;
pub mod share;
pub mod spnego;

// ------------------------------------------------------------------------------------------------
// Little-endian slice helpers. Every multi-byte SMB2 field is little-endian; the one exception in
// this whole crate is the transport length below, which is big-endian because it is NetBIOS's.
// ------------------------------------------------------------------------------------------------

/// Read a little-endian u16 at `off`, or 0 if out of range. The "or 0" is deliberate: every caller
/// bounds-checks the enclosing structure first, and a structurally short message already failed
/// there, so these helpers never need to carry a second error path.
pub fn r16(b: &[u8], off: usize) -> u16 {
    match b.get(off..off + 2) {
        Some(s) => u16::from_le_bytes([s[0], s[1]]),
        None => 0,
    }
}
pub fn r32(b: &[u8], off: usize) -> u32 {
    match b.get(off..off + 4) {
        Some(s) => u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        None => 0,
    }
}
pub fn r64(b: &[u8], off: usize) -> u64 {
    match b.get(off..off + 8) {
        Some(s) => u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]),
        None => 0,
    }
}
pub fn w16(b: &mut [u8], off: usize, v: u16) {
    b[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
pub fn w32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
pub fn w64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

// ------------------------------------------------------------------------------------------------
// Transport: SMB2 over direct TCP ([MS-SMB2] §2.1), port 445. Each message is preceded by a
// 4-byte header: one zero byte, then a 24-bit BIG-endian length. (This is the NetBIOS session
// header with the session-service types collapsed to zero; direct TCP kept the shape.)
// ------------------------------------------------------------------------------------------------

/// The TCP port SMB2 direct transport uses, and the port the share's listen grant must cover.
pub const DIRECT_TCP_PORT: u16 = 445;

/// The transport prefix's size in bytes.
pub const XPORT_LEN: usize = 4;

/// The largest SMB2 message this implementation frames: the negotiated max transaction size plus
/// slack for the header and a response structure. One constant sizes the server's reassembly and
/// build buffers and the transport check, so they cannot disagree.
pub const MAX_MESSAGE: usize = MAX_TRANSACT as usize + 512;

/// Write the direct-TCP transport prefix for a `len`-byte message.
pub fn xport_write(out: &mut [u8], len: usize) {
    out[0] = 0;
    out[1] = (len >> 16) as u8;
    out[2] = (len >> 8) as u8;
    out[3] = len as u8;
}

/// The message length a transport prefix announces, if the prefix is well-formed and the length is
/// one this implementation is willing to buffer. `None` covers both a nonzero type byte (a real
/// `NetBIOS` session service, which direct TCP does not speak) and an oversized announcement.
pub fn xport_parse(hdr: &[u8; XPORT_LEN]) -> Option<usize> {
    if hdr[0] != 0 {
        return None;
    }
    let len = ((hdr[1] as usize) << 16) | ((hdr[2] as usize) << 8) | hdr[3] as usize;
    if len == 0 || len > MAX_MESSAGE {
        return None;
    }
    Some(len)
}

// ------------------------------------------------------------------------------------------------
// The SMB2 header ([MS-SMB2] §2.2.1): 64 bytes at the front of every message, request and
// response alike.
// ------------------------------------------------------------------------------------------------

/// `0xFE 'S' 'M' 'B'`, the SMB2 protocol id. (SMB1 is `0xFF`; a `0xFF` here is a client this
/// server does not speak to, answered by dropping the connection.)
pub const PROTOCOL_ID: [u8; 4] = [0xFE, b'S', b'M', b'B'];
/// The header's fixed size, which is also the `StructureSize` field's required value.
pub const HDR_LEN: usize = 64;

// Header field offsets.
pub const H_STRUCT: usize = 4; // u16, always 64
pub const H_CREDIT_CHARGE: usize = 6; // u16
pub const H_STATUS: usize = 8; // u32 (responses; requests carry channel sequence here)
pub const H_COMMAND: usize = 12; // u16
pub const H_CREDIT: usize = 14; // u16, request: credits asked; response: credits granted
pub const H_FLAGS: usize = 16; // u32
pub const H_NEXT_COMMAND: usize = 20; // u32, offset to the next request in a compound
pub const H_MESSAGE_ID: usize = 24; // u64
pub const H_TREE_ID: usize = 36; // u32
pub const H_SESSION_ID: usize = 40; // u64
pub const H_SIGNATURE: usize = 48; // 16 bytes

/// Header flags this crate reads or sets.
pub const FLAG_SERVER_TO_REDIR: u32 = 0x0000_0001; // set on every response
pub const FLAG_RELATED_OPERATIONS: u32 = 0x0000_0004; // compound request: inherit the file id

/// Commands ([MS-SMB2] §2.2.1.1).
pub const CMD_NEGOTIATE: u16 = 0;
pub const CMD_SESSION_SETUP: u16 = 1;
pub const CMD_LOGOFF: u16 = 2;
pub const CMD_TREE_CONNECT: u16 = 3;
pub const CMD_TREE_DISCONNECT: u16 = 4;
pub const CMD_CREATE: u16 = 5;
pub const CMD_CLOSE: u16 = 6;
pub const CMD_FLUSH: u16 = 7;
pub const CMD_READ: u16 = 8;
pub const CMD_WRITE: u16 = 9;
pub const CMD_LOCK: u16 = 10;
pub const CMD_IOCTL: u16 = 11;
pub const CMD_CANCEL: u16 = 12;
pub const CMD_ECHO: u16 = 13;
pub const CMD_QUERY_DIRECTORY: u16 = 14;
pub const CMD_CHANGE_NOTIFY: u16 = 15;
pub const CMD_QUERY_INFO: u16 = 16;
pub const CMD_SET_INFO: u16 = 17;

/// NT status codes ([MS-ERREF]), the subset this server speaks.
pub const STATUS_SUCCESS: u32 = 0x0000_0000;
pub const STATUS_NO_MORE_FILES: u32 = 0x8000_0006;
pub const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;
pub const STATUS_END_OF_FILE: u32 = 0xC000_0011;
pub const STATUS_MORE_PROCESSING_REQUIRED: u32 = 0xC000_0016;
pub const STATUS_ACCESS_DENIED: u32 = 0xC000_0022;
pub const STATUS_OBJECT_NAME_NOT_FOUND: u32 = 0xC000_0034;
pub const STATUS_OBJECT_PATH_NOT_FOUND: u32 = 0xC000_003A;
pub const STATUS_FILE_IS_A_DIRECTORY: u32 = 0xC000_00BA;
pub const STATUS_NOT_SUPPORTED: u32 = 0xC000_00BB;
pub const STATUS_BAD_NETWORK_NAME: u32 = 0xC000_00CC;
pub const STATUS_USER_SESSION_DELETED: u32 = 0xC000_0203;
pub const STATUS_FS_DRIVER_REQUIRED: u32 = 0xC000_019C;
pub const STATUS_FILE_CLOSED: u32 = 0xC000_0128;
pub const STATUS_INFO_LENGTH_MISMATCH: u32 = 0xC000_0004;
/// A create that said "create" met a name that already exists. The write path's status, and the
/// one a client's create-or-open fallback keys on.
pub const STATUS_OBJECT_NAME_COLLISION: u32 = 0xC000_0035;
/// A name this share cannot hold (longer than [`server::MAX_NAME`]).
pub const STATUS_OBJECT_NAME_INVALID: u32 = 0xC000_0033;
/// The filesystem is full. Reported at the write that hits the end rather than predicted, because
/// nothing here can ask how much room is left; see [`share::NOMINAL_VOLUME_BYTES`].
pub const STATUS_DISK_FULL: u32 = 0xC000_007F;
/// The backing failed in a way the share model has no word for. Better than a short read a client
/// would take for end of file.
pub const STATUS_UNEXPECTED_IO_ERROR: u32 = 0xC000_00E9;
/// A path component before the last is a file, or a directory verb met a file. The mirror of
/// [`STATUS_FILE_IS_A_DIRECTORY`], and the pair a tree needs that a flat share did not.
pub const STATUS_NOT_A_DIRECTORY: u32 = 0xC000_0103;
/// `RMDIR` met a directory with something in it. Refused rather than emptied; the recursion belongs
/// in the client, one refusable step at a time.
pub const STATUS_DIRECTORY_NOT_EMPTY: u32 = 0xC000_0101;
/// **The proof did not check out, or nobody offered one to a share that requires one**
/// (milestone 54's identity item). One status for both, deliberately: see
/// [`authenticator::Verdict::Refused`] on why distinguishing them would make session setup an
/// oracle for which accounts exist.
///
/// It is what Windows and Samba answer, so a real client's retry logic already knows it: macOS
/// prompts for a password on this and retries on the same connection.
pub const STATUS_LOGON_FAILURE: u32 = 0xC000_006D;

/// The one dialect this server offers and accepts: SMB 2.1. See the crate header for why.
pub const DIALECT_0210: u16 = 0x0210;
/// The wildcard revision ([MS-SMB2] §3.3.5.3.1): a server's answer to an **SMB1** multi-protocol
/// negotiate whose dialect strings claim SMB2, telling the client to come back with a real SMB2
/// NEGOTIATE. How every real client's first exchange with this server ends, macOS included.
pub const DIALECT_WILDCARD: u16 = 0x02FF;

/// The negotiated maxima, one value for all three of `MaxTransactSize`, `MaxReadSize`,
/// `MaxWriteSize`. 64 KiB is the floor mainstream clients are written against ([MS-SMB2]
/// §3.3.5.4's SHOULD); going lower risks a client that never asks smaller, and going higher costs
/// exactly that many bytes of static buffer in a server with no allocator.
pub const MAX_TRANSACT: u32 = 65536;

/// Is this a well-formed SMB2 header? The gate every inbound message passes before any field of it
/// is believed.
pub fn is_smb2(msg: &[u8]) -> bool {
    msg.len() >= HDR_LEN && msg[..4] == PROTOCOL_ID && r16(msg, H_STRUCT) as usize == HDR_LEN
}

/// Write a response header for command `cmd`, echoing `msg_id`, `session_id` and `tree_id`,
/// granting `credits`, carrying `status`. Flags get `SERVER_TO_REDIR`; the signature stays zero
/// (nothing this server serves is signed; see the crate BUGS).
#[allow(clippy::too_many_arguments)] // a header is eight fields; a struct here would be ceremony
pub fn write_response_header(
    out: &mut [u8],
    cmd: u16,
    status: u32,
    msg_id: u64,
    session_id: u64,
    tree_id: u32,
    credits: u16,
    flags_extra: u32,
) {
    out[..HDR_LEN].fill(0);
    out[..4].copy_from_slice(&PROTOCOL_ID);
    w16(out, H_STRUCT, HDR_LEN as u16);
    w16(out, H_CREDIT_CHARGE, 1);
    w32(out, H_STATUS, status);
    w16(out, H_COMMAND, cmd);
    w16(out, H_CREDIT, credits);
    w32(out, H_FLAGS, FLAG_SERVER_TO_REDIR | flags_extra);
    w64(out, H_MESSAGE_ID, msg_id);
    w32(out, H_TREE_ID, tree_id);
    w64(out, H_SESSION_ID, session_id);
}

/// Decode UTF-16LE `name` bytes into ASCII in `out`, lower-cased for matching. `None` if any code
/// unit is not printable ASCII or `out` is too small. SMB names compare case-insensitively, and
/// this server's names are ASCII (crate BUGS), so lower-casing at the edge lets everything inside
/// compare bytes.
pub fn utf16le_to_ascii_lower<'a>(name: &[u8], out: &'a mut [u8]) -> Option<&'a [u8]> {
    if !name.len().is_multiple_of(2) || name.len() / 2 > out.len() {
        return None;
    }
    let n = name.len() / 2;
    for i in 0..n {
        let u = u16::from_le_bytes([name[2 * i], name[2 * i + 1]]);
        if !(0x20..0x7f).contains(&u) {
            return None;
        }
        out[i] = (u as u8).to_ascii_lowercase();
    }
    Some(&out[..n])
}

/// Encode ASCII `name` as UTF-16LE into `out`, returning the byte length. The inverse edge, used
/// for directory listings and the negotiate target name.
pub fn ascii_to_utf16le(name: &[u8], out: &mut [u8]) -> usize {
    for (i, &c) in name.iter().enumerate() {
        out[2 * i] = c;
        out[2 * i + 1] = 0;
    }
    name.len() * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transport_prefix_round_trips() {
        let mut h = [0u8; XPORT_LEN];
        for len in [1usize, 68, 0x1234, MAX_MESSAGE] {
            xport_write(&mut h, len);
            assert_eq!(xport_parse(&h), Some(len), "len {len}");
        }
    }

    #[test]
    fn the_transport_refuses_what_it_could_not_buffer() {
        let mut h = [0u8; XPORT_LEN];
        xport_write(&mut h, MAX_MESSAGE + 1);
        assert_eq!(xport_parse(&h), None);
        xport_write(&mut h, 10);
        h[0] = 0x81; // a real NetBIOS SESSION REQUEST type byte
        assert_eq!(xport_parse(&h), None);
        xport_write(&mut h, 0);
        assert_eq!(xport_parse(&h), None);
    }

    #[test]
    fn a_response_header_is_a_valid_smb2_header_and_echoes_what_it_must() {
        let mut out = [0u8; HDR_LEN];
        write_response_header(&mut out, CMD_READ, STATUS_SUCCESS, 7, 0x11, 0x22, 33, 0);
        assert!(is_smb2(&out));
        assert_eq!(r16(&out, H_COMMAND), CMD_READ);
        assert_eq!(r64(&out, H_MESSAGE_ID), 7);
        assert_eq!(r64(&out, H_SESSION_ID), 0x11);
        assert_eq!(r32(&out, H_TREE_ID), 0x22);
        assert_eq!(r16(&out, H_CREDIT), 33);
        assert_eq!(
            r32(&out, H_FLAGS) & FLAG_SERVER_TO_REDIR,
            FLAG_SERVER_TO_REDIR
        );
        assert_eq!(&out[H_SIGNATURE..H_SIGNATURE + 16], &[0u8; 16]);
    }

    #[test]
    fn is_smb2_refuses_smb1_and_short_buffers() {
        let mut msg = [0u8; HDR_LEN];
        msg[..4].copy_from_slice(&[0xFF, b'S', b'M', b'B']); // SMB1
        w16(&mut msg, H_STRUCT, 64);
        assert!(!is_smb2(&msg));
        assert!(!is_smb2(&[0xFE, b'S', b'M', b'B'])); // too short to hold a header
    }

    #[test]
    fn name_decoding_is_case_insensitive_and_refuses_non_ascii() {
        let mut buf = [0u8; 64];
        let mut wide = [0u8; 64];
        let n = ascii_to_utf16le(b"Hello.TXT", &mut wide);
        assert_eq!(
            utf16le_to_ascii_lower(&wide[..n], &mut buf).unwrap(),
            b"hello.txt"
        );
        // One non-ASCII code unit poisons the whole name: not found, never mangled.
        let n = ascii_to_utf16le(b"abc", &mut wide);
        wide[0] = 0xE9; // U+00E9, e-acute: printable, but not ASCII
        wide[1] = 0x00;
        assert!(utf16le_to_ascii_lower(&wide[..n], &mut buf).is_none());
        // An odd byte count is not UTF-16 at all.
        assert!(utf16le_to_ascii_lower(&wide[..3], &mut buf).is_none());
    }
}
