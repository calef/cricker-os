//! **The credential service's clients** (milestone 56, the credential half; notes/credentials.md).
//!
//! One binary, three roles, because a program that shares the honest path is a fairer test of a
//! boundary than a different program failing for its own reasons. Every role holds **exactly the
//! same endowment**: the verify endpoint, a report endpoint, and the page it writes its request
//! into. No store, no provision endpoint, no entropy, no budget.
//!
//! - [`ROLE_HONEST`] is what an SMB adapter would be: it asks three questions and reports the
//!   three answers, plus whether the shared page came back clean.
//! - [`ROLE_ATTACKER`] holds the identical endowment and tries to *write* the store through it:
//!   `PUT`, `SEAL`, an opcode nobody defined, and lengths outside the contract. Then it asks
//!   whether the credential it tried to install works. It must not.
//! - [`ROLE_PROVISIONER`] is the one that runs *before* either, on the **provision** endpoint, and
//!   is a separate role rather than a separate binary so that the difference between provisioning
//!   and verifying is visible as one field of a `Spawn` literal: which endpoint is in slot 0.
//! - [`ROLE_NTLM`] is what an SMB server would be (milestone 65): it answers a client's NTLMv2
//!   authentication holding no key at all, against [MS-NLMP] §4.2.4's published values.
//!
//! # What the attacker proves, and what it does not
//!
//! It does not prove that a permission check works, because there is no permission check. It
//! proves the shape: by the time this program runs, the provision endpoint has been deleted at
//! both ends, so `PUT` is not a request that gets refused, it is a word with no object behind it.
//! The serve loop it reaches implements one opcode. See user/src/credentialer.rs.
//!
//! # Capability contract (notes/abi.md §4)
//!
//! - slot 0: the credential service's endpoint (WRITE). **Which one is the whole story**: the
//!   provision endpoint for [`ROLE_PROVISIONER`], the verify endpoint for the other two.
//! - slot 1: the report endpoint (WRITE)
//! - mapped: one shared page, at [`PAGE_VA`] for a client and [`PROV_VA`] for the provisioner
//! - `a0`: the role
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `credcli`. Refused `credcli` (a
//! squished abbreviation) and `credentialer_client` (that name belongs to the real client milestone
//! 55 needs for SMB authentication, and giving it to a test program squats it). `witness` was
//! considered for the family and set aside: it is standard in proof theory, model checking and
//! cryptography, but `client` carries information about what the program is that `witness` does
//! not.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use cred_proto as proto;
use user_rt::{call, exit, send};

/// The credential service's endpoint (slot 0). Verify or provision, depending on the role, and
/// this program cannot tell which it was given: that is the point of an unforgeable reference.
const SERVICE: u64 = 0;
/// The report endpoint (slot 1).
const REPORT: u64 = 1;

/// A client's shared page. Must match user/src/credentialer.rs `VERIFY_VA`.
const PAGE_VA: u64 = 0x0000_0000_00e1_0000;
/// The provisioner's shared page. Must match user/src/credentialer.rs `PROV_VA`.
const PROV_VA: u64 = 0x0000_0000_00e0_0000;

/// An SMB adapter's shape: ask, and believe the answer.
pub const ROLE_HONEST: u64 = 0;
/// The same endowment, used to try to write the store.
pub const ROLE_ATTACKER: u64 = 1;
/// Phase one: fill the store and seal it.
pub const ROLE_PROVISIONER: u64 = 2;
/// An SMB server's shape (milestone 65): answer a client's NTLMv2 authentication without ever
/// holding the key that answers it.
pub const ROLE_NTLM: u64 = 3;

/// The report's first word, so the kernel test knows who is speaking.
pub const RPT_DONE: u64 = 0x_c2ed_c11e_0000_0001;

/// Bit 0 of the report's third word: the shared page came back empty after the last reply.
pub const F_CLEAN: u64 = 1 << 0;
/// Bit 1: the session key the service published is [MS-NLMP] §4.2.4.1.2's published one.
pub const F_SESSION_KEY: u64 = 1 << 1;
/// Bit 2: a refused proof published no session key at all.
pub const F_NO_KEY_ON_REFUSAL: u64 = 1 << 2;

/// The identities and secrets this milestone's tests use. Three, matching the three family members
/// design/roadmap/56-secrets-and-entropy.md says the real deployment serves.
const PEOPLE: [(&[u8], &[u8]); 3] = [
    (b"chris", b"correct horse battery staple"),
    (b"corinne", b"a different secret entirely"),
    (b"graeme", b"and a third one"),
];

/// One share: the resource name, the password, the account name, and the domain. A named type
/// rather than a bare tuple because four `&[u8]`s in a row are a puzzle at the use site, which is
/// exactly what clippy's `type_complexity` is for.
type Share = (&'static [u8], &'static [u8], &'static [u8], &'static [u8]);

/// **Three shares, one per family member**, each with its own account name and password, which is
/// what "a secret is scoped to a resource rather than to an identity" means in practice
/// (design/roadmap/65-secrets-service.md). A leaked key here authenticates to one share and to
/// nothing else, because there is nothing else it is the credential for.
///
/// The **first** uses [MS-NLMP] §4.2.1's account (`Domain\User`, password `Password`) on purpose:
/// that is the account Microsoft publishes every intermediate value for, so the kernel test can
/// assert against printed numbers rather than against something this tree computed. It moved into
/// `cred_proto::fixture` when milestone 54's SMB adapter and xtask's prober became its other two
/// readers; this role is still the only thing that *stores* it.
const SHARES: [Share; 3] = [
    // Through `cred_proto::fixture` rather than spelled here, since milestone 54's identity item:
    // the SMB adapter authenticates against this resource and xtask's prober computes a proof over
    // this password, so three programs must agree on it down to the byte. The values did not change.
    (
        proto::fixture::SMB_RESOURCE,
        proto::fixture::SMB_PASSWORD,
        proto::fixture::SMB_USER,
        proto::fixture::SMB_DOMAIN,
    ),
    (
        b"backups-corinne",
        b"another share secret",
        b"corinne",
        b"WORKGROUP",
    ),
    (
        b"backups-graeme",
        b"a third share secret",
        b"graeme",
        b"WORKGROUP",
    ),
];

/// The share the NTLM role authenticates against, and its published answer.
const SHARE: &[u8] = SHARES[0].0;

/// [MS-NLMP] §4.2.4: the server challenge, the client's `temp` blob, the `NTProofStr` a holder of
/// the password computes, and the `SessionBaseKey` that follows from it. Every one of these is a
/// number the specification prints.
const CHALLENGE: [u8; proto::CHALLENGE_LEN] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];

