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

### Placement probe (`smp.rs`, `work_can_be_placed_on_every_core`): left alone, and that was wrong

***Superseded on 2026-08-04. The argument below is kept because it is the argument that failed, and
because the way it failed is the useful part. The verdict that replaced it is "the second round"
below.***


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

## Postscript: a fast machine finds the same family (milestone 81, 2026-08-04)

The day after this landed, the aarch64 suite was run on the physical Apple Silicon core under
Hypervisor.framework for the first time (notes/hvf-leg.md), and **every** failure it produced was
this family, found from the **opposite direction**: five of them, one per run, in `sched.rs`
(three), `user/reap_tests.rs` and `user/supervision_tests.rs`. Four were yield counts standing in
for a wait (one `yield_now()`, then 100, then 4000, then 2000). The fifth,
`a_thread_that_never_yields_is_preempted_anyway`, was a vacuity guard racing a scheduling order.

The diagnostic above needs one word added to stay right. "A slow machine produces a deficit" is
true, but the deficit's cause is that **a yield count is not a duration**, and that is symmetric: a
loaded host burns cheap yields while another vCPU is descheduled, and a native host burns them in
nanoseconds while another core has not been dispatched at all. Both arrive as "the thing I was
waiting for has not happened yet", both are positive-direction failures, and both take the same
fix, which is to wait on the property with the clock as the bound. So the checklist for reading any
of the remaining 34 sites does not change; only the reason to expect a hit does.

The timer verdicts came through untouched, which is the stronger result: none of the re-aimed
assertions failed on a machine where guest time *is* host time and there is no icount instrument.
Aiming a test at the re-arm law instead of at elapsed wall clock made it accelerator-independent,
which is a property this note could not have claimed the day it was written.
## The second round, 2026-08-04: three more, and a diagnostic the first round did not have

The `cpu matrix` job became a merge blocker. Three sites failed across four models in a handful of
runs, on pull requests whose diffs could not reach them (an `xargs` change failed a timer
assertion), and one of the three was the probe the first round had deliberately left alone.

| site | model | what it said |
|---|---|---|
| `arch/riscv64/timer.rs`, `holding_a_lock_masks_the_timer` | `rv64`, the control | `left: 41, right: 40` |
| `smp.rs`, `work_can_be_placed_on_every_core` | `rva23s64`, `thead-c906` | "work placed on a core never ran there" |
| `sched.rs`, `a_thread_that_never_yields_is_preempted_anyway` | `sifive-u54` | "the spinner never ran at all" |

**The diagnostic that sorts this round is the window, not the direction.** The first round sorted
the family by the sign of the discrepancy, which found three assertions written against something
wider than the property. These three are all "positive" failures and the sign says nothing useful
about them. What they have in common is that each **measures across instructions that are not part
of the thing being measured**: a tick counted between the read and the mask, a probe's execution
that placement never promised, a spinner sampled before it was ever given a turn. Host contention
does not cause any of them. It **stretches the window in wall-clock terms**, which is what turns a
race that never lost on a quiet machine into one that loses on a shared runner.

### `holding_a_lock_masks_the_timer` (both ISAs): the window moved inside the lock

The claim is "no timer interrupt lands while an `IrqSafeMutex` is held". The measurement was
`before = ticks()` **outside** the critical section, then `ticks()` inside it, so the window
included the handful of instructions between the read and `M.lock()`. A tick landing there is
charged to the lock and the run goes red with a message accusing `IrqSafeMutex` of not masking.

That window is where a descheduled vCPU resumes, and a resuming vCPU has a deadline already in the
past, so it takes the interrupt at the first instruction it executes. Hence "left: 41, right: 40",
one surplus tick, on the control model. The same window also straddled a preemption point, and
`TICKS` is per core (DECISIONS §11): a steal (§28.3) moving the thread between the two reads
compares two unrelated counters, which on a machine whose cores started within a tick of each other
also looks like an off-by-one.

Both reads now happen **inside** the critical section, where interrupts are masked and the thread
can neither switch nor migrate, so `cpu::id()` is fixed across the block and the window is exactly
the property. Nothing about the assertion was weakened: a real masking failure lands a tick inside
that window and still fails, and it now fails without a competing explanation.

The post-release half ("and the moment we let go, the pending interrupt is delivered") kept its
claim and lost its fixed two-period spin: it waits, bounded in tick periods, and reads the counter
of the core it *was* on, by index. Dropping the guard is a preemption point.

**A new accessor per ISA, and it is the general form of half this note**: `ticks_on(core)` and
`missed_ticks_on(core)` beside `ticks()` and `missed_ticks()`. A per-core counter read either side
of a wait must **name its core**, or a migration silently changes the subject. `ticks_arrive_at_the_configured_rate`
had already discovered this in the first round and solved it locally by bracketing the hart id into
its snapshot; the accessor makes it available to the other four tests in those files, all of which
had the same hole (`the_timer_is_ticking`, `the_handler_keeps_up_when_no_lock_is_held`,
`a_long_critical_section_costs_a_tick`, and the masking test itself).

### `work_can_be_placed_on_every_core` (`smp.rs`): the first round's verdict was wrong

The first round left it alone on the argument that its wait is on the property itself and can only
fail in the "not yet" direction. **The premise is false, and the machine said so.** The failure is
not slow, it is wedged, and no budget fixes it:

