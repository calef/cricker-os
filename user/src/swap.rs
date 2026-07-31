//! **Live component replacement: the shared half** (milestone 23, DECISIONS §41).
//!
//! Four programs make up the hot-swap system (`swapper` the operator, `conx` and `cconx` the two
//! instances of the swappable component, `chatty` the client and the attacker), and this is what
//! they share: the wire protocol, the addresses of the pages they pass between them, the digest
//! both language implementations of the component must compute, and the serving loop itself.
//! Compiled into each binary with `#[path = "swap.rs"] mod swap;`, the same way the supervision
//! tree shares `suptree.rs`.
//!
//! # The shape
//!
//! ```text
//!                     ┌──── the stable name: one endpoint, forever ────┐
//!    chatty ──CALL──► │                  SVC                           │ ◄──RECV_CAP── conx v1
//!   (client)          └────────────────────────────────────────────────┘ ◄──RECV_CAP── cconx v2
//! ```
//!
//! There is no process in that data path. The client's capability never changes, the client's loop
//! never branches, and the swap is a change in *who is parked in `RECV_CAP`*. That is the whole
//! trick, and it is DECISIONS §12's endpoint-only naming cashed in: a client names an endpoint and
//! never a peer, so the peer is free to be somebody else tomorrow.
//!
//! # The one thing the component owns that cannot be shared
//!
//! The UART's registers. Two processes writing one device's registers is the interleaving hazard
//! the roadmap's step 2 exists for, so the operator takes the registers back with
//! `Frame::REVOKE` on the device capability (DECISIONS §41) between quiescing the old instance and
//! endowing the new one. The old instance is asked to touch them one more time afterwards, and the
//! kernel's fault message is the receipt.

use user_rt::invoke;

// ===========================================================================================
// The service protocol. Every request is a `CALL` on the stable endpoint; the component serves it
// with `RECV_CAP` and answers through the kernel's one-shot `Reply` capability (DECISIONS §12).
// ===========================================================================================

/// `call(SVC, OP_PUT, seq)` -> `(digest(seq), (version << 32) | seq)`.
///
/// Two words out, two words back, and the reply carries **who answered**. A client that only wanted
/// the answer would ignore the version word entirely; `chatty` reads it because the version word is
/// the only way anything outside the operator can tell that a swap happened at all.
pub const OP_PUT: u64 = 1;

/// `call(SVC, OP_QUIESCE, 0)` -> `(QUIESCED, served)`. The operator's in-band drain.
///
/// **It travels on the endpoint being drained, and that is the mechanism, not a shortcut.** The
/// endpoint's sender queue is FIFO, so by the time this request reaches the component every request
/// queued ahead of it has already been served and answered. There is no separate "have you finished"
/// protocol, no quiescence timeout, and no window in which the operator has to guess.
pub const OP_QUIESCE: u64 = 2;

/// The `OP_QUIESCE` answer's first word. A distinctive constant rather than `0`, so a reply that
/// was never written cannot be mistaken for an acknowledgement.
pub const QUIESCED: u64 = 0x5155_4954; // "QUIT"

/// What a component replies to a request it does not understand. Never seen in a healthy run; it is
/// here so that a protocol mismatch is a value the client can assert on rather than a hang.
pub const BAD_REQUEST: u64 = u64::MAX;

// ===========================================================================================
// The broker protocol: the latency ladder's middle rung (`broker`).
//
// **Opt-in per channel, never the default.** A producer that chooses this rung speaks the same
// `OP_PUT` on the front endpoint and gets one of two answers: the backend's own reply (steady
// state, pass-through) or `ACCEPTED` (the backend is down and the broker took custody). The
// control messages travel in band on the same endpoint, for the same reason `OP_QUIESCE` does:
// synchronous rendezvous means a server blocks on one endpoint, and a second endpoint would need a
// wait-any primitive the kernel deliberately does not have (DECISIONS §26.5).
// ===========================================================================================

