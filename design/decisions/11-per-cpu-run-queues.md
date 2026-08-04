# 11. SMP: per-CPU run queues, message-based migration. §6, reopened.

Decided 2026-07-22. This reopens §6, which chose single-core and named the cost: "a scheduler
rewrite later, accepted knowingly." This is that rewrite.

§6's caution was against building per-CPU structures *while still single-core*, when the need was
speculative. Going multi-core makes the need real, so per-CPU run queues are the design now, not
premature insurance. We build the per-CPU design directly rather than staging through an
intermediate global-lock scheduler.

The one real fork, how cores share scheduling work, was decided against work-stealing and for
**message-based migration**: no core ever touches another core's run queue; work moves by a message
to the target's inbox and an SGI. This keeps scheduling coherent with the rest of the kernel, where
coordination is already IPC (§10) and an interrupt is already a message (9a), and it makes the
cross-core race class unrepresentable instead of merely guarded. The trade accepted: no pull-based
load-balancing, and migration costs an IPI, neither of which matters on a 4-core QEMU box.

## What is already SMP-safe, and what is not

Two earlier decisions paid forward, and the starting point is cleaner for it:

- **`IrqSafeMutex` is already a real cross-core spinlock.** Its inner primitive is `spin::Mutex`
  (sync.rs), which provides mutual exclusion and the acquire/release fences on lock and unlock.
  Anything touched under a lock is already correct across cores. §9's "every kernel lock is an
  `IrqSafeMutex`" was SMP groundwork we didn't label as such.
- **TLB invalidation is already broadcast.** Every `tlbi` we emit is the inner-shareable form
  (`vmalle1is`, `vaae1is`); `flush_tlb`'s own comment says "wait for every core." aarch64's DVM
  broadcasts invalidation in hardware, so cross-core TLB shootdown needs no IPI for the cases DVM
  covers. This is a place aarch64 is simply better than x86, where shootdown is an IPI storm. §4
  rule 4 ("assume weak ordering") banked exactly this.

What has no SMP story, the four gaps:

1. **Secondary bring-up: none.** Cores 1..n park in `wfi` at `boot.s` with no wake path. No PSCI.
2. **Per-CPU storage: none.** `TPIDR_EL1` is unused; there is one boot stack in `link-aarch64.ld`.
3. **`HELD_RANK` is a single global** (sync.rs). A second core clobbers it and the lock-rank
   assertion starts firing on phantom violations.
4. **`SGIR` / `ITARGETSR` are hardcoded to core 0** (gic.rs).

## The design

**Per-CPU identity via `TPIDR_EL1`.** Each core holds a pointer to its own per-CPU block in
`TPIDR_EL1`, set once during that core's init; `cpu::current()` reads it back. `MPIDR_EL1`'s
affinity gives the physical id at bring-up, mapped to a dense logical `0..N`. This is the standard
aarch64 per-CPU base; Linux uses `TPIDR_EL1` identically.

**The per-CPU block.** One `PerCpu` per core, in a fixed `[PerCpu; MAX_CPUS]`: its run queue, `current`,
`idle`, `need_resched`, held-rank, timer counters, and a cross-core **inbox**. Everything except the
inbox is touched by that core alone.

**The core rule: no core ever touches another core's run queue.** A run queue is single-owner. The
only way work reaches core B is a **message** to B's inbox followed by an SGI; B drains its own inbox
into its own queue. This is the same "coordination is a message" paradigm as IPC (§10) and
interrupt-as-message (9a), now applied to scheduling. It makes the entire class of cross-core
run-queue races **unrepresentable** rather than defended-against, the same move as §9's rank
assertion and `TlbFlush`'s `Drop`. We chose this over work-stealing deliberately (see the design
alternatives discussion): stealing means shared mutable queues and cross-core locking, and
message-based migration is the coherent fit for a kernel whose whole thesis is that coordination is
IPC.

**Two consequences fall out:**

