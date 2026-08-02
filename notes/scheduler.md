# The scheduler: per-core run queues, two-choice placement, message-shaped stealing

This is the working note for DECISIONS §28 (SMP placement) as built. The decision record is the
authority on *why*; this is *how it fits together* and the caveats worth rereading.

## The shape

Each core owns one run queue, single-owner, touched only by that core with interrupts masked
(`cpu::PerCpu::with_runq`). No core takes another core's run-queue lock, ever. The only cross-core
structure is a per-core migration **inbox** (a real lock) plus the reschedule SGI that pokes a core
to drain it. That is the whole concurrency surface: run queues are private, the inbox is the one
shared thing, and it is small.

Two relaxed atomics mirror a core's load so another core can read it without touching the queue:
`runq_len` (updated in `with_runq`) and `inbox_len` (updated under the inbox lock). `runnable()` is
their sum. Stale reads are fine and expected; every placement and steal decision tolerates being a
beat out of date (the gossip lesson).

## Placement: the power of two choices

`spawn` -> `pick_spawn_target`: a per-core xorshift PRNG picks two online cores, the lighter by
`runnable()` wins, the thread is carried there by `spawn_on` (local push, or remote via the inbox +
SGI). Near-optimal balancing that reads at most two remote counters no matter the core count. The
PRNG is seeded per core from a fixed constant, so a given boot makes the same choices and the
icount benches stay reproducible. One online core: a no-op.

## Stealing: pull, by message

An idle core's `run_idle` calls `try_initiate_steal`: pick the most-loaded *other* core by run-queue
depth alone (never its inbox, which is work already in flight to it), CAS a one-slot steal request,
and poke it with the reschedule SGI. The victim's `serve_steal_request`, at its next scheduler
entry, hands back one queued thread through the requester's inbox. Pull beats push under
uncertainty, and no shared run-queue lock ever appears. Cost: a steal lands a tick late, bounded.

## Wakes: local for a rendezvous, load-aware for a device interrupt (§28.2, as amended)

An IPC rendezvous wakes its partner on the **waker's** core: the message is in registers, the cache
is warm, and a serial pipeline (net_stack<->std) stays co-located and fast. `wake` does this, and every
IPC path, supervision, and revocation uses it.

A **device interrupt** is different: it carries no locality, and pinning the woken driver to the
IRQ-handling core re-concentrates a pipeline or drops it on a busy core. So `irq_notify` wakes
**load-aware** through `wake_load_aware` / `pick_wake_target`: the least-loaded core, ties won by
the current core so a driver taking a completion interrupt every request (the block server at a
RedoxFS mount) is not migrated each time. The device-line **affinity** that spreads which core takes
each IRQ in the first place is the companion mechanism, in notes/interrupts.md.

## The costs migration made real

Turning on any migration at all strips the accidental cover off same-core assumptions. Two bit us
and are now fixed and tested:

- **RISC-V `tp` (the per-hart pointer) is thread-frame state.** A kernel thread preempted on one
  hart and resumed on another used to come back reading the wrong hart's per-CPU block. Fixed in
  `arch/riscv64/trap.s`; the full story and the regression test are in notes/riscv-port.md. aarch64
  is immune (its pointer is a system register the frame never carries). This is the concrete face of
  rule 4 (assume weak ordering) and rule 1 (arch state lives in arch).

- **The hang watchdog counts progress, not test starts.** A slow-but-live workload (std_net spends
  about 300 s in net_stack's userspace smoltcp poll, CPU-bound, no wakes or output for stretches over a
  minute) must not read as a deadlock. The watchdog credits a completed wake, a line of output, OR
  any core running a non-idle thread; only a real lost wakeup, every thread blocked and every core on
  its idle thread, stalls it. See `kernel/src/testing.rs`.

- **And because that alone traded a flake for a silent hang, there is also a per-test wall-clock
  ceiling.** See the section below: the progress heartbeat is blind to a livelock that keeps doing
  IPC, which is a real failure we hit, not a theoretical one.

## The two hang watchdogs, and what each one cannot see

The harness asks two independent questions, and either failing fails the run. They exist because there
are two ways a test never finishes, and no single instrument sees both.

| | **No-progress heartbeat** | **Per-test wall-clock ceiling** |
|---|---|---|
| Question | Is anything happening at all? | Has this test taken longer than allowed? |
| Catches | Deadlock, lost wakeup | Any non-terminating test, livelock included |
| Window | ~60 s of total silence | The test's budget (90 s default) |
| Blind to | Any loop that keeps doing IPC | Nothing that fails to terminate, but slow to react |
| Scope | Anywhere, including before tests start | Only while a test is running |

**Why the ceiling had to be added.** The heartbeat credits a completed rendezvous as progress. The
RedoxFS repeat-write livelock spins in an allocator commit *while still serving blk IPC*, so every
rendezvous reset the heartbeat: a failure that had been a loud 60 s trip became an infinite silent
hang at about 400% CPU with no watchdog fire. A livelock that makes progress is indistinguishable from
healthy work to a progress-only instrument. Turning a loud failure into a silent one is worse than the
flake the heartbeat fixed, so both mechanisms are live now.

