//! **The wall-clock contract** (milestone 51 lane A; DECISIONS §43, notes/clock.md).
//!
//! One definition of the three things the parties to wall-clock time have to agree on, so the
//! clock service, its readers, the std PAL and the kernel-side tests cannot drift: the layout of
//! the **shared clock page**, the small **propose protocol**, and the **policy** the service
//! applies to a proposal. The same split `fs_proto` makes for the filesystem and `gfx_proto` for
//! the framebuffer.
//!
//! # Three authorities, three different objects
//!
//! ```text
//!                              the RTC's registers (a DeviceFrame, one holder)
//!                                        │
//!                              ┌─────────▼────────┐
//!    propose ──an endpoint────►│  clock service   │
//!   (bounded, policy applies)  └─────────┬────────┘
//!                                        │ writes
//!                              ┌─────────▼────────┐
//!                              │  the clock page  │◄── set: the SAME page, mapped read/WRITE
//!                              └─────────┬────────┘
//!                                        │ mapped read-only
//!                                    readers
//! ```
//!
//! - **Read** is a **read-only mapping of the clock page** plus the ambient monotonic counter.
//!   No endpoint, no syscall, no server round trip: reading the time costs two loads and an add.
//!   A process with no such mapping does not know what time it is, and can say so.
//! - **Set** is a **read/write mapping of the same page**. Writing the offset *is* setting the
//!   clock; nothing polices it, which is what makes it the authority.
//! - **Propose** is an **endpoint** the service serves. A proposer holds no writable page, so the
//!   only thing it can do is ask, and [`policy::decide`] is what answers.
//!
//! The rights ladder is therefore the kernel's own: no capability, a `Frame` with `READ`, a
//! `Frame` with `WRITE`, an `Endpoint` with `WRITE`. Nothing new in the syscall surface, and the
//! authority a process holds is visible to `caps`.
//!
//! # Wall clock is counter plus offset
//!
//! `Instant` stays the raw monotonic counter, ambient and untouched. Wall-clock time is
//! [`wall_nanos`]: the counter converted to nanoseconds, plus an **offset**, which is the only
//! thing anyone ever writes. So adjusting the wall clock cannot perturb monotonic time **by
//! construction rather than by discipline**: a step is an offset write, and the counter never
//! sees it.
//!
//! # Examples
//!
//! Setting the wall clock is writing an offset, and the monotonic counter is not in the expression.
//! That is the section above stated as code, and it is the reason a clock step here cannot make
//! `Instant` go backwards the way a Unix `settimeofday` once could:
//!
//! ```
//! use clock_proto::{NANOS_PER_SEC, offset_for, wall_nanos};
//!
//! // The machine has been up for 5 seconds and the service has just learned the wall time.
//! let monotonic = 5 * NANOS_PER_SEC;
//! let now = 1_800_000_000 * NANOS_PER_SEC;
//! let offset = offset_for(now, monotonic);
//! assert_eq!(wall_nanos(offset, monotonic), now);
//!
//! // Two seconds later, with nobody having touched anything.
//! let monotonic = 7 * NANOS_PER_SEC;
//! assert_eq!(wall_nanos(offset, monotonic), now + 2 * NANOS_PER_SEC);
//!
//! // An operator steps the clock a minute forward. Only the offset changes.
//! let stepped = offset_for(now + 60 * NANOS_PER_SEC, monotonic);
//! assert_eq!(wall_nanos(stepped, monotonic), now + 60 * NANOS_PER_SEC);
//! assert_eq!(monotonic, 7 * NANOS_PER_SEC); // untouched, and there is no way to touch it
//! ```
//!
//! The other thing worth showing is **what a compromised network time client can and cannot do**,
//! because that is what makes the propose endpoint safe to hand out. It can lie inside
//! [`policy`]'s bounds and it can do nothing else, and the bounds are deliberately asymmetric:
//! forward skips instants nobody has observed, backward makes instants happen twice.
//!
//! ```
//! use clock_proto::{NANOS_PER_SEC, policy, state, status};
//!
//! let now = 1_800_000_000 * NANOS_PER_SEC;
//!
//! // A small correction, in either direction, is what a time client is for.
//! assert_eq!(policy::decide(state::SYNCED, now, now + NANOS_PER_SEC / 2), status::ACCEPTED);
//!
//! // Walking the clock past a certificate expiry in one step is the classic attack.
//! assert_eq!(
//!     policy::decide(state::SYNCED, now, now + 2 * policy::MAX_STEP_FORWARD_NANOS),
//!     status::REFUSED_TOO_FAR_FORWARD,
//! );
//!
//! // And the tighter half: a second forward is fine, a second and a half backward is not.
//! assert_eq!(policy::decide(state::SYNCED, now, now + 3 * NANOS_PER_SEC / 2), status::ACCEPTED);
//! assert_eq!(
//!     policy::decide(state::SYNCED, now, now - 3 * NANOS_PER_SEC / 2),
//!     status::REFUSED_TOO_FAR_BACKWARD,
//! );
//!
//! // The bootstrap case: a machine that does not know the time has no belief for a step limit to
//! // protect, so a plausible proposal is accepted outright. The sanity window still applies.
//! assert_eq!(policy::decide(state::UNKNOWN, 0, now), status::ACCEPTED);
//! assert_eq!(policy::decide(state::UNKNOWN, 0, 0), status::REFUSED_IMPLAUSIBLE);
//! assert!(!policy::plausible(0)); // 1970 plus uptime is not a time this code can be running at
//! ```
//!
//! # Everything is nanoseconds since the Unix epoch, in a `u64`
//!
//! One unit everywhere, so no conversion sits at a boundary where it can be forgotten. A `u64` of
//! nanoseconds runs out in the year 2554, which is recorded rather than defended: it is past every
//! horizon this project has, and picking `u64` keeps the wire words, the page words and the
//! arithmetic identical.
//!
//! Name: recorded (milestone 46, and notes/naming.md's crate section). The wire contract was
//! spelled four ways (`fs_proto`, `gfx_proto`, `netproto`, `line_editor::proto`) for one concept;
//! `*_proto` won on 2026-07-30 under DECISIONS §39, and `script/lint` has checked it since. That
//! rule plus the service the stem names produces this name, which is the whole of what `recorded`
//! claims: calef ruled on the rule, and never on this crate.
//! The stem is the service's own word (`clock`, DECISIONS §43), which is itself unrecorded.

