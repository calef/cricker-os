# The merge queue, and the two things that watch it

Two scripts, both maintainer tools rather than front doors, both born on 2026-08-04 out of the same
evening's failures. `scripts/merge-drain.sh` lands what does not need calef.
`scripts/trunk-health.sh` says when `main` is red. Names are provisional.

## Why they exist rather than being someone's job

The roles in CLAUDE.md are Maintainer, Developer, Steward. On 2026-08-04 three things went wrong in
one evening and all three were the same shape: **a duty that belonged to whoever happened to notice.**

- **Two green pull requests sat unmerged for hours** because nobody armed auto-merge on them. Not a
  judgement call, not a policy: they were opened and forgotten.
- **`main` went red and nobody owned it.** A developer cannot see `main` by design. The steward
  watched pull request checks and never the trunk. The maintainer's hygiene list is prune the
  worktree, delete the branch, relink `nife-dev`, leave no QEMU, and does not mention it.
- **Merging one pull request staled the other eight** under the new up-to-date rule, and nothing
  picked them back up until calef asked.

The pattern is the one milestone 92 argues about audits: a practice that lives in memory gets skipped
exactly when it matters, and the maintainer is structurally worst at this particular duty because
merging happens *between* conversations rather than during them.

The steward was supposed to cover that and did not, for a reason worth recording: **it reported and
never acted.** "The queue is stalled" arriving in a message is only useful if someone reads the
message and does something. These two scripts act.

## `scripts/merge-drain.sh`

```console
$ scripts/merge-drain.sh --once
merge-drain: STALLED. #213 is failing cpu matrix (riscv64 across QEMU CPU models) (§69 decided: Endow becomes ChildEndowment)
merge-drain: 4 armed, 1 stalled, of 5 unheld

$ scripts/merge-drain.sh            # loop until nothing is left to enqueue
merge-drain: 2 armed, 0 stalled, of 2 unheld
merge-drain: queue empty; nothing open that does not need calef
```

It takes the open pull requests **without** the `needs-architect` label, skips drafts, arms
auto-merge on every one of them, and names anything that is conflicted or failing. That is the whole
script. Arming is one API call that changes nothing until the checks pass, so there is no reason to
ration it, and an armed pull request enters GitHub's merge queue on its own when it goes green.

**It never merges anything labelled `needs-architect`**, which is the one policy the platform does
not know. That label means the work is outside standing merge authority: it touches the syscall
surface, adds a dependency, or owes a `DECISIONS` section.

**It stops rather than guessing, per pull request rather than per pass.** A conflict or a failing
check is reported with the pull request named, and the pass carries on arming the others. Both need
a person, and a loop that retries them just burns CI. A pass where nothing could be armed ends the
loop, because re-printing the same stall lines every 150 seconds is not watching.

## What the merge queue took over, and the four shapes that preceded it

**GitHub's merge queue was enabled on this repository by milestone 120's organization move** (the
setting exists only for organization-owned repositories, which is why it used to be absent rather
than hidden), and on 2026-08-16 this script lost about 150 lines to it. The queue serializes
candidates, tests each against the tip, and ejects what fails, which is precisely what the script
had been reconstructing from outside. Three things changed at once:

- **Ordering stopped being ours.** Enqueue everything eligible and let the queue decide.
- **Updating a branch became neither necessary nor possible.** The queue builds the merge candidate
  itself, and GitHub answers `update-branch` on a queued pull request with a 422.
- **"Arm exactly one" became the wrong answer** rather than a redundant one, because it holds ready
  work back for a cycle when arming is free.

**The history is kept because it is evidence about the up-to-date rule, not about this script.** The
merge queue can be turned off, and if it is, every one of these failures returns. The loop took four
shapes and three of them starved something:

1. **Arm the head only.** #134 sat CLEAN with twelve green checks behind a lower-numbered pull
   request that was still building. calef found it, not the script.
2. **Arm everything.** That starved the head instead. Under the up-to-date rule a merge stales every
   other branch, so a small doc-only pull request goes green during a big one's thirty-minute cycle,
   merges, and sends the big one back to the start. **#117 was re-updated twice that way.**
3. **One target.** Both failures are one fact from two sides: a merge is exclusive, so the queue can
   only land one thing at a time and the only question is which.