**Why budgets are per test.** std_net honestly runs 300 to 344 s, so one global ceiling would sit near
700 s and let a two-second unit test spin for eleven minutes before failing. The default is a tight
90 s; a test that is honestly slower declares its cost in `SLOW_TESTS` in `testing.rs`, with the
reason. Keep entries near 2x measured, so host load does not make them flaky.

**The honest limit.** Neither mechanism can tell a livelock from slow-but-correct work while it is
running. Only the budget, a human declaration of expected cost, separates them. That is why a new
`SLOW_TESTS` entry deserves a sentence about *why* the test is slow, not just a number.

**Proving it.** The `watchdog_probe` feature adds a test that loops forever doing a full rendezvous
each pass, so the heartbeat sees a healthy kernel and only the ceiling stops it. It is expected to
fail, so it is not in the normal suite:

```text
scripts/qemu-bounded.sh 200 cargo test -p kernel \
    --features watchdog_probe --target aarch64-unknown-none-softfloat
```

**The outermost backstop.** `scripts/qemu-bounded.sh` still guards the case where the kernel wedges so
hard the timer IRQ stops. It did not fire for the RedoxFS livelock only because that run invoked
`cargo` directly instead of the wrapper: **a bypassable backstop is not a backstop**, which is exactly
why the ceiling lives in the kernel, where nothing can route around it.

## Tests that guard this

- `smp::a_batch_of_cpu_bound_work_reaches_every_core`, `smp::work_can_be_placed_on_every_core`:
  placement and stealing fill the machine.
- `smp::a_migrated_kernel_thread_keeps_its_hart_pointer`: the tp invariant under forced migration
  (a no-op on aarch64, a real check on RISC-V via `sscratch` ground truth).
- `sched::a_finished_thread_is_reaped_and_its_memory_returned`,
  `user::a_dead_user_thread_frees_its_whole_address_space`: reaping and exact frame accounting under
  cross-core reap lag (the latter waits the lag out rather than reading the instant the count drops).

### A found flake, fixed: a per-CPU test was asserting an affinity nothing promised

Found while running the gates for milestone 35, on a tree whose diff cannot touch scheduling
(`#[cfg(kani)]` harnesses, the DMA validator's layout constants, and the IOMMU domain builder), which
is what made it clearly pre-existing rather than a regression.

`kernel::cpu::tests::boot_cpu_percpu_is_reachable` opened with `assert_eq!(id(), arch::boot_cpu_id())`,
so it asserted **the test case is executing on the boot core**. On aarch64 `boot_cpu_id()` is the
constant 0 and `id()` is derived from `TPIDR_EL1`, which each core sets once at boot and which no
context switch saves or restores; so `id() == 1` means the code really was running on core 1, not that
a pointer was stale. Nothing promises otherwise: with four cores online and §28's stealing, a secondary
core may pull the test thread, and then the assertion fails on an affinity the scheduler never offered.

Observed **once in four consecutive full-suite runs** on an unchanged tree (`left: 1, right: 0`), so
roughly a one-in-four flake on this machine, failing the aarch64 half of `script/test` when it fired.

**Resolved by weakening the test to the property its own doc comment always described**, now
`cpu::tests::percpu_is_self_consistent_on_whatever_core_we_run`: `current()` points at `PERCPU[id()]`
and no other block, plus `of(boot)` reaches the boot core's block by index, which is what the
cross-core paths (IPI, stealing) actually rely on. That is true on every core, so it is stronger
coverage rather than weaker: under §28 placement the suite scatters, and the property gets exercised on
several cores over a run instead of only the boot core.

The rejected alternative was **giving kernel test cases boot-core affinity** to keep the original
assertion. It reads like the more rigorous option and is the worse trade: it buys one assertion back at
the price of running the entire suite on one core, which is exactly where the placement bugs §28
introduced would hide. A harness that avoids the scheduler it is meant to test is not a harness. The
general rule this is an instance of: when a test fails because the system legitimately does something
the test did not expect, check whether the *test's* claim was ever promised before treating the
system's behaviour as the defect.

### The bounded-yield tests fail under host CPU contention, and the control says so (2026-07-30)

Recorded so the next person who sees it does not spend the afternoon reading a diff. Several
`kernel::sched::tests` cases wait for something with a fixed number of `yield_now()` calls, or assert
a count has settled: `threads_round_robin` ("thread 2 never ran"),
`an_interrupt_that_arrives_before_the_wait_is_not_lost`, `other_threads_run_while_one_is_blocked`,
`a_finished_thread_is_reaped_and_its_memory_returned`. Under TCG the guest's four cores are host
threads, so a fixed yield budget is really a wall-clock budget in disguise, and when the host is busy
the budget runs out before the work does.

Observed during milestone 37: **three different tests from that list failed across four full-suite
runs**, every one of them in a module that executes before any of that milestone's code exists, which
already ruled the diff out structurally. The confirmation was cheaper than the reasoning:
**unmodified `origin/main`, same machine, same minute, failed too** (a fourth test, the reaper's
count). Meanwhile a QEMU from another worktree had been holding **200% of the host for 43 minutes**.

Two things follow. A run that fails one of these is not evidence about the branch until it has been
seen on a quiet machine or contradicted by a control run, and a control run costs ten minutes and
settles it. And the standing fix is the one this file already argues for elsewhere: these bounds
should be **progress-based or wall-clock with slack**, not a yield count, because a yield count
measures the host's spare capacity and calls it the scheduler's behaviour.