1. the probe for core A is placed on A's run queue while the test thread runs on A;
2. an idle core B, which has no probe of its own yet, steals it (a *queued* thread is fair game);
3. the rest of the probes are placed, every core is now busy, and nothing is idle;
4. A holds only the test thread, which never yields into idleness (`schedule()`: "a thread yielding
   into an empty run queue simply carries on"), so A never asks for work back.

Stealing is pull-based from an idle core (§28.3). Once no core is idle, nothing rebalances, so
`SPREAD[A]` stays zero for the full 60 s and then reports a timeout. The one-persistent-probe-per-core
trick was supposed to prevent exactly this by keeping every core busy with its own, and it has a
hole: it only holds if every probe is placed before any core starts stealing. A contended host
stretches the placement loop across a tick, which is all it takes.

**This is the third time §28 has invalidated a placement assumption in this one file.** The comment
in `secondary_main` step 6 is the second, and it is the one that stated the rule this verdict rests
on: *a deadline cannot fix an unreachable condition*. That comment also records that widening the
wait from 10 s to 60 s changed nothing, which is what finally separated the two cases there. The
first round read the same file and reached the opposite conclusion about the test next door.

So the test now asserts what `spawn_on` actually promises: **arrival at the named core**. A new
per-core counter, `PerCpu::adopted`, counts threads this core has taken out of its own inbox, which
is the one point where a thread crosses from a remote core's hands into this core's queue.
`inbox_len` cannot serve, because it is a depth: a push and a drain between two reads leave it
where it was. Placements are made one at a time and each is followed to a reap, so the adoption the
counter shows can only be the thread the test just placed.

What is deliberately no longer asserted is that the target then **ran** it. That is not a property
`spawn_on` has: it is a placement hint, not a pin, and a steal moving the thread first is correct
behaviour. The claim is not lost, it is decomposed and each half is now stable:
`every_secondary_runs_scheduled_work` proves every core runs what is on its own queue, and
`a_batch_of_cpu_bound_work_reaches_every_core` proves placement plus stealing fills the machine.
Delivery here, execution there.

The test also got a case it never had: **this core as a target**. `place_on` puts a local target
straight onto our own run queue, with no inbox and no IPI, so the old loop's `target == here`
iteration was not exercising the cross-core path at all. A placer thread on another core now makes
that placement, which is sound because a thread changes core only by a steal and only an idle core
steals: the core running the test thread never goes idle, so the placer cannot land on it.

### `a_thread_that_never_yields_is_preempted_anyway` (`sched.rs`): a race the test built for itself

`assert!(SPINNING > 0, "the spinner never ran at all")` is a **sample**, taken after `STOP` is set.
The order was: spawn the spinner, spawn the polite thread, wait for the polite thread, set `STOP`,
then check that the spinner had run. If the polite thread got its turn first, `STOP` was already
true when the spinner was finally scheduled, so it left its loop without incrementing anything and
the run went red while the kernel did nothing wrong.

The spinner running is a **precondition** of the claim, not the claim, so it is waited on rather
than sampled. Waiting for it before the polite thread exists also makes the rest stronger: the
polite thread's turn can then only have come from preempting a thread that was genuinely running,
and `preemptions()` is baselined after that point, so the preemptions the test claims are the ones
that gave the polite thread its turn.

**The one-second deadline went with it, and its replacement is the interesting part.** The budget is
now 200 **delivered ticks** on this core, not a wall-clock interval. A tick is when a preemption can
happen, so preemption opportunities are the unit the claim is counted in; and it is the one budget a
contended host cannot inflate, because descheduling the emulator delivers *fewer* ticks over a
stretch of wall clock while a `timer::now()` deadline keeps running whether the guest executes an
instruction or not. This is the same move the first round made on the drift twins (assert the law,
not the rate), in the scheduler instead of the timer.

The helper (`within_ticks`) does not yield, because this test must not, and it re-anchors its budget
if the thread changes core: the tick counter is per core, and a migration means we were preempted,
which is the news the test is waiting for anyway. If ticks stop entirely it does not return, and the
harness's 90 s per-test ceiling is the backstop. A timer that is not delivering at all is the arch
timer tests' failure to report, not this one's.

### The instrument that was missing: run the matrix under deliberate load

Both rounds so far have worked from CI failures, which means waiting for the family to bite someone
else's pull request and then reasoning backwards. There is a cheaper way, and it should be the first
thing anyone reaches for here:

```sh
# one spinner per host core, then the matrix
n=$(sysctl -n hw.ncpu); i=0
while [ "$i" -lt "$n" ]; do ( while :; do :; done ) & i=$(( i + 1 )); done
script/cpu-matrix; kill %1 %2 %3 %4 %5 %6 %7 %8
```

On an eight-core machine that takes the load average to ~22 and reproduces this family in one run.
The first time it was tried (2026-08-04, immediately after the three fixes above) it failed **three
models at two sites that had never been seen before**, and neither was one of the three just fixed:

| site | model | what it said |
|---|---|---|
| `sched.rs`, `a_sender_blocks_until_a_receiver_arrives` | `rv64` | "the sender never woke after its message was taken" |
| `sched.rs`, `other_threads_run_while_one_is_blocked` | `sifive-u54`, `rva22s64` | "a worker made no progress while another thread was blocked on IPC" |

Both are **a yield count used as a duration**, which is the defect `wait_for`'s own doc comment
describes and which round one had already fixed once, in `threads_round_robin`. Since §28 scattered
work across cores, fifty yields on a core with an empty run queue are microseconds; they elapse
before the thread being waited on has been scheduled at all. Five such waits were converted to
`wait_for`; five other yield loops in the same file were left, because a cleanup drain asserts
nothing and a negative assertion ("it must NOT have woken *yet*") only gets safer when the machine
is slow.

Every matrix run this round produced, in order, because reporting the best one would be the exact
dishonesty this milestone exists to remove:

| run | conditions (peak 1-minute load average) | result |
|---|---|---|
| 1 | shared dev machine, no induced load (LA ~3) | 5/5 pass |
| 2 | same (LA ~3) | 5/5 pass |
| 3 | 8 spinners on 8 cores (LA 22.8) | **3 fail**: `rv64`, `sifive-u54`, `rva22s64`, at the two yield-count sites above |
| 4 | 8 spinners, after converting those waits (LA 15.5) | 5/5 pass |
| 5 | 8 spinners (LA 36.5, the heaviest of the five) | 5/5 pass |

Runs 1 and 2 are what a green CI run would have said, and they said it before the yield-count sites
were touched: **two clean matrices in a row proved nothing about them.** That is the argument for
the recipe in one line.

**The lesson is about method, not about those two tests.** This family is reproducible on demand,
and it has been diagnosed from CI logs three times instead. A red matrix under load proves nothing
about a *model* (notes/cpu-models.md is emphatic about that, and it is right), but it is the best
available prover of an *assertion*: it is the condition under which a wait that measures the wrong
thing gives the wrong answer.

### The handler-latency twins: still not fixable with a wall clock, and now said plainly

`the_handler_keeps_up_when_no_lock_is_held` on both ISAs got the `missed_ticks_on` core-scoping and
nothing else, because the rest of it cannot be re-aimed on this instrument. A miss means a whole
tick period elapsed before the handler re-armed, and **from inside the guest a 30 ms handler and a
30 ms deschedule are the same observation**. `miss_detail` (aarch64) reports how late the re-arm was,
which distinguishes them for a human reading the panic, but "excuse the misses whose lateness has
the deschedule signature" is a weaker claim, not a re-aimed one: a handler slow by more than two
periods would be excused by it.

The honest alternative is the instrument, not the assertion. Under `-icount shift=0,sleep=off`
virtual time is a function of instructions executed, so a deschedule cannot advance it and "the
handler took fewer than N instructions" is a claim a contended runner cannot falsify. Unlike the
placement probe, this one has no reason to need more than one core, so the icount bench's `-smp 1`
is not an obstacle. Recommended here, not built here.

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

  ***"Rarer by orders" was measured on 2026-08-17 and is wrong about a loaded developer laptop.***
  The acceptance run below fired this assertion **four times in forty-five** runs, twice per ISA, on
  an eight-core machine at a one-minute load average between 26 and 63. "Pathological host" is doing
  work the word cannot do: the condition is an ordinary laptop with three lanes gating on it, which
  is this project's normal condition rather than its worst case. The rarity claim was relative to
  the assertion it replaced and was never measured against anything; it stays above, struck through
  rather than deleted, because the comparison it makes is still true and the adjective is not.
- **The handler-latency assertions (`the_handler_keeps_up_when_no_lock_is_held`, both ISAs,
  missed-tick delta over a five-tick window) keep their wall-clock exposure and are not fixed
  here.** The aarch64 one is in the milestone's evidence table ("left: 3, right: 2" in a quiet
  window) but outside this lane's five. A deschedule long enough to pass a deadline is counted as
  a miss, and the guest cannot tell that miss from a slow handler; `miss_detail` (added to the
  aarch64 timer for milestone 78) records how *late* the re-arm was, which is the discriminator a
  fix would build on, since a few hundred cycles late is a slow handler and a whole period late
  is the emulator. *(Second round, 2026-08-04: they are core-scoped now, so a migration is no
  longer one of the ways they can lie, and the verdict on the rest is written above: the fix is
  the icount instrument, and excusing deschedule-shaped misses would be a weaker claim rather than
  a re-aimed one.)* **Settled: aarch64 took the taxonomy on 2026-08-15 and riscv64 on 2026-08-16,
  a day apart because the record said the riscv64 twin did not exist. Both now pass a
  deschedule-shaped miss loudly, with its numbers, and fail a slow handler. The instruction-count
  claim is made now, on both ISAs, by `script/icount` (2026-08-17): the handler is bounded
  deadline-to-re-armed at 2,500 instructions against a measured 1,056 on aarch64 and 900 on
  riscv64. It is a separate boot rather than a `#[test_case]`, and notes/instruction-clock.md says
  why.**