/// `call(FRONT, BOP_DOWN, 0)` -> `(0, depth)`. Stop forwarding; buffer from here on.
pub const BOP_DOWN: u64 = 10;
/// `call(FRONT, BOP_UP, 0)` -> `(0, drained)`. Drain the backlog to the backend, in order, then
/// resume pass-through.
pub const BOP_UP: u64 = 11;

/// The broker took custody of an item instead of answering it. `w1` = the queue depth after it.
pub const ACCEPTED: u64 = 0x4143_4350; // "ACCP"
/// The broker's buffer is full. The producer's request is refused rather than silently dropped:
/// backpressure is a value, not a policy hidden inside a server.
pub const QUEUE_FULL: u64 = 0x4655_4c4c; // "FULL"

// ===========================================================================================
// The report protocol. Every process holds a WRITE view of one report endpoint and says what it did
// on it; the kernel test is the receiver. Mirrored in kernel/src/user.rs (`live_swap_tests`), the
// same convention `authority_tests` and `c_seam_tests` follow: userspace owns the definition.
// ===========================================================================================

/// An instance started. `w1` = its version, `w2` = 1 if it could read the device's registers.
pub const RPT_UP: u64 = 1;
/// An instance answered `OP_QUIESCE`. `w1` = version, `w2` = how many requests it served.
pub const RPT_QUIESCED: u64 = 2;
/// **A failure, reported positively.** The old instance was told to touch the device *after* the
/// operator revoked it, and the access succeeded. `w1` = version. A healthy run never sends this;
/// the kernel's fault message arrives instead.
pub const RPT_PROBE_SURVIVED: u64 = 3;
/// The operator finished a step of the swap. `w1` = the step (see [`step`]), `w2` = detail.
pub const RPT_STEP: u64 = 4;
/// The operator's own verdict, read out of the shared log page after everything is over.
/// `w1` = a bitmap of [`log_checks`], `w2` = the sequence number the version changed at.
pub const RPT_LOG: u64 = 5;
/// The client's own verdict, computed inside the client from the replies it received.
/// `w1` = a bitmap of [`client_checks`], `w2` = the sequence number the version changed at.
pub const RPT_CLIENT: u64 = 6;
/// The attacker's result. `w1` = the negated error the kernel gave it, `w2` = the op it tried.
pub const RPT_ATTACK: u64 = 7;
/// The broker drained its backlog to a fresh backend. `w1` = items drained now, `w2` = items it
/// ever took custody of. Both are the queue rung's whole claim: nothing was lost while the backend
/// did not exist.
pub const RPT_DRAINED: u64 = 10;
/// A death reached the operator. `w1` = tid, `w2` = event.
pub const RPT_DEATH: u64 = 8;
/// Where that death happened. `w1` = pc, `w2` = fault address.
pub const RPT_SITE: u64 = 9;
/// Something could not be built or wired. `w1` = a stage code, so a broken system is debuggable
/// rather than silent.
pub const RPT_FAILED: u64 = 99;

/// The operator's steps, as they complete. The roadmap's four, plus the drain that has to happen
/// before the revoke can be safe.
pub mod step {
    /// The replacement is built and endowed, but not started: it cannot race the incumbent for
    /// requests, because a thread that has never been started is in nobody's queue.
    pub const BUILT: u64 = 1;
    /// The incumbent answered `OP_QUIESCE` and stopped receiving. Detail = requests it served.
    pub const DRAINED: u64 = 2;
    /// The device capability was revoked from every holder but the operator.
    pub const REVOKED: u64 = 3;
    /// The replacement holds the registers and is running. The down window ends here.
    pub const STARTED: u64 = 4;
    /// The corpse was collected through the supervision endpoint (DECISIONS §32).
    pub const REAPED: u64 = 5;
}

