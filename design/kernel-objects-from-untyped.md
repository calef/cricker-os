# Milestone 14: kernel objects from untyped, and the death of the kernel heap

The deliverable, from design/roadmap.md: retype TCBs, endpoints, and page tables out of untyped
memory, the way milestone 11 already does for user pages, and delete the kernel heap and slab.
This document is the working plan: what the heap actually backs today, the shape we are moving
to, the phases, and the two decisions that have to be made on purpose rather than by drift.

## Why a verifiable kernel cannot allocate

Milestone 11's property was: *a process cannot make the kernel allocate.* True for user pages,
false for everything else. Spawning a thread allocates a `Box<Thread>`, a `BTreeMap` node, and a
16-slot cspace `Vec`. Creating an endpoint grows a `Vec`. Blocking on IPC can grow a `VecDeque`.
Every one of those is the kernel spending its own pool on a user's behalf, so the pool can be
exhausted, and the exhaustion path is reachable by anything that can spawn or block.

The verification cost is worse than the exhaustion. Every allocation site is a hidden branch
("or fail"), every collection an unbounded structure a bounded model checker cannot swallow, and
the allocator itself is exactly the kind of pointer-heavy code BMC walls on. seL4's proof leans
on the kernel never allocating: after boot, every byte the kernel touches is either static or
handed to it by userspace via retype. That is the shape we are adopting, because §14 committed
us to proofs of the kernel itself, not just its logic crates.

## What the kernel heap backs today (the inventory)

Measured by reading, not guessing. Transient allocations in test code excluded.

| Object | Container | Created by | Lifetime | Bounded by |
|---|---|---|---|---|
| TCB (`Thread`) | `BTreeMap<Tid, Box<Thread>>` | `spawn` | thread lifetime | nothing |
| BTreeMap nodes | inside the map | `spawn` | thread lifetime | nothing |
| cspace | `Vec<Option<Cap>>`, 16 slots | `spawn` | thread lifetime | 16/thread |
| endpoint | `Vec<Endpoint>` (global table) | `create_endpoint` | forever (no delete) | nothing |
| IPC wait queues | 2 `VecDeque<Tid>` per endpoint | blocking | endpoint lifetime | nothing |
| run queue + inbox | `VecDeque<Tid>` per CPU | boot / blocking | forever | thread count |
| kernel-stack VA reuse | `Vec<u64>` | thread exit | forever | dead threads |
| stack/AS frame lists | `Vec<Frame>` per thread/AS | `spawn` | thread lifetime | pages owned |
| spawn closures | `Box<Box<dyn FnOnce>>` | kernel `spawn` | until thread entry | short |
| untyped region table | `Vec<Region>` (global) | `create` | forever | region count |
| virtio device table | `Vec<Device>` (global) | boot | forever | device count |
| revocation database | `Vec<Mapping>` (global) | every user mapping | until revoke | **nothing: grows per mapping** |

Three families fall out:

1. **Per-thread and per-endpoint objects** (TCB, cspace, wait queues, endpoint state). These are
   the seL4 kernel objects, and they become *retyped from untyped*: the process that wants a
   thread or an endpoint pays for it out of its own budget. This is the heart of the milestone.
2. **Fixed machine tables** (per-CPU queues, virtio devices, untyped regions). Small, bounded by
   the machine rather than by user behavior. These become static fixed-capacity structures; no
   syscall needed, nobody pays but the image.
3. **The revocation database.** The awkward one: it grows with every user mapping, it is global,
   and no fixed bound is honest. It has to be charged to the processes that create mappings.

## The seL4 shape, and how far we take it

In seL4 there is no kernel allocator at all. `Untyped_Retype` takes an untyped capability and an
object *type* (TCB, endpoint, CNode, page table, frame) and the kernel lays the object down
inside the untyped region. The kernel's "allocation" is a watermark bump in memory the caller
already owned. Our `untyped::retype_page` is already exactly this for one type (a user frame).
The milestone widens it: `RETYPE` gains an object-type argument, and the kernel object the
syscall creates lives in the page it just carved.

