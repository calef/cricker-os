# 71. The thread-start fault: a user thread dispatched with `sepc` = 0

**Status: BUILT (2026-08-03), both ISAs.** Found, proved on the machine, and fixed. It was **frame placement**,
which is where this entry said to look first, and the mechanism is exact rather than plausible:
`current_sp()` is a real call at opt-level 0, so it returned `sp - 16` and put the frame at
`sp - 304` while `trap.s` builds an S-mode trap frame at `sp - 288`. Sixteen bytes apart, so the user
frame's `x[2]` sat on the trap frame's `x[0]` slot, which `trap_entry` writes as a literal zero. That
is why `user sp` read `0x0000000000000000` and not garbage. The frame now goes at
`top - size_of::<TrapFrame>()` on both ISAs, with the shallow TCB path handled by a reservation in
`user_entry_trampoline` rather than a moving target. See notes/riscv-port.md.

**The scope note below is upheld and one sentence of it is now wrong.** The fault does have a silent
face: the `sepc == 0` guard fires only when `t5` happened to be 0, and otherwise the thread `sret`s
to a garbage PC and dies quietly, so a lost-wakeup hang with no guard message really can be this bug.
But the specific hang on `reclaim_frees_a_started_then_exited_childs_regions` is **not**: that test's
child takes the TCB path, which runs with interrupts masked and cannot take the clobber, and the hang
reproduces with this fix in the tree (one run in four under host load, a recipe that did not exist
before). It is tracked as its own open item in notes/scheduler.md.

## The evidence, which we now have because we instrumented instead of chasing

The fault was first seen on 2026-08-02 as a *hang*: a user thread whose first instruction fetch
faulted, so whatever it was meant to serve never answered, every waiter blocked, and the run died 60
seconds later in the lost-wakeup watchdog, arbitrarily far from the cause. It never reproduced
locally, in nine full runs across both ISAs. Rather than keep hunting it, `enter_user` got a guard
that converts the rare hang into a loud failure carrying its own evidence.

That guard fired on CI, on the milestone-70 branch:

```text
[PANIC] panicked at kernel/src/arch/riscv64/exceptions.rs:155:5:
thread 25769803877 on core 1 was dispatched to U-mode with sepc = 0 (user sp 0x0000000000000000).
Its context was never built, or was built and not seen by this core.
```

Three details do the work:

- **`user sp` is zero too.** This is not a bad entry point, it is an entire trap frame reading as
  zeros. Whatever `enter_user` is looking at, nobody wrote it.
- **Core 1**, a secondary rather than the boot core.
- The test was `a_user_program_reaches_el0_and_returns_twice`, which is one of the simplest
  user-entry paths in the suite.

## Where to look first, and why it is probably not the obvious thing

The obvious reading of "built but not seen by this core" is a missing release/acquire pair between
the core that builds a context and the core that dispatches it (DECISIONS rule 4: assume weak
ordering). **That is probably not it**, and the reason is worth knowing before anyone spends a day
on barriers: the frame is built by the thread ON ITS OWN kernel stack, in `kernel/src/user.rs`
around the `current_kernel_stack_top()` call, and `enter_user` runs a few lines later on the same
core with no yield in between.

The likelier suspect is **where the frame is placed on riscv64**, which is already known to be
delicate and is already commented as such:

```rust
#[cfg(target_arch = "aarch64")]
let slot = top - size_of::<TrapFrame>() as u64;
#[cfg(target_arch = "riscv64")]
let slot = (crate::arch::current_sp().min(top) - size_of::<TrapFrame>() as u64) & !15;
```

aarch64 uses a **fixed** offset from the stack top. riscv64 computes the slot from the **live
`sp`**, because its TCB entry path is shallow enough that a frame at the top would overlap and
corrupt this function's own stack. The existing comment says exactly what that failure looks like:
"sending the sret to a garbage sepc". `sscratch` is then armed to `frame + size` so re-entries
rebuild at the same address.

So the question to answer first is **whether the address `frame.write` targets is always the address
`enter_user` and the trap path later read**, on every path that reaches user entry and at every
stack depth. A slot that moves with call depth is a slot that can be written in one place and read
in another, and an all-zero frame is what reading the wrong place looks like.

Every failure so far has been on riscv64. That is consistent with the placement hypothesis and
inconsistent with a generic ordering bug, which would be expected to show on aarch64 too.

## What is already in place

- The `sepc == 0` guard in `kernel/src/arch/riscv64/exceptions.rs` and its `elr == 0` twin in the
  aarch64 file. Keep both; they are how this became findable.
- The frame's writability assertion, one check per exec.
- `script/cpu-matrix` uploads per-model logs as a CI artifact on failure, which is how the evidence
  above was recovered.

## Scope note

Do not "fix" this by widening a deadline or re-running. Three CI failures on 2026-08-03 traced to
three different tests on three different CPU models, and only one of them announced itself as this
fault; the other two were a frame-leak wait and the lost-wakeup watchdog, which are what this bug
looks like when the guard does not happen to catch it first.
