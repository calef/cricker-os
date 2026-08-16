//! **NTLMSSP messages** ([MS-NLMP] §2.2): the three-message dance SMB2 session setup carries.
//!
//! This module is the *message framing* only. The arithmetic (`NTOWFv2`, proofs, session keys) lives
//! in the `ntlm` crate, and this server does not call it: sessions are guest, nothing is verified,
//! and the CHALLENGE this module builds exists so that a client following the protocol can finish
//! it (macOS will not proceed to AUTHENTICATE without a well-formed CHALLENGE carrying target
//! info). When identity arrives (milestone 65's `cred` service holds the key and the operation),
//! the proof check slots in where [`parse`] returns [`Message::Authenticate`], and nothing on the
//! wire moves.

use crate::{ascii_to_utf16le, r32, w16, w32};

/// `"NTLMSSP\0"`, the signature every NTLMSSP message opens with.
pub const SIGNATURE: [u8; 8] = *b"NTLMSSP\0";

/// Message types (the u32 after the signature).
pub const NEGOTIATE: u32 = 1;
pub const CHALLENGE: u32 = 2;
pub const AUTHENTICATE: u32 = 3;

/// What arrived in a session-setup security buffer, from NTLMSSP's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    /// Type 1: the client wants a CHALLENGE.
    Negotiate,
    /// Type 3: the client answered the challenge. Guest scope: accepted without inspection.
    Authenticate,
}

/// Classify an NTLMSSP message, or `None` if these bytes are not one.
pub fn parse(token: &[u8]) -> Option<Message> {
    if token.len() < 12 || token[..8] != SIGNATURE {
        return None;
    }
    match r32(token, 8) {
        NEGOTIATE => Some(Message::Negotiate),
        AUTHENTICATE => Some(Message::Authenticate),
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
        let mut m = [0u8; 16];
        m[..8].copy_from_slice(&SIGNATURE);
        w32(&mut m, 8, NEGOTIATE);
        assert_eq!(parse(&m), Some(Message::Negotiate));
        w32(&mut m, 8, AUTHENTICATE);
        assert_eq!(parse(&m), Some(Message::Authenticate));
        // A CHALLENGE is a server's message; a client sending one is nonsense we refuse.
        w32(&mut m, 8, CHALLENGE);
        assert_eq!(parse(&m), None);
        m[0] = b'X';
        w32(&mut m, 8, NEGOTIATE);
        assert_eq!(parse(&m), None, "a wrong signature is not NTLMSSP");
        assert_eq!(parse(&m[..8]), None, "too short to carry a type");
    }
}