#![cfg_attr(not(test), no_std)]

// The atomics are loom's under `--cfg loom` and the real ones otherwise, which is what lets
// `script/interleaving-check` replay this seqlock under every interleaving the C11 model permits
// (milestone 80; notes/interleaving.md). The algorithms below are written once and are the ones the
// clock service runs: only where the words live differs, and that is not what is being checked.
#[cfg(not(loom))]
use core::sync::atomic::{AtomicU64, Ordering, fence};

#[cfg(loom)]
use loom::sync::atomic::{AtomicU64, Ordering, fence};

/// Give the model checker a scheduling point inside a spin, and the CPU a hint outside one.
///
/// A seqlock spins while the other side holds the sequence. On real hardware that is a few
/// instructions and `spin_loop` is the right hint. Under loom it is a **liveness hazard**: loom's
/// scheduler is cooperative, and a thread that spins without yielding can starve the writer whose
/// progress the spin is waiting for, so the model never terminates. `yield_now` tells loom this is
/// a point where the other thread must be allowed to run, which is exactly what the hardware does
/// for free.
#[inline]
fn spin_hint() {
    #[cfg(loom)]
    loom::thread::yield_now();
    #[cfg(not(loom))]
    core::hint::spin_loop();
}

/// Nanoseconds in a second, the conversion every reader does exactly once.
pub const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Wall-clock nanoseconds, from the offset the clock page carries and the monotonic nanoseconds
/// the reader measured for itself.
///
/// Saturating rather than wrapping: an implausible offset should read as an implausible time, not
/// as a plausible one on the other side of the wrap.
pub const fn wall_nanos(offset_nanos: u64, monotonic_nanos: u64) -> u64 {
    offset_nanos.saturating_add(monotonic_nanos)
}

/// The offset that makes `monotonic_nanos` read as `wall_nanos`: what the clock service writes
/// when it learns the time. The inverse of [`wall_nanos`].
pub const fn offset_for(wall_nanos: u64, monotonic_nanos: u64) -> u64 {
    wall_nanos.saturating_sub(monotonic_nanos)
}

// ================================================================================================
// What the machine knows about the time, which includes "nothing".
// ================================================================================================

/// **The states the wall clock can be in, and one of them is "I do not know".**
///
/// This is DECISIONS §42's no-silent-degradation rule on a second axis. A machine with no RTC, or
/// with an RTC reporting something impossible, must say so; confidently reporting 1970 plus uptime
/// is the failure, because the caller cannot tell it from a real answer.
pub mod state {
    /// **The wall clock is unknown.** No clock page, no clock service, an RTC that is absent, or an
    /// RTC whose reading failed [`super::policy::plausible`]. The offset is meaningless and must
    /// not be used. This is also what a zeroed page reads as, so a page nobody has published to is
    /// honest by default rather than by initialisation.
    pub const UNKNOWN: u64 = 0;
    /// Set once at startup from the hardware RTC. As good as the battery-backed clock on the board,
    /// which on QEMU is the host's clock and on a real board is whatever the coin cell managed.
    pub const RTC: u64 = 1;
    /// Set outright by an authority holding the page read/write: an operator, or the service's own
    /// startup on a machine where that is the only source.
    pub const SET: u64 = 2;
    /// Set from a **proposal the service accepted**, which is where a network time client's work
    /// lands. Distinguished from [`SET`] because "an external source I bounded" and "a human told
    /// me" are different provenance, and the difference is exactly what a caller deciding whether
    /// to trust a certificate expiry wants.
    pub const SYNCED: u64 = 3;

    /// Whether a state means the machine actually knows the time.
    pub const fn known(state: u64) -> bool {
        state != UNKNOWN
    }
}

// ================================================================================================
// The clock page: the read authority, and the set authority, are mappings of these thirty-two bytes.
// ================================================================================================

/// The page's first word, so a reader can tell a published clock page from a zeroed frame or from
/// somebody else's page. ASCII `CLOCKv01`, big-endian in the source so it reads in a hex dump.
pub const MAGIC: u64 = 0x434c_4f43_4b76_3031;

/// The words the clock page uses. The rest of the frame is reserved and must read as zero: a
/// future field is a new index here, and an old reader that never learned about it is unaffected.
pub const WORDS: usize = 4;

/// Word indices, named so the layout is one list rather than four scattered offsets.
const W_MAGIC: usize = 0;
const W_SEQ: usize = 1;
const W_STATE: usize = 2;
const W_OFFSET: usize = 3;

/// What a reader gets out of the page in one consistent look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// One of [`state`]'s values.
    pub state: u64,
    /// Wall-clock nanoseconds at monotonic zero. Meaningless unless [`state::known`].
    pub offset_nanos: u64,
    /// How many times the page has been published to, which is `seq / 2`. A reader that cares
    /// whether the clock stepped under it (a log, a cache with an expiry) compares this across two
    /// readings instead of comparing timestamps and guessing.
    pub generation: u64,
}

