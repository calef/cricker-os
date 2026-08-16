# 88. `needs-architect` as a required check, rather than as a script's restraint

**Status: PROPOSED.** (raised 2026-08-16, when the merge queue reduced `scripts/merge-drain.sh` to
an admission policy and made the weakness of that policy the only thing left in it.)

**Number is provisional**: §87 was in flight in an unmerged pull request when this was written, so
the integrator may renumber at merge.

**What.** `needs-architect` means a pull request is outside standing merge authority: it touches the
syscall surface, adds a dependency, or owes a `DECISIONS` section. Today the label is enforced by
*one script choosing not to arm it*. Nothing stops a merge by any other route, and nothing stops the
work merging if the label was never applied.

**Why it comes up now.** GitHub's merge queue took over the ordering half of `merge-drain.sh` on
2026-08-16, and what remains is the admission policy: which pull requests get enqueued at all. That
policy is now the script's entire reason to exist, which makes its enforcement worth looking at
rather than inheriting.

**The weakness, in the tree's own words.** notes/merge-queue.md's `BUGS` has said since the script
was written: *"A pull request that should be held but was never labelled will be merged by it. The
label is applied by hand at the moment the decision to hold is made, so a maintainer that forgets the
label has bypassed the gate rather than tripped it."* That is rung four of CLAUDE.md's ladder, which
is the rung the ladder exists to talk people out of. It is also the exact failure §69 and milestone
115 record from the naming side: a decision that lived in one person's attention.

**The proposal.** A required check that **fails while the label is present** and passes when it is
absent. Then:

- Holding is enforced by branch protection rather than by a loop declining to act, so no other merge
  route bypasses it. Rung two, and it fires without being remembered.
- Arming everything becomes unconditionally safe, and `merge-drain.sh` reduces to arming plus stall
  reporting, small enough to be a scheduled workflow rather than a session-lifetime loop. That kills
  notes/merge-queue.md's first `BUGS` entry, which is that neither watcher survives the session that
  starts it.
- Removing the label re-runs the check, so the release is a deliberate act with a timestamp on it,
  which the current arrangement does not produce.

**What it does not fix, stated plainly.** It enforces the label; it cannot apply the label. A pull
request that *should* be held and never gets labelled is exactly as unheld as it is today. The
mechanisms that could close that are separate and harder: a `CODEOWNERS` entry on
`kernel/src/syscall.rs` and on `design/decisions/`, or a check that reads the diff and demands the
label when the syscall surface or the dependency graph moved. **The second is the real answer** and
it is a bigger piece of work; this proposal is the cheap half that makes the expensive half optional
rather than urgent.

**The argument against.** A required check that fails on purpose reads as a broken build to anyone
who has not met the convention, which is precisely the newcomer milestone 117 is written about. If
this is taken, the check's output must say what it is and how to clear it, in one sentence, rather
than reporting a bare failure. Two lines of text are what separate a mechanism from a trap.

**A second cost worth naming.** It adds a required check to every pull request in a repository whose
merge cycle milestone 119 already measures as the bottleneck. The check itself is a label read and
costs nothing, but it is one more thing that must report before anything can merge, and a check that
fails to report is indistinguishable from one that failed.

**Blocked on nothing.** `merge-drain.sh` works as reduced. This decides whether the holding rule
lives in a script's restraint or in the platform, which is a question about where authority is
recorded rather than about throughput.
