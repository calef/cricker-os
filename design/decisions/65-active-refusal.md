# 65. A refusal that is not passive cannot be used as a question

Milestone 72 spent a day on an intermittent hang whose whole content is one line of test code:

```rust
if reclaim_region(tcb_region).is_err() { /* the child is still live */ }
```

over a comment reading *"the refusal leaves the region untouched"*. That comment was true when it was
written. §16 was later amended so that `reap_region_objects` sets `killed = true` on **every live
thread in the region** and *then* returns `Err`, which is what lets an owner's retry tear a runaway
down and is what §24's `^C` escalation is built on.

**The comment kept compiling.** The child did not survive. `schedule()` converts a killed thread to a
corpse at its next preemption, so whenever the timer beat the child's nine instructions it was reaped
before it could `SEND`, the receiver blocked forever, every core fell idle, and the lost-wakeup
watchdog fired sixty seconds later and arbitrarily far from the cause.

## The rule

**An operation whose failure path mutates state is not a predicate, and must not be spelled like
one.** `is_err()` reads as a question at a call site. Nothing about `reclaim_region(r).is_err()`
suggests that asking it destroys the answer.

Two obligations follow, and the second is the one that would have caught this:

1. **Name the destruction where the caller meets it.** `reclaim_region` now carries a `# BUGS`
   section saying `Err` is destructive and that a caller wanting to know whether a region is busy,
   without ending what is in it, has no such call today. That is CLAUDE.md's FreeBSD tenet applied to
   an API rather than to a manual page.
2. **When a decision changes an operation's semantics, its call sites are part of the change.** §16's
   amendment was correct and was recorded correctly. What was not done was reading the callers of the
   thing it amended. A stale comment is invisible to every gate this project has: it compiles, it
   passes clippy, and `script/decisions --check` will happily confirm that the `§16` it cites
   resolves.

## Why it took a day, which is the part worth reusing

Three properties made it look like a kernel bug rather than a test bug, and each one sent the
investigation somewhere real and wrong.

**It was load-sensitive**, because it needed the timer to beat nine instructions, so it read as a
race in the scheduler.

**It looked architecture-specific.** Every wild occurrence was riscv64, which invited a hunt through
`enter_frame` and interrupt masking. The skew was exposure and is countable: CI boots the suite seven
times per pull request and **six of those are riscv64**, because `cpu matrix` runs five CPU models and
does not stop at the first failure. A control on aarch64 with the window widened reproduced it on the
first run.

**It correlated with a real defect.** The suite reaches this test holding 101 threads and 109
endpoints leaked from earlier tests, which looks exactly like a cause. The A/B settles it: the
accumulation is identical in both arms while the hang appears and disappears. **A prediction that both
hypotheses make cannot choose between them**, and four occurrences of a correlation is not
convergence. That leak is real and is now its own open item; it was simply not this.

The method that broke it is milestone 71's, reused: **stop waiting for a race and widen its window**.
A call-free delay loop in front of the child's report guarantees the preemption, which turned "one run
in four under four host burners" into "every run, both ISAs", and turned removing the probe into a
disproof rather than an absence of evidence. Twenty runs clean afterwards, under the load that used to
hit it.

## BUGS

The kernel has no way to ask whether a region is busy without ending what is in it. Milestone 72 needed
that question and could not ask it; the test now avoids needing to. If a second caller wants it, the
answer is a new method, not a re-reading of `Err`.