/// **The clock page**, as seen through one process's mapping of it.
///
/// A seqlock, because the readers are many, the writers are few, and a reader must never block a
/// writer or vice versa: there is no lock a process could hold across an address-space boundary
/// anyway, and a torn read of a 128-bit-wide state is a wrong time rather than a crash, which is
/// the worst kind of bug to leave possible.
///
/// **The memory ordering is the point, not decoration** (the project's rule 4: assume weak
/// ordering, because we are on ARM and RISC-V and that is a gift). The writer's data stores must
/// not be visible before it has claimed the sequence, and must all be visible before it releases
/// it; the reader's data loads must not be hoisted above the first sequence read nor sunk below
/// the second. On x86 a sloppy version of this would pass every test forever and then fail on the
/// hardware we actually run.
///
/// **That last sentence was true of this code, and it took a model checker to find out** (milestone
/// 80, 2026-08-04). The writer claimed the sequence and then wrote the data with nothing ordering
/// the claim ahead of them, so a reader could observe the new offset while the sequence still read
/// even and unchanged, revalidate successfully, and return a state from one publish beside an
/// offset from another. A wrong wall clock, silently. It is not reachable on x86, QEMU's TCG
/// explores almost none of the orderings that produce it, and every host test and every emulated
/// boot passed throughout. The fix is a `fence(Release)` in [`publish`](Self::publish), commented
/// where it sits. See notes/interleaving.md.
///
/// Writers are **multiple** (the service, and whoever holds the page read/write), so claiming the
/// sequence is a compare-exchange rather than a plain store. Two writers racing is not a design we
/// encourage, but it is a design the capability layout permits, and a seqlock that assumed a single
/// writer would corrupt silently rather than serialise.
///
/// # BUGS
///
/// - **The orderings are checked against C11, not against ARM's model or RISC-V's.**
///   `script/interleaving-check` runs loom, which searches the C11 model: a tear it finds is real,
///   and a clean run is not a proof about either ISA. Litmus-level confidence would need herd7-style
///   tooling, and nothing in this tree yet reads this page from two physical cores under load.
/// - **A reader spins while a writer holds the sequence, and the spin has no bound.** A writer that
///   faults between its claim and its release leaves the page permanently odd and every reader
///   spinning in [`read`](Self::read). The window is four stores wide and the writer holds no lock,
///   so nothing can be preempted into a deadlock; a *killed* writer is simply not recovered from.
#[derive(Debug, Clone, Copy)]
pub struct ClockPage {
    base: *const AtomicU64,
}

// SAFETY: the whole point of the page is that several address spaces share it; every access below
// goes through atomics, so there is no non-atomic aliasing to protect against.
unsafe impl Send for ClockPage {}
// SAFETY: as for `Send` above: the page is shared by construction and every access goes through atomics, so there is no non-atomic aliasing to protect against.
unsafe impl Sync for ClockPage {}

impl ClockPage {
    /// Name the clock page mapped at `va`.
    ///
    /// # Safety
    ///
    /// `va` must be a mapped, 8-byte-aligned frame that is the clock page (or, for a writer about
    /// to call [`init`](Self::init), a zeroed frame that is about to become one), and it must stay
    /// mapped for as long as this value is used. Read-only is enough for [`read`](Self::read);
    /// [`publish`](Self::publish) and [`init`](Self::init) need it mapped read/write, and calling
    /// them on a read-only mapping faults the caller, which is the correct outcome: a process
    /// without the set authority cannot set the clock, and finds out immediately.
    pub const unsafe fn new(va: u64) -> Self {
        ClockPage {
            base: va as *const AtomicU64,
        }
    }

    fn word(&self, i: usize) -> &AtomicU64 {
        // SAFETY: `new`'s contract is a mapped frame; `i` is always one of the W_* constants, all
        // of which are inside WORDS and therefore inside the frame.
        unsafe { &*self.base.add(i) }
    }

    /// Stamp a fresh frame as a clock page in the unknown state. The writer does this once, before
    /// anyone else has a mapping, so it needs no sequence claim.
    ///
    /// The magic goes down **last**, with a release, so a reader that races the first publish sees
    /// either a page it does not recognise (and reports unknown) or a fully initialised one. Never
    /// a recognised page with garbage in it.
    pub fn init(&self) {
        self.word(W_SEQ).store(0, Ordering::Relaxed);
        self.word(W_STATE).store(state::UNKNOWN, Ordering::Relaxed);
        self.word(W_OFFSET).store(0, Ordering::Relaxed);
        self.word(W_MAGIC).store(MAGIC, Ordering::Release);
    }

