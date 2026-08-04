# 48. Job control: `jobs`, `wait`, `kill`, `fg`, `bg`, and a stopped state

**Status: NOT-STARTED.**

**Gate: DECISION, MILESTONE 47.** Phase one is sequenced after 47, because it wants `jobs`
alongside the other builtins on the same shell surface. Phase two is `Tcb::SUSPEND`/`RESUME`, which
DECISIONS §24 deferred, so it is a kernel-surface fork for Chris; the block asks for it to be
designed as one surface with the fault endpoint and for all three of §24's triggers at once.

**In brief.** Shell job control, in two phases split by whether they need a new kernel primitive.
**Phase one needs none**; phase two is `Tcb::SUSPEND`/`RESUME`, which DECISIONS §24 deferred and whose
own trigger list names "real job control (`fg`/`bg`, a stopped-process state) in the shell" as trigger
2. That trigger has now fired.

**Why it matters.** Unix job control is one of the most intricate things in a kernel: sessions, a
controlling terminal, process groups, `tcsetpgrp`, and `SIGTSTP`/`SIGCONT`/`SIGTTIN`/`SIGTTOU`. Most
of that machinery exists to answer one question (*who may read the keyboard*), and here that question
answers itself.

## A job is what the shell holds capabilities for

Structural rather than conventional. The shell built its children through the granular verbs, so it
holds their TCBs, their untyped region, and the supervision endpoint they report to. Unix's process
group is a *number* with inherited, mutable membership; "what I hold" cannot drift.

## Phase one: no new kernel surface

- **`jobs`**: the shell listing its own holdings, the same category as `caps`, `pwd` and `ls`.
- **`wait`**: §26 already delivers exit as a message with a kernel-stamped tid, so this is a receive
  on the supervision endpoint.
- **`kill`**: §24 already built this under another name: the cooperative tier is the shared-flag
  interrupt, and `kill -9` is the forcible tier (`Untyped::DESTROY`). Job control needs no signal
  model because the two-tier one exists.
- **`&`**: running in the background is simply *not granting the terminal*.
- **`fg` on a running background job**: a capability transfer, below.

**Foreground versus background is: who holds the terminal input capability.** `fg %1` is the shell
revoking that capability from whoever held it and granting it to job 1; revocation (§13, §16) is
already built and is exactly the primitive this needs. A background job that reads the terminal does
not get `SIGTTIN` and does not get stopped: **it has no capability to read with**, and the refusal is
"you hold no such capability". Sessions, controlling terminals, `tcsetpgrp` and two of the four signals
disappear, not by reimplementation but because the question they answer is already answered by who
holds what.

## Phase two: the stopped state

Only Ctrl-Z, `bg` on a stopped job, and zsh's `suspend` need pausing a thread resumably. §24's tiers
are notify and kill, with pause deliberately absent. Build `Tcb::SUSPEND`/`RESUME` per that tracker's
own instruction: **design it as one surface with the fault endpoint** (both are "the kernel turns a
thread's state into a message a supervisor holds"), and give the method its own DECISIONS entry. The
same verb unlocks the other two triggers, a userspace pager and a debugger, so it should be designed
for all three rather than for job control alone.

## The open question: `disown`

If the shell drops its capabilities to a job, nobody can reap it, and §26's dead-until-reaped means the
corpse persists. Unix reparents orphans to init and lets init reap them; here reparenting means
**transferring the supervision endpoint**, which is an explicit act rather than a rule nobody thinks
about. **Decided as DECISIONS §40**: a supervisor's death is its subtree's death, because a child's
resources come from its supervisor's region and §16's revocation reclaims the whole subtree in one act.
So `disown` means **transfer supervision upward**, not "abandon", and §40 records the hole that makes
the cascade close to the only coherent answer, namely that §32 authorizes reaping by matching the
child's recorded `fault_ep`, which nobody can satisfy once the supervisor's endpoint is gone.

**Sequencing.** Phase one after milestone 47 (it wants `jobs` alongside the other builtins and the same
shell surface). Phase two is gated on nothing but the SUSPEND decision. **Effort: 1 lane estimated per
phase**, noting estimates for unbuilt work are guesses on a history-calibrated scale.
