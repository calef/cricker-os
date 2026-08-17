//! **Apple's SMB2 extensions: the `AAPL` create context** (milestone 55).
//!
//! macOS will mount a plain SMB2 share, and it will not offer one as a **Time Machine
//! destination**. What it looks for is this: an `AAPL`-tagged create context on the first CREATE
//! of a tree connect, answered by the server with a context of its own saying which Apple
//! extensions it implements. Samba implements the same exchange as `vfs_fruit`, and
//! `fruit:aapl = yes` is the first line of the working configuration
//! (design/roadmap/55-time-machine.md records the whole stanza, measured off calef's router).
//!
//! There is no public specification. [MS-SMB2] defines the *container* ([`crate::create_context`])
//! and says nothing about this tag, so what follows is the layout Samba's `vfs_fruit` implements
//! and that macOS has been talking to for a decade. **Where this file states a constant it is
//! because that is what the reference implementation puts on the wire**, not because a document
//! says so, and the honest consequence is recorded under BUGS: nothing here has met a Mac.
//!
//! # The exchange
//!
//! The client sends **24 bytes** and no other length is accepted (the reference checks the same
//! way, and treats any other length as a context to ignore):
//!
//! | offset | field | |
//! |---|---|---|
//! | 0 | command (u32) | [`CMD_SERVER_QUERY`]; [`CMD_RESOLVE_ID`] is the other one and is not answered |
//! | 4 | reserved (u32) | |
//! | 8 | request bitmap (u64) | which of the three answers below it wants |
//! | 16 | client capabilities (u64) | what the *client* implements |
//!
//! The server answers with a 16-byte prefix (the same command code, a zero reserved word, and the
//! **request bitmap echoed** as the reply bitmap) followed by exactly the answers the bitmap asked
//! for, in bit order:
//!
//! | bit | answer |
//! |---|---|
//! | [`BIT_SERVER_CAPS`] | server capabilities (u64) |
//! | [`BIT_VOLUME_CAPS`] | volume capabilities (u64) |
//! | [`BIT_MODEL_INFO`] | a zero word, the model string's byte length (u32), then the string in UTF-16LE |
//!
//! # What this server claims, and what it refuses to claim
//!
//! Every bit below is a claim a client will act on, so each one is set only where this tree can
//! back it. That is the whole of the decision and it is listed in the pull request as a wire
//! decision, because the bits are something two programs agree on.
//!
//! **Server capabilities: [`SERVER_UNIX_BASED`], and nothing else.** The reference sets it
//! unconditionally, and it is true here in the sense the bit means (there is no NT ACL model
//! behind this share). The three it does not set:
//!
//! - [`SERVER_READ_DIR_ATTR`] would promise Apple's extended directory enumeration, which returns
//!   Finder info and resource-fork sizes inside the listing. This server has no Finder info to
//!   return (see the metadata question in milestone 55's block), so claiming it would make macOS
//!   ask a question with no true answer.
//! - [`SERVER_OSX_COPYFILE`] would promise a server-side copy, which arrives as an `FSCTL` this
//!   server answers `STATUS_FS_DRIVER_REQUIRED`.
//! - [`SERVER_NFS_ACE`] would promise POSIX permissions carried as NFS ACEs. The reference has
//!   `fruit:nfs_aces = no`, so the working configuration does not set it either.
//!
//! **Volume capabilities: [`VOLUME_FULL_SYNC`], and nothing else.** That bit **is**
//! `fruit:time machine = yes`: it is the single flag on the SMB side that tells macOS this
//! server's durability is good enough to hold a backup, and without it the share is a share and
//! never a destination. See BUGS for exactly how far that claim is currently backed. The two it
//! does not set:
//!
//! - [`VOLUME_CASE_SENSITIVE`] would be a lie in the other direction. The backing filesystem is
//!   case-sensitive, but this server folds every name to lower case at the wire
//!   ([`crate::utf16le_to_ascii_lower`], and the crate BUGS on names), so what a client can
//!   observe is a share that is not case-sensitive.
//! - [`VOLUME_RESOLVE_ID`] would promise [`CMD_RESOLVE_ID`], resolving a file by its on-disk id
//!   without a path. Nothing here mints stable file ids.
//!
//! # EXAMPLES
//!
//! The whole of the server side, which is what [`crate::server`] calls when it finds the tag:
//!
//! ```
//! use smb_proto::apple;
//!
//! // What macOS sends: a server query asking for all three answers.
//! let mut request = [0u8; apple::REQUEST_LEN];
//! request[0] = apple::CMD_SERVER_QUERY as u8;
//! request[8] = (apple::BIT_SERVER_CAPS | apple::BIT_VOLUME_CAPS | apple::BIT_MODEL_INFO) as u8;
//!
//! let mut reply = [0u8; apple::MAX_RESPONSE];
//! let n = apple::server_query(&request, &mut reply).unwrap();
//! let reply = &reply[..n];
//!
//! // The reply bitmap echoes the request's, and the answers follow in bit order.
//! assert_eq!(smb_proto::r64(reply, 8), 0b111);
//! assert_eq!(smb_proto::r64(reply, 16), apple::SERVER_UNIX_BASED);
//! assert_eq!(smb_proto::r64(reply, 24) & apple::VOLUME_FULL_SYNC, apple::VOLUME_FULL_SYNC);
//!
//! // The model comes back as the UTF-16LE it is on the wire, so a reader that wants to compare
//! // it encodes what it expected rather than decoding what arrived.
//! let mut want = [0u8; 2 * 11];
//! let n = smb_proto::ascii_to_utf16le(apple::MODEL, &mut want);
//! assert_eq!(apple::model_utf16le(reply), Some(&want[..n]));
//! ```
//!
//! # BUGS
//!
//! - **Nothing here has met a Mac**, and it cannot under the emulator's discovery half; the TCP
//!   half can be and is (the QEMU gate sends this context and checks the answer), but a conforming
//!   client this tree wrote is not `smbfs`. The first real Time Machine attempt is where the rest
//!   of milestone 55 gets written.
//! - **[`VOLUME_FULL_SYNC`] is claimed further than the stack currently backs it.** What is true:
//!   the FS server puts every `fs_proto` write through one RedoxFS transaction that commits to the
//!   header ring before the reply, so there is no write-back cache above the block device for a
//!   flush to push, and SMB2's FLUSH has genuinely nothing to do. What is not true: the block
//!   server issues no `VIRTIO_BLK_T_FLUSH`, so the durability of the last acknowledged write is
//!   the device's word rather than ours (notes/fs-server.md's crash-injection table records the
//!   same gap from the other side). A host that loses power can therefore lose a write this server
//!   acknowledged. Closing it is a device flush in the block server and a sync verb in `fs_proto`,
//!   and it is the piece of milestone 55 that should land before anybody's real backup does.
//! - **The model string is compiled in.** [`MODEL`] is one constant, matching the reference's
//!   `fruit:model = TimeCapsule`. `mdns_config` made the same string a document a person edits and
//!   this has not; milestone 131 (a share is configured, not compiled) is where the two meet.
//!   Worth knowing that the reference runs with the two knobs **disagreeing**: its `_device-info`
//!   TXT advertises `model=MacSamba` while its Samba config says `TimeCapsule` (notes/mdns.md's
//!   capture), so they are not one setting and copying one into the other would be wrong.
//! - **[`CMD_RESOLVE_ID`] is not answered.** A client that sends it gets no context back, which is
//!   what the reference does for a command it does not implement. Since [`VOLUME_RESOLVE_ID`] is
//!   never advertised, no conforming client should send it.
//! - **The context is answered on every CREATE that carries it**, not only the first of a tree
//!   connect. The exchange holds no state, so a client that re-asks gets the same answer; that is
//!   cheaper than remembering, and no client re-asks.
//!
//! Name: unrecorded. `apple` is provisional, minted by milestone 55's lane on 2026-08-17. It is a
//! noun, it is the vendor whose extensions these are, and the alternative that suggested itself
//! (`fruit`, after Samba's module) names the reference implementation rather than the thing.

