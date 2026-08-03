# 72. A lost wakeup that a hundred leaked threads may be causing

**Status: BUILT** (2026-08-03). The title is the hypothesis this milestone started with, and it was
wrong, which is why it is kept. The hang was **one line of test code**. It was not the leaked
threads, and it was not RISC-V. Full account, evidence and method in notes/scheduler.md under
"CLOSED: the lost wakeup"; this entry is what changed and what is left.

## What it was

`user::tests::reclaim_frees_a_started_then_exited_childs_regions` opened by probing
`reclaim_region(tcb_region).is_err()` on its own child's TCB region, over a comment reading "the
refusal leaves the region untouched". DECISIONS §16 was later amended so that a refusal is **not**
passive: it arms the kill on every live thread in the region, so an owner's retry can tear a runaway
down (§24's `^C` escalation needs that). The comment kept compiling; the child did not survive. The
scheduler converts a killed thread to a corpse at its next preemption, so whenever the timer beat the
child's nine instructions, the child was reaped **without ever sending**, the test's `ipc_recv` never
returned, and every core fell to idle: the 60 s lost-wakeup heartbeat, exactly as reported.

The fix is deleting the probe. The refusal's own behaviour is proved by
`force_kill_tests::destroy_force_kills_a_runaway_and_reclaims_its_region`, which points the
destructive call at a runaway that is meant to die, the only subject it can honestly be pointed at.
`reclaim_region` now carries a `BUGS` section saying `Err` is destructive, where a caller meets it.

## How a one-in-four race was turned into a proof

Widening the window rather than waiting for it, the method milestone 71 used on the frame fault: a
call-free three-instruction delay loop in front of `REPORT_STUB` guarantees a preemption before the
`SEND`. With the probe present that hangs the watchdog on the first run and every run; with the probe
gone the same widened child passes. The forced dump matched all four wild occurrences exactly (101
threads, 109 endpoints, every thread `wake_pending=false on_cpu=false`, all four inboxes empty).

## The parity answer is a control, not an argument (§19)

Every wild occurrence was riscv64, and **none of that is a RISC-V property**. The code is portable
`sched.rs`. Running the widened window on aarch64 hangs it identically on the first run, which is the
control that rules the ISA out; the riscv64 leg simply lost the race more often under TCG. A defect
that presents on one ISA is not thereby an ISA defect, and the cheap way to find out is to widen the
window on the other one.

## What is left, and it is a separate milestone

The **101 threads and 109 endpoints** were this milestone's stated lead, and they are not this bug:
blocked threads add no scheduling load, and 109 endpoints is a fifth of `MAX_ENDPOINTS`. The suite
accumulates them equally on both ISAs. But 101 of `MAX_THREADS = 128` is 79% of the way to a hard
`create_tcb` failure, and `thread_leak_police` polices only **runnable** leaks, so a blocked leak is
invisible to it by construction. Nothing would warn before the suite hit the wall. That wants its own
entry, with the evidence recorded in notes/scheduler.md.
