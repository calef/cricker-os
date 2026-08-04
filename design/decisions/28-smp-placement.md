# 28. SMP placement: two random choices at spawn, message-shaped stealing, local wakes

**Status: AMENDED.** (an implementation amendment below records what the build changed.)

**Decided 2026-07-28 (Chris), after §11's deferred "step 3c" was demonstrated by the machine** (a
starved core 0 beside three idle cores, the FS-server watchdog incident). Three parts, each chosen
against the alternatives on the record:

1. **Spawn placement: the power of two choices.** At thread creation, sample two random cores'
   runnable counters (relaxed atomics; stale reads are fine, the gossip lesson) and place on the
   lighter. Near-optimal balancing with O(1) state touched (Mitzenmacher; Sparrow's proof at
   datacenter scale), and the placement path never reads more than two remote cache lines no
   matter how many cores real silicon brings. Chosen over a full least-loaded scan (contends on
   every counter, ages badly with core count) and over Windows-style round-robin (blind to load).
2. **Wake stays local, deliberately.** A rendezvous partner wakes on the current core: message in
   registers, cache warm, direct-handoff locality (seL4's precedent, Linux wake_affine's lesson).
   The hot path affords no policy; the imbalance it can cause is the next part's job.
3. **Correction: idle cores steal by message.** An idle core sends a steal request over the §11
   inbox/SGI machinery to a loaded core, which hands one runnable thread back at its next
   scheduler entry. Pull beats push under uncertainty (every distributed work queue), no shared
   run-queue locks appear (the per-core queues stay single-owner), and it leans toward milestone
   17's message-passing direction rather than away. Cost accepted: a steal lands at the victim's
   next scheduler pass, bounded by the tick.

**Deferred, with triggers:** an explicit placement grant in the spawn manifest (milestone 23's
contract; overrides the default, recovering seL4's userspace-owns-placement story for pinned
components); priorities and CPU budgets (no mechanism today, round-robin is the whole story; the
trigger is a real workload where fairness visibly fails, and the design starts from budgets as
narrowing grants, not from nice); §12's dormant priority-donation item wakes only with priorities.

**Changeability, stated at ratification:** this is scheduler-internal policy. No ABI, no
capability semantics, no baseline movement (the icount benches are hart-pinned). The one-time cost
of enabling any migration at all: latent same-core assumptions (per-CPU state, weak-memory
orderings) lose their accidental cover, so the implementation lands with cross-core stress tests,
rule 4's discipline applied on purpose. Supersedes the Open design ideas placement entry when the
in-flight FS integration lands it. Implementation slots after milestone 22 phase B, before
milestone 23's swap-under-load demo.

## Implementation amendment (2026-07-29, as built)

The three parts shipped as ratified, with one addition the machine forced and two corrections worth
recording. Code in `kernel/src/sched.rs` and `kernel/src/cpu.rs`; scheduler note in
notes/scheduler.md; cross-core stress tests in `sched.rs`, `smp.rs`, and `user/tests.rs`.

- **Spawn placement, as built.** `spawn` calls `pick_spawn_target`: two samples of a per-core
  xorshift PRNG index the online cores, and the lighter by `runnable()` (a relaxed mirror of run
  queue + inbox depth, kept current in `cpu::with_runq` / `note_inbox_len`) wins. The PRNG is seeded
  per core from a fixed constant so a given boot makes the same choices, which keeps the icount
  benches reproducible. On one online core it is a no-op.

- **Stealing, as built.** An idle core's `try_initiate_steal` picks the most-loaded other core by run
  queue depth alone (never its inbox, which is work already in flight to it), CASes a one-slot steal
  request, and pokes it with the reschedule SGI; the victim's `serve_steal_request` hands back one
  queued thread through the requester's inbox at its next scheduler entry. Pull-based, no cross-core
  run-queue lock.

- **The wake SPLIT (the addition).** §28.2 said "wake stays local." That is right for an **IPC
  rendezvous**: the partner wakes on the waker's core, message in registers, cache warm, and the
  serial net_stack<->std pipeline stays co-located. It is wrong for a **device interrupt**, which carries
  no such locality: pinning the woken driver to the IRQ-handling core re-concentrates the pipeline
  (std_net) or lands it on a busy core. So `irq_notify` wakes LOAD-AWARE via `wake_load_aware` /
  `pick_wake_target`: the least-loaded core, ties won by the current core so a driver taking a
  completion interrupt every request (the block server at mount) is not migrated each time. Rendezvous
  wakes (`ipc_*`, supervision, revocation) stay local, unchanged. This is the split the IRQ-delivery
  work recommended; the device-line affinity that spreads which core takes each IRQ is its companion,
  documented in notes/interrupts.md.