use crate::{ascii_to_utf16le, r32, r64, w32, w64};

/// The create-context tag, four ASCII bytes.
pub const TAG: &[u8; 4] = b"AAPL";

/// **Server query**, the command this server answers: "what do you implement, and what are you".
pub const CMD_SERVER_QUERY: u32 = 1;
/// **Resolve id**, resolving a file by its on-disk id. Not answered; see BUGS.
pub const CMD_RESOLVE_ID: u32 = 2;

/// The request's length, and the only length accepted. The reference checks the same way.
pub const REQUEST_LEN: usize = 24;

// The request bitmap, which is also the reply bitmap: which of the three answers the client wants.
/// The client wants the server capabilities word.
pub const BIT_SERVER_CAPS: u64 = 1 << 0;
/// The client wants the volume capabilities word.
pub const BIT_VOLUME_CAPS: u64 = 1 << 1;
/// The client wants the model string.
pub const BIT_MODEL_INFO: u64 = 1 << 2;

// Server capabilities.
/// Apple's extended directory enumeration (Finder info and fork sizes inside a listing).
pub const SERVER_READ_DIR_ATTR: u64 = 1 << 0;
/// Server-side copy, through an `FSCTL`.
pub const SERVER_OSX_COPYFILE: u64 = 1 << 1;
/// The server is UNIX-based: there is no NT ACL model behind the share. The one this server sets.
pub const SERVER_UNIX_BASED: u64 = 1 << 2;
/// POSIX permissions carried as NFS ACEs.
pub const SERVER_NFS_ACE: u64 = 1 << 3;

