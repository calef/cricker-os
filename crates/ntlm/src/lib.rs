//! **NTLMv2's keyed operations** (milestone 65; notes/ntlm.md).
//!
//! The arithmetic behind the secrets service's NTLM operation, and nothing else: no syscalls, no
//! allocation, no `alloc`, so it compiles for the host and for both bare-metal targets from one
//! source and its tests run in milliseconds. The store that holds the key is `cred`; the wire
//! contract is `cred_proto`; the service is `user/src/credentialer.rs`.
//!
//! Name: unrecorded. Provisional, minted by milestone 65's lane on 2026-08-04 and not yet put to
//! calef. It is a standard term of art, in the family the naming tenet says are already right
//! (`elf`, `pci`, `dtb`, `gpt`): a reader who knows what NTLM is needs no decoder, and respelling
//! it would cost the recognition the tenet exists to buy. The narrower question is whether the
//! crate should say *which* NTLM it implements, since it is v2 only and deliberately holds no v1
//! path. See notes/naming.md and notes/ntlm.md.
//!
//! # Examples
//!
//! The whole server side of an NTLMv2 authentication, on [MS-NLMP] §4.2.4's published example, which
//! is the vector this crate's *wiring* is pinned against rather than the libraries' arithmetic.
//! Note that the presented password never appears: the server holds a key and compares a MAC.
//!
//! ```
//! use ntlm::{CHALLENGE_LEN, ntowfv2, proof, session_base_key};
//!
//! // Provisioning time. `NTOWFv2` is what the store holds, and the account name and the domain are
//! // inputs to it, which is why they are provisioning fields rather than request fields.
//! let key = ntowfv2(b"Password", b"User", b"Domain").unwrap();
//!
//! // Authentication time. The challenge is the server's, sent in its CHALLENGE_MESSAGE.
//! let challenge: [u8; CHALLENGE_LEN] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
//!
//! // The blob is the client's `temp` and is entirely public, so nothing here validates it: a blob
//! // that is nonsense produces a proof that does not match, which is the correct outcome.
//! # #[rustfmt::skip]
//! let blob: [u8; 68] = [
//!     0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Responserversion, Z(6)
//!     0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Time (zero in the example)
//!     0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, // ClientChallenge
//!     0x00, 0x00, 0x00, 0x00,
//!     0x02, 0x00, 0x0c, 0x00, // target info: MsvAvNbDomainName = "Domain"
//!     0x44, 0x00, 0x6f, 0x00, 0x6d, 0x00, 0x61, 0x00, 0x69, 0x00, 0x6e, 0x00,
//!     0x01, 0x00, 0x0c, 0x00, // MsvAvNbComputerName = "Server"
//!     0x53, 0x00, 0x65, 0x00, 0x72, 0x00, 0x76, 0x00, 0x65, 0x00, 0x72, 0x00,
//!     0x00, 0x00, 0x00, 0x00, // MsvAvEOL
//!     0x00, 0x00, 0x00, 0x00,
//! ];
//!
//! // What the client sent, and what the server computes to compare against it.
//! let expected = proof(&key, &challenge, &blob);
//! assert_eq!(&expected[..4], &[0x68, 0xcd, 0x0a, 0xb8]);
//!
//! // A wrong password gives a proof that does not match, which is the whole verification.
//! let wrong = ntowfv2(b"password", b"User", b"Domain").unwrap();
//! assert_ne!(proof(&wrong, &challenge, &blob), expected);
//!
//! // And the one derived value the service lets out, only against a proof that verified. It is a
//! // function of both challenges, so a key released for this session signs nothing else.
//! let session = session_base_key(&key, &expected);
//! assert_eq!(&session[..4], &[0x8d, 0xe4, 0x0c, 0xca]);
//! assert_ne!(session, key); // recovering the key from it would be an HMAC key recovery
//! ```
//!
//! One asymmetry in [`ntowfv2`] is the specification's and is the kind of detail a reimplementation
//! gets wrong silently, so it is worth a line of its own: **the user is uppercased and the domain is
//! not.**
//!
//! ```
//! use ntlm::ntowfv2;
//!
//! let key = ntowfv2(b"Password", b"User", b"Domain").unwrap();
//!
//! // The user's case cannot matter, because it is folded away.
//! assert_eq!(ntowfv2(b"Password", b"user", b"Domain").unwrap(), key);
//!
//! // The domain's case does matter, and a "tidier" implementation that uppercased both would fail
//! // against every real Windows client.
//! assert_ne!(ntowfv2(b"Password", b"User", b"DOMAIN").unwrap(), key);
//! ```
//!
//! # Why this exists as a separate thing from a password verifier
//!
//! **NTLMv2 does not verify a presented secret.** The client never sends the password, and the
//! server never receives anything it can compare against a stored tag. The server holds a key,
//! computes a MAC over a challenge it chose, and compares that to the MAC the client sent. So the
//! NT hash is not a verifier, it is **a key the server computes with**, and the shape of
//! `cred_proto::verify::VERIFY` (secret in, boolean out) does not fit it.
//!
//! That is the whole reason milestone 65 exists as a milestone. The principle is unchanged, though:
//! *hand out the operation, not the secret.* Only the operation was wrong.
//!
//! # The chain, in the order [MS-NLMP] §3.3.2 computes it
//!
//! ```text
//!   password ──MD4(UTF-16LE)──► NT hash ──HMAC-MD5 over UPPER(user)||domain──► NTOWFv2
//!                                                                                │
//!   server challenge (8) ┐                                                       │
//!   the client's blob    ├──────────────HMAC-MD5 keyed by NTOWFv2────────────────┤
//!                        ┘                                                       ▼
//!                                                                          NTProofStr (16)
//!                                                                                │
//!                                    HMAC-MD5 keyed by NTOWFv2 over NTProofStr ──┤
//!                                                                                ▼
//!                                                                     SessionBaseKey (16)
//! ```
//!
//! The service stores **`NTOWFv2`**, not the NT hash, and [`ntowfv2`] is the only way to make one.
//! Two consequences worth having:
//!
//! 1. The account name and the domain are bound at provisioning time, so a caller of the runtime
//!    operation cannot choose them. A caller that could would be choosing half the key derivation,
//!    which is how "compute a response for `Administrator`" becomes a request the service would
//!    otherwise happily serve.
//! 2. A stolen `NTOWFv2` authenticates as one account in one domain, where a stolen NT hash
//!    authenticates as that account anywhere the password was reused.
//!
//! # BUGS
//!
//! - **MD4 and MD5 are broken and are here for wire compatibility.** Nothing in this crate is a
//!   security choice; NTLMv2 specifies these functions. Keep them out of anything that is not
//!   NTLM. The blast radius is stated in notes/ntlm.md.
//! - **The uppercasing is ASCII-only.** [MS-NLMP] §3.3.2 says `Uppercase(User)`, and Windows means
//!   its own locale-aware uppercasing. Doing that properly needs Unicode case tables, which this
//!   tree does not carry and would not carry for one protocol. An account name outside ASCII will
//!   therefore derive a different key here than on Windows and will fail to authenticate. It is a
//!   wrong answer rather than a crash, and it is why the limitation is named here instead of only
//!   in a tracker.
//! - **Nothing in this crate is constant-time.** MD4, MD5 and HMAC are not, and do not need to be:
//!   every input to them here is either public (the challenge, the blob) or a key whose *bytes*
//!   never branch. The one comparison that must be constant-time is the proof comparison, and it
//!   lives in `cred`, next to the store, where `subtle` already is.
//! - **No `zeroize`.** Intermediate key material (the NT hash inside [`ntowfv2`]) lives on the
//!   caller's stack until that frame is reused. The service is the only process that runs this
//!   code and its whole address space is the secret's blast radius already, so scrubbing one local
//!   would be a gesture rather than a boundary.

