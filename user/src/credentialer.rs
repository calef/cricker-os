//! **The credential service** (milestone 56, the credential half; notes/credentials.md).
//!
//! The one process that holds the credential store, and the only thing in the system that can read
//! it. Everything else holds an endpoint that means *"you may ask whether a secret is right"*,
//! which is a strictly smaller authority than *"you may read the secret"*: a client cannot
//! enumerate the identities, cannot obtain a salt or a tag, and cannot write a record.
//!
//! ```text
//!   the provisioner ──the provision endpoint──►┌────────────┐
//!   (once, at boot)      (PUT, then SEAL)      │ credential │──►the entropy service
//!                                              │  service   │   (salts; §44's endpoint)
//!   a client ────────the verify endpoint──────►└────────────┘
//!                    (VERIFY, forever)            the store lives here and nowhere else
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
//! 1. **Provision.** RECV on the provision endpoint. Each [`cred_proto::provision::PUT`] derives a
//!    record with a salt drawn from the entropy service. [`cred_proto::provision::SEAL`] ends it.
//! 2. **Delete.** The service `cap_delete`s its receive end of the provision endpoint, and the
//!    provisioner deletes its send end. Nothing in the system can name it any more.
//! 3. **Verify.** RECV on the verify endpoint, forever. One opcode. Yes or no.
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
//! No initrd, no filesystem, no network, no device. A compromised credential service is a machine
//! whose logins an attacker can answer, which is exactly as much damage as owning the credential
//! store should be worth.
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

#![no_std]
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

/// How many identities the store holds. Three, because Chris's existing setup serves three family
/// members with separate passwords (design/roadmap.md, milestone 56), and a store sized to the
/// requirement makes "the fourth is refused" a thing the tests can show rather than a branch
/// nothing reaches.
const CAPACITY: usize = 3;

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
        let (w0, cap, _) = recv_cap(PROV);
        if cap == abi::endpoint::NO_CAP {
            // A plain SEND on a CALL-only contract: nobody is waiting for an answer, so there is
            // nothing to reply into. Drop it rather than replying into a slot we do not hold.
            continue;
        }
        match proto::op(w0) {
            proto::provision::PUT => {
                let verdict = put(store, scratch, w0);
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

/// One `PUT`, with its salt drawn fresh. Split out so the wipe above covers every exit.
fn put(store: &mut Store<CAPACITY>, scratch: &mut [Block], w0: u64) -> u64 {
    // SAFETY: the wiring mapped one page read/write at PROV_VA before this program ran.
    let page = unsafe { core::slice::from_raw_parts(PROV_VA as *const u8, proto::PAGE) };
    let Some((identity, secret)) = proto::read(page, w0) else {
        return proto::MALFORMED;
    };
    let mut salt = [0u8; cred::SALT_LEN];
    if !fill(&mut salt) {
        // No unpredictable bits, so no record. Answering with a weak salt would be the silent
        // degradation DECISIONS §42 forbids, and it would be invisible: every login would still
        // work, and the store would be one rainbow table wide.
        return proto::NO_ENTROPY;
    }
    match store.put(identity, secret, salt, scratch) {
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
        let verdict = match proto::op(w0) {
            proto::verify::VERIFY => answer(store, scratch, w0),
            // Every other opcode, including the provisioning ones. A client that tries `PUT` here
            // is not refused by a permission check; it is talking to a loop in which that opcode
            // has no meaning, because the object that gave it meaning no longer exists.
            _ => proto::MALFORMED,
        };
        // On every path: the client wrote a secret into this page, and leaving it there would make
        // the frame a place the secret persists after the answer.
        wipe(VERIFY_VA);
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

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // No channel worth trusting to report on, and nothing here should be reported in detail
    // anyway: a panic message from a credential service is a place a secret could escape. A fault
    // the kernel turns into a kill is the honest signal, and a dead credential service refuses
    // every login rather than answering any of them wrongly. aarch64 `brk`, RISC-V `ebreak`.
    #[cfg(target_arch = "aarch64")]
    // SAFETY: a trap instruction; no memory is accessed.
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem));
    };
    #[cfg(target_arch = "riscv64")]
    // SAFETY: as above.
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
