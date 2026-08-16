# 72. `time` needs no clock: duration is ambient, wall-clock identity is authority

**Status: DECIDED.** calef, 2026-08-16: **counter-only `time`**, adopting the objection the lane
that built it raised against its own specification. The boundary this draws is the reusable part:
**wall-clock identity is authority and a capability gates it (§43, `date`'s manifest); a duration
is ambient, because the ABI already opened the counter to EL0.** `user_rt::monotonic_nanos` says
so in its own doc comment, one layer below where this argument was being had.

**A capability that gates nothing is worse than no capability**, which is the reason this was
worth reopening a shipped thing for. In a system whose claim is that capabilities describe real
authority, a decorative one costs a reader who audits the confinement story by counting them.

**The old design's best argument survives intact rather than losing**, and that is what made the
decision easy in the end. `swish`'s block held that a `time` which cannot measure must not run
the command unmeasured, because that is §42's silent degradation with a stopwatch on it. Correct,
and unaffected: with an ambient counter `time` can always measure, so the two clock refusals
(`NoClock`, `UnknownClock`) became unreachable rather than tolerated. `NothingToTime` stays, being
a usage error. Counter-only is also strictly *more* correct: a clock stepped mid-command used to
make the answer a difference of readings on two different clocks, which the shell detected and
disclaimed; a monotonic counter cannot be stepped, so both the failure and its disclaimer are
gone.

**If the counter is ever gated** (seL4's time-protection work is the precedent, since timing is a
side channel), `time` inherits whatever the ABI does. The tool follows the ABI rather than
pretending to lead it.

**What.** Milestone 86 shipped `time` reading the shell's clock capability, as its block specified.
The lane then argued in a BUGS entry that a duration does not need a clock at all: wall clock is
`offset + counter`, the offset cancels across a command, and the counter is ambient
(`user_rt::monotonic_nanos`, two register reads, no syscall), so `end - start` reduces to a counter
difference any process could take.

**The trade.** A counter-only `time` needs no capability, cannot be refused, and is *immune* to a
mid-command clock step. The clock version buys a wall-clock number and the ability to notice a step
(the shell reads `clock_proto`'s generation at both ends), and costs refusing to measure on a
machine with no believable clock.

**Recommendation.** Worth reopening. The lane implemented the block rather than diverging
unilaterally, which was right, but its objection is the stronger argument on the merits.

**Blocked.** Nothing shipped depends on it. Revisiting is cheap: read `monotonic_nanos` at both
ends, delete two `Untimed` arms, and the clock wiring below it stops being needed.

## What it exposed one level up

Removing `time`'s clock reads left the shell's clock *grant* unread: init still endows a clock
page the shell no longer opens. That is this decision's own principle one level out, a capability
held for nothing, and it is deliberately not fixed here because the grant is init's wiring rather
than `time`'s, and a shell may want a wall clock again (a prompt timestamp, a `date` builtin).
Whoever revisits the shell's endowment should either find it a reader or stop granting it.
