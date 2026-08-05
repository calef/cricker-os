# 76. What catches a milestone status that is wrong in both places?

**Status: PROPOSED.** (raised 2026-08-04 after nine milestones were found misrecorded in one sweep.)

**What.** `script/roadmap --check` verifies that `design/roadmap/README.md` and the milestone's own
file agree. That was built on 2026-08-03 to catch the split-brain defect, and it does. **It cannot
see a status that is wrong in both places**, because the tree is a third record and nothing compares
against it.

**The cost, measured.** On 2026-08-04 nine milestones (81, 82, 85, 94, 96, 100, 101, 109, 110, 113)
had merged implementation PRs while both records read `NOT-STARTED`. `script/roadmap --ready` is the
list work is picked from, so it offered finished work: a developer spent its entire budget re-running
mutation tests for milestone 85, which had merged hours earlier. The gate was green throughout.

**Two candidate signals, both measured rather than assumed.**

- **A `milestone N:` commit subject on `main`.** Purely local, no network, cheap. **Recall is poor**:
  23 milestones have such a commit and 35 of 54 BUILT milestones do not, because the work often lands
  under a descriptive subject and the merge commit carries the PR title. It would have caught none of
  the nine.
- **A merged PR titled `milestone N...`.** Caught **all nine**. Needs `gh` and the network, so it
  cannot live in `script/lint`; it would be a CI-only check, or a periodic one the steward runs.

**The recommendation.** A CI-only check, because the signal that works needs the network and CI has
it: fail (or warn) when a milestone reads `NOT-STARTED` and a merged PR names it in the
implementation form. Pair it with the convention that **the integrator sets status in the merge**,
but do not rely on the convention alone, since milestone 92's own argument is that a practice living
in memory gets skipped exactly when it matters, and this one was skipped nine times in one day.

**The argument against.** Both signals key on a **title convention** that nothing enforces, so the
check is only as good as how people write PR titles, and a lane that titles its PR differently is
invisible to it. That is a real weakness and it is the same class of weakness as
`script/decisions --check`'s: it verifies a citation resolves to *some* section, never the right one.
A check that is right most of the time is still worth more than the nothing that exists now, but it
should be honest in its own output about what it cannot see.

**Not blocked.** Nothing waits on this; the nine records are corrected. It decides whether the tenth
is caught by a machine or by another sweep.