- **The run queue needs no cross-core lock at all.** Only its owning core reads or writes it, and
  reentrancy from that core's own timer/IRQ is handled by masking IRQs around the access. §9's
  `IrqSafeMutex` masks IRQs *and* spinlocks; here the spinlock half is simply unnecessary. A per-CPU
  `VecDeque` behind IRQ-masking, not a `spin::Mutex`.
- **The hot path holds no lock.** `schedule()` pops from its own queue (IRQs masked) and switches; it
  touches no global structure. To make that true, a run-queue entry is a small
  `RunNode { tid, ctx: *mut Context, kstack_top, ttbr0 }` carrying everything a switch needs, cached
  at enqueue. The `Thread` box stays owned in the global `threads` map (for lookup and reaping), and
  the raw `ctx` pointer is valid because a thread leaves every queue before the reaper frees it. This
  is the "decouple" answer to the run-queue↔global-map ordering question: the map is off the hot path.

**What stays global, behind a lock:** the `threads` map (Tid → Thread; owner and directory, touched
on spawn/reap, not on the switch) and `endpoints` (IPC rendezvous; shared, because a send on one core
wakes a receiver bound to an endpoint). Neither is on the scheduling hot path.

**The inbox is the one cross-core structure.** A per-core `IrqSafeMutex<VecDeque<Tid>>`. A producer
(another core) locks it, pushes a Tid, unlocks, and SGIs the target. The owner locks it, drains into
a local, unlocks, then enqueues into its own run queue (no lock). Touched only on migration, which is
rare; the hot path never sees it. (Lock-free MPSC inbox is a later exercise; a tiny spinlock is the
correct first cut.)

**Lock ordering.** With no run-queue locks, the surface is small:

- **`THREADS` and `ENDPOINTS` rank above `INBOX`.** Spawn or IPC-wake finds/creates a thread (holding
  THREADS or ENDPOINTS), then pushes to a target inbox. Always that order.
