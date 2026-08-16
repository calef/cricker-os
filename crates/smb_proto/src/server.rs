//! **The per-connection SMB2 state machine**: one inbound message (possibly a compound chain) in,
//! one response message out, against a [`Share`].
//!
//! Pure logic over byte slices, no allocation, no IO, so the whole protocol runs under host tests
//! (see the bottom of this file for a full client session driven through it) and stays reachable
//! by the tree's verification method. The `smb_server` role feeds it bytes from the socket
//! contract and writes back what it returns; nothing else is between this machine and the wire.
//!
//! What it implements, and the shape of what it refuses, are the crate header's scope: SMB 2.1,
//! guest sessions, a flat share, compounds with related operations (macOS opens files as
//! CREATE + `QUERY_INFO` + CLOSE chains, so compounds are not optional). Everything refused is
//! refused with a real NT status, because a client's retry logic keys on them.
//!
//! # The write path, and where read-only is enforced
//!
//! The mutating commands are `CREATE` with a disposition that creates or truncates, `WRITE`, and
//! `SET_INFO`. Each one asks [`Share::writable`] **first**, and a share that answers `false` gets
//! [`STATUS_ACCESS_DENIED`] without the backing being called at all. That ordering is deliberate
//! rather than incidental: refusing here is a statement a client can act on before it opens a
//! file, and it means a read-only share is read-only even if its backing would have said yes.
//! [`crate::share`]'s trait defaults are the independent second line.

use crate::share::{Error, FileId, Node, Share};
use crate::{
    CMD_CHANGE_NOTIFY, CMD_CLOSE, CMD_CREATE, CMD_ECHO, CMD_FLUSH, CMD_IOCTL, CMD_LOCK, CMD_LOGOFF,
    CMD_NEGOTIATE, CMD_QUERY_DIRECTORY, CMD_QUERY_INFO, CMD_READ, CMD_SESSION_SETUP, CMD_SET_INFO,
    CMD_TREE_CONNECT, CMD_TREE_DISCONNECT, CMD_WRITE, DIALECT_0210, DIALECT_WILDCARD, H_COMMAND,
    H_CREDIT, H_MESSAGE_ID, H_NEXT_COMMAND, H_SESSION_ID, H_TREE_ID, HDR_LEN, MAX_TRANSACT,
    STATUS_ACCESS_DENIED, STATUS_BAD_NETWORK_NAME, STATUS_DISK_FULL, STATUS_END_OF_FILE,
    STATUS_FILE_CLOSED, STATUS_FILE_IS_A_DIRECTORY, STATUS_FS_DRIVER_REQUIRED,
    STATUS_INVALID_PARAMETER, STATUS_MORE_PROCESSING_REQUIRED, STATUS_NO_MORE_FILES,
    STATUS_NOT_SUPPORTED, STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_INVALID,
    STATUS_OBJECT_NAME_NOT_FOUND, STATUS_OBJECT_PATH_NOT_FOUND, STATUS_SUCCESS,
    STATUS_UNEXPECTED_IO_ERROR, STATUS_USER_SESSION_DELETED, ascii_to_utf16le, is_smb2, ntlmssp,
    r16, r32, r64, spnego, utf16le_to_ascii_lower, w16, w32, w64, write_response_header,
};

/// How many files (and the root) one connection may hold open at once. macOS keeps a handful open
/// during a browse; sixteen is comfortable, and the seventeenth gets a real status rather than a
/// corrupted table.
pub const MAX_HANDLES: usize = 16;

/// **The longest name this share will carry**, in ASCII bytes. A handle keeps its own copy of its
/// name (the write path needs one for rename, for delete-on-close, and for `QUERY_INFO`'s name
/// class, none of which can re-derive it from a listing that moves), so the bound is what a
/// connection's handle table costs: `MAX_HANDLES * MAX_NAME` bytes of the [`Connection`], which
/// lives on the adapter's small stack. A longer name is [`STATUS_OBJECT_NAME_INVALID`], said out
/// loud rather than truncated into a different file's name.
pub const MAX_NAME: usize = 64;

/// `STATUS_INSUFFICIENT_RESOURCES`, the seventeenth handle's answer.
pub const STATUS_INSUFFICIENT_RESOURCES: u32 = 0xC000_009A;

/// The share's name: what a client must ask for in `TREE_CONNECT`'s path after the server part.
/// One share, fixed; the mount instructions in notes/smb.md use it.
pub const SHARE_NAME: &[u8] = b"share";

/// File attribute bits this server emits.
const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_NORMAL: u32 = 0x80;

/// `SESSION_SETUP` response flag: this session is a guest. Set on every session this server
/// grants, which is the honest label for "nothing was verified".
const SESSION_FLAG_IS_GUEST: u16 = 0x0001;

/// CREATE dispositions ([MS-SMB2] §2.2.13). The four that write are the write path's whole
/// create story, and naming them here is what keeps the dispatch below readable.
const FILE_SUPERSEDE: u32 = 0;
const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_OPEN_IF: u32 = 3;
const FILE_OVERWRITE: u32 = 4;
const FILE_OVERWRITE_IF: u32 = 5;

/// `CreateAction`, the CREATE response's word for what actually happened. A client's create-or-open
/// logic reads it, so getting it wrong is worse than refusing.
const ACTION_SUPERSEDED: u32 = 0;
const ACTION_OPENED: u32 = 1;
const ACTION_CREATED: u32 = 2;
const ACTION_OVERWRITTEN: u32 = 3;

/// `CreateOptions` bits this server reads ([MS-SMB2] §2.2.13).
const OPT_DIRECTORY_FILE: u32 = 0x0000_0001;
const OPT_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
/// The client asking that the name go away when the last handle closes. This is how macOS's
/// `unlink` reaches an SMB server: open, mark, close.
const OPT_DELETE_ON_CLOSE: u32 = 0x0000_1000;

/// `SET_INFO` info types and the file classes this server acts on ([MS-FSCC] §2.4).
const INFO_TYPE_FILE: u8 = 1;
const CLASS_BASIC: u8 = 4;
const CLASS_RENAME: u8 = 10;
const CLASS_DISPOSITION: u8 = 13;
const CLASS_ALLOCATION: u8 = 19;
const CLASS_END_OF_FILE: u8 = 20;

/// The NT status one [`Error`] becomes. **The one place the mapping lives**, so a client meets one
/// answer for one condition however the request reached it; a per-call-site guess is how a
/// protocol grows two statuses for the same thing.
pub const fn status_for(e: Error) -> u32 {
    match e {
        Error::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
        Error::Exists => STATUS_OBJECT_NAME_COLLISION,
        Error::IsDirectory => STATUS_FILE_IS_A_DIRECTORY,
        Error::ReadOnly => STATUS_ACCESS_DENIED,
        Error::NoSpace => STATUS_DISK_FULL,
        Error::NameTooLong => STATUS_OBJECT_NAME_INVALID,
        Error::Io => STATUS_UNEXPECTED_IO_ERROR,
    }
}

/// One open handle: the node, its name, and (for the root) where its directory enumeration has
/// got to.
#[derive(Clone, Copy)]
struct Handle {
    node: Node,
    /// The volatile half of the wire file id, so a stale id from a closed handle misses.
    volatile: u64,
    /// The name this handle was opened under, ASCII, lower-cased. Kept per handle because a
    /// writable share's listing moves under it: rename, delete-on-close and `QUERY_INFO`'s name
    /// class all need the name, and none of them can go and look it up again.
    name: [u8; MAX_NAME],
    name_len: u8,
    /// Set by `FILE_DELETE_ON_CLOSE` or by `SET_INFO`'s disposition class. The name goes at CLOSE,
    /// which is [MS-FSCC] §2.4.11's semantics and Unix's `unlink`: an open handle keeps reading.
    delete_on_close: bool,
    /// The next [`Share::entry`] index `QUERY_DIRECTORY` will emit; 0 and 1 are the synthetic
    /// `.` and `..` rows.
    enum_index: usize,
    /// Set once enumeration answered `STATUS_NO_MORE_FILES`, so the next query starts over only
    /// if the client asks for a restart.
    enum_done: bool,
}

impl Handle {
    /// The name this handle carries.
    fn name(&self) -> &[u8] {
        &self.name[..self.name_len as usize]
    }
}

/// The connection state machine. One per TCP connection; drop it when the connection closes.
pub struct Connection {
    negotiated: bool,
    /// Nonzero once a session-setup exchange has begun; `ready` once it completed.
    session_id: u64,
    session_ready: bool,
    tree_id: u32,
    handles: [Option<Handle>; MAX_HANDLES],
    next_volatile: u64,
    /// The NTLMSSP server challenge for this connection, provided by the caller (it is the one
    /// input that should differ per connection, and this crate has no entropy of its own).
    challenge: [u8; 8],
}

/// What one compound element needs from the chain: the file id CREATE produced, carried to
/// related operations that name `0xFFFF..FF`.
const RELATED_FID: [u8; 16] = [0xFF; 16];

impl Connection {
    pub fn new(challenge: [u8; 8]) -> Self {
        Self {
            negotiated: false,
            session_id: 0,
            session_ready: false,
            tree_id: 0,
            handles: [None; MAX_HANDLES],
            next_volatile: 1,
            challenge,
        }
    }

    /// Handle one transport message (one SMB2 message or compound chain), writing the response
    /// message into `out` and returning its length. `None` means the bytes were not SMB2 at all
    /// and the connection should be dropped; everything else, however wrong, gets an SMB2 status
    /// answer, because that is what a client can act on.
    ///
    /// `out` must hold [`crate::MAX_MESSAGE`] bytes.
    pub fn handle(&mut self, msg: &[u8], out: &mut [u8], share: &impl Share) -> Option<usize> {
        // The one SMB1 message this server answers, because it is how every real client arrives:
        // macOS's own mount_smbfs opens with an SMB1 multi-protocol NEGOTIATE (captured
        // 2026-08-15; the bytes are pinned in the test below), offering "NT LM 0.12",
        // "SMB 2.002" and "SMB 2.???". [MS-SMB2] §3.3.5.3.1: when the strings claim SMB2, the
        // answer is an SMB2 NEGOTIATE response carrying the wildcard revision 0x02FF, message id
        // 0, and the client then sends a real SMB2 NEGOTIATE. A client that offers only SMB1
        // dialects gets the same treatment as any other protocol this server does not speak:
        // the connection drops.
        if !self.negotiated && is_smb1_negotiate(msg) {
            if !smb1_offers_smb2(msg) {
                return None;
            }
            return Some(self.negotiate_response(out, 0, 1, DIALECT_WILDCARD));
        }
        if !is_smb2(msg) {
            return None;
        }
        let mut in_off = 0usize;
        let mut out_pos = 0usize;
        let mut chain_fid: Option<[u8; 16]> = None;

        loop {
            let req = &msg[in_off..];
            if !is_smb2(req) {
                return None;
            }
            let next = r32(req, H_NEXT_COMMAND) as usize;
            // This element's extent: to the next header, or to the end of the message.
            let elem = if next != 0 {
                if next < HDR_LEN || in_off + next > msg.len() {
                    return None;
                }
                &msg[in_off..in_off + next]
            } else {
                req
            };

            let start = out_pos;
            let n = self.dispatch(elem, &mut out[start..], share, &mut chain_fid);
            out_pos += n;

            if next == 0 {
                break;
            }
            // Pad this response to 8 alignment and point its NextCommand at the following one.
            while !out_pos.is_multiple_of(8) {
                out[out_pos] = 0;
                out_pos += 1;
            }
            w32(out, start + H_NEXT_COMMAND, (out_pos - start) as u32);
            in_off += next;
        }
        Some(out_pos)
    }

