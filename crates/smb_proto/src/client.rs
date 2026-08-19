//! **The client half of the wire**: request builders and response readers.
//!
//! Two consumers, both on the host: this crate's own tests (which drive [`crate::server`] as a
//! client would) and xtask's SMB prober, which connects to the QEMU guest through `hostfwd` and
//! performs a real mount-shaped exchange. Keeping the client in the same crate as the server is
//! the point, not a convenience: every offset exists exactly once, so a test cannot pass because
//! both sides share the same misreading of the spec in two files. (They can still share one
//! misreading of the spec in one file, which is what the real-macOS mount in notes/smb.md is
//! for.)
//!
//! The builders speak **raw NTLMSSP** (no SPNEGO), which the server accepts alongside the wrapped
//! form; the SPNEGO path is proven by `spnego`'s own tests plus a wrapped-token session test.
//! Everything returns a fixed-capacity [`Msg`] so this module stays `no_std` like the rest of the
//! crate.

use crate::{
    CMD_CLOSE, CMD_CREATE, CMD_ECHO, CMD_FLUSH, CMD_NEGOTIATE, CMD_QUERY_DIRECTORY, CMD_QUERY_INFO,
    CMD_READ, CMD_SESSION_SETUP, CMD_SET_INFO, CMD_TREE_CONNECT, CMD_WRITE, DIALECT_0210,
    FLAG_RELATED_OPERATIONS, H_COMMAND, H_CREDIT, H_FLAGS, H_MESSAGE_ID, H_NEXT_COMMAND,
    H_SESSION_ID, H_STRUCT, H_TREE_ID, HDR_LEN, PROTOCOL_ID, ascii_to_utf16le, ntlmssp, r16, r32,
    r64, utf16le_to_ascii_lower, w16, w32, w64,
};

/// The largest request these builders emit. Requests are small (the biggest is a compound of
/// three); responses are the big direction and are the caller's buffer.
pub const MSG_MAX: usize = 768;

/// An owned request message, fixed capacity, so builders work without an allocator.
pub struct Msg {
    buf: [u8; MSG_MAX],
    len: usize,
}

impl core::ops::Deref for Msg {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}
impl core::ops::DerefMut for Msg {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.buf[..self.len]
    }
}

/// A request header at `buf[at..]`: like the response one, minus the server flag.
fn request_header(buf: &mut [u8], at: usize, cmd: u16, msg_id: u64, sid: u64, tid: u32) {
    buf[at..at + HDR_LEN].fill(0);
    buf[at..at + 4].copy_from_slice(&PROTOCOL_ID);
    w16(buf, at + H_STRUCT, HDR_LEN as u16);
    w16(buf, at + 6, 1); // CreditCharge
    w16(buf, at + H_COMMAND, cmd);
    w16(buf, at + H_CREDIT, 31); // ask for credits the way real clients do
    w64(buf, at + H_MESSAGE_ID, msg_id);
    w32(buf, at + H_TREE_ID, tid);
    w64(buf, at + H_SESSION_ID, sid);
}

fn msg(len: usize, buf: [u8; MSG_MAX]) -> Msg {
    Msg { buf, len }
}

/// A NEGOTIATE offering the dialects a macOS-era client offers, ours among them.
pub fn negotiate(msg_id: u64) -> Msg {
    let mut b = [0u8; MSG_MAX];
    request_header(&mut b, 0, CMD_NEGOTIATE, msg_id, 0, 0);
    let h = HDR_LEN;
    w16(&mut b, h, 36); // StructureSize
    let dialects = [0x0202u16, DIALECT_0210, 0x0300, 0x0302];
    w16(&mut b, h + 2, dialects.len() as u16);
    w16(&mut b, h + 4, 1); // SecurityMode: signing enabled
    b[h + 12..h + 28].copy_from_slice(b"nife smb prober!"); // ClientGuid
    for (i, d) in dialects.iter().enumerate() {
        w16(&mut b, h + 36 + 2 * i, *d);
    }
    msg(h + 36 + 2 * dialects.len(), b)
}

