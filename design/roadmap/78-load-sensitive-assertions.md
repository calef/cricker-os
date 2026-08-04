# 78. The load-sensitive assertions, and the three that measure the wrong thing

**Status: PARTIAL.** Raised 2026-08-03 after a day in which five distinct assertions failed on
pull requests that changed no executable code, two of them documentation only. Milestone 72 fixed the
one that was a real bug. What is left is a family, and it is not one problem.

## The day's evidence

| assertion | site | what it reported |
|---|---|---|
| reaper count | `sched.rs:2819` | `left: 5, right: 6`, message "finished threads were never reaped" |
| frame hygiene | `user/live_swap_tests.rs:230` | `before >= free_frames()`, margin measured at 2 frames |
| address-space frames | `user/tests.rs:1746` | "**-52** frames did not come back", and separately "-19" |
| timer drift | `arch/riscv64/timer.rs:254` | ticks within one period either way |
| placement probe | `smp.rs:343` | 60 s wait for work to run where it was placed |
| handler latency | `arch/aarch64/timer.rs:323` | `left: 3, right: 2`, missed ticks rose during a quiet window |
| round-robin fairness | `sched.rs:2709` | `thread {i} never ran`, one thread of several had not been scheduled |

`notes/cpu-models.md` already records three of these as load-sensitive with the evidence that settles
it, including the case where the control model `rv64` failed too, which is what proves the failures
are not model-specific.

**A seventh, found by a lane on 2026-08-03 and reported rather than absorbed.** `sched.rs:2709`,
`threads_round_robin`, asserting every spawned thread ran at least once. It failed on one
`script/gates` run and passed on the immediate re-run, with two full `script/test` runs either side of
it green. The lane judged it pre-existing on grounds worth repeating, because they are the right shape
for this call: its own code runs before the scheduler exists, does three register reads and an
`ecall`, and holds a leaf lock nothing else takes. It could not have starved a thread.

**One of them reproduces off CI.** On 2026-08-03 a local `script/test` on an aarch64 dev machine hit
`user/tests.rs:1746` with "**-19** frames did not come back", the same value the milestone-71 lane saw.
That matters because it removes the easy explanation: this family is not an artefact of GitHub's
runners, and a quiet machine is not a defence against it.

## The split that makes this two problems, not one

**Two are genuinely timing.** Timer drift and the placement probe measure how fast something happened,
and a contended runner is slower than a quiet one. Their margins are a judgement about how slow is
acceptable, and widening them trades sensitivity for noise honestly.

**Three are not, and this is the finding.** The reaper count, the frame hygiene check and the
address-space frame count all report a **negative** discrepancy: fewer threads than the baseline, more
free frames than at the start, minus fifty-two frames. **A slow machine does not produce a negative
count.** These are not timeouts at all. They are waits written against something wider than the
property under test, so state arriving from *outside* the measured window trips them: a teardown from
an earlier test completing late, or a thread the baseline counted exiting during the batch.

The 72 lane named this shape, and `notes/riscv-parity-scope.md` apparently records it twice already.
Milestone 72's own postscript is the clearest instance: removing a destructive probe changed which
threads were alive at a later test's baseline, and the count moved.

## The instrument this project already owns

**The test runner passes no `-icount`. Only the bench does.** So in `script/test`, guest `CNTVCT_EL0`
follows host time, and a QEMU process the host descheduled makes the guest observe a missed deadline
that says nothing about our handler. The two timing assertions cannot distinguish "our code is slow"
from "the emulator was not running", and no margin fixes that: widening changes how often you notice,
not what is being measured.

Under `-icount shift=0,sleep=off`, which the bench already uses, **virtual time is a deterministic
function of instructions executed**, so host scheduling cannot advance it at all. That removes the
confound rather than tolerating it.

So the likely answer for the two genuinely-timing assertions is **not a wider bound but a different
instrument**: move the property to the icount tripwire, where "the handler takes fewer than N
instructions" is a claim a contended runner cannot falsify. That would make them **stronger** than
they are today, not weaker, which is the test of whether this milestone did its job.

Worth checking before committing to it: icount is slower and changes what the suite measures, so this
may be right for the timer assertions and wrong for the placement probe, whose subject is genuinely
cross-core wall clock. Decide per assertion, as below.

## What the fix is not

**Not wider margins.** Widening a bound that fires on a negative discrepancy hides the defect rather
than fixing it, and this project already carries a scar for exactly that shape: DECISIONS §61 records
three lints dropped because they were measuring the wrong thing, and the same reasoning applies to an
assertion.

