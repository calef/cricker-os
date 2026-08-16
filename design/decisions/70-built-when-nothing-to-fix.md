# 70. BUILT measures the end-state, and a false premise is rewritten in as the finding

**Status: DECIDED.** calef, 2026-08-15, and the precedent is the point: **a milestone is BUILT
when its block's end-state holds in the tree, however it came to hold.** The status column exists
so a reader can ask "does the tree have this property?", not "how much work did it take?": a
benchmark milestone is BUILT when the numbers exist however easy the run was, and an audit that
finds nothing wrong still happened. The guard that keeps this from laundering wrong premises into
history is mandatory: **the false premise is rewritten into the block as a finding, not a
footnote**, because "we looked and it already holds" is knowledge the tree did not have before
the looking, and it is usually a §76-shaped discovery that the records and the tree disagreed.
Milestone 82's block received exactly this treatment on 2026-08-04, provisionally; this ruling
makes it the rule. The shape recurs with every audit or sweep that finds nothing to fix, and the
answer is now the same each time.

**What.** Milestone 82 asked for `unsafe_op_in_unsafe_fn` to be adopted after fixing every
violation. The lane found **zero** violations: every package we own is edition 2024, where the lint
is warn-by-default, and `script/lint` runs `-D warnings`, so the rule has been a hard gate since the
edition bump with nobody having written it down. The lint was enabled anyway (it costs nothing and
catches a package at an older edition, which `vendor/redoxfs` is).

**The question.** BUILT, or something else? The deliverable landed and the premise was false.

**Recommendation.** BUILT, with the block rewritten to record what was actually true, on the grounds
that the milestone's purpose (the rule is in force and visible) is satisfied. But this is the first
instance of the shape and the answer sets a precedent.

**Blocked.** The roadmap's status row for 82.