    /// One consistent look at the page. Never blocks a writer, and never fails: a page without the
    /// magic reads as [`state::UNKNOWN`], which is the truth about a frame nobody has published to.
    pub fn read(&self) -> Reading {
        const UNKNOWN: Reading = Reading {
            state: state::UNKNOWN,
            offset_nanos: 0,
            generation: 0,
        };
        if self.word(W_MAGIC).load(Ordering::Acquire) != MAGIC {
            return UNKNOWN;
        }
        loop {
            let s1 = self.word(W_SEQ).load(Ordering::Acquire);
            if s1 & 1 != 0 {
                // A writer holds it. Spin: a publish is four stores long.
                spin_hint();
                continue;
            }
            let st = self.word(W_STATE).load(Ordering::Relaxed);
            let off = self.word(W_OFFSET).load(Ordering::Relaxed);
            // Keep the two data loads above the second sequence load. Without this the compiler or
            // the machine may reorder them after it, and the check would validate nothing. Removing
            // it fails the same three loom harnesses the writer's release fence does, so both
            // halves of the pair are checked rather than argued (notes/interleaving.md).
            //
            // PAIR: the writer's `W_SEQ.store(claimed + 2, Ordering::Release)` at the end of
            // `publish`, below in this file. **The only protocol in the tree where both halves must
            // be here**, because a reader is in a different address space and reaches this page with
            // no syscall at all: there is no IPC rendezvous underneath to supply the edge the way
            // there is for every other shared page (notes/memory-ordering.md).
            fence(Ordering::Acquire);
            if self.word(W_SEQ).load(Ordering::Relaxed) == s1 {
                return Reading {
                    state: st,
                    offset_nanos: off,
                    generation: s1 / 2,
                };
            }
        }
    }

    /// Write a new state and offset. **This is the set authority**: it needs nothing but a
    /// read/write mapping, because being able to write the offset is what setting the clock means.
    ///
    /// Returns the new generation.
    pub fn publish(&self, new_state: u64, offset_nanos: u64) -> u64 {
        let claimed = loop {
            let s = self.word(W_SEQ).load(Ordering::Relaxed);
            if s & 1 != 0 {
                spin_hint();
                continue;
            }
            // Acquire on success so our stores below cannot be hoisted above the claim. It is NOT
            // what makes the claim visible before them; see the release fence after this loop.
            if self
                .word(W_SEQ)
                .compare_exchange_weak(s, s + 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break s;
            }
        };
        // **The odd sequence must be visible BEFORE the data it protects.** This is the half of a
        // seqlock that a total-store-order machine gives you for free and a weakly ordered one does
        // not, and it was missing here until milestone 80's loom run found it (2026-08-04; see
        // notes/interleaving.md). Without it a reader can observe the new offset while the sequence
        // still reads even and unchanged, so its revalidation passes and it returns a state from
        // one publish beside an offset from another: a wrong wall clock, silently.
        //
        // A `fence(Release)` and not a stronger ordering on the claim, which is the part worth
        // knowing. `AcqRel` and even `SeqCst` on the compare-exchange were both tried and both
        // still tear: an acquire or release RMW orders accesses around *itself*, and what this
        // needs is its own store ordered ahead of the plain stores that follow. That is a
        // store-store barrier between the two, which is exactly the `smp_wmb()` Linux puts in
        // `write_seqcount_begin` for the same reason.
        //
        // PAIR: the reader's `fence(Ordering::Acquire)` in `read`, above in this file. That one is
        // the load-load half of the same barrier: this fence keeps the odd sequence ahead of the
        // data, and the reader's keeps its revalidating load of `W_SEQ` behind the data it is
        // revalidating. Neither one is sufficient alone, and milestone 80's loom run failed with
        // either removed (notes/interleaving.md, notes/memory-ordering.md).
        fence(Ordering::Release);
        self.word(W_STATE).store(new_state, Ordering::Relaxed);
        self.word(W_OFFSET).store(offset_nanos, Ordering::Relaxed);
        // Release: everything above is visible to any reader that sees the even sequence below.
        self.word(W_SEQ).store(claimed + 2, Ordering::Release);
        (claimed + 2) / 2
    }
}

// ================================================================================================
// The propose protocol: two words out, two words back, over one endpoint.
// ================================================================================================

/// **The propose protocol** (a proposer → the clock service), spoken over an endpoint `CALL`.
///
/// Deliberately tiny, and deliberately **not** a way to set the clock. Everything that arrives here
/// is a request the service is free to refuse, which is what makes the endpoint safe to hand to a
/// network time client: a compromised one can lie inside [`policy`]'s bounds and can do nothing
/// else at all.
pub mod propose {
    /// Where the opcode sits in the first `CALL` word: bits 63:56, the same position `fs_proto`
    /// and `line_editor::proto` use, so the contracts read alike.
    pub const OP_SHIFT: u32 = 56;

    /// Build a request's first word.
    pub const fn req(op: u64) -> u64 {
        op << OP_SHIFT
    }

    /// The opcode of a request word.
    pub const fn op(w0: u64) -> u64 {
        w0 >> OP_SHIFT
    }

    /// `CALL(req(PROPOSE), proposed_unix_nanos)`. Ask the service to move the wall clock to
    /// `proposed_unix_nanos`. Reply `r0` is one of the `status` codes and `r1` is
    /// the wall-clock nanoseconds in force afterwards (0 when the state is unknown), so a proposer
    /// learns what happened without needing a read mapping.
    pub const PROPOSE: u64 = 1;

    /// `CALL(req(STATE), 0)`. Ask what the clock knows. Reply `r0` is a [`state`](super::state)
    /// value and `r1` the wall-clock nanoseconds now (0 when unknown).
    ///
    /// Redundant for anyone holding the page, and that is fine: a proposer is exactly the process
    /// that may hold the endpoint and no mapping, and it has to know whether the clock is unknown
    /// (in which case its proposal bootstraps) or already running (in which case a step applies).
    pub const STATE: u64 = 2;
}