/// The dialect a NEGOTIATE response settled on.
pub fn negotiate_dialect(resp: &[u8]) -> u16 {
    r16(resp, HDR_LEN + 4)
}
/// The `MaxReadSize` a NEGOTIATE response advertised (the prober sizes its reads by it).
pub fn negotiate_max_read(resp: &[u8]) -> u32 {
    r32(resp, HDR_LEN + 32)
}

/// A `SESSION_SETUP` carrying a raw NTLMSSP NEGOTIATE.
pub fn session_setup_negotiate(msg_id: u64) -> Msg {
    let mut token = [0u8; 40];
    token[..8].copy_from_slice(&ntlmssp::SIGNATURE);
    w32(&mut token, 8, ntlmssp::NEGOTIATE);
    w32(&mut token, 12, 0x0000_0201); // flags: unicode | NTLM
    session_setup(msg_id, 0, &token)
}

/// The largest NTLMSSP AUTHENTICATE these builders emit: the 64-byte fixed part plus two names and
/// an NTLMv2 response with room for a real client's target info.
pub const TOKEN_MAX: usize = 64 + 2 * 64 + ntlmssp::PROOF_LEN + 512;

/// An owned NTLMSSP token, fixed capacity, for the same reason [`Msg`] is one.
pub struct Token {
    buf: [u8; TOKEN_MAX],
    len: usize,
}