- **Inboxes are equal rank and never nested.** A core locks at most one inbox at a time (the
  target's), so §9's rule that `R < R` is false forbids the only possible cycle.
- **`HELD_RANK` becomes a `PerCpu` field.** Each core tracks its own; `force_reset_ranks` resets only
  the caller's.

**Placement and waking.** Prefer the **current core**: a `spawn` or an IPC-wake whose thread can run
here just enqueues locally, no lock, no IPI. Only when a thread must run elsewhere (spreading across
idle cores at spawn, or waking a thread whose target core is idle in `wfi`) do we message the target
inbox and SGI it; the SGI handler drains the inbox and re-runs `schedule()`. That is also the
reschedule-a-remote-core primitive. Spreading policy stays trivial (round-robin idle cores);
balancing cleverness is unmeasurable on QEMU.

**Bring-up via PSCI.** QEMU `virt` implements PSCI. Core 0, after its own init and once the heap
exists, calls `PSCI CPU_ON` (via `SMC`) for each secondary, passing an entry point and a per-core
stack **allocated from the frame allocator** (the heap is up by then, so no static stack array).
Each secondary sets `sp`, sets `TPIDR_EL1`, enables its own GICC (PMR + CTLR) and its timer PPI,
then enters the scheduler and runs its idle thread.

**Memory ordering, as one invariant.** The rule that keeps this tractable:

> **Per-CPU state is touched only by its own core. All cross-core work movement is exactly: lock
> the target's inbox, push a Tid, unlock, SGI.**

The inbox's `spin::Mutex` supplies the acquire/release fences for the Tid handoff, and the SGI is an
event the receiver observes only after the push is visible. So the per-CPU lock-free atomics
(`need_resched`, `idle`) stay single-core-accessed and need nothing above `Relaxed`. The audit is
mechanical: any lock-free atomic read or written by more than one core either becomes per-CPU or gets
Acquire/Release. The known suspects, all `Relaxed` today (`NEED_RESCHED`, `IDLE_TID`, the timer
counters), all become per-CPU, which resolves them.

**GIC.** The SGI is now the migration primitive, so it matters more than I first framed. Parameterize
`send_sgi(intid, target)` off the core-0 hardcode; each core runs its own GICC enable + PMR. SPI
routing (`ITARGETSR`) stays on core 0: the only sources are the per-core timer PPI (needs no routing)
and virtio SPIs (one core fields them). The timer being a PPI means preemption is already per-core for
free.

## Build order (done, 2026-07-22)

The migration path came online *with* the queues, not after: there was no separate race-prone
stealing phase to bolt on, because we are not stealing. All of this landed and passes under
`-smp 4` (91 kernel tests):

1. **Per-CPU infrastructure** ✅ (3a, 3b-i). `TPIDR_EL1`, the `PerCpu` block, `cpu::current()`, and
   `HELD_RANK` / run queue / `current` / `idle` / `need_resched` → per-CPU. Behavior-neutral on one
   core, verified in isolation.
2. **Secondary bring-up** ✅ (step 2). PSCI `CPU_ON`, per-core stacks. Cores come up and idle.
3. **Secondaries schedule** ✅ (3b-ii). Per-core idle thread, GIC CPU interface, timer, and the fine
   map; the reaper fixed to run after the switch, not during. Each core schedules from its own queue.
4. **Cross-core migration** ✅ (3c). The inbox + reschedule SGI; `spawn_on(core, f)` places work on
   any core. The memory-ordering invariant held: the inbox lock's release/acquire orders the handoff,
   no extra barriers needed.

**Remaining, and deliberately deferred:** wiring `spawn` itself to round-robin over `spawn_on` (auto
load-balancing). The mechanism is done; making it the default placement policy would scatter the
existing tests' threads across cores and make their yield-based synchronization timing-dependent, so
it wants those tests audited first. Also still deferred (unchanged): per-CPU allocator caches for
*scalability* (§9), which are a different problem from correctness.

Three SMP-latent bugs, each invisible on one core, surfaced during 3b-ii and are recorded in that
commit: `VBAR_EL1` is per-core (a secondary that never set it died silently on its first trap); the
per-core boot stacks were an immutable `static` that landed in read-only `.rodata`, which only bit
once the fine map enforced W^X; and a global tick counter, advanced by every core, broke "holding a
lock masks *my* timer." The clean starting point (`IrqSafeMutex` already a real spinlock, every
`tlbi` already inner-shareable) is why there were only three.

## Testing

`-smp 4` in `qemu-runner-aarch64.sh`. New invariants, each proving something one core could not: a shared
counter incremented by threads on multiple cores under a lock sums **exactly** (cross-core mutual
exclusion); a spawned thread runs on a core other than the spawner (the inbox/SGI path actually
delivers work); an IPC send on one core wakes a receiver that runs on another; the per-CPU rank
tracking does not false-positive under concurrent locking. The semihosting exit stays single-caller:
core 0 drives the runner, the others idle at suite end.

## Risks, named

The race that eats SMP schedulers, two cores mutating one run queue, is **gone by construction**: no
core touches another's queue. What is left is smaller and more legible: the inbox handoff (a Tid
published under a lock, consumed after an SGI), the memory ordering of that handoff, and PSCI
bring-up. First-encounter weak-memory bugs are still heisenbugs, so the ordering invariant above is
kept deliberately narrow. This is still the hardest debugging in the project, but the single-owner
choice removed its worst part.

## Out of scope

**Work-stealing** (pull-based migration, an idle core reaching into a busy core's queue) is
deliberately not built: it is the shared-mutable-queue design we chose against. It stays available as
a contained later exercise ("replace the inbox push with a stolen queue") once the foundation is
solid. Also out: CPU affinity/pinning, NUMA, CPU hotplug, per-CPU reserve pools for allocation
scalability (§9 parked those separately), and any balancing cleverer than round-robin spread.
