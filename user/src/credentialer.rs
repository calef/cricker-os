//! **The secrets service** (milestone 56's credential half, generalised by milestone 65;
//! notes/credentials.md and notes/ntlm.md).
//!
//! **Hold the key, expose the operation, never the key.** The one process in the system that holds
//! secrets, and the only thing that can read them. Everything else holds an endpoint that means
//! *"you may ask this store to compute with a secret"*, which is a strictly smaller authority than
//! *"you may read the secret"*: a client cannot enumerate the identities, cannot obtain a salt, a
//! tag or an NTLM key, and cannot write a record.
//!
//! **This program's name lags its job**, and knowingly. Milestone 65 says the credentialer becomes
//! one operation in a secrets service rather than a service beside it, because a second process
//! holding secrets is exactly what the design exists to avoid. So the generalisation happened here
//! rather than in a new program. A rename is a naming decision and belongs to calef (CLAUDE.md);
//! this header is the note that it is owed.
//!
//! # Two kinds of secret, two operations, one store
//!
//! | secret | operation | never exposed |
//! |---|---|---|
//! | an Argon2id tag | `VERIFY`: is this the secret? | the tag, the salt |
//! | an `NTOWFv2` | `NTLM_PROOF`: is this the proof, and here is the session key | the key |
//!
//! The second is the reason this is a milestone rather than another opcode. **NTLMv2 does not
//! verify a presented secret**: the client never sends the password, the server holds a key and
//! computes a MAC, so "secret in, boolean out" does not fit it. What the SMB server gets is an
//! endpoint that answers challenges. Compromise it and an attacker can authenticate sessions
//! *while they hold the endpoint*, and cannot extract the key, crack it offline, or carry it
//! anywhere else. Storing the key in the SMB server offers none of that, which is what Samba does.
//!
//! ```text
//!   the provisioner ──the provision endpoint──►┌────────────┐
//!   (once, at boot)   (PUT/PUT_NTLM, then SEAL) │  secrets   │──►the entropy service
//!                                               │  service   │   (salts; §44's endpoint)
//!   a client ────────the verify endpoint───────►└────────────┘
//!              (VERIFY / NTLM_PROOF, forever)      the store lives here and nowhere else
//! ```
//!
//! # Two phases, because this kernel has one wait point
//!
//! The clock service records the constraint (user/src/clock.rs): there is no wait-any primitive
//! and no threads inside one address space, so a process can block on exactly **one** endpoint.
//! The clock's answer was to make its wide authority a page write rather than a message. This
//! service's answer is different and, for a credential store, better: writing the store is not an
//! *operation* at all, it is a **phase**, and the phase ends.
//!
//! 1. **Provision.** RECV on the provision endpoint. Each [`cred_proto::provision::PUT`] and
//!    [`cred_proto::provision::PUT_NTLM`] derives a record with a salt drawn from the entropy
//!    service. [`cred_proto::provision::SEAL`] ends it.
//! 2. **Delete.** The service `cap_delete`s its receive end of the provision endpoint, and the
//!    provisioner deletes its send end. Nothing in the system can name it any more.
//! 3. **Serve.** RECV on the verify endpoint, forever. Two opcodes, one per kind of secret. Yes
//!    or no, and on an NTLM yes, a session key in the shared page.
//!
//! **That is the asymmetry, and it is structural rather than a check.** A client is not refused
//! permission to write the store; there is no object through which the request could travel by the
//! time a client holds anything. Compare a Unix password database, where `smbd` opens the file and
//! the only thing standing between a compromised server and every hash in it is that the code does
//! not choose to read them.
//!
//! # It never invents a salt
//!
//! Every salt comes from the entropy service (DECISIONS §44), and a provisioning request that
//! cannot get one is answered [`cred_proto::NO_ENTROPY`] rather than being served with something
//! weaker. A predictable salt is a store one rainbow table covers, so falling back would be
//! exactly the silent degradation DECISIONS §42 forbids, in the one place it would be hardest to
//! notice: everything would keep working.
//!
//! The decoy record's salt and tag come from the same place, at start-up. A service that could not
//! draw them **refuses to start**, because a credential service with no unpredictable bits cannot
//! do its job and saying so is the only honest option.
//!
//! # Capability contract (notes/abi.md §4)
//!
//! - slot 0: the **provision** endpoint (RECV), deleted at the seal
//! - slot 1: the **verify** endpoint (RECV)
//! - slot 2: the **entropy** service's endpoint (WRITE)
//! - slot 3: an **untyped budget**, for the memory-hard scratch and nothing else
//! - slot 4: a **readiness** endpoint (WRITE), one message once the store is sealed
//! - mapped: the provision page, and the verify page. **Two frames, never one**: the provisioner
//!   writes plaintext secrets into its page, and a client that shared that frame would read them.
//!
//! No initrd, no filesystem, no network, no device. A compromised secrets service is a machine
//! whose logins an attacker can answer and whose shares they can authenticate to, for as long as
//! they hold it, which is exactly as much damage as owning the store should be worth. It is not
//! a machine whose passwords they can carry away.
//!
//! # BUGS
//!
//! **One verify page means one client at a time.** The page is per service, not per channel, so
//! two clients sharing the endpoint would also share the frame each writes its presented secret
//! into. Nothing here detects that. `fs_proto`'s answer (one page per channel) is the shape to
//! copy when a second client exists; today the intended client is the single SMB adapter.
//!
//! **Nothing survives a reboot.** The store is memory only. Secrets at rest are unsolved and this
//! service does not pretend otherwise; see notes/credentials.md.
//!
//! **No rate limit, no lockout, no attempt counter.** A client holding the verify endpoint can
//! guess as fast as it can `CALL`, and each guess costs it one Argon2id derivation of the service's
//! time. That cost is the only thing slowing an online attack down, and it is also a way to make
//! the service unresponsive to everyone else.
//!
//! **`NTLM_PROOF` does not even have that.** An NTLM answer is three MD5-shaped operations, four
//! orders of magnitude cheaper than a password verify, so the KDF's incidental rate limiting does
//! not apply to it at all. Online guessing against it is still useless (a guess is a 128-bit MAC,
//! not a password), but it is a free way to spin this service, and a service that answers logins
//! is a service worth spinning. Nothing here counts requests.
//!
//! **Revocation is per holder, not per secret.** Destroying a client's endpoint ends its access,
//! which is the thing this design buys and a stored hash could never offer. Revoking one *secret*
//! is a different question and this service cannot answer it: the store is sealed, so there is no
//! object through which a record could be removed, which is the same property that makes the seal
//! worth having. Rotating a secret means restarting the service and reprovisioning, so a
//! deployment that needs finer granularity runs more than one. See notes/credentials.md.
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `credential`. Refused `credential`
//! (the plain resource noun, on the pattern `clock` and `entropy` follow). The argument for it was
//! made twice and wrong twice: `credentialer` is a real profession rather than a coinage, and this
//! service will never give you a credential, so naming it for the resource implies the one thing it
//! exists to refuse.
//!
//! **That ratification predates what this program became.** Milestone 65 turned it from a
//! credential service into a **secrets service**: it now holds an `NTOWFv2` beside the
//! Argon2id tag and answers `NTLM_PROOF`, so the argument above ("this service will never
//! give you a credential") still holds while the noun no longer describes the scope. A
//! rename is owed, and it is calef's, not a lane's; `cred`, `cred_proto` and
//! `credentialer_test_client` are in the same boat. Recorded here rather than acted on.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use cred::{Block, Cost, Store, Verdict};
use cred_proto as proto;
use user_rt::{call, cap_delete, exit, recv_cap, reply, send};

