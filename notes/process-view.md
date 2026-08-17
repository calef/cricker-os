# The process view: a listing is a capability, not a fact about the machine

Milestone 126, first stratum. **`ps` works here, and it cannot enumerate the machine.** What it can
see is one supervision domain, because somebody handed it the endpoint that supervises one. There is
no `/proc` to open, no pid space to scan, and no call in a program's reach that takes a process id it
was not shown.

This note is why that shape, what it costs, and the one place the authority is currently wider than
the job.

## The problem, which the reader already agrees with

`ps aux` on Linux reads `/proc`, and `/proc` is **ambient**: any process gets it with no grant from
anyone. So the listing is every process on the box, including the ones with secrets in `argv`. Nobody
defends that design; they live with it, and `hidepid` exists because enough people stopped wanting
to.

That is what makes this a good first demonstration of the argument milestone 121 makes for
directories. **Enumeration is a larger power than reading something you were handed**, and the claim
needs no setup: the reader already knows the Unix behaviour is wrong.

## The design: a view over a supervision subtree

**The scope is the supervision subtree, because the kernel already maintains it.** A thread's
supervision endpoint is recorded at `START` (`Thread::fault_ep`, DECISIONS §26) and never changes.
The set of threads whose deaths arrive on one endpoint is therefore a set the kernel keeps for its
own reasons, exactly maintained, and it costs nothing to read.

So the domain a viewer sees is **the endpoint it holds**. Same move `rm -r` makes with a directory
subtree: authority is a subtree, not a global. A scope the system already keeps cannot drift out of
agreement with reality, which is the property a registry would not have had.

**The wide grant is not forbidden. It is nameable.** An operator's monitor over the whole machine is
a program handed the endpoint that supervises the whole machine, and `caps ps` prints which one that
is *before* anything is spawned. On Linux there is no such distinction to print, and that is the
entire difference. This is Fuchsia's job-handle shape: their `ps` needs a handle to the root job to
see everything, and a handle to a smaller job to see less.

## The surface: one new method, no new syscall

`abi::endpoint::SURVEY`, method 6 on an endpoint capability. **A new method inside the established
capability model, not a new syscall number.**

```text
invoke(cap, SURVEY, cursor, 0, 0) -> (next_cursor, tid, state)
```

- `next_cursor` returns in x0 (a0 on RISC-V), `tid` in x1, `state` in x2.
- Start with `cursor = 0`. Feed each `next_cursor` back. `abi::survey::DONE` (zero) means finished.
- A negative first word is an `abi::Error`.
- **Needs `READ`**, exactly as `endpoint::REAP` does.

`state` is one of `abi::survey`'s four codes: `READY`, `RUNNING`, `BLOCKED`, `DEAD`. Only four,
because a supervised thread cannot be found in the other two. `Embryo` has not run, so it has no
recorded supervision endpoint yet; `Finished` is what *un*supervised death looks like, and a
supervised thread dies into `DEAD` and waits for its supervisor.

### Membership is the relationship, and it is proved

The kernel walks its thread table and reports an entry when `capability::survey_includes(fault_ep,
invoked_ep)` says so, which is `matches!(fault_ep, Some(ep) if ep == invoked_ep)` and nothing else.
Three Kani harnesses in `crates/capability`:

- inclusion holds **if and only if** the thread's recorded endpoint is the invoked one. Both
  directions matter here, unlike for the reap: one is confinement (a stranger is never shown) and the
  other is truthfulness (a member is never hidden, so a missing row means gone rather than
  concealed);
- the view and the reap have the **same scope**, for every liveness, so the set a monitor sees and
  the set a supervisor may collect from cannot diverge;
- plus §32's two existing reap harnesses, which this reuses rather than restates.

### Why one entry per call

`SCHED` is given back between entries. A survey that held it for a whole domain would let a
userspace program decide how long the scheduler is locked, which is a latency hole a program could
open on purpose.

The cost is that **a survey is a sequence of snapshots rather than one**. The cursor is a *slot
index*, not a position in a filtered sequence (`slots::Table::iter_from`), which is what makes the
resume safe: the entry that was at slot `k` is the only thing that can be at slot `k`, and if it
died, `k` is empty rather than somebody else's thread. So a resumed walk never reports a member twice
and never resolves a cursor to the wrong thread. It can miss a member born into an already-passed
slot, and it can list a member that dies before the table prints. That is `readdir`'s bargain, taken
knowingly.

## Refused, empty, and populated are three answers