4. **Whatever is in flight finishes first.** The third shape preferred a CLEAN pull request on the
   reasoning that it lands in minutes. Wrong: merging the cheap one **stales the one in flight**, so
   a five-minute merge costs a thirty-minute one a whole further cycle and saves nothing, because
   the cheap one would have landed straight afterwards anyway. **#120 paid three cycles** while #137
   and #139 went past it.

The rule those four shapes were groping toward: **order the two operations by what they cost the
queue, not by what they cost themselves.** A merge queue is that rule implemented by the platform,
which is why the script no longer needs to hold it.

## `scripts/trunk-health.sh`

```console
$ scripts/trunk-health.sh --once
main is green at 5a09f754

$ scripts/trunk-health.sh
MAIN IS RED at d1e6b1e9 -- failing: CI -- nobody is assigned to this
main recovered at 38dc6473
```

It reports the *transition* to red and the transition back, never every red poll: a trunk broken for
an hour is one fact, not twenty-four. It reports recovery deliberately, because a watcher that only
speaks on failure teaches its reader that silence means health, and silence is also what a dead
watcher produces.

The phrase "nobody is assigned to this" is not filler. A red trunk with an owner is a task; a red
trunk without one is the failure being surfaced.

## The prevention half, which is not these scripts

`main` went red on 2026-08-04 because two pull requests, each green against the base it was cut from,
merged in an order **neither had ever been tested in**: one added `script/citations`, the other added
a gate requiring every `script/` entry point to carry a provenance block. Neither branch ever
contained the other.

No per-pull-request check can see that, because the failing input is the merge order, which is not a
property of either branch. GitHub's **require branches to be up to date before merging** is the
mechanical answer and was applied the same evening (§73). It converts that failure from a red trunk
into one re-run. These scripts are the detection half; that rule is the prevention half, and it is the
better one.

**The merge queue is the same prevention with the cost removed** (2026-08-16). Up-to-date-before-merge
buys the guarantee by making every author pay for it serially, in full CI cycles, which is what made
the ordering brain above necessary and what milestone 119 measured as the bottleneck. The queue tests
the same thing, the candidate against the tip, without staling anybody's branch to do it. Same
prevention, one rung up: the platform holds it rather than a rule everybody has to route around.

## What the queue bought, measured

Taken 2026-08-16, and it is milestone 119's (the merge queue is the bottleneck) own definition of
done: the block already had the before-median and the sharding, and said plainly that the after-median
had to come from a run of pull requests rather than from the first one on the new path. There are
eighteen now.

**Where the numbers come from.** The GitHub REST API, three endpoints: the merged pull requests, each
one's timeline events, and every workflow run's jobs with their start and finish. Nothing here is read
off a dashboard or remembered.

**The two windows, and why the before one starts where it does.** The proof shards landed on `main`
with #159 at 2026-08-14T05:22Z; the queue's first group build ran at 2026-08-15T21:46Z. So the before
window is the 40.4 hours between them, which holds the *current* prover constant and measures only
what the queue changed. The after window is the 10.2 hours from the first group build to
2026-08-16T08:00Z, where this snapshot stops.

| | before: sharded, no queue | after: the queue |
|---|---|---|
| window | 08-14T05:22 to 08-15T21:46 (40.4 h) | 08-15T21:46 to 08-16T08:00 (10.2 h) |
| pull requests landed | 39 | 18 |
| **"land this" to merged, median** | **17.0 min** (n=29, auto-merge armed) | **12.3 min** (n=17, last enqueue) |
| gap between consecutive merges, median | 15.8 min (n=34) | 10.8 min (n=17) |
| merges per elapsed hour | 0.97 | 1.76 |
| opened to merged, median | 47.0 min (n=38) | 160.9 min (n=18) |
| CI job-minutes per landed pull request | 122 | 157, or 109 with the storm hour removed |
| runs on `main` that went red | 2 of 120 since 08-13 | 0 of 30 |

**The row that got worse says nothing about the queue, and saying so is the point.** Opened-to-merged
counts everything that happened to a pull request, including how long it sat before a person enqueued
it. Its after-window median of 160.9 minutes decomposes: the median from *first* enqueue to merged is
112.1 minutes and from *last* enqueue to merged is 12.3, and the difference is one afternoon's storm
of evictions plus eleven re-enqueues that were operator error. The queue's own cost is the 12.3.

### EXAMPLE: five pull requests, one cycle

The clearest thing in the data. At 2026-08-15T23:42:51 through :57, five pull requests were enqueued
within six seconds of each other. GitHub built five chained candidates concurrently from 23:43:07,
each containing one more entry than the last. #204 and #205 landed at 23:50:37; #207, #208 and #209
landed at 00:03:32.

