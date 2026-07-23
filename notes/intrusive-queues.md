# Intrusive queues

*(Milestone 14 phase A.2. The mechanism behind `crates/intrusive`, which the per-CPU run queues
and migration inboxes are built on. See design/kernel-objects-from-untyped.md, decision D1.)*

## The idea

An ordinary queue (`VecDeque`) owns storage and puts your data in it. An intrusive queue owns
nothing: the "next" pointer lives **inside the thing being queued**, and the queue is just a
head pointer and a tail pointer. Pushing a thread means writing its link field and the tail
pointer. Popping means reading the head and unhooking it.

Every serious kernel does scheduler queues this way (Linux `list_head`, seL4's TCB queues),
because of what it removes:

- **Allocation.** A push writes two pointers. It cannot allocate, so it cannot fail, so it is
  legal anywhere, including the paths where allocation is forbidden (our §9: IRQ context). The
  scheduler used to pre-reserve VecDeque capacity so a push from the timer IRQ could never
  reallocate; that standing apology is gone, because the rule became structural.
- **The lookup.** The old queues held Tids; every pop paid a table lookup to reach the thread.
  An intrusive pop hands back the TCB itself. The migration path got the full benefit: draining
  an inbox into a run queue is now pure pointer movement and touches no table at all, which is
  why `drain_inbox` needs no scheduler lock.

## One link means one queue, and that is the invariant

A thread has one `next` field, so it can be on at most one queue. For a scheduler this is not a
restriction; it is the state machine made physical:

| State | Where |
|---|---|
| Running | on a CPU, on no queue |
| Ready | on exactly one run queue or inbox |
| Blocked | on exactly one endpoint wait queue, or on none (a `CALL`er awaiting its Reply) |
| Finished | on no queue, awaiting the reaper |

The structure enforces what the states already meant. Phase A.3 moved the IPC wait queues onto
the same link, so "a thread waits on one endpoint at a time" stopped being a rule and became a
property of there being one field. The link's handoffs are strictly sequential: endpoint queue
(Blocked) to run queue (Ready, via `wake`, which pops first) to no queue (Running), each
transition under `SCHED`.

## What keeps the pointers honest

The queue borrows nodes it does not own, and the borrow checker cannot see any of it. The
safety is a discipline, stated once and kept:

1. **A queued thread is `Ready`, and only `Finished` threads are reaped**, so a queued TCB is
   never freed. This is the load-bearing rule, and it is why `sched::tcb_ptr` (the one place a
   Tid becomes a pointer) documents it rather than each push site re-arguing it.
2. **The `Box` in the thread table pins the address** (phase B replaces it with a TCB page from
   untyped, pinned the same way).
3. **Only the queue that holds a thread touches its link**, under that queue's synchronization:
   a run queue is single-core with interrupts masked; an inbox is behind its mutex.

## What is proved

The crate's Kani harness (`script/verify`) drives the real `Fifo` with a **symbolic operation
sequence**: six steps, each an arbitrary push-or-pop over three nodes, checked against a
trivially-correct model. Every interleaving up to that depth at once, including the
drained-to-empty transitions where head/tail bugs live. FIFO order, no loss, no invention,
lengths agree, and no stale link is ever dereferenced (Kani checks the pointer accesses
themselves). The host tests cover the same ground concretely, plus link hygiene: a popped node
leaves with its link cleared.

## The wake-before-switch-out race (found by a flake, fixed like the reaper)

Moving the wait queues (A.3) surfaced a scheduler race that predated it: the suite went from
10/10 green on the A.2 baseline to 8/10, failing in the address-space teardown test two
different ways. The race:

1. Thread T (core A) queues itself on an endpoint, marks itself `Blocked`, releases `SCHED`.
   **T is still executing**; its saved context is stale until A's `schedule()` switches away.
2. Core B rendezvouses with T (or an interrupt signals its endpoint): pops T, wakes it, queues
   it on B's run queue.
3. B switches into T's **stale context** while A is still running T's present one. Two cores in
   one thread, on two different ideas of its registers.

The window existed with Tid queues too; the pointer rewire only shifted the timing enough to
observe it. The tell that it was old: the kernel had already solved the *same race for death*.
A `Finished` thread cannot be freed while its core is still switching off its stack, so §11
reaps it from its **successor**, after the switch (`finish_switch`). Being woken is the same
hazard as being freed, for the same reason, with the same fix:

- `Thread::on_cpu`: set when a core schedules a thread in, cleared by that core's successor
  after the switch away.
- `wake()` finding `on_cpu` set does not queue the thread; it parks the wake in
  `Thread::wake_pending`.
- `finish_switch`, running on the thread's own core with the context provably saved, completes
  the wake. (`cpu::switched_from` generalizes the old `to_reap`: the successor now finishes
  both duties, reap or wake, depending on the predecessor's state.)

Verified by moving the suite back to repeated clean runs (statistics, not proof: this is SMP
interleaving, which is exactly the thing our bounded model checking cannot reach; recorded as
a known limit in notes/verification.md).

## The aliasing fine print, honestly

Rust's strictest aliasing models (Stacked Borrows, the experimental semantics Miri checks) would
complain about this design: while a TCB pointer sits in a queue, other code occasionally creates
`&mut Thread` to the same TCB through the table (a mailbox write, the revocation sweep's
`iter_mut`), and under those models a new `&mut` invalidates previously-derived raw pointers.
At the machine level the discipline is sound — every access is serialized by `SCHED` or a queue's
own synchronization, and no two accesses overlap in time — but the language-level story for
intrusive, self-referential kernel structures is still being worked out (Rust-for-Linux lives
with the same tension). Recorded rather than hidden: if rustc ever starts optimizing on the
stricter model, the fix is `UnsafeCell`-based link access, and this paragraph is the pointer to
why. The Kani harnesses are unaffected: they drive the queues with standalone nodes and no
aliasing `&mut`.

## Where Tids still live

Phases A.2 and A.3 moved all the *queues* to pointers; what still speaks generational Tids is
the capability payloads (`Reply(Tid)`), the per-CPU `current`/`idle`/`to_reap` slots, and the
kernel's own bookkeeping around table lookups, all safely (see notes/generational-names.md).
That is the design doc's D2 path proceeding one structure at a time; the payloads fall last,
behind the safety mechanism the generational table already provides.