#[rustfmt::skip]
const BLOB: [u8; 68] = [
    0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa,
    0x00, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x0c, 0x00,
    0x44, 0x00, 0x6f, 0x00, 0x6d, 0x00, 0x61, 0x00, 0x69, 0x00, 0x6e, 0x00,
    0x01, 0x00, 0x0c, 0x00,
    0x53, 0x00, 0x65, 0x00, 0x72, 0x00, 0x76, 0x00, 0x65, 0x00, 0x72, 0x00,
    0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];

const PROOF: [u8; proto::KEY_LEN] = [
    0x68, 0xcd, 0x0a, 0xb8, 0x51, 0xe5, 0x1c, 0x96, 0xaa, 0xbc, 0x92, 0x7b, 0xeb, 0xef, 0x6a, 0x1c,
];

const SESSION_KEY: [u8; proto::KEY_LEN] = [
    0x8d, 0xe4, 0x0c, 0xca, 0xdb, 0xc1, 0x4a, 0x82, 0xf1, 0x5c, 0xb0, 0xad, 0x0d, 0xe9, 0x5c, 0xa3,
];

/// The identity the attacker tries to install for itself.
const IMPOSTOR: &[u8] = b"impostor";
const IMPOSTOR_SECRET: &[u8] = b"let me in";

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, _a1: u64, _a2: u64) -> ! {
    match role {
        ROLE_PROVISIONER => provisioner(),
        ROLE_ATTACKER => attacker(),
        ROLE_NTLM => ntlm(),
        _ => honest(),
    }
}

/// **Phase one**: three identities, then the seal, then a `PUT` that must arrive at a deleted
/// endpoint. That last one is deliberately *not* sent: a `CALL` on an endpoint whose receiver is
/// gone blocks forever, which would hang the test rather than fail it, and "the provisioner would
/// hang if it tried" is a worse thing to encode in a test than in a sentence. The proof that the
/// store is closed is the attacker's, which reaches a live endpoint that does not implement `PUT`.
fn provisioner() -> ! {
    let mut codes = Codes::new();
    for (identity, secret) in PEOPLE {
        codes.push(request(PROV_VA, identity, secret, proto::provision::PUT));
    }
    // Then the three shares, each bound to its own account and domain. A share's secret is scoped
    // to the resource, so these are six independent secrets in one store rather than three people
    // with two credentials each.
    for (resource, password, user, domain) in SHARES {
        codes.push(put_ntlm(resource, password, user, domain));
    }
    // A seventh secret in a six-slot store: FULL, not a silent overwrite of somebody.
    codes.push(request(
        PROV_VA,
        b"nobody",
        b"no room",
        proto::provision::PUT,
    ));
    // Seal. `place` needs a non-empty identity and secret even for an opcode that reads neither;
    // the words carry the opcode and the page is wiped by the service either way.
    codes.push(request(PROV_VA, b"seal", b"seal", proto::provision::SEAL));
    done(codes, u64::from(page_is_clean(PROV_VA)))
}

