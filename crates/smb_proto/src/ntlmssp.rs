//! **NTLMSSP messages** ([MS-NLMP] §2.2): the three-message dance SMB2 session setup carries.
//!
//! This module is the *message framing* only, and that is now a security property rather than a
//! description of unfinished work. The arithmetic (`NTOWFv2`, proofs, session keys) lives in the
//! `ntlm` crate, **which this crate does not depend on**: `ntlm` is a
//! `[dev-dependencies]` entry, so the shipping `smb_proto` cannot compute a proof and the tests
//! can. That is the strongest available form of "the SMB server never holds the key" (AGENTS.md's
//! ladder, rung one): the code that would need one is not linked into the artifact, and Cargo is
//! the mechanism.
//!
//! What this module does is take an AUTHENTICATE message apart ([`parse`] into
//! [`Message::Authenticate`], carrying an [`Authenticate`]) and hand the pieces to the
//! [`crate::authenticator`] seam, which answers yes or no. Every byte it extracts is either public
//! (the account name, the domain, the client's blob) or a MAC (`NTProofStr`); none of it is a key,
//! and none of it is useful without the key that verifies it.
//!
//! The CHALLENGE this module builds is what lets a real client get that far: macOS will not
//! proceed to AUTHENTICATE without a well-formed CHALLENGE carrying target info.

use crate::{ascii_to_utf16le, r16, r32, w16, w32};

/// `"NTLMSSP\0"`, the signature every NTLMSSP message opens with.
pub const SIGNATURE: [u8; 8] = *b"NTLMSSP\0";

/// Message types (the u32 after the signature).
pub const NEGOTIATE: u32 = 1;
pub const CHALLENGE: u32 = 2;
pub const AUTHENTICATE: u32 = 3;

/// The width of an NTLMv2 `NTProofStr`, in bytes ([MS-NLMP] §2.2.2.8). Sixteen, because it is an
/// HMAC-MD5; the same number as `ntlm::KEY_LEN` and `cred_proto::KEY_LEN`, spelled here so this
/// crate needs neither of them.
pub const PROOF_LEN: usize = 16;

/// **What a client's AUTHENTICATE said** ([MS-NLMP] §2.2.1.3), as borrows into the message.
///
/// Nothing here is secret. The two names are public identifiers, the blob is entirely the client's
/// own and entirely public (`ntlm::proof`'s doc argues that), and the proof is a MAC that verifies
/// only under a key this crate never sees. So an [`Authenticate`] can be handed to a seam, logged,
/// or dropped, and the only thing it is good for is asking somebody who holds a key whether it
/// checks out.
///
/// The names are **UTF-16LE as they arrived**, not decoded. That is deliberate: the key derivation
/// binds the exact bytes the client encoded ([MS-NLMP] §3.3.2, and `ntlm::ntowfv2`'s asymmetric
/// uppercasing), so re-encoding them here would be this crate inventing a spelling the client did
/// not send. A consumer that needs them as text says so and converts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Authenticate<'a> {
    /// `DomainName`, UTF-16LE. Empty when the client sent none, which is legal.
    pub domain: &'a [u8],
    /// `UserName`, UTF-16LE. Empty for an anonymous or guest-shaped AUTHENTICATE.
    pub user: &'a [u8],
    /// `Workstation`, UTF-16LE. Carried because it is there; nothing here reads it.
    pub workstation: &'a [u8],
    /// `NtChallengeResponse`: `NTProofStr` followed by the client's `temp` blob
    /// ([MS-NLMP] §2.2.2.8). Empty for a guest-shaped AUTHENTICATE, which is exactly how the
    /// server tells "I am nobody" from "here is who I am".
    pub nt_response: &'a [u8],
    /// `NegotiateFlags`.
    pub flags: u32,
}

impl<'a> Authenticate<'a> {
    /// The `NTProofStr` and the blob it was computed over, or `None` when the client sent no
    /// NTLMv2 response at all (an empty or too-short `NtChallengeResponse`).
    ///
    /// `None` is the guest-shaped AUTHENTICATE the tree's own prober and `mount_smbfs -N` send. It
    /// is a *distinct* answer from "a proof that did not verify", and keeping the two apart is what
    /// lets a share with an authenticator refuse an anonymous client without pretending it did
    /// arithmetic (see [`crate::authenticator`]).
    pub fn proof(&self) -> Option<(&'a [u8; PROOF_LEN], &'a [u8])> {
        if self.nt_response.len() < PROOF_LEN {
            return None;
        }
        let (p, blob) = self.nt_response.split_at(PROOF_LEN);
        Some((p.try_into().ok()?, blob))
    }
}

