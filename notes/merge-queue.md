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
  worktree, delete the branch, relink `cricker-dev`, leave no QEMU, and does not mention it.
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
merge-drain: updating #124 against main (Milestone 58: the RISC-V TLB shootdown)

$ scripts/merge-drain.sh            # loop until the queue is empty or stalled
merge-drain: #124 waiting on checks (Milestone 58: the RISC-V TLB shootdown)
merge-drain: updating #128 against main (Milestone 67: swish the language)
merge-drain: queue empty; nothing open that does not need calef
```

It takes the open pull requests **without** the `needs-architect` label, arms auto-merge on **every one
of them**, and clicks "Update branch" on **one**. Auto-merge is armed *before* the update, so a merge
lands whether or not the loop is still alive; the script should be an accelerator, never a
dependency.

**At most one merge in flight**, and this loop took three shapes to get there. Each earlier one
failed in a way that looked like the opposite bug, which is why the reasoning is here rather than
only in a commit.

1. **Arm the head only.** #134 sat CLEAN with twelve green checks behind a lower-numbered pull
   request that was still building. calef found it, not the script.
2. **Arm everything.** That starved the head instead. Under the up-to-date rule a merge stales every
   other branch, so a small doc-only pull request goes green during a big one's thirty-minute cycle,
   merges, and sends the big one back to the start. **#117 was re-updated twice that way.**
3. **One target.** Both failures are one fact from two sides: a merge is exclusive, so the queue can
   only land one thing at a time and the only question is which.

4. **Whatever is in flight finishes first.** The third shape preferred a CLEAN pull request on the
   reasoning that it lands in minutes. Wrong: merging the cheap one **stales the one in flight**, so a
   five-minute merge costs a thirty-minute one a whole further cycle and saves nothing, because the
   cheap one would have landed straight afterwards anyway. **#120 paid three cycles** while #137 and
   #139 went past it.

So: pick exactly one target, arm exactly that one, leave the rest alone until it lands. Order the two
operations by **what they cost the queue, not by what they cost themselves**: in flight first, then
anything already current, then the oldest.

Arming and updating still have very different costs and that is why only the target is updated:
arming is one API call that changes nothing until the checks pass, while updating triggers a full CI
run, and `cpu matrix` is this tree's load-sensitive check (notes/cpu-models.md).

**It stops rather than guessing.** A conflict or a failing check ends the pass with the pull request
named. Both need a human, and a loop that retries them just burns CI.

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

## BUGS

- **Neither script survives the session that starts it.** They are ordinary loops, not services.
  CLAUDE.md's session-start list is what makes them run; nothing enforces it, and a session that
  forgets has exactly the gap they were written to close. A launchd job or a scheduled workflow would
  fix this and neither has been built.
- **`merge-drain.sh` trusts the label.** A pull request that *should* be held but was never labelled
  will be merged by it. The label is applied by hand at the moment the decision to hold is made, so a
  maintainer that forgets the label has bypassed the gate rather than tripped it.
- **It cannot tell "checks still running" from "checks that will never run".** A pull request whose
  required check was removed, or whose workflow file is broken, reads as `BLOCKED` with no failures
  and the drain waits on it indefinitely. It says which pull request it is waiting on, so the stall
  is visible, but it will not time out.
- **`trunk-health.sh` polls at 90 seconds and reads only `main`.** A release branch, if this tree ever
  grows one, is invisible to it.
- **Neither reports its own death.** If the process is killed, both simply stop saying anything, and
  the failure mode is indistinguishable from a healthy quiet queue. This is the same defect the
  scripts exist to fix, one level up, and it is not fixed.
