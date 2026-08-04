# 94. The untracked-work sweep, and the convention that ends the category

**Status: NOT-STARTED.** Raised 2026-08-03 by Chris, completing 92 and 93's family: those keep
claims true; this one finds the work the tree has already identified but never gave a home, and
then changes the working conventions so the category stops refilling.

**The failure mode, with today's near-miss as the exhibit.** Work gets identified in places that
evaporate: a lane's final report ("worth a fix lane someday"), a pull-request comment, a commit
message, a "follow-up, not built" aside in a decision block. Milestone 90 exists only because
Chris happened to catch milestone 84's guard-page finding in a report and said "add a milestone";
had he been away that day, the finding would be sitting in a merged PR's description, which
nobody rereads. The BUGS convention is *not* the failure mode and this milestone must not damage
it: a recorded limitation living next to its feature is the FreeBSD posture working as designed.
The defect is narrower: work someone actually intends, resting in a medium nobody will search.

**Measured, as a floor not a count:** 11 TODO-class markers in the Rust tree, and 29 notes
containing "someday", "follow-on", "not built", or "deferred" phrasing. The real surface is
larger (agent reports, DECISIONS follow-up asides like §26's signature variant, roadmap blocks'
stretch items), which is what the sweep is for.

**Deliverable one, the one-time sweep.** Code (TODO/FIXME class), notes (the deferral phrasings,
BUGS entries that are actually intended work rather than recorded limitations), DECISIONS
follow-up asides, and the roadmap's own stretch/later items. Every found item ends in one of the
family's states: **minted as a milestone** (integrator numbers at merge), **already tracked**
(link the milestone it lives in; dedupe, do not double-mint), or **recorded-accepted where it
sits** (a deliberate limitation, staying in BUGS on purpose, with the sweep's blessing recorded
so 93's audits do not re-litigate it). "Noted" is not a state.

**Deliverable two, the conventions, drafted by this milestone and landed in CLAUDE.md by Chris**
(a lane does not edit that file; the milestone's output is the proposed text):

1. **A lane's identified work survives the lane.** A final report may name new work only in one
   of the tracked forms: a proposed milestone (provisional, integrator mints), or an explicit
   BUGS record placed where a reader meets the limitation. "Worth doing someday" with no home is
   the integrator's cue to stop the merge until it has one.
2. **The integrator's merge checklist grows one line**: before a lane's branch merges, every
   piece of identified work in its report has a home, in the same breath as the worktree prune
   and the branch deletion.
3. **TODO markers in code cite their home or do not exist**: script/lint gains a check that a
   TODO/FIXME names a milestone (`TODO(milestone N):`) or fails. Eleven existing markers get
   swept into compliance as part of deliverable one. A TODO with a home is a cross-reference; a
   TODO without one is exactly the evaporating medium this milestone exists to drain.

## Scope note

One-time by design: the sweep clears the backlog, the conventions stop the refill, and the
*recurring* duty lands with 93's audits (whose sweeps will meet deferral phrasing in docs) and
the lint gate (which meets it in code). If the sweep's minting produces a burst of small
milestones, that is the mechanism working, not scope creep; the index absorbs them the way it
absorbed 79 through 85 in one day. Convention text is drafted here, decided by Chris, and its
numbers in CLAUDE.md are his to place.