/// **An SMB server's shape**, and the whole point of milestone 65: this program answers a client's
/// NTLMv2 authentication while holding no key at all. It has the challenge it issued, the blob and
/// the proof the client sent, and one endpoint. It never sees an `NTOWFv2` and there is no message
/// that would give it one.
fn ntlm() -> ! {
    let mut codes = Codes::new();
    let mut flags = 0;

    // The real thing: [MS-NLMP] §4.2.4's published proof against the share provisioned with
    // §4.2.1's account. A match, and the published session key in the page.
    codes.push(ntlm_request(SHARE, &PROOF));
    if session_key() == SESSION_KEY {
        flags |= F_SESSION_KEY;
    }

    // One bit off. The proof is a MAC, so this is what every forgery looks like.
    let mut bad = PROOF;
    bad[0] ^= 0x01;
    codes.push(ntlm_request(SHARE, &bad));
    if session_key() == [0u8; proto::KEY_LEN] {
        flags |= F_NO_KEY_ON_REFUSAL;
    }

    // A resource that has a password but no NTLM secret. Its stored key is zeros, which is a value
    // an attacker KNOWS, so this is the strongest form of the attack available and it must fail.
    codes.push(ntlm_request(PEOPLE[0].0, &PROOF));
    // And one nobody provisioned, which must be indistinguishable from the above.
    codes.push(ntlm_request(b"no-such-share", &PROOF));

    // One password, two derivations: the share also answers an ordinary verify. Deliberately last,
    // because its reply leaves the page wiped, which is what makes the cleanliness check below say
    // something rather than measuring a wipe this program did itself.
    codes.push(request(PAGE_VA, SHARE, SHARES[0].1, proto::verify::VERIFY));
    if page_is_clean(PAGE_VA) {
        flags |= F_CLEAN;
    }
    done(codes, flags)
}

/// A `PUT_NTLM`, which needs both request words because the account name's and the domain's
/// lengths ride in the second one.
fn put_ntlm(resource: &[u8], password: &[u8], user: &[u8], domain: &[u8]) -> u64 {
    // SAFETY: the wiring mapped one page read/write at PROV_VA before this program ran.
    let page = unsafe { core::slice::from_raw_parts_mut(PROV_VA as *mut u8, proto::PAGE) };
    let Some((w0, w1)) = proto::place_ntlm_put(page, resource, password, user, domain) else {
        return u64::MAX;
    };
    let (r0, r1) = call(SERVICE, w0, w1);
    if r1 != proto::NO_DATA { u64::MAX } else { r0 }
}

/// An `NTLM_PROOF`, with the challenge and the blob this program would have exchanged with a real
/// client.
fn ntlm_request(resource: &[u8], proof: &[u8; proto::KEY_LEN]) -> u64 {
    // SAFETY: as in `request`.
    let page = unsafe { core::slice::from_raw_parts_mut(PAGE_VA as *mut u8, proto::PAGE) };
    let Some(w0) = proto::place_ntlm_proof(page, resource, &CHALLENGE, &BLOB, proof) else {
        return u64::MAX;
    };
    let (r0, r1) = call(SERVICE, w0, 0);
    if r1 != proto::NO_DATA { u64::MAX } else { r0 }
}

/// The `SessionBaseKey` the service published, or zeros if the page is somehow too small (which it
/// cannot be: the wiring maps a whole frame).
fn session_key() -> [u8; proto::KEY_LEN] {
    // SAFETY: as in `request`.
    let page = unsafe { core::slice::from_raw_parts(PAGE_VA as *const u8, proto::PAGE) };
    proto::session_key(page).unwrap_or([0; proto::KEY_LEN])
}

/// **The honest client**: the right secret, the wrong secret, and an identity nobody provisioned.
fn honest() -> ! {
    let mut codes = Codes::new();
    let (identity, secret) = PEOPLE[0];
    codes.push(request(PAGE_VA, identity, secret, proto::verify::VERIFY));
    codes.push(request(
        PAGE_VA,
        identity,
        b"not the secret",
        proto::verify::VERIFY,
    ));
    codes.push(request(
        PAGE_VA,
        b"nobody-at-all",
        secret,
        proto::verify::VERIFY,
    ));
    // The pairing is what is checked, not the secret alone: one person's password must not open
    // another person's account.
    let (other, _) = PEOPLE[1];
    codes.push(request(PAGE_VA, other, secret, proto::verify::VERIFY));
    done(codes, u64::from(page_is_clean(PAGE_VA)))
}

