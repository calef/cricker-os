# 62. Tests that assert on time: make a red run mean something

**Status: PARTIAL.** Raised 2026-08-01, from evidence rather than from taste. The token read
`NOT-STARTED` until 2026-08-17, by which point most of what this block asks for had been built **by
other lanes**, chiefly milestone 78's four rounds and milestone 50's shell work, and nobody came back
to this file. That is why it is PARTIAL rather than NOT-STARTED and PARTIAL rather than BUILT; the
precedent is milestone 40, whose status said `NOT-STARTED` with two phases shipped for the same reason.

**Gate: NONE.** Both of the things this line used to name are done (2026-08-17): the acceptance run
ran, and the icount instrument landed with milestone 78, which shared this half rather than gating on
it. What is left is what the acceptance run found, which is a disposition rather than a build. See
"What remains" below.

**What is built.** The prescribed fix exists by name: `sched::wait_for`
(`kernel/src/sched.rs:3233`), "bounded by the CLOCK rather than by a yield count", and
`sched::within_ticks` (`kernel/src/sched.rs:3260`), budgeted in guest timer ticks, which is this
block's own guest-ticks prescription. The example this block names is converted:
`threads_round_robin` (`kernel/src/sched.rs:3809`) calls `wait_for(all_ran)` instead of giving twenty
yields. `ticks_arrive_at_the_configured_rate` was rebuilt on both ISAs against the re-arm law rather
than against elapsed counter time (`kernel/src/arch/aarch64/timer.rs:382`,
`kernel/src/arch/riscv64/timer.rs:423`). And the watchdog progress heartbeat this block asks for is
`testing::note_progress` (`kernel/src/testing.rs:288`), bumped on every IPC rendezvous, wake and
console line, beside a per-test wall-clock ceiling.

**What remains, and it is why this is not BUILT.** Both items in the gate line above were answered
on 2026-08-17 and the block still is not done, which is worth saying plainly rather than rounding up.
The icount instrument landed with milestone 78 (the load-sensitive assertions), as `script/icount`;
its note is notes/instruction-clock.md. The
acceptance run happened, and **it did not pass**, which is the section below.

So the residual is no longer a missing instrument; it is a **disposition**. `script/icount` asserts
zero missed ticks on both ISAs, which a contended host cannot falsify and which is strictly stronger
than either of the two wall-clock assertions that failed the acceptance run. Those two still sit in
`script/test`, still fail at roughly one run in six at these loads, and no longer carry a claim the
instrument does not make better. Deciding what they are for now is the work, and it is a per-assertion
argument of the kind this block and milestone 78 have made five times, **not** a wider bound: the
acceptance run is evidence against widening, since the retry budget's implicit second claim is already
asserted properly by the sibling test that passed a line before it panicked.

The heartbeat that landed credits work by *any* thread rather than per test, and
`kernel/src/testing.rs:48` records that this blinded it once for real; that limitation is stated where
a reader meets the feature, which is what this project asks of a known cost.

## The acceptance run, 2026-08-17: 45 runs under load, 36 green

The evidence this block asks for, taken by `script/repeat-under-load` (one busy loop per host core,
the full suite, the load average sampled every ten seconds), on an eight-core Mac at a one-minute
load average between **26.1 and 63.0**, over 108 minutes. The full per-run table, the diagnosis of
every red, and the honest limits are in notes/load-sensitive-assertions.md. **Nine of forty-five runs
went red**, and the shape of those nine is the result rather than the count:

- **Eight were two assertions, twice each, on both ISAs.** `ticks_arrive_at_the_configured_rate`'s
  eight-attempt retry budget, and `the_handler_keeps_up_when_no_lock_is_held`'s taxonomy cut. Both
  were already named in that note's BUGS section as residuals that only the icount instrument can
  close, and **neither had ever been observed** before this run; one of the two entries called itself
  "rarer by orders" and has been corrected in place. That instrument now exists, which is what turns
  these eight reds from a wait into a decision. The perfect ISA symmetry is what says these are
  properties of the assertions rather than of either architecture.
- **The ninth was a real kernel bug**: a double free of a frame during `DESTROY`'s reclaim of a
  region whose resident was blocked in `recv` (riscv64, one occurrence). Recorded in the BUGS section
  of notes/object-revocation.md. It wants its own lane.

**Nothing was widened to make this green**, and the run is the argument for not widening: the retry
budget's implicit second claim is already asserted properly by the sibling test, which passed in the
same log a line before the panic.

**The most useful thing the run measured was not the load average.** Runs sharing the host with
another lane's emulator went red 6 times in 17; runs with only their own went red 3 times in 28. Run
42 passed at the highest peak load in the table (63.0) and run 11 failed near the lowest (33.8). A
competing emulator predicts these failures where a load average does not.

**The count in this block is stale**, and left below as written rather than edited into the prose,
because the argument it supports is history: `sched.rs` holds **6** of these spins now, not nineteen.
Tree-wide the shape matches 19 sites, but 9 are not test code at all, and none of the 10 in tests has
the flake-prone shape. They are all "let it settle, then prove nothing more happened", which is a
negative assertion a loaded host cannot fail in the failing direction.

## The problem

