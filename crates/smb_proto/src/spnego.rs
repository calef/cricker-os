//! **The minimal SPNEGO (RFC 4178) this server needs**, which is: find the NTLMSSP token inside
//! what a client sent, and wrap our NTLMSSP answers the way GSS-API clients expect.
//!
//! macOS does not put raw NTLMSSP in the session-setup security buffer; it wraps it in SPNEGO's
//! DER (a `NegTokenInit` carrying the first token, `NegTokenResp` for the rest). This module is a DER
//! *walker*, not a DER library: it understands exactly the tags on the path to the token and
//! refuses everything else, which is the right size for a parser facing the network (the less it
//! accepts, the less there is to be wrong about). Raw NTLMSSP still passes through one level up,
//! so a hand-rolled client (xtask's prober, the host tests) can skip SPNEGO entirely.

use crate::ntlmssp;

/// The SPNEGO mechanism OID, 1.3.6.1.5.5.2, DER-encoded.
const SPNEGO_OID: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x02];
/// The NTLMSSP mechanism OID, 1.3.6.1.4.1.311.2.2.10, DER-encoded.
const NTLMSSP_OID: &[u8] = &[0x2b, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37, 0x02, 0x02, 0x0a];

/// One DER TLV: its tag, its content, and how many bytes it spanned. `None` for anything
/// malformed, including the long length forms this module never emits and (beyond two bytes)
/// never needs to read: SMB2 security buffers are bounded well under 64 KiB.
fn tlv(b: &[u8]) -> Option<(u8, &[u8], usize)> {
    let tag = *b.first()?;
    let l0 = *b.get(1)? as usize;
    let (len, hdr) = match l0 {
        0..=0x7f => (l0, 2),
        0x81 => (*b.get(2)? as usize, 3),
        0x82 => (((*b.get(2)? as usize) << 8) | *b.get(3)? as usize, 4),
        _ => return None,
    };
    let content = b.get(hdr..hdr + len)?;
    Some((tag, content, hdr + len))
}

/// The content of the TLV at the front of `b`, if its tag is `want`.
fn expect(b: &[u8], want: u8) -> Option<&[u8]> {
    let (tag, content, _) = tlv(b)?;
    (tag == want).then_some(content)
}

/// Walk a SEQUENCE's children looking for context tag `want`, skipping earlier fields. RFC 4178's
/// token bodies are SEQUENCE bodies of optional context-tagged fields in ascending order, so a linear
/// scan that ignores what it does not need is exactly the grammar.
fn field(mut seq: &[u8], want: u8) -> Option<&[u8]> {
    while !seq.is_empty() {
        let (tag, content, used) = tlv(seq)?;
        if tag == want {
            return Some(content);
        }
        seq = &seq[used..];
    }
    None
}

/// Dig the NTLMSSP token out of a session-setup security buffer, whatever it is wrapped in:
/// nothing (raw NTLMSSP), a `NegTokenInit` (`0x60` application wrapper, first leg), or a
/// `NegTokenResp` (`0xa1`, later legs). `None` means there is no NTLMSSP here and the setup fails.
pub fn unwrap_token(buf: &[u8]) -> Option<&[u8]> {
    if buf.len() >= 8 && buf[..8] == ntlmssp::SIGNATURE {
        return Some(buf);
    }
    match buf.first()? {
        // NegTokenInit: 60 { OID(SPNEGO) a0 { SEQ { a0 mechTypes ... a2 { OCTETS(token) } } } }
        0x60 => {
            let app = expect(buf, 0x60)?;
            let (tag, oid, used) = tlv(app)?;
            if tag != 0x06 || oid != SPNEGO_OID {
                return None;
            }
            let init = expect(&app[used..], 0xa0)?;
            let seq = expect(init, 0x30)?;
            let mech_token = field(seq, 0xa2)?;
            expect(mech_token, 0x04)
        }
        // NegTokenResp: a1 { SEQ { [a0 negState] [a1 supportedMech] a2 { OCTETS(token) } } }
        0xa1 => {
            let resp = expect(buf, 0xa1)?;
            let seq = expect(resp, 0x30)?;
            let response_token = field(seq, 0xa2)?;
            expect(response_token, 0x04)
        }
        _ => None,
    }
}

/// Write tag+length+content into `out` at `at`, returning the bytes written. Minimal-length DER,
/// as DER requires; two length bytes cover everything this crate builds.
fn wrap(out: &mut [u8], at: usize, tag: u8, content_len: usize) -> usize {
    out[at] = tag;
    match content_len {
        0..=0x7f => {
            out[at + 1] = content_len as u8;
            2
        }
        0x80..=0xff => {
            out[at + 1] = 0x81;
            out[at + 2] = content_len as u8;
            3
        }
        _ => {
            out[at + 1] = 0x82;
            out[at + 2] = (content_len >> 8) as u8;
            out[at + 3] = content_len as u8;
            4
        }
    }
}

