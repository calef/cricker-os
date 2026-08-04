# 85. Mutation testing over the host crates

**Status: NOT-STARTED.** Raised 2026-08-03, same survey as 79.

**Gate: NONE.** One time-boxed run over the host crates, triage of every survivor, a recorded
baseline, then a weekly workflow. A report rather than a gate, which is what keeps it from blocking
anything.

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
