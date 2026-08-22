# The `thread::spawn` fork: what a std thread would be, under nife's capability model

*(Written 2026-08-22, investigating milestone 64's rank-3 gap ahead of a decision. See
`design/roadmap/64-std-for-real-crates.md`'s BUGS section and `notes/crates-io-on-nife.md`,
rank 3, for where this was first named. This note is the six-questions write-up; the decision
itself is calef's, requested on pull request #394.)*

## The question in one sentence

`std::thread::spawn` needs N schedulable things that see **one shared, live, growable heap**.
nife's capability model gives every `Thread` (TCB) its own private `AddressSpace`, and that
space is **consumed** at bind time and **owned outright** for automatic teardown. Nothing lets
two TCBs point at the same address space today, and that is not an oversight: it is exactly
what `Tcb::CONFIGURE`'s own contract says (`crates/abi/src/lib.rs`): the `aspace_slot` capability
"is **consumed**: it becomes the thread's, and dies with it." `kernel/src/thread.rs` mirrors it
on the `Thread` struct: `space: Option<AddressSpace>` is "**owned**, so the reaper's `drop` unmaps
and frees the entire address space when the thread dies."

So "what is a std thread" is really "what does it mean for the kernel's address-space object to
have more than one owner", and today the answer is: nothing does, by construction.

## 1. What else was considered, and why did each lose

Three shapes, not two. The obvious one (build it) turns out to hide two very different designs,
and a fourth, decline, is a legitimate option in its own right and the cheapest one available
right now.

| Option | Shape | Loses because |
|---|---|---|
| **A. Shared VSpace, kernel-level** | `Tcb::CONFIGURE`'s existing contract changes so an `Aspace` capability can be *bound* to more than one TCB instead of consumed by the first one. The object itself gains multi-holder liveness tracking. | Real cost: a syscall-surface change, the exact irreversible category CLAUDE.md names. Not proposed as a loser, just costed below. |
| **B. Sibling processes, shared frames** | Keep every existing verb's contract unchanged (`RETYPE_OBJ`, `MAP_INTO`, `CONFIGURE`, `CAP_INSERT`, `START`, all exactly as they are). A "thread" is a second TCB with its own fresh `Aspace`, into which the parent re-maps its own frames at matching virtual addresses. | Looks free (no syscall touched) until you account for what a *growing* heap needs: every future allocation has to be mapped into every sibling space, at the same VA, before either side can safely dereference it. That is a live synchronization protocol built from scratch in userspace, is race-prone the instant two "threads" grow the heap concurrently (which is the ordinary case), and no prior art actually does this for the general shared-heap case. Costed in full below. |
| **C. Decline** | `thread::spawn` stays `Unsupported`, permanently rather than "phase one". Parallelism on nife stays process-plus-IPC-shaped, which is what the rest of the system already is. | Not really a loser: it is the cheapest, most reversible option and the one this note ends up suggesting for *now*, precisely because it forecloses nothing. |

Two things that were **not** separately costed. A pure userspace "green threads" library
(cooperative, stack-switching inside one existing TCB, no kernel involvement) was set aside
without a table row: DECISIONS §5 already closed this door for the kernel's own execution model
("async cannot be the execution model for userspace... a userspace process is an arbitrary ELF
binary [and] will loop forever because we will write a bug") and the identical argument applies
one level up: a `std::thread::spawn` that is secretly cooperative cannot preempt a tight loop in
one of its own "threads" any better than an async executor could, so it would be shipping the
same defect this kernel already refused once. And "give a std thread its own address space but
keep frames aliased only for a small explicit shared arena" is Option B's honest, smaller
cousin, folded into B's costing below rather than given its own row, because it is the same
mechanism at reduced scope.

## 2. What this tree already does in the analogous case

**Process spawn already does exactly the RETYPE/CONFIGURE/CAP_INSERT/START sequence Option B
would reuse**, unprivileged, from userspace: `kernel/src/user.rs`'s `Spawn` struct documents a
process's entire authority as "a function of `arg0`, `grants`, and `maps`", and init itself
becomes "the spawn service" after boot (`INIT_BOOT_ROLE`'s doc comment) and spawns every other
process the same way a std program's PAL would spawn a thread. So Option B needs no new
mechanism at the kernel boundary at all; it is a userspace consumer of machinery that has
existed since milestone 19c.3.

**Multi-holder kernel objects already exist, and Endpoint is the precedent.** DECISIONS §16
records that `Untyped::DESTROY` "refuses... [while] an endpoint in it has a blocked waiter", and
§26's supervision handshake is two different processes each holding a capability that names the
*same* Endpoint object (a spawner grants a supervision endpoint into a child's reserved slot,
and both sides act on the one object). So "a kernel object with more than one holder, and
liveness tracking to match" is not a new category for this kernel to reason about; Option A would
be extending a pattern that Endpoint already proves out, to AddressSpace and (if a thread also
needs to invoke capabilities the spawning thread already holds, for IO from a worker) to CSpace.

**Frame sharing across address spaces is already the tree's workhorse pattern for exactly one
of the two things Option B needs.** `notes/net.md`, `notes/framebuffer-contract.md` and
`notes/line-discipline.md` all describe the same shape: one side mints a frame, keeps its own
mapping, and grants a capability the other side maps into its own space at a VA of its own
choosing. That proves the single-buffer case (control by message, bulk data by shared frame)
works and is cheap. What none of those consumers do, and what a std thread would need, is
replicate a **whole, dynamically growing heap**, not one fixed buffer, which is the finding in
§5 below.

## 3. Prior art outside the tree

**seL4 explicitly supports Option A, and it is nife's own stated lineage.** `crates/abi/src/lib.rs`'s
`objtype::ASPACE` comment already cites "the seL4 principle" for how this kernel's address-space
budget model works. seL4's own `TCB_Configure`/`TCB_SetSpace` binds a VSpace capability to a TCB
without uniquely consuming it: a VSpace capability can be copied like any other capability, and
multiple TCBs can be configured against copies naming the same underlying VSpace object, which is
exactly how seL4-based systems build a conventional multithreaded process (the seL4 tutorials'
own threads example has a new thread explicitly "share the main thread's CSpace and VSpace").
nife's `CONFIGURE` consuming the aspace capability uniquely is a deliberate simplification made
at milestone 19c.3 (buys automatic Rust-`Drop` teardown, costs the sharing seL4 offers for free),
not the ceiling of what the underlying object model can support.
[seL4 Reference Manual](https://sel4.systems/Info/Docs/seL4-manual-latest.pdf),
[seL4 threads tutorial](https://github.com/sel4/sel4-tutorials/blob/master/tutorials/threads/threads.md).

**Fuchsia/Zircon dodges the question by definition rather than answering it.** A Zircon
`Process` object *is* the address space (the root VMAR); every `Thread` object lives inside
exactly one process and there is no way to create a thread with its own separate address space
at all. That is the mirror image of nife's situation: Zircon never had to decide whether two
schedulable things can share memory, because its process object always implies "yes, by
construction." It is not a design nife can adopt piecemeal, because the sharing is the
definition of the object, not a capability granted onto it.
[Zircon process concepts](https://fuchsia.dev/fuchsia-src/concepts/process/overview).

**Everywhere std::thread actually ships, "thread" means one address space, many schedulable
contexts, never many address spaces kept in sync.** Linux's `pthread_create` is `clone` with
`CLONE_VM` (share the mm_struct, do not copy it); it is the same shape as seL4's and Zircon's,
a single shared address-space *object*, referenced by multiple schedulable entities, rather than
several address spaces kept mutually consistent by replaying every mutation into each one.
**No prior art surveyed does Option B's shape (independent address spaces, kept aliased by
replicated mappings) for the general shared-heap case.** The pattern exists everywhere for a
fixed producer/consumer buffer between two different logical processes (exactly what nife
already uses it for), never for "these N things are secretly one heap." That absence, across
three independent designs that solved the identical problem, is itself evidence.

## 4. Is the premise true

Checked at the source rather than assumed. `patches/std-nife/overlay/std/src/sys/thread/nife.rs`:

```rust
pub unsafe fn new(_stack: usize, _init: Box<ThreadInit>) -> io::Result<Thread> {
    Err(io::Error::UNSUPPORTED_PLATFORM)
}
```

Unconditional. No PAL arm, no partial behavior; every call to `std::thread::spawn` on nife hits
this and returns `Unsupported`. The `targets/{aarch64,riscv64}-unknown-nife.json` specs both
carry `"singlethread": true`, which is what routes std to its `no_threads` sync primitives and
single-`static` TLS in the first place, so the failure is consistent with the whole std build,
not a stray gap.

And it costs nothing to fix in the sense the milestone's own BUGS section already states:
**no build failures sit behind rank 3.** `rayon`, `crossbeam-channel`, `tokio`, and `ignore` all
compile and link against the current PAL; they simply get `Unsupported` back the first time they
call `thread::spawn` at run time. (One inconsistency worth flagging, not fixed here:
`notes/crates-io-on-nife.md` names this same set as `rayon, crossbeam, tokio, diesel` in its
summary table around line 381 but `rayon, crossbeam-channel, tokio, ignore` in the prose two
paragraphs later around line 448. Both cannot be the fourth crate; somebody should reconcile it
against the actual probe output, but it does not change this fork.)

## 5. What each option costs, measured where the tree allows it

**Option A, shared VSpace.** Reuses every retype/configure/start primitive that exists; the
change is *who owns the `AddressSpace` object and when it is freed*, not new machinery from
scratch:

- `crates/abi/src/lib.rs`: `tcb::CONFIGURE`'s contract changes from "the aspace cap is consumed"
  to "the aspace cap may be bound without consuming it" (or a `SHARE`-shaped rights bit
  distinguishes the two calls). This is the syscall-surface line: every existing caller of
  `CONFIGURE` (init, every service spawn today) is written against the consuming contract, and
  DECISIONS.md would owe a section under §10/§16's existing surface, not a new syscall number.
- `kernel/src/thread.rs`: `Thread.space` stops being uniquely owned; needs the same
  liveness-tracking shape `Endpoint` already has (§16's "region... an endpoint in it has a
  blocked waiter" check), so `AddressSpace` teardown waits for the last referencing TCB, not the
  first one's exit. This is the biggest single piece, and it is bounded: the region-ownership and
  generational-staleness machinery §16 already built for object revocation is the mechanism to
  extend, not a new one to invent.
- `kernel/src/user.rs`, `kernel/src/revoke.rs`: the reap path (§32, `reap_region_objects`) needs
  to know "still referenced" as a third state alongside live/dead.
- Whether sibling TCBs also need a shared `CSpace` (so a worker thread can invoke a capability
  the spawning thread already holds, e.g. do IO through an already-granted socket or directory
  slot) is the same fork one level down; seL4 answers it the same way, by letting `TCB_SetSpace`
  bind a CSpace root capability without consuming it either.
- Plus, common to every option that yields a real OS thread: the std-side plumbing the PAL's own
  comment already names as missing regardless of the kernel side (TLS story, park/unpark on a
  kernel primitive, join).

**Option B, sibling processes with replicated frames.** Touches no syscall and no capability
method, matching §22's own discipline for `std::fs`/`std::net` ("no new syscall and no new
capability method"). The cost is not at the kernel boundary; it is what "shared heap" actually
demands once you look past the first allocation:

- A single, fixed-size shared arena (map N frames once, at spawn time, into both spaces at
  matching VAs) is genuinely cheap and needs nothing new: it is the exact client/server
  shared-frame pattern `net_stack` and the compositor already use, aimed at a sibling instead of
  a server.
- A **general, growing** heap is not that. Rust's global allocator (`crates/user_heap`) grows by
  minting and mapping new frames on demand, from whichever thread happens to allocate. For two
  "threads" to keep seeing the same heap, every growth by either side has to be mapped into the
  other's address space, at the same VA, before either side may safely dereference a pointer that
  crosses the growth. That is a synchronization protocol the allocator would have to invent from
  nothing (there is no shared page table to make this free the way it is in Option A or in every
  piece of prior art surveyed in §3), and it has to be race-free against two "threads" allocating
  concurrently, which is the ordinary case a work-stealing pool like Rayon's exists to produce.
  Nothing in this tree's frame-sharing precedent (§2 above) does this, because every existing user
  of shared frames shares one buffer whose owner is unambiguous, not a heap two independent
  allocators both mutate.
- So Option B's apparent saving (no syscall touched) is real only for the small-fixed-arena case,
  and disappears for anything that wants to behave like an ordinary multithreaded Rust program.
  `rayon`'s work-stealing pool and `tokio`'s executor both allocate continuously across worker
  threads; a fixed arena sized in advance is a real constraint they were not written to expect.

**Option C, decline.** Zero kernel cost, zero std-side plumbing beyond what already exists
(`Unsupported` is already the answer). The cost is entirely in the roadmap: milestone 149's
Rayon-parallel NPB-Rust variants stay out of scope permanently rather than pending, and any
future crate that needs `thread::spawn` (`crossbeam-channel`'s and `tokio`'s more advanced uses,
beyond what already compiles) gets the same permanent `Unsupported`. That is a scope decision to
write down (a `BUGS` entry or a roadmap scope note, per §71's convention), not a defect.

## 6. Reversibility

**Option A is the expensive, hard-to-undo one, correctly**: it changes what an existing syscall
method promises, and every future program written against `CONFIGURE` inherits that promise.
Once something depends on shared-VSpace semantics, walking it back costs a rewrite of whatever
was built on it.

**Option B is not actually cheap to reverse either, and that is the finding worth carrying**: it
looks reversible because no syscall changes, but a fixed-arena version ships a real constraint
into every consumer built against it (rayon-shaped or not), and *that* is exactly as hard to
walk back as a capability promise once real programs exist that assume a bounded shared arena.
"No syscall touched" is not the same test as "cheap to undo."

**Option C is the only genuinely free-to-reverse choice available right now.** Declining today
forecloses neither A nor B later: nothing is built, nothing is promised, and the only cost is
naming the scope limit honestly where milestone 149 and any future consumer meet it.

## What this note is not

It does not pick A or B. That is the syscall-surface fork CLAUDE.md says is calef's, and "which
option costs less" only resolves once there is a concrete customer for real OS threads on nife,
which there is not today (the NPB Rayon-parallel variants are useful evidence, not a paying
workload). What it does do is retire the "B is the free option" intuition: measured against what
Rust's own allocator needs, B is not obviously cheaper than A, only differently expensive, and in
a way that is easy to miss until a consumer actually grows a heap across the boundary.
