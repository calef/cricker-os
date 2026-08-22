#![no_std]
//! **Live component replacement: the shared half** (milestone 23, DECISIONS §41).
//!
//! Four programs make up the hot-swap system (`swapper` the operator, `rust_swappable` and
//! `c_swappable` the two instances of the swappable component, `chatty` the client and the
//! attacker), and this is what they share: the wire protocol, the addresses of the pages they pass
//! between them, the digest both language implementations of the component must compute, and the
//! serving loop itself. Compiled into each binary with `#[path = "swap.rs"] mod swap;`, the same
//! way the supervision tree shares `suptree.rs`.
//!
//! # The shape
//!
//! ```text
//!                     ┌──── the stable name: one endpoint, forever ────┐
//!    chatty ──CALL──► │                  SVC                           │ ◄──RECV_CAP── rust_swappable v1
//!   (client)          └────────────────────────────────────────────────┘ ◄──RECV_CAP── c_swappable v2
//! ```
//!
//! There is no process in that data path. The client's capability never changes, the client's loop
//! never branches, and the swap is a change in *who is parked in `RECV_CAP`*. That is the whole
//! trick, and it is DECISIONS §12's endpoint-only naming cashed in: a client names an endpoint and
//! never a peer, so the peer is free to be somebody else tomorrow.
//!
//! # Examples
//!
//! **A caveat about this example first, because it is a limitation and not a footnote.** This crate
//! takes an unconditional `user_rt` dependency, so `script/test`'s host pass excludes it (see the
//! exclusion list in `xtask`, derived and checked by `script/lint`). The example below therefore runs
//! under `cargo test --doc -p swap_proto` on an aarch64 host and **is not checked by the gate**. It
//! is written as a real example rather than a fenced comment so that it is at least checkable; the
//! fix is to split the pure half out from the serving loop, which is a change no lane has made.
//!
//! What a client learns from one exchange, and it is two facts in two words: the answer, and *who
//! answered*. The second is the only way anything outside the operator can tell a swap happened.
//!
//! ```
//! use swap_proto::{digest, tag, tag_seq, tag_version};
//!
//! // v1 answers request 7.
//! let (r0, r1) = (digest(7), tag(1, 7));
//! assert_eq!(r0, digest(7)); // the client checks the arithmetic itself
//! assert_eq!(tag_version(r1), 1);
//! assert_eq!(tag_seq(r1), 7);
//!
//! // The operator swaps the component. The client's capability did not change, its loop did not
//! // branch, and the only observable difference is the version word.
//! let (r0_after, r1_after) = (digest(8), tag(2, 8));
//! assert_eq!(r0_after, digest(8)); // the same eight lines, in C this time, bit for bit
//! assert_eq!(tag_version(r1_after), 2);
//!
//! // Which is why the digest is deliberately trivial: two language implementations of one
//! // definition must agree, so the client can check its server without trusting either.
//! assert_ne!(digest(7), digest(8));
//! ```
//!
//! # The one thing the component owns that cannot be shared
//!
//! The UART's registers. Two processes writing one device's registers is the interleaving hazard
//! the roadmap's step 2 exists for, so the operator takes the registers back with
//! `Frame::REVOKE` on the device capability (DECISIONS §41) between quiescing the old instance and
//! endowing the new one. The old instance is asked to touch them one more time afterwards, and the
//! kernel's fault message is the receipt.
//!
//! Name: recorded (milestone 46, and notes/naming.md's crate section). The wire contract was
//! spelled four ways (`fs_proto`, `gfx_proto`, `netproto`, `line_editor::proto`) for one concept;
//! `*_proto` won on 2026-07-30 under DECISIONS §39, and `script/lint` has checked it since. That
//! rule plus the service the stem names produces this name, which is the whole of what `recorded`
//! claims: calef ruled on the rule, and never on this crate.
//! The stem is milestone 23's word for live replacement, which no record weighs against another.

