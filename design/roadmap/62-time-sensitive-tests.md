# 62. Tests that assert on time: make a red run mean something

**Status: NOT-STARTED.** Raised 2026-08-01, from evidence rather than from taste.

## The problem

A population of tests assert on **elapsed time or on a fixed number of yields**. `sched.rs` alone
holds about nineteen `for _ in 0..N { yield_now() }` spins, and the shape is always the same: give
the scheduler N chances, then assert something happened. `threads_round_robin` gives twenty yields
and asserts every thread ran at least once. `ticks_arrive_at_the_configured_rate` and the riscv
timer-drift assertion compare guest ticks against elapsed counter time.

None of them is wrong about what it wants to prove. All of them fail when the host is busy, because
a yield is not a guarantee and the guest's clock is the host's clock.

## Why this is worth a milestone rather than tolerance

**It makes a real regression invisible.** On 2026-08-01 it cost the integrator three separate
diagnosis cycles, and two of those ended in the wrong conclusion before being re-run. The credentials
lane hit three flakes, the xattr lane two, the CPU-matrix lane two, and the integrator hit three more
in different tests each time. A suite that fails for reasons unrelated to the change trains everyone
to re-run rather than to read, which is the exact habit that lets a genuine failure through.

**Milestone 59 multiplies it fivefold.** The CPU matrix runs the same suite five times, so every
timing test now has five chances per run to be unlucky, on a shared CI runner nobody controls.

**And the honest diagnostic we rely on is expensive.** The current rule is "a green run under load is
conclusive, a red one is not, so re-run quiet." That works, and it costs a full suite run every time,
and it depends on a human remembering to apply it.

## What the fix probably is, per class

- **The bounded spins** are the easy majority. Waiting for a condition with a deadline is a different
  thing from taking N turns: the test wants "eventually", not "within twenty yields". An
  event-driven wait, or a bound expressed in **guest ticks** rather than host-scheduler turns, makes
  them insensitive to what else the machine is doing.
- **The genuinely temporal tests** cannot be made deterministic and should not pretend to be.
  `ticks_arrive_at_the_configured_rate` is *about* the clock. These want an explicit, stated
  tolerance and a recorded retry budget, so a flake is a documented cost rather than a surprise.
- **A third class may want to move off the emulator entirely.** Scheduling policy is pure logic and
  some of it could be host-tested against a simulated clock, which is where this project already puts
  logic it wants to check in milliseconds.

## BUGS

- **Fixing this cannot be verified by running the suite once.** A flake that fires one run in six is
  indistinguishable from a fixed one until you have run it many times, so the acceptance evidence is
  a repeat count, not a green run.
- **Deleting the timing assertions would be worse than the flakes.** `ticks_arrive_at_the_configured
  rate` is the test that catches re-arming the timer from `now()` inside the handler, which is a real
  bug this project has a comment about. The goal is tests that fail only when something is wrong, not
  fewer tests.

**Effort: not estimated**, and deliberately: the count is known (~19 spins plus a handful of clock
assertions) but how many are mechanical and how many need a rethink is not.