A population of tests assert on **elapsed time or on a fixed number of yields**. `sched.rs` alone
holds about nineteen `for _ in 0..N { yield_now() }` spins, and the shape is always the same: give
the scheduler N chances, then assert something happened. `threads_round_robin` gives twenty yields
and asserts every thread ran at least once. `ticks_arrive_at_the_configured_rate` and the riscv
timer-drift assertion compare guest ticks against elapsed counter time.

None of them is wrong about what it wants to prove. All of them fail when the host is busy, because
a yield is not a guarantee and the guest's clock is the host's clock.

## Why this is worth a milestone rather than tolerance

**It makes a real regression invisible.** On 2026-08-01 it cost the integrator three separate
diagnosis cycles, and two of those ended in the wrong conclusion before being re-run. The credentials
lane hit three flakes, the xattr lane two, the CPU-matrix lane two, and the integrator hit three more
in different tests each time. A suite that fails for reasons unrelated to the change trains everyone
to re-run rather than to read, which is the exact habit that lets a genuine failure through.

**Milestone 59 multiplies it fivefold.** The CPU matrix runs the same suite five times, so every
timing test now has five chances per run to be unlucky, on a shared CI runner nobody controls.

**And the honest diagnostic we rely on is expensive.** The current rule is "a green run under load is
conclusive, a red one is not, so re-run quiet." That works, and it costs a full suite run every time,
and it depends on a human remembering to apply it.

## What the fix probably is, per class

- **The bounded spins** are the easy majority. Waiting for a condition with a deadline is a different
  thing from taking N turns: the test wants "eventually", not "within twenty yields". An
  event-driven wait, or a bound expressed in **guest ticks** rather than host-scheduler turns, makes
  them insensitive to what else the machine is doing.
- **The genuinely temporal tests** cannot be made deterministic and should not pretend to be.
  `ticks_arrive_at_the_configured_rate` is *about* the clock. These want an explicit, stated
  tolerance and a recorded retry budget, so a flake is a documented cost rather than a surprise.
- **A third class may want to move off the emulator entirely.** Scheduling policy is pure logic and
  some of it could be host-tested against a simulated clock, which is where this project already puts
  logic it wants to check in milliseconds.

## The watchdog cannot tell "stuck" from "slow", and it is the same defect one level up

Added 2026-08-04 from `notes/semihosting.md:82`, which records the limitation and names the fix
nobody owns. The suite's own deadlock detector has exactly the problem this milestone is about.

**The heartbeat is bumped once per test, at the test's start**, in `Testable::run`, and never while
a test runs. So "no progress for 60 s" cannot distinguish a genuine deadlock from a test that is
simply *slower* than 60 s. Both look identical from outside: the heartbeat stops advancing because
no *new* test started.

**This has already cost a diagnosis.** Milestone 32's FS-server test tripped the watchdog as a false
deadlock. It was not stuck, it was starved: leaked spinning driver threads crammed onto core 0 slowed
the RedoxFS mount past 60 s. Raising the limit made it pass, **which is exactly what a deadlock can
never do**, and that is the tell the current instrument cannot produce on its own. The thread dump
reinforced the wrong read, because a starved thread and a deadlocked one both sit `Blocked`/`Ready`
with nothing obviously moving.

**The fix, per the note.** A per-test *progress* heartbeat: the test, or the IPC and scheduler paths
under it, bumps a counter as work happens, so a slow test keeps the watchdog fed and a wedged one
does not. The other half already exists and was built while chasing that false deadlock: the
enriched `sched::dump_threads` reports each thread's EL0 PC and the per-endpoint sender, receiver
and pending counts, so two dumps a few seconds apart show whether the pipeline is changing state
(starved but progressing) or frozen.

Until the heartbeat is per-progress, the operating rule is what the note says to do by hand: read a
watchdog trip as "stuck **or** slow", and confirm which by raising the limit before assuming a lost
wakeup. That is a human step in the loop for exactly the reason the rest of this milestone gives, a
red run whose meaning has to be argued about trains everyone to re-run rather than to read.

**It belongs here rather than in a harness milestone** because it is the same failure the bounded
spins have: a bound expressed in something other than the property under test. Nineteen tests take
N turns and hope; the watchdog takes 60 seconds and hopes. Whatever answers the first should answer
the second.

## BUGS

- **Fixing this cannot be verified by running the suite once.** A flake that fires one run in six is
  indistinguishable from a fixed one until you have run it many times, so the acceptance evidence is
  a repeat count, not a green run. *(Answered 2026-08-17 by the section above, and the answer was
  "not yet". `script/repeat-under-load` is the instrument, so the next person does not have to build
  one to re-ask the question.)*
- **A green acceptance run would prove less than this block wants, and this one was not green
  either.** Thirty-six passes are thirty-six draws from one host, one QEMU build and one load shape;
  they say nothing about a GitHub runner, and nothing about the assertions that simply did not get
  unlucky. Milestone 124's lane took 45 loaded full-suite runs without reproducing the fault it was
  hunting, which is the standard of evidence here and also the warning attached to it.
- **Deleting the timing assertions would be worse than the flakes.** `ticks_arrive_at_the_configured
  rate` is the test that catches re-arming the timer from `now()` inside the handler, which is a real
  bug this project has a comment about. The goal is tests that fail only when something is wrong, not
  fewer tests.

**Effort: not estimated**, and deliberately: the count is known (~19 spins plus a handful of clock
assertions) but how many are mechanical and how many need a rethink is not.
