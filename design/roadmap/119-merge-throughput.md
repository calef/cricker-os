# 119. The merge queue is the bottleneck, and the long pole is one prover

**Status: PARTIAL** 2026-08-14. Minted 2026-08-05 by Chris, after an evening in which the constraint
stopped being how fast lanes produce and became how fast one queue can land.

**Gate: NONE.** The measurement and the sharding need nothing. The two structural options at the end
are Chris's, and the block says which, but neither blocks a start.

**Built:** the per-crate cost measurement, `script/verify --shard k/n` balanced by measured time,
and two concurrent shards in `verify.yml` behind an aggregate job that preserves the required
check name. Serial 30.3 min to 15.1 min.

**Not done, and it is the block's own definition of done:** the after-median. A before-and-after
wants median merge-cycle wall clock over a *run* of pull requests, and only one sample exists on
the new path. Take it from the next several merges rather than from the first.

**Two corrections this milestone's own measurement made to it**, both recorded in the sharding
section below: four shards buy nothing over two, because `glob` is atomic at 15.0 minutes; and
the floor is not "the slowest harness, minutes on its own" but a specific 10.8-minute one whose
cost is an unwind bound rather than a CI problem.

## The measurement, taken 2026-08-05

Ten pull requests open, every one of them work that was finished and gated on a developer's machine
before it was opened. They land **one at a time**, because §73's require-branches-up-to-date rule
means merging any one of them stales every other.

Per-check wall clock on a full run:

| check | time |
|---|---|
| **`verify` (Kani proofs)** | **28 to 36 minutes** |
| `cpu matrix` (riscv64 across QEMU models) | ~6 minutes |
| `fuzz` (parsers) | ~5 minutes |
| `build + test` (host + QEMU) | ~3 minutes |
| everything else | under a minute each |

So a merge cycle is **the Kani job plus noise**, and ten of them is most of a day. The lanes that
produced that work took about an hour each and ran in parallel; the queue that lands it is serial and
runs at one prover's pace. **That is the whole bottleneck in one sentence.**

## Why the obvious levers are already pulled

**Scoping already works.** `script/verify --affected-since` computes whether a change can reach a
proof at all, from the dependency closure rather than a path list. Measured the same evening:
documentation-only branches finish `verify` in **11 to 20 seconds**; only changes that can actually
reach a harness pay the half hour. A further fix landed for `script/` entry points, which belong to no
crate and were falling through to run-by-default.

**Dropping `verify` from the required checks was considered and refused** (§73). The proofs are the
thesis; a demonstrator whose headline claim is machine-checked verification does not stop gating on
it to merge faster.

**Parallelism inside the job is already at its ceiling.** `VERIFY_JOBS=2`, and that is a measurement
rather than caution: four concurrent CBMC formulas at `glob`'s size exceed the runner's 16 GB, and the
first four attempts were memory-killed about fifteen minutes in, reported as a bare "operation was
canceled" with orphaned `cbmc` processes.

## The lever that is not pulled: shard the proofs

`VERIFY_JOBS` is capped by **one runner's memory**. Sharding is capped by nothing, and **this tree
already does it**: `.github/workflows/mutation.yml` runs four shards, each re-establishing its own
baseline, because the same wall-clock problem appeared there first.

`script/verify` invokes `cargo kani` per crate over nineteen crates, so the split is natural and needs
no new concept. Four shards of two jobs each is eight concurrent formulas across four 16 GB runners
rather than eight on one, which is the arrangement that was memory-killed. **Expect the long pole to
become the single slowest harness**, which is `glob`'s and is minutes on its own, so the floor is real
and should be measured rather than assumed.

Balance matters and should be measured, not guessed: nineteen crates split four ways by *crate count*
will be lopsided, because `glob` and `calendar` dominate. Shard by measured time.

## Two structural options, and both are Chris's

**A merge queue.** GitHub's feature tests each pull request against the *projected* trunk and can
batch, so N pull requests cost one test cycle instead of N. It is the feature designed for exactly
this failure, and with a half-hour prover the batching is the whole argument. The cost is a more
complicated merge path and a new failure mode to learn (a batch that fails has to be bisected).

**A self-hosted runner.** `cordoba` exists and has 23 GB, which fits `VERIFY_JOBS=4` where the hosted
16 GB fits two. That halves the job without sharding, at the cost of owning a runner and its security
posture, which for a public repository accepting outside pull requests is a real decision rather than
a configuration.

**Recommendation: shard first**, because it needs no decision, no new infrastructure and no new
failure mode, and because it is the tree's own established pattern. Measure what it buys before
spending either of the two options above.

## What "done" means

A recorded before-and-after: median merge-cycle wall clock over a run of pull requests, not a single
sample, and the honest note that a queue drains at the pace of its slowest *required* check no matter
how the work inside it is arranged.

## Scope note

**Not "make CI faster" in general.** `fuzz` at five minutes and `cpu matrix` at six are not the
bottleneck and should not be touched by this milestone; a lane that arrives at "everything got a
little quicker" has diluted it.

**Not a change to what is gated.** The required list stays as §73 settled it. This milestone makes
the same guarantee arrive sooner, and if it ever proposes weakening the guarantee to do so, that is a
different milestone and Chris's decision.

**The honest limit**: the serialization is §73's rule, and no amount of sharding removes it. Ten pull
requests will still land one after another; each one just costs less. Only a merge queue changes the
shape, which is why it is named here rather than deferred.
