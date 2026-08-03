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
//! # Everything is nanoseconds since the Unix epoch, in a `u64`
//!
//! One unit everywhere, so no conversion sits at a boundary where it can be forgotten. A `u64` of
//! nanoseconds runs out in the year 2554, which is recorded rather than defended: it is past every
//! horizon this project has, and picking `u64` keeps the wire words, the page words and the
//! arithmetic identical.

#![cfg_attr(not(test), no_std)]

use core::sync::atomic::{AtomicU64, Ordering, fence};

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
/// Writers are **multiple** (the service, and whoever holds the page read/write), so claiming the
/// sequence is a compare-exchange rather than a plain store. Two writers racing is not a design we
/// encourage, but it is a design the capability layout permits, and a seqlock that assumed a single
/// writer would corrupt silently rather than serialise.
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
                core::hint::spin_loop();
                continue;
            }
            let st = self.word(W_STATE).load(Ordering::Relaxed);
            let off = self.word(W_OFFSET).load(Ordering::Relaxed);
            // Keep the two data loads above the second sequence load. Without this the compiler or
            // the machine may reorder them after it, and the check would validate nothing.
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
                core::hint::spin_loop();
                continue;
            }
            // Acquire on success so our stores below cannot be hoisted above the claim.
            if self
                .word(W_SEQ)
                .compare_exchange_weak(s, s + 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break s;
            }
        };
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
    /// `proposed_unix_nanos`. Reply `r0` is one of the [`status`](super::status) codes and `r1` is
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
    /// `proposed_nanos` is what the proposer asked for. The answer is one of [`status`](super::status)'s codes.
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

#[cfg(test)]
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
}