/// The reply's first word for a [`propose::PROPOSE`]. Not an errno space: this contract has no
/// POSIX behind it, and every refusal here is a policy answer rather than a failure.
pub mod status {
    /// The proposal was applied. The clock is now [`super::state::SYNCED`].
    pub const ACCEPTED: u64 = 0;
    /// Outside the sanity window entirely: [`super::policy::plausible`] says no machine running
    /// this code is at that instant. A proposal of 1970, or of 2038, lands here.
    pub const REFUSED_IMPLAUSIBLE: u64 = 1;
    /// Plausible in the absolute, but more than [`super::policy::MAX_STEP_FORWARD_NANOS`] ahead of
    /// what the clock already believes.
    pub const REFUSED_TOO_FAR_FORWARD: u64 = 2;
    /// Plausible in the absolute, but more than [`super::policy::MAX_STEP_BACKWARD_NANOS`] behind
    /// what the clock already believes. The asymmetry with forward is deliberate; see [`policy`](super::policy).
    pub const REFUSED_TOO_FAR_BACKWARD: u64 = 3;
    /// The request was not one this contract defines.
    pub const BAD_REQUEST: u64 = 4;
}

// ================================================================================================
// The policy: what the service does with a proposal, as a pure function.
// ================================================================================================

/// **The policy a proposal is judged by.**
///
/// It lives in the contract crate rather than inside the service for two reasons. It is the part
/// worth testing on the host in milliseconds, and it is the part a proposer needs in order to be a
/// well-behaved one: a network time client that can predict the answer can decline to ask rather
/// than hammering the endpoint with proposals that will be refused. Stating the bounds publicly
/// costs nothing, because the authority was never secrecy about the bounds; it is that the
/// proposer cannot write the page.
pub mod policy {
    use super::{state, status};

    /// **The build-era floor: 2026-01-01T00:00:00Z.** No machine running this code existed before
    /// it, so any claimed time below this is wrong no matter who said it.
    ///
    /// The milestone block calls this out as the escape from the NTS chicken-and-egg (TLS needs a
    /// roughly correct clock, and a correct clock needs TLS), and it is chosen here on purpose
    /// rather than discovered halfway through that work. It is a **floor on plausibility, not a
    /// claim of accuracy**: passing it means "not obviously a lie", never "trustworthy".
    pub const NOT_BEFORE_NANOS: u64 = 1_767_225_600 * super::NANOS_PER_SEC;

    /// The ceiling: 2100-01-01T00:00:00Z. Far enough out to be uncontroversial and near enough to
    /// catch the classic attacks, which push the clock past a certificate's expiry rather than
    /// nudging it.
    pub const NOT_AFTER_NANOS: u64 = 4_102_444_800 * super::NANOS_PER_SEC;

    /// How far forward one accepted proposal may move a clock that already knows the time: an hour.
    /// Enough to absorb a machine that has been asleep, or an RTC an hour out because somebody set
    /// it to local time; not enough to walk the clock past an expiry in one step.
    pub const MAX_STEP_FORWARD_NANOS: u64 = 3600 * super::NANOS_PER_SEC;

    /// How far **backward** one accepted proposal may move a clock that already knows the time: one
    /// second.
    ///
    /// The asymmetry is the whole point and it is not timidity. Moving forward skips over instants
    /// nobody has observed yet; moving backward makes instants happen twice, which is what breaks
    /// log ordering, cache expiries, build stamps and anything that recorded a timestamp and
    /// assumed it would not be reissued. Unix reaches for `adjtime` slewing largely because of
    /// this, and here the same conservatism is one constant instead of a mechanism, because
    /// `Instant` is never affected by any of it.
    pub const MAX_STEP_BACKWARD_NANOS: u64 = super::NANOS_PER_SEC;

    // The asymmetry is a decision, not an accident, so it is a build-time fact rather than
    // something a reader has to notice: anyone "tidying" the two constants into one fails to
    // compile rather than quietly making backwards steps as free as forwards ones.
    const _: () = assert!(
        MAX_STEP_BACKWARD_NANOS < MAX_STEP_FORWARD_NANOS,
        "moving the clock backwards must stay far tighter than moving it forwards",
    );

    /// Whether an absolute instant is one a machine running this code could be at. The sanity
    /// window, applied to an RTC reading as well as to a proposal: an RTC that fails this is an RTC
    /// the service refuses to believe, and the clock stays [`state::UNKNOWN`] rather than becoming
    /// confidently wrong.
    pub const fn plausible(unix_nanos: u64) -> bool {
        unix_nanos >= NOT_BEFORE_NANOS && unix_nanos < NOT_AFTER_NANOS
    }

    /// **The decision.** `current_state` and `current_nanos` are what the clock believes now;
    /// `proposed_nanos` is what the proposer asked for. The answer is one of [`status`]'s codes.
    ///
    /// The bootstrap case is the interesting one: when the clock is [`state::UNKNOWN`] there is
    /// nothing to step *from*, so a plausible proposal is accepted outright. That is not a hole,
    /// because a machine that does not know the time has no belief for a step limit to protect;
    /// the sanity window is the only guard that means anything, and it is applied.
    pub const fn decide(current_state: u64, current_nanos: u64, proposed_nanos: u64) -> u64 {
        if !plausible(proposed_nanos) {
            return status::REFUSED_IMPLAUSIBLE;
        }
        if !state::known(current_state) {
            return status::ACCEPTED;
        }
        if proposed_nanos > current_nanos {
            if proposed_nanos - current_nanos > MAX_STEP_FORWARD_NANOS {
                return status::REFUSED_TOO_FAR_FORWARD;
            }
        } else if current_nanos - proposed_nanos > MAX_STEP_BACKWARD_NANOS {
            return status::REFUSED_TOO_FAR_BACKWARD;
        }
        status::ACCEPTED
    }
}

// ================================================================================================
// The two RTC bindings, named so the driver picks a register layout from what the machine said.
// ================================================================================================

