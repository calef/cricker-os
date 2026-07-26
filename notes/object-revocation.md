# Object revocation: tearing a process back down

Retype-from-untyped (milestone 19) let a process build its own kernel objects from EL0: an address
space, a TCB, an endpoint, all carved out of an untyped region it holds. What it could not do was
tear them back down. Every object permanently pinned its region, and nothing reclaimed it. So the
system could build processes but never fully reap them, which is untenable for the thesis (run real
workloads on real machines): a workload that comes and goes must be able to leave.

This is that teardown. The prerequisite everyone assumed was "retype to EL0" (it already shipped);
the real one was **reclamation**.

## The model: region ownership, and generational staleness instead of a derivation tree

To reclaim an object you must guarantee no live capability can still reach it. seL4 does this with a
**capability derivation tree (CDT)**: every copy/mint/grant records a parent-child edge, and revoking
a capability walks the subtree deleting every descendant. It is powerful (revoke one delegation, leave
its siblings) and it is exactly the machinery milestone 19 declined to build.

cricker-os has a different lever already in hand, and it decided the whole design:

- **Objects carry generational names.** A `Tcb`, `Endpoint`, or `Aspace` capability holds a
  `(generation, slot)` name into a registry (`crates/slots`, notes/generational-names.md). Free the
  registry slot and every outstanding capability to that object stops resolving, *forever*, because
  its generation no longer matches. There is no capability to hunt down; invalidation is a side
  effect of freeing the object. This is the milestone-14 machinery, reused for teardown.
- **Frames do not carry names.** A `Frame` capability holds a raw physical address, so frame
  revocation (§13) must actively find and delete every matching capability. That asymmetry is why
  frames needed a revocation *log* and objects do not.

So the model is: **an object's lifetime is its backing region's lifetime, and generational names make
every delegated capability safely stale the moment the region is reclaimed.** Revocation is
region-ownership-scoped: you reclaim by destroying a region you own, which invalidates every object
in it at once. You cannot revoke "just the copy I handed to B" the way a CDT can. For a kernel where
revocation is authority the region owner retains over what it built, that coarseness is the right
semantic, not a limitation. If per-delegation revocation is ever wanted, that is when a CDT earns its
cost, and we would know why we are paying for it.

**How much is thrown away if we later add the CDT? Almost nothing.** The per-object teardown (unblock
waiters, unbind an address space, free the registry slot) and the region reclamation are needed in
both models; the CDT only changes *when* a reclaim fires, not the teardown it fires. The CDT is a
layer on top (derivation-edge recording at delegation sites, a subtree-walk revoke), purely additive.
The region-ownership model is the first floor of the same building, not a fork away from it.

## The trigger: explicit destroy by the owner, and why not auto-on-exit

The reclaim trigger is `Untyped::DESTROY`, invoked by whoever holds the untyped capability. Not
automatic on thread exit. The reason is the capability model itself: **a region belongs to whoever
created it, not to the thread that happens to live in it.** The thread is an occupant; the owner holds
the untyped cap; the occupant never does. Reclamation destroys memory, which is an authority, and in
this kernel authority *is* a capability you hold. Letting an occupant's death free memory it holds no
capability to would reintroduce the ambient authority the whole design removes. Three consequences
make it concrete: the owner is usually not done when the child exits (it may want to read a result or
reuse the space); the bump allocator can only free a whole region, so auto-on-exit could not be
per-object anyway; and an owner-driven loop (build, run, destroy, build again) is deterministic where
scattered reaper-time frees are not.

Thread *exit* still does the automatic half: the reaper tears down the thread's live state (drops it
from the run queue, frees its kmem kernel stack, runs its `Drop` chain). It just does not free the
*memory* of a region-backed object; that waits for the owner's explicit destroy. The split was
already latent in the code (the reaper recycled kmem TCB pages and explicitly left region-backed ones
"for region destroy").

## The lock structure forced the shape

`destroy` is reachable from inside `AddressSpace::Drop`, which runs inside `Threads::remove`, which the
reaper calls **while holding `SCHED`**. So if `destroy` (or the object reaping it triggers) took
`SCHED`, it would self-deadlock against the reaper. That single fact split the implementation:

- **`reap_region_objects` (takes `SCHED`)** is the caller-driven step: it scans the thread and
  endpoint registries for objects whose backing page lies in the region, refuses if any is still in
  use (a Ready/Running/Blocked thread, or an endpoint with a blocked waiter), and otherwise tears the
  dead ones down. `reap_aspaces_in_region` does the same for unbound address spaces under the aspace
  registry lock, a second lock domain, sequenced never nested.
