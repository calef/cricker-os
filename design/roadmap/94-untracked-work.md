# 94. The untracked-work sweep, and the convention that ends the category

**Status: BUILT** 2026-08-18. Deliverable one landed 2026-08-04 (PR #91, with the minting in PR #94)
in all three of its parts: the sweep ran, the TODO lint landed, the inventory is
notes/untracked-work-sweep.md, and each of the nine recorded-accepted findings carries its blessing
in the paragraph a reader meets the limitation in. **Deliverable two waited fourteen days for one
edit a lane may not make.** Its conventions were drafted in this block on 2026-08-04 and could not be
landed from a lane, because a lane does not edit AGENTS.md; nobody staged them for calef until
2026-08-18, when the maintainer put the drafted text up as a pull request and he approved it. The
text below is what landed, verbatim.

**That delay is this milestone's own failure mode, for the second time.** Deliverable one's inventory
sat in a pull request body for twelve days until the item-level list was gone and had to be
re-derived; deliverable two sat in a roadmap block for fourteen, correctly filed and unactionable,
because the block recorded *what to do* and nothing recorded *who could do it*. A proposal that only
its author may enact is one assignment short of tracked. Raised 2026-08-03 by calef, completing 92 and 93's family: those keep
claims true; this one finds the work the tree has already identified but never gave a home, and
then changes the working conventions so the category stops refilling.

**What the sweep measured, kept because the numbers are the argument:** 11 TODO-class markers and 29
notes with deferral phrasing were this block's predicted floor, and the sweep corrected it to 11 hits
of which 2 were real markers. `git grep -w TODO` is 82% false positives on this tree, which is what
shaped the lint rather than a threshold anyone chose.

**Two of the three parts of deliverable one were finished twelve days late, by this milestone's own
failure mode.** The inventory sat in PR #91's description and the nine blessings sat in a lane
report, both of which are the evaporating medium the block was written to drain, and neither is
readable a fortnight on. The consequence was not hypothetical: the item-level list of the nine did
not survive, so the nine now in the tree are the sweep's own rule applied a second time and the
note's `BUGS` section says so. **A milestone that swept the tree for work with no home left its own
findings without one**, which is worth more as evidence than the sweep is as a deliverable.

**The failure mode, with today's near-miss as the exhibit.** Work gets identified in places that
evaporate: a lane's final report ("worth a fix lane someday"), a pull-request comment, a commit
message, a "follow-up, not built" aside in a decision block. Milestone 90 exists only because
calef happened to catch milestone 84's guard-page finding in a report and said "add a milestone";
had he been away that day, the finding would be sitting in a merged PR's description, which
nobody rereads. The BUGS convention is *not* the failure mode and this milestone must not damage
it: a recorded limitation living next to its feature is the FreeBSD posture working as designed.
The defect is narrower: work someone actually intends, resting in a medium nobody will search.

**Measured, as a floor not a count** (predicted before the sweep, and the first half was wrong in
the useful direction: 11 hits, 2 markers): 11 TODO-class markers in the Rust tree, and 29 notes
containing "someday", "follow-on", "not built", or "deferred" phrasing. The real surface is
larger (agent reports, DECISIONS follow-up asides like §26's signature variant, roadmap blocks'
stretch items), which is what the sweep is for.

**Deliverable one, the one-time sweep. Done; the record is notes/untracked-work-sweep.md.** Code
(TODO/FIXME class), notes (the deferral phrasings, BUGS entries that are actually intended work
rather than recorded limitations), DECISIONS follow-up asides, and the roadmap's own stretch/later
items. Every found item ends in one of the
family's states: **minted as a milestone** (integrator numbers at merge), **already tracked**
(link the milestone it lives in; dedupe, do not double-mint), or **recorded-accepted where it
sits** (a deliberate limitation, staying in BUGS on purpose, with the sweep's blessing recorded
so 93's audits do not re-litigate it). "Noted" is not a state.

**Deliverable two, the conventions, drafted by this milestone and landed in AGENTS.md by calef**
(a lane does not edit that file; the milestone's output is the proposed text). Its third convention
**landed as machinery** in PR #91 and is the one that needed no prose:

3. **TODO markers in code cite their home or do not exist**: `script/lint` gains a check that a
   TODO/FIXME names a milestone with a block, or fails. **Done.** A marker is the word with its
   punctuation attached; a mention is not, and markdown is exempt because prose explaining the
   convention has to spell the shape it forbids.

The first two are prose about how lanes and merges work, so they are AGENTS.md's and they are what
remains outstanding. **The text is written out here, ready to paste**, rather than left in a lane
report: a proposal that lives only in a report is the third instance of this milestone's own failure
mode, and two were enough.

### Proposed text, for AGENTS.md

Goes in the roles section, after the paragraph beginning *"A developer's final report ends by
handing off"*, which is the rule this one enforces.

> **Identified work leaves the lane in a tracked form, or the merge waits.** A lane that finds work
> it is not doing may report it in exactly two shapes, and "worth doing someday" is neither. Either a
> **proposed milestone** (provisional, the integrator mints the number at merge like every other
> global name), or a **recorded limitation written where a reader meets the feature**, in the `BUGS`
> section beside it, which is what §71's promotion triggers are then measured against. A finding with
> no home is the integrator's cue to hold the merge until it has one.
>
> This is rung three of the ladder, and it is worth naming why the lower rungs fail here
> specifically. A lane report is read once, by one person, on the day it is written. A pull request
> body is read while the diff is open and never again. Both feel like records while you are writing
> them, which is what makes this the failure that recurs: milestone 90 exists only because calef
> happened to be at his desk the day a report named it, and milestone 94 swept the tree for exactly
> this category and then left its own inventory in a pull request body for twelve days, by which
> point the item-level list was gone and had to be re-derived. See notes/untracked-work-sweep.md.
>
> **The merge checklist grows one line**, in the same breath as pruning the worktree, deleting the
> branch and relinking `nife-dev`: every piece of identified work in the lane's report has a home.

Two things this deliberately does not do. It does not gate: no check can tell an intention from an
observation in prose, and a lint that tried would be `git grep -w TODO`'s 82% false-positive rate
wearing a different hat. And it does not touch the `BUGS` convention, which is the FreeBSD posture
working as designed; the whole point is to route intentions *into* it rather than out of it.

## Scope note

One-time by design: the sweep clears the backlog, the conventions stop the refill, and the
*recurring* duty lands with 93's audits (whose sweeps will meet deferral phrasing in docs) and
the lint gate (which meets it in code). If the sweep's minting produces a burst of small
milestones, that is the mechanism working, not scope creep; the index absorbs them the way it
absorbed 79 through 85 in one day. Convention text is drafted here, decided by calef, and its
numbers in CLAUDE.md are his to place.

## BUGS

- **The nine blessings in the tree are a re-derivation, not the sweep's own list.** PR #91 recorded
  the count and never the items, and nothing else survived. notes/untracked-work-sweep.md carries
  the full caveat and the rule that was re-applied to produce them. Where the two sets differ,
  nobody can now tell.
- **The ten already-tracked findings are a count and not a list.** Same cause. An audit meeting one
  of them will find the block that owns it, which is what the dedupe was for, so the loss costs
  provenance rather than correctness.
- **Nothing gates a blessing.** A limitation can lose the paragraph that blesses it to an ordinary
  edit and no check would notice. This is a marked exception, not an oversight: nine paragraphs do
  not obviously pay for a gate. If the set grows past what a person can hold, it wants one.
- **The work and the record moved in separate commits, and nothing linked them.** `4fbb90cb` landed
  the inventory and the nine blessings without touching this block or the index, so both records
  went on claiming the work was outstanding while agreeing with each other. That is §76's shape and
  it is invisible to `script/roadmap --check` by construction: the gate compares the index against
  the block, never either against the tree. Milestone 93's audits are the mechanism for this class.