    /// Dispatch one request element, returning the bytes written at `out[..]`.
    fn dispatch(
        &mut self,
        req: &[u8],
        out: &mut [u8],
        share: &impl Share,
        chain_fid: &mut Option<[u8; 16]>,
    ) -> usize {
        let cmd = r16(req, H_COMMAND);
        let msg_id = r64(req, H_MESSAGE_ID);
        let credits = r16(req, H_CREDIT).max(1);
        let sid = r64(req, H_SESSION_ID);
        let tid = r32(req, H_TREE_ID);

        // A tiny closure would capture `out` and fight the later writes; a macro-free helper is
        // clearer. `err` builds the standard 9-byte error response body.
        let err = |out: &mut [u8], status: u32| -> usize {
            write_response_header(out, cmd, status, msg_id, sid, tid, credits, 0);
            let b = HDR_LEN;
            out[b..b + 9].fill(0);
            w16(out, b, 9);
            HDR_LEN + 9
        };

        // The state gates, in the order the protocol layers them.
        match cmd {
            CMD_NEGOTIATE | CMD_ECHO => {}
            CMD_SESSION_SETUP => {
                if !self.negotiated {
                    return err(out, STATUS_INVALID_PARAMETER);
                }
            }
            _ => {
                if !self.session_ready || sid != self.session_id {
                    return err(out, STATUS_USER_SESSION_DELETED);
                }
                if !matches!(cmd, CMD_TREE_CONNECT | CMD_LOGOFF) && tid != self.tree_id {
                    return err(out, STATUS_INVALID_PARAMETER);
                }
            }
        }

        match cmd {
            CMD_NEGOTIATE => self.negotiate(req, out, msg_id, credits, err),
            CMD_SESSION_SETUP => self.session_setup(req, out, msg_id, credits, err),
            CMD_TREE_CONNECT => self.tree_connect(req, out, msg_id, credits, share.writable(), err),
            CMD_TREE_DISCONNECT => {
                self.tree_id = 0;
                simple_ok(out, cmd, msg_id, sid, tid, credits)
            }
            CMD_LOGOFF => {
                *self = Connection::new(self.challenge);
                self.negotiated = true; // logoff ends the session, not the negotiation
                simple_ok(out, cmd, msg_id, sid, tid, credits)
            }
            CMD_CREATE => self.create(req, out, share, chain_fid, err),
            CMD_CLOSE => self.close(req, out, share, chain_fid, err),
            CMD_READ => self.read(req, out, share, chain_fid, err),
            CMD_WRITE => self.write(req, out, share, chain_fid, err),
            CMD_SET_INFO => self.set_info(req, out, share, chain_fid, err),
            CMD_QUERY_DIRECTORY => self.query_directory(req, out, share, chain_fid, err),
            CMD_QUERY_INFO => self.query_info(req, out, share, chain_fid, err),
            CMD_ECHO | CMD_FLUSH => simple_ok(out, cmd, msg_id, sid, tid, credits),
            // The not-heres. DFS referrals get the status Windows uses for "no DFS here" so
            // clients fall back to the plain path.
            CMD_IOCTL => err(out, STATUS_FS_DRIVER_REQUIRED),
            CMD_LOCK | CMD_CHANGE_NOTIFY => err(out, STATUS_NOT_SUPPORTED),
            _ => err(out, STATUS_NOT_SUPPORTED),
        }
    }

    fn negotiate(
        &mut self,
        req: &[u8],
        out: &mut [u8],
        msg_id: u64,
        credits: u16,
        err: impl Fn(&mut [u8], u32) -> usize,
    ) -> usize {
        // The client's dialect list must contain the one dialect we speak.
        let count = r16(req, 66) as usize;
        let mut offered = false;
        for i in 0..count {
            if r16(req, 100 + 2 * i) == DIALECT_0210 {
                offered = true;
            }
        }
        if !offered {
            return err(out, STATUS_NOT_SUPPORTED);
        }
        self.negotiated = true;
        self.negotiate_response(out, msg_id, credits, DIALECT_0210)
    }

    /// The SMB2 NEGOTIATE response body, shared by the real negotiate and the SMB1 wildcard
    /// answer below (which differ only in the dialect field and in whether negotiation is done).
    fn negotiate_response(&self, out: &mut [u8], msg_id: u64, credits: u16, dialect: u16) -> usize {
        write_response_header(out, CMD_NEGOTIATE, STATUS_SUCCESS, msg_id, 0, 0, credits, 0);
        let b = HDR_LEN;
        out[b..b + 64].fill(0);
        w16(out, b, 65); // StructureSize
        w16(out, b + 2, 1); // SecurityMode: signing enabled, not required
        w16(out, b + 4, dialect);
        // ServerGuid: fixed, recognisable, not pretending to be random. A GUID distinguishes
        // servers to a client that talks to several; one QEMU guest is one server.
        out[b + 8..b + 24].copy_from_slice(b"nife smb server!");
        w32(out, b + 28, MAX_TRANSACT); // MaxTransactSize
        w32(out, b + 32, MAX_TRANSACT); // MaxReadSize
        w32(out, b + 36, MAX_TRANSACT); // MaxWriteSize
        // SystemTime / ServerStartTime stay zero: this system's wall clock is a capability the
        // server does not hold, and a zero FILETIME is "unknown", which is true.
        let hint_at = b + 64;
        let hint_len = spnego::build_negotiate_hint(&mut out[hint_at..]);
        w16(out, b + 56, hint_at as u16); // SecurityBufferOffset
        w16(out, b + 58, hint_len as u16);
        hint_at + hint_len
    }

    fn session_setup(
        &mut self,
        req: &[u8],
        out: &mut [u8],
        msg_id: u64,
        credits: u16,
        err: impl Fn(&mut [u8], u32) -> usize,
    ) -> usize {
        let sec_off = r16(req, 76) as usize;
        let sec_len = r16(req, 78) as usize;
        let Some(sec) = req.get(sec_off..sec_off + sec_len) else {
            return err(out, STATUS_INVALID_PARAMETER);
        };
        let Some(token) = spnego::unwrap_token(sec) else {
            return err(out, STATUS_INVALID_PARAMETER);
        };
        // The reply wraps its token the way the request wrapped its own, so a raw-NTLMSSP client
        // (the tests, the prober) is answered in kind and a SPNEGO one gets SPNEGO.
        let wrapped = sec.first() != Some(&ntlmssp::SIGNATURE[0]);

        match ntlmssp::parse(token) {
            Some(ntlmssp::Message::Negotiate) => {
                if self.session_id == 0 {
                    self.session_id = 0x1D0_0001; // one session per connection; the value is arbitrary
                }
                let mut challenge = [0u8; ntlmssp::CHALLENGE_MAX];
                let challenge_len = ntlmssp::build_challenge(&mut challenge, &self.challenge);
                write_response_header(
                    out,
                    CMD_SESSION_SETUP,
                    STATUS_MORE_PROCESSING_REQUIRED,
                    msg_id,
                    self.session_id,
                    0,
                    credits,
                    0,
                );
                let b = HDR_LEN;
                out[b..b + 8].fill(0);
                w16(out, b, 9); // StructureSize
                w16(out, b + 2, 0); // SessionFlags: not decided yet
                let buf_at = b + 8;
                let blen = if wrapped {
                    spnego::build_challenge_resp(&mut out[buf_at..], &challenge[..challenge_len])
                } else {
                    out[buf_at..buf_at + challenge_len]
                        .copy_from_slice(&challenge[..challenge_len]);
                    challenge_len
                };
                w16(out, b + 4, buf_at as u16);
                w16(out, b + 6, blen as u16);
                buf_at + blen
            }
            Some(ntlmssp::Message::Authenticate) => {
                if self.session_id == 0 {
                    // AUTHENTICATE with no prior exchange: tolerated, guest is guest either way.
                    self.session_id = 0x1D0_0001;
                }
                self.session_ready = true;
                write_response_header(
                    out,
                    CMD_SESSION_SETUP,
                    STATUS_SUCCESS,
                    msg_id,
                    self.session_id,
                    0,
                    credits,
                    0,
                );
                let b = HDR_LEN;
                out[b..b + 8].fill(0);
                w16(out, b, 9);
                w16(out, b + 2, SESSION_FLAG_IS_GUEST);
                let buf_at = b + 8;
                let blen = if wrapped {
                    out[buf_at..buf_at + spnego::ACCEPT_COMPLETED_RESP.len()]
                        .copy_from_slice(&spnego::ACCEPT_COMPLETED_RESP);
                    spnego::ACCEPT_COMPLETED_RESP.len()
                } else {
                    0
                };
                w16(out, b + 4, if blen == 0 { 0 } else { buf_at as u16 });
                w16(out, b + 6, blen as u16);
                buf_at + blen
            }
            None => err(out, STATUS_INVALID_PARAMETER),
        }
    }

    fn tree_connect(
        &mut self,
        req: &[u8],
        out: &mut [u8],
        msg_id: u64,
        credits: u16,
        writable: bool,
        err: impl Fn(&mut [u8], u32) -> usize,
    ) -> usize {
        let path_off = r16(req, 68) as usize;
        let path_len = r16(req, 70) as usize;
        let Some(path) = req.get(path_off..path_off + path_len) else {
            return err(out, STATUS_INVALID_PARAMETER);
        };
        let mut buf = [0u8; 128];
        let Some(path) = utf16le_to_ascii_lower(path, &mut buf) else {
            return err(out, STATUS_BAD_NETWORK_NAME);
        };
        // The path is `\\server\share`; the share name is the last backslash component. The
        // server part is whatever the client typed to reach us (an IP, usually) and is not ours
        // to check.
        let name = match path.iter().rposition(|&c| c == b'\\') {
            Some(i) => &path[i + 1..],
            None => path,
        };
        if name != SHARE_NAME {
            return err(out, STATUS_BAD_NETWORK_NAME);
        }
        self.tree_id = 1;
        write_response_header(
            out,
            CMD_TREE_CONNECT,
            STATUS_SUCCESS,
            msg_id,
            self.session_id,
            self.tree_id,
            credits,
            0,
        );
        let b = HDR_LEN;
        out[b..b + 16].fill(0);
        w16(out, b, 16); // StructureSize
        out[b + 2] = 1; // ShareType: disk
        // MaximalAccess: what this share will actually do, which is the point of the field. A
        // client that reads it (macOS does) refuses a write client-side on a read-only share and
        // never spends a round trip finding out.
        w32(out, b + 12, maximal_access(writable));
        b + 16
    }

