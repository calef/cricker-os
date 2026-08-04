# 113. The proofs' own unsafe code is ungated

**Status: NOT-STARTED.** Raised 2026-08-04 by milestone 82's lane, recorded in
`notes/unsafe-obligations.md`'s BUGS as the second of three things neither unsafe lint can reach.

**Gate: NONE.** Fourteen blocks is the whole population, and the block asks for both candidate
gates to be measured over the tree before either is proposed.

**The finding.** `cfg(kani)` is set by the model checker and by nothing else. `script/lint` never
compiles those modules, so `clippy::undocumented_unsafe_blocks` and `unsafe_op_in_unsafe_fn` cannot
fire inside them at all. The tree has **14 `unsafe {}` blocks under `#[cfg(kani)]`**, in
`crates/intrusive` and `crates/ipc`. `intrusive`'s two both carry SAFETY comments.
**Eleven of `ipc`'s twelve do not**, and the gate has never said so.

The one exception worth naming is `ipc`'s `seed`, which is an `unsafe fn` under `#[cfg(kani)]`: it
is the single `unsafe fn` in the tree whose blocks clippy does not reprove on every run, and the
survey found it by enumerating rather than by the gate complaining.

**Why this is worse here than it would be elsewhere.** This is the verification path. The project's
whole method is pure logic in host-testable crates plus machine-checked proofs, and the unsafe code
that sets up a proof is the code deciding what the proof is *about*. An undocumented invariant in a
harness is an unexamined assumption inside the thing that exists to examine assumptions.

**What the lane did not do, on purpose, and it is the right call to repeat here.** It did not write
eleven SAFETY comments. `DECISIONS` §61 records the failure mode: a generated pass produced a
comment that was false at its first site, and `undocumented_unsafe_blocks` checks that a comment
exists, never that it is true. Eleven comments no gate compiles would satisfy a reader and stop
nothing, and the next harness added would skip them again. **The fix is a gate, and the comments
follow from it.**

**Two candidate gates, and the milestone should measure before choosing.**

1. **A clippy pass with `--cfg kani`**, added to `script/lint`. Cheap, runs on every gate, and does
   not need Kani installed. The risk is that `cfg(kani)` code is written against Kani's intrinsics
   (`kani::any`, `kani::assume`), so a plain clippy invocation may not compile it at all without a
   shim, and a gate that cannot build is not a gate.
2. **`-D warnings` on the `script/verify` build**, which already compiles with `cfg(kani)` set by
   definition. Correct by construction, and it only runs when someone runs the proofs, which is not
   every commit.

**Measure what each would find before proposing which**, which is the discipline §61 exists to
enforce: run both over the tree, read the hits, and record the counts in the milestone the way
`Cargo.toml` records the counts that killed three lints. A number decides this, not an argument
about which is more principled.

## Scope note

**Fourteen blocks is the whole population**, which makes this bounded and makes the gate the only
interesting part. If the gate lands and finds eleven, writing eleven true comments is an afternoon;
the value is that the twelfth harness cannot arrive without one.

**Related and distinct: milestone 112 (the SAFETY comments that bind nobody).** 112 is about
obligations the type system does not carry; this is about code the gate does not see. They share an
origin (milestone 82's survey) and nothing else, and neither blocks the other.

**Do not widen this into "gate every cfg".** `cfg(kani)` is the case with evidence: 11 undocumented
blocks in the code that backs the verification claim. Other cfg-gated code in the tree is compiled by
one of `script/lint`'s thirteen configurations, and milestone 82's survey already found the
fourteenth that is not (`-p user -p user_rt` for riscv64, which the gate builds for aarch64 only).
That one is worth fixing and it is a different bug.
