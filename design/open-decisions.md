# Open decisions

*Decisions waiting on Chris. One entry each: what is being decided, the options, the recommendation
with its reason, and what is blocked until it is answered. Started 2026-08-04, because five of these
had accumulated in a conversation's scrollback in one day, which is the medium milestone 94 exists to
abolish. Answered entries move to the record they belong in (`DECISIONS.md` for design, the
milestone block for scope, the commit for a name) and are deleted from here; this file is a queue,
never an archive.*

## 1. `BootEndowment::unused` wants a truer name

**What.** The field holds the capabilities aarch64's boot path is handed because it shares that path
with milestone 19d's test roles: a report endpoint and a test SGI. The interactive system never uses
them and deletes them with the device authority.

**The problem.** They are not unused. The test roles use them. "Unused" is true only from the
interactive system's point of view, which is one of the two callers.

**Options.** `not_ours` (states whose they are, from the caller's side); `test_roles_only` (states
whose they are, absolutely); leave `unused` and let the doc comment carry the nuance.

**Recommendation.** Something possessive. The field's whole job is to say "these arrived for someone
else", and a name that says so removes the need for the reader to hold the doc comment in mind.

**Blocked.** Nothing. Cosmetic, and cheap while `crates/system_initializer` is one day old.

## 2. `Endow` is a verb, and names the same idea as `Endowment`

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

## 3. Is milestone 82 BUILT when its finding was "nothing to fix"?

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

## 4. Does a recorded limitation ever become a `RECORDED` row?

**What.** The roadmap has a `RECORDED` status for "analysis captured, decision deliberately not
taken" (milestones 39 and 52). Separately, the BUGS convention records limitations next to their
features, and milestone 94's sweep blessed nine of them so a future doc audit does not re-litigate
them. Some of those sit close to `RECORDED`'s shape, notably §26's signature variant.

**The question.** Is a recorded limitation ever promoted to a roadmap row, and if so, what promotes
it? Surfaced by the sweep, which did not decide it.

**Recommendation.** None yet; this wants the convention thought through rather than a snap answer.

**Blocked.** Nothing, but it will recur at every audit under milestones 92 and 93.

## 5. Whose clock does `time` need? (the design the lane argued against)

**What.** Milestone 86 shipped `time` reading the shell's clock capability, as its block specified.
The lane then argued in a BUGS entry that a duration does not need a clock at all: wall clock is
`offset + counter`, the offset cancels across a command, and the counter is ambient
(`user_rt::monotonic_nanos`, two register reads, no syscall), so `end - start` reduces to a counter
difference any process could take.

**The trade.** A counter-only `time` needs no capability, cannot be refused, and is *immune* to a
mid-command clock step. The clock version buys a wall-clock number and the ability to notice a step
(the shell reads `clock_proto`'s generation at both ends), and costs refusing to measure on a
machine with no believable clock.

**Recommendation.** Worth reopening. The lane implemented the block rather than diverging
unilaterally, which was right, but its objection is the stronger argument on the merits.

**Blocked.** Nothing shipped depends on it. Revisiting is cheap: read `monotonic_nanos` at both
ends, delete two `Untimed` arms, and the clock wiring below it stops being needed.

## 6. Milestone 44's ten admin minutes, which only Chris can spend

**What.** Enable private vulnerability reporting (the committed `SECURITY.md` points at a button that
does not exist), and apply the `main` ruleset with its required checks, an empty bypass list, and
**not** linear history. Exact steps are in notes/repo-hardening.md.

**One caveat that moved today.** The check names changed: `miri` is now `undefined-behavior check`,
and the HVF leg is deliberately **not** a CI check (GitHub's hosted macOS runners have no nested
virtualization), so it must not appear in the required list.

**Blocked.** Milestone 44 stays PARTIAL until this is done.
