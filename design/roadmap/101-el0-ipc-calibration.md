# 101. The L4 calibration, read from the IPC number that pays for the trap

**Status: NOT-STARTED.** Raised 2026-08-04 as "build the EL0-to-EL0 IPC benchmark nobody owns", and
that is not what the tree says. **The benchmark exists and has been published for days.** The
milestone survives because the comparison built on it did not follow, and the corrected comparison
is considerably worse for us than the one on the page.

**Gate: NONE.** The paragraph that overstates the L4 comparison by a factor of three can be
corrected today, and the block says it should not wait for a board. Reading cycles from a PMU
rather than estimating them rides milestone 16's silicon, and folding the whole thing into
milestone 25 is the alternative the block names.

**What the source actually says.** notes/benchmarks.md:66 names a true EL0-to-EL0 benchmark as "the
right follow-up" and says it "needs one `CNTKCTL_EL1` bit so EL0 can read the counter". Both halves
are stale. The bit was opened by milestone 19e (`kernel/src/arch/aarch64/timer.rs:134`, with the
riscv twin `scounteren.TM` in its timer init), and `ipc_rtt_el0` has been in `kernel/src/bench.rs`
since the EL0 primitive suite landed: two EL0 processes, two endpoints, a client self-timing
`SEND`-then-`RECV` against a server process. The same note reports it, 130 lines below the sentence
that asks for it.

**The numbers, from the note.**

| what | ns/iter (HVF, debug) | what it includes |
|---|---|---|
| `ipc_rtt` (kernel-side, milestone 21) | ~951 | the rendezvous, no trap |
| `ipc_rtt_el0` (EL0, the primitive suite) | ~2272 | two rendezvous and four `svc`s |

The note already draws the right conclusion from those two rows: the ~1.3 µs gap "is exactly the
trap cost of the four `svc`s a real round trip pays, which is the reason the EL0 numbers, not the
kernel-side ones, are what compare to lmbench."

**And then the L4 section compares the kernel-side one anyway.** It converts ~705 ns at ~3.2 GHz to
~2,200 cycles and reports us "4 to 7 times heavier" than an L4-lineage fastpath's 300 to 600. Run
the same arithmetic on the number that includes the trap: ~2272 ns at ~3.2 GHz is roughly **7,300
cycles**, which is **12 to 24 times** an L4 fastpath, not 4 to 7. The two nanosecond figures come
from different runs (the 705 predates the suite; the note's own later table gives 951 for the same
kernel-side benchmark), so the ratio wants one clean run rather than a subtraction across sessions,
and it is not going to move the conclusion by a factor of three.

**This milestone is that correction, done properly.** It is a measurement and a paragraph, not a new
harness:

1. One release-build run of both IPC benchmarks on one machine, because L4's published numbers are
   release builds and the note flags every EL0 figure as debug.
2. Cycles from the PMU rather than from a nanosecond-times-clock estimate, which means it rides
   milestone 16's real silicon: HVF passes only the architected virtual counter through
   (notes/benchmarks.md's own caveat), so a cycle count under the hypervisor is arithmetic, not a
   measurement.
3. The calibration paragraph rewritten against the EL0 row, and the stale follow-up sentence
   removed so the note stops asking for something it already contains.

**What it costs is the headline**, and that is the reason to do it. "Our ~705 ns round trip sounds
like that club" is the sentence a reader quotes, and it is measured on the plane that skips the
boundary L4 measures across. An honest loss recorded plainly is worth more than an overclaimed win
(CLAUDE.md), and this is the tree's largest outstanding instance of that trade.

## Scope note

**Do not delete or demote the kernel-side numbers.** They are the gating instrument: icount
regression tripwires run against them, and the note explains why a kernel-internal path length is
the right thing for a gate and the wrong thing for a comparison. Both planes stay; only the
comparison moves.

**A candidate to fold into milestone 25 (cross-OS comparison) rather than run alone.** 25 owns the
lmbench and `sel4bench` table and the release-build discipline this needs, and it already carries
the deferred `sel4bench` leg waiting on hardware. What would decide it: if 25 is scheduled before
milestone 16's silicon lands, this is one row of 25's work and should not be a separate lane. If it
is not, correcting a paragraph that overstates the result by a factor of three should not wait for
a board.