/// **Which RTC the machine has**, discovered from the device tree's `compatible` and passed to the
/// clock service at spawn.
///
/// The service could have keyed the register layout off `target_arch`, which is what the console
/// driver does for its UART. It does not, because the RTC is where that shortcut runs out: the
/// VisionFive 2 is riscv64 and has neither of these devices, so an ISA-keyed driver would compile
/// clean and read garbage on the first real board. The binding is what the driver actually knows
/// how to drive, so the binding is what it is told.
pub mod rtc {
    /// No RTC in the device tree. The clock service still runs and still serves proposals; it just
    /// starts out not knowing what time it is, which is a state the contract has (DECISIONS §42).
    pub const NONE: u64 = 0;
    /// `arm,pl031`, QEMU `virt` on aarch64 at `0x9010000`. One 32-bit register at offset 0, `DR`,
    /// reading **seconds** since the Unix epoch.
    pub const PL031: u64 = 1;
    /// `google,goldfish-rtc`, QEMU `virt` on riscv64 at `0x101000`. Two 32-bit registers,
    /// `TIME_LOW` at 0 and `TIME_HIGH` at 4, together **nanoseconds** since the Unix epoch. Read
    /// LOW first: it latches HIGH, and reading them the other way round gives a value that is
    /// correct except across the low word's wrap, which is a bug that shows up once every four
    /// seconds and looks like a 4-second jump.
    pub const GOLDFISH: u64 = 2;
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    /// A time inside the sanity window, used as "now" by the policy tests.
    const NOW: u64 = 1_800_000_000 * NANOS_PER_SEC; // 2027-01-15-ish

    #[test]
    fn the_sanity_window_rejects_the_two_lies_that_matter() {
        // 1970 is what a machine with no clock reports, and it is exactly what this milestone
        // exists to stop being mistaken for an answer.
        assert!(!policy::plausible(0));
        // The far future is where a clock attack aims: past every certificate's expiry.
        assert!(!policy::plausible(policy::NOT_AFTER_NANOS));
        assert!(policy::plausible(NOW));
        // 2038, where a 32-bit `time_t` wraps, is deliberately INSIDE the window. It is a real
        // instant this machine may run at, and refusing it would be treating a C bug as a fact
        // about time. Asserted so a future tightening of the ceiling has to mean it.
        assert!(policy::plausible(2_147_483_647 * NANOS_PER_SEC));
    }

    #[test]
    fn an_unknown_clock_accepts_any_plausible_proposal() {
        // The bootstrap: nothing to step from, so only the window applies.
        assert_eq!(
            policy::decide(state::UNKNOWN, 0, NOW),
            status::ACCEPTED,
            "a machine that does not know the time has no belief a step limit could protect"
        );
        assert_eq!(
            policy::decide(state::UNKNOWN, 0, 0),
            status::REFUSED_IMPLAUSIBLE,
            "but the window still applies, so 1970 is still refused"
        );
    }

    #[test]
    fn a_known_clock_is_stepped_only_within_the_bounds() {
        let known = state::RTC;
        for (delta, want) in [
            (0i64, status::ACCEPTED),
            (
                policy::MAX_STEP_FORWARD_NANOS as i64,
                status::ACCEPTED, // exactly the limit is inside it
            ),
            (
                policy::MAX_STEP_FORWARD_NANOS as i64 + 1,
                status::REFUSED_TOO_FAR_FORWARD,
            ),
            (-(policy::MAX_STEP_BACKWARD_NANOS as i64), status::ACCEPTED),
            (
                -(policy::MAX_STEP_BACKWARD_NANOS as i64) - 1,
                status::REFUSED_TOO_FAR_BACKWARD,
            ),
        ] {
            let proposed = (NOW as i64 + delta) as u64;
            assert_eq!(
                policy::decide(known, NOW, proposed),
                want,
                "a step of {delta} ns from a known clock"
            );
        }
    }

    /// The asymmetry is a decision, not an accident, so it gets a test that fails if someone
    /// "tidies" the two constants into one.
    #[test]
    fn backwards_is_held_far_tighter_than_forwards() {
        let known = state::SYNCED;
        let ten_minutes = 600 * NANOS_PER_SEC;
        assert_eq!(
            policy::decide(known, NOW, NOW + ten_minutes),
            status::ACCEPTED
        );
        assert_eq!(
            policy::decide(known, NOW, NOW - ten_minutes),
            status::REFUSED_TOO_FAR_BACKWARD,
            "the same magnitude backwards makes instants happen twice, and is refused"
        );
    }

    #[test]
    fn the_offset_round_trips_through_the_wall_clock() {
        let monotonic = 42 * NANOS_PER_SEC;
        let offset = offset_for(NOW, monotonic);
        assert_eq!(wall_nanos(offset, monotonic), NOW);
        // And the property the whole design is for: the offset changes, the monotonic input does
        // not, and the two are independent by construction.
        let stepped = offset_for(NOW + 5 * NANOS_PER_SEC, monotonic);
        assert_eq!(wall_nanos(stepped, monotonic) - NOW, 5 * NANOS_PER_SEC);
        assert_eq!(monotonic, 42 * NANOS_PER_SEC);
    }

    /// A zeroed frame is a frame nobody published to, and it must read as "unknown" rather than as
    /// "1970". This is the default-honest property, and it is the one a reader gets for free when a
    /// clock service failed to start at all.
    #[test]
    fn a_zeroed_page_reads_as_unknown() {
        let frame = [const { AtomicU64::new(0) }; WORDS];
        // SAFETY: `frame` is WORDS aligned u64s, alive for the body of this test.
        let page = unsafe { ClockPage::new(frame.as_ptr() as u64) };
        assert_eq!(
            page.read(),
            Reading {
                state: state::UNKNOWN,
                offset_nanos: 0,
                generation: 0,
            }
        );
        assert!(!state::known(page.read().state));
    }