    /// Resolve a request's 16-byte file id (honouring the compound `RELATED_FID`) to a handle
    /// slot.
    fn resolve_fid(
        &self,
        req: &[u8],
        at: usize,
        chain_fid: &Option<[u8; 16]>,
    ) -> Result<usize, u32> {
        let mut fid = [0u8; 16];
        let Some(raw) = req.get(at..at + 16) else {
            return Err(STATUS_INVALID_PARAMETER);
        };
        fid.copy_from_slice(raw);
        if fid == RELATED_FID {
            match chain_fid {
                Some(f) => fid = *f,
                None => return Err(STATUS_INVALID_PARAMETER),
            }
        }
        let slot = u64::from_le_bytes(fid[..8].try_into().unwrap()) as usize;
        let volatile = u64::from_le_bytes(fid[8..].try_into().unwrap());
        match self.handles.get(slot) {
            Some(Some(h)) if h.volatile == volatile => Ok(slot),
            _ => Err(STATUS_FILE_CLOSED),
        }
    }

    /// CREATE, which after the write path is the share's whole open-and-make story: it resolves
    /// the name, applies the disposition (create, truncate, or neither), and installs a handle.
    ///
    /// The disposition is where read-only lives. Every disposition except `FILE_OPEN` can change
    /// the share, so on a share that answers `writable() == false` each of them is refused here,
    /// before the backing is asked. `FILE_OPEN_IF` is the interesting one: on a read-only share it
    /// degrades to `FILE_OPEN` rather than being refused outright, because "open it if it is
    /// there" is answerable without writing anything and refusing it would break clients that
    /// open every file that way.
    fn create(
        &mut self,
        req: &[u8],
        out: &mut [u8],
        share: &impl Share,
        chain_fid: &mut Option<[u8; 16]>,
        err: impl Fn(&mut [u8], u32) -> usize,
    ) -> usize {
        let disposition = r32(req, 100);
        let options = r32(req, 104);
        let name_off = r16(req, 108) as usize;
        let name_len = r16(req, 110) as usize;
        let Some(wide) = req.get(name_off..name_off + name_len) else {
            return err(out, STATUS_INVALID_PARAMETER);
        };
        let mut buf = [0u8; 128];
        let Some(mut name) = (if wide.is_empty() {
            Some(&[] as &[u8])
        } else {
            utf16le_to_ascii_lower(wide, &mut buf)
        }) else {
            return err(out, STATUS_OBJECT_NAME_NOT_FOUND);
        };
        if name.first() == Some(&b'\\') {
            name = &name[1..];
        }
        if name.contains(&b'\\') {
            // A flat share has no paths below the root; saying "path not found" (not "name") is
            // what tells the client which component failed.
            return err(out, STATUS_OBJECT_PATH_NOT_FOUND);
        }
        if name.len() > MAX_NAME {
            return err(out, STATUS_OBJECT_NAME_INVALID);
        }

        // The read-only gate, before the backing hears about it. Only FILE_OPEN is unconditionally
        // harmless; FILE_OPEN_IF is demoted rather than refused (see the doc comment).
        let writable = share.writable();
        let mut disposition = disposition;
        if !writable {
            if disposition == FILE_OPEN_IF {
                disposition = FILE_OPEN;
            }
            if disposition != FILE_OPEN || options & OPT_DELETE_ON_CLOSE != 0 {
                return err(out, STATUS_ACCESS_DENIED);
            }
        }

        // The root is opened without asking the backing anything: it is the one node the share
        // model guarantees, and a directory has no id to mint.
        let (node, action) = if name.is_empty() {
            if !matches!(
                disposition,
                FILE_OPEN | FILE_OPEN_IF | FILE_SUPERSEDE | FILE_OVERWRITE_IF
            ) {
                return err(out, STATUS_ACCESS_DENIED);
            }
            (Node::Root, ACTION_OPENED)
        } else {
            match open_with_disposition(share, name, disposition) {
                Ok((id, action)) => (Node::File(id), action),
                Err(e) => return err(out, status_for(e)),
            }
        };

        // CreateOptions can insist on one kind of node.
        let is_dir = matches!(node, Node::Root);
        if options & OPT_DIRECTORY_FILE != 0 && !is_dir {
            if let Node::File(id) = node {
                share.close(id);
            }
            return err(out, STATUS_NOT_SUPPORTED); // FILE_DIRECTORY_FILE on a file
        }
        if options & OPT_NON_DIRECTORY_FILE != 0 && is_dir {
            return err(out, STATUS_FILE_IS_A_DIRECTORY); // FILE_NON_DIRECTORY_FILE on the root
        }

        let Some(slot) = self.handles.iter().position(Option::is_none) else {
            if let Node::File(id) = node {
                share.close(id);
            }
            return err(out, STATUS_INSUFFICIENT_RESOURCES);
        };
        let volatile = self.next_volatile;
        self.next_volatile += 1;
        let mut stored = [0u8; MAX_NAME];
        stored[..name.len()].copy_from_slice(name);
        self.handles[slot] = Some(Handle {
            node,
            volatile,
            name: stored,
            name_len: name.len() as u8,
            delete_on_close: options & OPT_DELETE_ON_CLOSE != 0,
            enum_index: 0,
            enum_done: false,
        });

        let mut fid = [0u8; 16];
        fid[..8].copy_from_slice(&(slot as u64).to_le_bytes());
        fid[8..].copy_from_slice(&volatile.to_le_bytes());
        *chain_fid = Some(fid);

        let (size, attrs) = match node {
            Node::Root => (0, ATTR_DIRECTORY),
            Node::File(i) => (share.size(i), ATTR_NORMAL),
        };
        write_response_header(
            out,
            CMD_CREATE,
            STATUS_SUCCESS,
            r64(req, H_MESSAGE_ID),
            self.session_id,
            self.tree_id,
            r16(req, H_CREDIT).max(1),
            0,
        );
        let b = HDR_LEN;
        out[b..b + 88].fill(0);
        w16(out, b, 89); // StructureSize
        w32(out, b + 4, action); // CreateAction: what actually happened
        w64(out, b + 40, size); // AllocationSize
        w64(out, b + 48, size); // EndOfFile
        w32(out, b + 56, attrs);
        out[b + 64..b + 80].copy_from_slice(&fid);
        b + 88
    }

    fn close(
        &mut self,
        req: &[u8],
        out: &mut [u8],
        share: &impl Share,
        chain_fid: &mut Option<[u8; 16]>,
        err: impl Fn(&mut [u8], u32) -> usize,
    ) -> usize {
        let slot = match self.resolve_fid(req, 72, chain_fid) {
            Ok(s) => s,
            Err(status) => return err(out, status),
        };
        let h = self.handles[slot].take().unwrap();
        let (size, attrs) = match h.node {
            Node::Root => (0, ATTR_DIRECTORY),
            Node::File(i) => (share.size(i), ATTR_NORMAL),
        };
        // The size above is read while the handle is still open, because closing it is what makes
        // the backing forget the file; the POSTQUERY_ATTRIB fields below have to be the truth as
        // of the close.
        if let Node::File(i) = h.node {
            share.close(i);
        }
        // Delete-on-close: the name goes after the handle does. A failure is not reported, because
        // CLOSE has no field to report it in and the client has already stopped caring; the share
        // keeps the file, which is the safe direction to fail in.
        if h.delete_on_close && share.writable() && !h.name().is_empty() {
            let _ = share.remove(h.name());
        }
        write_response_header(
            out,
            CMD_CLOSE,
            STATUS_SUCCESS,
            r64(req, H_MESSAGE_ID),
            self.session_id,
            self.tree_id,
            r16(req, H_CREDIT).max(1),
            0,
        );
        let b = HDR_LEN;
        out[b..b + 60].fill(0);
        w16(out, b, 60); // StructureSize
        w16(out, b + 2, 1); // Flags: POSTQUERY_ATTRIB, the fields below are filled
        w64(out, b + 40, size);
        w64(out, b + 48, size);
        w32(out, b + 56, attrs);
        b + 60
    }

    fn read(
        &mut self,
        req: &[u8],
        out: &mut [u8],
        share: &impl Share,
        chain_fid: &mut Option<[u8; 16]>,
        err: impl Fn(&mut [u8], u32) -> usize,
    ) -> usize {
        let slot = match self.resolve_fid(req, 80, chain_fid) {
            Ok(s) => s,
            Err(status) => return err(out, status),
        };
        let Node::File(file) = self.handles[slot].unwrap().node else {
            return err(out, STATUS_INVALID_PARAMETER); // a directory has no bytes to read
        };
        let want = r32(req, 68).min(MAX_TRANSACT) as usize;
        let offset = r64(req, 72);

        let b = HDR_LEN;
        let data_at = b + 16;
        let n = match share.read(file, offset, &mut out[data_at..data_at + want]) {
            Ok(n) => n,
            // A refusal is now distinguishable from end of file, which is the whole reason the
            // `Share` trait grew an error channel: before this, an FS server saying no read back
            // as an empty file.
            Err(e) => return err(out, status_for(e)),
        };
        if n == 0 {
            return err(out, STATUS_END_OF_FILE);
        }
        write_response_header(
            out,
            CMD_READ,
            STATUS_SUCCESS,
            r64(req, H_MESSAGE_ID),
            self.session_id,
            self.tree_id,
            r16(req, H_CREDIT).max(1),
            0,
        );
        out[b..b + 16].fill(0);
        w16(out, b, 17); // StructureSize
        out[b + 2] = data_at as u8; // DataOffset
        w32(out, b + 4, n as u32); // DataLength
        data_at + n
    }

