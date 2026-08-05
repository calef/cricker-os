# 82. `unsafe_op_in_unsafe_fn`: the obligation moves inside the fn

**Status: BUILT** 2026-08-04 (PR #92). **The premise below was wrong and that is the finding**: zero violations existed, because every owned package is edition 2024 where this lint is warn-by-default and `script/lint` runs `-D warnings`, so it had been a hard gate since the edition bump with nobody having written it down. The lint is enabled explicitly anyway, and the bounded burn-down described below had no items. Raised 2026-08-03, same survey as 79.

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