/// The provision endpoint (slot 0): RECV, and only until the seal.
const PROV: u64 = 0;
/// The verify endpoint (slot 1): RECV, forever.
const VERIFY: u64 = 1;
/// The entropy service's endpoint (slot 2): WRITE. Names no device (DECISIONS §44).
const ENTROPY: u64 = 2;
/// The untyped budget (slot 3): pays for the memory-hard scratch.
const BUDGET: u64 = 3;
/// The readiness endpoint (slot 4): WRITE, one message.
const READY: u64 = 4;

/// The provisioner's page. Plaintext secrets cross it; nothing but the provisioner maps it.
const PROV_VA: u64 = 0x0000_0000_00e0_0000;
/// A client's page. Must match `user/src/credentialer_test_client.rs`.
const VERIFY_VA: u64 = 0x0000_0000_00e1_0000;

/// How many secrets the store holds. **Six: three logins and three shares.**
///
/// Three, because calef's existing setup serves three family members with separate passwords
/// (design/roadmap/56-secrets-and-entropy.md). Doubled by milestone 65, because each of those
/// family members also has a Time Machine share with its own account and password, and a secret
/// here is scoped to a *resource* rather than to a person. Three family members therefore means
/// at least three shares, which is why multi-share is the deliverable rather than a later
/// generalisation: a single-secret store would have been discovered as wrong at the worst moment.
///
/// A store sized to the requirement also makes "the seventh is refused" a thing the tests can
/// show rather than a branch nothing reaches.
const CAPACITY: usize = 6;