// milestone 68's doc ratchet: every public item in this crate is documented, and
// `script/lint`'s -D warnings keeps it that way. See notes/doc-coverage.md for the
// crates that are not there yet.
#![warn(missing_docs)]

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

/// **The sequence number a wedging instance swallows**: it takes the request, files the caller's
/// one-shot `Reply` capability away, and stops answering *without dying* (milestone 23's third
/// residual; notes/hung-component.md).
///
/// **Keyed on the request rather than on a count, and that is what makes it deterministic.** A wedge
/// that fired after N requests would depend on how far the conversation had got when the operator
/// was ready, which is a race. This fires on the identity of one request, so the same thing happens
/// in the same place whatever order the scheduler picked, on either architecture, under TCG or HVF.
/// It sits between [`SWAP_TRIGGER`] and the end of the conversation so there is a real conversation
/// on both sides of the hang.
///
/// Zero means "never wedge", which is what the two healthy roles pass.
///
/// Provisional name (`wedge`), like everything else in this crate's report and role vocabulary. It
/// is at least the word this tree already uses for the condition: DECISIONS §26 calls it "alive but
/// wedged" and milestone 62's block calls its own stuck test "wedged".
pub const WEDGE_SEQ: u64 = 24;

/// **What a released caller gets instead of an answer.** Not an error code, because nothing failed
/// in a way the ABI can name: the caller's request was taken by a component that then stopped
/// answering, and the only thing in the system that could ever free it was that component's own
/// cooperation.
///
/// **This is the shape of the gap, and it is worth reading the constant as evidence.**
/// [`abi::Error::Gone`] covers a caller whose *endpoint* was destroyed; it does not reach a caller
/// whose *server is alive and silent*, because a caller parked awaiting a reply is woken by exactly
/// one thing (`sched::ipc_reply`, addressed by tid) and nothing else in the kernel wakes it. So the
/// release has to travel as an ordinary reply, from the component, in band. See
/// notes/hung-component.md.
pub const WEDGE_RELEASED: u64 = 0x5245_4c53; // "RELS"

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
// on it; the kernel test is the receiver. Mirrored in kernel/src/user/live_swap_tests.rs, the
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
/// **A component was refused before it was built** (milestone 23's manifest). `w1` = the
/// `component_plan::Refusal` code, `w2` = the contract's own index in the operator's order.
///
/// The manifest's control that must fail, and the reason it is a *report* rather than a fault: a
/// supervisor that cannot satisfy a declaration has a legible thing to say and a child it did not
/// build. The operator sends this before it starts anything at all, so the test can assert that the
/// refusal landed ahead of the first build step rather than after a half-wired component existed.
pub const RPT_REFUSED: u64 = 11;
/// **What the supervision domain looks like while a component is hung** (milestone 23's third
/// residual). `w1` = how many members the survey reported, `w2` = their states, packed by
/// [`survey_counts`].
///
/// The operator reads this out of `abi::endpoint::SURVEY` (milestone 126), which is the only view of
/// its own children it has. The report exists so the test can assert what the view **cannot** say.
pub const RPT_SURVEY: u64 = 12;

/// **Every member of the domain refused to be collected.** `w1` = how many the operator asked about,
/// `w2` = how many answered [`abi::Error::StillAlive`].
///
/// `Endpoint::REAP` (DECISIONS §32) is the supervisor's whole vocabulary over its domain, and it
/// refuses a thread that is not dead, on purpose: collecting a corpse is not killing. A hung
/// component is not a corpse, so the vocabulary is empty. The operator asks about **every** member
/// rather than about the one it suspects, because a survey returns a tid and nothing that says which
/// tid is which, and asking about all of them is the stronger statement anyway.
pub const RPT_UNCOLLECTABLE: u64 = 13;