impl core::ops::Deref for Token {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// **A guest-shaped AUTHENTICATE**: every field pair empty. This is what `mount_smbfs -N` sends and
/// what this crate's own guest tests send, so it stays a first-class builder rather than a
/// degenerate case of the one below: a share with no authenticator admits it, and a share with one
/// must refuse it, and both of those are assertions somebody has to be able to write.
pub fn ntlmssp_authenticate_empty() -> Token {
    let mut buf = [0u8; TOKEN_MAX];
    buf[..8].copy_from_slice(&ntlmssp::SIGNATURE);
    w32(&mut buf, 8, ntlmssp::AUTHENTICATE);
    // Six empty field triples (lm, nt, domain, user, workstation, session key), all offset 64.
    for f in 0..6 {
        w32(&mut buf, 16 + 8 * f, 64);
    }
    w32(&mut buf, 60, 0x0000_0201);
    Token { buf, len: 64 }
}

/// **A real NTLMv2 AUTHENTICATE** ([MS-NLMP] §2.2.1.3): the account, the domain, and
/// `NtChallengeResponse` = `NTProofStr || blob`.
///
/// The proof is the caller's, and that is the whole point of this signature: **this crate computes
/// no key material**, so a caller that wants a verifiable proof brings one (`ntlm::proof` under
/// `ntlm::ntowfv2`, which is a `[dev-dependencies]` of this crate and a real dependency of xtask's
/// prober). A caller that wants an unverifiable one flips a bit, which is what the refusal tests do.
///
/// The names go on the wire as UTF-16LE, uppercased by nobody: the derivation binds the exact bytes
/// ([MS-NLMP] §3.3.2 uppercases the user *inside* the key derivation, not on the wire), so a builder
/// that helpfully cased them would produce a message whose proof did not match its own names.
pub fn ntlmssp_authenticate(
    user: &[u8],
    domain: &[u8],
    proof: &[u8; ntlmssp::PROOF_LEN],
    blob: &[u8],
) -> Token {
    let mut buf = [0u8; TOKEN_MAX];
    buf[..8].copy_from_slice(&ntlmssp::SIGNATURE);
    w32(&mut buf, 8, ntlmssp::AUTHENTICATE);
    w32(&mut buf, 60, 0x0000_0201); // unicode | NTLM

    // The payloads, in the order the field triples below name them. `p` is the running offset from
    // the start of the message, which is what §2.2.2.10's `Offset` means.
    let mut p = 64usize;
    let nt_len = ntlmssp::PROOF_LEN + blob.len();
    buf[p..p + ntlmssp::PROOF_LEN].copy_from_slice(proof);
    buf[p + ntlmssp::PROOF_LEN..p + nt_len].copy_from_slice(blob);
    let nt_at = p;
    p += nt_len;
    let dom_at = p;
    let dom_len = ascii_to_utf16le(domain, &mut buf[p..]);
    p += dom_len;
    let user_at = p;
    let user_len = ascii_to_utf16le(user, &mut buf[p..]);
    p += user_len;

    // LmChallengeResponse: absent, which is what an NTLMv2-only client sends.
    w32(&mut buf, 16, 64);
    for (at, len, off) in [
        (20, nt_len, nt_at),
        (28, dom_len, dom_at),
        (36, user_len, user_at),
        (44, 0, p), // Workstation: none
        (52, 0, p), // EncryptedRandomSessionKey: no key exchange
    ] {
        w16(&mut buf, at, len as u16);
        w16(&mut buf, at + 2, len as u16);
        w32(&mut buf, at + 4, off as u32);
    }
    Token { buf, len: p }
}

/// A `SESSION_SETUP` carrying a raw NTLMSSP AUTHENTICATE with no credentials in it: the guest leg.
pub fn session_setup_authenticate(msg_id: u64, sid: u64) -> Msg {
    session_setup(msg_id, sid, &ntlmssp_authenticate_empty())
}

/// A `SESSION_SETUP` carrying a real NTLMv2 AUTHENTICATE. See [`ntlmssp_authenticate`] on why the
/// proof is the caller's.
pub fn session_setup_authenticate_ntlmv2(
    msg_id: u64,
    sid: u64,
    user: &[u8],
    domain: &[u8],
    proof: &[u8; ntlmssp::PROOF_LEN],
    blob: &[u8],
) -> Msg {
    session_setup(
        msg_id,
        sid,
        &ntlmssp_authenticate(user, domain, proof, blob),
    )
}

/// **The server challenge out of a `SESSION_SETUP` response's CHALLENGE message** ([MS-NLMP]
/// §2.2.1.2 offset 24), which is what a client needs before it can compute anything.
///
/// `None` when the response carries no token or the token is not an NTLMSSP CHALLENGE. Lives here
/// rather than in the prober because it is a wire offset, and every wire offset in this tree lives
/// in this crate (rule 7).
pub fn session_setup_challenge(resp: &[u8]) -> Option<[u8; 8]> {
    let token = session_setup_token(resp)?;
    // A SPNEGO-wrapped CHALLENGE is unwrapped first; a raw one is returned as itself.
    let token = crate::spnego::unwrap_token(token)?;
    if token.len() < 32 || token[..8] != ntlmssp::SIGNATURE || r32(token, 8) != ntlmssp::CHALLENGE {
        return None;
    }
    token[24..32].try_into().ok()
}

/// A `SESSION_SETUP` around an arbitrary security token (the wrapped-token test uses this too).
pub fn session_setup(msg_id: u64, sid: u64, token: &[u8]) -> Msg {
    let mut b = [0u8; MSG_MAX];
    request_header(&mut b, 0, CMD_SESSION_SETUP, msg_id, sid, 0);
    let h = HDR_LEN;
    b[h..h + 24].fill(0);
    w16(&mut b, h, 25); // StructureSize
    let buf_at = h + 24;
    w16(&mut b, h + 12, buf_at as u16); // SecurityBufferOffset
    w16(&mut b, h + 14, token.len() as u16);
    b[buf_at..buf_at + token.len()].copy_from_slice(token);
    msg(buf_at + token.len(), b)
}

/// The security token a `SESSION_SETUP` response carries, if any.
pub fn session_setup_token(resp: &[u8]) -> Option<&[u8]> {
    let off = r16(resp, HDR_LEN + 4) as usize;
    let len = r16(resp, HDR_LEN + 6) as usize;
    if len == 0 {
        return None;
    }
    resp.get(off..off + len)
}

/// A `TREE_CONNECT` to `path` (ASCII, `\\server\share`).
pub fn tree_connect(msg_id: u64, sid: u64, path: &[u8]) -> Msg {
    let mut b = [0u8; MSG_MAX];
    request_header(&mut b, 0, CMD_TREE_CONNECT, msg_id, sid, 0);
    let h = HDR_LEN;
    w16(&mut b, h, 9); // StructureSize
    let buf_at = h + 8;
    let wide = ascii_to_utf16le(path, &mut b[buf_at..]);
    w16(&mut b, h + 4, buf_at as u16);
    w16(&mut b, h + 6, wide as u16);
    msg(buf_at + wide, b)
}

/// A CREATE opening `name` (ASCII, empty for the root), disposition `FILE_OPEN`.
pub fn create(msg_id: u64, sid: u64, tid: u32, name: &[u8]) -> Msg {
    create_disposition(msg_id, sid, tid, name, 1)
}

/// A CREATE with a chosen disposition (the read-only tests probe refusals with it, and the write
/// path opens with it).
pub fn create_disposition(msg_id: u64, sid: u64, tid: u32, name: &[u8], disposition: u32) -> Msg {
    create_full(msg_id, sid, tid, name, disposition, 0)
}

/// A CREATE with a chosen disposition **and** create options: how a client asks for
/// `FILE_DELETE_ON_CLOSE` (`0x1000`), which is what an SMB `rm` is.
pub fn create_full(
    msg_id: u64,
    sid: u64,
    tid: u32,
    name: &[u8],
    disposition: u32,
    options: u32,
) -> Msg {
    let mut b = [0u8; MSG_MAX];
    let len = build_create(&mut b, 0, msg_id, sid, tid, name, disposition, options, 0);
    msg(len, b)
}

/// Write a CREATE at `at`, returning the total length from `at`.
#[allow(clippy::too_many_arguments)]
fn build_create(
    b: &mut [u8],
    at: usize,
    msg_id: u64,
    sid: u64,
    tid: u32,
    name: &[u8],
    disposition: u32,
    options: u32,
    flags: u32,
) -> usize {
    request_header(b, at, CMD_CREATE, msg_id, sid, tid);
    w32(b, at + H_FLAGS, flags);
    let h = at + HDR_LEN;
    b[h..h + 56].fill(0);
    w16(b, h, 57); // StructureSize
    w32(b, h + 4, 2); // ImpersonationLevel: impersonation
    // DesiredAccess: read and write plus DELETE, the whole of what this share offers. The server
    // does not gate on it (a read-only share refuses by disposition and by command instead; see
    // notes/smb.md's wire decisions), so one mask serves every builder.
    w32(b, h + 24, 0x0013_019F);
    w32(b, h + 32, 7); // ShareAccess: read | write | delete
    w32(b, h + 36, disposition);
    w32(b, h + 40, options); // CreateOptions
    let name_at = h + 56;
    let wide = ascii_to_utf16le(name, &mut b[name_at..]);
    w16(b, h + 44, name_at as u16 - at as u16); // NameOffset, from this header
    w16(b, h + 46, wide as u16);
    h + 56 + wide - at
}

/// **A CREATE carrying an `AAPL` create context**, which is how a macOS client negotiates Apple's
/// extensions: it hangs the context off the first CREATE of a tree connect and reads the server's
/// answer off the response ([`crate::apple`]).
///
/// `bitmap` is which of the three answers to ask for and `client_caps` is what the client claims
/// to implement, both spelled by the caller rather than defaulted, because a test whose whole
/// subject is a negotiation should not have half of it hidden in a builder.
pub fn create_aapl(
    msg_id: u64,
    sid: u64,
    tid: u32,
    name: &[u8],
    bitmap: u64,
    client_caps: u64,
) -> Msg {
    let mut b = [0u8; MSG_MAX];
    let body = build_create(&mut b, 0, msg_id, sid, tid, name, 1, 0, 0);
    // The chain goes after the name, 8-byte aligned, and both offsets are from the SMB2 header.
    let at = body.next_multiple_of(8);
    let mut payload = [0u8; crate::apple::REQUEST_LEN];
    w32(&mut payload, 0, crate::apple::CMD_SERVER_QUERY);
    w64(&mut payload, 8, bitmap);
    w64(&mut payload, 16, client_caps);
    let n = crate::create_context::write_one(&mut b[at..], crate::apple::TAG, &payload)
        .expect("MSG_MAX holds one AAPL context");
    w32(&mut b, HDR_LEN + 48, at as u32); // CreateContextsOffset
    w32(&mut b, HDR_LEN + 52, n as u32); // CreateContextsLength
    msg(at + n, b)
}

/// **The payload of the create context tagged `tag` in a CREATE response**, or `None` if the
/// response carried no such context. Reads through the response's own offset pair, so a client
/// and the server share one reading of where a chain lives.
pub fn create_context_data<'a>(resp: &'a [u8], tag: &[u8]) -> Option<&'a [u8]> {
    let off = r32(resp, HDR_LEN + 80) as usize;
    let len = r32(resp, HDR_LEN + 84) as usize;
    if len == 0 {
        return None;
    }
    let blob = off.checked_add(len).and_then(|end| resp.get(off..end))?;
    crate::create_context::find(blob, tag)
}