    /// WRITE ([MS-SMB2] §2.2.21): bytes from the request's own buffer to the share.
    ///
    /// The one place the response's `Count` matters more than it looks: a short count is a
    /// contract with the client that it must send the rest, so this reports what the backing
    /// actually took rather than what was asked for. A backing that takes nothing while the
    /// client keeps offering would spin, so a zero-length take on a nonzero request is reported
    /// as an IO error instead of as a successful nothing.
    fn write(
        &mut self,
        req: &[u8],
        out: &mut [u8],
        share: &impl Share,
        chain_fid: &mut Option<[u8; 16]>,
        err: impl Fn(&mut [u8], u32) -> usize,
    ) -> usize {
        if !share.writable() {
            return err(out, STATUS_ACCESS_DENIED);
        }
        let slot = match self.resolve_fid(req, 80, chain_fid) {
            Ok(s) => s,
            Err(status) => return err(out, status),
        };
        let Node::File(file) = self.handles[slot].unwrap().node else {
            return err(out, STATUS_INVALID_PARAMETER); // a directory takes no bytes
        };
        let data_off = r16(req, 66) as usize;
        let len = (r32(req, 68) as usize).min(MAX_TRANSACT as usize);
        let offset = r64(req, 72);
        let Some(data) = req.get(data_off..data_off + len) else {
            return err(out, STATUS_INVALID_PARAMETER);
        };

        let n = match share.write(file, offset, data) {
            Ok(n) => n,
            Err(e) => return err(out, status_for(e)),
        };
        if n == 0 && !data.is_empty() {
            return err(out, STATUS_UNEXPECTED_IO_ERROR);
        }

        write_response_header(
            out,
            CMD_WRITE,
            STATUS_SUCCESS,
            r64(req, H_MESSAGE_ID),
            self.session_id,
            self.tree_id,
            r16(req, H_CREDIT).max(1),
            0,
        );
        let b = HDR_LEN;
        out[b..b + 17].fill(0);
        w16(out, b, 17); // StructureSize
        w32(out, b + 4, n as u32); // Count
        b + 17
    }

    /// `SET_INFO` ([MS-SMB2] §2.2.39): the four file classes a client changes a file with, plus
    /// the one it changes a file's *name* with.
    ///
    /// The classes are chosen by what a real client sends on a write, not by what the spec lists:
    /// `FileEndOfFileInformation` is how a file is truncated (and how a client shortens one it
    /// just overwrote), `FileRenameInformation` is `mv`, `FileDispositionInformation` is `rm`,
    /// `FileBasicInformation` is the timestamp write every copy ends with. Everything else is
    /// `STATUS_NOT_SUPPORTED`, said out loud, because a client that gets a success for something
    /// that did not happen will believe it.
    fn set_info(
        &mut self,
        req: &[u8],
        out: &mut [u8],
        share: &impl Share,
        chain_fid: &mut Option<[u8; 16]>,
        err: impl Fn(&mut [u8], u32) -> usize,
    ) -> usize {
        if !share.writable() {
            return err(out, STATUS_ACCESS_DENIED);
        }
        let info_type = req[66];
        let class = req[67];
        let buf_len = r32(req, 68) as usize;
        let buf_off = r16(req, 72) as usize;
        let slot = match self.resolve_fid(req, 80, chain_fid) {
            Ok(s) => s,
            Err(status) => return err(out, status),
        };
        let Some(data) = req.get(buf_off..buf_off + buf_len) else {
            return err(out, STATUS_INVALID_PARAMETER);
        };
        if info_type != INFO_TYPE_FILE {
            return err(out, STATUS_NOT_SUPPORTED);
        }

        let handle = self.handles[slot].unwrap();
        let result: Result<(), u32> = match class {
            // FileBasicInformation: four FILETIMEs and the attributes. Accepted and discarded;
            // this server holds no clock capability and fs_proto's FSTAT carries no times, so
            // there is nothing to write them to (crate BUGS). Refusing would break every copy,
            // which ends with one of these.
            CLASS_BASIC => Ok(()),
            // FileAllocationInformation: a preallocation hint. A no-op on purpose (crate BUGS):
            // turning it into a truncate would zero-extend a file the client is about to fill.
            CLASS_ALLOCATION => Ok(()),
            CLASS_END_OF_FILE => match handle.node {
                Node::File(id) if data.len() >= 8 => {
                    share.truncate(id, r64(data, 0)).map_err(status_for)
                }
                Node::File(_) => Err(STATUS_INVALID_PARAMETER),
                Node::Root => Err(STATUS_INVALID_PARAMETER),
            },
            CLASS_DISPOSITION => {
                if data.is_empty() {
                    Err(STATUS_INVALID_PARAMETER)
                } else if matches!(handle.node, Node::Root) {
                    Err(STATUS_FILE_IS_A_DIRECTORY)
                } else {
                    // The name goes at CLOSE, not here. Setting it back to zero un-marks it,
                    // which is what a client that changed its mind expects.
                    self.handles[slot].as_mut().unwrap().delete_on_close = data[0] != 0;
                    Ok(())
                }
            }
            CLASS_RENAME => self.rename(share, slot, data),
            _ => Err(STATUS_NOT_SUPPORTED),
        };
        if let Err(status) = result {
            return err(out, status);
        }

        write_response_header(
            out,
            CMD_SET_INFO,
            STATUS_SUCCESS,
            r64(req, H_MESSAGE_ID),
            self.session_id,
            self.tree_id,
            r16(req, H_CREDIT).max(1),
            0,
        );
        let b = HDR_LEN;
        w16(out, b, 2); // StructureSize, and the whole body
        b + 2
    }

    /// `FileRenameInformation`'s body ([MS-FSCC] §2.4.37): `ReplaceIfExists`, seven reserved
    /// bytes, an eight-byte `RootDirectory` handle, the name's length, then the UTF-16 name.
    ///
    /// `RootDirectory` must be zero: nonzero means "relative to that open directory", and this
    /// share has exactly one directory, so honouring it would be pretending to a namespace that
    /// does not exist. The handle's own stored name is the source, which is why a handle carries
    /// one at all.
    fn rename(&mut self, share: &impl Share, slot: usize, data: &[u8]) -> Result<(), u32> {
        if data.len() < 20 {
            return Err(STATUS_INVALID_PARAMETER);
        }
        if r64(data, 8) != 0 {
            return Err(STATUS_NOT_SUPPORTED);
        }
        let n = r32(data, 16) as usize;
        let wide = data.get(20..20 + n).ok_or(STATUS_INVALID_PARAMETER)?;
        let mut buf = [0u8; 128];
        let mut to = utf16le_to_ascii_lower(wide, &mut buf).ok_or(STATUS_OBJECT_NAME_INVALID)?;
        if to.first() == Some(&b'\\') {
            to = &to[1..];
        }
        if to.contains(&b'\\') {
            return Err(STATUS_OBJECT_PATH_NOT_FOUND);
        }
        if to.is_empty() || to.len() > MAX_NAME {
            return Err(STATUS_OBJECT_NAME_INVALID);
        }
        let handle = self.handles[slot].unwrap();
        if matches!(handle.node, Node::Root) {
            return Err(STATUS_FILE_IS_A_DIRECTORY);
        }
        let mut from = [0u8; MAX_NAME];
        let from_len = handle.name_len as usize;
        from[..from_len].copy_from_slice(handle.name());
        share.rename(&from[..from_len], to).map_err(status_for)?;
        // The handle now answers to the new name, which matters because delete-on-close and
        // QUERY_INFO's name class both read it, and because a client may rename twice.
        let h = self.handles[slot].as_mut().unwrap();
        h.name = [0u8; MAX_NAME];
        h.name[..to.len()].copy_from_slice(to);
        h.name_len = to.len() as u8;
        Ok(())
    }

    fn query_directory(
        &mut self,
        req: &[u8],
        out: &mut [u8],
        share: &impl Share,
        chain_fid: &mut Option<[u8; 16]>,
        err: impl Fn(&mut [u8], u32) -> usize,
    ) -> usize {
        let class = req[66];
        let flags = req[67];
        let slot = match self.resolve_fid(req, 72, chain_fid) {
            Ok(s) => s,
            Err(status) => return err(out, status),
        };
        if !matches!(self.handles[slot].unwrap().node, Node::Root) {
            return err(out, STATUS_INVALID_PARAMETER);
        }
        // The search pattern: `*` enumerates; a literal name matches one entry. Anything else
        // non-ASCII simply matches nothing.
        let pat_off = r16(req, 88) as usize;
        let pat_len = r16(req, 90) as usize;
        let mut pat_buf = [0u8; 128];
        let pattern: &[u8] = match req.get(pat_off..pat_off + pat_len) {
            Some(w) if !w.is_empty() => {
                utf16le_to_ascii_lower(w, &mut pat_buf).unwrap_or(b"\x00" as &[u8])
            }
            _ => b"*",
        };
        let out_max = (r32(req, 92) as usize).min(MAX_TRANSACT as usize);

        // RESTART_SCANS (0x01) or INDEX_SPECIFIED (0x04) rewind; REOPEN (0x10) is both flags'
        // effect here since a handle is all the state there is.
        if flags & 0x15 != 0 {
            let h = self.handles[slot].as_mut().unwrap();
            h.enum_index = 0;
            h.enum_done = false;
        }
        if self.handles[slot].unwrap().enum_done {
            return err(out, STATUS_NO_MORE_FILES);
        }

        let b = HDR_LEN;
        let data_at = b + 8;
        let mut pos = data_at;
        let mut last_entry_at = 0usize;
        let mut wrote_any = false;
        let single = flags & 0x02 != 0; // RETURN_SINGLE_ENTRY

        let limit = (data_at + out_max).min(out.len());
        loop {
            let index = self.handles[slot].unwrap().enum_index;
            // Rows 0 and 1 are `.` and `..`; row k >= 2 is share entry k - 2.
            let (name, size, is_dir): (&[u8], u64, bool) = match index {
                0 => (b".", 0, true),
                1 => (b"..", 0, true),
                k => match share.entry(k - 2) {
                    Some(e) => (e.name, e.size, e.is_dir),
                    None => {
                        self.handles[slot].as_mut().unwrap().enum_done = true;
                        break;
                    }
                },
            };
            let matched = pattern == b"*" || pattern == name;
            if matched {
                let avail = limit.saturating_sub(pos);
                let Some(len) =
                    encode_dir_entry(class, &mut out[pos..pos + avail], name, size, is_dir)
                else {
                    // Out of room: stop here, do not consume this entry.
                    break;
                };
                last_entry_at = pos;
                pos += len;
                // NextEntryOffset is this entry's full aligned length; patched to 0 on the last.
                while !pos.is_multiple_of(8) {
                    out[pos] = 0;
                    pos += 1;
                }
                w32(out, last_entry_at, (pos - last_entry_at) as u32);
                wrote_any = true;
            }
            self.handles[slot].as_mut().unwrap().enum_index += 1;
            if wrote_any && (single || pattern != b"*") {
                break;
            }
        }

        if !wrote_any {
            self.handles[slot].as_mut().unwrap().enum_done = true;
            return err(out, STATUS_NO_MORE_FILES);
        }
        w32(out, last_entry_at, 0); // the final entry ends the list
        write_response_header(
            out,
            CMD_QUERY_DIRECTORY,
            STATUS_SUCCESS,
            r64(req, H_MESSAGE_ID),
            self.session_id,
            self.tree_id,
            r16(req, H_CREDIT).max(1),
            0,
        );
        w16(out, b, 9); // StructureSize
        w16(out, b + 2, data_at as u16);
        w32(out, b + 4, (pos - data_at) as u32);
        pos
    }

