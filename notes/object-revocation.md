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
`destroy` would free the child's pages a second time. The fix without a CDT: a parent counts its live
children and refuses `destroy` while any remain (so it can never double-free a live child's pages),
and a child returns its pages to the *parent*, not the allocator, when it is destroyed.

**Return-of-pages is LIFO.** A child destroyed at the top of the parent's watermark gives its pages
straight back to the parent's budget (the watermark un-bumps), so they are re-splittable. That is
exactly what a spawn-then-reap loop does, split a child, run it, destroy it, split the next, so a split
parent is *not* committed for its lifetime: the loop runs forever on a budget of one child. The
benchmark proves it, a 100-iteration spawn loop runs on a 64-page budget that could hold only ~6
children at once. A child freed *out of order* is not at the top, so its pages stay a hole in the
parent until the parent itself is destroyed (which frees the whole run, holes included, exactly once).
This is the LIFO half of seL4's return-to-parent; the general case is what a derivation tree buys, and
we still do not build one.

## Region slots became generational, retiring the lifetime cap

The untyped region table used to be a fixed count-based array whose slots were never reused, so its
256 entries bounded region *creations over the kernel's lifetime*, not concurrent use. A system that
spawns and reaps without end would wedge after 256 regions ever. Object revocation made the table a
generational `slots::Table`: `destroy` removes a region's slot, bumping its generation (so every stale
`Untyped` capability fails, the same stale-safety as everything else), and the next `create` reuses it.
Now the bound is on *concurrent* regions. This is what turns a one-shot reclaim into a repeatable one.

## Endpoints: wake a blocked waiter with an error

An endpoint in a reclaimed region has to be torn down too, or its page would be freed while the
registry still points at it. Revoking one **drains its wait queues**: each blocked thread is popped
off (which frees its intrusive link), marked aborted, and woken, then the endpoint is removed from the
registry, its generational name going stale. The woken thread's blocking `ipc_recv`/`ipc_send` returns
an **error** (the endpoint is gone), not a message it never received, so a waiter blocked forever
cannot pin a region forever, and the reclaim always makes progress.

The delicate part was the IPC core, the block-and-wake path where the lost-wakeup hang once lived. Two
things made it safe without regressing the hot path. First, `endpoint_of` became **fallible**: a stale
`Endpoint` capability (its endpoint reclaimed out from under a holder) used to reach a name that always
resolved, so a miss panicked; now it returns `None` and the caller aborts cleanly. Second, the abort
is routed through a **per-thread `ipc_aborted` flag** rather than changing `ipc_recv`/`ipc_send`'s
return types (66 callers): the IPC primitive sets the flag inside its existing lock and the syscall
layer reads-and-clears it, so no extra lock lands on the fast path and the IPC benchmark does not move.
Kernel-side IPC callers never set the flag (their endpoints are never revoked), so they are untouched.

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

The follow-ons this note once listed are all done: the EL0 spawn benchmark (`lat_proc`, notes/
benchmarks.md), LIFO return-of-pages-to-parent, and error-return for a blocked waiter (both above).
What is left is the general, non-LIFO case of return-to-parent, which is what a full capability
derivation tree buys, and we still have no reason to build one.

## DESTROY force-kills a runaway (milestone 22, DECISIONS §16 amendment)

`DESTROY` used to refuse outright while a live thread occupied the region, which is correct for a
cooperative child but leaves the shell's `^C` escalation (§24) nothing to escalate to: a thread
spinning at EL0, never checking its endpoint, would refuse `DESTROY` forever. The fix is small and
avoids the two hard problems (removing a node from the intrusive `Fifo`, and stopping a thread
running on another core):

- `DESTROY` on a live resident thread now marks it `killed` and still refuses that pass.
- `schedule()` converts a killed thread to a `Finished` corpse at its next preemption instead of
  requeueing it, so the ordinary reaper tears it down (stack, address space) just like a clean exit.
- The owner retries `DESTROY` (the shell already retries for the exit sliver); once the runaway has
  been preempted and reaped, the retry finds the region object-free and reclaims it.

A runaway is preemptible by construction (§5), so **each core reaps its own killed thread on the
timer**; no cross-core IPI, no queue surgery, one branch in `schedule()` and one flag. The tradeoff
is that reclamation waits for one timeslice rather than being instantaneous, which is the bounded
escalation the shell wants, not a stop-the-world. A thread that only ever *blocks* is never scheduled
to hit that preemption, so it is the cooperative tier's job (it is listening on its interrupt
endpoint by definition), not the forcible tier's. Proven on both ISAs by
`destroy_force_kills_a_runaway_and_reclaims_its_region` (`kernel/src/user.rs`): a one-instruction EL0
runaway, reclaimed out from under itself.