/// The 16-byte file id a CREATE response carries.
pub fn create_file_id(resp: &[u8]) -> [u8; 16] {
    let mut fid = [0u8; 16];
    fid.copy_from_slice(&resp[HDR_LEN + 64..HDR_LEN + 80]);
    fid
}
/// The `EndOfFile` (size) a CREATE response reports.
pub fn create_end_of_file(resp: &[u8]) -> u64 {
    r64(resp, HDR_LEN + 48)
}
/// The `CreateAction` a CREATE response reports: 0 superseded, 1 opened, 2 created,
/// 3 overwritten. The field a client's create-or-open logic reads, so the tests read it too.
pub fn create_action(resp: &[u8]) -> u32 {
    r32(resp, HDR_LEN + 4)
}

/// A READ of `len` bytes at `offset`.
pub fn read(msg_id: u64, sid: u64, tid: u32, fid: &[u8; 16], offset: u64, len: u32) -> Msg {
    let mut b = [0u8; MSG_MAX];
    request_header(&mut b, 0, CMD_READ, msg_id, sid, tid);
    let h = HDR_LEN;
    b[h..h + 48].fill(0);
    w16(&mut b, h, 49); // StructureSize
    w32(&mut b, h + 4, len);
    w64(&mut b, h + 8, offset);
    b[h + 16..h + 32].copy_from_slice(fid);
    // One trailing buffer byte, which the spec requires the request to carry.
    msg(h + 49, b)
}