    fn query_info(
        &mut self,
        req: &[u8],
        out: &mut [u8],
        share: &impl Share,
        chain_fid: &mut Option<[u8; 16]>,
        err: impl Fn(&mut [u8], u32) -> usize,
    ) -> usize {
        let info_type = req[66];
        let class = req[67];
        let slot = match self.resolve_fid(req, 88, chain_fid) {
            Ok(s) => s,
            Err(status) => return err(out, status),
        };
        let handle = self.handles[slot].unwrap();
        let node = handle.node;
        let (size, attrs, is_dir) = match node {
            Node::Root => (0, ATTR_DIRECTORY, true),
            Node::File(i) => (share.size(i), ATTR_NORMAL, false),
        };
        let writable = share.writable();

        let b = HDR_LEN;
        let data_at = b + 8;
        let d = &mut out[data_at..];
        let len = match (info_type, class) {
            // FILE / FileBasicInformation: four zero FILETIMEs (unknown), attributes.
            (1, 4) => {
                d[..40].fill(0);
                w32(d, 32, attrs);
                40
            }
            // FILE / FileStandardInformation.
            (1, 5) => {
                d[..24].fill(0);
                w64(d, 0, size);
                w64(d, 8, size);
                w32(d, 16, 1); // NumberOfLinks
                d[21] = is_dir as u8;
                24
            }
            // FILE / FileInternalInformation: a stable index; the handle's node is one.
            (1, 6) => {
                let id = match node {
                    Node::Root => 2u64,
                    Node::File(i) => 3 + i,
                };
                w64(d, 0, id);
                8
            }
            // FILE / FileEaInformation: no extended attributes.
            (1, 7) => {
                w32(d, 0, 0);
                4
            }
            // FILE / FileAllInformation: Basic + Standard + Internal + Ea + Access + Position +
            // Mode + Alignment + Name.
            (1, 18) => {
                d[..96].fill(0);
                w32(d, 32, attrs); // Basic.attributes
                w64(d, 40, size); // Standard.AllocationSize
                w64(d, 48, size); // Standard.EndOfFile
                w32(d, 56, 1); // Standard.NumberOfLinks
                d[61] = is_dir as u8;
                d[62] = handle.delete_on_close as u8; // Standard.DeletePending
                w32(d, 76, maximal_access(writable)); // AccessInformation
                // NameInformation: `\` + the name the handle was opened under. The handle's own
                // copy, not a listing lookup: a writable share's listing moves.
                let mut name = [0u8; 1 + MAX_NAME];
                name[0] = b'\\';
                let nlen = 1 + {
                    let n = handle.name().len();
                    name[1..1 + n].copy_from_slice(handle.name());
                    n
                };
                let wide = ascii_to_utf16le(&name[..nlen], &mut d[100..]);
                w32(d, 96, wide as u32);
                100 + wide
            }
            // FILE / FileNetworkOpenInformation.
            (1, 34) => {
                d[..56].fill(0);
                w64(d, 32, size); // AllocationSize
                w64(d, 40, size); // EndOfFile
                w32(d, 48, attrs);
                56
            }
            // FILE / FileStreamInformation: files have one unnamed data stream; the root has none
            // (zero bytes of output, which is a success saying "no streams").
            (1, 22) => {
                if is_dir {
                    0
                } else {
                    d[..24].fill(0);
                    let wide = ascii_to_utf16le(b"::$DATA", &mut d[24..]);
                    w32(d, 4, wide as u32);
                    w64(d, 8, size);
                    w64(d, 16, size);
                    24 + wide
                }
            }
            // FILESYSTEM / FileFsVolumeInformation.
            (2, 1) => {
                d[..18].fill(0);
                let wide = ascii_to_utf16le(b"nife", &mut d[18..]);
                w32(d, 12, wide as u32); // LabelLength
                18 + wide
            }
            // FILESYSTEM / FileFsSizeInformation. Free space is nominal on a writable share and
            // zero on a read-only one, which is the honest pair: see `share::NOMINAL_VOLUME_BYTES`
            // for why there is no real number to report, and the crate BUGS for what a client
            // that believes it will meet instead.
            (2, 3) => {
                let (total, free) = volume_units(writable);
                w64(d, 0, total); // TotalAllocationUnits
                w64(d, 8, free); // AvailableAllocationUnits
                w32(d, 16, 1); // SectorsPerAllocationUnit
                w32(d, 20, VOLUME_UNIT as u32); // BytesPerSector
                24
            }
            // FILESYSTEM / FileFsDeviceInformation: a disk, read-only only when it is.
            (2, 4) => {
                w32(d, 0, 7); // FILE_DEVICE_DISK
                w32(d, 4, if writable { 0 } else { 0x8 }); // FILE_READ_ONLY_DEVICE
                8
            }
            // FILESYSTEM / FileFsAttributeInformation. Dropping READ_ONLY_VOLUME is what lets
            // macOS attempt a write at all: it honours the bit client-side and refuses before the
            // wire, which is exactly what made the read-only mount behave and what has to go for
            // the write path to be reachable.
            (2, 5) => {
                const CASE_PRESERVED: u32 = 0x2;
                const UNICODE_ON_DISK: u32 = 0x4;
                const READ_ONLY_VOLUME: u32 = 0x0008_0000;
                let flags =
                    CASE_PRESERVED | UNICODE_ON_DISK | if writable { 0 } else { READ_ONLY_VOLUME };
                w32(d, 0, flags);
                w32(d, 4, MAX_NAME as u32); // MaximumComponentNameLength
                let wide = ascii_to_utf16le(b"nifefs", &mut d[12..]);
                w32(d, 8, wide as u32);
                12 + wide
            }
            // FILESYSTEM / FileFsFullSizeInformation.
            (2, 7) => {
                let (total, free) = volume_units(writable);
                w64(d, 0, total);
                w64(d, 8, free); // CallerAvailableAllocationUnits
                w64(d, 16, free); // ActualAvailableAllocationUnits
                w32(d, 24, 1);
                w32(d, 28, VOLUME_UNIT as u32);
                32
            }
            // SECURITY: this server has no security descriptors to show; the status is honest.
            (3, _) => return err(out, STATUS_ACCESS_DENIED),
            _ => return err(out, STATUS_NOT_SUPPORTED),
        };

        write_response_header(
            out,
            CMD_QUERY_INFO,
            STATUS_SUCCESS,
            r64(req, H_MESSAGE_ID),
            self.session_id,
            self.tree_id,
            r16(req, H_CREDIT).max(1),
            0,
        );
        w16(out, b, 9);
        w16(out, b + 2, data_at as u16);
        w32(out, b + 4, len as u32);
        data_at + len
    }
}

/// **Resolve a name under a CREATE disposition**, returning the file's id and the `CreateAction`
/// the response must report.
///
/// Free rather than a method because it is pure protocol-to-share translation, and because
/// keeping it out of [`Connection`] makes it obvious that nothing here touches connection state:
/// a disposition either produced a file or it did not.
///
/// The read-only gate is the caller's ([`Connection::create`]); by the time this runs, a write is
/// already permitted.
fn open_with_disposition(
    share: &impl Share,
    name: &[u8],
    disposition: u32,
) -> Result<(FileId, u32), Error> {
    match disposition {
        FILE_OPEN => share.open(name).map(|id| (id, ACTION_OPENED)),
        FILE_CREATE => share.create(name).map(|id| (id, ACTION_CREATED)),
        FILE_OPEN_IF => match share.open(name) {
            Ok(id) => Ok((id, ACTION_OPENED)),
            Err(Error::NotFound) => share.create(name).map(|id| (id, ACTION_CREATED)),
            Err(e) => Err(e),
        },
        FILE_OVERWRITE => {
            let id = share.open(name)?;
            truncate_or_close(share, id, ACTION_OVERWRITTEN)
        }
        // SUPERSEDE and OVERWRITE_IF differ only in the action they report. Superseding properly
        // means replacing the file's identity (attributes and all), which this model has none of;
        // truncating to zero is the whole of the difference here, and the crate BUGS say so.
        FILE_OVERWRITE_IF | FILE_SUPERSEDE => {
            let action = if disposition == FILE_SUPERSEDE {
                ACTION_SUPERSEDED
            } else {
                ACTION_OVERWRITTEN
            };
            match share.open(name) {
                Ok(id) => truncate_or_close(share, id, action),
                Err(Error::NotFound) => share.create(name).map(|id| (id, ACTION_CREATED)),
                Err(e) => Err(e),
            }
        }
        _ => Err(Error::NotFound),
    }
}

/// Truncate a freshly opened file to zero, closing it if that fails: a CREATE that answers an
/// error must not leave the backing holding an id nobody will ever close.
fn truncate_or_close(share: &impl Share, id: FileId, action: u32) -> Result<(FileId, u32), Error> {
    match share.truncate(id, 0) {
        Ok(()) => Ok((id, action)),
        Err(e) => {
            share.close(id);
            Err(e)
        }
    }
}

/// The access mask this share grants, which `TREE_CONNECT` and `FileAllInformation` both report.
/// Read side: `READ_DATA | READ_EA | READ_ATTRIBUTES | EXECUTE | READ_CONTROL | SYNCHRONIZE`.
/// Write side adds `WRITE_DATA | APPEND_DATA | WRITE_EA | WRITE_ATTRIBUTES | DELETE`, which is
/// every verb the write path actually implements and nothing beyond it.
const fn maximal_access(writable: bool) -> u32 {
    const READ_SIDE: u32 = 0x0012_00A9;
    const WRITE_SIDE: u32 = 0x0001_0116;
    if writable {
        READ_SIDE | WRITE_SIDE
    } else {
        READ_SIDE
    }
}

/// The allocation unit the volume information classes report in. 4 KiB, matching the filesystem's
/// block size, so a client's arithmetic lands on real boundaries.
const VOLUME_UNIT: u64 = 4096;

/// `(total, free)` allocation units. Read the doc on [`crate::share::NOMINAL_VOLUME_BYTES`] before
/// believing either number: nothing in this stack can ask the filesystem how big it is.
const fn volume_units(writable: bool) -> (u64, u64) {
    let total = crate::share::NOMINAL_VOLUME_BYTES / VOLUME_UNIT;
    (total, if writable { total } else { 0 })
}

/// Is this an SMB1 multi-protocol NEGOTIATE (`\xFFSMB`, command `0x72`)? The only SMB1 shape
/// this crate recognises at all.
fn is_smb1_negotiate(msg: &[u8]) -> bool {
    msg.len() >= 37 && msg[..4] == [0xFF, b'S', b'M', b'B'] && msg[4] == 0x72
}

/// Do the SMB1 negotiate's dialect strings claim SMB2? A byte scan for the two literals rather
/// than a parse of the SMB1 dialect list, deliberately: this server refuses to speak SMB1, so
/// growing a parser for its wire format to answer one yes/no question would be surface without a
/// user. The strings cannot appear in an SMB1 negotiate except as dialect names.
fn smb1_offers_smb2(msg: &[u8]) -> bool {
    msg.windows(8).any(|w| w == b"SMB 2.??" || w == b"SMB 2.00")
}