**Not deleting the assertion**, either. Milestone 72's lane declined to delete
`live_swap_tests.rs:230` precisely because it could not make it fire and would not remove a check it
did not understand. That was the right call and it is the standard here.

## What the fix probably is

For each of the three, decide **what property the test is actually responsible for** and assert that
instead. The reaper test wants "the frames this batch allocated came back", not "the global thread
count returned to a number another test also influences". A per-test accounting scoped to the objects
that test created is immune to a neighbour's late teardown by construction, where a global count can
never be.

That is a per-assertion decision, so the deliverable is three small changes with three arguments, not
a framework.

## Scope note

**39 sites across 7 files** match the shape (`wait_for`, or an assertion against `free_frames`,
`thread_count` or `used()`). Do not touch all 39. The five with evidence are the milestone; the rest
are a list to check against the same question and mostly to leave alone.

The honest cost of leaving this open, and the reason it is worth doing: every red check in this
repository currently needs a human to decide "known or real", and on 2026-08-03 that judgement was
made at least six times and got the wrong answer twice.

## Postscript, 2026-08-03: the frame-hygiene assertion is gone

Removed the same day this milestone was raised (#46), after it failed the cpu matrix twice more on
`main`, once on `rv64`, the control model, and once on a Dependabot PR that touched only workflow
files. That is a deletion, and the paragraph above says deletion is not the fix, so the difference
is worth stating plainly. The 72 lane rightly declined to delete a check it could not explain; by
removal time the explanation was complete (the BUGS section of notes/live-replacement.md: only
frames arriving from outside the run could trip it, with a measured margin of two frames). And the
assertion this milestone asks for, one scoped to the property the test is responsible for, was
already standing twelve lines above it: the budget reclaim must succeed and must return exactly
`SWAPPER_BUDGET_PAGES`. The global count added no coverage on top of that, only the exposure to
neighbours. One of the five is done; the reaper count, the address-space frames and the two timing
assertions remain, and the status stays NOT-STARTED for them.

*(That last sentence has been overtaken. The verdicts landed and are recorded per assertion in
notes/load-sensitive-assertions.md; the status is PARTIAL, and the section below is what is left.)*

## What is left: the software timer grid, on the icount instrument

The timer twins were rebuilt rather than widened, and the rebuild is the model this milestone asked
for: both tests now assert the **law** directly, that over a window in which `MISSED_TICKS` did not
move, the deadline advanced by exactly one interval per delivered tick. The deadline is read back
out of the machine on both ISAs, `CNTV_CVAL_EL0` on aarch64 and the software `DEADLINE` array on
riscv64, which is kept because SBI's `set_timer` is write-only, with a `deadline()` accessor added
beside `missed_ticks()` on each. The defect this catches, re-arming from `now()` inside the handler,
fails on the first tick. A descheduled emulator cannot fail it: a deschedule long enough to slip the
grid increments `MISSED_TICKS`, which is the re-anchor safety valve working, and the window is
retried.

**One claim moved out of scope rather than being weakened, and it is riscv64's alone.** Nothing
proves that SBI actually fired at the software grid's deadlines. `DEADLINE` is our own array; on
aarch64 the equivalent value is in a register the hardware itself consults, so the readback is
evidence and on riscv64 it is bookkeeping. The residual gap is an implementation that maintains
`DEADLINE` correctly and arms SBI with something else, and **no wall-clock margin could distinguish
that from load either**, which is the same reason the rest of this milestone exists.

The instrument is the one this project already owns. Under `-icount shift=0,sleep=off`, which
`script/bench` already uses, virtual time is a deterministic function of instructions executed, so
host scheduling cannot advance it and "the interrupt arrived at the instruction the deadline named"
becomes a claim a contended runner cannot falsify. Recommended by the lane that rebuilt the twins,
not built by it. Cost: it belongs in the bench harness rather than the test suite, because
`script/test` passes no `-icount` and adding it there would change what the whole suite measures.

**And the placement probe stays where it is**, checked against the same question and left alone
deliberately. Its wait is on exactly the property under test, its failure direction is purely
positive, and moving it to the icount instrument is not an option even in principle: the icount
bench boots `-smp 1` *because* a shared virtual clock makes multi-hart timing fictional, and a
cross-core delivery test cannot run on a one-core instrument.