/// The data a READ response carries.
pub fn read_data(resp: &[u8]) -> &[u8] {
    let off = resp[HDR_LEN + 2] as usize;
    let len = r32(resp, HDR_LEN + 4) as usize;
    &resp[off..off + len]
}

/// A WRITE of `data` at offset 0.
pub fn write(msg_id: u64, sid: u64, tid: u32, fid: &[u8; 16], data: &[u8]) -> Msg {
    write_at(msg_id, sid, tid, fid, 0, data)
}

/// The size of the WRITE request [`write_at_into`] builds around `len` bytes of payload: the
/// header and the 48-byte body. A caller sizing its own buffer adds this to what it means to send.
pub const WRITE_OVERHEAD: usize = HDR_LEN + 48;

/// **Build a WRITE of `data` at `offset` into `out`**, and answer how many bytes it took.
///
/// The form for a payload that does not fit [`MSG_MAX`], which is every bulk write: the builders
/// return an owned fixed-capacity [`Msg`] because this crate has no allocator, and a 64 KiB message
/// cannot be one. A caller with a buffer passes it here instead. `out` must be at least
/// [`WRITE_OVERHEAD`] plus `data.len()`.
///
/// [`write_at`] is this function with a [`Msg`]'s buffer, so there is one WRITE request in the tree
/// rather than one per caller shape.
pub fn write_at_into(
    out: &mut [u8],
    msg_id: u64,
    sid: u64,
    tid: u32,
    fid: &[u8; 16],
    offset: u64,
    data: &[u8],
) -> usize {
    request_header(out, 0, CMD_WRITE, msg_id, sid, tid);
    let h = HDR_LEN;
    out[h..h + 48].fill(0);
    w16(out, h, 49);
    let data_at = h + 48;
    w16(out, h + 2, data_at as u16); // DataOffset, from the start of the header
    w32(out, h + 4, data.len() as u32);
    w64(out, h + 8, offset);
    out[h + 16..h + 32].copy_from_slice(fid);
    out[data_at..data_at + data.len()].copy_from_slice(data);
    data_at + data.len()
}