/// The operator's verdict bits, computed from the shared log page after the run.
pub mod log_checks {
    /// Every sequence number in `[0, REQUESTS)` was served by somebody. No request was lost in the
    /// down window: they parked on the endpoint's sender queue and the new instance drained them.
    pub const NO_GAP: u64 = 1 << 0;
    /// The version recorded per sequence number never goes backwards. **This is the "never two
    /// owners" assertion**: two instances serving concurrently would interleave, and an interleave
    /// is a v1 after a v2.
    pub const MONOTONE: u64 = 1 << 1;
    /// Both versions appear, so the swap really happened inside the conversation rather than
    /// before it started or after it ended.
    pub const BOTH_VERSIONS: u64 = 1 << 2;
    /// The incumbent's post-revoke probe faulted, at the device's own virtual address, and the
    /// kernel said so. The receipt for the revoke.
    pub const REVOKE_ENFORCED: u64 = 1 << 3;
}

/// The client's verdict bits. Computed inside the client, from what the client itself observed;
/// nothing here is taken on the operator's word.
pub mod client_checks {
    /// Every call returned. None failed, none was refused, none had to be retried.
    pub const ALL_REPLIED: u64 = 1 << 0;
    /// Every reply echoed the sequence number of the request that asked for it.
    pub const SEQ_ECHOED: u64 = 1 << 1;
    /// Every reply's digest matched the client's own independent computation of the same
    /// definition. **The contract held across the swap**, not merely the connection.
    pub const DIGEST_CORRECT: u64 = 1 << 2;
    /// The version word changed exactly once, and only upwards.
    pub const ONE_TRANSITION: u64 = 1 << 3;
    /// The client saw both versions, so its conversation genuinely spans the swap.
    pub const SPANNED_SWAP: u64 = 1 << 4;
    /// **The queued rung only.** At least one call came back `ACCEPTED` rather than with an answer,
    /// so the producer really did keep running through a window in which no backend existed. Without
    /// this bit a run in which the swap happened between two calls would look identical.
    pub const WAS_BUFFERED: u64 = 1 << 5;
    /// **The queued rung only.** Every call that got a real answer got a *correct* one, and every
    /// call that did not got `ACCEPTED`. Nothing was refused, and the queue never overflowed.
    pub const NONE_REFUSED: u64 = 1 << 6;
}

// ===========================================================================================
// The wiring: pages and addresses every process in the system agrees on.
// ===========================================================================================

pub const PAGE: u64 = 4096;

/// The shared log page, mapped read/write into the operator and into each instance. One byte per
/// sequence number, holding the version of whoever served it. The operator's witness, in the
/// operator's own address space, written by processes that never see each other.
pub const LOG_VA: u64 = 0x0300_0000;

/// The device's registers, at the same virtual address in every instance. The same number on both
/// sides is what lets the kernel's reported fault address be compared directly against this
/// constant, with no translation step to get wrong.
pub const DEV_VA: u64 = 0x0310_0000;

/// Where the operator copies an instance's program image, so a component's builder holds one ELF
/// rather than the whole initrd.
pub const IMAGE_VA: u64 = 0x3000_0000;

/// How many requests the client makes. Small enough to fit one log page, large enough that the swap
/// lands well inside the conversation.
pub const REQUESTS: u64 = 64;

/// The sequence number at which the incumbent tells the operator to start swapping. A third of the
/// way in, so there is a real conversation on both sides of the swap.
pub const SWAP_TRIGGER: u64 = 20;

/// The versions. Two, and they are also the two implementations: v1 computes the digest in Rust,
/// v2 computes it in C (DECISIONS §31's seam). A client that cannot tell them apart except by the
/// version word is the milestone's claim in its strongest form.
pub const V1: u64 = 1;
pub const V2: u64 = 2;

/// `chatty`'s roles. One binary, because an attacker that shares the honest client's code and
/// capabilities is a fair test of the boundary rather than a different program failing for its own
/// reasons.
pub const ROLE_CLIENT: u64 = 0;
pub const ROLE_USURPER: u64 = 1;
/// The producer on the queued channel: the same conversation, one rung up the latency ladder.
pub const ROLE_PRODUCER: u64 = 2;

/// `swapper`'s roles. Two systems, one operator, because they share every helper: the loader, the
/// endowments, the log page and the reporting.
pub const ROLE_DIRECT: u64 = 0;
pub const ROLE_QUEUED: u64 = 1;