// Volume capabilities.
/// The volume can resolve a file by its on-disk id ([`CMD_RESOLVE_ID`]).
pub const VOLUME_RESOLVE_ID: u64 = 1 << 0;
/// The volume is case-sensitive.
pub const VOLUME_CASE_SENSITIVE: u64 = 1 << 1;
/// **The Time Machine bit**: the volume honours a full sync. `fruit:time machine = yes`.
pub const VOLUME_FULL_SYNC: u64 = 1 << 2;

/// **What this server claims to implement.** See the module header for why each of the other three
/// bits is left clear.
pub const SERVER_CAPS: u64 = SERVER_UNIX_BASED;

/// **What this server claims about the volume.** [`VOLUME_FULL_SYNC`] is what makes macOS willing
/// to back up here at all; the BUGS section is honest about how far the stack backs it.
pub const VOLUME_CAPS: u64 = VOLUME_FULL_SYNC;

/// **The model string**, which picks the icon macOS draws and matches the reference's
/// `fruit:model = TimeCapsule`. Compiled in; see BUGS.
pub const MODEL: &[u8] = b"TimeCapsule";

/// The longest response [`server_query`] can produce: the 16-byte prefix, both capability words,
/// the model header, and the model in UTF-16LE. A caller sizes its scratch buffer with this, so
/// the response cannot outgrow what was reserved for it.
pub const MAX_RESPONSE: usize = 16 + 8 + 8 + 8 + 2 * MODEL.len();

/// **Answer an `AAPL` create context**, writing the reply payload into `out` and returning its
/// length.
///
/// `None` means "no context in the answer", which is the reference's behaviour for anything it
/// does not recognise and is the right one: an unanswered context is how the extension mechanism
/// says "not implemented", and refusing the CREATE instead would turn an optional extension into a
/// failed mount. It covers a request that is not [`REQUEST_LEN`] bytes, a command that is not
/// [`CMD_SERVER_QUERY`], and an `out` too small to hold the answer.
pub fn server_query(request: &[u8], out: &mut [u8]) -> Option<usize> {
    if request.len() != REQUEST_LEN || r32(request, 0) != CMD_SERVER_QUERY {
        return None;
    }
    // The client's own capabilities sit at offset 16 and are deliberately not read. The reference
    // gates two of its answers on them, because it only claims a bit when both sides implement it;
    // every bit this server claims is one it implements unconditionally, so there is nothing to
    // intersect and reading the word would be a decision that is not being made.
    let bitmap = r64(request, 8);

    // The whole length is known from the bitmap before a byte is written, so the buffer is
    // checked once rather than at each field. A partially written answer is not a thing this
    // function can produce.
    let mut n = 16;
    if bitmap & BIT_SERVER_CAPS != 0 {
        n += 8;
    }
    if bitmap & BIT_VOLUME_CAPS != 0 {
        n += 8;
    }
    if bitmap & BIT_MODEL_INFO != 0 {
        n += 8 + 2 * MODEL.len();
    }
    if out.len() < n {
        return None;
    }

    w32(out, 0, CMD_SERVER_QUERY);
    w32(out, 4, 0);
    w64(out, 8, bitmap); // the reply bitmap is the request's, echoed

    let mut at = 16;
    if bitmap & BIT_SERVER_CAPS != 0 {
        w64(out, at, SERVER_CAPS);
        at += 8;
    }
    if bitmap & BIT_VOLUME_CAPS != 0 {
        w64(out, at, VOLUME_CAPS);
        at += 8;
    }
    if bitmap & BIT_MODEL_INFO != 0 {
        // A zero word, then the string's length in bytes, then the string. The zero is what the
        // reference writes; it reads as a pad and nothing has ever been observed in it.
        w32(out, at, 0);
        w32(out, at + 4, (2 * MODEL.len()) as u32);
        ascii_to_utf16le(MODEL, &mut out[at + 8..]);
    }
    Some(n)
}

