//! Machine-checked proofs for the firing arithmetic (milestone 129).
//!
//! **Why this function and not the parser.** A parser is a total function on bytes and the host
//! tests sample it well; a wrong answer there is a document that does not load, which is loud. The
//! arithmetic is the opposite: every wrong answer is quiet. An off-by-one at a period boundary
//! fires an entry twice in one pass and nothing complains; a lost phase drifts an entry a few
//! nanoseconds per hour and shows up as a schedule nobody can explain a month later; a `+ period`
//! that overflows wraps a deadline into the past and turns a scheduler into a spin. None of those
//! is reachable by sampling, because the interesting inputs are the ones nobody thinks to type.
//!
//! Kani quantifies over them instead. The bounds below are stated where they are needed and are
//! generous rather than convenient: `1 << 50` nanoseconds is thirteen days, which is longer than
//! anything this system has stayed up.
//!
//! # The one property that is not here, and why
//!
//! **Phase preservation is host-tested and not machine-checked**, and it is the most interesting of
//! the four laws `next_after` obeys, so the absence is worth a paragraph rather than a shrug.
//!
//! The claim is that the answer is congruent to `prev` modulo `period`. Every way of writing that
//! needs a modulo of a *computed* value on top of the one already inside `next_after`, and a second
//! 64-bit modulo is where CBMC stops finishing: the direct spelling
//! (`next % period == prev % period`) did not return after ten minutes, the cheaper one
//! (`(next - prev) % period == 0`) did not either, and neither did the cheaper one bounded to
//! `1 << 32`. Shipping a harness bounded far enough down to finish would have been worse than
//! shipping none: it would read as proved and cover a range no schedule lives in.
//!
//! What covers it instead: `next_after_is_strictly_in_the_future_and_keeps_its_phase` in the crate's
//! host tests asserts the congruence at values up to `u64::MAX / 4`, across four periods including
//! `1` and a large prime. That is sampling and it is honest about being sampling.
//!
//! **It is also the least likely of the four to be got wrong silently**, which is the reason this is
//! a recorded gap rather than a blocker: the implementation reaches its answer by snapping `now`
//! back onto the beat (`now - (now - prev) % period`), so the phase is not computed and then
//! preserved, it is the only thing the expression can produce. A change that broke it would have to
//! rewrite the line, not slip past a boundary in it.

use crate::next_after;

/// Nanoseconds this system could plausibly be dealing with: `1 << 50`, about thirteen days, which is
/// longer than this system has ever stayed up.
///
/// The bound exists to keep `saturating_add` out of the proofs rather than to keep them tractable:
/// at `u64::MAX` the saturation is the *correct* answer, and asserting "strictly greater than now"
/// against it would be asserting a falsehood. What the bound says is "on any machine that has not
/// been up for thirty-eight thousand years", which is the regime these properties are about.
const HORIZON: u64 = 1 << 50;

/// **A fire is always strictly in the future**, so a polling loop cannot fire the same occurrence
/// twice however often it looks.
///
/// This is the property the `+ 1` in `next_after` exists for, and the one a `>=` comparison would
/// have broken exactly on period boundaries: the case a hand-written test is least likely to pick
/// and a polling scheduler hits constantly.
#[kani::proof]
fn a_fire_is_strictly_in_the_future() {
    let prev: u64 = kani::any();
    let period: u64 = kani::any();
    let now: u64 = kani::any();
    kani::assume(period > 0 && period <= HORIZON);
    kani::assume(prev <= HORIZON);
    kani::assume(now <= HORIZON);

    let next = next_after(prev, period, now);
    assert!(next > now);
}

/// **A fire is always at least one whole period on from the last one**, which is what stops a
/// schedule from tightening under load: an entry that fired late must not have its next occurrence
/// pulled forward to compensate.
///
/// Its own harness rather than a second assertion on the one above, because the two claims have
/// very different costs and pairing them would make the cheap one wait for the expensive one every
/// time the suite runs.
#[kani::proof]
fn a_fire_is_at_least_one_whole_period_on() {
    let prev: u64 = kani::any();
    let period: u64 = kani::any();
    let now: u64 = kani::any();
    kani::assume(period > 0 && period <= HORIZON);
    kani::assume(prev <= HORIZON);
    kani::assume(now <= HORIZON);

    assert!(next_after(prev, period, now) >= prev + period);
}

/// **A stall is skipped, not caught up.** One call advances past `now` by at most one period, so an
/// entry that missed two hundred occurrences fires once rather than two hundred times.
///
/// The bound is the whole claim: `next <= now + period` says that no matter how far behind the
/// scheduler fell, exactly one occurrence survives the gap. Without it the honest implementation
/// and a catch-up implementation would satisfy every other property here.
#[kani::proof]
fn a_stall_costs_one_fire_and_not_a_backlog() {
    let prev: u64 = kani::any();
    let period: u64 = kani::any();
    let now: u64 = kani::any();
    kani::assume(period > 0 && period <= HORIZON);
    kani::assume(prev <= HORIZON);
    kani::assume(now <= HORIZON);
    // The only regime the bound is claimed in: time has not gone backwards since the last fire was
    // scheduled. `due` never calls it otherwise, because it only advances a row it found overdue.
    kani::assume(now >= prev);

    let next = next_after(prev, period, now);
    assert!(next <= now + period);
}

/// **It is minimal**, which is what makes "the next fire" a true description rather than "a fire
/// somewhere later". Nothing congruent to `prev` sits between `now` and the answer.
///
/// Stated as: one period earlier is not in the future. Together with the phase property above, that
/// pins the answer exactly, and it is the property that would catch a `+ 2` where the `+ 1` is.
#[kani::proof]
fn nothing_on_the_beat_is_skipped_between_now_and_the_answer() {
    let prev: u64 = kani::any();
    let period: u64 = kani::any();
    let now: u64 = kani::any();
    kani::assume(period > 0 && period <= HORIZON);
    kani::assume(prev <= HORIZON);
    kani::assume(now <= HORIZON);
    kani::assume(now >= prev);

    let next = next_after(prev, period, now);
    // `next - period` is the previous occurrence on the same beat. It must not be a fire we could
    // have returned instead, which means it is at or before `now`, or it is before the first
    // occurrence after `prev` at all.
    assert!(next - period <= now);
}

/// **A zero period never fires**, rather than dividing by zero.
///
/// `parse` refuses `every 0s`, so no document reaches this; the proof is here because the function
/// is `pub` and a caller who did not read the doc comment is exactly the caller a total function
/// exists for.
#[kani::proof]
fn a_zero_period_is_never_due() {
    let prev: u64 = kani::any();
    let now: u64 = kani::any();
    assert_eq!(next_after(prev, 0, now), u64::MAX);
}
