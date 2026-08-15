//! **The per-connection SMB2 state machine**: one inbound message (possibly a compound chain) in,
//! one response message out, against a [`Share`].
//!
//! Pure logic over byte slices, no allocation, no IO, so the whole protocol runs under host tests
//! (see the bottom of this file for a full client session driven through it) and stays reachable
//! by the tree's verification method. The `smb_server` role feeds it bytes from the socket
//! contract and writes back what it returns; nothing else is between this machine and the wire.
//!
//! What it implements, and the shape of what it refuses, are the crate header's scope: SMB 2.1,
//! guest sessions, a flat read-only share, compounds with related operations (macOS opens files
//! as CREATE + `QUERY_INFO` + CLOSE chains, so compounds are not optional). Everything refused is
//! refused with a real NT status, because a client's retry logic keys on them.

use crate::share::{Node, Share};
use crate::{ntlmssp, spnego};
use crate::{
    ascii_to_utf16le, is_smb2, r16, r32, r64, utf16le_to_ascii_lower, w16, w32, w64,
    write_response_header,
};
use crate::{
    CMD_CHANGE_NOTIFY, CMD_CLOSE, CMD_CREATE, CMD_ECHO, CMD_FLUSH, CMD_IOCTL, CMD_LOCK, CMD_LOGOFF,
    CMD_NEGOTIATE, CMD_QUERY_DIRECTORY, CMD_QUERY_INFO, CMD_READ, CMD_SESSION_SETUP, CMD_SET_INFO,
    CMD_TREE_CONNECT, CMD_TREE_DISCONNECT, CMD_WRITE,
};
use crate::{
    DIALECT_0210, H_COMMAND, H_CREDIT, H_MESSAGE_ID, H_NEXT_COMMAND, H_SESSION_ID,
    H_TREE_ID, HDR_LEN, MAX_TRANSACT,
};
use crate::{
    STATUS_ACCESS_DENIED, STATUS_BAD_NETWORK_NAME, STATUS_END_OF_FILE,
    STATUS_FILE_CLOSED, STATUS_FILE_IS_A_DIRECTORY, STATUS_FS_DRIVER_REQUIRED,
    STATUS_INVALID_PARAMETER, STATUS_MORE_PROCESSING_REQUIRED, STATUS_NO_MORE_FILES,
    STATUS_NOT_SUPPORTED, STATUS_OBJECT_NAME_NOT_FOUND, STATUS_OBJECT_PATH_NOT_FOUND,
    STATUS_SUCCESS, STATUS_USER_SESSION_DELETED,
};

/// How many files (and the root) one connection may hold open at once. macOS keeps a handful open
/// during a browse; sixteen is comfortable, and the seventeenth gets a real status rather than a
/// corrupted table.
pub const MAX_HANDLES: usize = 16;

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

/// One open handle: the node, and (for the root) where its directory enumeration has got to.
#[derive(Clone, Copy)]
struct Handle {
    node: Node,
    /// The volatile half of the wire file id, so a stale id from a closed handle misses.
    volatile: u64,
    /// The next [`Share::entry`] index `QUERY_DIRECTORY` will emit; 0 and 1 are the synthetic
    /// `.` and `..` rows.
    enum_index: usize,
    /// Set once enumeration answered `STATUS_NO_MORE_FILES`, so the next query starts over only
    /// if the client asks for a restart.
    enum_done: bool,
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
            CMD_TREE_CONNECT => self.tree_connect(req, out, msg_id, credits, err),
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
            CMD_QUERY_DIRECTORY => self.query_directory(req, out, share, chain_fid, err),
            CMD_QUERY_INFO => self.query_info(req, out, share, chain_fid, err),
            CMD_ECHO | CMD_FLUSH => simple_ok(out, cmd, msg_id, sid, tid, credits),
            // The read-only refusals, and the not-heres. DFS referrals get the status Windows
            // uses for "no DFS here" so clients fall back to the plain path.
            CMD_WRITE | CMD_SET_INFO => err(out, STATUS_ACCESS_DENIED),
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

        write_response_header(out, CMD_NEGOTIATE, STATUS_SUCCESS, msg_id, 0, 0, credits, 0);
        let b = HDR_LEN;
        out[b..b + 64].fill(0);
        w16(out, b, 65); // StructureSize
        w16(out, b + 2, 1); // SecurityMode: signing enabled, not required
        w16(out, b + 4, DIALECT_0210);
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
                    out[buf_at..buf_at + challenge_len].copy_from_slice(&challenge[..challenge_len]);
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
        // MaximalAccess: the read-side generic rights (READ_DATA | READ_EA | READ_ATTRIBUTES |
        // EXECUTE | READ_CONTROL | SYNCHRONIZE), which is the truth about this share.
        w32(out, b + 12, 0x0012_00A9);
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
        // FILE_OPEN = 1 and FILE_OPEN_IF = 3 open what exists; every disposition that would
        // create or truncate is the read-only refusal.
        if disposition != 1 && disposition != 3 {
            return err(out, STATUS_ACCESS_DENIED);
        }
        let Some(node) = share.lookup(name) else {
            return err(out, STATUS_OBJECT_NAME_NOT_FOUND);
        };
        // CreateOptions can insist on one kind of node.
        let is_dir = matches!(node, Node::Root);
        if options & 0x1 != 0 && !is_dir {
            return err(out, STATUS_NOT_SUPPORTED); // FILE_DIRECTORY_FILE on a file
        }
        if options & 0x40 != 0 && is_dir {
            return err(out, STATUS_FILE_IS_A_DIRECTORY); // FILE_NON_DIRECTORY_FILE on the root
        }

