#![cfg_attr(not(test), no_std)]
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
//! share is **read-only**. See the `BUGS` section at the bottom of this header.
//!
//! # BUGS
//!
//! - **Guest only, and guest means everyone.** Every AUTHENTICATE is accepted and flagged as a
//!   guest session. There is no user database, no proof check, and no signing. Do not put anything
//!   on a share this serves that the local network may not read.
//! - **Read-only.** `WRITE`, `SET_INFO`, and every disposition that would create or truncate are
//!   refused with `STATUS_ACCESS_DENIED`. Milestone 55 needs writes; this records where they go.
//! - **No credit or message-id accounting.** Credits granted are whatever was asked (at least 1),
//!   and message ids are echoed, never validated. A well-behaved client is fine; a hostile one can
//!   replay. The listener re-arms between connections, so one bad connection costs one connection.
//! - **ASCII names only.** SMB names are UTF-16LE; this crate matches names whose code units are
//!   all ASCII and treats anything else as not found. Same shape as `ntlm`'s uppercasing bug: a
//!   wrong answer rather than a crash, named here where the reader meets it.
//! - **One connection served at a time** (the `smb_server` role's shape, recorded here because the
//!   protocol allows more): the socket contract is synchronous and `MAX_SOCKETS` is 4, so this is
//!   a per-connection state machine, not a multiplexer. macOS opens one connection per mount.
//!
//! Name: unrecorded. Provisional, minted by milestone 54's lane on 2026-08-15, following the
//! `fs_proto`/`socket_proto`/`gfx_proto` pattern (a protocol contract crate takes the protocol's
//! standard name plus the `_proto` suffix). `smb` is a term of art in the family the naming tenet
//! says are already right (`elf`, `pci`, `ntlm`).

pub mod client;
pub mod ntlmssp;
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