/// negState values (RFC 4178 §4.2.2).
const ACCEPT_COMPLETED: u8 = 0;
const ACCEPT_INCOMPLETE: u8 = 1;

/// A `NegTokenResp` carrying `token` (our NTLMSSP CHALLENGE) with negState accept-incomplete and
/// supportedMech NTLMSSP, which is the reply the first SPNEGO leg expects. Returns the length.
pub fn build_challenge_resp(out: &mut [u8], token: &[u8]) -> usize {
    // Inner fields, sized before writing (DER lengths precede content).
    let neg_state_len = 2 + 3; // a0 03 { 0a 01 01 }
    let mech_len = 2 + 2 + NTLMSSP_OID.len(); // a1 0c { 06 0a oid }
    let octets_hdr = wrap_len(token.len());
    let resp_tok_len = wrap_len(octets_hdr + token.len()) - 2 + 2 + octets_hdr + token.len();
    // ^ a2 { 04 { token } }: content is octets_hdr + token, plus a2's own header
    let seq_content = neg_state_len + mech_len + resp_tok_len;
    let seq_len = wrap_len(seq_content) + seq_content;

    let mut p = 0;
    p += wrap(out, p, 0xa1, seq_len);
    p += wrap(out, p, 0x30, seq_content);
    // negState: a0 { ENUMERATED accept-incomplete }
    p += wrap(out, p, 0xa0, 3);
    out[p] = 0x0a;
    out[p + 1] = 1;
    out[p + 2] = ACCEPT_INCOMPLETE;
    p += 3;
    // supportedMech: a1 { OID NTLMSSP }
    p += wrap(out, p, 0xa1, 2 + NTLMSSP_OID.len());
    p += wrap(out, p, 0x06, NTLMSSP_OID.len());
    out[p..p + NTLMSSP_OID.len()].copy_from_slice(NTLMSSP_OID);
    p += NTLMSSP_OID.len();
    // responseToken: a2 { OCTET STRING token }
    p += wrap(out, p, 0xa2, octets_hdr + token.len());
    p += wrap(out, p, 0x04, token.len());
    out[p..p + token.len()].copy_from_slice(token);
    p + token.len()
}

/// How many bytes [`wrap`] spends on a tag and length for `content_len` bytes of content.
const fn wrap_len(content_len: usize) -> usize {
    match content_len {
        0..=0x7f => 2,
        0x80..=0xff => 3,
        _ => 4,
    }
}

/// The final `NegTokenResp`: negState accept-completed, no token. Nine bytes, fixed.
pub const ACCEPT_COMPLETED_RESP: [u8; 9] =
    [0xa1, 0x07, 0x30, 0x05, 0xa0, 0x03, 0x0a, 0x01, ACCEPT_COMPLETED];