/// **An instance stopped answering without dying.** `w1` = its version, `w2` = how many requests it
/// had served first.
///
/// Reported by the *operator*, after the instance told it on the coordination channel. That
/// announcement is scaffolding and the note says so: a real hang announces nothing, and what is
/// under test is what a supervisor can do once it knows, not how it found out. How it could find out
/// is the part behind a decision (notes/hung-component.md).
pub const RPT_WEDGED: u64 = 14;

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
    /// **The hung rung only.** One of this client's calls was swallowed by a component that stopped
    /// answering, and it came back [`super::WEDGE_RELEASED`] rather than an answer, later, when the
    /// operator got that component to let go.
    ///
    /// Without this bit the hung run would be indistinguishable from a run in which the component
    /// simply happened to be replaced between two calls, which is the ordinary swap and proves
    /// nothing new. With it, the client is saying: *I was stranded inside a `CALL`, and I was freed
    /// by the component that stranded me.*
    pub const WAS_RELEASED: u64 = 1 << 7;
}

/// **Pack the survey's state histogram into one report word.** Four counts, a byte each, in the
/// order [`abi::survey`] declares them, so a reader of `RPT_SURVEY` needs no second table.
///
/// A byte each is enough because `MAX_THREADS` is far below 256 and this operator's domain is three
/// children; a domain wider than a byte would saturate rather than wrap, which is a lie a test would
/// catch and a real monitor would not want. Hence the `min`.
pub const fn survey_counts(ready: u64, running: u64, blocked: u64, dead: u64) -> u64 {
    const fn saturate(n: u64) -> u64 {
        if n > 255 { 255 } else { n }
    }
    saturate(ready) | (saturate(running) << 8) | (saturate(blocked) << 16) | (saturate(dead) << 24)
}

/// The ready count packed into a [`survey_counts`] word.
pub const fn survey_ready(w: u64) -> u64 {
    w & 0xff
}
/// The running count packed into a [`survey_counts`] word.
pub const fn survey_running(w: u64) -> u64 {
    (w >> 8) & 0xff
}
/// The blocked count packed into a [`survey_counts`] word.
pub const fn survey_blocked(w: u64) -> u64 {
    (w >> 16) & 0xff
}
/// The dead count packed into a [`survey_counts`] word.
pub const fn survey_dead(w: u64) -> u64 {
    (w >> 24) & 0xff
}

// ===========================================================================================
// The wiring: pages and addresses every process in the system agrees on.
// ===========================================================================================

/// The page size every mapping below is sized and aligned to.
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
/// The C-computed digest.
pub const V2: u64 = 2;

/// `chatty`'s roles. One binary, because an attacker that shares the honest client's code and
/// capabilities is a fair test of the boundary rather than a different program failing for its own
/// reasons.
pub const ROLE_CLIENT: u64 = 0;
/// The attacker: the same code and capabilities as [`ROLE_CLIENT`], testing the boundary rather
/// than a program written to fail for its own reasons.
pub const ROLE_USURPER: u64 = 1;
/// The producer on the queued channel: the same conversation, one rung up the latency ladder.
pub const ROLE_PRODUCER: u64 = 2;

/// `swapper`'s roles. Three systems, one operator, because they share every helper: the loader, the
/// endowments, the log page and the reporting.
pub const ROLE_DIRECT: u64 = 0;
/// The queued channel's system: the producer/consumer variant, one rung up the latency ladder
/// from [`ROLE_DIRECT`].
pub const ROLE_QUEUED: u64 = 1;
/// **The hung component** (milestone 23's third residual, notes/hung-component.md). The direct
/// channel's system, with one difference: the incumbent stops answering instead of being drained, so
/// the operator has to run the swap against a component that never cooperates.
pub const ROLE_HUNG: u64 = 2;

/// Where the queued channel's log entries start, so the two systems can share one witness page
/// without stepping on each other.
pub const BROKER_LOG_BASE: u64 = 128;