/// A WRITE of `data` at `offset`. The bytes must fit [`MSG_MAX`] minus the header and body; a
/// caller writing more than that either chunks its own writes as a real client does against
/// `MaxWriteSize`, or uses [`write_at_into`] with a buffer of its own.
pub fn write_at(msg_id: u64, sid: u64, tid: u32, fid: &[u8; 16], offset: u64, data: &[u8]) -> Msg {
    let mut b = [0u8; MSG_MAX];
    let len = write_at_into(&mut b, msg_id, sid, tid, fid, offset, data);
    msg(len, b)
}

/// The `Count` a WRITE response reports: how many bytes the share actually took.
pub fn write_count(resp: &[u8]) -> u32 {
    r32(resp, HDR_LEN + 4)
}

/// A `SET_INFO` carrying `body` for one file information class ([MS-SMB2] §2.2.39).
pub fn set_info(msg_id: u64, sid: u64, tid: u32, fid: &[u8; 16], class: u8, body: &[u8]) -> Msg {
    let mut b = [0u8; MSG_MAX];
    request_header(&mut b, 0, CMD_SET_INFO, msg_id, sid, tid);
    let h = HDR_LEN;
    b[h..h + 32].fill(0);
    w16(&mut b, h, 33); // StructureSize
    b[h + 2] = 1; // InfoType: FILE
    b[h + 3] = class;
    w32(&mut b, h + 4, body.len() as u32); // BufferLength
    let buf_at = h + 32;
    w16(&mut b, h + 8, buf_at as u16); // BufferOffset
    b[h + 16..h + 32].copy_from_slice(fid);
    b[buf_at..buf_at + body.len()].copy_from_slice(body);
    msg(buf_at + body.len(), b)
}

/// `SET_INFO` / `FileEndOfFileInformation` (class 20): truncate or extend to exactly `size`.
pub fn set_end_of_file(msg_id: u64, sid: u64, tid: u32, fid: &[u8; 16], size: u64) -> Msg {
    set_info(msg_id, sid, tid, fid, 20, &size.to_le_bytes())
}

/// `SET_INFO` / `FileDispositionInformation` (class 13): mark (or unmark) delete-on-close.
pub fn set_disposition(msg_id: u64, sid: u64, tid: u32, fid: &[u8; 16], delete: bool) -> Msg {
    set_info(msg_id, sid, tid, fid, 13, &[delete as u8])
}

/// `SET_INFO` / `FileRenameInformation` (class 10): rename the open file to `to`.
pub fn set_rename(msg_id: u64, sid: u64, tid: u32, fid: &[u8; 16], to: &[u8]) -> Msg {
    let mut body = [0u8; 20 + 2 * 64];
    body[0] = 1; // ReplaceIfExists
    // Bytes 1..8 reserved, 8..16 RootDirectory (must be 0: this share has one directory).
    let wide = ascii_to_utf16le(to, &mut body[20..]);
    w32(&mut body, 16, wide as u32);
    set_info(msg_id, sid, tid, fid, 10, &body[..20 + wide])
}

/// `SET_INFO` / `FileBasicInformation` (class 4): the four timestamps and the attributes a client
/// writes at the end of a copy. This server accepts and discards them (crate BUGS).
pub fn set_basic(msg_id: u64, sid: u64, tid: u32, fid: &[u8; 16], attributes: u32) -> Msg {
    let mut body = [0u8; 40];
    w32(&mut body, 32, attributes);
    set_info(msg_id, sid, tid, fid, 4, &body)
}

/// A CLOSE.
pub fn close(msg_id: u64, sid: u64, tid: u32, fid: &[u8; 16]) -> Msg {
    let mut b = [0u8; MSG_MAX];
    request_header(&mut b, 0, CMD_CLOSE, msg_id, sid, tid);
    let h = HDR_LEN;
    b[h..h + 24].fill(0);
    w16(&mut b, h, 24);
    b[h + 8..h + 24].copy_from_slice(fid);
    msg(h + 24, b)
}