/// What arrived in a session-setup security buffer, from NTLMSSP's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message<'a> {
    /// Type 1: the client wants a CHALLENGE.
    Negotiate,
    /// Type 3: the client answered the challenge.
    Authenticate(Authenticate<'a>),
}

/// One `<len, maxlen, offset>` field triple at `at`, resolved to the payload it points at
/// ([MS-NLMP] §2.2.2.10's `NTLMSSP_MESSAGE_FIELDS`: `Len` and `MaxLen` are `u16`, `Offset` a `u32`
/// from the start of the message).
///
/// An empty slice for a field that is absent, and an empty slice for a field whose offset or length
/// runs off the end of the message. Those two are treated alike **on purpose**: a truncated field
/// is a client sending nonsense, and answering "you gave me nothing" is both true and the same
/// refusal, where a distinct error would only add a branch nothing acts on. `MaxLen` is ignored,
/// which is what every implementation does: it is the buffer the client allocated, not the data.
fn field(msg: &[u8], at: usize) -> &[u8] {
    if msg.len() < at + 8 {
        return &[];
    }
    let len = r16(msg, at) as usize;
    let off = r32(msg, at + 4) as usize;
    msg.get(off..off + len).unwrap_or(&[])
}

/// Classify an NTLMSSP message, or `None` if these bytes are not one.
///
/// An AUTHENTICATE is taken apart here rather than at the call site, so the offsets in
/// [MS-NLMP] §2.2.1.3 appear exactly once in this tree and are pinned by this module's tests.
pub fn parse(token: &[u8]) -> Option<Message<'_>> {
    if token.len() < 12 || token[..8] != SIGNATURE {
        return None;
    }
    match r32(token, 8) {
        NEGOTIATE => Some(Message::Negotiate),
        AUTHENTICATE => {
            // §2.2.1.3's fixed part: six field triples from offset 12, then the flags. A message
            // too short to hold them is not an AUTHENTICATE, however it is labelled.
            if token.len() < 64 {
                return None;
            }
            Some(Message::Authenticate(Authenticate {
                // 12: LmChallengeResponse, which NTLMv2 does not use and this server ignores.
                nt_response: field(token, 20),
                domain: field(token, 28),
                user: field(token, 36),
                workstation: field(token, 44),
                // 52: EncryptedRandomSessionKey, meaningful only with key exchange, which this
                // server does not negotiate.
                flags: r32(token, 60),
            }))
        }
        _ => None,
    }
}

/// The flags our CHALLENGE carries. Chosen against what macOS's client sends in its NEGOTIATE, not
/// aspirationally: unicode strings, NTLM, target name is a server, extended session security (the
/// NTLMv2 marker), target info present, and the two key-size flags every modern client asserts.
const CHALLENGE_FLAGS: u32 = 0x0000_0001 // NEGOTIATE_UNICODE
    | 0x0000_0200 // NEGOTIATE_NTLM
    | 0x0001_0000 // TARGET_TYPE_SERVER
    | 0x0008_0000 // NEGOTIATE_EXTENDED_SESSIONSECURITY
    | 0x0080_0000 // NEGOTIATE_TARGET_INFO
    | 0x2000_0000 // NEGOTIATE_128
    | 0x8000_0000; // NEGOTIATE_56

/// The name this server calls itself in the CHALLENGE's target name and target info. ASCII,
/// upper-case by `NetBIOS` convention. Provisional like every name here.
pub const TARGET_NAME: &[u8] = b"NIFE";

/// AV pair ids ([MS-NLMP] §2.2.2.1).
const AV_EOL: u16 = 0;
const AV_NB_COMPUTER: u16 = 1;
const AV_NB_DOMAIN: u16 = 2;