// ===========================================================================================
// The capability half of the contract (milestone 23's manifest residual).
//
// §41's sentence is that any program which **speaks the protocol** and **holds the right
// capabilities** is the component. Everything above is the first half. Until this section existed
// the second half was a set of literal arrays inside `swapper`, which is what the roadmap block
// calls the defect in six words: "endowments are literals in the operator's source". A vendor's
// build of the same component was therefore not a drop-in, because what to hand it lived in the
// operator rather than with the contract.
//
// So the declarations live here, next to the wire format they belong to, and the operator wires
// from them. Two things follow that are worth stating rather than noticing:
//
//   - **A manifest belongs to the contract, not to the build.** What a console component needs is
//     decided by what a console component *is*, and a build that needed something else would not be
//     substitutable for one that did not. `rust_swappable` and `c_swappable` are wired from one
//     declaration, which is the drop-in claim in its smallest true form.
//   - **A role name is the component's, and the object is the supervisor's.** `CLIENT` asks to use an
//     endpoint it calls `service`; on the direct channel that is the shared service endpoint and on
//     the queued channel it is the broker's front endpoint. One declaration, two routings, and the
//     component cannot tell which it got.
//
// The role names are **provisional** (see this crate's report): a lane may not mint a name, and
// these are the words a reader meets first.
// ===========================================================================================

use component_plan::Direction::{Serve, Use};
use component_plan::{CapNeed, MapNeed, PageKind, Requirements};

/// **The pages one instance is built out of**: its segments (three for a debug build of a program
/// this small), its four-page stack, its address-space root, its TCB, its page tables and its
/// revocation log.
///
/// Thirty-two, roughly double what any of these programs uses. It was a constant in the operator
/// until milestone 23's manifest lane; it is here now because it is a fact about the thing being
/// built rather than about who builds it. It is also the one number in a manifest that is a property
/// of the **build** rather than of the contract, which `component_plan`'s `BUGS` section records.
pub const INSTANCE_PAGES: u64 = 32;

/// **The console component**: the thing that gets replaced. It serves the stable endpoint, owns a
/// UART, writes the witness page, and holds nothing it could build anything with.
///
/// The declared order of `caps` **is** the component's cspace slot order, which is why [`SVC`] and
/// its three siblings are derived from this value rather than written down beside it.
pub const CONSOLE: Requirements = Requirements {
    contract: "console",
    caps: &[
        CapNeed {
            role: "service",
            direction: Serve,
        },
        CapNeed {
            role: "report",
            direction: Use,
        },
        CapNeed {
            role: "operator",
            direction: Use,
        },
        CapNeed {
            role: "control",
            direction: Serve,
        },
    ],
    maps: &[
        MapNeed {
            role: "witness",
            va: LOG_VA,
            kind: PageKind::Shared,
        },
        MapNeed {
            role: "uart",
            va: DEV_VA,
            kind: PageKind::DeviceRegisters,
        },
    ],
    pages: INSTANCE_PAGES,
};

/// **The same programs behind a queue broker, with no device.** A different contract rather than a
/// configuration of [`CONSOLE`], and §41 says why: the backend behind a broker is a plain service,
/// and mixing the device story into the queue story would make it unclear which mechanism carried
/// which claim. The capability half of the two contracts is identical; the device is the difference.
pub const BACKEND: Requirements = Requirements {
    contract: "backend",
    caps: CONSOLE.caps,
    maps: &[MapNeed {
        role: "witness",
        va: LOG_VA,
        kind: PageKind::Shared,
    }],
    pages: INSTANCE_PAGES,
};

/// **The client** (`chatty`, in all three of its roles). It *uses* the service and never serves it,
/// which is the whole of why the attacker role cannot become the server: `Use` is `WRITE`, and there
/// is no longer anywhere for an operator to type `READ` by mistake.
pub const CLIENT: Requirements = Requirements {
    contract: "client",
    caps: &[
        CapNeed {
            role: "service",
            direction: Use,
        },
        CapNeed {
            role: "report",
            direction: Use,
        },
        CapNeed {
            role: "operator",
            direction: Use,
        },
    ],
    maps: &[],
    pages: INSTANCE_PAGES,
};

