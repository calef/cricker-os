# 105. The two forks milestone 22 named and left

**Status: NOT-STARTED.** Raised 2026-08-04. Milestone 22 (trusted init) built the supervision tree
and, in `notes/trusted-init.md`, recorded two questions it deliberately did not answer, both marked
"Chris's call, not a thing to slip in". **This block's job is to state them precisely enough to be
decided, not to pick.** Both are kernel-surface changes; one is also a rights-model change.

**Gate: DECISION.** Both halves are Chris's and are recorded that way in notes/trusted-init.md,
"Chris's call, not a thing to slip in": whether reclamation and construction are separable rights,
and whether a tid becomes a handle through `Tcb::NAME`. The block states them precisely and picks
neither on purpose.

## Fork one: a reap-only right, split out of `WRITE`

**The fact.** `Untyped::DESTROY` needs `WRITE` on the region, and so does `RETYPE`. One right
authorizes both reclaiming a dead child and constructing a new one.

**What that forces today.** A root supervisor able to restart a dead tier-one server would be a root
supervisor able to build processes, which is precisely the authority milestone 22 set out to give
away. So `root_supervisor` chooses to be *unable* to build, and its policy for a dead tier-one server
is "report and stop". The note calls this "the fail-closed floor pushed as high and as small as it
goes", which is honest about it being a floor rather than a design.

**The shape of the change.** Either a new rights bit below `WRITE`, or a distinct `Untyped::REAP`
method that needs less than `WRITE` does. A supervisor holding it could recover a dead child without
regaining construction authority, which is the property the supervision tree wants and cannot
express.

**What to decide.** Whether reclamation and construction are genuinely separable rights or only
look it. The case against is that reaping a region hands its pages back to an allocator the holder
can then draw from, so the separation may be narrower than it reads. That is the question, and it is
a rights-model question, so it is settled in `DECISIONS.md` before any code.

## Fork two: `Tcb::NAME`, or a tid that becomes a handle

**The fact.** The kernel's fault message names the dead thread by tid (§26.5). No method turns a tid
into something a builder holds. So `sub_server_supervisor` names instances by a handle the *spawner*
issues instead, which works because the tree runs one sub-server at a time.

**Why it does not generalize.** A supervisor with several children receives a tid and cannot say
which child it belongs to without a mapping the kernel could supply and does not.

**Three options the note records, none chosen.**

1. **`Tcb::NAME`.** Small, and it discloses nothing new: the tid is already in the fault message, so
   turning it into a handle reveals no fact the supervisor did not receive.
2. **Per-child fault endpoints.** Rejected once already by §26.5, because synchronous rendezvous
   means `RECV` blocks on one endpoint, so this costs a supervisor thread per child or a wait-any
   primitive that does not exist.
3. **The builder reports the tid it created.** No kernel change at all, and it makes the supervision
   relationship depend on a userspace protocol between builder and supervisor rather than on
   anything the kernel guarantees.

**What to decide.** Whether the kernel owes a supervisor the ability to resolve the identity it
already sends, or whether that is userspace bookkeeping. Option 3 is free and option 1 is a method;
the argument for 1 is that a supervisor which trusts a builder's report is trusting a party it may
be supervising.

## Scope note

**These are one milestone because they arrive together and separately.** Both come from the same
build and both bite the same consumer, a supervisor with more than one child, which is what
milestone 23 (the component OS with live replacement) is. Neither depends on the other, so a
decision on one does not commit the other.

**No implementation is proposed here on purpose.** CLAUDE.md's rule is that a method not fitting the
established model is a design fork raised before it is built, and a rights-model change is further
out than that. The deliverable of *this* block is the statement; the deliverable of the milestone is
whatever Chris decides plus its `DECISIONS.md` section.