- **The taxonomy's threshold leaves a window, on both ISAs, and it is one tick period wide.**
  `miss_detail` reports `now - next`, which is the lateness *beyond* the period already missed, so
  the cut at one interval classifies "one to two periods late" as a slow handler (red) and "two or
  more" as the emulator (pass). A host deschedule of between one and two tick periods therefore
  still fails, wearing the message that blames this kernel. Measured, not reasoned: a probe holding
  a lock across two and a half periods produced a lateness of 0.83 of an interval (2026-08-16, the
  fourth round's table). Widening the cut would trade the flake for a slow handler going
  unreported, which is the wrong trade while the honest fix (the icount instrument, where "the
  handler took fewer than N instructions" is not falsifiable by the host) is available and merely
  unbuilt. **The instrument is built** (2026-08-17, `script/icount`), and it does not close this
  entry so much as route around it: the ambiguous window is a property of a taxonomy that exists
  only because the host can deschedule the guest, and on the instrument nothing can, so that boot
  asserts `missed_ticks == 0` with no taxonomy at all. The window survives on the test path, where
  the taxonomy still lives, and that is now the only place it survives. See
  notes/instruction-clock.md.
- **Scope was five sites, not 39.** The roadmap's scope note counts 39 sites in 7 files matching
  the shape (`wait_for`, or assertions against `free_frames`, `thread_count`, `used()`). The
  other 34 were not audited here; the diagnostic above is the checklist for reading any of them.
  *(Rounds two, three and four took eight more between them, all found by reading rather than by
  waiting for a red run. The backlog is real and it is smaller than 34.)*
- **The concurrency confound that broke the first reuse assertion also reaches the frame assertion
  beside it**, and this was found by reasoning after the fact rather than by a red run, so it is
  recorded as a fact rather than as a fix. If the second batch's peak concurrency exceeds the
  first's, `NEXT_STACK_VA` legitimately bumps, and if that bump straddles a 2 MiB boundary a page
  table is legitimately built and `used()` legitimately sits above `before`. A false failure, from
  scheduling, with no leak. It is rare for the same reason the assertion is a weak detector: a
  couple of slots is 56 KiB against a 2 MiB span, so roughly 3% of bumps land on a boundary, and a
  bump needs the batch to be reaped later than the one before it. Both halves of that product would
  have to fire in the same run. Worth knowing before anyone reads a red run at this site as a leak.
- **A frame count is a weak detector of a page-table leak, and the reaper test's is the measured
  case.** Eight thread stacks are 224 KiB of address space against a 2 MiB L3 span, so a leak
  charged per 2 MiB is usually invisible; the fifth round proved it by deleting the VA push and
  watching the leg go green. The reuse assertion added beside it covers that defect directly, and
  the frame bound is now scoped to the leak it can see. **The same question is unasked at every
  other site that infers a mechanism from a frame count**, which is the third grep the fifth round
  adds to the reading order.
- **The reuse assertion reads a global watermark, deliberately, and what makes it safe is the unit
  rather than the direction.** A one-way failure direction was not enough on its own: the first
  version had one and was still wrong, because the watermark bounds *concurrency* and the claim was
  about *reuse*. One thread is the unit precisely because one thread cannot exceed a high-water mark
  eight deaths just set. Two residuals remain, both requiring a coincidence rather than mere load.
  A neighbour that drained `FREE_STACK_VAS` to empty between the watermark read and the probe's
  spawn would fail it falsely; nothing else spawns during this test (tests run sequentially on the
  boot thread and the other cores are idle), and it needs the list down to its last slot. And a
  neighbour bumping the watermark could mask a genuine reuse failure for one run, which is the
  mirror of the `<=` coincidence caveat above; the defect is per spawn and permanent, so it fails
  every other run regardless.
- **The `>=` frame assertion in `kernel_stacks_do_not_touch_the_frame_allocator_in_steady_state`
  inherits the coincidence caveat above**, in the other direction of the same trade: a real
  regression of `k` frames passes if a neighbour frees at least `k` inside the same window. The
  defect it guards (kernel stacks drawn from the frame allocator instead of the kernel budget) is
  per spawn and persistent, so six spawns fail it essentially every run regardless; a one-shot
  coincidence pass is possible in a way `assert_eq` did not permit, and the equality was demanding
  that the rest of the machine hold still.
- **`work_can_be_placed_on_every_core` no longer proves that a *specific* core executed a
  *specific* thread**, and nothing else does either. That is the second round's honest cost.
  Delivery to the named core is asserted exactly; execution is asserted for every core, but over
  the population of threads rather than per placement. Closing it needs a pin, which DECISIONS §28
  deliberately deferred: while placement is a hint, "this thread ran on that core" is not a
  property the scheduler has, and a test asserting it is asserting a coincidence.
- **The adoption counter can in principle be moved by something other than the placement under
  test.** Serving a steal pushes into the requester's inbox, so a target that stole from a third
  core in the same window would also increment. The test closes that by construction (one
  placement in flight at a time, each followed to a reap, so nothing else is runnable to steal),
  not by the counter being unambiguous. A version that placed several at once would have to reason
  about this again.
- **Two siblings were checked against the same question and left alone**, which is what the
  milestone's scope note asks for.
  - `user/tests.rs`, `a_user_program_that_never_yields_is_preempted_anyway` spins a tenth of a
    second of counter time and then asserts `preemptions()` rose: a wall-clock window with no
    wait, so it is the family's shape. It survives because falsifying it needs the emulator
    descheduled across *all four* cores for the whole window, where the `sched.rs` twin needed
    only an unlucky order on one. Nothing has ever been recorded against it.
  - `smp.rs`, `every_secondary_runs_scheduled_work` indexes `RAN_ON` by the core a probe ran on,
    the indexing the placement probe had to abandon. It survives because each secondary's probe is
    spawned onto its own queue as that core's first act and exits at once, so the window in which
    an idle neighbour could steal it is a few instructions rather than a whole placement loop. If
    it ever does fail, suspect this first; the fix is the placement probe's.

## See also

- design/roadmap/78-load-sensitive-assertions.md: milestone 78's spec and the day's evidence
- notes/cpu-models.md BUGS (the load-sensitivity evidence, including the control model failing)
- notes/live-replacement.md BUGS (the completed frame-hygiene analysis, the template)
- notes/riscv-parity-scope.md (the shape named: a wait written against something wider than the
  property)
- notes/benchmarks.md (the two instruments, and why icount cannot host cross-core tests)

### `the_handler_keeps_up_when_no_lock_is_held` (aarch64): the taxonomy its comment promised

Resolved 2026-08-15, the day merge-queue runner load made it urgent: the assertion broke three
unrelated pull requests in one afternoon (#204, #210, #215), because parallel queue-group builds
saturate the shared runners and a descheduled emulator misses deadlines the guest cannot tell
from a slow handler. The comment above the assertion had already written the taxonomy: re-armed
less than one interval late is a slow handler and our bug; a whole interval or more is the
emulator descheduled and says nothing about this kernel. The assertion now applies it. The
deschedule shape prints its numbers and passes, loudly labeled; the slow-handler shape still
fails. The riscv64 tour has no direct twin of this assertion (checked when the scheduler smoke
line was fixed the same day); if one grows, it takes the same taxonomy.

***That last sentence was wrong, and the twin was a `#[test_case]` rather than a tour line. See
the fourth round below.***

## The fourth round, 2026-08-16: the block's three were already done, and three more were not

This round started from milestone 78's own evidence table, which names **three assertions that
report a negative discrepancy** and asks for three arguments. The first thing done was to check
them against the tree rather than against the table, because this project's record is that a
diagnosis written days ago has usually been overtaken. All three had been:

| the block's three | site | state on 2026-08-16 |
|---|---|---|
| reaper count, `left: 5, right: 6` | `sched.rs`, `a_finished_thread_is_reaped_and_its_memory_returned` | rescoped; per-`Tid` `thread_present` waits and `used() <= before` |
| address-space frames, "-52" and "-19" | `user/tests.rs`, `a_dead_user_thread_frees_its_whole_address_space` | rescoped, the same two changes |
| frame hygiene, margin of two frames | `user/live_swap_tests.rs` | removed 2026-08-03 in PR #46, with the analysis |

So **the block's "the split that makes this two problems" section is history, not a worklist**, and
its "What is left" section is the current word. The verdicts above already said so; what this round
adds is that a reader coming to the block first will be sent to three finished sites, which is the
kind of stale pointer §71 exists to catch.

What *was* left is what reading the same two questions across the files this lane could touch turned
up: `sched.rs` and the two arch timer files, three other lanes holding the test wiring. Three sites
answered wrong, and one of them is the note contradicting itself on a single page.

### `the_handler_keeps_up_when_no_lock_is_held` (riscv64): the twin the record said did not exist

**The 2026-08-15 verdict directly above ends "the riscv64 tour has no direct twin of this
assertion", and the BUGS section on this same page says the handler-latency assertions are unfixed
on *both ISAs*. Both cannot be true, and the BUGS line is the correct one.** The twin is
`kernel/src/arch/riscv64/timer.rs`, a `#[test_case]` in the arch timer module, five tick periods and
a bare `assert_eq!` on the missed-tick delta, exactly the assertion that broke #204, #210 and #215
on the other ISA. What was checked that day was the **boot tour**, which is a different artefact
from the test suite and genuinely has no such line; the conclusion was then written down about the
ISA. That is rung four of the ladder failing in its usual way, a fact living in one sentence with
nothing comparing it to the code.

The fix is the aarch64 one, ported without variation, which is what rule 5 asks for: a `miss_detail`
module behind `#[cfg(test)]` recording `(now, next, count)` from inside `rearm`, three relaxed
stores in trap context per DECISIONS §9, and a `deadline()`-style accessor beside `missed_ticks()`.
The assertion now applies the taxonomy the aarch64 comment wrote: re-armed **less than one interval
late is a slow handler and this kernel's bug**, and it still fails; **a whole interval or more is
the emulator having been descheduled**, and that prints its numbers and passes, labeled. Nothing
about the kernel's sensitivity changed on either ISA; what changed is that one of them was carrying
a flake the other had already stopped carrying.

### `reclaim_frees_an_embryo_tcbs_region` (`sched.rs`): the reaper count's defect, three tests away

The reaper count's fix has been called "the third appearance of this exact fix" on this page. It is
the fourth, and the fourth site is in the same file as the third. The test bracketed
`crate::sched::thread_count()` around a retype and a reclaim:

```rust
let threads_before = crate::sched::thread_count();
// ... create the region, retype an unstarted TCB out of it ...
assert_eq!(thread_count(), threads_before + 1, "the embryo should be in the table before reclaim");
// ... reclaim ...
assert_eq!(thread_count(), threads_before, "the TCB's table slot must be freed by reclaim");
```

The headcount is the size of the whole table. A neighbouring thread finishing its teardown between
the baseline and the first read lands the count at `threads_before`, one **below** what the
assertion demands, and the run goes red accusing an embryo that is present and correct. Negative
direction, a global baseline, a claim about one object: the milestone's signature, unrecorded
against this site only because nothing had happened to fall on it yet.

**The rescope is also the stronger claim, which is the part worth keeping.** `create_tcb` returns a
generational `Tid`, so `thread_present(tid)` asks the narrow question the test is responsible for
("is *this* embryo in the table"), and it is immune to neighbours by construction. The old second
assertion could pass with the embryo still sitting in the table, as long as somebody else's thread
left in the same window; the new one cannot. `thread_present`'s own doc comment has argued this
since it was written.

The frame half of the same test (`assert_eq!(free_frames(), frames_before)`) was checked against the
same question and **left alone deliberately**. Its window is a region create and a reclaim with no
wait in it, microseconds of guest execution rather than the seconds a `wait_for` spans, and the
claim "reclaim returns the region's memory *exactly*" is the property under test rather than a
proxy. Loosening it to `>=` would trade real coverage for an exposure nothing has ever hit. The same
reading applies to the other four `reclaim_frees_*` and `split_returns_*` frame equalities in that
file, and they were left for the same reason: they bracket synchronous operations, not waits.

### `kernel_stacks_do_not_touch_the_frame_allocator_in_steady_state` (`sched.rs`): the whole family in three lines

The clearest single specimen anywhere in this note, and it had all three failure modes at once:

```rust
let baseline = super::thread_count();
for _ in 0..6 {
    super::spawn(|| {}).expect("spawn failed");
    while super::thread_count() > baseline { super::yield_now(); }
}
assert_eq!(memory::stats().unwrap().free(), free_before, "...");
```

1. **A global baseline**, `thread_count()`, which is the reaper count's defect again.
2. **An unbounded yield loop with no clock at all.** Both directions are wrong. A neighbour reaping
   first puts the count at or below the baseline and the loop exits immediately, leaving this
   batch's stacks in flight when the frame count is read. A neighbour's thread outliving the batch
   holds the count above the baseline and the loop **never exits**, spinning until the harness's
   90 s per-test ceiling and reporting a hang in a test about kernel stacks. Nothing in it was
   bounded by anything.
3. **A global frame equality**, which is the assertion the reaper test and the address-space test
   both traded for `<=` on the argument that a neighbour's late teardown can only free frames.

All three take the fixes already argued on this page. Each spawn is followed to *its own* reap by
`thread_present` on the `Tid` it returned, bounded by the module's `wait_for`. The final assertion
becomes `free() >= free_before`, waited on, which is `used() <= before` in the other units: the
defect this test guards spends allocator frames on kernel stacks, driving `free` **down** and
keeping it there, so a real regression times the wait out and fails with the frame count in the
message. A dead `REAPED` static, stored to and never read, went with it.

### The instrument, and the loop this round ran

The note's own recipe is eight spinners and then the matrix. **This round deliberately did not use
it**, and the reason belongs here rather than in a report: five other lanes were gating on the same
laptop, and eight spinners would have failed somebody else's run with exactly the family under
study, which is a worse outcome than a slower measurement. The machine supplied its own contention
instead, at a one-minute load average between 8 and 12 on eight cores for the whole loop, which is
the condition the recipe manufactures.

Twelve consecutive full `script/test` runs, both ISA legs each (aarch64 and riscv64 kernels under
QEMU, plus the host-logic crates), on the changed tree: **12 of 12 green**, one-minute load average
between 4.2 and 22.2, the heaviest run being run 9 at 22.2, which is where the eight-spinner recipe
puts an eight-core machine. See the milestone report for the table; the result is stated here
because a flakiness claim with no run count is the thing this milestone exists to delete.

**And the honest half of that number: in all twelve runs the taxonomy branch was never taken.** Not
once, on either ISA, did a miss land inside the five-period window, so twelve green runs are
evidence that nothing regressed and **no evidence at all** that the new classification works. That
is the same trap the second round recorded ("two clean matrices in a row proved nothing about
them"), and it is why the branch was proven by injection instead:

| probe (riscv64, reverted after) | what it produced | result |
|---|---|---|
| force a real miss, then stamp `miss_detail` with a sub-interval lateness | `late_by` 0, interval 100000 | **red**, "the handler itself is slow, which is this kernel's bug" |
| force a real miss, leave the recorded lateness alone | `late_by` 82830, interval 100000 | **red**, same message |

The second row is the interesting one and it was not the expected answer. A lock held across two
and a half tick periods produces a lateness of between half and one and a half intervals (the
handler runs at `lock + 2.5i`, and `next` is `D + i` where `D - lock` is somewhere in `(0, i]`), so
it straddles the threshold and this run landed under it. **The taxonomy's cut is at one interval of
`now - next`, and `now - next` is already the lateness *beyond* the first missed period.** So the
classification really reads: one to two periods late is called a slow handler and fails; two or more
is called the emulator and passes. A host deschedule between one and two tick periods is still a red
run on both ISAs. The aarch64 fix narrowed that window rather than closing it, and porting it
faithfully carries the same residual, which belongs in BUGS rather than in a wider riscv64 threshold
that would break parity with the twin.

### What this round did not do

**The icount instrument is still recommended and still not built**, for both remaining claims: that
SBI actually fired at the riscv64 software grid's deadlines, and that the handler takes fewer than
N instructions. Both need `-icount shift=0,sleep=off`, which lives in the bench harness rather than
the test suite, and the harness files were held by other lanes this round. That is the milestone's
"What is left" section, unchanged.

**The other 34 sites in the scope note were not audited**, again. What this round adds to that
backlog is a sharper reading order than "check them against the same question": the three found
here were all found by grepping the allowed files for a **global count taken as a baseline**
(`thread_count()`, `free_frames()`, `stats().free()`) and for a **loop with no clock in it**. Those
two greps are cheap, and between them they caught every site this round changed.

## The fifth round, 2026-08-17: the assertion that could not catch the bug in its own name

This round was briefed on the block's three negative-discrepancy assertions, which the fourth round
had already found finished and said so on this page. The brief was written from the **block**, whose
gate line still asked for "three small changes with three arguments" and whose evidence table still
read as a worklist. So the fourth round's own complaint (a reader coming to the block first is sent
to three finished sites) had a second victim, and the fix this time went into the table rather than
onto this page: the block's evidence rows now carry a **disposition column**, and its gate line names
the icount instrument, which is what is actually left. A note saying the table is stale does not stop
the table being read.

Checking the three a second time produced one record correction and **one defect**, and the defect is
the more interesting half.

### The reaper test could not catch milestone 6's leak, and an injection proves it

`a_finished_thread_is_reaped_and_its_memory_returned` exists for one defect: stack address ranges
that are not reused, so an L2 and an L3 page table accumulate per 2 MiB of address space consumed,
forever. It asserted that a second batch of eight threads costs zero extra frames, and the first
round re-aimed that assertion from `==` to a waited `<=` on the argument that "sensitivity to the
milestone-6 bug is unchanged".

**The sensitivity was unchanged and it was never there.** Deleting the `FREE_STACK_VAS` push from
`KernelStack::drop`, which is that defect exactly, passed the **entire aarch64 leg** including this
test. The arithmetic says why, and it is not luck:

- a slot is `STACK_SLOT_SPAN`, 7 pages, 28 KiB;
- eight of them consume 224 KiB of fresh address space;
- a leaked page table costs a *frame* only when the bump crosses a 2 MiB L3 boundary;
- 224 KiB is 11% of one table's span.

So the frame count can see the defect only when the batch happens to straddle a boundary. **And it is
worse than 11% random.** Where `NEXT_STACK_VA` stands when this test runs is a function of how many
threads the tests before it spawned, which is fixed for a given tree, so for any given tree the
assertion either always catches the defect or always misses it, and which one is decided by unrelated
code upstream. That is a detector whose sensitivity is set by its neighbours, which is the family's
own theme arriving from a direction this page had not recorded: not a neighbour causing a false
failure, a neighbour deciding whether a real failure is visible at all.

The frame count was always a **proxy** for reuse. The fix is to assert the mechanism: every thread in
the second batch must land **below the watermark that stood before the batch began**, which is what
reusing a dead thread's range means. Each thread reports its own `sp`, because a test cannot read the
stack out of a thread it is simultaneously waiting to see reaped, and a local's address is a stack
address with no race in it.

**The failure direction is one-way, so this is not a new global exposure.** `NEXT_STACK_VA` moves only
when the free list is empty, so a neighbour spawning inside the window can only *raise* the watermark,
and raising it makes the claim easier to satisfy. Contention cannot fail it; that is the same test the
first round applied to `used() <= before`, applied to the half that was still a proxy.

The frame assertion stays, scoped to the leak it can actually see (a per-thread frame the reaper did
not return), and the comment claiming its milestone-6 sensitivity was corrected in place rather than
deleted, because what it said was true about the arithmetic it was defending and false about the bug.

#### The first version of that assertion joined the family, which is the useful part

**It asserted the watermark claim for all eight threads of a batch, and failed on a clean kernel**, on
the first full gate run: thread 1, two slots above the watermark, on a tree with nothing injected. The
two runs before it (the defect injected, then reverted) had both agreed with it, which is exactly how
long this family usually takes to look settled.

The diagnosis is this page's own, turned on its author. **The watermark is the high-water mark of
`FREE_STACK_VAS` running dry, which is a fact about how many threads were alive at once, not about
whether dead ones are reused.** Eight sequential spawns need as many slots as the reaper falls behind
by, and that number is a scheduling outcome: a batch whose threads are reaped later relative to
spawning legitimately needs more slots than the previous batch did and bumps the watermark. So the
assertion was written against something **wider than the property**, and load decides whether the
extra width shows. That is the first-round diagnostic verbatim, committed while fixing the site it
diagnoses.

The fix is to make one thread the unit, because one thread cannot exceed a high-water mark that eight
have just set. After a batch of eight has been spawned and reaped, `FREE_STACK_VAS` is provably
non-empty: over the batch, eight pushes against eight pop-or-bump decisions leave the list at its
starting size plus the number of bumps, and a bump only happens when it was empty. So the list holds
at least one slot, every slot in it was handed out below the watermark, and a single pop cannot drain
what eight deaths just stocked. The probe therefore lands below the watermark on any kernel that
reuses at all, and at the watermark on one that does not.

**Two lessons, and the second is about method rather than about stacks.** A one-way failure direction
is necessary and not sufficient: this assertion had one (a neighbour can only raise the watermark) and
was still wrong, because the quantity it bounded was the wrong quantity. And **an injection that fires
proves only that the assertion can fail, never that it fails for the right reason.** The injected run
went red and the clean run went green, and the assertion was still measuring concurrency. Only a
second clean run under different scheduling said so, which is the argument for the load recipe in this
note's second round, arriving from a third direction.

### Three panics that could still print the impossible quantity

The three converted sites all wait on a one-directional bound and then **re-sample the allocator to
format the panic**. The measurement was re-aimed in the first round; the message was not, and it is
the message a person reads at 2 a.m. deciding "known or real".

- `user/tests.rs` prints `used() as i64 - before as i64`, so a genuine timeout whose frames land in
  the gap between the wait giving up and the panic being formatted still prints **"-19 frames did not
  come back"**: the exact string this milestone is named for, now emitted by a form that can no longer
  fail for that reason. A reader who trusts the sign re-runs the whole 2026-08-03 investigation.
- the two `sched.rs` sites use `saturating_sub`, which is worse rather than better. It clamps the
  impossible quantity to **"leaked 0 frames"** and removes the sign that gave the original bug away.

All three now report the observation the predicate actually decided on. `wait_for` re-evaluates once
past its deadline, so a `false` return leaves the captured sample strictly on the failing side of the
bound: the printed count is positive by construction, with no cast and no clamp.

### The injections, and what each one settled

Every injection was reverted. Recorded in full, including the two that did not reach their target,
because a failed injection is how the useful facts arrived.

| injection | what it is | result |
|---|---|---|
| delete the `FREE_STACK_VAS` push in `KernelStack::drop` | milestone 6's leak, exactly | **whole aarch64 leg green.** The defect is invisible to the suite |
| the same, against the first (eight-thread) reuse assertion | | **red on thread 0**, naming both addresses |
| restore the push, keep that assertion | | green, and misleading: two runs agreed with an assertion that was measuring concurrency |
| nothing injected, full gate | the run that caught it | **red on thread 1, on a clean kernel.** The eight-thread form was wrong; see above |
| the VA push deleted again, against the single-thread probe | | **red**, sp `…6ff80` against watermark `…69000` |
| skip `untyped::destroy` in `AddressSpace::drop` | a dead space returns nothing | never reached the target test: exhausted memory ~370 tests in, at "no stack region for the net client" |
| leak one frame per `AddressSpace::drop` | a dead space returns all but one frame | caught by `reclaim_frees_an_unbound_address_spaces_region` first, `left: 61501, right: 61502` |
| leak one frame per dead user thread, in `finish_switch`'s reap arm | narrowed to the aspace test's own subject | caught by `destroy_force_kills_a_runaway_and_reclaims_its_region` first, `left: 52403, right: 52404` |
| leak four frames inside the aspace test's measured window | the defect arranged where only this assertion can see it | **red**, "four user address spaces came and went and **4** frames did not come back" |

Three things worth keeping from the misses, and they are worth more than the hit.

**The whole-region injection is too coarse to test anything downstream of it**, so a leak injection
aimed at a late test has to be one frame wide.

**The exact-equality frame assertions the fourth round deliberately left alone are genuinely
sensitive.** Two of them (`reclaim_frees_an_unbound_address_spaces_region` and
`destroy_force_kills_a_runaway_and_reclaims_its_region`) each caught a **one-frame** leak, ahead of
the target test both times. That is the measurement behind that round's argument that a synchronous
bracket keeps its `==`, which until now was only an argument.

**And it says something about the aspace test that three rounds of prose did not.** A leak in the
kernel path it guards is caught by an earlier, exact, synchronous assertion before this test runs, in
both narrowings tried. Its `used() <= before` is not the tree's first line of defence against that
defect and probably never was; what it uniquely covers is a leak that only appears after a *user*
thread has faulted and been reaped four times over, which no `reclaim_frees_*` bracket stages. Worth
knowing before anyone spends another round on it. The final row is that assertion proven directly:
the defect arranged inside its own window, the wait timing out, and the new message naming the
injected count exactly rather than a figure re-sampled after the fact.

### What this round did not do

**The icount instrument is still recommended and still not built.** Unchanged from the fourth round,
and it remains the whole of the block's "What is left".

**The other sites in the scope note were not audited**, again. What this round adds to the reading
order is a third grep beside the fourth round's two: a **frame count standing in for a mechanism**.
The two greps for a global baseline and for a clockless loop find assertions that fail when they
should not. This one finds assertions that pass when they should not, and nothing on this page had
looked for those.

## The sixth round, 2026-08-17: the instrument, and the two claims it was owed

The five rounds above re-aimed everything that could be re-aimed. What they could not re-aim, they
deferred to the same place every time, in six separate paragraphs across this page and the block:
**the icount instrument, recommended and not built.** This round built it. It is `script/icount`,
a boot mode rather than a `#[test_case]`, and notes/instruction-clock.md is its note.

### What was actually unaskable, and why no margin was ever going to do

Both remaining claims fail on the same sentence, which this page states twice: *from inside the
guest, a slow handler and a descheduled emulator are the same observation.* That is not a
sensitivity problem, so it does not have a sensitivity fix. Widening hides the defect (DECISIONS
§61, and the block forbids it by name); deleting the assertion is what round one already refused.

The third option is to change the **unit**. Under `-icount shift=0,sleep=off` virtual time advances
by exactly one nanosecond per guest instruction retired and by nothing else, so a claim denominated
in instructions has no host term in it at all. Both claims are now stated that way:

| claim | aarch64 | riscv64 | bound |
|---|---|---|---|
| deadline to handler observing it | 1,008 instructions | 300-400 | 2,000 / 1,500 |
| deadline to next one armed (the whole handler) | 1,056 | 800-900 | 2,500 / 2,500 |
| ticks missed over 64 sampled | 0 | 0 | 0 |

**The aarch64 numbers are identical across all 64 ticks**, minimum equal to maximum. That is the
instrument demonstrating itself: the measurement has no variance, so a bound on it is a statement
about this kernel and about nothing else. riscv64's pair differ by one counter tick because that
ISA's `rdtime` reads in steps of 100 instructions where aarch64's counter reads in steps of 16.

### The injection, which is the only part of this that proves anything

The claim that matters is riscv64's, because it is the one this milestone was left holding: SBI's
`set_timer` is write-only, so `DEADLINE` is our own array and reading it back proves only that the
kernel remembers what it meant to write. The block names the exact residual: *"an implementation
that maintains `DEADLINE` correctly and arms SBI with something else"*.

So that implementation was built and run, twice, each a single line in `rearm` with the grid store
left untouched beside it. Both were reverted.

| injection | `script/icount --arch riscv64` | the riscv64 leg of `script/test` |
|---|---|---|
| `sbi_set_timer(now + interval())`: re-anchor every tick | **red**, arrival 420,400 instructions against a bound of 1,500 | **red**, `the_handler_keeps_up_when_no_lock_is_held` |
| `sbi_set_timer(next + interval() / 4)`: a fixed offset, no drift, no misses, 100 Hz still exactly delivered | **red**, arrival 2,500,400 on every one of 64 ticks | **red**, `ticks_arrive_at_the_configured_rate` |

**The prediction was that the suite would miss them, and it did not. That is the useful part.** What
the suite cannot do is say what is wrong. The first injection fails as *"the timer handler is taking
longer than a whole tick period, with no lock held... Late by less than one interval means the
handler itself is slow, which is this kernel's bug"*, which is false, and which is the assertion that
broke #204, #210 and #215 in one afternoon. The second fails as *"no miss-free measurement window in
eight tries: either the host is too contended to observe the grid, or the handler is slower than a
whole tick period"*, whose first clause is an invitation to re-run.

The icount message names the actual defect: *"either the trap path grew, or the timer was armed with
something other than the deadline the kernel recorded"*, on a number that has no host term in it.

**So what the instrument buys is diagnostic certainty rather than detection**, which is this
milestone's own thesis rather than a lesser result. The block's cost line is that every red check
here needs a human to decide "known or real", and that on 2026-08-03 the judgement was made six times
and got the wrong answer twice. Both injections produce exactly that judgement call on the test path,
and none of it on the instrument.

A third injection asked what the instrument can *see* rather than whether it fires: exactly 200
instructions added to the aarch64 `tick`. Arrival went 1,008 -> 1,216 and the handler 1,056 -> 1,264,
both **+208** on every one of 64 ticks, with the eight-instruction residual (the loop's operand
setup) smaller than one counter tick. That is the resolution measured, and it is what makes the
instrument able to answer milestone 106's pricing question on aarch64.

### What the instrument cost, and a correction it forced

The block states the cost as *"icount is slower and changes what the suite measures"*, and the
first half of that had never been measured. **It is wrong**:
the same bench boot took 2.47-2.61 s under `-icount shift=0,sleep=off` and 2.62-2.80 s without it,
three runs each, on the same binary. `sleep=off` fast-forwards virtual time through idling, which
covers icount's per-instruction overhead.

The two real reasons are different and better. Every vCPU shares **one** virtual clock, so the
instrument is `-smp 1`, and a suite run there would not fail: it would silently stop proving every
cross-core property it exists for. And a clock-bound wait stops costing host time and starts costing
instructions, at roughly five to one. The first of those is the same fact that keeps the placement
probe on the wall clock, arriving from the other side.

### What this round did not do

**The other sites in the scope note were still not audited.** Five rounds have now said this. The
reading order (three greps: a global count as a baseline, a loop with no clock, a frame count
standing in for a mechanism) is the accumulated answer and nobody has run it across the remaining
files.

**Only the timer is instrumented.** `tick_trace` is three relaxed counters and one call site, not a
framework, and any other path wanting an instruction-denominated claim needs its own. That is
deliberate: the block asked for two claims, not for infrastructure.
## The acceptance run, 2026-08-17: 45 loaded runs, 36 green, and what the nine reds were

Milestone 62 has said since 2026-08-01 that its own fix cannot be verified by running the suite
once: a flake that fires one run in six is indistinguishable from a fixed one until you have run it
many times, so the evidence it asks for is a **repeat count under load**. Four rounds of this page
produced such a count three times, by hand, and each time the number lived in a report. This is that
count taken by an instrument that records its own conditions, `script/repeat-under-load`.

**The result is not the green the milestone hoped for, and that is the useful part.** Nine of
forty-five runs went red. Eight were two assertions, in perfect ISA symmetry, both already named in
this page's BUGS section as known residuals that only the icount instrument can close. The ninth was
a real kernel bug that nothing in this tree had recorded.

### The instrument and the conditions, stated before the result

`script/repeat-under-load -n 45 -s 8`, which starts one busy loop per host core, runs `script/test`
(both ISA legs plus the host crates), and records per run the elapsed seconds, the one-minute load
average sampled every ten seconds, and how many QEMU processes were up.

- Host: Mac15,3, 8 cores, Darwin 25.5.0 arm64. Tree `d9f0d151`.
- 45 runs, 108 minutes of wall clock, 2026-08-17 22:46Z to 2026-08-18 00:34Z.
- One-minute load average across the whole loop: **26.1 low, 63.0 peak**, which is at and above the
  ~22 the eight-spinner recipe above puts an eight-core machine at.
- **The machine was not mine alone, and that is recorded rather than smoothed over.** Two other
  lanes were gating on the same laptop for the first third of it, one of them running its own
  emulator under `-icount`. Their emulators are the `QEMUs seen` column: this run has at most one
  alive at a time, so any sample reading two or more is a neighbour.
- A pilot run of the same command was cut short after one run and is not counted here; the eight
  spinners were stopped between every run, and the loop left no QEMU behind.

### Every run, in order

Reporting only the interesting ones would be the exact dishonesty this milestone exists to remove,
which is the second round's rule and it still holds.

| run | result | seconds | 1-min load average (min/mean/peak) | QEMUs seen | what failed |
|---|---|---|---|---|---|
| 1 | pass | 171 | 28.9 / 39.7 / 50.8 | 2 |  |
| 2 | **FAIL** | 47 | 31.5 / 34.9 / 39.2 | 1 | aarch64 `timer.rs:414`, the eight-attempt retry budget |
| 3 | **FAIL** | 100 | 39.0 / 41.5 / 44.0 | 3 | riscv64 `timer.rs:509`, the miss taxonomy |
| 4 | pass | 160 | 40.4 / 45.4 / 49.0 | 3 |  |
| 5 | **FAIL** | 104 | 38.6 / 42.3 / 45.4 | 3 | riscv64 `timer.rs:509`, the miss taxonomy |
| 6 | pass | 151 | 33.4 / 38.6 / 42.4 | 2 |  |
| 7 | pass | 151 | 35.7 / 38.4 / 43.2 | 4 |  |
| 8 | **FAIL** | 113 | 32.5 / 36.7 / 39.8 | 3 | riscv64 `timer.rs:455`, the eight-attempt retry budget |
| 9 | pass | 157 | 37.1 / 39.5 / 44.6 | 3 |  |
| 10 | pass | 152 | 31.8 / 36.1 / 43.2 | 4 |  |
| 11 | **FAIL** | 51 | 31.8 / 32.7 / 33.8 | 2 | aarch64 `timer.rs:470`, the miss taxonomy |
| 12 | pass | 158 | 34.3 / 39.3 / 41.8 | 4 |  |
| 13 | **FAIL** | 51 | 36.7 / 40.3 / 43.9 | 3 | aarch64 `timer.rs:414`, the eight-attempt retry budget |
| 14 | **FAIL** | 102 | 39.2 / 41.6 / 45.1 | 3 | riscv64 `timer.rs:455`, the eight-attempt retry budget |
| 15 | pass | 167 | 38.2 / 42.4 / 48.3 | 3 |  |
| 16 | pass | 164 | 48.0 / 51.1 / 56.5 | 3 |  |
| 17 | pass | 175 | 31.4 / 40.4 / 46.9 | 2 |  |
| 18 | pass | 149 | 26.9 / 30.6 / 33.6 | 1 |  |
| 19 | pass | 164 | 26.1 / 30.2 / 35.8 | 1 |  |
| 20 | pass | 145 | 28.1 / 30.6 / 35.8 | 1 |  |
| 21 | pass | 160 | 33.8 / 41.8 / 50.7 | 1 |  |
| 22 | pass | 160 | 43.0 / 46.7 / 52.0 | 1 |  |
| 23 | pass | 148 | 33.6 / 40.0 / 50.9 | 1 |  |
| 24 | pass | 148 | 29.0 / 39.0 / 52.4 | 1 |  |
| 25 | pass | 159 | 27.1 / 35.8 / 42.0 | 1 |  |
| 26 | pass | 161 | 29.8 / 35.1 / 41.7 | 1 |  |
| 27 | pass | 151 | 35.5 / 40.3 / 45.5 | 1 |  |
| 28 | pass | 172 | 31.9 / 37.6 / 41.3 | 1 |  |
| 29 | **FAIL** | 43 | 36.6 / 39.8 / 41.7 | 0 | aarch64 `timer.rs:470`, the miss taxonomy |
| 30 | pass | 164 | 41.7 / 50.1 / 54.0 | 1 |  |
| 31 | **FAIL** | 117 | 33.6 / 39.7 / 49.8 | 1 | riscv64 `frames/src/lib.rs:315`, **double free of a frame** |
| 32 | pass | 161 | 34.6 / 39.7 / 42.3 | 1 |  |
| 33 | pass | 162 | 33.6 / 37.8 / 40.6 | 1 |  |
| 34 | pass | 170 | 31.1 / 39.6 / 47.5 | 2 |  |
| 35 | pass | 161 | 30.5 / 37.6 / 52.2 | 1 |  |
| 36 | pass | 150 | 32.5 / 37.0 / 45.6 | 1 |  |
| 37 | pass | 159 | 34.1 / 40.2 / 45.4 | 1 |  |
| 38 | pass | 160 | 43.6 / 47.2 / 51.7 | 1 |  |
| 39 | pass | 161 | 40.0 / 46.2 / 48.5 | 1 |  |
| 40 | pass | 157 | 33.7 / 36.4 / 40.1 | 1 |  |
| 41 | pass | 160 | 34.6 / 40.7 / 45.6 | 1 |  |
| 42 | pass | 175 | 41.9 / 53.5 / 63.0 | 1 |  |
| 43 | pass | 161 | 31.3 / 40.7 / 54.1 | 1 |  |
| 44 | pass | 163 | 29.6 / 34.4 / 38.9 | 1 |  |
| 45 | pass | 174 | 28.5 / 40.4 / 52.7 | 1 |  |

### The eight timing reds are two assertions, twice each, on both ISAs

The symmetry is the finding, because it says this is a property of the assertions rather than of
either architecture:

| assertion | aarch64 | riscv64 | what it said |
|---|---|---|---|
| `ticks_arrive_at_the_configured_rate`, the eight-attempt retry budget | `timer.rs:414`, runs 2 and 13 | `timer.rs:455`, runs 8 and 14 | "no miss-free measurement window in eight tries" |
| `the_handler_keeps_up_when_no_lock_is_held`, the taxonomy's cut | `timer.rs:470`, runs 11 and 29 | `timer.rs:509`, runs 3 and 5 | "the timer handler is taking longer than a whole tick period, with no lock held" |

**Both were predicted on this page and neither had ever been observed.** The BUGS section says the
drift test "can still go red on a pathological host, by design" and calls that "rarer by orders";
and it says the taxonomy's cut leaves a one-period-wide window in which a host deschedule "still
fails, wearing the message that blames this kernel", measured only by a lock-holding probe at 0.83
of an interval. This run is the first time either was seen in the wild, and it corrects the first
of those claims: **at these loads the retry budget is not rare.** It fired four times in
forty-five runs, and one of those was the third run of the loop.

The taxonomy red carries its own numbers, which is the 2026-08-15 fix working even as the assertion
fails: run 3 reported `last miss re-armed 55810 counter ticks late against an interval of 100000`.
That is 0.56 of an interval, inside the "slow handler" bucket, and the handler was not slow. It is
the documented window, observed, at a lateness lower than the probe that first measured it.

**Neither is fixable by widening, and the run is evidence for that rather than against it.** The
retry budget's second, implicit claim ("a host that cannot give eight clean windows is pathological
or the handler is slow") is answered properly next door, by the sibling assertion carrying the
taxonomy, which **passed immediately before this one panicked in run 2's log**. And widening the
taxonomy's cut trades the flake for a slow handler going unreported, which BUGS already refuses.
What both need is the instrument milestone 78 built the same day: under `-icount shift=0,sleep=off`
virtual time is a function of instructions executed, so a contended host cannot move it at all.

**And that changes what these two are for, which is the thing to carry forward from this run.**
`script/icount` now asserts **zero missed ticks** on both ISAs, which is a strictly stronger claim
than either assertion above makes and is one a loaded host cannot falsify. It is a separate boot
mode, so `script/test` still carries the wall-clock pair, still fails on them at roughly one run in
six under this load, and no longer learns anything from them that the instrument does not assert
better. That is a disposition worth making deliberately rather than a bound worth widening: the
question for whoever takes it is whether the wall-clock pair keeps a claim of its own on a machine
where `script/icount` is not run, not whether eight attempts should have been sixteen.

### The ninth red is not a timing assertion, and it wants its own lane

Run 31, riscv64, during
`force_kill_tests::destroy_reclaims_a_region_whose_resident_is_blocked_in_recv`:

```
[PANIC] panicked at crates/frames/src/lib.rs:315:9:
double free of frame 0x82a3e000
```

That is `Frames::free`'s deliberate loud failure, and its doc comment is right that both cases it
covers are kernel bugs. Nothing in `notes/` or `design/` records this; a grep for "double free"
finds only the assertion itself. **One occurrence in forty-five loaded runs, and zero in the quiet
run that preceded the loop.** It is recorded in the BUGS section of notes/object-revocation.md,
beside the `DESTROY` path it fired in.

Worth naming because it is the whole point of the milestone: a red run that means something. Eight
of these nine reds are noise the instrument cannot yet remove, and the ninth is a memory-safety bug
in the kernel. A suite that fails for reasons unrelated to the change trains everyone to re-run
rather than to read, and this run is what that costs: the one red worth reading arrived wearing the
same colour as eight that were not.

### Load average did not predict the failures; a second emulator did

The most useful number in the table is the one this instrument added, and it was not the load
average:

| condition | runs | red |
|---|---|---|
| a neighbouring lane's emulator seen during the run | 17 | 6 |
| this run's emulator alone | 28 | 3 (one of which is the double free) |

And the load average separates them not at all. Run 42 **passed** at a peak of 63.0, the highest in
the table; run 11 **failed** at a peak of 33.8, near the lowest. Eight steady spinners raise the
load average a great deal and apparently do not, on their own, reliably deschedule the vCPU across a
250 ms measurement window; another TCG emulator competing for the same cores does.

**The honest caveat, and it cuts against the table above.** A run that dies in 43 seconds gives the
ten-second sampler only four looks, so the neighbour count for exactly the runs that failed fastest
is the least reliable figure here. Runs 2 and 29 are recorded at one and zero, and neither can be
trusted to mean no neighbour was up. Read the split as a lead worth instrumenting properly, not as a
measured ratio.

### What this run proves, and three things it does not

It proves the suite is **not** load-insensitive at the loads the recipe on this page manufactures,
and it names exactly where: two assertions, both ISAs, both awaiting icount rather than a margin.
That is a real answer to milestone 62's acceptance question, and it is a "no".

It does **not** prove the other assertions are correct. Thirty-six green runs are thirty-six draws
from one host at one load band, which is the fifth round's lesson arriving again: an injection that
fires proves only that an assertion can fail, and a loop that passes proves only that it did not
fail here. Milestone 124's lane did 45 loaded full-suite runs without reproducing a fault it was
hunting.

It does **not** establish a rate for the double free. One in forty-five is a sighting, not a
frequency, and the next lane on it should assume the window is narrower than that.

And it does **not** transfer to CI. This is one laptop, one QEMU build, one load shape. GitHub's
runners are a different machine with a different contention profile, which is the same caveat
notes/cpu-models.md attaches to its own matrix.
