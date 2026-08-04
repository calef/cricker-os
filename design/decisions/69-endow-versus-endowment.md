# 69. `Endow` is a verb, and names the same idea as `Endowment`

**Status: PROPOSED.** (raised 2026-08-04; waiting on Chris, who names things.)

**What.** `supervision_proto::Endow` (what a child is handed at construction) and
`grant_plan::Endowment` (what the shell computed a child should receive) name one idea a
construction step apart, in two crates, with nothing stating the relationship. `Endow` is a verb
where the naming tenet says noun, and its exception is reserved for terms of art, which this is not.

**Size.** 61 references across the tree.

**Options.** `ChildEndowment` (parallel to `BootEndowment`, says whose); `Construction` (says when,
and it is what `supervision_proto` builds from); leave it and record why.

**Recommendation.** Rename, and fold it into milestone 98, which already exists to settle a naming
inconsistency in one deliberate pass rather than a drive-by.

**Blocked.** Nothing today. It grows slowly with every new `Endow` literal.