**This is the deliverable, not a detail.** A monitor that reports nothing because it *could not look*
reads exactly like a quiet machine, which is the worst failure this tool has available. `fs_proto`
chose `EPERM` over an empty listing for the same reason (milestone 108's shape).

| what the viewer holds | answer |
|---|---|
| the endpoint with `READ`, domain has members | the rows |
| the endpoint with `READ`, domain is empty | `DONE` on the first call: an answer |
| the endpoint **send-only** (`WRITE`, no `READ`) | `NotPermitted` |
| nothing in the slot | `NoSuchSlot` |

The send-only case is the interesting one and it is a real relationship in this tree: a peer that
reports *to* a supervisor holds exactly that. It may send here and it may not look, and the kernel
says so rather than answering with a plausible nothing.

All four are asserted in one kernel test on both ISAs
(`kernel/src/user/survey_tests::a_viewer_without_the_domain_is_refused_rather_than_shown_an_empty_list`),
and the empty case is in the same test on purpose: neither claim means anything without the other.

## What `ps` is, concretely

Two halves at the IO boundary, which is the crate-and-program pair convention.

- **`crates/ps`** is the listing: the cursor walk, the buffer, the columns, the refusal catalogue.
  Host-tested in milliseconds, nine tests, and total for *every* reader including one that never
  advances its cursor.
- **`user/src/ps.rs`** is the syscall and two sinks, about sixty lines.

The kernel's survey tests drive `ps::collect` against the real `endpoint::SURVEY`, so the cursor
protocol is proved end to end rather than by a second copy of the walk written in a test.

**Collect first, complain second, print third.** DECISIONS §67's rule is that a program says
everything it has to complain about and closes its second stream before it writes a byte of output,
because the reader drains diagnostics to end-of-stream first. A survey cannot know its complaints up
front (an endpoint can die mid-walk), so the whole domain goes into a buffer, then diagnostics, then
the table.

**The buffer is the caller's, and that was a gate's doing.** It began as a `[Row; MAX_ROWS]` local,
which made `collect`'s frame 4,336 bytes: larger than the 4,096-byte guard page under every kernel
thread stack, so one call could move `sp` past the guard in a single step and land in a neighbouring
thread's stack without ever faulting. `script/stack-frame-check` failed the build and named the
shape, which is the second time that gate has caught a `[T; MAX]` local wearing the clothes of a
bound. A caller-provided slice is the fix it recommends and is better anyway: a program that sizes
its own listing knows where the memory came from, and `ps` sizes it at `MAX_ROWS` while the kernel's
tests size it at eight.

A compile-time assertion in the kernel's survey tests keeps `ps::MAX_ROWS` at least as large as
`sched::MAX_THREADS`, so the shipped program has no truncation case. A caller with a shorter buffer
does, and it is **not silent**: `Survey::complete` is false, nothing is printed, and diagnostics say
the domain has more in it. Same rule as the refusal: a monitor never reports less than it saw
without saying so.

## Where it comes from at the prompt

`Manifest::domain` is a declaration, `Manifest::clock`'s twin: a process domain is not a name a
person types, so there is no token to place and no refusal to write. What the field does is tell
**init** which children to endow, and tell a person reading `caps ps` that the authority exists.

Init places the endpoint in `grant_plan::DOMAIN_SLOT` (seven) with `READ`, using the same named-slot
mechanism §67 gave the diagnostics stream, for the same reason: how many low slots a child gets
depends on what else the line granted it, and a program that probes a fixed number needs that number
not to move.

The endpoint it places is `deaths`, which is what supervises **every job init spawns for the shell**.
So `ps` at the prompt lists this shell's jobs, including itself, and nothing else. Init, the shell,
the terminal, the filesystem server, the compositor, the net stack and every driver are outside it,
which is why `ps | wc` at the boot gate counts a handful of lines where a `/proc`-shaped listing
would count dozens.

## EXAMPLES

At the prompt:

```text
$ ps
         TID  STATE
           5  blocked
           9  running

$ ps > running.txt        the table lands in the file
$ ps | wc                 the table is counted, and no /proc was read to make it
$ caps ps                 the scope, printed before anything is spawned
  ps would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    cap 7  endpoint  domain   READ. the processes this shell's jobs are supervised
                              by, and no others. it cannot name a process outside
                              this domain, or learn that one exists
    ...
```

The refusal, which has no Unix equivalent because on Unix there is no domain to be outside of:

```text
$ ps        ps: this process holds no process-domain capability
```

From a host test, with no kernel at all:

```rust
let mut reader = |cursor: u64| match cursor {
    0 => (1, 7, abi::survey::RUNNING),
    _ => (abi::survey::DONE as i64, 0, 0),
};
let survey = ps::collect(&mut reader);
assert_eq!(survey.rows()[0].tid, 7);
```

## BUGS

- **Holding a domain with `READ` was more authority than looking needs. Fixed 2026-08-17**, and the
  entry is kept because the shape recurs and the fix is small enough to reuse.

  The finding: `READ` on a supervision endpoint is also what `RECV` and `endpoint::REAP` take, so a
  viewer endowed a view could take a death message out from under the real supervisor
  (`job_undertaker`, at the interactive boot) or collect a corpse. `ps` did neither, and its source
  was the whole argument that it did not, which is exactly the kind of argument this system exists
  to replace with a mechanism.

  The lane that found it deliberately left it, on the reasoning that splitting view from control
  changes the rights model and is the *same* decision the signalling stratum needs. **That
  reasoning turned out to be wrong in a way worth recording**, because the signalling stratum
  mostly evaporated when calef ruled that a domain names its members and does not act on them: there
  was no second decision to wait for, and the deferral was buying nothing.

  The fix is `capability::Rights::ENUMERATE`, the kernel-level twin of `fs_proto`'s directory
  `ENUMERATE`, and it is the same argument one layer down. `SURVEY` takes it; `RECV` and `REAP`
  still take `READ`; `system_initializer` grants a viewer `ENUMERATE` **alone**. So a `ps` does not
  get refused a reap, it cannot name one, which is the ladder's top rung in place of an argument
  about a program's source.

  **The tell that one bit was doing three jobs** is worth carrying off. `READ` on an endpoint
  unlocked receive, reap and survey, and no grant could express any one of them. When a right
  unlocks operations that differ in kind rather than in degree, it is not a right, it is a
  category.

- **A process has no name, so there is no `CMD` column.** This system has `arg0` in `Spawn` and no
  display name at all. A name is information rather than authority, but a confined viewer may still
  not be entitled to it and there is no design for that today; a `CMD` column that appeared without
  one would be a leak wearing a familiar heading.

- **A survey is a sequence of snapshots.** See "why one entry per call" above. A member born into an
  already-passed slot is missed until the next survey, and a row read early may be stale by the time
  the table prints. Fuchsia handles a process dying mid-enumeration and this does not; their answer
  is worth reading when somebody needs one.

- **A child that is built but not yet started is not in its domain.** Supervision is recorded at
  `START`, so an embryo has no endpoint to match. That is invisible at the prompt (init starts a job
  in the same breath as building it) and would matter to a builder watching its own construction.

- **The comparison against Linux is not apples to apples, and a write-up must say so.** Ours lists a
  domain; theirs lists a machine. That is the entire point, and a table putting the two side by side
  without stating it would be dishonest in the way §14's map "tie" caveat exists to prevent.

- **`ps` lists itself**, and in a pipeline it may or may not list its own reader. It is a member of
  the domain it was spawned into, which is truthful and is what Unix's `ps` does too. The pipeline
  half is sharper: **both stages of `ps | wc` go into the same domain**, so whether `ps` walks before
  or after `wc` exists is a race, and the boot gate saw three lines on one run and two on the next.
  That is a snapshot behaving like a snapshot rather than a bug, and it is why the boot gate asserts
  the header and the scope while the confinement claim is asserted in the kernel test, which builds
  the domain it measures instead of inheriting one.

- **A doomed thread does not say so.** DECISIONS §16's `killed` flag marks a thread whose region
  owner has torn it down and which has not yet reached a preemption. It surveys as `RUNNING` or
  `READY`. Adding a bit to the state word is additive and cheap; it was left out to keep the first
  method minimal, and the moment something wants to watch a `^C` land is the moment to add it.

## What this does not build

The rest of the view stratum (`top`, `pgrep`, `pmap`, `pwdx`, `w`), the signalling stratum, the
machine-wide statistics, `watch`, and `sysctl` (which milestone 126's block records as a design fork
rather than a program to port). `top` in particular needs per-thread CPU accounting that does not
exist at all: `QuotaToken` is dead code whose own comment says `spawn_with_quota` has no caller.

See `design/roadmap/126-who-else-is-running.md`, notes/supervision.md (the mechanism this reads),
notes/pipes.md (the second stream), and notes/program-manifest.md (how the grant is declared).
