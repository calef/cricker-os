# 5. The GIC and the timer: the kernel is preemptible

**Status: BUILT.**

Backfilled 2026-08-03 from history (milestone 76). Built in `809d856` (2026-07-13): a 100 Hz
tick, and a timer interrupt that can land between any two instructions, which is the moment the
locking discipline from DECISIONS §9 stopped being a hypothesis.

The commit is one of the best-documented in the tree and three of its findings still matter:

- GIC priorities run backwards and PMR is set before the interface is enabled, because the other
  order leaves a window running on whatever the firmware left in PMR.
- The bug that was shipped and then measured: re-arming a one-shot timer with the relative TVAL
  makes every tick start late and the lateness is never recovered. Measured at a configured
  100 Hz: 17 ticks in 250 ms instead of 25. The fix is the absolute CVAL deadline on a fixed grid,
  plus a safety valve that re-anchors if the next deadline is already in the past.
- The test the milestone was built for, `holding_a_lock_masks_the_timer`, and its mirror,
  `a_long_critical_section_costs_a_tick`, which asserts the cost of the deadlock prevention is
  real, because if it ever stopped being real the masking would have stopped too.

All 52 kernel tests ran preemptibly from this commit on.
