# 116. The fences with no partner

**Status: NOT-STARTED.** Minted 2026-08-04 by the integrator, from milestone 43's audit proposal C,
after the same mistake was found twice the same day by two methods that share nothing.

**Gate: NONE.** The population is findable by grep and the judgement is per site; nothing blocks a
start.

**A release fence orders what came before it against a matching acquire on the reader.** With no
acquire on the other side, it orders nothing that matters and the code reads as though it does. That
is worse than an absent fence, because the fence is the comment: a reader who sees
`fence(Ordering::Release)` stops asking the question.

**Found twice on 2026-08-04, independently:**

- **Milestone 80's loom harness** found the clock page's seqlock writer claiming the sequence with
  nothing ordering the claim ahead of the data stores. A reader could revalidate successfully and
  return a state from one publish beside an offset from another: a silently wrong wall clock. The
  instructive part is that `AcqRel` on the claim does not fix it and neither does `SeqCst`; it needs
  a `fence(Release)` between the claim and the stores, which is Linux's `smp_wmb()`.
- **Milestone 43's audit** found three release-side fences in the compositor's subsystem **whose
  acquire side did not exist**, and separately noted that the kernel's own stand-in for the same
  input ring does fence, which is what shows the gap is an oversight rather than a design.

Neither method could have found the other's, and **no gate in the tree can see either**. Clippy has
no lint for it, Kani does not model concurrency, Miri's stacked borrows are about aliasing, and the
suite passes because both ISAs' hardware happens to be forgiving at the sizes involved.

**The work.** Inventory every `fence`, every `Ordering::Release` store and every `Ordering::Acquire`
load outside test code, and pair them. For each unpaired site decide, on the record, whether it is a
bug (fix it, with the harness that proves it), sound for a stated reason (a single writer with
interrupts masked is a real reason; say so where the fence is), or unreachable. Then decide whether
the pairing can be checked mechanically at all.

**Be honest about that last part**, because it is the interesting question and the answer may be no.
A lint that pairs fences across functions is a dataflow problem, not a pattern match, and milestone
112 already established this tree's posture: a narrow check that is true beats a broad one that is
aspirational. If the answer is "this is a review discipline plus a loom harness per protocol", say
that, record it as a limitation, and make the inventory itself the deliverable. Milestone 80's
`interleaving-check` is the tool that *can* decide a specific protocol, so the useful output may be
a list of protocols that want a harness rather than a gate.

**Scope note.** Not a rewrite. Nothing changes its ordering because of this milestone except where a
site is shown to be wrong, and each such change carries the argument for why the new ordering is
right.