/// The 4-byte "structure size 4" success body ECHO, FLUSH, LOGOFF and `TREE_DISCONNECT` share.
fn simple_ok(out: &mut [u8], cmd: u16, msg_id: u64, sid: u64, tid: u32, credits: u16) -> usize {
    write_response_header(out, cmd, STATUS_SUCCESS, msg_id, sid, tid, credits, 0);
    let b = HDR_LEN;
    w16(out, b, 4);
    w16(out, b + 2, 0);
    b + 4
}

/// Encode one directory entry of `class` at `out[0..]`, returning its unaligned length, or `None`
/// if it does not fit. The offsets are [MS-FSCC] §2.4's; the classes share a 64-byte prefix and
/// differ in what sits between it and the name.
fn encode_dir_entry(
    class: u8,
    out: &mut [u8],
    name: &[u8],
    size: u64,
    is_dir: bool,
) -> Option<usize> {
    let wide_len = name.len() * 2;
    // (fixed part before the name, whether the prefix is the full 64-byte one)
    let (fixed, full_prefix) = match class {
        1 => (64, true),   // FileDirectoryInformation
        2 => (68, true),   // FileFullDirectoryInformation
        3 => (94, true),   // FileBothDirectoryInformation
        37 => (104, true), // FileIdBothDirectoryInformation
        38 => (80, true),  // FileIdFullDirectoryInformation
        12 => (12, false), // FileNamesInformation
        _ => return None,
    };
    if fixed + wide_len > out.len() {
        return None;
    }
    out[..fixed].fill(0);
    if full_prefix {
        w64(out, 40, size); // EndOfFile
        w64(out, 48, size); // AllocationSize
        w32(out, 56, if is_dir { ATTR_DIRECTORY } else { ATTR_NORMAL });
        w32(out, 60, wide_len as u32); // FileNameLength
    } else {
        w32(out, 8, wide_len as u32);
    }
    ascii_to_utf16le(name, &mut out[fixed..]);
    Some(fixed + wide_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::share::{FIXTURE, MemoryShare};
    use crate::{H_NEXT_COMMAND, H_STATUS, MAX_MESSAGE, client, r32};

    fn conn() -> Connection {
        Connection::new([0xA5; 8])
    }

    /// Drive one request through the machine and return the response bytes.
    fn rt(c: &mut Connection, req: &[u8]) -> Vec<u8> {
        rt_on(c, req, &FIXTURE)
    }

    /// The same against an arbitrary share, which the write path's tests need (`FIXTURE` is
    /// read-only by construction, which is the point of it).
    fn rt_on(c: &mut Connection, req: &[u8], share: &impl Share) -> Vec<u8> {
        let mut out = vec![0u8; MAX_MESSAGE];
        let n = c.handle(req, &mut out, share).expect("smb2 in, smb2 out");
        out.truncate(n);
        out
    }

    fn status(resp: &[u8]) -> u32 {
        r32(resp, H_STATUS)
    }

    /// Negotiate, set up the guest session, and connect the share against `share`. `establish`
    /// below is this against the fixture.
    fn establish_on(c: &mut Connection, share: &impl Share) -> (u64, u32) {
        let resp = rt_on(c, &client::negotiate(1), share);
        assert_eq!(status(&resp), STATUS_SUCCESS);
        let resp = rt_on(c, &client::session_setup_negotiate(2), share);
        let sid = r64(&resp, H_SESSION_ID);
        let resp = rt_on(c, &client::session_setup_authenticate(3, sid), share);
        assert_eq!(status(&resp), STATUS_SUCCESS);
        let resp = rt_on(
            c,
            &client::tree_connect(4, sid, b"\\\\10.0.2.15\\share"),
            share,
        );
        assert_eq!(status(&resp), STATUS_SUCCESS);
        (sid, r32(&resp, H_TREE_ID))
    }

    /// Negotiate, set up the guest session, and connect the share: the preamble every later test
    /// uses, asserted once in full here.
    fn establish(c: &mut Connection) -> (u64, u32) {
        let resp = rt(c, &client::negotiate(1));
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert_eq!(client::negotiate_dialect(&resp), DIALECT_0210);
        assert_eq!(client::negotiate_max_read(&resp), MAX_TRANSACT);

        let resp = rt(c, &client::session_setup_negotiate(2));
        assert_eq!(status(&resp), STATUS_MORE_PROCESSING_REQUIRED);
        let sid = r64(&resp, H_SESSION_ID);
        assert_ne!(sid, 0);
        // The security buffer holds an NTLMSSP CHALLENGE carrying our connection's challenge.
        let token = client::session_setup_token(&resp).expect("a challenge token");
        assert_eq!(&token[..8], &ntlmssp::SIGNATURE);
        assert_eq!(&token[24..32], &[0xA5; 8]);

        let resp = rt(c, &client::session_setup_authenticate(3, sid));
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert_eq!(
            r16(&resp, HDR_LEN + 2) & SESSION_FLAG_IS_GUEST,
            SESSION_FLAG_IS_GUEST,
            "a session nothing verified must say guest"
        );

        let resp = rt(c, &client::tree_connect(4, sid, b"\\\\10.0.2.15\\share"));
        assert_eq!(status(&resp), STATUS_SUCCESS);
        let tid = r32(&resp, H_TREE_ID);
        assert_ne!(tid, 0);
        (sid, tid)
    }

    #[test]
    fn a_full_session_reads_a_file_end_to_end() {
        let mut c = conn();
        let (sid, tid) = establish(&mut c);

        let resp = rt(&mut c, &client::create(5, sid, tid, b"hello.txt"));
        assert_eq!(status(&resp), STATUS_SUCCESS);
        let fid = client::create_file_id(&resp);
        assert_eq!(client::create_end_of_file(&resp), 16);

        let resp = rt(&mut c, &client::read(6, sid, tid, &fid, 0, 64));
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert_eq!(client::read_data(&resp), b"nife serves SMB\n");

        // A read at EOF is END_OF_FILE, not an empty success: that distinction is what ends a
        // client's read loop.
        let resp = rt(&mut c, &client::read(7, sid, tid, &fid, 16, 64));
        assert_eq!(status(&resp), STATUS_END_OF_FILE);

        let resp = rt(&mut c, &client::close(8, sid, tid, &fid));
        assert_eq!(status(&resp), STATUS_SUCCESS);
        // And the handle is gone: the same fid now misses.
        let resp = rt(&mut c, &client::read(9, sid, tid, &fid, 0, 64));
        assert_eq!(status(&resp), STATUS_FILE_CLOSED);
    }

    #[test]
    fn reads_are_offset_correct_across_chunks() {
        let mut c = conn();
        let (sid, tid) = establish(&mut c);
        let resp = rt(&mut c, &client::create(5, sid, tid, b"readme.md"));
        let fid = client::create_file_id(&resp);
        let total = client::create_end_of_file(&resp);
        let mut got = Vec::new();
        let mut off = 0u64;
        let mut msg = 6u64;
        while off < total {
            let resp = rt(&mut c, &client::read(msg, sid, tid, &fid, off, 7));
            assert_eq!(status(&resp), STATUS_SUCCESS);
            let data = client::read_data(&resp);
            assert!(!data.is_empty() && data.len() <= 7);
            got.extend_from_slice(data);
            off += data.len() as u64;
            msg += 1;
        }
        assert_eq!(got, FIXTURE.files[1].1);
    }

    #[test]
    fn the_listing_walks_dot_dotdot_then_every_file_and_then_ends() {
        let mut c = conn();
        let (sid, tid) = establish(&mut c);
        let resp = rt(&mut c, &client::create(5, sid, tid, b""));
        assert_eq!(status(&resp), STATUS_SUCCESS);
        let fid = client::create_file_id(&resp);

        // Class 37 (FileIdBothDirectoryInformation) is what macOS asks for.
        let resp = rt(
            &mut c,
            &client::query_directory(6, sid, tid, &fid, 37, b"*"),
        );
        assert_eq!(status(&resp), STATUS_SUCCESS);
        let names: Vec<Vec<u8>> = client::dir_entries(&resp, 37)
            .map(|n| n.as_bytes().to_vec())
            .collect();
        assert_eq!(
            names,
            vec![
                b".".to_vec(),
                b"..".to_vec(),
                b"hello.txt".to_vec(),
                b"readme.md".to_vec()
            ]
        );

        // Exhausted: the protocol's end-of-listing status, and it stays ended.
        let resp = rt(
            &mut c,
            &client::query_directory(7, sid, tid, &fid, 37, b"*"),
        );
        assert_eq!(status(&resp), STATUS_NO_MORE_FILES);
        // A restart scan rewinds.
        let resp = rt(
            &mut c,
            &client::query_directory_flags(8, sid, tid, &fid, 37, b"*", 0x01),
        );
        assert_eq!(status(&resp), STATUS_SUCCESS);
    }

    #[test]
    fn a_pattern_that_names_one_file_returns_exactly_it() {
        let mut c = conn();
        let (sid, tid) = establish(&mut c);
        let resp = rt(&mut c, &client::create(5, sid, tid, b""));
        let fid = client::create_file_id(&resp);
        let resp = rt(
            &mut c,
            &client::query_directory(6, sid, tid, &fid, 3, b"hello.txt"),
        );
        assert_eq!(status(&resp), STATUS_SUCCESS);
        let names: Vec<Vec<u8>> = client::dir_entries(&resp, 3)
            .map(|n| n.as_bytes().to_vec())
            .collect();
        assert_eq!(names, vec![b"hello.txt".to_vec()]);
    }

    #[test]
    fn the_compound_macos_uses_to_stat_a_file_works_with_related_fids() {
        let mut c = conn();
        let (sid, tid) = establish(&mut c);
        // CREATE + QUERY_INFO(all) + CLOSE, the second and third naming the related fid.
        let msg = client::compound_create_query_close(5, sid, tid, b"hello.txt");
        let mut out = vec![0u8; MAX_MESSAGE];
        let n = c.handle(&msg, &mut out, &FIXTURE).unwrap();
        let resp = &out[..n];
        // Three responses, chained by NextCommand.
        let r1 = resp;
        assert_eq!(status(r1), STATUS_SUCCESS);
        let n1 = r32(r1, H_NEXT_COMMAND) as usize;
        assert!(n1 > 0 && n1.is_multiple_of(8));
        let r2 = &resp[n1..];
        assert_eq!(status(r2), STATUS_SUCCESS, "QUERY_INFO on the related fid");
        let n2 = r32(r2, H_NEXT_COMMAND) as usize;
        let r3 = &r2[n2..];
        assert_eq!(status(r3), STATUS_SUCCESS, "CLOSE on the related fid");
        assert_eq!(r32(r3, H_NEXT_COMMAND), 0);
        // The QUERY_INFO's FileAllInformation carries the size where the spec puts it.
        let data_off = r16(r2, HDR_LEN + 2) as usize;
        assert_eq!(r64(&r2[data_off..], 48), 16, "Standard.EndOfFile");
        // And nothing leaked: the handle table is empty again.
        assert!(c.handles.iter().all(Option::is_none));
    }

    /// **Every mutating command on a read-only share, and the status each gets.** This is the
    /// milestone's read-only claim as a test: the refusals happen at the protocol layer, so they
    /// hold for a backing that would have said yes, and `FIXTURE`'s trait defaults are never
    /// reached. Each disposition that writes is listed separately, because a client picks one and
    /// a hole in any of them is a writable share nobody declared.
    #[test]
    fn what_the_readonly_share_refuses_and_with_which_status() {
        let mut c = conn();
        let (sid, tid) = establish(&mut c);
        // Every disposition that would create or truncate, one message id each.
        for (i, disposition) in [
            0u32, // FILE_SUPERSEDE
            2,    // FILE_CREATE
            4,    // FILE_OVERWRITE
            5,    // FILE_OVERWRITE_IF
        ]
        .iter()
        .enumerate()
        {
            let resp = rt(
                &mut c,
                &client::create_disposition(20 + i as u64, sid, tid, b"new.txt", *disposition),
            );
            assert_eq!(
                status(&resp),
                STATUS_ACCESS_DENIED,
                "disposition {disposition} on a read-only share"
            );
        }
        // FILE_OPEN_IF is demoted to FILE_OPEN rather than refused, so an existing file opens and
        // a missing one is simply absent. A client that opens everything that way still works.
        let resp = rt(
            &mut c,
            &client::create_disposition(30, sid, tid, b"hello.txt", 3),
        );
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert_eq!(client::create_action(&resp), ACTION_OPENED);
        let resp = rt(
            &mut c,
            &client::create_disposition(31, sid, tid, b"nope.txt", 3),
        );
        assert_eq!(status(&resp), STATUS_OBJECT_NAME_NOT_FOUND);
        // Delete-on-close is a write however it is spelled.
        let resp = rt(
            &mut c,
            &client::create_full(32, sid, tid, b"hello.txt", 1, OPT_DELETE_ON_CLOSE),
        );
        assert_eq!(status(&resp), STATUS_ACCESS_DENIED);

        // Writing to, truncating, renaming and deleting an open file.
        let resp = rt(&mut c, &client::create(6, sid, tid, b"hello.txt"));
        let fid = client::create_file_id(&resp);
        let resp = rt(&mut c, &client::write(7, sid, tid, &fid, b"x"));
        assert_eq!(status(&resp), STATUS_ACCESS_DENIED);
        let resp = rt(&mut c, &client::set_end_of_file(8, sid, tid, &fid, 0));
        assert_eq!(status(&resp), STATUS_ACCESS_DENIED);
        let resp = rt(&mut c, &client::set_rename(9, sid, tid, &fid, b"other.txt"));
        assert_eq!(status(&resp), STATUS_ACCESS_DENIED);
        let resp = rt(&mut c, &client::set_disposition(10, sid, tid, &fid, true));
        assert_eq!(status(&resp), STATUS_ACCESS_DENIED);
        // Even the harmless-looking one: a timestamp write is still a write.
        let resp = rt(&mut c, &client::set_basic(11, sid, tid, &fid, 0x80));
        assert_eq!(status(&resp), STATUS_ACCESS_DENIED);
        // And the file is untouched, which the fixture makes checkable.
        let resp = rt(&mut c, &client::read(12, sid, tid, &fid, 0, 64));
        assert_eq!(client::read_data(&resp), b"nife serves SMB\n");

        // A name that is not there, and a path below a flat root.
        let resp = rt(&mut c, &client::create(13, sid, tid, b"absent.txt"));
        assert_eq!(status(&resp), STATUS_OBJECT_NAME_NOT_FOUND);
        let resp = rt(&mut c, &client::create(14, sid, tid, b"a\\b.txt"));
        assert_eq!(status(&resp), STATUS_OBJECT_PATH_NOT_FOUND);
    }

    /// **A read-only share says so in every field a client reads before it tries.** macOS refuses
    /// a write client-side on the strength of `READ_ONLY_VOLUME`, which is why the read-only mount
    /// behaved and why this pair has to flip together with the refusals above: a share that
    /// refuses writes while advertising a writable volume produces a client that tries and fails,
    /// and one that advertises read-only while accepting writes produces a client that never
    /// tries.
    #[test]
    fn the_advertised_volume_matches_what_the_share_will_do() {
        fn check(share: &impl Share) {
            const READ_ONLY_VOLUME: u32 = 0x0008_0000;
            let want_writable = share.writable();
            let mut c = conn();
            let (sid, tid) = establish_on(&mut c, share);
            // TREE_CONNECT's MaximalAccess, which is the first thing a client sees.
            let resp = rt_on(
                &mut c,
                &client::tree_connect(9, sid, b"\\\\h\\share"),
                share,
            );
            const FILE_WRITE_DATA: u32 = 0x2;
            assert_eq!(
                r32(&resp, HDR_LEN + 12) & FILE_WRITE_DATA != 0,
                want_writable,
                "MaximalAccess must offer write exactly when the share is writable"
            );
            let resp = rt_on(&mut c, &client::create(5, sid, tid, b""), share);
            let fid = client::create_file_id(&resp);
            // FileFsAttributeInformation.
            let resp = rt_on(&mut c, &client::query_info(6, sid, tid, &fid, 2, 5), share);
            let d = r16(&resp, HDR_LEN + 2) as usize;
            assert_eq!(
                r32(&resp, d) & READ_ONLY_VOLUME != 0,
                !want_writable,
                "READ_ONLY_VOLUME must be the negation of writable()"
            );
            // FileFsSizeInformation: a volume with no free space is one macOS will not write to.
            let resp = rt_on(&mut c, &client::query_info(7, sid, tid, &fid, 2, 3), share);
            let d = r16(&resp, HDR_LEN + 2) as usize;
            assert_eq!(
                r64(&resp, d + 8) > 0,
                want_writable,
                "free space must be nonzero exactly when the share is writable"
            );
        }
        check(&FIXTURE);
        check(&MemoryShare::new(&[(b"a.txt" as &[u8], b"x" as &[u8])]));
    }

    /// **The write path end to end, through the wire.** Create a file that was not there, write
    /// to it, read it back over SMB, and check the share itself holds the bytes. The last of those
    /// is what separates this from a client believing its own write, and it is the same discipline
    /// the QEMU gate applies one level out (there the independent reader is a different process
    /// going through the FS server).
    #[test]
    fn a_file_is_created_written_and_reads_back_from_the_share_itself() {
        let share = MemoryShare::new(&[]);
        let mut c = conn();
        let (sid, tid) = establish_on(&mut c, &share);

        // FILE_CREATE on a name nothing holds.
        let resp = rt_on(
            &mut c,
            &client::create_disposition(5, sid, tid, b"note.txt", 2),
            &share,
        );
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert_eq!(client::create_action(&resp), ACTION_CREATED);
        assert_eq!(client::create_end_of_file(&resp), 0, "a new file is empty");
        let fid = client::create_file_id(&resp);

        let body = b"written over SMB2\n";
        let resp = rt_on(&mut c, &client::write(6, sid, tid, &fid, body), &share);
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert_eq!(client::write_count(&resp), body.len() as u32);

        // Back over the wire...
        let resp = rt_on(&mut c, &client::read(7, sid, tid, &fid, 0, 64), &share);
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert_eq!(client::read_data(&resp), body);
        // ...and out of the share, which the wire cannot fake.
        assert_eq!(share.contents(b"note.txt").as_deref(), Some(&body[..]));

        // A second FILE_CREATE of the same name is a collision, not a silent overwrite.
        let resp = rt_on(
            &mut c,
            &client::create_disposition(8, sid, tid, b"note.txt", 2),
            &share,
        );
        assert_eq!(status(&resp), STATUS_OBJECT_NAME_COLLISION);

        let resp = rt_on(&mut c, &client::close(9, sid, tid, &fid), &share);
        assert_eq!(status(&resp), STATUS_SUCCESS);
    }

    /// **Every disposition, on a name that is there and a name that is not**, with the
    /// `CreateAction` each reports. The table is the test: a client picks its disposition from
    /// what it means to do, and reading back the wrong action is how create-or-open logic breaks.
    #[test]
    fn the_dispositions_create_open_and_truncate_and_say_which() {
        // (disposition, exists, expected status, expected action, expected size after)
        let cases: &[(u32, bool, u32, u32, u64)] = &[
            (0, true, STATUS_SUCCESS, ACTION_SUPERSEDED, 0), // SUPERSEDE truncates
            (0, false, STATUS_SUCCESS, ACTION_CREATED, 0),
            (1, true, STATUS_SUCCESS, ACTION_OPENED, 3), // OPEN keeps
            (1, false, STATUS_OBJECT_NAME_NOT_FOUND, 0, 0),
            (2, true, STATUS_OBJECT_NAME_COLLISION, 0, 0), // CREATE is create
            (2, false, STATUS_SUCCESS, ACTION_CREATED, 0),
            (3, true, STATUS_SUCCESS, ACTION_OPENED, 3), // OPEN_IF keeps
            (3, false, STATUS_SUCCESS, ACTION_CREATED, 0),
            (4, true, STATUS_SUCCESS, ACTION_OVERWRITTEN, 0), // OVERWRITE truncates
            (4, false, STATUS_OBJECT_NAME_NOT_FOUND, 0, 0),
            (5, true, STATUS_SUCCESS, ACTION_OVERWRITTEN, 0),
            (5, false, STATUS_SUCCESS, ACTION_CREATED, 0),
        ];
        for &(disposition, exists, want_status, want_action, want_size) in cases {
            let share = if exists {
                MemoryShare::new(&[(b"f.txt" as &[u8], b"abc" as &[u8])])
            } else {
                MemoryShare::new(&[])
            };
            let mut c = conn();
            let (sid, tid) = establish_on(&mut c, &share);
            let resp = rt_on(
                &mut c,
                &client::create_disposition(5, sid, tid, b"f.txt", disposition),
                &share,
            );
            assert_eq!(
                status(&resp),
                want_status,
                "disposition {disposition}, exists {exists}"
            );
            if want_status == STATUS_SUCCESS {
                assert_eq!(
                    client::create_action(&resp),
                    want_action,
                    "disposition {disposition}, exists {exists}: CreateAction"
                );
                assert_eq!(
                    client::create_end_of_file(&resp),
                    want_size,
                    "disposition {disposition}, exists {exists}: size after"
                );
                assert_eq!(
                    share.contents(b"f.txt").map(|b| b.len() as u64),
                    Some(want_size),
                    "disposition {disposition}, exists {exists}: the share agrees"
                );
            }
        }
    }

    /// **`SET_INFO`'s classes**: end-of-file shrinks and grows, rename moves the name (and the
    /// handle follows it), disposition deletes at CLOSE and not before, and basic information is
    /// accepted while changing nothing, which the crate BUGS name as a limitation rather than
    /// hide.
    #[test]
    fn set_info_truncates_renames_and_deletes() {
        let share = MemoryShare::new(&[(b"f.txt" as &[u8], b"abcdefgh" as &[u8])]);
        let mut c = conn();
        let (sid, tid) = establish_on(&mut c, &share);
        let resp = rt_on(&mut c, &client::create(5, sid, tid, b"f.txt"), &share);
        let fid = client::create_file_id(&resp);

        // Shrink, then grow: `ftruncate` in both directions, which is what makes "replace these
        // contents" mean what a client thinks.
        let resp = rt_on(
            &mut c,
            &client::set_end_of_file(6, sid, tid, &fid, 3),
            &share,
        );
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert_eq!(share.contents(b"f.txt").as_deref(), Some(&b"abc"[..]));
        let resp = rt_on(
            &mut c,
            &client::set_end_of_file(7, sid, tid, &fid, 5),
            &share,
        );
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert_eq!(share.contents(b"f.txt").as_deref(), Some(&b"abc\0\0"[..]));

        // Timestamps: accepted, discarded, and the file is untouched.
        let resp = rt_on(&mut c, &client::set_basic(8, sid, tid, &fid, 0x80), &share);
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert_eq!(share.contents(b"f.txt").as_deref(), Some(&b"abc\0\0"[..]));

        // Rename, and the handle answers to the new name afterwards.
        let resp = rt_on(
            &mut c,
            &client::set_rename(9, sid, tid, &fid, b"g.txt"),
            &share,
        );
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert!(share.contents(b"f.txt").is_none(), "the old name is gone");
        assert_eq!(share.contents(b"g.txt").as_deref(), Some(&b"abc\0\0"[..]));
        let resp = rt_on(&mut c, &client::query_info_all(10, sid, tid, &fid), &share);
        let d = r16(&resp, HDR_LEN + 2) as usize;
        let nlen = r32(&resp[d..], 96) as usize;
        let mut name = [0u8; 32];
        let name =
            crate::utf16le_to_ascii_lower(&resp[d + 100..d + 100 + nlen], &mut name).unwrap();
        assert_eq!(
            name, b"\\g.txt",
            "the handle carries the name it was renamed to"
        );

        // Delete on close: marked here, and it happens at CLOSE, not now.
        let resp = rt_on(
            &mut c,
            &client::set_disposition(11, sid, tid, &fid, true),
            &share,
        );
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert!(
            share.contents(b"g.txt").is_some(),
            "a marked file is still there until the handle closes"
        );
        let resp = rt_on(&mut c, &client::close(12, sid, tid, &fid), &share);
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert!(share.contents(b"g.txt").is_none(), "and gone after it");

        // A class this server does not act on is refused rather than silently succeeding.
        let resp = rt_on(&mut c, &client::create(13, sid, tid, b"h.txt"), &share);
        let _ = status(&resp);
        let resp = rt_on(
            &mut c,
            &client::create_disposition(14, sid, tid, b"h.txt", 2),
            &share,
        );
        let fid = client::create_file_id(&resp);
        let resp = rt_on(
            &mut c,
            &client::set_info(15, sid, tid, &fid, 41, &[0u8; 8]),
            &share,
        );
        assert_eq!(status(&resp), STATUS_NOT_SUPPORTED);
    }

    /// **`FILE_DELETE_ON_CLOSE` at create time**, which is how macOS's `unlink` reaches a share:
    /// open, mark on the way in, close.
    #[test]
    fn delete_on_close_at_create_removes_the_name() {
        let share = MemoryShare::new(&[(b"doomed.txt" as &[u8], b"x" as &[u8])]);
        let mut c = conn();
        let (sid, tid) = establish_on(&mut c, &share);
        let resp = rt_on(
            &mut c,
            &client::create_full(5, sid, tid, b"doomed.txt", 1, OPT_DELETE_ON_CLOSE),
            &share,
        );
        assert_eq!(status(&resp), STATUS_SUCCESS);
        let fid = client::create_file_id(&resp);
        assert!(share.contents(b"doomed.txt").is_some());
        rt_on(&mut c, &client::close(6, sid, tid, &fid), &share);
        assert!(share.contents(b"doomed.txt").is_none());
    }

    /// **A handle opened before a create still reads its own file.** The regression this test
    /// exists for is the reason `Node::File` stopped being a listing index: with indices, creating
    /// a file that sorts earlier moves every later file's index, and an open handle silently
    /// starts reading its neighbour. The bug would be invisible on a read-only share, which is
    /// why nothing caught it until writes existed.
    #[test]
    fn an_open_handle_survives_a_create_that_reorders_the_share() {
        let share = MemoryShare::new(&[(b"zebra.txt" as &[u8], b"stripes" as &[u8])]);
        let mut c = conn();
        let (sid, tid) = establish_on(&mut c, &share);
        let resp = rt_on(&mut c, &client::create(5, sid, tid, b"zebra.txt"), &share);
        let fid = client::create_file_id(&resp);

        // A name that would sort (and, in the fs-backed share, list) ahead of it.
        let resp = rt_on(
            &mut c,
            &client::create_disposition(6, sid, tid, b"aardvark.txt", 2),
            &share,
        );
        assert_eq!(status(&resp), STATUS_SUCCESS);

        let resp = rt_on(&mut c, &client::read(7, sid, tid, &fid, 0, 64), &share);
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert_eq!(client::read_data(&resp), b"stripes");
    }

    #[test]
    fn the_state_gates_hold_without_their_preconditions() {
        // A command before any session: refused as a session problem, not served.
        let mut c = conn();
        let resp = rt(&mut c, &client::create(1, 7, 7, b"hello.txt"));
        assert_eq!(status(&resp), STATUS_USER_SESSION_DELETED);
        // Session setup before negotiate.
        let mut c = conn();
        let resp = rt(&mut c, &client::session_setup_negotiate(1));
        assert_eq!(status(&resp), STATUS_INVALID_PARAMETER);
        // A wrong share name.
        let mut c = conn();
        let resp = rt(&mut c, &client::negotiate(1));
        assert_eq!(status(&resp), STATUS_SUCCESS);
        let resp = rt(&mut c, &client::session_setup_negotiate(2));
        let sid = r64(&resp, H_SESSION_ID);
        let _ = rt(&mut c, &client::session_setup_authenticate(3, sid));
        let resp = rt(&mut c, &client::tree_connect(4, sid, b"\\\\host\\wrong"));
        assert_eq!(status(&resp), STATUS_BAD_NETWORK_NAME);
    }

    #[test]
    fn a_negotiate_without_our_dialect_is_refused() {
        let mut c = conn();
        let mut req = client::negotiate(1);
        // Rewrite the offer to SMB 3.1.1 only: one dialect, and not ours.
        w16(&mut req, 66, 1);
        w16(&mut req, 100, 0x0311);
        let resp = rt(&mut c, &req);
        assert_eq!(status(&resp), STATUS_NOT_SUPPORTED);
    }

    /// **The exact first message macOS sends**, captured byte for byte from `mount_smbfs` on
    /// macOS 26 through a logging proxy on 2026-08-15: an SMB1 multi-protocol NEGOTIATE offering
    /// `NT LM 0.12`, `SMB 2.002` and `SMB 2.???`. The first cut of this server dropped it as
    /// not-SMB2 and every real mount timed out; the machine overruled the assumption that modern
    /// clients open with SMB2. The answer is [MS-SMB2] §3.3.5.3.1's wildcard, and the client then
    /// negotiates properly.
    #[test]
    fn the_smb1_probe_macos_opens_with_gets_the_wildcard_answer_and_smb2_proceeds() {
        const MACOS_HEX: &str = "ff534d4272000000000801c8000000000000000000000000\
                                 ffff0100ffff0000002200024e54204c4d20302e31320002\
                                 534d4220322e3030320002534d4220322e3f3f3f00";
        let bytes: Vec<u8> = (0..MACOS_HEX.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&MACOS_HEX[i..i + 2], 16).unwrap())
            .collect();

        let mut c = conn();
        let resp = rt(&mut c, &bytes);
        assert!(is_smb2(&resp), "the answer to the SMB1 probe is SMB2");
        assert_eq!(status(&resp), STATUS_SUCCESS);
        assert_eq!(
            r64(&resp, H_MESSAGE_ID),
            0,
            "the wildcard answer carries message id 0"
        );
        assert_eq!(client::negotiate_dialect(&resp), DIALECT_WILDCARD);

        // And the connection then proceeds exactly as an SMB2-first one does.
        let (sid, tid) = establish(&mut c);
        let resp = rt(&mut c, &client::create(9, sid, tid, b"hello.txt"));
        assert_eq!(status(&resp), STATUS_SUCCESS);

        // An SMB1-only client (no SMB2 dialect strings) is dropped, not answered.
        let mut only_smb1 = bytes.clone();
        let cut = only_smb1
            .windows(2)
            .position(|w| w == [0x02, b'S'])
            .unwrap();
        only_smb1.truncate(cut);
        // Keep the SMB1 header valid; the byte-count field no longer matters to our scan.
        let mut fresh = conn();
        let mut out = vec![0u8; MAX_MESSAGE];
        assert_eq!(fresh.handle(&only_smb1, &mut out, &FIXTURE), None);
    }

    #[test]
    fn not_smb2_drops_the_connection_rather_than_answering() {
        let mut c = conn();
        let mut out = vec![0u8; MAX_MESSAGE];
        assert_eq!(c.handle(b"GET / HTTP/1.1\r\n", &mut out, &FIXTURE), None);
        let mut smb1 = vec![0u8; 64];
        smb1[..4].copy_from_slice(&[0xFF, b'S', b'M', b'B']);
        assert_eq!(c.handle(&smb1, &mut out, &FIXTURE), None);
    }

    #[test]
    fn echo_works_before_any_session_and_flush_after() {
        let mut c = conn();
        let resp = rt(&mut c, &client::echo(1));
        assert_eq!(status(&resp), STATUS_SUCCESS);
        let (sid, tid) = establish(&mut c);
        let resp = rt(&mut c, &client::create(9, sid, tid, b"hello.txt"));
        let fid = client::create_file_id(&resp);
        let resp = rt(&mut c, &client::flush(10, sid, tid, &fid));
        assert_eq!(status(&resp), STATUS_SUCCESS);
    }

    #[test]
    fn every_handle_slot_can_fill_and_the_next_open_says_so() {
        let mut c = conn();
        let (sid, tid) = establish(&mut c);
        for i in 0..MAX_HANDLES as u64 {
            let resp = rt(&mut c, &client::create(10 + i, sid, tid, b"hello.txt"));
            assert_eq!(status(&resp), STATUS_SUCCESS, "open {i}");
        }
        let resp = rt(&mut c, &client::create(99, sid, tid, b"hello.txt"));
        assert_eq!(status(&resp), STATUS_INSUFFICIENT_RESOURCES);
    }
}