#![cfg_attr(not(test), no_std)]

use digest::{Digest, Update};
use hmac::{Hmac, Mac};
use md4::Md4;
use md5::Md5;

/// The width of every key and every output in NTLMv2: 16 bytes. MD4, MD5 and HMAC-MD5 all produce
/// 128 bits, so the NT hash, `NTOWFv2`, `NTProofStr` and `SessionBaseKey` are all this wide. One
/// constant rather than four, because four would invite a mismatch nothing checks.
pub const KEY_LEN: usize = 16;

/// The server challenge's width, in bytes. Fixed by the protocol at 8; it is the `ServerChallenge`
/// field of the `CHALLENGE_MESSAGE` ([MS-NLMP] §2.2.1.2).
pub const CHALLENGE_LEN: usize = 8;

/// **What went wrong with the question.** Both variants are caller bugs rather than authentication
/// outcomes: a name that is not UTF-8 cannot be re-encoded as UTF-16LE, which is the one input
/// conversion in the chain that can fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The password is not valid UTF-8, so there is no UTF-16LE encoding of it to hash.
    Password,
    /// The account name or the domain is not valid UTF-8.
    Name,
}

/// **The NT hash**: `MD4(UTF-16LE(password))`, [MS-NLMP] §3.3.2's `NTOWFv1` without the v1 wrapper.
///
/// Public because the published test vectors pin it and a vector nobody can run is not a test. It
/// is not what the store holds: [`ntowfv2`] is, and the difference is in this module's header.
///
/// The encoding is streamed a code unit at a time rather than built into a buffer, which is not a
/// micro-optimisation: a fixed buffer would need a bound on the UTF-16 length of a UTF-8 string,
/// and getting that bound wrong is a truncation that produces a plausible wrong hash.
pub fn nt_hash(password: &[u8]) -> Result<[u8; KEY_LEN], Error> {
    let s = core::str::from_utf8(password).map_err(|_| Error::Password)?;
    let mut d = Md4::new();
    utf16le(&mut d, s, false);
    Ok(d.finalize().into())
}