/// Build a CHALLENGE message around `server_challenge`, returning its length. `out` needs
/// [`CHALLENGE_MAX`] bytes.
///
/// Layout ([MS-NLMP] §2.2.1.2): signature, type, `TargetName` fields, flags, the 8-byte challenge,
/// 8 reserved bytes, `TargetInfo` fields, then the two payloads. No Version block, and the flags do
/// not claim one.
pub fn build_challenge(out: &mut [u8], server_challenge: &[u8; 8]) -> usize {
    const FIXED: usize = 48; // through the TargetInfo field pair, no Version
    let name_len = TARGET_NAME.len() * 2;
    // Target info: two AV pairs naming this machine, then the terminator.
    let av_len = (4 + name_len) * 2 + 4;

    out[..FIXED].fill(0);
    out[..8].copy_from_slice(&SIGNATURE);
    w32(out, 8, CHALLENGE);
    // TargetName: len, maxlen, offset.
    w16(out, 12, name_len as u16);
    w16(out, 14, name_len as u16);
    w32(out, 16, FIXED as u32);
    w32(out, 20, CHALLENGE_FLAGS);
    out[24..32].copy_from_slice(server_challenge);
    // 32..40 reserved, already zero.
    w16(out, 40, av_len as u16);
    w16(out, 42, av_len as u16);
    w32(out, 44, (FIXED + name_len) as u32);

    let mut p = FIXED;
    p += ascii_to_utf16le(TARGET_NAME, &mut out[p..]);
    for id in [AV_NB_COMPUTER, AV_NB_DOMAIN] {
        w16(out, p, id);
        w16(out, p + 2, name_len as u16);
        p += 4;
        p += ascii_to_utf16le(TARGET_NAME, &mut out[p..]);
    }
    w16(out, p, AV_EOL);
    w16(out, p + 2, 0);
    p + 4
}

