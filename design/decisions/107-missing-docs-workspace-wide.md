# 107. `missing_docs` moves to `workspace.lints.rust`, opt-out rather than opt-in

**Status: DECIDED.** calef, 2026-08-22, on a milestone 68 lane's six-questions write-up (pull
request #395): switch to workspace-wide opt-out now.

## The question

Milestone 68's own remaining open policy question: should `missing_docs` move from a per-crate
`#![warn(missing_docs)]` opt-in (23 crates, then 50 of 57 after this lane's burn-down) to
`[workspace.lints.rust]` with an explicit `#![allow(missing_docs)]` per crate not yet ready? The
roadmap doc already named the tradeoff: opt-out is higher on this file's own ladder (the default
becomes on, and the opt-out list is a worklist that can only shrink) and is the only version that
covers a crate created tomorrow, but it inverts a rule §61 itself states: *"adding a lint to this
table is a decision to fix every existing violation first,"* which is why §61 recorded `missing_docs`
as deliberately absent from the table rather than present with `#[allow]`s underneath it.

## Why this is a considered exception to §61, not a quiet reversal of it

**§61's rule still holds; the facts it was applied to changed.** At §61's own writing (milestone 68,
2026-08-03), item coverage was 67-94% and adopting the lint meant a genuine "write everything first"
commitment with no visible end. By 2026-08-22, three burn-down passes had closed the gap from 32
crates to 7, and this lane's own measurement (one workspace-wide `cargo clippy -W missing_docs` into
a clean target, avoiding both traps §61's sibling section already named: `--show-coverage`
undercounting, and cached per-package diagnostics) put it at **404 items across 31 crates**, then
**235 across 7** after the lane's own burn-down. The switch's real cost is 7 `#[allow]` lines, not
32, and not the open-ended commitment §61 was refusing.

**This is written down explicitly rather than left as a silent exception**, per this project's own
rule that an exception must say so where a reader meets it: this decision, cited from
`design/roadmap/68-code-quality-gates.md`.

## The decision

**Move `missing_docs = "warn"` into `[workspace.lints.rust]`.** Every crate with `[lints]
workspace = true` (all 57) now warns by default. The 7 crates with a real remaining worklist
(`isa` 54, `smb_proto` 52, `mdns_proto` 41, `pci` 24, `gpt` 23, `grant_plan` 22, `compositor` 19,
per `notes/doc-coverage.md`) carry an explicit `#![allow(missing_docs)]`, citing this decision and
their own item count, so the opt-out is visible at the crate rather than implied by its absence from
a list. The 50 per-crate `#![warn(missing_docs)]` opt-ins are removed as redundant; the workspace
default now covers what they used to state individually.

**What this buys, per the roadmap doc's own framing**: a crate added tomorrow is checked by
default, closing the gap the doc's own BUGS section named ("nothing requires a new crate to adopt
it"). The opt-out list can only shrink from here, the same ratchet shape §38 already uses for dead
code, now recreated properly for docs instead of being deferred a second time.

## What this does not decide

The 235 remaining items across 7 crates are not written by this decision. They are `notes/doc-coverage.md`'s
tracked worklist, the same as before; this decision only changes which side of the default they sit
on while they are worked down.

## What it unblocks

Milestone 68 flips to **BUILT**: both halves it was PARTIAL on (doc examples, closed 2026-08-17; the
`missing_docs` policy question, closed here) are now resolved. The remaining per-crate worklist is
ordinary ratchet work, not a blocker on the milestone's own status.