/// **`NTOWFv2`**: `HMAC-MD5(NT hash, UTF-16LE(Uppercase(user) || domain))`, [MS-NLMP] §3.3.2.
///
/// This is the value the secrets service stores, and the reason the account name and the domain
/// are provisioning inputs rather than request fields. The domain is **not** uppercased; only the
/// user is. That asymmetry is the specification's, not a mistake here, and it is the kind of
/// detail a reimplementation gets wrong silently, so the published vector below pins it.
pub fn ntowfv2(password: &[u8], user: &[u8], domain: &[u8]) -> Result<[u8; KEY_LEN], Error> {
    let user = core::str::from_utf8(user).map_err(|_| Error::Name)?;
    let domain = core::str::from_utf8(domain).map_err(|_| Error::Name)?;
    let nt = nt_hash(password)?;
    let mut mac = mac(&nt);
    utf16le(&mut mac, user, true);
    utf16le(&mut mac, domain, false);
    Ok(mac.finalize().into_bytes().into())
}

/// **`NTProofStr`**: `HMAC-MD5(NTOWFv2, server_challenge || blob)`, [MS-NLMP] §3.3.2.
///
/// `blob` is the client's `temp`: the response version bytes, the timestamp, the client challenge,
/// and the target info the client echoed back. It is entirely the client's and entirely public, so
/// this function does no validation of it at all; a blob that is nonsense produces a proof that
/// does not match, which is the correct outcome and needs no separate check.
///
/// The full `NtChallengeResponse` on the wire is `NTProofStr || blob`, so a server that has the
/// wire field already has both halves and never needs this crate to concatenate them.
pub fn proof(key: &[u8; KEY_LEN], challenge: &[u8; CHALLENGE_LEN], blob: &[u8]) -> [u8; KEY_LEN] {
    let mut mac = mac(key);
    Update::update(&mut mac, challenge);
    Update::update(&mut mac, blob);
    mac.finalize().into_bytes().into()
}