/// Where the queued channel's log entries start, so the two systems can share one witness page
/// without stepping on each other.
pub const BROKER_LOG_BASE: u64 = 128;

// ===========================================================================================
// The component's work, defined once so two implementations can be checked against each other.
// ===========================================================================================

/// FNV-1a over the eight little-endian bytes of `seq`. Deliberately trivial arithmetic: the point
/// is that a C implementation and a Rust implementation of the same eight lines must agree bit for
/// bit, so the client can check its server's answers without trusting either.
pub const fn digest(seq: u64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < 8 {
        h ^= (seq >> (8 * i)) & 0xff;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    h
}

/// Pack the second reply word: the answering instance's version above the sequence number it
/// answered. Two facts in one word because `CALL` returns two and the digest owns the other.
pub const fn tag(version: u64, seq: u64) -> u64 {
    (version << 32) | (seq & 0xffff_ffff)
}

pub const fn tag_version(w: u64) -> u64 {
    w >> 32
}

pub const fn tag_seq(w: u64) -> u64 {
    w & 0xffff_ffff
}

// ===========================================================================================
// The device probe.
// ===========================================================================================

/// **Read one register of the UART we were given.** A read, never a write, on purpose: this runs
/// inside a kernel test whose own output goes out of the same UART, and a component that printed
/// would garble the harness it is being judged by. A read proves the mapping is live, and after the
/// operator's revoke it faults exactly as a write would.
///
/// The register layout is the one architecture-specific fact a UART driver is *for*: aarch64's
/// `virt` has a PL011 (32-bit registers, the flag register at 0x18), RISC-V's has an NS16550 (byte
/// registers, the line status register at 0x05).
#[cfg(target_arch = "aarch64")]
pub fn probe_device() -> u64 {
    // SAFETY: DEV_VA is our device mapping of the PL011, handed to us at build time. After the
    // operator revokes it this read faults, which is the point of the call site that does it last.
    unsafe { core::ptr::read_volatile((DEV_VA + 0x18) as *const u32) as u64 }
}

/// The NS16550 twin (RISC-V): byte registers, the line status register at 0x05.
#[cfg(target_arch = "riscv64")]
pub fn probe_device() -> u64 {
    // SAFETY: as the aarch64 arm.
    unsafe { core::ptr::read_volatile((DEV_VA + 0x05) as *const u8) as u64 }
}

// ===========================================================================================
// The shared page, as bytes.
// ===========================================================================================

/// The log page, as a slice. Volatile accessors rather than a plain slice because two address
/// spaces write it and one reads it, and the reads are ordered against the writes by IPC (the
/// operator only ever reads after a message from the writer has come through the kernel).
pub fn log_put(seq: u64, version: u64) {
    // SAFETY: the log page is mapped read/write at LOG_VA in every process that calls this.
    unsafe { core::ptr::write_volatile((LOG_VA as *mut u8).add(seq as usize), version as u8) };
}

pub fn log_get(seq: u64) -> u64 {
    // SAFETY: as `log_put`.
    unsafe { core::ptr::read_volatile((LOG_VA as *const u8).add(seq as usize)) as u64 }
}

// ===========================================================================================
// The component itself: one serving loop, two implementations of one function.
// ===========================================================================================

/// The capability layout every instance is built with. The operator's `Endow.caps` lists them in
/// this order, so they land in these slots.
pub const SVC: u64 = 0; // READ: requests arrive here. The stable name.
pub const RPT: u64 = 1; // WRITE: the record the test reads.
pub const NOTE: u64 = 2; // WRITE: the operator's own coordination channel.
pub const POKE: u64 = 3; // READ: what to do once quiesced.

/// **Touch the device one last time.** The operator sends this to an instance it has already
/// revoked, and the fault that follows is the receipt.
pub const POKE_PROBE: u64 = 1;
/// **Just go.** The operator sends this to an instance it is retiring cleanly, so its corpse can be
/// collected and its region returned. Distinct from `POKE_PROBE` because a probe that *succeeds* is
/// a test failure, and the last instance of a run still legitimately holds the device.
pub const POKE_QUIT: u64 = 2;

/// **Serve the stable endpoint until told to quiesce, then prove the revoke.**
///
/// `xform` is the only thing that differs between the two instances: v1 passes a Rust function, v2
/// passes one that calls into C. Everything else, including the capability layout and the wire
/// format, is this one function, which is what makes "the replacement is a different program" a
/// claim about the program and not about the harness.
pub fn serve(version: u64, xform: fn(u64) -> u64, log_base: u64, device: bool) -> ! {
    if device {
        // Before serving anything: can we reach the device we were endowed with? An instance that
        // could not would still answer every request correctly, and the swap would look perfect
        // while the hardware went unowned. A read that faults never returns, so reaching the next
        // line *is* the answer.
        let _ = probe_device();
    }
    user_rt::send(RPT, RPT_UP, version, device as u64);

    let mut served = 0u64;
    loop {
        let (op, slot, arg) = user_rt::recv_cap(SVC);
        if slot == abi::endpoint::NO_CAP {
            continue; // a plain SEND slipped in; the contract says CALL, and there is nobody to answer
        }
        match op {
            OP_PUT => {
                log_put(log_base + arg, version);
                served += 1;
                user_rt::reply(slot, xform(arg), tag(version, arg));
                if served == SWAP_TRIGGER && version == V1 {
                    // Tell the operator the conversation is well under way. Sent *after* the reply,
                    // so the client is already waiting on its next call when the swap begins.
                    user_rt::send(NOTE, NOTE_SWAP_NOW, version, served);
                }
            }
            OP_QUIESCE => {
                user_rt::send(RPT, RPT_QUIESCED, version, served);
                user_rt::reply(slot, QUIESCED, served);
                break;
            }
            _ => {
                user_rt::reply(slot, BAD_REQUEST, 0);
            }
        }
    }

    // Quiesced. We no longer receive on the stable endpoint, so requests arriving from here on park
    // on its sender queue for whoever receives next. We wait for the operator to tell us what to do
    // with the corpse we are about to become.
    let (what, _, _) = user_rt::recv(POKE);
    if what == POKE_PROBE && device {
        // Touch the registers one last time. If the operator's revoke was real this faults, and the
        // kernel's fault message is the receipt. Reaching the line after it is the failure, and it
        // is reported positively: a silent success here would be indistinguishable from the healthy
        // run, in which this program simply stops existing at the read above.
        let _ = probe_device();
        user_rt::send(RPT, RPT_PROBE_SURVIVED, version, 0);
    }
    user_rt::exit()
}

/// The operator's coordination messages, on `NOTE`. Separate from the report endpoint on purpose:
/// the report endpoint is read by the test (it is the record), and mixing "tell the auditor" with
/// "tell the operator" on one channel would mean the operator either steals the record or cannot
/// hear its own system.
pub const NOTE_SWAP_NOW: u64 = 1;
/// The attacker has made its attempt, so the operator knows the run is complete rather than
/// missing a report.
pub const NOTE_ATTACK_DONE: u64 = 2;
/// A client finished its conversation. The operator waits for this before reading the witness page:
/// a log read while the conversation is still running would show the requests that have not been
/// made yet as requests nobody served.
pub const NOTE_CLIENT_DONE: u64 = 3;
/// The broker quiesced and is about to exit.
pub const NOTE_BROKER_DONE: u64 = 4;

/// Trap. A half-built system is not worth limping along, and a fault is legible: the kernel prints
/// the pc and the process dies where the mistake was.
pub fn fail() -> ! {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem))
    };
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    user_rt::exit()
}

/// A raw `RECV_CAP`, returning the kernel's answer rather than a message. The attacker uses it:
/// `user_rt::recv_cap` is written for a caller that is allowed to receive, and the whole question
/// here is what happens to one that is not.
pub fn try_recv_cap(slot: u64) -> i64 {
    // SAFETY: a plain syscall. If it succeeds we have stolen a request, which is the failure.
    unsafe { invoke(slot, abi::endpoint::RECV_CAP, 0, 0, 0) }
}