/// **The model string a reply carries**, as the UTF-16LE it is on the wire, or `None` if the reply
/// did not carry one.
///
/// The inverse of the model-info leg above, and it exists so that a client (the host tests, and
/// xtask's prober against a live guest) reads the field through the same offsets the server writes
/// it through rather than through a second copy of them. Where the answer sits depends on which
/// other bits the bitmap set, which is the whole reason this is a function and not an offset.
///
/// It returns the **encoded** bytes rather than decoding them, because decoding needs a buffer and
/// this crate has no allocator; a caller that wants to compare encodes what it expected with
/// [`crate::ascii_to_utf16le`]. What it does check is that the string is the ASCII a model always
/// is (every second byte zero), so a reply carrying something else is `None` rather than a slice a
/// caller would compare byte-wise and misread.
pub fn model_utf16le(reply: &[u8]) -> Option<&[u8]> {
    let bitmap = r64(reply, 8);
    if bitmap & BIT_MODEL_INFO == 0 {
        return None;
    }
    let mut at = 16;
    if bitmap & BIT_SERVER_CAPS != 0 {
        at += 8;
    }
    if bitmap & BIT_VOLUME_CAPS != 0 {
        at += 8;
    }
    let len = r32(reply.get(at..at + 8)?, 4) as usize;
    let wide = reply.get(at + 8..at + 8 + len)?;
    // Every second byte must be zero for this to be the ASCII the model always is; a model with a
    // non-ASCII character would need a real decoder, and there is no reason for one to exist.
    if !len.is_multiple_of(2) || wide.iter().skip(1).step_by(2).any(|&b| b != 0) {
        return None;
    }
    Some(wide)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the 24 bytes a client sends.
    fn request(cmd: u32, bitmap: u64, client_caps: u64) -> [u8; REQUEST_LEN] {
        let mut r = [0u8; REQUEST_LEN];
        w32(&mut r, 0, cmd);
        w64(&mut r, 8, bitmap);
        w64(&mut r, 16, client_caps);
        r
    }

    /// Decode the ASCII of a model reply, for tests that want to read it as a string.
    fn model_ascii(reply: &[u8]) -> Vec<u8> {
        model_utf16le(reply)
            .unwrap()
            .iter()
            .step_by(2)
            .copied()
            .collect()
    }

    #[test]
    fn the_full_query_a_mac_sends_gets_every_answer() {
        let all = BIT_SERVER_CAPS | BIT_VOLUME_CAPS | BIT_MODEL_INFO;
        let mut out = [0u8; MAX_RESPONSE];
        let n = server_query(&request(CMD_SERVER_QUERY, all, 0), &mut out).unwrap();
        assert_eq!(n, MAX_RESPONSE);
        assert_eq!(r32(&out, 0), CMD_SERVER_QUERY);
        assert_eq!(r32(&out, 4), 0);
        assert_eq!(r64(&out, 8), all, "the reply bitmap is the request's");
        assert_eq!(r64(&out, 16), SERVER_CAPS);
        assert_eq!(r64(&out, 24), VOLUME_CAPS);
        assert_eq!(model_ascii(&out[..n]), MODEL);
    }

    /// The claims, pinned one at a time. A bit set by accident is a promise to a client, so each
    /// one is asserted in both directions rather than by comparing two constants.
    #[test]
    fn the_claims_are_exactly_the_ones_the_reference_backs() {
        assert_eq!(SERVER_CAPS & SERVER_UNIX_BASED, SERVER_UNIX_BASED);
        assert_eq!(SERVER_CAPS & SERVER_READ_DIR_ATTR, 0);
        assert_eq!(SERVER_CAPS & SERVER_OSX_COPYFILE, 0);
        assert_eq!(SERVER_CAPS & SERVER_NFS_ACE, 0);

        // The Time Machine bit. Without it macOS mounts the share and never offers it.
        assert_eq!(VOLUME_CAPS & VOLUME_FULL_SYNC, VOLUME_FULL_SYNC);
        assert_eq!(VOLUME_CAPS & VOLUME_CASE_SENSITIVE, 0);
        assert_eq!(VOLUME_CAPS & VOLUME_RESOLVE_ID, 0);
    }

    #[test]
    fn a_partial_bitmap_gets_exactly_what_it_asked_for() {
        let mut out = [0u8; MAX_RESPONSE];

        // Volume caps alone: the word lands where server caps would otherwise have been.
        let n = server_query(&request(CMD_SERVER_QUERY, BIT_VOLUME_CAPS, 0), &mut out).unwrap();
        assert_eq!(n, 24);
        assert_eq!(r64(&out, 8), BIT_VOLUME_CAPS);
        assert_eq!(r64(&out, 16), VOLUME_CAPS);
        assert_eq!(model_utf16le(&out[..n]), None);

        // The model alone, so the reader's offset arithmetic is exercised with both words absent.
        let n = server_query(&request(CMD_SERVER_QUERY, BIT_MODEL_INFO, 0), &mut out).unwrap();
        assert_eq!(n, 16 + 8 + 2 * MODEL.len());
        assert_eq!(model_ascii(&out[..n]), MODEL);

        // An empty bitmap is a legal question with a three-word answer and nothing after it.
        let n = server_query(&request(CMD_SERVER_QUERY, 0, 0), &mut out).unwrap();
        assert_eq!(n, 16);
        assert_eq!(model_utf16le(&out[..n]), None);
    }

    #[test]
    fn an_unknown_bit_in_the_bitmap_is_echoed_and_answers_nothing() {
        let mut out = [0u8; MAX_RESPONSE];
        let odd = BIT_SERVER_CAPS | (1 << 40);
        let n = server_query(&request(CMD_SERVER_QUERY, odd, 0), &mut out).unwrap();
        assert_eq!(r64(&out, 8), odd, "the bitmap is echoed whole");
        assert_eq!(n, 24, "and only the bits with an answer produce one");
    }

    #[test]
    fn what_is_not_a_server_query_gets_no_context_back() {
        let mut out = [0u8; MAX_RESPONSE];
        // Resolve-id, which this server does not advertise and does not answer.
        assert_eq!(
            server_query(&request(CMD_RESOLVE_ID, BIT_SERVER_CAPS, 0), &mut out),
            None
        );
        // A length that is not the one the reference accepts, in both directions.
        assert_eq!(server_query(&[0u8; 16], &mut out), None);
        assert_eq!(server_query(&[0u8; 32], &mut out), None);
        assert_eq!(server_query(&[], &mut out), None);
    }

    #[test]
    fn a_short_buffer_is_refused_rather_than_truncated() {
        let all = BIT_SERVER_CAPS | BIT_VOLUME_CAPS | BIT_MODEL_INFO;
        let req = request(CMD_SERVER_QUERY, all, 0);
        for len in 0..MAX_RESPONSE {
            let mut out = vec![0u8; len];
            assert_eq!(server_query(&req, &mut out), None, "buffer of {len}");
        }
        let mut out = [0u8; MAX_RESPONSE];
        assert!(server_query(&req, &mut out).is_some());
    }

    /// The client's own capability word is read past, not read. A request that advertises nothing
    /// gets the same answer as one that advertises everything, which is the decision the module
    /// header records.
    #[test]
    fn the_clients_own_capabilities_do_not_change_the_answer() {
        let all = BIT_SERVER_CAPS | BIT_VOLUME_CAPS | BIT_MODEL_INFO;
        let mut a = [0u8; MAX_RESPONSE];
        let mut b = [0u8; MAX_RESPONSE];
        let na = server_query(&request(CMD_SERVER_QUERY, all, 0), &mut a).unwrap();
        let nb = server_query(&request(CMD_SERVER_QUERY, all, u64::MAX), &mut b).unwrap();
        assert_eq!(a[..na], b[..nb]);
    }
}
