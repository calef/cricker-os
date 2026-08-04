# 70. Is milestone 82 BUILT when its finding was "nothing to fix"?

**Status: PROPOSED.** (raised 2026-08-04; waiting on Chris, and the answer sets a precedent.)

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
