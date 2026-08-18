# 97. Six gates run on every pull request and none of them can stop one

**Status: PROPOSED.** Raised 2026-08-18 by the maintainer, from a red trunk rather than from a
worry: `main` failed `script/fastpath-footprint` for several hours and the mechanism that should
have prevented it had been disabled by configuration since before anyone looked.

**What is blocked: nothing, and that is the problem.** Merges continue either way. What is at stake
is whether a gate this tree wrote means anything.

## What was measured

The ruleset on `main` requires **seven** status checks:

`build + test (host + QEMU)` · `rustfmt` · `clippy` · `verify (Kani proofs)` ·
`bench (icount regression tripwire)` · `coverage (host crates)` · `supply chain`

**Six more run on every pull request and cannot block a merge**: `fastpath footprint`,
`cpu matrix (riscv64 across QEMU CPU models)`, `fuzz`, `stack frames`, `verify scope`, and `prove`.

The live case: PR #316's `fastpath footprint` check **failed**, the merge queue merged it anyway,
and `main` went red on a gate nobody had been told was advisory. riscv64's `syscall_entry` had grown
8.2% past its bound, for a good reason that nobody recorded, because the mechanism that exists to
force the recording could not fire.

## Why this is worse than an ordinary red

**It is a rung-two gate demoted to rung zero by configuration** (AGENTS.md's ladder), and the
demotion is invisible from where the work happens. A lane sees a red check on its own pull request.
The queue does not care. Nothing anywhere tells the lane which of those two is authoritative, so the
honest lane wastes an afternoon and the incurious one is right.

That is the same shape as every failure AGENTS.md records: a fact that exists only in a place nobody
reads, here a ruleset page in GitHub's settings rather than a report or a call site.

## The two mechanisms disagree about what an advisory check means, in opposite directions

Measured 2026-08-18, after the red above, and this is the part that makes the current arrangement
actively expensive rather than merely unenforced.

**GitHub's merge queue ignores a failing advisory check and merges anyway.** That is how `main` went
red: #316's `fastpath footprint` failed and nothing stopped it.

**`scripts/merge-drain.sh` refuses to enqueue a pull request with *any* failing check**, required or
not. Its own comment says why, and the reasoning is sound in isolation: *"the queue ejects what fails,
and nothing here should retry it and burn CI."*

So the same red check means "merge it" to the queue and "do not touch it" to the drain. The effect is
not theoretical: within an hour of #323 fixing the baseline, **three pull requests (#319, #321, #325)
sat stalled** with the drain printing `STALLED. #N is failing fastpath footprint` for each, because
each had branched before the fix and inherited a failure that no longer existed on `main`. Every one
of them was mergeable and none of them was moving.

**Neither behaviour is wrong on its own; having both is.** A tree where the enforcing mechanism does
not block and the non-enforcing one does is one where nobody can predict what a red check costs, and
the answer changes depending on which robot looks first.

Whichever way option 1 goes, the drain's filter should be narrowed to the **required** set, so that
one list decides. If the six become required, the narrowing is a no-op and the drain keeps working.
If they stay advisory, the narrowing is what stops a report-only check from silently holding the
queue.

## The options

**Sharpened 2026-08-18 by calef, who refused the first version of this section and was right to.**
His question was the one that matters: *if we want a proven OS, how do we detect when we are no
longer proven?* Answering it properly required reading `.github/workflows/verify.yml` rather than
reasoning about it, and the reading moved two of the six and corrected the reason for a third.

1. **Require four** (recommended): `fastpath footprint`, `stack frames`, `cpu matrix`, `fuzz`.
   None carries a job-level `if:`, so each runs and reports on every change and none can leave a
   required context unreported. The only thing that changes is whether their failure is
   authoritative.

2. **`fuzz` moved into that list** and the first version of this section was wrong to exclude it.
   The argument against was that a 60-second-per-target run is a sample rather than an exhaustive
   search. True, and it does not follow: **a fuzz red is never a false positive.** `cargo-fuzz`
   goes red when it has an input that crashes; the sampling makes the *timing of discovery*
   nondeterministic, not the finding. The two were being conflated. Its steps are guarded rather
   than its job, with a comment saying exactly why, so it is structurally safe to require. The
   residual cost is honest and small: a red can hold a change hostage to a pre-existing crash the
   fuzzer surfaced on that run rather than one that change introduced, which is true of every gate
   on a shared trunk.

3. **`verify scope` comes OUT of the list, as redundant.** The aggregator below already refuses to
   report green when the scope job did not succeed, so requiring it separately buys nothing.

4. **`prove` stays unrequired, and the first version of this section gave the wrong reason.** It
   said `prove` is the queue's long pole. That is true and it is not the argument. The argument is
   that **requiring it would wedge the repository**, permanently, on exactly the changes that need
   it least:

   `prove` is a matrix job whose name is a template, `prove (shard ${{ matrix.shard }}/2)`, and the
   matrix only expands **if the job runs**. It carries `if: needs.scope.outputs.needed == 'true'`,
   so on a change that cannot reach a harness GitHub never expands it and emits one placeholder
   check under the raw, uninterpolated name. Measured on two live pull requests the same afternoon:

   | pull request | contents | the `prove` check's name |
   |---|---|---|
   | #327 | a `design/decisions/` edit, docs only | `prove (shard ${{ matrix.shard }}/2)`, skipping |
   | #322 | kernel and crate code | `prove (shard 1/2)` and `prove (shard 2/2)`, pending |

   A required context is a **string**, and an unreported one is not failed but *unsatisfied*, which
   has no timeout. So requiring `prove (shard 1/2)` blocks every documentation change forever,
   needing a ruleset override to land a typo fix. Under the merge queue it is worse than a block:
   a group that can never satisfy a required context is **evicted and rebuilt forever**, which is
   the loop that already happened here on 2026-08-15 when a lint rejected GitHub's own synthetic
   branch names.

5. **Require none, and say so out loud.** The floor rather than an option to prefer: one line naming
   which checks are advisory, so a lane meeting a red check knows whether it is looking at a blocker
   or a note. Strictly better than the status quo, which is this arrangement with the sentence
   missing.

## The proofs are already gated, which answers the question that prompted all of this

`verify.yml` is three jobs and only the third is required. `verify scope` decides whether anything in
the change can reach a harness; **`prove`** does the proving, sharded two ways; and
**`verify (Kani proofs)`** is an *aggregator* that reads the other two and exits 1 unless every shard
reports `success` or `skipped`:

```sh
case "$prove" in
  success) echo "==> every shard proved its crates" ;;
  skipped) echo "==> nothing in this change can reach a proof" ;;
  *)       echo "==> a shard reported '$prove'" >&2; exit 1 ;;
esac
```

It has a fixed name and `if: always()`, both load-bearing, and its own comment names the failure it
exists to avoid. **So a broken proof already fails a required check**, and the detection calef asked
after is present rather than missing. The aggregator is the pattern that converts a
variable-shaped upstream into one stable required context, and it is the answer for any future job
shaped like `prove`.

## The follow-on: nothing checks that the aggregator watches every proving job

**This is the real weakness, and it is the one worth spending on.** The aggregator's coverage is a
hand-maintained `needs: [scope, prove]` list plus a shell `case`. Add a third proving job tomorrow,
forget to add it to `needs:`, and **the required check reports green while the new job burns**. The
proofs would stop being gated and the gate would keep saying they were, which is a worse state than
the advisory checks this section is about: an advisory check that fails is at least visible.

It is the same shape as the rest of this section one level in. `prove` is protected by structure (a
matrix that expands or does not); the aggregator's *coverage* is protected by somebody remembering,
which is rung four and is what AGENTS.md calls rung zero when written honestly.

The mechanism is cheap and is not built: parse `.github/workflows/verify.yml`, find every job whose
steps run `script/verify`, and fail if any of them is absent from the aggregator's `needs:`. That is
a `script/lint` check over one YAML file with no network and no GitHub API, which is the same shape
as the branch-prefix and counted-claims checks already there. **It should be built whichever way the
ruleset question is answered**, because it protects a property that is true today rather than
proposing a new one.

## What decides it

This is calef's because it changes **what may merge**, which is merge authority rather than tooling.
It is also cheap to reverse (a ruleset edit), so it does not want a long deliberation, and the
*move fast on what can be undone* tenet applies: the expensive part is not the configuration, it is
the hours of trunk-red that the current arrangement will keep producing while nobody decides.

## BUGS

- **This section names six advisory checks as of 2026-08-18 and nothing keeps that list current.**
  A check added to CI is advisory by default, so the list grows silently in the direction of less
  enforcement. A gate comparing the workflow's job names against the ruleset's required contexts is
  conceivable and is not built.
- **This section's first version got `fuzz` and `prove` wrong**, and was corrected only because
  calef refused it and asked how we would detect being no longer proven. Both errors were the same
  error: reasoning about CI from the shape of the check names instead of reading the workflow. The
  corrected text is above; the failure is recorded here because a section that reads as though it
  were right the first time teaches the wrong method.
- **The follow-on above is unbuilt.** Until it exists, "the proofs are gated" rests on the
  aggregator's `needs:` list being complete, and nothing checks that it is.
