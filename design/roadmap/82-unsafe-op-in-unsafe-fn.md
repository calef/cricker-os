# 82. `unsafe_op_in_unsafe_fn`: the obligation moves inside the fn

**Status: NOT-STARTED.** Raised 2026-08-03, same survey as 79.

**Gate: NONE.** A bounded burn-down of 33 `unsafe fn`s: the fixes are the milestone and the
one-line `[workspace.lints.rust]` addition lands last.

An `unsafe fn` body is one implicit unsafe block, so a function with three distinct unsafe
operations carries three distinct invariants under a single signature, and milestone 68's
`undocumented_unsafe_blocks` lint cannot see any of them: it fires on blocks, and there are no
blocks. The lint `unsafe_op_in_unsafe_fn` removes the implicitness, each interior operation gets an
explicit `unsafe {}` block, and each block then owes the SAFETY comment the existing lint enforces.
The two lints compose into the property this kernel actually wants: **every unsafe operation sits
next to the written invariant that makes it sound**, whether or not its enclosing fn is unsafe.

The tree has 33 `unsafe fn`s across `kernel/`, `crates/`, and `user/`, so this is a bounded
burn-down, not a campaign. Per the lint-policy comment in the workspace `Cargo.toml`, adding the
lint is a decision to fix every violation first: the milestone is the fixes, with the one-line
`[workspace.lints.rust]` addition landing last. Rust's 2024 edition makes this lint warn-by-default,
so this is also alignment with where the language is going rather than a house rule.