/// **`SessionBaseKey`**: `HMAC-MD5(NTOWFv2, NTProofStr)`, [MS-NLMP] §3.3.2.
///
/// This is the one value derived from the key that the secrets service lets out, and it is let out
/// only against a proof that verified. It is per-session: it is a function of the server challenge
/// and of a client challenge inside the blob, so a key released for one authentication signs
/// nothing else. Recovering `NTOWFv2` from it would be an HMAC key recovery, which is why handing
/// it over is not handing over the secret. See notes/ntlm.md for the full argument and its limits.
pub fn session_base_key(key: &[u8; KEY_LEN], proof: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    let mut mac = mac(key);
    Update::update(&mut mac, proof);
    mac.finalize().into_bytes().into()
}

/// HMAC-MD5 under `key`. `new_from_slice` cannot fail for HMAC (any key length is legal; it is
/// hashed down or zero-padded), so the `expect` is unreachable rather than optimistic.
fn mac(key: &[u8; KEY_LEN]) -> Hmac<Md5> {
    Hmac::<Md5>::new_from_slice(key).expect("HMAC accepts a key of any length")
}

/// Feed `s` as UTF-16LE, one code unit at a time, ASCII-uppercasing first if asked.
///
/// See this module's BUGS on why the uppercasing is ASCII-only.
fn utf16le<D: Update>(d: &mut D, s: &str, upper: bool) {
    let mut units = [0u16; 2];
    for c in s.chars() {
        let c = if upper { c.to_ascii_uppercase() } else { c };
        for u in c.encode_utf16(&mut units) {
            d.update(&u.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex16(s: &str) -> [u8; KEY_LEN] {
        let b: Vec<u8> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect();
        b.try_into().unwrap()
    }

    // -------------------------------------------------------------------------------------------
    // The dependencies' own answers, checked against the specifications that publish them.
    //
    // **This is the entire argument for depending on `md4`, `md-5` and `hmac` rather than writing
    // them** (DECISIONS §46), and it is the same discipline `cred` applies to `argon2`: a
    // dependency whose answers we never check is a dependency we have merely hoped about. These
    // run against the exact versions this tree links, so a bump that changes an answer fails here.
    // -------------------------------------------------------------------------------------------

    /// **RFC 1320's MD4 test suite**, in full. Seven vectors is the whole published set and they
    /// are three lines, so there is no reason to sample it.
    ///
    /// The first is also the most recognisable value in this whole area: `MD4("")` is the NT hash
    /// of the empty password, which is what a Windows account with no password stores.
    #[test]
    fn rfc_1320_md4_vectors() {
        for (input, expected) in [
            ("", "31d6cfe0d16ae931b73c59d7e0c089c0"),
            ("a", "bde52cb31de33e46245e05fbdbd6fb24"),
            ("abc", "a448017aaf21d8525fc10ae87aa6729d"),
            ("message digest", "d9130a8164549fe818874806e1c7014b"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "d79e1c308aa5bbcdeea8ed63df412da9",
            ),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "043f8582f241db351ce627e153e7f0e4",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "e33b4ddc9c38f2199c3e7b164fcc0536",
            ),
        ] {
            let mut d = Md4::new();
            Digest::update(&mut d, input.as_bytes());
            let out: [u8; KEY_LEN] = d.finalize().into();
            assert_eq!(out, hex16(expected), "MD4({input:?})");
        }
    }

    /// **RFC 2202 §2's HMAC-MD5 vectors**, the three that use a key this crate's shape can hold
    /// (16 bytes or shorter). The longer-key cases exercise HMAC's key-hashing path, which NTLM
    /// never reaches: every key here is exactly [`KEY_LEN`].
    #[test]
    fn rfc_2202_hmac_md5_vectors() {
        for (key, data, expected) in [
            (
                vec![0x0b; 16],
                b"Hi There".to_vec(),
                "9294727a3638bb1c13f48ef8158bfc9d",
            ),
            (
                b"Jefe".to_vec(),
                b"what do ya want for nothing?".to_vec(),
                "750c783e6ab0b503eaa86e310a5db738",
            ),
            (
                vec![0xaa; 16],
                vec![0xdd; 50],
                "56be34521d144c88dbb8c733f0e8b3f6",
            ),
        ] {
            let mut m = Hmac::<Md5>::new_from_slice(&key).unwrap();
            Update::update(&mut m, &data);
            let out: [u8; KEY_LEN] = m.finalize().into_bytes().into();
            assert_eq!(out, hex16(expected));
        }
    }

    // -------------------------------------------------------------------------------------------
    // [MS-NLMP] §4.2.4: the NTLMv2 authentication example. Microsoft publishes every intermediate
    // value in the chain, which is unusually generous and makes this the one test that pins our
    // *wiring* rather than the libraries' arithmetic: the UTF-16LE encoding, which of the two
    // names is uppercased, the order of the challenge and the blob, and the key each HMAC is under.
    // -------------------------------------------------------------------------------------------

    const USER: &[u8] = b"User";
    const DOMAIN: &[u8] = b"Domain";
    const PASSWORD: &[u8] = b"Password";
    /// [MS-NLMP] §4.2.1: `ServerChallenge` = `0123456789abcdef`.
    const SERVER_CHALLENGE: [u8; CHALLENGE_LEN] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];

    /// §4.2.4.1.3's `temp`: the response version, six zero bytes, an all-zero timestamp, the
    /// all-`0xaa` client challenge, four zeros, the target info (`Domain` and `Server` as AV
    /// pairs), and four more zeros. Written out as the specification prints it, because the
    /// interesting failure is a byte in the wrong place and a constructed one would hide that.
    /// **68 bytes, and the length is load-bearing.** A first draft of this array carried four
    /// extra trailing zeros and every published value up to `NTOWFv2` still matched, because the
    /// blob does not enter the chain until the proof. The machine caught it there. That is worth
    /// leaving in the record: a vector transcribed by shape rather than by count fails at exactly
    /// one assertion and looks like a bug in the code being tested.
    #[rustfmt::skip]
    const TEMP: [u8; 68] = [
        // Responserversion, HiResponserversion, Z(6)
        0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // Time (all zero in the example)
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // ClientChallenge, Z(4)
        0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa,
        0x00, 0x00, 0x00, 0x00,
        // Target info: MsvAvNbDomainName = "Domain"
        0x02, 0x00, 0x0c, 0x00,
        0x44, 0x00, 0x6f, 0x00, 0x6d, 0x00, 0x61, 0x00, 0x69, 0x00, 0x6e, 0x00,
        // MsvAvNbComputerName = "Server"
        0x01, 0x00, 0x0c, 0x00,
        0x53, 0x00, 0x65, 0x00, 0x72, 0x00, 0x76, 0x00, 0x65, 0x00, 0x72, 0x00,
        // MsvAvEOL, then Z(4)
        0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ];

    /// §4.2.2.1.1: the NT hash of `Password`, which is the value an `smbd` would hold in its
    /// password database and which this system's secrets service never stores.
    #[test]
    fn ms_nlmp_nt_hash() {
        assert_eq!(
            nt_hash(PASSWORD).unwrap(),
            hex16("a4f49c406510bdcab6824ee7c30fd852"),
        );
    }

    /// §4.2.4.1.1. This is the vector that pins the uppercasing rule: `User` is uppercased to
    /// `USER` and `Domain` is not, and swapping either produces a different key.
    #[test]
    fn ms_nlmp_ntowfv2() {
        let k = ntowfv2(PASSWORD, USER, DOMAIN).unwrap();
        assert_eq!(k, hex16("0c868a403bfd7a93a3001ef22ef02e3f"));
        assert_ne!(
            ntowfv2(PASSWORD, USER, b"DOMAIN").unwrap(),
            k,
            "the domain must not be uppercased",
        );
        assert_eq!(
            ntowfv2(PASSWORD, b"user", DOMAIN).unwrap(),
            k,
            "the user must be uppercased, so its case cannot matter",
        );
    }

    /// §4.2.4.1.3's `NTProofStr` and §4.2.4.1.2's `SessionBaseKey`, computed from the stored key
    /// exactly as the service computes them.
    #[test]
    fn ms_nlmp_proof_and_session_key() {
        let k = ntowfv2(PASSWORD, USER, DOMAIN).unwrap();
        let p = proof(&k, &SERVER_CHALLENGE, &TEMP);
        assert_eq!(p, hex16("68cd0ab851e51c96aabc927bebef6a1c"));
        assert_eq!(
            session_base_key(&k, &p),
            hex16("8de40ccadbc14a82f15cb0ad0de95ca3"),
        );
    }

    /// **A different challenge is a different proof**, which is the property that makes a captured
    /// authentication unusable a second time. One bit of the challenge is enough; the test flips
    /// the last byte because a MAC that ignored a trailing byte of its first input is exactly the
    /// concatenation bug this shape invites.
    #[test]
    fn a_replayed_proof_does_not_verify_against_a_fresh_challenge() {
        let k = ntowfv2(PASSWORD, USER, DOMAIN).unwrap();
        let p = proof(&k, &SERVER_CHALLENGE, &TEMP);
        let mut fresh = SERVER_CHALLENGE;
        fresh[CHALLENGE_LEN - 1] ^= 0x01;
        assert_ne!(proof(&k, &fresh, &TEMP), p);
    }

    /// **The blob is covered by the MAC**, so a client cannot change the target info or the
    /// timestamp it committed to. Swept over every byte position rather than a chosen one: a
    /// concatenation that dropped a length, or a MAC fed the blob before the challenge, would go
    /// wrong at a position nobody would have picked.
    #[test]
    fn every_byte_of_the_blob_changes_the_proof() {
        let k = ntowfv2(PASSWORD, USER, DOMAIN).unwrap();
        let good = proof(&k, &SERVER_CHALLENGE, &TEMP);
        for i in 0..TEMP.len() {
            let mut blob = TEMP;
            blob[i] ^= 0x80;
            assert_ne!(proof(&k, &SERVER_CHALLENGE, &blob), good, "blob byte {i}");
        }
    }

    /// An empty blob is legal arithmetic (the MAC is over the challenge alone) and must not be
    /// mistaken for the real one. NTLMv2 never sends one, but the service's parse allows a
    /// zero-length blob and this pins what that produces rather than leaving it to chance.
    #[test]
    fn an_empty_blob_is_a_different_proof_and_not_a_panic() {
        let k = ntowfv2(PASSWORD, USER, DOMAIN).unwrap();
        assert_ne!(
            proof(&k, &SERVER_CHALLENGE, &[]),
            proof(&k, &SERVER_CHALLENGE, &TEMP),
        );
    }

    /// Non-UTF-8 input is an error rather than a lossy conversion. A password that silently became
    /// a different password would authenticate on this machine and nowhere else, which is the
    /// worst kind of wrong: it works until it matters.
    #[test]
    fn input_that_is_not_utf8_is_refused_rather_than_mangled() {
        assert_eq!(nt_hash(&[0xff, 0xfe]), Err(Error::Password));
        assert_eq!(ntowfv2(&[0xff], USER, DOMAIN), Err(Error::Password));
        assert_eq!(ntowfv2(PASSWORD, &[0xff], DOMAIN), Err(Error::Name));
        assert_eq!(ntowfv2(PASSWORD, USER, &[0xff]), Err(Error::Name));
    }

    /// A password outside the Basic Multilingual Plane encodes as a surrogate pair, which is the
    /// one case the streaming encoder has to get right and the one a fixed-size buffer sized in
    /// code points would overflow. Pinned as "it produces something, deterministically, and it is
    /// not the same as the replacement character" rather than against a published vector, because
    /// Microsoft publishes none for this.
    #[test]
    fn an_astral_password_encodes_as_a_surrogate_pair() {
        let with = nt_hash("a\u{1F600}b".as_bytes()).unwrap();
        assert_eq!(with, nt_hash("a\u{1F600}b".as_bytes()).unwrap());
        assert_ne!(with, nt_hash("a\u{FFFD}b".as_bytes()).unwrap());
        assert_ne!(with, nt_hash(b"ab").unwrap());
    }
}