/// The `NegTokenInit2` hint a server may put in its NEGOTIATE response's security buffer, saying "I
/// speak NTLMSSP". Advisory: clients that ignore it (or get an empty buffer) try NTLMSSP anyway,
/// but sending it is what every mainstream server does and costs 40 bytes. Fixed content, built
/// once here rather than hand-transcribed, so the OIDs cannot drift from the parser's.
pub fn build_negotiate_hint(out: &mut [u8]) -> usize {
    // mechTypes: a0 { 30 { 06 oid } }
    let oid_tlv = 2 + NTLMSSP_OID.len();
    let mech_seq = wrap_len(oid_tlv) + oid_tlv;
    let mech_types = wrap_len(mech_seq) + mech_seq;
    let inner_seq = wrap_len(mech_types) + mech_types;
    let init = wrap_len(inner_seq) + inner_seq;
    let spnego_oid_tlv = 2 + SPNEGO_OID.len();

    let mut p = 0;
    p += wrap(out, p, 0x60, spnego_oid_tlv + init);
    p += wrap(out, p, 0x06, SPNEGO_OID.len());
    out[p..p + SPNEGO_OID.len()].copy_from_slice(SPNEGO_OID);
    p += SPNEGO_OID.len();
    p += wrap(out, p, 0xa0, inner_seq);
    p += wrap(out, p, 0x30, mech_types);
    p += wrap(out, p, 0xa0, mech_seq);
    p += wrap(out, p, 0x30, oid_tlv);
    p += wrap(out, p, 0x06, NTLMSSP_OID.len());
    out[p..p + NTLMSSP_OID.len()].copy_from_slice(NTLMSSP_OID);
    p + NTLMSSP_OID.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `NegTokenInit` as macOS sends it (mechTypes listing NTLMSSP, then the mechToken), built by
    /// hand here so the parser is tested against the *client's* shape, not against our builder.
    fn neg_token_init(token: &[u8]) -> Vec<u8> {
        let mut mech_token = vec![0x04, token.len() as u8];
        mech_token.extend_from_slice(token);
        let mut a2 = vec![0xa2, mech_token.len() as u8];
        a2.extend(mech_token);
        let mut oid = vec![0x06, NTLMSSP_OID.len() as u8];
        oid.extend_from_slice(NTLMSSP_OID);
        let mut mech_seq = vec![0x30, oid.len() as u8];
        mech_seq.extend(oid);
        let mut a0 = vec![0xa0, mech_seq.len() as u8];
        a0.extend(mech_seq);
        let mut seq = vec![0x30, (a0.len() + a2.len()) as u8];
        seq.extend(a0);
        seq.extend(a2);
        let mut init = vec![0xa0, seq.len() as u8];
        init.extend(seq);
        let mut body = vec![0x06, SPNEGO_OID.len() as u8];
        body.extend_from_slice(SPNEGO_OID);
        body.extend(init);
        let mut out = vec![0x60, body.len() as u8];
        out.extend(body);
        out
    }

    fn fake_ntlmssp(msg_type: u32) -> Vec<u8> {
        let mut t = ntlmssp::SIGNATURE.to_vec();
        t.extend_from_slice(&msg_type.to_le_bytes());
        t.extend_from_slice(&[0u8; 20]);
        t
    }

    #[test]
    fn the_token_is_found_inside_a_neg_token_init() {
        let inner = fake_ntlmssp(ntlmssp::NEGOTIATE);
        let wrapped = neg_token_init(&inner);
        assert_eq!(unwrap_token(&wrapped), Some(inner.as_slice()));
    }

    #[test]
    fn the_token_is_found_inside_a_neg_token_resp_past_the_fields_before_it() {
        let inner = fake_ntlmssp(ntlmssp::AUTHENTICATE);
        // a1 { 30 { a0 negState, a2 { 04 token } } }: the negState before it must be skipped.
        let mut a2_content = vec![0x04, inner.len() as u8];
        a2_content.extend_from_slice(&inner);
        let mut seq = vec![0xa0, 0x03, 0x0a, 0x01, 0x01];
        seq.push(0xa2);
        seq.push(a2_content.len() as u8);
        seq.extend(a2_content);
        let mut resp = vec![0xa1, (seq.len() + 2) as u8, 0x30, seq.len() as u8];
        resp.extend(seq);
        assert_eq!(unwrap_token(&resp), Some(inner.as_slice()));
    }

    #[test]
    fn raw_ntlmssp_passes_straight_through() {
        let raw = fake_ntlmssp(ntlmssp::NEGOTIATE);
        assert_eq!(unwrap_token(&raw), Some(raw.as_slice()));
    }

    #[test]
    fn garbage_and_wrong_mechanisms_yield_nothing() {
        assert_eq!(unwrap_token(&[]), None);
        assert_eq!(unwrap_token(&[0x60, 0x03, 0x06, 0x01, 0x55]), None); // not SPNEGO's OID
        // A NegTokenInit whose mechToken is missing entirely.
        let mut body = vec![0x06, SPNEGO_OID.len() as u8];
        body.extend_from_slice(SPNEGO_OID);
        body.extend_from_slice(&[0xa0, 0x04, 0x30, 0x02, 0xa0, 0x00]);
        let mut msg = vec![0x60, body.len() as u8];
        msg.extend(body);
        assert_eq!(unwrap_token(&msg), None);
        // A truncated length announcement.
        assert_eq!(unwrap_token(&[0xa1, 0x82, 0x0f]), None);
    }

    #[test]
    fn our_challenge_resp_round_trips_through_our_own_unwrapper() {
        // The builder and the parser must agree, and this is the cheapest place to prove the
        // builder's DER is well-formed: our own strict walker refuses malformed DER.
        let mut challenge = [0u8; ntlmssp::CHALLENGE_MAX];
        let n = ntlmssp::build_challenge(&mut challenge, &[9, 8, 7, 6, 5, 4, 3, 2]);
        let mut wrapped = [0u8; 512];
        let m = build_challenge_resp(&mut wrapped, &challenge[..n]);
        assert_eq!(unwrap_token(&wrapped[..m]), Some(&challenge[..n]));
    }

    #[test]
    fn the_negotiate_hint_is_wellformed_spnego() {
        let mut buf = [0u8; 64];
        let n = build_negotiate_hint(&mut buf);
        // It carries no token, so unwrap finds none; what must hold is the DER structure.
        let (tag, content, used) = tlv(&buf[..n]).unwrap();
        assert_eq!(tag, 0x60);
        assert_eq!(used, n);
        assert_eq!(expect(content, 0x06), Some(SPNEGO_OID));
    }

    #[test]
    fn a_big_token_takes_the_two_byte_length_form_and_still_round_trips() {
        let mut token = fake_ntlmssp(ntlmssp::CHALLENGE);
        token.resize(300, 0xAB); // force 0x82 length encodings
        let mut wrapped = [0u8; 512];
        let m = build_challenge_resp(&mut wrapped, &token);
        assert_eq!(unwrap_token(&wrapped[..m]), Some(token.as_slice()));
    }
}