**Five pull requests, 20.6 minutes from enqueue to the last merge.** At the before-window median of
17.0 minutes each, serialized, the same five would have been about 85 minutes, and under §73's
up-to-date rule each merge would have staled the other four at least once, so the real before-cost is
higher than that and is the thing the ordering brain above was written to manage.

### The caveats, and there are five

- **The samples are small and each is one afternoon.** 39 landings against 18, both from the same
  week, both from lanes run by the same architect. This is a measurement of this tree in August, not
  a general result about merge queues.
- **Runner contention varies and is not controlled.** The same `CI` job, on candidates that differ
  only in which pull requests they contain, ranged from 6.6 to 23.5 minutes across the 44 group
  builds. Any single comparison of two runs is inside that noise; only the medians are worth reading.
- **One storm inflates every early after-number.** Between 21:46 and 22:34 on 08-15, twenty-five
  candidate builds failed CI for one reason: `script/lint`'s branch-prefix check rejected the
  queue's own `gh-readonly-queue/*` branches, so every candidate was ejected and rebuilt. 678
  job-minutes, and the pull requests caught in it carry a two-hour first-enqueue-to-merged that is
  the gate's bug rather than the queue's behaviour. #217 fixed it and was merged directly, outside
  the queue, because the queue could not land anything until it was.
- **Several re-enqueues on 08-16 were operator error, not eviction.** Eleven re-enqueues across
  seventeen pull requests, nine of which needed more than one. Some were the queue ejecting a
  candidate; others were a person removing and re-adding one. The timeline records both as the same
  event pair, so this measurement cannot separate them and does not try.
