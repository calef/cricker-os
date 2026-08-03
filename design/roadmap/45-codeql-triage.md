# 45. Triage the CodeQL code-scanning alerts, and decide what the tool is for

**Status: BUILT.**

**Built 2026-07-30. All nine alerts are fixed, and the policy is DECISIONS §35.** The seven
`actions/missing-workflow-permissions` went first: every CI job held a `GITHUB_TOKEN` with permissions
it never used, which is an odd default for a project whose thesis is that a component holds the
authority its job needs and nothing more.

The two `rust/access-invalid-pointer` alerts turned out to be two different findings wearing one label.
**Nullness was structurally fixable and the type was failing to say so**: every pointer entering the
intrusive queues comes from a `&mut Thread`, so non-nullness is a fact of construction rather than a
caller's promise. `Fifo`, `Endpoint` and the `Node` trait moved to `NonNull`, every conversion at every
call site is infallible (`NonNull::from`, never `NonNull::new(..).unwrap()`), and `Option<NonNull<T>>`
is the same size as `*mut T` through the niche, so it costs nothing. **Validity and aliasing remain
inexpressible**, which is the design of an intrusive queue rather than a gap, and that reasoning now
lives in the crate's own docs as the standing caveat.

Two things recorded because they were wrong or nearly so. I predicted twice that `NonNull` would improve
the code *without* satisfying CodeQL, reasoning that the rule was about validity generally; it cleared
both alerts, so the rule was more precise than I credited. And I first "proved" that with a query
against `refs/heads/<branch>`, which has **zero analyses**, so it would have returned zero whatever the
code did. The real comparison is `/language:rust`: 2 results on `refs/heads/main`, 0 on
`refs/pull/5/head`, holding across four commits each side.


**In brief.** Nine alerts on first run. Seven (`actions/missing-workflow-permissions`) were fixed immediately by giving every workflow an explicit least-privilege `permissions: contents: read`, which is the right call for this repo specifically: a project whose thesis is that a component holds the authority its job needs and nothing more has no business letting its CI token default to write access it never uses. **The two that remain are high severity and need judgement, not configuration**: `rust/access-invalid-pointer` at `crates/intrusive/src/lib.rs:93` and `:109`, the raw-pointer dereferences in the intrusive wait-queue's `push_back` and `pop_front`. Both already carry `SAFETY` comments citing the queue's caller contract, and `intrusive` is one of the 13 Kani-proved crates, so the question is precisely what CodeQL sees that Kani does not: Kani proves the pure logic under chosen bounds, while the pointer validity here rests on a *caller* contract enforced by convention rather than by the type system. Decide per alert whether it is a true positive worth restructuring for, or a false positive to dismiss **with a written reason**; then set the standing policy for how alerts get triaged, since an alert list nobody dispositions decays into wallpaper

**Why it matters.** **the alerts land exactly where this project's most-used unsafe abstraction lives**, so the answer is worth having either way: either the wait queue's contract can be made structural rather than documented, which is a real improvement to the code every blocked thread passes through, or we write down why it cannot be and what upholds it instead. Also forces the meta-decision milestone 44 left open, now that scanning is actually running: a scanner whose findings are never dispositioned is worse than none, because it manufactures the appearance of review