        let Some(slot) = self.handles.iter().position(Option::is_none) else {
            return err(out, STATUS_INSUFFICIENT_RESOURCES);
        };
        let volatile = self.next_volatile;
        self.next_volatile += 1;
        self.handles[slot] = Some(Handle {
            node,
            volatile,
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
        w32(out, b + 4, 1); // CreateAction: opened
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
        let n = share.read(file, offset, &mut out[data_at..data_at + want]);
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
        let node = self.handles[slot].unwrap().node;
        let (size, attrs, is_dir) = match node {
            Node::Root => (0, ATTR_DIRECTORY, true),
            Node::File(i) => (share.size(i), ATTR_NORMAL, false),
        };

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
                    Node::File(i) => 3 + i as u64,
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
                w32(d, 76, 0x0012_00A9); // AccessInformation: what tree connect granted
                // NameInformation: `\` + the name.
                let mut name = [0u8; 65];
                name[0] = b'\\';
                let nlen = 1 + match node {
                    Node::Root => 0,
                    Node::File(i) => {
                        let e = share.entry(i).unwrap();
                        let n = e.name.len().min(64);
                        name[1..1 + n].copy_from_slice(&e.name[..n]);
                        n
                    }
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
            // FILESYSTEM / FileFsSizeInformation: a small, full, read-only volume.
            (2, 3) => {
                w64(d, 0, 1024); // TotalAllocationUnits
                w64(d, 8, 0); // Available
                w32(d, 16, 1); // SectorsPerAllocationUnit
                w32(d, 20, 4096); // BytesPerSector
                24
            }
            // FILESYSTEM / FileFsDeviceInformation: a disk, read-only.
            (2, 4) => {
                w32(d, 0, 7); // FILE_DEVICE_DISK
                w32(d, 4, 0x8); // FILE_READ_ONLY_DEVICE
                8
            }
            // FILESYSTEM / FileFsAttributeInformation.
            (2, 5) => {
                // CASE_PRESERVED | UNICODE_ON_DISK | READ_ONLY_VOLUME
                w32(d, 0, 0x2 | 0x4 | 0x0008_0000);
                w32(d, 4, 64); // MaximumComponentNameLength
                let wide = ascii_to_utf16le(b"nifefs", &mut d[12..]);
                w32(d, 8, wide as u32);
                12 + wide
            }
            // FILESYSTEM / FileFsFullSizeInformation.
            (2, 7) => {
                w64(d, 0, 1024);
                w64(d, 8, 0);
                w64(d, 16, 0);
                w32(d, 24, 1);
                w32(d, 28, 4096);
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
        1 => (64, true),                     // FileDirectoryInformation
        2 => (68, true),                     // FileFullDirectoryInformation
        3 => (94, true),                     // FileBothDirectoryInformation
        37 => (104, true),                   // FileIdBothDirectoryInformation
        38 => (80, true),                    // FileIdFullDirectoryInformation
        12 => (12, false),                   // FileNamesInformation
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
    use crate::client;
    use crate::share::FIXTURE;
    use crate::{H_NEXT_COMMAND, H_STATUS, MAX_MESSAGE, r32};

    fn conn() -> Connection {
        Connection::new([0xA5; 8])
    }

    /// Drive one request through the machine and return the response bytes.
    fn rt(c: &mut Connection, req: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; MAX_MESSAGE];
        let n = c.handle(req, &mut out, &FIXTURE).expect("smb2 in, smb2 out");
        out.truncate(n);
        out
    }

    fn status(resp: &[u8]) -> u32 {
        r32(resp, H_STATUS)
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
        let resp = rt(&mut c, &client::query_directory(6, sid, tid, &fid, 37, b"*"));
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
        let resp = rt(&mut c, &client::query_directory(7, sid, tid, &fid, 37, b"*"));
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

    #[test]
    fn what_the_readonly_share_refuses_and_with_which_status() {
        let mut c = conn();
        let (sid, tid) = establish(&mut c);
        // Creating a file (disposition FILE_CREATE = 2).
        let resp = rt(
            &mut c,
            &client::create_disposition(5, sid, tid, b"new.txt", 2),
        );
        assert_eq!(status(&resp), STATUS_ACCESS_DENIED);
        // Writing to an open file.
        let resp = rt(&mut c, &client::create(6, sid, tid, b"hello.txt"));
        let fid = client::create_file_id(&resp);
        let resp = rt(&mut c, &client::write(7, sid, tid, &fid, b"x"));
        assert_eq!(status(&resp), STATUS_ACCESS_DENIED);
        // A name that is not there, and a path below a flat root.
        let resp = rt(&mut c, &client::create(8, sid, tid, b"absent.txt"));
        assert_eq!(status(&resp), STATUS_OBJECT_NAME_NOT_FOUND);
        let resp = rt(&mut c, &client::create(9, sid, tid, b"a\\b.txt"));
        assert_eq!(status(&resp), STATUS_OBJECT_PATH_NOT_FOUND);
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