/// The heap's virtual cap. It holds one allocation, the Argon2 scratch, plus the slack an
/// allocator needs to place it; the untyped behind it is the real ceiling either way.
const HEAP_MAX: u64 = 6 * 1024 * 1024;

/// The startup report's first word, so a reader of the endpoint knows this is the credential
/// service speaking. ASCII-ish, so a hex dump of a report reads.
pub const RPT_READY: u64 = 0x_c2ed_0000_0000_0001;

/// A start-up failure, in the `0xDEAD_...` shape every driver here uses, with the low byte naming
/// the step so a failure is diagnosable from one word.
const E_ENTROPY: u64 = 0x01;
const E_SCRATCH: u64 = 0x02;

/// **The wire contract and the store must agree about six numbers**, and this is the only place
/// both are in scope to be compared. `cseam` is the cautionary tale rule 7 was written from: a
/// layout deliberately written twice with nothing checking that the two copies agree, whose drift
/// shows up as a component scribbling on the wrong bytes arbitrarily far from the edit. A page
/// offset that disagreed here would put a client's blob where the service reads a proof.
const _: () = assert!(proto::MAX_IDENTITY == cred::MAX_IDENTITY);
const _: () = assert!(proto::MAX_SECRET == cred::MAX_SECRET);
const _: () = assert!(proto::MAX_NAME == cred::MAX_NAME);
const _: () = assert!(proto::MAX_BLOB == cred::MAX_BLOB);
const _: () = assert!(proto::CHALLENGE_LEN == cred::NTLM_CHALLENGE_LEN);
const _: () = assert!(proto::KEY_LEN == cred::NTLM_KEY_LEN);

#[global_allocator]
static HEAP: user_rt::heap::UntypedHeap = user_rt::heap::UntypedHeap::new();

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, _a1: u64, _a2: u64) -> ! {
    HEAP.init(BUDGET, user_rt::heap::DEFAULT_BASE, HEAP_MAX);

    let cost = Cost::DEFAULT;

    // The decoy first, and the service dies without it. Its salt and tag are what a lookup miss
    // lands on, so that "no such identity" costs one full derivation instead of returning
    // instantly; a decoy of zeros would be a tag an attacker could aim a preimage at, and a
    // constant salt would let one precomputation cover every miss on every machine.
    let mut decoy_salt = [0u8; cred::SALT_LEN];
    let mut decoy_tag = [0u8; cred::TAG_LEN];
    if !fill(&mut decoy_salt) || !fill(&mut decoy_tag) {
        die(E_ENTROPY);
    }

    // One allocation, at start-up, reused by every derivation. Doing it here rather than per
    // request means a login cannot fail because the heap was fragmented, and it means the 4 MiB
    // this service costs is visible in its budget rather than appearing under load.
    let mut scratch: Vec<Block> = vec![Block::default(); cost.blocks()];
    if scratch.len() < cost.blocks() {
        die(E_SCRATCH);
    }

    let mut store = Store::<CAPACITY>::new(cost, decoy_salt, decoy_tag);
    provision(&mut store, &mut scratch);

    // Phase two begins here, and phase one can never resume: the receive end of the provision
    // endpoint is gone (see `provision`), so even this code could not go back to it.
    send(READY, RPT_READY, store.len() as u64, cost.m_kib() as u64);
    serve(&store, &mut scratch)
}