    #[test]
    fn a_published_page_reads_back_and_counts_generations() {
        let frame = [const { AtomicU64::new(0) }; WORDS];
        // SAFETY: as above.
        let page = unsafe { ClockPage::new(frame.as_ptr() as u64) };
        page.init();
        assert_eq!(
            page.read().state,
            state::UNKNOWN,
            "init is honest, not 1970"
        );

        assert_eq!(page.publish(state::RTC, 7), 1);
        assert_eq!(
            page.read(),
            Reading {
                state: state::RTC,
                offset_nanos: 7,
                generation: 1,
            }
        );

        assert_eq!(page.publish(state::SYNCED, 9), 2);
        assert_eq!(
            page.read().generation,
            2,
            "a reader can see the clock moved"
        );
    }

    /// The seqlock's invariant, stated where a refactor will trip over it: a reader must never
    /// return data it took from under an odd sequence.
    #[test]
    fn a_reader_never_returns_data_from_a_half_written_page() {
        let frame = [const { AtomicU64::new(0) }; WORDS];
        // SAFETY: as above.
        let page = unsafe { ClockPage::new(frame.as_ptr() as u64) };
        page.init();
        page.publish(state::RTC, 7);
        // Stand where a writer stands mid-publish: sequence odd, data torn (a new state with the
        // old offset). A single-threaded test cannot let `read` spin, so this checks the guard
        // directly rather than calling it.
        frame[W_SEQ].store(3, Ordering::Relaxed);
        frame[W_STATE].store(state::SYNCED, Ordering::Relaxed);
        assert_eq!(
            frame[W_SEQ].load(Ordering::Relaxed) & 1,
            1,
            "odd means a writer holds it, and `read` spins rather than taking this"
        );
        // Finish the publish the way `publish` would, and the reading is whole again.
        frame[W_OFFSET].store(11, Ordering::Relaxed);
        frame[W_SEQ].store(4, Ordering::Release);
        assert_eq!(
            page.read(),
            Reading {
                state: state::SYNCED,
                offset_nanos: 11,
                generation: 2,
            }
        );
    }

    /// The request word is a wire format shared with the clock service at runtime; `req` and `op`
    /// must be inverses AND put the opcode in bits 63:56 exactly, because the service on the other
    /// side of the endpoint decodes with its own copy of the constant. Milestone 85's mutation run
    /// showed the shift direction and the whole function bodies were pinned by nothing.
    #[test]
    fn the_request_word_is_the_wire_format_it_claims() {
        assert_eq!(propose::req(propose::PROPOSE), 1u64 << 56);
        assert_eq!(propose::op(propose::req(propose::STATE)), propose::STATE);
        assert_eq!(propose::op(0), 0);
    }

    /// The plausibility window's endpoints are seconds times a billion, and the multiplication is
    /// exactly the kind of arithmetic a mutant can quietly break (a `/` there made the floor
    /// microscopic, and every implausible proposal became plausible). Pin the products.
    #[test]
    // Asserting on constants is this test's entire purpose: the constant is the thing a mutant
    // rewrites, and the assertion is what notices (milestone 85).
    #[allow(clippy::assertions_on_constants)]
    fn the_sanity_window_is_where_it_says() {
        assert_eq!(policy::NOT_BEFORE_NANOS, 1_767_225_600_000_000_000);
        assert_eq!(policy::NOT_AFTER_NANOS, 4_102_444_800_000_000_000);
        assert!(policy::NOT_BEFORE_NANOS < policy::NOT_AFTER_NANOS);
    }
}

// ================================================================================================
// The seqlock under loom: every interleaving, and every reordering C11 permits (milestone 80).
// ================================================================================================
#[cfg(all(test, loom))]
mod interleavings {
    //! **The clock page is the sharpest weak-memory target in the tree**, and that is why it is
    //! here rather than only the scheduler's steal slot. It is a hand-rolled seqlock whose two
    //! halves run in *different address spaces*, with no lock available between them, and its own
    //! documentation says the memory ordering is the point rather than decoration. On x86 a sloppy
    //! version passes forever; on the aarch64 and riscv64 this project targets it does not.
    //!
    //! Run with `script/interleaving-check`. See notes/interleaving.md.

    use loom::thread;

    use super::*;

