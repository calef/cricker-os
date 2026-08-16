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

## BUGS

- **Neither script survives the session that starts it.** They are ordinary loops, not services.
  CLAUDE.md's session-start list is what makes them run; nothing enforces it, and a session that
  forgets has exactly the gap they were written to close. A launchd job or a scheduled workflow would
  fix this and neither has been built.
- **`merge-drain.sh` trusts the label.** A pull request that *should* be held but was never labelled
  will be merged by it. The label is applied by hand at the moment the decision to hold is made, so a
  maintainer that forgets the label has bypassed the gate rather than tripped it.
- **The reduced `merge-drain.sh` has not been run against a live queue.** It was written and
  shellchecked in a container with no `gh` at all, so every claim above about what it does is read
  from the source rather than observed. The arming call it makes is the one that put #211 into the
  queue by hand on 2026-08-16, so the mechanism is known good; the loop around it is not.
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
- **Neither reports its own death.** If the process is killed, both simply stop saying anything, and
  the failure mode is indistinguishable from a healthy quiet queue. This is the same defect the
  scripts exist to fix, one level up, and it is not fixed.
