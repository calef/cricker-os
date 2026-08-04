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
  a re-aimed one.)*
- **Scope was five sites, not 39.** The roadmap's scope note counts 39 sites in 7 files matching
  the shape (`wait_for`, or assertions against `free_frames`, `thread_count`, `used()`). The
  other 34 were not audited here; the diagnostic above is the checklist for reading any of them.
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

- design/roadmap.md milestone 78 (the spec and the day's evidence)
- notes/cpu-models.md BUGS (the load-sensitivity evidence, including the control model failing)
- notes/live-replacement.md BUGS (the completed frame-hygiene analysis, the template)
- notes/riscv-parity-scope.md (the shape named: a wait written against something wider than the
  property)
- notes/benchmarks.md (the two instruments, and why icount cannot host cross-core tests)