/// A FLUSH.
pub fn flush(msg_id: u64, sid: u64, tid: u32, fid: &[u8; 16]) -> Msg {
    let mut b = [0u8; MSG_MAX];
    request_header(&mut b, 0, CMD_FLUSH, msg_id, sid, tid);
    let h = HDR_LEN;
    b[h..h + 24].fill(0);
    w16(&mut b, h, 24);
    b[h + 8..h + 24].copy_from_slice(fid);
    msg(h + 24, b)
}

/// A LOGOFF: the clean end of a session, and what lets the server end the connection without
/// waiting out a receive timeout (see `smb_server`'s BUGS).
pub fn logoff(msg_id: u64, sid: u64) -> Msg {
    let mut b = [0u8; MSG_MAX];
    request_header(&mut b, 0, crate::CMD_LOGOFF, msg_id, sid, 0);
    let h = HDR_LEN;
    w16(&mut b, h, 4);
    w16(&mut b, h + 2, 0);
    msg(h + 4, b)
}

/// An ECHO.
pub fn echo(msg_id: u64) -> Msg {
    let mut b = [0u8; MSG_MAX];
    request_header(&mut b, 0, CMD_ECHO, msg_id, 0, 0);
    let h = HDR_LEN;
    w16(&mut b, h, 4);
    w16(&mut b, h + 2, 0);
    msg(h + 4, b)
}

/// A `QUERY_DIRECTORY` for `pattern` in information class `class`.
pub fn query_directory(
    msg_id: u64,
    sid: u64,
    tid: u32,
    fid: &[u8; 16],
    class: u8,
    pattern: &[u8],
) -> Msg {
    query_directory_flags(msg_id, sid, tid, fid, class, pattern, 0)
}

/// The same with explicit flags (`0x01` restarts the scan).
#[allow(clippy::too_many_arguments)]
pub fn query_directory_flags(
    msg_id: u64,
    sid: u64,
    tid: u32,
    fid: &[u8; 16],
    class: u8,
    pattern: &[u8],
    flags: u8,
) -> Msg {
    let mut b = [0u8; MSG_MAX];
    request_header(&mut b, 0, CMD_QUERY_DIRECTORY, msg_id, sid, tid);
    let h = HDR_LEN;
    b[h..h + 32].fill(0);
    w16(&mut b, h, 33);
    b[h + 2] = class;
    b[h + 3] = flags;
    b[h + 8..h + 24].copy_from_slice(fid);
    let pat_at = h + 32;
    let wide = ascii_to_utf16le(pattern, &mut b[pat_at..]);
    w16(&mut b, h + 24, pat_at as u16);
    w16(&mut b, h + 26, wide as u16);
    w32(&mut b, h + 28, 0x1_0000); // OutputBufferLength: plenty
    msg(pat_at + wide, b)
}

/// One name from a directory listing, ASCII-lowered, owned so the iterator needs no allocator.
pub struct DirName {
    buf: [u8; 64],
    len: usize,
}
impl DirName {
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// The names a `QUERY_DIRECTORY` response lists, in order. `class` must be the class requested,
/// because the name's position depends on it.
pub fn dir_entries(resp: &[u8], class: u8) -> impl Iterator<Item = DirName> + '_ {
    let off = r16(resp, HDR_LEN + 2) as usize;
    let total = r32(resp, HDR_LEN + 4) as usize;
    let name_at = match class {
        1 => 64,
        2 => 68,
        3 => 94,
        37 => 104,
        38 => 80,
        12 => 12,
        _ => usize::MAX,
    };
    let name_len_at = if class == 12 { 8 } else { 60 };
    let mut pos = off;
    let end = off + total;
    core::iter::from_fn(move || {
        if name_at == usize::MAX || pos >= end || pos >= resp.len() {
            return None;
        }
        let entry = &resp[pos..end.min(resp.len())];
        let next = r32(entry, 0) as usize;
        let nlen = r32(entry, name_len_at) as usize;
        let wide = entry.get(name_at..name_at + nlen)?;
        let mut buf = [0u8; 64];
        let len = utf16le_to_ascii_lower(wide, &mut buf)?.len();
        pos = if next == 0 { end } else { pos + next };
        Some(DirName { buf, len })
    })
}