/// The largest CHALLENGE [`build_challenge`] emits, for sizing callers' buffers.
pub const CHALLENGE_MAX: usize = 48 + 3 * (4 + 2 * TARGET_NAME.len()) + 4;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{r16, r64};

    #[test]
    fn a_built_challenge_parses_as_a_challenge_and_carries_the_challenge_bytes() {
        let mut buf = [0u8; CHALLENGE_MAX];
        let ch = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let n = build_challenge(&mut buf, &ch);
        assert!(n <= CHALLENGE_MAX);
        let msg = &buf[..n];
        assert_eq!(&msg[..8], &SIGNATURE);
        assert_eq!(r32(msg, 8), CHALLENGE);
        assert_eq!(&msg[24..32], &ch);
        // The TargetName fields point inside the message at the UTF-16 name.
        let (nlen, noff) = (r16(msg, 12) as usize, r32(msg, 16) as usize);
        assert_eq!(nlen, TARGET_NAME.len() * 2);
        assert!(noff + nlen <= n);
        assert_eq!(msg[noff], b'N');
        // The TargetInfo fields point inside the message and end with the EOL pair.
        let (ilen, ioff) = (r16(msg, 40) as usize, r32(msg, 44) as usize);
        assert!(ioff + ilen == n, "target info must be the final payload");
        assert_eq!(r32(msg, (ioff + ilen) - 4), 0, "MsvAvEOL terminates it");
        let _ = r64(msg, 24); // silence: challenge read as one word elsewhere
    }

    #[test]
    fn parse_classifies_the_three_message_types_by_their_wire_type_field() {
        let mut m = [0u8; 64];
        m[..8].copy_from_slice(&SIGNATURE);
        w32(&mut m, 8, NEGOTIATE);
        assert_eq!(parse(&m), Some(Message::Negotiate));
        w32(&mut m, 8, AUTHENTICATE);
        assert!(matches!(parse(&m), Some(Message::Authenticate(_))));
        // A CHALLENGE is a server's message; a client sending one is nonsense we refuse.
        w32(&mut m, 8, CHALLENGE);
        assert_eq!(parse(&m), None);
        m[0] = b'X';
        w32(&mut m, 8, NEGOTIATE);
        assert_eq!(parse(&m), None, "a wrong signature is not NTLMSSP");
        assert_eq!(parse(&m[..8]), None, "too short to carry a type");
    }

    /// **A message labelled AUTHENTICATE that cannot hold the fields is not one.** §2.2.1.3's
    /// fixed part is 64 bytes, and a shorter message would have `parse` reading the flags out of
    /// somebody else's bytes; the field triples themselves are bounds-checked, but the flags word
    /// is at a fixed offset and needs the length check to be safe.
    #[test]
    fn an_authenticate_too_short_for_its_own_fixed_fields_is_refused() {
        let mut m = [0u8; 64];
        m[..8].copy_from_slice(&SIGNATURE);
        w32(&mut m, 8, AUTHENTICATE);
        assert!(matches!(parse(&m), Some(Message::Authenticate(_))));
        assert_eq!(parse(&m[..63]), None, "63 bytes cannot hold the flags word");
    }

    /// **The field triples resolve to their payloads**, which is the arithmetic this module exists
    /// to hold in one place. Built with the client-side builder rather than transcribed, and then
    /// asserted against the *payloads* [MS-NLMP] §4.2.4 publishes, which is the honest split: the
    /// container is ours, the bytes inside it are Microsoft's.
    #[test]
    fn an_authenticate_carries_the_names_and_the_nt_response_it_was_built_with() {
        // §4.2.4.1.3's NTProofStr and §4.2.4.1.3's `temp`, the two values the `ntlm` crate's own
        // tests pin against the specification. Repeated here as the payload this parse must find,
        // not as arithmetic: nothing in this crate computes them.
        let proof: [u8; PROOF_LEN] = [
            0x68, 0xcd, 0x0a, 0xb8, 0x51, 0xe5, 0x1c, 0x96, 0xaa, 0xbc, 0x92, 0x7b, 0xeb, 0xef,
            0x6a, 0x1c,
        ];
        let blob = [0x01u8, 0x01, 0x00, 0x00, 0xaa, 0xaa, 0xaa, 0xaa];
        let token = crate::client::ntlmssp_authenticate(b"User", b"Domain", &proof, &blob);

        let Some(Message::Authenticate(a)) = parse(&token) else {
            panic!("the builder's own AUTHENTICATE did not parse as one");
        };
        let mut want = [0u8; 32];
        let n = ascii_to_utf16le(b"User", &mut want);
        assert_eq!(a.user, &want[..n], "UserName");
        let n = ascii_to_utf16le(b"Domain", &mut want);
        assert_eq!(a.domain, &want[..n], "DomainName");
        let (got_proof, got_blob) = a.proof().expect("an NT response was built in");
        assert_eq!(got_proof, &proof);
        assert_eq!(got_blob, &blob);
    }

    /// **A guest-shaped AUTHENTICATE has no proof**, and that is what `proof()` must say. The tree's
    /// own prober and `mount_smbfs -N` both send this: every field pair empty. A share with an
    /// authenticator has to be able to tell it from a failed verification, so this is the assertion
    /// that keeps `Verdict::Refused` from being reached without any arithmetic happening.
    #[test]
    fn a_guest_shaped_authenticate_carries_no_proof_at_all() {
        let token = crate::client::ntlmssp_authenticate_empty();
        let Some(Message::Authenticate(a)) = parse(&token) else {
            panic!("an empty AUTHENTICATE is still an AUTHENTICATE");
        };
        assert!(a.user.is_empty());
        assert!(a.nt_response.is_empty());
        assert_eq!(a.proof(), None);
    }

    /// **A field pointing off the end of the message resolves to nothing**, rather than panicking
    /// or reading whatever follows. The interesting case is a length that is plausible and an
    /// offset that is not: a client can write any pair of numbers there, and this is the one place
    /// they are trusted enough to index with.
    #[test]
    fn a_field_that_runs_off_the_message_is_empty_rather_than_a_panic() {
        let mut m = [0u8; 80];
        m[..8].copy_from_slice(&SIGNATURE);
        w32(&mut m, 8, AUTHENTICATE);
        // UserName: 16 bytes at offset 0xFFF0.
        w16(&mut m, 36, 16);
        w32(&mut m, 40, 0xFFF0);
        // NtChallengeResponse: a length past the end from a legal offset.
        w16(&mut m, 20, 4096);
        w32(&mut m, 24, 64);
        let Some(Message::Authenticate(a)) = parse(&m) else {
            panic!("a well-formed header with nonsense fields is still an AUTHENTICATE");
        };
        assert!(a.user.is_empty(), "an offset past the end resolves to none");
        assert!(a.nt_response.is_empty(), "a length past the end likewise");
        assert_eq!(a.proof(), None);
    }
}