/// **The queue broker**, the latency ladder's opt-in rung. It serves the endpoint producers hold and
/// uses the one a backend serves, which is the only manifest here that names both directions on the
/// same channel.
pub const BROKER: Requirements = Requirements {
    contract: "broker",
    caps: &[
        CapNeed {
            role: "requests",
            direction: Serve,
        },
        CapNeed {
            role: "backend",
            direction: Use,
        },
        CapNeed {
            role: "report",
            direction: Use,
        },
        CapNeed {
            role: "operator",
            direction: Use,
        },
    ],
    maps: &[],
    pages: INSTANCE_PAGES,
};

/// Every declaration in this crate is well formed, checked at compile time on both architectures.
/// A role declared twice or two pages at one address would otherwise be a component reading the
/// wrong slot, or one mapping silently winning, with nothing to see at run time.
const _: () = assert!(CONSOLE.problem().is_none());
const _: () = assert!(BACKEND.problem().is_none());
const _: () = assert!(CLIENT.problem().is_none());
const _: () = assert!(BROKER.problem().is_none());

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

/// The answering instance's version packed into a [`tag`] word.
pub const fn tag_version(w: u64) -> u64 {
    w >> 32
}

/// The sequence number packed into a [`tag`] word.
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

/// Read back the version byte [`log_put`] wrote at `seq`.
pub fn log_get(seq: u64) -> u64 {
    // SAFETY: as `log_put`.
    unsafe { core::ptr::read_volatile((LOG_VA as *const u8).add(seq as usize)) as u64 }
}

// ===========================================================================================
// The component itself: one serving loop, two implementations of one function.
// ===========================================================================================

/// **The capability layout every instance is built with, derived from [`CONSOLE`].**
///
/// These four used to be written here as `0, 1, 2, 3` with a comment saying that the operator's
/// `ChildEndowment.caps` listed them in the same order, and that comment was the only thing holding
/// the two files together: a reordered array in `swapper` would have produced a component receiving
/// on its report channel, with nothing to see but a hang. Now the number **is** the position in the
/// declaration, computed at compile time, and a role this component does not declare does not
/// compile.
pub const SVC: u64 = component_plan::slot_of(&CONSOLE, "service");
/// WRITE: the record the test reads. See [`SVC`].
pub const RPT: u64 = component_plan::slot_of(&CONSOLE, "report");
/// WRITE: the operator's own coordination channel. See [`SVC`].
pub const NOTE: u64 = component_plan::slot_of(&CONSOLE, "operator");
/// READ: what to do once quiesced. See [`SVC`].
pub const POKE: u64 = component_plan::slot_of(&CONSOLE, "control");

