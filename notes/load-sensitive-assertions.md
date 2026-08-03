# Load-sensitive assertions, and the three that measured the wrong thing

*(Milestone 78. `kernel/src/sched.rs`, `kernel/src/user/tests.rs`, the timer drift twins in
`kernel/src/arch/riscv64/timer.rs` and `kernel/src/arch/aarch64/timer.rs`, `kernel/src/smp.rs`,
and the frame-hygiene assertion already removed from `kernel/src/user/live_swap_tests.rs`.)*

On 2026-08-03 five distinct assertions had failed pull requests that changed no executable code.
The roadmap block (design/roadmap.md, milestone 78) holds the evidence table; this note records
what was done about each and why, because the verdicts are per assertion and the arguments are the
deliverable.

## The diagnostic that sorts the family

**A slow machine produces a deficit, never a surplus.** Host contention deschedules the guest's
vCPUs, so work the test is waiting on happens late or not yet. That can only make a count *lower*
than a wait expected: fewer ticks delivered, a thread not yet run, frames not yet returned. So the
direction of a failure is the diagnosis:

- A failure in the **positive** direction ("not yet") is honest load sensitivity. The fix, if any,
  is to wait on the property itself with the watchdog as backstop, which is `smp.rs`'s documented
  `wait_for` argument.
- A failure in the **negative** direction (fewer threads than the baseline, more free frames than
  at the start) is not a timeout at all. The assertion was written against something wider than the
  property under test, and state arriving from *outside* the measured window tripped it: a
  neighbouring test's teardown landing late. `notes/riscv-parity-scope.md` named this shape ("a
  wait written against something wider than the property") and the BUGS section of
  `notes/live-replacement.md` is the completed analysis of one instance.

The second kind cannot be fixed by margins. Widening a bound that fires on a negative discrepancy
only hides the defect, which is DECISIONS §61's reasoning about the dropped lints, applied to
assertions.

## The verdicts

### Reaper count (`sched.rs`, `a_finished_thread_is_reaped_and_its_memory_returned`): rescoped

Failed as "finished threads were never reaped, left: 5, right: 6". The count was *below* its
baseline: eight reaped threads cannot produce that, but one baseline-counted thread exiting
mid-test does. The test sampled `thread_count()` at the top and asserted the table returned to it,
so its baseline was a number the rest of the system moves on its own.

