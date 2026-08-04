# 85. Mutation testing over the host crates

**Status: BUILT** 2026-08-04 (PR #76). Raised 2026-08-03, same survey as 79.

Everything below was done and is on `main`: `script/mutation`, a run of **5,551 mutants over 38 host
crates** (92.4% of the viable ones killed), every one of the 391 survivors triaged with a ledger
recording whether it was killed by a new test, proved equivalent, or confirmed a hang,
`.cargo/mutants.toml` carrying each exclusion's reason, `.cargo/mutants-baseline.txt` as the
machine-readable baseline, `notes/mutation-testing.md`, and `.github/workflows/mutation.yml` running
four shards weekly and reporting against that baseline. It is a report, not a gate, as intended.

The coverage job answers "did this line run under a test"; it cannot answer "would any test notice
if this line were wrong", and the second question is the one a test suite exists for. cargo-mutants
answers it by mutating the code and re-running the tests, and the survivors, mutations no test
caught, are a worklist sorted by exactly the property this project cares about. The exhaustive
suites (`ntp_proto`, `gpt`) should score near-perfectly, which is itself a calibration check on the
tool; the interesting results will be in the middle of the tree.

The work: one full, time-boxed run over the host crates; triage every survivor into either a test
worth writing or an exclusion recorded in `.cargo/mutants.toml` with a reason (config, not a code
dependency, per §46); a note recording the baseline; then a weekly scheduled workflow that reports
against that baseline. A report, not a gate, until the weekly numbers prove stable enough that a
new survivor deserves to fail something.
