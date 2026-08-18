# 78. The load-sensitive assertions, and the three that measure the wrong thing

**Status: BUILT** (2026-08-17). Raised 2026-08-03 after a day in which five distinct assertions
failed on pull requests that changed no executable code, two of them documentation only. Milestone 72
fixed the one that was a real bug. What followed was a family rather than one problem, and it took
five rounds and an instrument.

**What closed it: `script/icount`**, on both ISAs. The two claims this block was last left holding
are asserted there: that the timer fired at the deadline the kernel armed (on riscv64, that SBI was
armed with the `DEADLINE` word rather than with something else that leaves the array looking right),
and that the handler costs fewer than N instructions. A third came free and closed a `BUGS` entry:
**zero missed ticks**, which is only assertable on this instrument, because the miss taxonomy on both
ISAs exists to tell a slow handler from a descheduled emulator and virtual time has no deschedules.
CI runs it beside the bench tripwire. See notes/instruction-clock.md.

**Everything in the evidence table below is closed**, and the table is history rather than a
worklist: the disposition column says where each verdict lives. Read it for the diagnosis it
records, not for work to pick up. The three negative-discrepancy assertions this gate line used to
send a lane at are done.

## The day's evidence, which is history rather than a worklist

**All three negative-discrepancy assertions were fixed, and the line numbers in this table are stale**
(corrected 2026-08-17 by the status-accuracy sweep; notes/load-sensitive-assertions.md:465 had already
named this exact defect, that a reader coming to the block first is sent to three finished sites). The
reaper count is now at `kernel/src/sched.rs:3978`, rescoped to per-`Tid` waits and `used() <= before`.
The address-space frame check is `kernel/src/user/tests.rs:2221`, where the old "-19" survives as a
comment recording the past failure rather than as a live baseline. The frame-hygiene check was
**removed** rather than rescoped. `threads_round_robin` is at `kernel/src/sched.rs:3809` and waits on
the clock. The timer twins were rebuilt against the re-arm law on both ISAs
(`kernel/src/arch/aarch64/timer.rs:382`, `kernel/src/arch/riscv64/timer.rs:423`). The table is left
below as the record of the day that prompted the milestone; read it for the failure shapes, not for
where the code is.

The line numbers are 2026-08-03's and every file has moved since. Find an assertion by its message
text. The disposition column was added 2026-08-17 after this table briefed a lane at three finished
sites: the three negative-discrepancy rows had been settled on 2026-08-03 and the block still asked
for "three small changes with three arguments" in its own gate line, which is the stale pointer §71
exists to catch, sitting in the paragraph a reader meets first.

| assertion | site | what it reported | disposition |
|---|---|---|---|
| reaper count | `sched.rs:2819` | `left: 5, right: 6`, message "finished threads were never reaped" | **rescoped** 2026-08-03, per-`Tid` `thread_present` waits |
| frame hygiene | `user/live_swap_tests.rs:230` | `before >= free_frames()`, margin measured at 2 frames | **removed** 2026-08-03 (#46), the narrower claim was already beside it |
| address-space frames | `user/tests.rs:1746` | "**-52** frames did not come back", and separately "-19" | **rescoped** 2026-08-03, same two changes |
| timer drift | `arch/riscv64/timer.rs:254` | ticks within one period either way | **re-aimed** at the re-arm grid law, both ISAs |
| placement probe | `smp.rs:343` | 60 s wait for work to run where it was placed | **rescoped** 2026-08-04 after the first verdict ("leave it alone") was refuted |
| handler latency | `arch/aarch64/timer.rs:323` | `left: 3, right: 2`, missed ticks rose during a quiet window | **taxonomy**, aarch64 2026-08-15 and riscv64 2026-08-16; the instruction-count claim still wants icount |
| round-robin fairness | `sched.rs:2709` | `thread {i} never ran`, one thread of several had not been scheduled | **rescoped** 2026-08-03, waits on the property |

**Every argument is in notes/load-sensitive-assertions.md**, one section per assertion, plus the
four rounds of sites found by reading rather than by waiting for a red run. That note is the record;
this block is the spec and the day's evidence.

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

*(The diagnosis, kept in the present tense it was written in. All seven assertions it sorts have since
been dispositioned; see the column above.)*

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

## What was left, and what closed it: the software timer grid, on the icount instrument

*(Written when it was the remaining work; kept in its own tense, with the outcome at the end of the
section. `script/icount` is the answer, notes/instruction-clock.md is its note, and the sixth round
in notes/load-sensitive-assertions.md is the record of building it.)*


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

## The outcome, 2026-08-17

Built, on both ISAs, as `script/icount`: a boot mode under `-icount shift=0,sleep=off` where virtual
time advances by one nanosecond per guest instruction retired and by nothing the host does. It
asserts the two claims above and one that came free.

| claim | aarch64 | riscv64 | bound |
|---|---|---|---|
| deadline to the handler observing it | 1,008 instructions | 300-400 | 2,000 / 1,500 |
| deadline to the next one armed (the whole handler) | 1,056 | 800-900 | 2,500 / 2,500 |
| ticks missed over 64 sampled | 0 | 0 | 0 |

The aarch64 numbers are the same on all 64 ticks, minimum equal to maximum, which is the instrument
proving itself rather than being argued for.

**The riscv64 claim was proved by injection**, twice, with the residual this block named built
rather than argued about: an implementation that keeps `DEADLINE` on the grid and arms SBI from
`now()` sends the arrival latency to 420,400 instructions against a bound of 1,500, and one that
arms it a *fixed* quarter period off the grid (no drift, no misses, the delivered rate still exactly
100 Hz) reads 2,500,400 on every tick.

**And the prediction attached to those injections was wrong, which is the more useful finding.** The
existing suite catches both. What it does not do is say what is wrong: the first fails as "the
handler itself is slow, which is this kernel's bug" on the assertion that broke #204, #210 and #215,
and the second as "either the host is too contended to observe the grid, or the handler is slower
than a whole tick period". Both hand a reader the "known or real" judgement this block's own cost
line is about. The instrument's value is diagnostic certainty rather than detection, and its message
names the defect.

**Three things it deliberately did not do.** It is not on the test path, and
notes/instruction-clock.md carries the measured reason (not speed: one shared virtual clock, and
clock-bound waits costing instructions). It instruments the timer only, since that is where both
claims are. And the scope note's remaining sites are still unaudited, for the sixth round running.