- **`untyped::destroy` (never takes `SCHED`)** is the memory half: it frees the region's frames and
  removes its slot. It stays `SCHED`-free the way it always was (its `revoke_region` takes the
  revocation lock, not `SCHED`, and `AddressSpace::Drop` forgets its own records first so the
  scheduler-taking cap-deletion path is never reached).

`sched::reclaim_region` sequences them: refuse-if-children, reap objects, reap aspaces, unpin, destroy.
It must run outside any `Drop`. That the lock structure *derived* the architecture is the good kind of
constraint.

## Untyped subdivision: SPLIT, and the one honest tradeoff

For a spawner to reclaim per child, each child needs its own reclaimable region. `Untyped::SPLIT`
carves pages off a parent untyped's unspent budget into a new child untyped (seL4's
untyped-retype-into-untyped). The child is independently reclaimable.

The sub-decision this forced, recorded here because a naive version has a double-free trap: a child's
pages are part of the parent's run, so if both the child and the parent were destroyed, the parent's
`destroy` would free the child's pages a second time. The correct fix without a CDT is the simplest
one: mark a split parent `has_children` and refuse to destroy it. The child is ordinary memory that
frees to the allocator at its own destroy. **The tradeoff, deliberately taken:** a child does *not*
return its pages to the parent (the bump allocator has no free list), so a split parent is committed
for the spawner's lifetime; a spawner sizes its budget for the children it will ever carve. seL4
returns pages to the parent through its derivation tree, which we do not build. A future refinement
could add LIFO return-to-parent for the common strictly-nested case, if the parent commitment ever
bites.

## Region slots became generational, retiring the lifetime cap

The untyped region table used to be a fixed count-based array whose slots were never reused, so its
256 entries bounded region *creations over the kernel's lifetime*, not concurrent use. A system that
spawns and reaps without end would wedge after 256 regions ever. Object revocation made the table a
generational `slots::Table`: `destroy` removes a region's slot, bumping its generation (so every stale
`Untyped` capability fails, the same stale-safety as everything else), and the next `create` reuses it.
Now the bound is on *concurrent* regions. This is what turns a one-shot reclaim into a repeatable one.

## Endpoints: the safe subset, and what waits

An endpoint in a reclaimed region has to be torn down too, or its page would be freed while the
registry still points at it. An **idle** endpoint (no thread blocked on either wait queue,
`ipc::Endpoint::is_idle`) is removed from the registry, its generational name going stale. An endpoint
with a **blocked waiter** currently **refuses** the reclaim, exactly as a live thread does.

That refusal is the safe subset of the intended semantic. The chosen richer behaviour is to wake a
blocked waiter with an *error return* (its IPC fails, its capability stale), so a waiter blocked
forever cannot pin a region forever. But that needs surgery on the IPC rendezvous core, the block and
wake path where the lost-wakeup hang once lived, to give `ipc_recv`/`ipc_send` an error channel.
Refusing while a waiter is blocked closes the safety gap (no dangling waiter) without touching that
core, and leaves error-return as a focused follow-on. The common case a spawner hits, an endpoint
nobody is parked on, reclaims cleanly.

## What the tests prove

Each piece is nailed by a test that watches the free-frame count return **exactly** to baseline, which
is the honest witness that memory came back rather than leaked:

- an embryo TCB's region reclaims (the mechanism in isolation);
- an unbound address space's region reclaims (name stale, ASID freed);
- a started, run, and exited child's regions reclaim, and reclaim is *refused* while it is still live;
- `SPLIT` carves reclaimable children and commits the parent;
- 320 create+destroy cycles (past the old 256 cap) all succeed, proving slot reuse;
- an idle endpoint's region reclaims, and a region whose endpoint has a blocked waiter refuses until
  the waiter is gone.

## The surface, and what remains

New EL0 methods on the `Untyped` object (the syscall boundary stays the three calls; these are methods
under `invoke`): `SPLIT` (subdivide) and `DESTROY` (reclaim). See `crates/abi` for the contract and
DECISIONS.md for the record.

Remaining follow-ons, all deliberately scoped out above: error-return for a blocked waiter (the IPC
-core change); LIFO return-of-pages-to-parent for `SPLIT`; and the EL0 spawn-to-reap benchmark
(`lat_proc`) that would put cricker-os's spawn latency next to Linux's `fork`+`exec`, now that a
repeatable spawn loop is finally possible.
