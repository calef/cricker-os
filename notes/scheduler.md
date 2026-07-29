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
is warm, and a serial pipeline (netd<->std) stays co-located and fast. `wake` does this, and every
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
  about 300 s in netd's userspace smoltcp poll, CPU-bound, no wakes or output for stretches over a
  minute) must not read as a deadlock. The watchdog credits a completed wake, a line of output, OR
  any core running a non-idle thread; only a real lost wakeup, every thread blocked and every core on
  its idle thread, stalls it. See `kernel/src/testing.rs`. It does not catch a busy-spin livelock,
  which is indistinguishable from a live CPU-bound test at runtime.

## Tests that guard this

- `smp::a_batch_of_cpu_bound_work_reaches_every_core`, `smp::work_can_be_placed_on_every_core`:
  placement and stealing fill the machine.
- `smp::a_migrated_kernel_thread_keeps_its_hart_pointer`: the tp invariant under forced migration
  (a no-op on aarch64, a real check on RISC-V via `sscratch` ground truth).
- `sched::a_finished_thread_is_reaped_and_its_memory_returned`,
  `user::a_dead_user_thread_frees_its_whole_address_space`: reaping and exact frame accounting under
  cross-core reap lag (the latter waits the lag out rather than reading the instant the count drops).