/// **One binary serves both contracts, so both must put its capabilities in the same slots.** That
/// was true by inspection of two literal arrays in the operator; it is a compile error to break it
/// now. `serve` reads [`SVC`] whichever channel it was started on, and [`BACKEND`] is a separate
/// declaration that could drift.
const _: () = assert!(component_plan::slot_of(&BACKEND, "service") == SVC);
const _: () = assert!(component_plan::slot_of(&BACKEND, "report") == RPT);
const _: () = assert!(component_plan::slot_of(&BACKEND, "operator") == NOTE);
const _: () = assert!(component_plan::slot_of(&BACKEND, "control") == POKE);

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
///
/// `wedge` is [`WEDGE_SEQ`] on the hung channel and **zero everywhere else**, which is the whole of
/// how that channel differs: the same program, the same contract, one request it never answers.
pub fn serve(version: u64, xform: fn(u64) -> u64, log_base: u64, device: bool, wedge: u64) -> ! {
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
            // **Stop answering, without dying** (milestone 23's third residual,
            // notes/hung-component.md). Everything a supervisor in this tree can notice is a
            // *death*: a fault or an exit, a kernel-stamped message on the supervision endpoint, a
            // corpse to reap, a region that comes home. None of that happens here. This process
            // keeps its endpoint, keeps its device, stays `Blocked`, and every mechanism in the
            // system reads it as a healthy server between requests, **because that is what a healthy
            // server between requests looks like.**
            //
            // The hang is a `CALL` on the coordination channel that the operator never replies to,
            // which is the commonest real hang there is: blocked awaiting a peer that will not
            // answer. It is also the only shape whose blocked-ness is *provable* rather than raced.
            // A `SEND` followed by a `RECV` would leave a window in which this thread was still
            // `Ready`, and the operator surveying inside that window would read a state it must not
            // be able to read (see notes/hung-component.md, "why the wedge is a CALL").
            //
            // What is deliberately *not* done first: no `log_put`, so the witness page carries a
            // real gap at this sequence number, and no `reply`, so the caller stays parked awaiting
            // a reply that is not coming. Both are facts the operator reads afterwards.
            OP_PUT if wedge != 0 && arg == wedge => {
                let (what, _) = user_rt::call(NOTE, NOTE_WEDGED, served);
                // Reached only because the operator answered, which in a real hang it cannot do.
                // **The caller first, then the device**, because the device read is expected to
                // fault and a fault takes this reply capability to the grave with the cspace holding
                // it, leaving that caller blocked for the life of the machine.
                if what == NOTE_RELEASE {
                    user_rt::reply(slot, WEDGE_RELEASED, 0);
                }
                if device {
                    let _ = probe_device();
                    user_rt::send(RPT, RPT_PROBE_SURVIVED, version, 0);
                }
                user_rt::exit()
            }
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
/// **This instance has stopped answering and is not coming back on its own.** `w1` = version,
/// `w2` = requests served first. **A `CALL`, not a `SEND`**, and that is the mechanism rather than a
/// style choice: the `CALL` is what parks the instance, so it is hung *because* it announced.
///
/// The operator serves this one message with `RECV_CAP` and keeps the reply capability, which is the
/// only handle anything in the system has on a wedged component. Everything the operator does next
/// is done while that component is genuinely, provably stuck.
///
/// The announcement itself is scaffolding and must be read as such: a component that announces its
/// own hang is not a hang anyone had to detect. It is here because *detection* is behind a decision
/// (a deadline needs a timed wait, which is milestone 106's fork) while *what a supervisor can do
/// about a hang it already knows about* is not, and the second is what this run measures.
/// notes/hung-component.md says which is which and why the announcement does not weaken the
/// assertions that follow it.
pub const NOTE_WEDGED: u64 = 5;
/// **The operator's answer to [`NOTE_WEDGED`]: let go.** The only thing in this system that can free
/// a caller stranded inside a `CALL`, and it has to travel this way round.
///
/// The operator cannot answer the stranded caller itself. The one-shot `Reply` capability the kernel
/// minted names that caller, lives in the *component's* cspace, carries `WRITE` without `GRANT`, and
/// is consumed on use (DECISIONS §12). So the operator cannot be handed it, cannot forge it, and
/// cannot reach it by revoking anything. Freeing a stranded caller requires the cooperation of the
/// component whose lack of cooperation is the definition of the hang, which is the finding rather
/// than a limitation of this fixture.
pub const NOTE_RELEASE: u64 = 6;

/// Trap. A half-built system is not worth limping along, and a fault is legible: the kernel prints
/// the pc and the process dies where the mistake was.
///
/// Delegates to [`user_rt::trap`] since milestone 130, for the reason `supervision_proto::fail`
/// records: the name earns its keep, the duplicated asm did not.
pub fn fail() -> ! {
    user_rt::trap()
}

/// A raw `RECV_CAP`, returning the kernel's answer rather than a message. The attacker uses it:
/// `user_rt::recv_cap` is written for a caller that is allowed to receive, and the whole question
/// here is what happens to one that is not.
pub fn try_recv_cap(slot: u64) -> i64 {
    // SAFETY: a plain syscall. If it succeeds we have stolen a request, which is the failure.
    unsafe { invoke(slot, abi::endpoint::RECV_CAP, 0, 0, 0) }
}