Now each batch keeps the eight `Tid`s it spawned and waits for `thread_present` to go false on
each. That is the property the test is responsible for ("the threads this batch created were
reaped"), and it is immune to neighbours by construction: a generational `Tid` resolving to
nothing means *this* thread is gone, whatever else the table is doing. Third appearance of this
exact fix; `reclaim_frees_a_started_then_exited_childs_regions` got it first (see
riscv-parity-scope.md) and `thread_present`'s doc comment already argued it.

The frame half of the test also changed direction: the second batch's cost is asserted as
`used() <= before` (waited on, clock-bounded) rather than `==`. A leak, the milestone-6 bug the
test guards, leaves `used` *above* `before` forever, so the wait times out and fails exactly as
before. Equality additionally demanded that no other test free a frame during the window, which is
the neighbour exposure again, in the frame allocator instead of the thread table.

### Address-space frames (`user/tests.rs`, `a_dead_user_thread_frees_its_whole_address_space`): rescoped

The recorded failure is "-19 frames did not come back", on CI (milestone 71's lane, and again on
PR #50's build+test job on 2026-08-03) and once on a quiet aarch64 dev machine. Negative: `used()`
settled 19 frames *below* the baseline, so the wait for equality could never succeed. The frames
arrived from outside the measured window; the test's own settle loop (two agreeing samples before
taking the baseline) already rules out its own in-flight frees, which is how we know the source is
a neighbour.

Same two changes as the reaper test: the reap waits are per-`Tid` (`thread_present` on the outlaw
just spawned, replacing `thread_count() <= baseline`), and the final assertion waits for
`used() <= before`. Leak sensitivity is unchanged: every frame an outlaw's address space keeps
holds `used()` above `before` forever.

### Frame hygiene (`user/live_swap_tests.rs`): already removed, nothing to do

Removed 2026-08-03 in PR #46 with the completed analysis in `notes/live-replacement.md`'s BUGS
section; the tree confirms it gone, replaced by a comment saying why. The removal is not a
counterexample to "deletion is not the fix": the property the test is responsible for (the budget
reclaim returns exactly `SWAPPER_BUDGET_PAGES`) was already asserted twelve lines above, so the
global count added only the neighbour exposure. Milestone 78's postscript records the same.

### Timer drift (`ticks_arrive_at_the_configured_rate`, both ISAs): re-aimed at the re-arm law

The old assertion compared delivered ticks to elapsed counter time, one period of slack each way.
`script/test` passes no `-icount`, so the guest counter follows host time: a host that deschedules
the vCPU for a few periods coalesces ticks into exactly the deficit the re-arm-from-`now` defect
produced, and it failed under load on `rv64`, the control model. No margin separates "our re-arm
is late" from "the emulator was not running"; widening changes how often you notice, not what is
measured.

The milestone named only the riscv64 site, but the aarch64 twin supplied its own evidence during
this lane's first gate run (2026-08-03): "timer drift: 22 ticks in 25 periods" at
`arch/aarch64/timer.rs`, on an otherwise green suite, while the host compiled the std farm beside
QEMU. A deficit, the coalescing signature, on the ISA the milestone had no drift evidence for.
Both twins got the same fix, which parity requires anyway: a test that measures the wrong thing on
one ISA measures it on both.

The property the test was written for (re-arming relative to `now` compounds lateness: TVAL on
aarch64, `now() + interval` on riscv64) is the **grid law**. The grid is state the kernel owns on
both ISAs: `CNTV_CVAL_EL0` on aarch64, readable back out of the hardware, and the software
`DEADLINE` array on riscv64, kept because SBI's `set_timer` is write-only. So both tests now
assert the law directly: over a window in which `MISSED_TICKS` did not move, the deadline advanced
by exactly one interval per delivered tick. The defect fails this on the first tick, because each
re-arm overshoots the grid by the handler latency. A descheduled emulator cannot fail it: a
deschedule long enough to slip the grid increments `MISSED_TICKS` (the re-anchor safety valve, and
correct behaviour), and the window is retried. A small `deadline()` accessor was added beside
`missed_ticks()` on each ISA. One wall-clock bound survives because contention cannot falsify it:
descheduling only drops ticks, so *more* ticks than elapsed periods still fails, which is
`rearm`'s spin-forever failure mode.

What moved out of scope rather than being weakened: the end-to-end claim that SBI actually fired
at the software grid's deadlines. Under `-icount shift=0,sleep=off` virtual time is a
deterministic function of instructions executed, so the icount instrument (`script/bench`) is
where that claim is checkable without the host as a confound. Recommended, not built here; see the
milestone report. Until then the residual gap is an implementation that maintains `DEADLINE`
correctly but arms SBI with something else, which no wall-clock margin could distinguish from load
either.

### Placement probe (`smp.rs`, `work_can_be_placed_on_every_core`): left alone

Checked against the same question and it already passes. The wait (`wait_for(done)`, 60 s,
subordinate to the 90 s watchdog) is on exactly the property under test, not a proxy: `done` is
"each core's probe has marked its own core". Its failure direction is purely positive ("has not
happened yet"), so load produces late passes and honest timeouts, never a wrong measurement, and
the file's own comment block carries the argument for the budget. Moving it to the icount
instrument is not an option even in principle: the subject is genuinely cross-core wall clock, and
the icount bench boots `-smp 1` *because* icount's shared virtual clock makes multi-hart timing
fictional (notes/benchmarks.md). A cross-core delivery test cannot run on a one-core instrument.

### Round-robin fairness (`sched.rs`, `threads_round_robin`): rescoped

Failed once ("thread {i} never ran") and passed on re-run. The window was 300 yields, and a yield
count is not a duration: §28 scatters the three threads across cores, and on a contended host the
test core burns 300 cheap yields before a starved vCPU has run its thread at all. That is the
exact defect `smp.rs` documented and fixed in its own waits on 2026-07-30.

The test now waits on the property (every counter above zero), clock-bounded by the module's
`wait_for`, and asserts the wait succeeded. It also waits for its three threads to be reaped
before returning, so its own teardown is not the late-landing state a later test's accounting
finds in flight; this test was a candidate supplier for the reaper test's baseline drift, three
tests upstream of it in the same file.

## BUGS

- **The `<=` frame assertions can be masked by a coincidence.** A real leak of `k` frames passes
  if a neighbour's late teardown frees at least `k` frames inside the same window. The window is
  seconds wide and re-rolled every run, and a persistent leak (every batch leaks, which is what
  the milestone-6 bug was) fails essentially every run regardless, so the trap still bites; but a
  one-shot coincidence pass is possible in a way `assert_eq` did not permit. The trade is
  deliberate: equality bought that exactness by also asserting the rest of the machine held
  still, which is false on any loaded run and was producing red CI on documentation PRs.
- **The drift test can still go red on a pathological host**, by design: eight consecutive
  quarter-second windows each containing a missed tick fails with a message naming the condition.
  That is rarer by orders than the old failure (one miss anywhere in a single fixed window), and
  the message now says "host contention or a genuinely slow handler" instead of reporting drift
  the re-arm logic does not have.
- **The handler-latency assertions (`the_handler_keeps_up_when_no_lock_is_held`, both ISAs,
  missed-tick delta over a five-tick window) keep their wall-clock exposure and are not fixed
  here.** The aarch64 one is in the milestone's evidence table ("left: 3, right: 2" in a quiet
  window) but outside this lane's five. A deschedule long enough to pass a deadline is counted as
  a miss, and the guest cannot tell that miss from a slow handler; `miss_detail` (added to the
  aarch64 timer for milestone 78) records how *late* the re-arm was, which is the discriminator a
  fix would build on, since a few hundred cycles late is a slow handler and a whole period late
  is the emulator.
- **Scope was five sites, not 39.** The roadmap's scope note counts 39 sites in 7 files matching
  the shape (`wait_for`, or assertions against `free_frames`, `thread_count`, `used()`). The
  other 34 were not audited here; the diagnostic above is the checklist for reading any of them.

## See also

- design/roadmap.md milestone 78 (the spec and the day's evidence)
- notes/cpu-models.md BUGS (the load-sensitivity evidence, including the control model failing)
- notes/live-replacement.md BUGS (the completed frame-hygiene analysis, the template)
- notes/riscv-parity-scope.md (the shape named: a wait written against something wider than the
  property)
- notes/benchmarks.md (the two instruments, and why icount cannot host cross-core tests)