- **Correction: migration needs the per-hart pointer to be right (RISC-V).** §28's scattering is the
  first workload to preempt kernel threads on secondary harts and then move them. That exposed a
  latent RISC-V bug: the trap frame saved and unconditionally restored `tp`, the kernel per-CPU
  pointer, so a thread preempted on one hart and resumed on another came back reading the wrong
  hart's per-CPU state. Fixed in `arch/riscv64/trap.s` (restore `tp` only for a U-mode return; a
  kernel return keeps the live, correct one). Full write-up in notes/riscv-port.md. aarch64 was
  immune (its pointer is `TPIDR_EL1`, a system register the frame never carries).

- **Correction: the hang watchdog now credits real progress, not test starts.** With migration and a
  slow-but-live workload, the old "did a new test begin in the last 60 s" heartbeat could not tell a
  deadlock from a slow test, and it tripped std_net, which legitimately runs about 300 s in net_stack's
  userspace smoltcp poll (CPU-bound, no wakes and no output for stretches over a minute). The
  watchdog now counts progress as a completed wake or a line of output OR any core running a
  non-idle thread; only a genuine lost wakeup (every thread blocked, every core on its idle thread)
  stalls it. See `kernel/src/testing.rs`.

- **Correction to that correction: a progress-only heartbeat traded a flake for a silent hang, so the
  test harness also enforces a per-test wall-clock ceiling** (2026-07-29). The caveat recorded above,
  that the progress heartbeat cannot see a busy-spin livelock, was accepted on the argument that the
  leaked-spinner regression test and `scripts/qemu-bounded.sh` covered it. **That reasoning was
  incomplete, and the machine showed it:** the RedoxFS repeat-write livelock spins in an allocator
  commit *while still serving blk IPC*, so every rendezvous reset the heartbeat, and a failure that
  had been a loud 60 s watchdog trip became an infinite silent hang at about 400% CPU with no
  watchdog fire at all. A livelock that makes IPC progress is indistinguishable from healthy work to a
  progress-only instrument, and turning a loud failure into a silent one is strictly worse than the
  flake the heartbeat fixed.

  So the harness now asks two questions and either can fail the run. **The heartbeat** ("is anything
  happening at all?", ~60 s) is unchanged and still catches a deadlock fast, anywhere, including
  before the first test. **The per-test ceiling** stamps each test with a wall-clock budget and fails
  when it is exceeded *even while progress is being made*, which is exactly the case the heartbeat
  cannot see. The failure names the test, its runtime, and its budget, and says which of the two
  failures it is, so a livelock is diagnosable rather than an anonymous timeout.

  **Budgets are per test, not one global ceiling**, and that is the judgment call worth recording.
  std_net honestly runs 300 to 344 s, so a single ceiling would have to sit near 700 s, which would
  let a two-second unit test spin for eleven minutes before failing: a limit that catches almost
  nothing is not worth the false confidence. Instead the default is a tight 90 s and the known-slow
  tests declare their own cost in `SLOW_TESTS`, each entry carrying the reason. The exception stays
  visible and reviewable instead of being absorbed into a number that protects nothing, and the cost
  is one table with (today) one row.

  **What each mechanism can and cannot see** is stated in `testing.rs` and notes/scheduler.md rather
  than left implicit, because that is what went wrong the first time. Neither can distinguish a
  livelock from slow-but-correct work while it runs; only the budget, a human declaration of expected
  cost, separates them. A feature-gated probe (`watchdog_probe`) loops forever doing a full rendezvous
  each pass, so the heartbeat sees a healthy kernel and only the ceiling stops it; it is expected to
  fail and stays out of the normal suite. And `qemu-bounded.sh` remains the outermost backstop, for a
  kernel wedged so hard the timer IRQ stops: it did not fire in the reported case only because that
  run invoked `cargo` directly instead of the wrapper, which is the argument for the in-kernel check.
  **A bypassable backstop is not a backstop.**
