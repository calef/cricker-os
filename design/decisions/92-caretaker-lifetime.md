# 92. A caretaker is supervised by the client it serves

**Status: DECIDED.** calef, 2026-08-16, after asking why the cheaper answer was not the better one.
This unblocks the last item of milestone 31's phase 3, which the lane that scoped it declined to
guess at.

## The problem

`fs_subtree_caretaker`'s serve loop never returns. A job is built in its own 40-page region off a
pool of six, and `job_undertaker` returns that region when the job dies (§13, and §16's LIFO
reclaim). A caretaker built the same way **never dies**, so its region never comes home, and the
LIFO rule then pins the region above it too. Six `rm`s and the prompt answers "could not spawn".

## The decision

**The caretaker is supervised by the client it serves**, so §40's supervisor-death-is-subtree-death
collects it. No new collector, no new lifetime concept, and the machinery already exists and is
proven.

Three reasons, and the second is the one that will matter most:

1. **It is the honest semantic.** A caretaker holds authority *derived from* a grant made to one
   client. When the grantee dies, the derived authority should die. Any other rule says the
   caretaker's life is a fact about how it was constructed rather than about what it is for.
2. **The chained case comes out free.** Milestone 31's remaining work descends one name per
   caretaker, so depth two and beyond is a *chain* of them, each an ordinary FS client above and
   an ordinary FS server below. That chain is a tree, and §40 kills a tree. The alternative has to
   track N members and collect them in an order that respects §16's LIFO, which is the same
   ordering problem with more parts.
3. **It reuses rather than teaches.** Supervision is built; job membership would have to learn
   something new.

## The alternative, and the fallback it becomes

**Job membership** (the caretaker joins the job, and `job_undertaker` returns both regions) was
recommended first and lost on the merits. The recorded reason it was recommended is worth keeping
because it is a bias to watch for: it was the *smaller change to make*, which is implementation
convenience presented as design. calef's question ("why not option 2?") is what surfaced that.

**It remains the fallback, and the fallback has a condition rather than a preference.** The
objection to supervision was construction order: the shell builds the caretaker first, because it
needs the narrowed endpoint to hand to the child, and a child cannot be supervised by a client
that does not exist yet. That objection is probably answerable, because `supervision_proto`
already splits construction: `build_child_space` returns a TCB and an address space, and starting
the child is a separate step, so "build the client's space, build the caretaker beneath it, hand
over the endpoint, start the client" is plausibly expressible today. **Whoever builds this
establishes that fact rather than assuming it**, and if supervision genuinely cannot be set up
before the client runs, they take job membership *and record why*, so the next reader meets a
constraint instead of a taste.

Two other options were considered and refused. A **self-terminating caretaker** (exit after N
operations or when the client's endpoint closes) puts a policy inside a program that should hold
none, and a client that merely pauses would lose its filesystem. A **second undertaker** for a
second kind of corpse is the pair-that-drifts shape §91 refused a few hours earlier, for the same
reason.

## BUGS

- **The region pool is unchanged and may not be enough.** Six regions, and a caretaker per grant
  component means one command line can consume more than one. At depth two or beyond that is a
  capacity question at the prompt, and it is named here rather than discovered there: the pool may
  have to grow before this ships. Nothing in this decision addresses it.
- **This says nothing about a caretaker with no client**, because none exists yet. A caretaker
  built speculatively, or shared by two clients, would need a rule this decision does not have.
