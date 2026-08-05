# 6. Threads, the context switch, and preemption

**Status: BUILT.**

Backfilled 2026-08-03 from history (milestone 76). Built in `782d4bb` (2026-07-14): two threads
that never yield, 48 preemptions in half a second, "neither asked to be interrupted; both were."
The argument from DECISIONS §5 (preemptive threads with real stacks), executable.

What the commit records that still shapes the kernel:

- A suspended thread's entire saved CPU state is eight bytes, a stack pointer, because everything
  else is on the stack it points at. The context switch is fifteen instructions ending in a `ret`
  that returns somewhere else.
- A thread that has never run needs no special start path: a forged switch frame makes the same
  `ret` that resumes a thread also start one. Milestone 7a later pulled the identical trick to
  enter EL0 ("a fake way back" through `eret`).
- The trampoline must unmask interrupts by hand, because a brand-new thread has no saved SPSR;
  miss that one instruction and the first spawned thread can never be preempted, "a cooperative
  scheduler with extra steps."
- The reaper test failed with exactly two leaked frames, which were page tables, which exposed
  that stack VAs were bump-allocated and never reused. Fixed with a free list, and a test asserts
  a second batch of eight threads costs zero additional frames.