What we deliberately do not copy: seL4's CNode trees, object-size arguments, and the full CDT.
Our objects are page-granular (a 4 KiB page holds one kernel object or many of one kind; we
decide per type), because page granularity is what `retype_page` already speaks and sub-page
retype bookkeeping is complexity the demonstrator does not need yet.

## The phases

Each phase compiles, boots, and passes the full suite before the next begins.

**Phase A: every kernel structure gets a fixed shape (no syscall changes).**
Replace the unbounded collections with allocation-free structures: the thread table stops being
a `BTreeMap` of `Box`es, the IPC wait queues stop being `VecDeque`s, the per-CPU queues get
fixed capacity, the small global tables become static arrays. The heap is still linked after
this phase, but nothing on the spawn/IPC/exit path allocates. This is most of the mechanical
work, it is where decision D1 (queues) and D2 (thread table) bind, and it is independently
valuable: the hot paths become O(1) and heap-free even before retype lands.

**Phase B: kernel objects stop touching the heap (decided 2026-07-23, after the conversation).**
The original sketch here widened `RETYPE` with object types. The conversation found the sketch
premature: no syscall lets a process create a thread or an endpoint today, so a user-facing
retype would be API with no caller, and a user-created TCB is inert without a thread-control
surface (configure, start, cap installation) whose requirements only the milestone 19 init task
can supply. **Decided: the surface stays narrow; the API is designed against init's real
requirements later.** What phase B builds instead, per the decisions:

- **B.1** (no decisions): every kernel object gets a fixed shape. The cspace becomes a
  const-generic array in `crates/caps`, `KernelStack`'s frame list an array, and the endpoint /
  untyped-region / virtio / stack-VA tables fixed arrays.
- **B.2**: TCBs move from `Box` to a **static pool** (BSS, MAX_THREADS slots; table slot i is
  pool slot i). Retype-from-untyped was considered and declined while the kernel is the only
  payer: sub-page packing would rebuild the slab in the milestone that deletes it (see
  notes/tcb.md). The pool upgrades to retype-backed storage behind the table when init lands.
- **B.3**: spawn's boxed closures move **onto the new thread's own stack**, above the trampoline
  frame, at their concrete type (a monomorphized call shim in `x20`, the closure address in
  `x19`, no vtable). Call sites unchanged; one reviewed unsafe region; invisible to userspace.
  The fn-pointer alternative taxed every capturing call site forever to avoid one unsafe block.
- **B.4**: `exec` carves a **per-process untyped region**; image pages and page tables come from
  its watermark, `AddressSpace` records the region id instead of a frame list, and teardown is
  `untyped::destroy`, which §13 revocation already makes safe (the "reclaim-on-process-death"
  wiring untyped.rs deferred). One budget per process; when init arrives, only the region's
  provenance changes.

**Phase C: charge the revocation database, then delete the heap. (Built; milestone complete.)**
The mapping records moved into log pages retyped from each mapping process's own region, found
through a fixed registry of live address spaces: the mapper pays for its own records, a mapping
that cannot afford its record is refused (and unmapped), and the records are reclaimed with the
region at teardown, so "forget this space" became one registry slot going empty. Then the
`GlobalAlloc`, the slab wiring, and `kernel/src/heap.rs` were removed from the kernel build (the
heap and slab crates stay: host-tested logic whose notes tell their story). **The kernel that
boots after phase C cannot allocate, structurally: there is no allocator to call.**

## Decision D1: what replaces the IPC wait queues

A blocked thread sits on at most one wait queue (it is blocked; it cannot be in two places).
The classic answer, seL4's and Linux's alike, is the **intrusive list**: the queue link lives
*inside* the TCB, and an endpoint's queue is just a head pointer into TCBs it does not own.
Queues of any length, zero allocation, O(1) push and pop.