/// **Phase one.** Write the store, then destroy the ability to write the store.
fn provision(store: &mut Store<CAPACITY>, scratch: &mut [Block]) {
    loop {
        let (w0, cap, w1) = recv_cap(PROV);
        if cap == abi::endpoint::NO_CAP {
            // A plain SEND on a CALL-only contract: nobody is waiting for an answer, so there is
            // nothing to reply into. Drop it rather than replying into a slot we do not hold.
            continue;
        }
        match proto::op(w0) {
            proto::provision::PUT | proto::provision::PUT_NTLM => {
                let verdict = put(store, scratch, w0, w1);
                // Unconditionally, on every path including the malformed one: the page holds a
                // plaintext secret and the provisioner is still mapping it.
                wipe(PROV_VA);
                reply(cap, verdict, proto::NO_DATA);
            }
            proto::provision::SEAL => {
                wipe(PROV_VA);
                reply(cap, proto::OK, proto::NO_DATA);
                // **The seal.** After this the service holds no capability to this endpoint, so
                // there is no code path back into the loop above even if one were written. The
                // provisioner drops its send end for the same reason; between them, nothing in the
                // system can name the object any more.
                cap_delete(PROV);
                return;
            }
            _ => {
                reply(cap, proto::MALFORMED, proto::NO_DATA);
            }
        }
    }
}

/// One `PUT` or `PUT_NTLM`, with its salt drawn fresh. Split out so the wipe above covers every
/// exit.
///
/// The two opcodes share this function rather than getting one each, because everything that can
/// go wrong is the same for both and the difference is one parse and one store call. `PUT_NTLM`
/// still draws a salt: it derives an Argon2id tag from the same password, so a resource that
/// speaks NTLM can also answer an ordinary verify, which is what lets milestone 49's login and
/// milestone 55's SMB server share one account instead of needing two.
fn put(store: &mut Store<CAPACITY>, scratch: &mut [Block], w0: u64, w1: u64) -> u64 {
    // SAFETY: the wiring mapped one page read/write at PROV_VA before this program ran.
    let page = unsafe { core::slice::from_raw_parts(PROV_VA as *const u8, proto::PAGE) };
    let ntlm = proto::op(w0) == proto::provision::PUT_NTLM;
    let parsed = if ntlm {
        proto::read_ntlm_put(page, w0, w1)
    } else {
        proto::read(page, w0).map(|(i, s)| (i, s, &[][..], &[][..]))
    };
    let Some((identity, secret, user, domain)) = parsed else {
        return proto::MALFORMED;
    };
    let mut salt = [0u8; cred::SALT_LEN];
    if !fill(&mut salt) {
        // No unpredictable bits, so no record. Answering with a weak salt would be the silent
        // degradation DECISIONS §42 forbids, and it would be invisible: every login would still
        // work, and the store would be one rainbow table wide.
        return proto::NO_ENTROPY;
    }
    let stored = if ntlm {
        store.put_ntlm(identity, secret, user, domain, salt, scratch)
    } else {
        store.put(identity, secret, salt, scratch)
    };
    match stored {
        Ok(()) => proto::OK,
        Err(cred::Error::Full) => proto::FULL,
        Err(_) => proto::MALFORMED,
    }
}

/// **Phase two.** One endpoint, one question, forever.
fn serve(store: &Store<CAPACITY>, scratch: &mut [Block]) -> ! {
    loop {
        let (w0, cap, _) = recv_cap(VERIFY);
        if cap == abi::endpoint::NO_CAP {
            continue;
        }
        let (verdict, key) = match proto::op(w0) {
            proto::verify::VERIFY => (answer(store, scratch, w0), None),
            proto::verify::NTLM_PROOF => ntlm_answer(store, w0),
            // Every other opcode, including the provisioning ones. A client that tries `PUT` here
            // is not refused by a permission check; it is talking to a loop in which that opcode
            // has no meaning, because the object that gave it meaning no longer exists.
            _ => (proto::MALFORMED, None),
        };
        // On every path: the client wrote a secret into this page, and leaving it there would make
        // the frame a place the secret persists after the answer.
        wipe(VERIFY_VA);
        // **After the wipe, never before.** The wipe is what removes the client's request from the
        // shared frame, and it covers the reply area too, so a session key published first would
        // be erased by the very call that makes publishing it safe.
        if let Some(key) = key {
            publish(VERIFY_VA, &key);
        }
        reply(cap, verdict, proto::NO_DATA);
    }
}