/// **The attacker**: the same slot 0, used for everything the contract does not offer.
fn attacker() -> ! {
    let mut codes = Codes::new();
    // Write the store through the endpoint we were given. The answer is `MISMATCH`, not a refusal,
    // and that is the model working rather than a hole in it: `provision::PUT` and `verify::VERIFY`
    // are both opcode 1, because the *endpoint* gives a number its meaning and a client cannot
    // choose which serve loop reads it. So this is a verify of an identity nobody provisioned, and
    // the honest answer is no. See `cred_proto`'s "an opcode is not an authority".
    codes.push(request(
        PAGE_VA,
        IMPOSTOR,
        IMPOSTOR_SECRET,
        proto::provision::PUT,
    ));
    // Re-seal it, in case a service that had not sealed would accept one. Since milestone 65 this
    // is `NTLM_PROOF`'s number on this endpoint, so the answer is MISMATCH rather than MALFORMED:
    // what the attacker sent is a proof for a resource nobody provisioned. See the kernel test.
    codes.push(request(
        PAGE_VA,
        IMPOSTOR,
        IMPOSTOR_SECRET,
        proto::provision::SEAL,
    ));
    // An opcode nobody defined, in case the dispatch falls through to something.
    codes.push(request(PAGE_VA, IMPOSTOR, IMPOSTOR_SECRET, 0x7f));
    // A length outside the contract, in case the service indexes before it checks. `place` will
    // not build this request, so the word is hand-made and the page is left as it was.
    let (r0, r1) = call(
        SERVICE,
        proto::req(proto::verify::VERIFY, 0xffff, 0xffff),
        0,
    );
    codes.push(if r1 == proto::NO_DATA {
        r0
    } else {
        // A reply that carried a second word at all is a finding, whatever it said.
        u64::MAX
    });
    // `PUT_NTLM`, the opcode milestone 65 added to the *provisioning* space. On this endpoint it
    // is opcode 3, which no serve loop here implements, so it is MALFORMED rather than a refusal:
    // there is still nothing to refuse.
    codes.push(request(
        PAGE_VA,
        IMPOSTOR,
        IMPOSTOR_SECRET,
        proto::provision::PUT_NTLM,
    ));
    // And now the question that matters: did any of that install a credential?
    codes.push(request(
        PAGE_VA,
        IMPOSTOR,
        IMPOSTOR_SECRET,
        proto::verify::VERIFY,
    ));
    done(codes, u64::from(page_is_clean(PAGE_VA)))
}

/// Place a request in the shared page and `CALL`. Returns the service's reply code, or
/// [`u64::MAX`] if the reply carried data in its second word, which nothing here ever should.
fn request(va: u64, identity: &[u8], secret: &[u8], op: u64) -> u64 {
    // SAFETY: the wiring mapped one page read/write at `va` before this program ran, and this
    // process is its only user.
    let page = unsafe { core::slice::from_raw_parts_mut(va as *mut u8, proto::PAGE) };
    let Some(w0) = proto::place(page, identity, secret, op) else {
        return u64::MAX;
    };
    let (r0, r1) = call(SERVICE, w0, 0);
    if r1 != proto::NO_DATA { u64::MAX } else { r0 }
}

/// **Is the shared page empty?** The service wipes it after reading every request, so after the
/// last `CALL` this frame should hold neither the secret this program presented nor anything the
/// service put there. Checking the whole page and not just the request area, because a byte of a
/// salt or a tag landing past the request area would be the interesting kind of leak.
fn page_is_clean(va: u64) -> bool {
    // SAFETY: as in `request`.
    let page = unsafe { core::slice::from_raw_parts(va as *const u8, proto::PAGE) };
    page.iter().all(|&b| b == 0)
}

/// Up to eight reply codes, one per byte, so a run of requests fits in one report word. A code
/// wider than a byte (only [`u64::MAX`], which means "the reply was malformed") is recorded as
/// `0xff`, which no real code can be.
struct Codes {
    packed: u64,
    n: u32,
}

impl Codes {
    fn new() -> Self {
        Codes { packed: 0, n: 0 }
    }

    fn push(&mut self, code: u64) {
        if self.n >= 8 {
            return;
        }
        let byte = if code <= proto::MAX_CODE { code } else { 0xff };
        self.packed |= byte << (8 * self.n);
        self.n += 1;
    }
}

/// Report: who spoke, the packed reply codes, and a **flag word**. Bit 0 is "the shared page came
/// back empty", which every role sets; the NTLM role sets two more (see [`F_SESSION_KEY`] and
/// [`F_NO_KEY_ON_REFUSAL`]). A flag word rather than a second bool because the third report word
/// is the only one left and a role that learns two things has to say both.
fn done(codes: Codes, flags: u64) -> ! {
    send(REPORT, RPT_DONE, codes.packed, flags);
    exit()
}

user_rt::panic_handler!();
