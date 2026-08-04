# 72. Whose clock does `time` need? (the design the lane argued against)

**Status: PROPOSED.** (raised 2026-08-04; waiting on Chris. Milestone 86 shipped the other answer,
so this reopens a built thing rather than blocking an unbuilt one.)

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
