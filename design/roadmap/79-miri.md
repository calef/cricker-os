# 79. Miri over the host crates

**Status: NOT-STARTED.** Raised 2026-08-03, from a survey of what analysis the tree runs against what
it could. Milestones 79 to 85 all come from that survey.

This project's method is pure logic in host-testable crates, and Miri interprets exactly those tests
while checking the rules nothing else here checks: aliasing, pointer provenance, uninitialized reads.
Kani proves the properties it is asked about; fuzzing sees crashes; clippy sees shapes. None of them
sees a `&mut` that aliases, and in a tree with 224 `unsafe` occurrences under `crates/` that class is
live. The pinned nightly already ships Miri as a rustup component, so the toolchain cost is one line
in `script/bootstrap`.

The work: a `script/miri` front door delegating to `cargo xtask miri`, which runs `cargo miri test`
over the host-testable crates. The first full run is most of the milestone: triage every finding,
fix what is real, and record what is not in the note this milestone writes.

## Scope note

Miri is an interpreter, roughly two orders of magnitude slower than native. The exhaustive suites
(`ntp_proto` runs its entire 10^9-value domain, `gpt` does 460,000 table validations) cannot run
under it as-is; the honest treatment is to exclude or sample them and say so, because "Miri-clean"
then means "the sampled paths are clean". Cadence is a weekly scheduled workflow plus on-demand,
not per-PR. `-Zmiri-strict-provenance` is a later ratchet to consider once the default run is clean.