/// One `VERIFY`. The reply is a verdict and nothing else; see `cred_proto`'s module docs on why
/// the reply channel has no room for data.
fn answer(store: &Store<CAPACITY>, scratch: &mut [Block], w0: u64) -> u64 {
    // SAFETY: the wiring mapped one page read/write at VERIFY_VA before this program ran.
    let page = unsafe { core::slice::from_raw_parts(VERIFY_VA as *const u8, proto::PAGE) };
    let Some((identity, presented)) = proto::read(page, w0) else {
        return proto::MALFORMED;
    };
    match store.verify(identity, presented, scratch) {
        Ok(Verdict::Match) => proto::MATCH,
        Ok(Verdict::Mismatch) => proto::MISMATCH,
        // A malformed question is not an authentication outcome. Reporting it as MISMATCH would be
        // safe but would tell a buggy client it had the wrong password, which is the kind of wrong
        // diagnosis that costs somebody an afternoon.
        Err(_) => proto::MALFORMED,
    }
}

/// **One `NTLM_PROOF`.** Returns the verdict and the `SessionBaseKey` to publish, which
/// `cred::Store::ntlm_proof` has already zeroed unless the proof verified.
///
/// The key is returned for every well-formed request, matching or not, rather than only for a
/// match. That is not carelessness about a secret: it is zeros on a mismatch, and publishing
/// unconditionally means an acceptance and a refusal do the same work after the MAC. A branch here
/// would be a branch an attacker with a clock could read.
///
/// No scratch and no Argon2id: an NTLM answer is three MD5-shaped operations, so it is roughly
/// four orders of magnitude cheaper than a password verify. That asymmetry is the protocol's, not
/// a choice made here, and it is the reason this endpoint has no rate limit worth the name. See
/// this program's BUGS.
fn ntlm_answer(store: &Store<CAPACITY>, w0: u64) -> (u64, Option<[u8; cred::NTLM_KEY_LEN]>) {
    // SAFETY: the wiring mapped one page read/write at VERIFY_VA before this program ran.
    let page = unsafe { core::slice::from_raw_parts(VERIFY_VA as *const u8, proto::PAGE) };
    let Some((identity, challenge, blob, presented)) = proto::read_ntlm_proof(page, w0) else {
        return (proto::MALFORMED, None);
    };
    match store.ntlm_proof(identity, challenge, blob, presented) {
        Ok((Verdict::Match, key)) => (proto::MATCH, Some(key)),
        Ok((Verdict::Mismatch, key)) => (proto::MISMATCH, Some(key)),
        Err(_) => (proto::MALFORMED, None),
    }
}

/// Write the `SessionBaseKey` into the shared page.
fn publish(va: u64, key: &[u8; cred::NTLM_KEY_LEN]) {
    // SAFETY: as in `wipe`; the wiring mapped one page read/write here and this process is the
    // only writer between a request arriving and its reply going out.
    let page = unsafe { core::slice::from_raw_parts_mut(va as *mut u8, proto::PAGE) };
    proto::put_session_key(page, key);
}

/// Zero the request area of a shared page.
fn wipe(va: u64) {
    // SAFETY: the wiring mapped one page read/write here, and this process is the only writer
    // between a request arriving and its reply going out.
    let page = unsafe { core::slice::from_raw_parts_mut(va as *mut u8, proto::PAGE) };
    proto::wipe(page);
}

/// Fill `out` with bytes from the entropy service, `entropy_proto::MAX_BYTES` at a time. `false`
/// when the service could not supply them, which every caller treats as fatal to the request.
///
/// A short reply is a refusal here, not something to pad out. The entropy contract is explicit
/// that a count below what was asked means the device went dry, and half a salt is not a salt.
fn fill(out: &mut [u8]) -> bool {
    let mut done = 0;
    while done < out.len() {
        let want = (out.len() - done).min(entropy_proto::MAX_BYTES as usize);
        let (r0, r1) = call(
            ENTROPY,
            entropy_proto::req(entropy_proto::GET, want as u64),
            0,
        );
        // `delivered` is what makes "no entropy capability" distinguishable from "no entropy"
        // without a probe: a count is 0..=8 and every kernel error is a huge u64.
        let Some(n) = entropy_proto::delivered(r0) else {
            return false;
        };
        if n < want {
            return false;
        }
        done += entropy_proto::take(n, r1, &mut out[done..]);
    }
    true
}

fn die(step: u64) -> ! {
    send(READY, 0xDEAD_0000_0000_0000 | step, 0, 0);
    exit()
}

user_rt::panic_handler!();