The alternative is a **fixed-capacity ring** in each endpoint: simpler to reason about and to
prove, but it invents a new failure mode ("queue full: what now, drop the blocker?") that no
amount of tuning removes honestly.

The wrinkle is milestone 18: `crates/ipc` proves the rendezvous over real `VecDeque`s, and the
kernel runs that proved code. Whatever replaces the `VecDeque` must move the proofs with it,
same properties over the new structure, or the rewire quietly demotes proved code back to
argued code. The decision core only ever asks "is the queue empty" and "pop the head", so the
proof surface is small either way; the intrusive version proves those over a fixed-capacity
model of the link fields.

**Decided and built: intrusive lists through the TCB**, because "queue full" is not a failure
mode a kernel should have, and because it is the design every kernel we learn from converged on.
Phase A.2 moved the run queues and inboxes (`crates/intrusive`); phase A.3 moved the endpoint
wait queues and restated the `ipc` proofs over the intrusive structure. A.3 also surfaced and
fixed the wake-before-switch-out race (see notes/intrusive-queues.md): being woken mid-switch
is the same hazard as being freed mid-switch, and it now has the same successor-side fix.

## Decision D2: what replaces the thread table

**Decided: capability-only naming is the end point** (seL4's shape: everything that names a
thread holds a reference to the TCB; the kernel never looks a thread up by number, so there is
no table). The decision here is the path, because our Tids are woven through the scheduler, the
queues, and the capability payloads, and they cannot all change at once.

The path runs through the intrusive-list work, one Tid use at a time:

1. Run queues and inboxes stop holding Tids when they become intrusive lists (D1): a queue
   entry becomes a TCB link.
2. Endpoint wait queues follow, when the proved `ipc` crate is restated over links.
3. Iteration (revocation sweeps every cspace) gets a global intrusive all-threads list.
4. Capability payloads (`Object::Reply(Tid)`) fall last, and only behind a safety mechanism:
   a pointer in a long-lived capability dangles when the thread dies. Today's Tid is safe
   precisely because a dead Tid fails the lookup. seL4 makes raw pointers safe with the CDT
   (destroying a TCB revokes every capability naming it); we deferred the CDT deliberately.

The interim table is therefore not a plain array but a **generational slot table**: a Tid is
`(generation, slot)` packed in one u64, lookup is an index plus a generation compare, and slot
reuse bumps the generation, so **a dead thread's name can never resolve again**. That property,
stale names fail safely, is the same one that eventually lets capabilities carry direct thread
names without a CDT. The table is not a rival to capability-only that later gets demolished;
built generationally, it is the first step of it. Steps 1 to 3 hollow out its callers until its
one remaining job is validating the long-lived names inside capabilities, which is exactly what
a generation check is for.

The table is pure logic (a fixed-capacity generational map, generic over the entry), so it
lives in a host crate with milestone 18-style proofs: a removed name never resolves again, two
live entries never share a name, reuse changes the name. Those are the properties step 4 will
one day lean on, proved before anything leans on them.

## What this milestone does not do

- No sub-page object packing decisions until the object sizes are measured (a TCB is a few
  hundred bytes; whether four share a page is a phase B detail, not a commitment).
- No CDT. Revocation stays frame-scoped (§13); the derivation tree still waits for a driver.
- No change to user-facing map/retype semantics for frames: milestone 11's paths keep working.

## Where this leaves verification

Phase A shrinks every structure the milestone 18 proofs touch to a fixed shape, which is
exactly what bounded model checking wants. The follow-on proofs this unlocks, once the kernel
cannot allocate: the retype watermark never hands out overlapping pages (extend `frames`-style
proofs to `untyped`), and the intrusive queue's link discipline (a TCB is on at most one queue).
Those become milestone 18-style harnesses in the phase that builds each structure.