    /// A clock page backed by loom atomics rather than by a mapped frame.
    ///
    /// The crate's own `ClockPage` is a raw pointer into a shared page, which is exactly right for
    /// something two address spaces map and exactly wrong for a model checker, whose atomics carry
    /// per-execution state and cannot be conjured from an address. Leaking a real array of loom
    /// atomics and pointing the ordinary `ClockPage` at it means **the algorithm under test is the
    /// algorithm that ships**: `read`, `publish` and `init` are not reimplemented here, only the
    /// addressing is. The leak is per model execution and is four words wide.
    fn page() -> ClockPage {
        let words: &'static [AtomicU64; WORDS] = Box::leak(Box::new([
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
        ]));
        // SAFETY: `ClockPage::new` wants a mapped, 8-byte-aligned frame of at least `WORDS` words
        // that stays mapped for the value's life. A leaked array of exactly `WORDS` atomics is all
        // three: it is aligned by the type, it is never freed, and `word()` indexes it with the
        // same pointer arithmetic it would use on a frame.
        unsafe { ClockPage::new(words.as_ptr() as u64) }
    }

    /// Non-vacuity, across executions rather than within one. See
    /// `steal_request`'s copy of this for why a model checker needs it.
    struct Reached(core::sync::atomic::AtomicBool);
    impl Reached {
        const fn new() -> Self {
            Self(core::sync::atomic::AtomicBool::new(false))
        }
        fn mark(&self) {
            self.0.store(true, core::sync::atomic::Ordering::Relaxed);
        }
        fn assert(&self, what: &str) {
            assert!(
                self.0.load(core::sync::atomic::Ordering::Relaxed),
                "vacuous harness: no execution ever reached {what}, so nothing was checked"
            );
        }
    }

    /// **The seqlock's whole reason to exist: a reader never sees half a publish.**
    ///
    /// The state and the offset are a matched pair (a state of `SYNCHRONISED` with the offset from
    /// before the sync is a wrong wall clock, not a crash, which is the worst kind of bug to leave
    /// possible). A reader that catches the writer mid-publish must retry, never blend.
    #[test]
    fn a_reader_never_sees_half_a_publish() {
        static SAW_THE_OLD: Reached = Reached::new();
        static SAW_THE_NEW: Reached = Reached::new();

        loom::model(|| {
            let page = page();
            page.init();
            page.publish(state::RTC, 1_000);

            let writer = {
                let p = page;
                thread::spawn(move || p.publish(state::SYNCED, 2_000))
            };

            let r = page.read();
            match (r.state, r.offset_nanos) {
                (state::RTC, 1_000) => SAW_THE_OLD.mark(),
                (state::SYNCED, 2_000) => SAW_THE_NEW.mark(),
                other => panic!("a torn reading: {other:?} is neither publish"),
            }

            writer.join().unwrap();
        });

        SAW_THE_OLD.assert("an execution where the reader gets in before the publish");
        SAW_THE_NEW.assert("an execution where the reader gets in after the publish");
    }

    /// **The generation is what a reader compares, so it must count publishes and not retries.**
    ///
    /// `Reading::generation` exists so a log or an expiring cache can ask "did the clock step under
    /// me" without comparing timestamps and guessing. That makes it a value the reader depends on,
    /// not a diagnostic, and it is derived from the same sequence word the tearing check uses.
    #[test]
    fn the_generation_a_reader_sees_matches_the_pair_it_read() {
        loom::model(|| {
            let page = page();
            page.init();

            let writer = {
                let p = page;
                thread::spawn(move || p.publish(state::RTC, 1_000))
            };

            let r = page.read();
            // Generation 0 is the initialised page nobody has published to; 1 is the one publish.
            let expected = if r.state == state::UNKNOWN { 0 } else { 1 };
            assert_eq!(
                r.generation, expected,
                "the generation disagrees with the reading it came with: {r:?}"
            );

            writer.join().unwrap();
        });
    }

    /// **Two writers serialise rather than corrupt.**
    ///
    /// The crate's own documentation says this and says why: the capability layout permits several
    /// processes to hold a read/write mapping, so claiming the sequence is a compare-exchange
    /// rather than a plain store, and a seqlock that assumed one writer would corrupt silently.
    /// "Would corrupt silently" is a claim about interleavings, which is the sentence no test in
    /// this tree could check before this one.
    #[test]
    fn two_writers_serialise_rather_than_corrupt_the_page() {
        static BOTH_LANDED: Reached = Reached::new();

        loom::model(|| {
            let page = page();
            page.init();

            let first = {
                let p = page;
                thread::spawn(move || p.publish(state::RTC, 1_000))
            };
            let second = {
                let p = page;
                thread::spawn(move || p.publish(state::SYNCED, 2_000))
            };

            let g1 = first.join().unwrap();
            let g2 = second.join().unwrap();
            assert_ne!(g1, g2, "two publishes were handed the same generation");

            let r = page.read();
            assert_eq!(
                r.generation, 2,
                "two publishes did not advance the page twice"
            );
            assert!(
                matches!(
                    (r.state, r.offset_nanos),
                    (state::RTC, 1_000) | (state::SYNCED, 2_000)
                ),
                "the page holds a pair neither writer published: {r:?}"
            );
            BOTH_LANDED.mark();
        });

        BOTH_LANDED.assert("any execution at all");
    }

    /// **A page is recognisable only once it is whole.**
    ///
    /// `init` writes the magic last, with a release, precisely so a reader racing the first publish
    /// sees either a page it does not recognise (and reports `UNKNOWN`, which is the truth about a
    /// frame nobody has published to) or a fully initialised one. Never a recognised page with
    /// garbage in it. The release is the whole mechanism, and until now nothing checked it.
    #[test]
    fn a_racing_reader_sees_an_unrecognised_page_or_a_whole_one() {
        static SAW_UNRECOGNISED: Reached = Reached::new();
        static SAW_INITIALISED: Reached = Reached::new();

        loom::model(|| {
            let page = page();

            let writer = {
                let p = page;
                thread::spawn(move || {
                    p.init();
                    p.publish(state::RTC, 1_000);
                })
            };

            let r = page.read();
            match (r.state, r.offset_nanos) {
                (state::UNKNOWN, 0) => SAW_UNRECOGNISED.mark(),
                (state::RTC, 1_000) => SAW_INITIALISED.mark(),
                other => panic!("a reader saw a recognised page with garbage in it: {other:?}"),
            }

            writer.join().unwrap();
        });

        SAW_UNRECOGNISED.assert("an execution where the reader arrives before the magic lands");
        SAW_INITIALISED.assert("an execution where the reader arrives after the publish");
    }
}