/// A `QUERY_INFO` for `FileAllInformation` (info type FILE, class 18).
pub fn query_info_all(msg_id: u64, sid: u64, tid: u32, fid: &[u8; 16]) -> Msg {
    let mut b = [0u8; MSG_MAX];
    let len = build_query_info_all(&mut b, 0, msg_id, sid, tid, fid, 0);
    msg(len, b)
}

/// A `QUERY_INFO` for an arbitrary info type and class: 1 is FILE, 2 is FILESYSTEM. The volume
/// classes are how a client learns whether the share is writable before it tries.
pub fn query_info(
    msg_id: u64,
    sid: u64,
    tid: u32,
    fid: &[u8; 16],
    info_type: u8,
    class: u8,
) -> Msg {
    let mut b = [0u8; MSG_MAX];
    request_header(&mut b, 0, CMD_QUERY_INFO, msg_id, sid, tid);
    let h = HDR_LEN;
    b[h..h + 40].fill(0);
    w16(&mut b, h, 41);
    b[h + 2] = info_type;
    b[h + 3] = class;
    w32(&mut b, h + 4, 0x1_0000); // OutputBufferLength
    b[h + 24..h + 40].copy_from_slice(fid);
    msg(h + 41, b)
}

fn build_query_info_all(
    b: &mut [u8],
    at: usize,
    msg_id: u64,
    sid: u64,
    tid: u32,
    fid: &[u8; 16],
    flags: u32,
) -> usize {
    request_header(b, at, CMD_QUERY_INFO, msg_id, sid, tid);
    w32(b, at + H_FLAGS, flags);
    let h = at + HDR_LEN;
    b[h..h + 40].fill(0);
    w16(b, h, 41);
    b[h + 2] = 1; // InfoType FILE
    b[h + 3] = 18; // FileAllInformation
    w32(b, h + 4, 0x1_0000); // OutputBufferLength
    b[h + 24..h + 40].copy_from_slice(fid);
    h + 41 - at
}

/// The compound macOS uses to stat a path: CREATE, then `QUERY_INFO` and CLOSE as related
/// operations naming the `0xFF..FF` file id. Three headers in one message, chained by
/// `NextCommand`.
pub fn compound_create_query_close(msg_id: u64, sid: u64, tid: u32, name: &[u8]) -> Msg {
    let mut b = [0u8; MSG_MAX];
    let fid = [0xFFu8; 16];
    let mut pos = build_create(&mut b, 0, msg_id, sid, tid, name, 1, 0, 0);
    pos = pad8(&mut b, pos);
    w32(&mut b, H_NEXT_COMMAND, pos as u32);

    let q_at = pos;
    let q_len = build_query_info_all(
        &mut b,
        q_at,
        msg_id + 1,
        sid,
        tid,
        &fid,
        FLAG_RELATED_OPERATIONS,
    );
    pos = pad8(&mut b, q_at + q_len);
    w32(&mut b, q_at + H_NEXT_COMMAND, (pos - q_at) as u32);

    let c_at = pos;
    request_header(&mut b, c_at, CMD_CLOSE, msg_id + 2, sid, tid);
    w32(&mut b, c_at + H_FLAGS, FLAG_RELATED_OPERATIONS);
    let h = c_at + HDR_LEN;
    b[h..h + 24].fill(0);
    w16(&mut b, h, 24);
    b[h + 8..h + 24].copy_from_slice(&fid);
    msg(h + 24, b)
}

/// Zero-pad to the next 8-byte boundary, returning the new position.
fn pad8(b: &mut [u8], mut pos: usize) -> usize {
    while !pos.is_multiple_of(8) {
        b[pos] = 0;
        pos += 1;
    }
    pos
}
