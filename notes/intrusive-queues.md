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
| Blocked | on no queue today (on exactly one endpoint queue after phase A.3) |
| Finished | on no queue, awaiting the reaper |

The structure enforces what the states already meant. When phase A.3 moves the IPC wait queues
onto the same link, "a thread waits on one endpoint at a time" stops being a rule and becomes
a property of there being one field.

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

## Where Tids still live

Phase A.2 moved the *queues* to pointers; the capability payloads (`Reply(Tid)`) and the
endpoint wait queues still speak generational Tids, safely (see notes/generational-names.md).
The design doc's D2 path removes them one structure at a time; the endpoint queues are next
(A.3), and they carry the extra weight of moving the proved `ipc` crate with them.
