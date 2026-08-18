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

1. **Require four of the six** (recommended): `fastpath footprint`, `stack frames`, `verify scope`,
   `cpu matrix`. All four are deterministic, all four already run on every pull request, and the
   only thing that changes is whether their failure is authoritative. Cost is real but bounded:
   `cpu matrix` is the slowest of the four and it is already on the critical path for anything
   touching riscv64.
2. **Require all six.** Refused, for a different reason each. **`fuzz` is time-boxed at 60 seconds
   per target rather than exhaustive**, so a red is evidence and a green is not proof; making it
   blocking makes every merge hostage to a sampling run and teaches people to re-run it until it
   passes, which is worse than not having it. **`prove` is already the queue's long pole**, and
   AGENTS.md says a group's CI goes green while `verify` is still running, every time; requiring it
   before milestone 119's remaining half measures that would set queue throughput by accident.
3. **Require none, and say so out loud.** Cheapest, and not dishonest as long as it is written: one
   line in AGENTS.md or `notes/merge-queue.md` naming which checks are advisory, so a lane reading a
   red check knows whether it is looking at a blocker or a note. This is strictly better than the
   status quo, which is the same arrangement with nothing written down.

**The recommendation is 1, and 3 is the floor.** What is not acceptable is what we have, because it
is 3 without the sentence.

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
- **`prove` and `verify (Kani proofs)` are different checks and only the second is required**, which
  is easy to misread as "the proofs are gated". What is gated is the harness run, not the shard
  matrix.