- **The after window's composition is not the before window's.** It holds the day's largest change
  (#210, the SMB service) and three that waited on calef for a decision. That pulls
  opened-to-merged up and leaves the enqueue-to-merged numbers alone, which is why both are in the
  table.

### The prover is the long pole, but only for the changes that reach it

Group builds run `CI` and `verify` concurrently, so the landing waits on whichever finishes last.

| | CI, median | verify, median |
|---|---|---|
| all 44 group builds | 12.2 min | 3.6 min |
| the 19 after the storm | 10.7 min | 0.6 min |
| the 6 where the proofs actually ran | 11.2 min | 16.7 min |

Twelve of the nineteen post-storm builds finished `verify` in under two minutes, because the scope
job proved that nothing in the change could reach a harness. **So the median landing is now CI-bound
rather than prover-bound**, which is the scoping and the sharding working exactly as milestone 119
predicted, and it is a real change from the block's 2026-08-05 measurement that "a merge cycle is the
Kani job plus noise".

What is left is the tail, and in the tail the prover decides the landing: in those six builds it ran
a median 5.6 minutes past a `CI` that was already green.

**And that tail is almost entirely false positives.** Re-running the `--affected-since` predicate over
the seventeen changes that landed since 08-14 having run the full suite: for all five of the
post-storm ones, **no file in the change was inside any harness crate's dependency closure.** They
proved everything because of files the predicate cannot attribute to a crate, and so runs by default:

| landing | what made it prove the whole suite | harness crates it could reach |
|---|---|---|
| #207 | `Cargo.lock`, `Cargo.toml` (a new workspace member) | none |
| #208 | `art/cobble-first-draft.jpg` | none |
| #210 | `Cargo.lock`, `Cargo.toml`, `scripts/qemu-runner-*.sh` | none |
| #218 | `Cargo.lock`, `Cargo.toml` | none |
| #219 | `scripts/merge-drain.sh` | none |

Three levers follow, ranked by what the counts say and by how much judgment each needs. Together they
account for twelve of the seventeen:

1. **`scripts/` is not `script/`, and the predicate only knows the singular.** A change to
   `merge-drain.sh` proves twenty crates. Nothing under `scripts/` is an input to `cargo kani`: the
   QEMU runners belong to `xtask test` and `kani-lint-shim/` belongs to `script/lint`'s clippy pass,
   which is the same argument the existing `script/` case already makes. **Three of seventeen**, and
   it is one branch in the `elif` that handles `script/` today.
2. **`Cargo.lock` and the workspace `Cargo.toml`: seven of seventeen**, the largest bucket and the
   one that needs judgment rather than a line. Adding a workspace member cannot change a harness's
   closure; bumping a dependency version can. The honest version parses the lock diff for changed
   package entries and tests those against the closure, and it wants its own lane.
3. **Binary and data files: two of seventeen** (`art/`, `bench/baseline-*.txt`). Same shape as the
   documentation case the predicate already handles.

**More shards is not the lever, and the block already measured why.** `glob` is atomic at 15.0 minutes
of a 30.3-minute serial suite, so two shards reach 15.1 and four reach 15.0. The measured group-build
`verify` when the proofs run is 16.7 minutes, which is that floor plus Kani's install. Nothing under
it comes from arranging CI differently; it comes from the unwind bound in one `glob` harness, or from
not proving crates a change provably cannot reach.

### What the measurement corrected about the milestone

**The queue does not amortize CI cost across a group; it amortizes wall clock.** Milestone 119's
block expects "N pull requests cost one test cycle instead of N", and the ruleset as configured
(`max_entries_to_build: 5`) builds one candidate *per entry*, concurrently, each running the full
`CI` and `verify`. That is why cost per landed pull request is flat across the two windows (122
minutes before, 109 after with the storm removed) while wall clock nearly halved. The saving is real
and it is a different saving from the one predicted.

Cost still matters even though this repository is public and its Actions minutes are free, because
what it actually buys is concurrent runners, and the spread in identical `CI` jobs above is what
contention looks like. Trading it back is a setting rather than a project: `max_entries_to_build: 1`
would build one group of up to five pull requests once, at one fifth the cost, and would pay for it
in bisection when a group fails. Nobody has needed that yet.

### Do the two watchers still earn their keep

**`merge-drain.sh` does, and its job changed rather than ended.** It is now the enqueuer: every
landing in the after window entered the queue through the arming call it makes.

**`trunk-health.sh` is closer to superseded, and the number is honest about how little it proves.**
Zero of the thirty runs on `main` since the queue went live were red, against two of a hundred and
twenty in the three days before. Ten hours is not evidence that the trunk cannot go red, and the
class of failure that remains is one the queue never sees: a flake, and the scheduled workflows
(toolchain bump, drift, mutation) which run against `main` on a timer and are not part of any merge.
Keep it; expect it to speak rarely.

## Squash and rebase merging are disabled at the repository, not only in the queue

**Decided by calef 2026-08-18**, closing a gap between a rule and its enforcement. `AGENTS.md` has
said *never squash-merge a branch* since the convention was written, and the reason is `git blame`:
milestone 96's lane put the loader unification in its own commit **ahead of** the migration so that a
boot failure could not be ambiguous between two changes, and a squash-merge would have destroyed
exactly that. The merge commit already carries the pull request's title, so `git log --first-parent`
reads as one entry per piece of work while the detail stays reachable underneath. The clean log costs
nothing.

**What was actually enforcing it until now was the merge queue's `merge_method: MERGE`**, plus people
having read `AGENTS.md`. The repository still had `allow_squash_merge` and `allow_rebase_merge` set,
so the buttons were there and the rule held by configuration coincidence and memory. That is rung two
propped on rung four, and the same day measured what unenforced policy is worth here: a CI gate that
could not block a merge let `main` go red for hours, which is the advisory-checks decision waiting
on its own branch as this is written.

Now `allow_squash_merge=false`, `allow_rebase_merge=false`, `allow_merge_commit=true`. Reversible in
one API call; the previous settings are recoverable from any repository snapshot.

**This does not change how a lane works, and the distinction is the one worth keeping straight.**
Squashing *within* a branch is still the rule: commit early and push often, because a pushed branch
survives a dead session and nothing else does, then squash the checkpoints into the **purposes**
before reporting. A checkpoint has no reader; a purpose commit has one. What is now impossible is
collapsing those purposes into one at merge, which is a different act on a different object.

Two exceptions stay unsquashed inside a branch, and they are why this matters: a commit that records
a correction or a surprise, and a commit whose separateness is itself the argument.

## BUGS

- **A watcher started from a lane worktree dies when that worktree is pruned, and now refuses to
  start there** (2026-08-18). `/bin/sh` reads a script lazily, so deleting the file under a running
  shell can kill it mid-loop. It happened twice in one day: the merge drain died when the worktree
  it was launched from was pruned, and `trunk-health.sh` died the same way during a 24-worktree
  cleanup, **silently, while `main` was red for hours on a gate nobody was watching**. The drain
  survived the second sweep only because it had been relaunched with an absolute path into the main
  checkout. Both scripts now refuse the watching form outside the main checkout and say why;
  `--once` is still allowed anywhere, because it exits long before a prune could reach it.


- **Neither script survives the session that starts it.** They are ordinary loops, not services.
  CLAUDE.md's session-start list is what makes them run; nothing enforces it, and a session that
  forgets has exactly the gap they were written to close. A launchd job or a scheduled workflow would
  fix this and neither has been built.
- **`merge-drain.sh` trusts the label.** A pull request that *should* be held but was never labelled
  will be merged by it. The label is applied by hand at the moment the decision to hold is made, so a
  maintainer that forgets the label has bypassed the gate rather than tripped it.
- **The reduced `merge-drain.sh` had not been run against a live queue, and the first time it was,
  it enqueued nothing for three hours.** Fixed 2026-08-17; the entry is kept because the prediction
  that preceded it was right and was not acted on. It said every claim about the script was read
  from the source rather than observed, because it was written and shellchecked in a container with
  no `gh` at all. What the source could not show: **`gh pr merge --auto --merge --delete-branch`
  fails outright when a merge queue is enabled** (`Cannot use -d or --delete-branch when merge queue
  enabled`), and the call site sent its output to `/dev/null` under `|| true`. So every pass printed
  `9 armed, 0 stalled` while the queue sat empty and nothing merged. The flag was redundant as well
  as fatal: this repository sets `delete_branch_on_merge`, so the platform deletes the head branch
  itself.

  **Two lessons, and the second is the reusable one.** A count of *attempts* was being printed as a
  count of *results*, which is the shape AGENTS.md's ladder calls rung zero wearing a uniform. And
  **nothing on a pull request object says it is in a merge queue**: a queued pull request reports
  `mergeStateStatus: CLEAN` with a **null** `autoMergeRequest`, because arming became membership.
  `mergeQueue.entries` is the only authority, and the obvious field looking authoritative while
  being wrong is what cost the three hours. The verification now asks the queue.
- **`merge-drain.sh` re-arms what is already armed, forever.** A pass counts an arming call as work
  whether or not it changed anything, so one pull request that never merges (a required check that
  was removed, a broken workflow file, a queue that is wedged) keeps the loop alive at 150-second
  intervals with nothing happening. It is cheap and it is silent, which is the bad combination: the
  script cannot tell a queue that is moving from one that is stuck, and neither can its reader.
- **It reports what the queue is about to reject, not what the queue did.** Stalls are read from the
  pull request's own checks. A candidate that fails *inside* the merge queue, against the tip rather
  than against its own base, is ejected by GitHub and this script says nothing about it; the next
  pass simply arms it again.
- **`trunk-health.sh` polls at 90 seconds and reads only `main`.** A release branch, if this tree ever
  grows one, is invisible to it.
- **Neither reports its own death, and on 2026-08-18 that cost hours of red trunk.** If the process
  is killed, both simply stop saying anything, and the failure mode is indistinguishable from a
  healthy quiet queue. The entry above removes the commonest *cause* of the death; it does nothing
  about the silence, which is still unfixed. This is the same defect the
  scripts exist to fix, one level up, and it is not fixed. The queue has taken over most of what
  `merge-drain.sh` did and the whole class of failure `trunk-health.sh` watched for, so this defect
  now costs less than it did; it costs more than nothing, because the arming call is still what puts
  a pull request into the queue and a dead drain still looks exactly like an empty one.
- **The measurement above is a snapshot and nothing re-derives it.** Every number in it was taken by
  hand from the API on 2026-08-16 and pasted into prose, which is precisely the class milestone 125
  (a number in the prose is a claim) exists to fix. Re-take them rather than trusting them once the
  windows are wider than a day; the scripts that produced them were a lane's scratch files and were
  not kept, deliberately, because a throwaway analysis committed as a tool is a tool nobody
  maintains.
- **"Opened to merged" is mostly a measurement of people.** It is in the table because leaving it out
  would be picking the flattering metric, but it is dominated by how long a pull request waited for a
  human to enqueue it, and no arrangement of CI moves it. Read the enqueue-to-merged rows for
  anything about the queue.
- **The eviction and the re-enqueue are the same timeline event pair.** `added_to_merge_queue` and
  `removed_from_merge_queue` do not distinguish a candidate GitHub ejected from one a person removed
  and re-added, so the eleven re-enqueues counted above cannot be attributed. The honest reading is
  an upper bound on the queue's own churn.
