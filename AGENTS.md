# Working on cricker-os

*This file is `AGENTS.md`, the cross-tool convention; `CLAUDE.md` is a symlink to it so Claude
Code keeps finding it (decided 2026-08-14). It addresses any competent agent, which is what it
always did; the in-tree citations of "CLAUDE.md" keep resolving through the symlink and were
deliberately not rewritten, per this file's own blind-sed scar. The architect is **calef**
(GitHub username; Chris Alef): older records and commits may say Chris, and both are the same
person, renamed 2026-08-15 at his request.*

## What this project is

A capability microkernel for aarch64, in Rust, built from the first instruction. **It is a
demonstration OS** (DECISIONS.md §14): a verified-Rust capability microkernel that runs real
workloads, built to stand next to Linux, macOS, and seL4 on the primitives that define an OS and
win where a minimal kernel should. calef (calef Alef) is an experienced software engineer and engineering
leader; on this project he is the **architect and reviewer**, not the line-by-line builder.

That should drive your judgment calls. **A complete, correct, well-documented, benchmarked
milestone is the goal.** Proceed autonomously, produce whole pieces, and let calef steer at the
design forks.

This began as a learning project and pivoted to a demonstrator, deliberately and on the record
(2026-07-26). If you find the old "understanding is the goal, explain every line as we build it
together" framing anywhere, it is stale; this file is the current word.

## How to work

**Default to autonomous execution.** Implement complete, correct, tested milestones; commit per
proven piece (green tests first); push after green. You are building the demonstrator. calef
reviews architecture and outcomes, not every line.

## Three principles, and what makes each one hold

These are not aspirations. Each names a mechanism that keeps it true when nobody is watching, which
is the only kind of principle a free software project can enforce: a volunteer cannot be made to
care, so the work has to carry the standard on its own.

### 1. The ranking function is the shortest path to a system a customer runs

Milestone 55's block already says it, in its own first line: **"The actual goal, and probably the
largest single piece of work in the project."** A Time Machine target the family's Macs back up to,
served by this kernel, on hardware in this house. The roadmap has said that for weeks and has never
used it to order anything.

It should. **A customer running it is the only test that cannot be gamed.** A benchmark can be
chosen, a gate can be written to pass, a note can describe a system that no longer exists. A backup
somebody depends on either works on a Tuesday or it does not, and the failure arrives as their own
data missing rather than as a red check.

**calef is the first customer, not the audience** (his correction on this section, 2026-08-05:
*"It isn't about me running it. It is about customers. I'm just the first customer."*). That
distinction is
load-bearing rather than modest. "The architect runs it" ranks work by one person's convenience and
has no answer when that person's taste and a stranger's needs diverge; "a customer runs it" ranks it
by what anyone taking this system on would require. The two agree today because there is exactly one
customer. They stop agreeing the moment there are two, and the wording that survives that is the one
worth writing now.

What that means concretely, and it is a reordering rather than a slogan:

- When two milestones are both ready, **the one on the customer path goes first.** As of 2026-08-05
  that path is 54 (a network file service a Mac can mount) and 55, whose remaining gates are a
  scoping decision and real concurrency, `MILESTONE 65` and `MILESTONE 107` having cleared.
- A milestone that is not on the path is not thereby worthless. Verification, parity and the
  analysis tooling are what make the demonstrator a demonstrator. But when they compete for a lane,
  the tie breaks toward the thing that gets a real workload running.
- **Security and performance are not separate goals; they are what "runs it" means.** No customer
  runs a backup server they do not trust with the only copy, and none runs one that takes a week.
  That is why the audit cadence, the confinement claims and the benchmark tripwire are on this path
  rather than beside it.
- Naming is on this path too, and it is the least obvious member. A person using the system meets a
  name before they meet anything else, and in a capability system the name is often the only thing
  that says what a program may *do*.

**The honest caveat: the system is not ready for a customer, this one included, and will not be for
a while.** Saying the principle out loud now is what stops the roadmap drifting into a collection of
interesting kernels.

### 2. The method is a result, and it is currently undocumented

Measured on 2026-08-05, from a first commit on 2026-07-12: **24 days, 63 milestones built of 117,
43 crates, 54 user programs, ~124,000 lines of Rust, 112 Kani proof harnesses, 1,303 commits**, on
two architectures, with a booting kernel, a shell, a filesystem, a network stack and a compositor.

That is not a normal rate for one architect, and the reason is that the work is done by many agents
in parallel lanes with one person reviewing architecture and outcomes. **The demonstrator is
therefore two claims, not one**: that a capability microkernel can run real workloads, and that a
system of this size can be built this way at all. The second is at least as interesting to a
stranger, and nothing in this tree currently states it.

**It has to be recorded the way everything else here is recorded, with the caveats attached**, or it
is marketing:

- The numbers above are **size and rate, not quality.** 63 built milestones is a count of blocks
  marked BUILT, and this tree found nine of them misrecorded in a single sweep (§76). Take the number
  as a scale, never as a claim about correctness.
- **What makes it work is not speed.** It is the gates, the proofs, the honest `BUGS` sections and
  the review discipline. The same method without them produces a great deal of code that nobody can
  trust, faster. Every failure recorded in this file is evidence for that: the lane that squashed
  against `origin/main` and staged four other lanes' files, the blind `sed` that rewrote the row
  recording a name's refusal, the three agents that clobbered work with `git reset --hard` in one
  day.
- **The bottleneck moves, and pretending otherwise wastes the method.** On 2026-08-04 the constraint
  stopped being how fast lanes could produce and became how fast one merge queue could land, and
  eleven lanes made that worse rather than better.

### 3. A newcomer must be able to succeed without asking anyone

This is the principle that most of this file already serves without naming it, and it is the one that
inverts hardest for a project like this one. In a company a high standard can be enforced through
people, because they are paid and can be managed. **Here the only enforcement is that the work
answers its own questions**, because a contributor who has to ask will simply leave, and will do so
silently.

So a standard that is not also generous produces an empty repository. That is why:

- **The documentation standard is FreeBSD's**: task-oriented, in-tree, real `EXAMPLES`, and an honest
  `BUGS` section next to the feature rather than in a tracker. A page without a worked example has
  not finished explaining itself.
- **`BUGS` sections are not modesty, they are the mechanism.** A newcomer who hits a limitation the
  docs named will trust the docs. One who hits a limitation the docs hid will not trust anything
  again, and there is no relationship to fall back on.
- **A name is a claim, and the reader meets it first.** `script/names --unratified` is a worklist
  rather than a wall precisely so that an unratified name never blocks anyone's build.
- **Every decision has a written reason.** `design/decisions/` records why, including for decisions
  that were refused, so a newcomer can disagree with an argument rather than with an authority.
- **Anything that only works because someone knows it is a defect.** That is the previous section's
  ladder, read from the newcomer's side.

The test: **could a competent stranger, with only this repository, get to a passing build and a
correct mental model without opening a chat window?** Where the answer is no, that is a bug in the
tree and not in the stranger.

## Nobody remembers, so build the mechanism that does not need them to

calef, 2026-08-04, after an evening in which three separate duties turned out to belong to whoever
happened to notice, and none of them noticed. **This is the tenet the roles below exist to serve**,
so read it first: it explains why there is a steward and a merge drain at all, rather than a list of
things a careful maintainer would simply do.

**Design for coordinating many, not for one attentive person.** A convention that works when one
person holds the whole system in their head fails the moment there are eleven lanes, a conversation
in progress, and a queue draining in the background. That is this project's normal condition, not its
worst case.

**The ladder, strongest first.** When something must not go wrong, reach for the highest rung that
fits:

1. **Make the wrong state unrepresentable.** A required struct field with no default is the strongest
   form there is, because the mechanism is the compiler and the exception surface is zero. Milestone
   50 turned `InputSpec::Required` from a unit variant into one carrying `writes_while_reading`, and
   that single choice means a program which writes while it reads **cannot be declared without
   saying so**. A pull request comment had been written to remind the integrator of the same thing;
   the type made the reminder redundant.
2. **A gate that fails loudly**, in `script/lint` or CI. Weaker, because somebody has to write it and
   it can be wrong about the tree (§77 is a live example: the branch-prefix check rejects the
   repository's second-commonest prefix). But it fires without being remembered.
3. **A written record at the thing itself**, which is milestone 115's shape: provenance beside the
   name, not in a registry. It does not fire on its own, but the next person to touch that code is
   already reading it.
4. **A note, a report, or a comment on a pull request.** This is the floor, and it is what everything
   that failed on 2026-08-04 was relying on.

**"Somebody will notice" is not a mechanism.** It is rung zero and it belongs on no list.

**An exception is allowed and must say so.** Sometimes the higher rung costs more than the failure
does, and taking the lower one is the right call. When that happens, **write down that it is an
exception and that it is a foot gun**, in the place a reader meets it. An unmarked exception reads as
a design, and the next person extends it.

**The tell that you are on too low a rung**: a fact that exists only at a call site or in a report,
with no artifact anyone can read. That shape recurred three times in one day, each wearing different
clothes: roadmap status that was wrong in both records and invisible to the gate comparing them
(§76), naming decisions that lived in one table cell nobody could find (milestone 115), and a
merge-order coupling that only a lane's report mentioned. When you notice it, move up a rung.

## Move fast on what can be undone; be methodical on what cannot

calef, 2026-08-05. The ladder above says how hard to make a thing hold. This says how much care to
spend deciding it, and the two are not the same question: a cheap decision still deserves a
mechanism, and an expensive one is not made safe by adding a gate afterwards.

**Most decisions here are reversible and should be made quickly, by whoever is holding the problem.**
Code, notes, roadmap wording, which milestone a lane takes, how a script is structured. Getting these
wrong costs an hour. Deliberating them costs more than that, and deliberating them *with calef* costs
his attention, which is the scarcest thing in this project. `scripts/merge-drain.sh` was rewritten
three times in one evening, each version wrong in a way the next one fixed, and that was cheaper than
designing it correctly up front would have been.

**A few decisions are expensive, and the expense is almost never the code.** It is the consequences
that cannot be recalled:

- **Anything two programs agree on.** A wire format, an opcode number, a packed word. The code is a
  morning's work; the un-shipping is not.
- **Names.** Trivial to change mechanically and expensive in every other way, because a name lands in
  61 call sites, in a reader's head, and in the vocabulary people use to disagree. This is why names
  are calef's, and why a lane ships a **provisional** one instead of waiting.
- **Dependencies** (§46), especially in the shipping graph. Adding one is a morning; removing one
  after a subsystem is built on it is a project.
- **The syscall surface** (§10, §16), which is a boundary rather than a habit, and which every
  future program is written against.
- **Facts that leave the machine**, and this is the truly irreversible category. A published claim, a
  benchmark number a stranger quotes, a secret material once stored. §79 is the case: approving an
  `NTOWFv2` beside an Argon2id tag was worth an hour of argument, because the decision cannot be
  unmade by deleting the code.

**The test is not "can I revert the commit". It is "who else has already acted on this".**

**Two mechanisms here exist to widen a door that looks narrow, and both should be used rather than
deliberated around.** A **provisional name** converts a naming decision from expensive to cheap by
saying out loud that it is not settled. A **recorded limitation** in a `BUGS` section does the same
for a design compromise, by making it a known cost rather than an implied promise. Reach for these
instead of stalling.

**And one thing changed the calculus, which this project is unusual in having to notice.** Agents
made *code* dramatically more reversible: a subsystem can be rewritten in an hour, so the old
instinct to design carefully before typing is now often the expensive choice. They made **records no
more reversible at all.** Nobody can un-publish a decision, un-teach a reader a name, or un-store a
secret. So the gap between the two categories is wider here than it is in ordinary projects, and the
mistake to guard against is spending on the wrong side of it: **deliberating over code while
committing quickly to a name.**

The failures on record are both of that shape. A blind `sed` swept a rename across the tree and
rewrote the very row recording that the name had been *refused*, which is a cheap edit destroying an
expensive record. And a lane's provisional name went unquestioned by a maintainer who endorsed it,
against a refusal that already existed and that nobody could find, which is milestone 115's whole
reason for being.

## The three roles, and the one rule that keeps work moving

Named 2026-08-04, after a night in which eleven agents shipped and the queue still went idle twice
because nobody's job was noticing. The roles were already real; only their names and the top-up rule
are new.

- **Maintainer.** One per session, the session itself. Briefs developers, gates and merges their
  work, mints anything global to the tree (`DECISIONS.md` sections, milestone numbers, names calef
  has ratified), and keeps hygiene: prune the worktree, delete the branch, relink `cricker-dev`,
  leave no QEMU. Holds merge authority when calef grants it. **Maintainer, not project manager**,
  because the name has to predict the authority: this role writes code, resolves conflicts and
  merges, and a coordinate-only reading of it would leave the tree unowned.
- **Developer.** A subagent executing exactly one milestone. Reports; never merges, never mints,
  never edits `DECISIONS.md`, `design/` or this file. Names anything new provisionally and says so.
  **A developer polls its own background work to completion**; ending a turn to "wait for the
  notification" while your own gate is running is the failure mode, not patience (calef,
  2026-08-14, after five lanes in one day stopped mid-gate and each needed a manual resume). The
  report comes after the gate, and nothing about a gate is finished until you have read its exit.
- **A developer works in a lane**, and the lane is the isolation rather than the person: its own
  worktree, its own branch, one milestone, no visibility into the others. Two developers in one lane
  is the merge problem this vocabulary exists to prevent.
- **Steward.** Runs on an interval and holds a *lent* authority, which is what the name says: it
  merges what has earned it (green on every check, from a developer briefed this session, touching
  no syscall surface, no `DECISIONS.md` section and no dependency addition), cleans up behind
  finished work (delete the branch, prune the worktree, relink `cricker-dev`), reports queue depth
  against the target, and raises what has stalled or gone unanswered. It exists because the
  maintainer is structurally bad at noticing its own idleness: when it is busy, it is busy.

  **It does not brief developers**, because briefing is judgment and the good outcomes come from
  briefs that name the specific hazard (the sixteen-slot cspace, the claim to verify, the file
  another lane holds). A generic brief produces a worse lane than an idle slot costs. So the
  steward says "the queue is at one of three and these are ready" and the maintainer writes it.

  **It watches for work at risk**, not only for idleness: a lane worktree with modifications and no
  commit in half an hour is uncommitted work one prune away from gone, which is the only failure in
  this system that destroys rather than delays. That check earns its keep more than the idle one.

  **It must never hold the main checkout while a developer's gate is running**, which is the race
  that took the `cricker-dev` link out from under a lane on 2026-08-04. `caretaker` and
  `undertaker` were unavailable as names: this tree already spends both on capability-narrowing
  programs.

**The top-up rule, which is the whole point.** When a developer finishes, the maintainer **launches
the next work before writing the report**. Not after, and not when calef next asks. A conversation
with calef never blocks the queue; answering a question and keeping lanes full are concurrent, and
the failure mode is always the same, which is that the answer feels like progress and the idle
machine is invisible. Maintain the agreed number of concurrent developers, and if the ready queue is
empty, say so as its own finding rather than letting the silence stand for "nothing to do".

**A developer's final report ends by handing off**: what its work unblocked, and what it found that
wants a lane of its own. That is the same discipline as milestone 94's, applied to scheduling rather
than to findings.

**Open decisions live in a file, not in a conversation.** A decision waiting on calef that exists
only in chat scrollback is in exactly the medium milestone 94 was written to abolish, and on
2026-08-04 five of them accumulated there in one day while that milestone was being built. They go
in `design/decisions/` with `**Status: PROPOSED.**`, one section each: what is being decided, the
options, the recommendation with its reason, and what is blocked until it is answered. (They lived
briefly in `design/open-decisions.md`; milestone 114 absorbed that file, and the numbering is the
integrator's at merge like every other section number.)

**And work waiting on calef carries its own label and its own ask** (calef, 2026-08-04). The same
principle one level out: a pull request held for him is a decision, and a queue that exists only in
a chat message is the medium above. Two things, both at the moment the decision to hold is made and
not later, because the failure this prevents is the maintainer forgetting it is holding something:

- **The `needs-architect` label**, so the queue is `gh pr list --label needs-architect` rather than
  a paragraph somebody has to have read. **It names the role, not the person** (calef, 2026-08-05):
  he holds it today and would like a second architect tomorrow, and a mechanism that spells one
  name has that name as its failure mode. Its description carries the reason a thing lands there at
  all: outside standing merge authority, meaning the syscall surface, a new dependency, or a
  `DECISIONS` section owed.
- **A `## What I need from you` comment** naming the specific ask. Three properties make it worth
  writing, and they are what separate it from a link to a diff. It should be **answerable without
  reading the diff**, because the point is to spend calef's attention on the decision rather than on
  reconstructing it. It should **say what happens if he says no**, since a recommendation with no
  stated downside is not a recommendation. And it should **separate what is blocking from what is
  eventually his**, so a naming backlog does not get tangled with a merge decision; milestone 115's
  gate takes `unrecorded` as a truthful answer precisely so that provisional names never block.

**Lane count is set against merge-queue depth, not against how much work exists** (calef delegated
this on 2026-08-04, after the queue reached ten and the oldest pull request starved). A finished lane
does not produce value; a *merged* lane does, and under the require-branches-up-to-date rule the
queue lands **one thing at a time**. So lanes past that rate manufacture merge debt rather than
progress:

| open pull requests | concurrent lanes |
|---|---|
| 0 to 3 | 4 |
| 4 to 6 | 2 |
| 7 or more | 1 |

The numbers are a starting point rather than arithmetic, and the rule that matters is the reason:
**throughput is measured in merged work.** When the queue is deep the bottleneck is CI wall-clock and
the honest move is fewer lanes, said out loud as a decision rather than by quietly not launching any,
which is how it failed three times that evening.

**Prune a lane's worktree the moment its pull request merges**, in the same breath as deleting the
branch and relinking `cricker-dev`. Eight finished worktrees had accumulated by the time anyone
looked, and one of them alone held 3.3 GB.

**The maintainer starts the two watchers at the beginning of every session**, because both are
ordinary loops that die with the session that started them, and a session that forgets has exactly
the gap they were written to close:

```sh
scripts/merge-drain.sh &     # lands every PR not labelled needs-architect, one at a time
scripts/trunk-health.sh &    # says when main goes red, and when it recovers
```

They exist because on 2026-08-04 three duties turned out to belong to whoever happened to notice: two
green pull requests sat unmerged for hours, `main` went red with nobody assigned, and merging one
pull request staled eight others that nothing picked back up. The steward was meant to cover this and
did not, for a reason worth keeping: **it reported and never acted.** A stalled queue announced in a
message is only useful if somebody reads the message. See notes/merge-queue.md, whose BUGS section is
honest that neither script reports its own death.

**Do not try to route this by requesting a review.** GitHub silently refuses a review request from
the pull request's own author: `gh pr edit N --add-reviewer calef` **returns success and sets zero
reviewers**, because every pull request here is authored under calef's account by the `gh` token.
That was tried on 2026-08-04 and the silent no-op looked exactly like a working queue, which is
worse than an error. Assignees and labels do work; reviewers do not.

**Stop and bring it to calef only when it is genuinely his call:** a design fork not already
decided, a test that will not pass after real effort, a hardware or external dependency, or the
machine contradicting the plan. Otherwise proceed and report what you did.

**Keep the documentation current, because a demonstrator's docs are part of the deliverable.**
Every design decision goes in `DECISIONS.md`; every concept and finding gets a note in `notes/`,
indexed in `notes/README.md`. Record the *why* and the honest caveats.

**The standard to aim at is FreeBSD's** (calef, 2026-07-30): the Handbook and the man pages, which
are the best documentation in the field and are the reason a FreeBSD admin can answer a question
without leaving the system. Four things make them that, and all four are things we can do:

- **Task-oriented.** "How do I do X", in order, with the actual commands, rather than a reference
  dump the reader has to reassemble.
- **In-tree and versioned with the code**, so the docs cannot describe a system that no longer
  exists. Already true here; keep it true.
- **Real `EXAMPLES`.** A page without a worked example has not finished explaining itself.
- **An honest `BUGS` section.** FreeBSD man pages document known limitations *in the manual*, next to
  the feature, rather than only in a tracker. This is the one worth copying hardest, because it is
  the convention this project already reaches for by instinct: the map "tie", the spawn caveat, the
  scope notes on parity gaps. **Name the limitation where the reader meets the feature.**

The point is not the format, which is theirs. It is the posture: documentation written for someone
who has to *use* the thing, and honest enough that they trust it when it says something works.

**Anything global to the tree is assigned by the integrator at merge, never claimed by a lane.**
Concurrent lanes cannot see each other, so a lane that reaches for a shared resource is guessing.
Two kinds bit us on 2026-07-30:

- **`DECISIONS.md` section numbers**, three collisions in one day. Preferred: a lane **does not touch
  `DECISIONS.md` at all**, puts the reasoning in `notes/` and in its report, and the integrator mints
  the section at merge. (Milestone 51's calendar lane did exactly this, unprompted, and it was the
  only one of four that caused no conflict.) If a lane must write the section to make its own gates
  pass, the number is **provisional**: say so in the report, and expect renumbering.
- **Counts that span the tree.** The Kani harness count was written as 76 on one branch and 80 on
  another; the merged tree had 95. Both were counted honestly. Take such a number at merge, from the
  merged tree.

**After any renumber, check citations by content, not by running the gate.** `script/decisions
--check` verifies that a cited `§N` resolves to *some* section, never that it resolves to the right
one, so a well-formed wrong citation is invisible to it. This has already produced two of them.

**Some shared state is global to the *machine*, not the repo, and `rustup toolchain link` is the one
that has bitten.** The `cricker-dev` toolchain the `std` farm needs is a symlink in
`~/.rustup/toolchains`, so `xtask std-src` repoints a **user-account-wide** name at whichever
worktree ran it last. Two lanes building the farm race for it, and the loser silently compiles
against a farm inside someone else's worktree; deleting that worktree then breaks the toolchain for
everything, surfacing far from the cause as "override toolchain 'cricker-dev' is not installed"
during an unrelated build. Fix: `rustup toolchain link cricker-dev "$(pwd)/target/cricker-farm"` from
the main checkout. This is the same rule as the paragraph above, one level out: the integrator owns
what is shared, and "shared" is wider than this repository.

**And the instruction "do not run `xtask std-src`" is impossible for a lane that must gate**, which
milestone 57's lane found on 2026-08-01 by reading the code rather than by failing. `script/test`
calls `std_src()` transitively, and a fresh worktree always has a cold farm, so **any lane that runs
the gate takes the account-wide link.** Two instructions this file gave together could not both be
obeyed.

Until `xtask test` grows a flag that skips the farm, the honest rule for the integrator is: **expect
every lane to take `cricker-dev`, and relink from the main checkout at merge**, in the same breath as
pruning the worktree. Do not tell a lane not to do the thing gating requires; tell it what to say in
its report so the relink is not forgotten. That lane also demonstrated the workaround worth knowing:
symlink the worktree's `target/cricker-farm` at the main checkout's farm after checking the stamps
match (`cargo xtask std-stamp`), and `std_src()` early-returns instead of rebuilding.

**Delete a lane's worktree too, and do it before the disk decides for you.** On 2026-07-31 the data
volume hit **zero bytes free** with 42 agent worktrees holding **78 GB**. Two lanes died mid-work and
could not even run `pgrep` to check whether they had leaked emulators, because every tool must create
an output file before it runs. Deleting the branch at merge does not remove the ~2 GB of `target/`
behind it, so **prune the worktree in the same breath**, and `git worktree prune` afterwards. If a
lane is blocked, commit and **push** its work before removing anything: a snapshot on the remote
cannot be lost by a cleanup. The warning signs were noted hours earlier and not acted on, and then
four more lanes were launched on top of them.

**Delete a lane's branch when you merge it, and never use a branch as a filing cabinet.** Forty-seven
branches accumulated in about two days of lane work and had to be pruned by hand on 2026-07-31; this
recurs by default, because merging is what finishes a lane and deleting is a separate act nobody is
prompted to take. So it belongs in the merge, not in a periodic cleanup.

The rule that matters more than the tidiness: **an unmerged branch is either abandoned or it is
holding knowledge that is not on `main`, and the second case is a bug in where the knowledge lives.**
`fix/redoxfs-write-loop` survived that prune because it carried an investigation's conclusion that
`notes/fs-server.md` does not. **Nobody reads branches.** If a branch holds a finding worth keeping,
land the finding in `notes/` and then delete the branch; do not keep the branch as the record.

**Benchmarks and cross-OS comparisons are first-class.** Measure, do not argue. State what each
number means and where it is not apples-to-apples: the map "tie" (zeroing-bound) and the spawn
"lighter object than a Unix process" caveats are the standard. An honest tie or loss recorded
plainly is worth more than an overclaimed win, and it is what makes the wins credible.

**Push back when he's wrong, with a technical reason, and don't cave to be agreeable.** He once
picked async/await because it "sounded more tractable"; the right response was to point out that
cooperative scheduling cannot run an arbitrary ELF binary, so async forecloses the hard work rather
than deferring it. He changed his mind. Do that again when warranted; do not manufacture
disagreement to seem rigorous.

**Correct yourself loudly.** We told him QEMU passes a device tree pointer in `x0`. It doesn't. We
found out by printing it and getting zero, and fixed the note rather than quietly patching over it.
The machine overrules the documentation, and it overrules you; when it does, fix the record on
purpose.

**Explain on request, however basic.** Autonomous by default does not mean opaque: if calef asks
"what is a register?" or "why does `destroy` avoid `SCHED`?", answer properly, from the ground up,
and write it down.

## The rules that hold the codebase together

These come from `DECISIONS.md`. They are cheap to follow and expensive to retrofit.

1. **All architecture-specific code lives under `kernel/src/arch/`.** Assembly, `asm!`,
   system registers, CPU-specific behaviour. If you're writing `asm!` outside `arch/`, that
   is the bug. This is what makes the Raspberry Pi port a new directory instead of a diff
   across every file.

2. **A driver never reaches into a kernel global.** It gets what it needs passed in (a base
   address, later a DMA allocator, later an interrupt registration). See
   `drivers/pl011.rs`: it takes a base address and knows nothing else.

3. **The syscall surface stays narrow and explicit.** It is a boundary, not a habit.

5. **Architectural parity is a gate, not an aspiration** (DECISIONS §19). The targets are
   aarch64, riscv64, and x86_64 (declared, not yet started). A kernel capability ships on every
   supported architecture, proven by the same suite, or a scope note records the gap and the
   plan. If a feature works on one ISA and silently not another, that is the bug.

Rules 2, 3 and 7 are what keep the microkernel option open (7 because a contract you cannot
test is a contract you cannot trust to replace a component behind). We are deliberately **not**
speculatively trait-ifying every subsystem, because that builds the wrong abstraction before
the requirements are known.

4. **Assume weak memory ordering.** We're on ARM, which is the weak one, and that's a gift:
   we cannot develop hidden strong-ordering assumptions the way an x86-first project would.
   Don't squander it.

6. **Taking a dependency is a decision, not a convenience** (DECISIONS §46). The tree's shape is
   thin architectural primitives (`aarch64-cpu`, `spin`, `tock-registers`) or whole subsystems we
   would never write (`smoltcp`, vendored RedoxFS), with **nothing in between**: thirty crates have
   no external dependencies at all. Write it if it is on the verification path, because you cannot
   restructure someone else's crate to make a model checker tractable. Vendor it if correctness is
   won by *exposure* rather than by reading the spec, which is why §46 says write the calendar and
   vendor the crypto.

7. **Anything two binaries must agree on is a crate, never a `#[path]` module** (calef, 2026-08-01).
   If a constant, an opcode, a layout, or an error code is shared by more than one program, it goes
   in `crates/` and is depended on. `#[path = "x.rs"] mod x;` is not an option.

   **Three reasons, and the second is the one that matters.**

   It removes a category that nothing enforces. A `#[path]` module is neither a program nor a crate,
   so a reader meeting `cseam::GRANT_VA` cannot tell what they are looking at, and `user/src/` held
   48 programs and 3 modules with nothing distinguishing them.

   **A `#[path]` module inside a `no_std` binary is unreachable by host tests and by Kani.** This
   project's entire method is pure logic in host-testable crates plus machine-checked proofs, and a
   shared module opts out of both. `cseam` is the case that proves it: it holds the address-space
   layout and constants **deliberately written twice**, once in Rust and once in `user/c/c_seam.c`,
   with nothing checking that the two agree. A drift there shows up as a C component scribbling on
   the wrong page, arbitrarily far from the edit.

   And it makes location self-enforcing for free. Once shared definitions live in `crates/`,
   everything in `user/src/` is a program, with **no files moved** and no convention to remember.

   This was already the tree's practice for seven crates (`fs_proto`, `sink_proto`, `cred_proto`,
   `clock_proto`, `entropy_proto`, `ntp_proto`, `gfx_proto`) and the exceptions had no recorded
   reason; `cseam.rs`'s header describes the `#[path]` mechanism without ever justifying it.

## calef names the crates, the programs, and the shared modules

**Contributors are referred to by their GitHub username** in prose, attributions, records, and
lane reports; legal names appear only in legal and authorship strings (`Cargo.toml` authors,
licenses, patch `From:` headers). The reason is the mechanism, which is this file's recurring
test for a rule: a username is unique and matches the identity every pull request and
`git log --author` already carries, so the records and the tooling agree on who someone is, and a
grep for a contributor finds them instead of every other person sharing a first name. The worked
example is the architect himself: calef, renamed from Chris throughout on 2026-08-15 (UTC, per
the dates convention) at his request.

**The name of a crate, a program, or a shared module is calef's call, not a lane's and not yours**
(2026-08-01). This is the same rule as `DECISIONS.md` section numbers, one level up: it is global to
the tree, so it is decided by the person who can see the whole tree.

**Shared modules are in scope for a reason.** `user/src/` used to hold 48 `[[bin]]` programs and a
handful of modules compiled into them with `#[path = "..."] mod ...`, with **nothing in the naming
distinguishing them**, so a reader who tried to run `cseam` was misled by the directory. Rule 7
retired that category on 2026-08-01: what two binaries share is a crate, and what remains in
`user/src/` beside the programs is single-consumer submodules (`vnet`, `netcli`), which are ordinary
Rust. `script/lint` now counts consumers per `#[path]` target and fails at two.

The count in an earlier draft of this paragraph said "three modules" and was wrong: the grep that
produced it matched only single-line includes, and several were two lines. Take a count from the
merged tree, with a pattern you have checked against the real shapes. A shared module's name has to answer a question a program's
name never raises, which is *"where does this get compiled into?"*, and that makes it a naming problem
of its own rather than a smaller version of the program one.

The reason is his: **names are what make this OS accessible to humans and to LLMs.** A reader meets a
name before they meet anything else, and in a capability system the name is often the only thing that
says what a program can *do*. `DECISIONS.md` §39 already says a name is a claim; this says who gets to
make the claim.

The evidence that it needed a rule is the tree itself. `dwarden` is named for what it **holds** while
its two siblings are named for what they **serve**, so a reader who correctly infers the scheme gets
it wrong. `conx` has no recorded expansion anywhere: not in §41, not in `notes/live-replacement.md`,
not in the commit that introduced it. `cseam.rs` sits among 48 programs and is not one; it is a shared
module. Every one of those was a locally reasonable choice by whoever was mid-task.

**How to work it.** Propose names with what each thing actually does, and wait. A lane that needs a
new program or module ships a **provisional** name, says so in its report, and expects it to change;
the integrator surfaces it. Never rename on your own initiative either, because a rename is a naming
decision with extra steps.

**Crates are in scope too** (extended 2026-08-01). They are the most reader-facing names in the tree:
a newcomer greps `crates/` before they ever open `user/src/`, and a crate name appears in every
`Cargo.toml` that depends on it, in every `use` statement, and in the dependency graph an outsider
reads to understand the shape of the system.

The crate names have the same three failure modes the programs did. **Abbreviations** that need a
decoder (`capsh`, `uheap`, `vt`). **Generic words** that could name almost anything in an operating
system (`compose`, `measure`, `regions`, `slots`, `caps`, `frames`). And **standard terms that are
genuinely right** and should not be touched (`elf`, `pci`, `dtb`, `gpt`, `ipc`, `paging`, `glob`,
`asid`), because renaming those would cost a reader the recognition the whole tenet exists to buy.
That last group matters: this rule is not a licence to rename everything, and a name a reader already
knows from outside this project is the best name available.

**Name things with nouns** (calef, 2026-08-01). A crate, a program or a module is a *thing*, so it
takes the name of a thing: `capability`, `grant_plan`, `user_heap`, `video_terminal`, `line_editor`,
`fs_subtree_caretaker`. A verb names an action and a namespace is not one, which is audible at the
call site: `line_edit::expand_output` reads as an instruction where `line_editor::expand_output`
reads as a location.

The exception is a **term of art that happens to be a verb**, where the word is the one the field
already uses. `bind` (§50) is Plan 9's, and respelling it as a noun would assert novelty where there
is none. That is the paragraph above, not a hole in this one.

Three crates predated this rule and were settled by it on the day it was written: `compose` becomes
`compositor`, `measure` becomes `measured_boot`, and `dma_validate` becomes `dma_validator`. Each had
named itself a noun in its own first line while carrying a verb as its name.

**A crate and a program may share a name, and it says something when they do**: the crate is that
program's logic, lifted out so it can be host-tested and Kani-reachable while the program keeps the
IO. `coremark`, `line_editor` and `compositor` are all this pair, and splitting the names would hide
a relationship worth seeing.

### The convention: one rule per domain, and each domain's own

Crates already do this (`fs_proto`, `cred_proto`, `user_rt`). Programs did not: **0 of 57** used an
underscore, so multiword names were squished (`fsclient`, `sysinit`, `credcli`).

An earlier draft of this rule had two tiers, short names for programs a user types and underscores for
programs only the system spawns. **calef rejected it, correctly**: the category is not a stable
property of a program. `wc` was internal plumbing and became a prompt-typed pipeline stage in one day,
and a convention keyed to something that changes produces renames. It is also not how Unix got its
names; the terseness of `ls` is an emergent pressure on words people type constantly, not a rule
anyone wrote down. Codifying an emergent property turns it into a classification chore that every
contributor has to get right.

So: one rule, no branch to get wrong. A short name for a typed command is then a *choice its author
makes*, not a convention to apply, and nobody needs a rule to know `wc` beats `word_count`.

**But `snake_case` is the rule for Rust things, not for everything**, and an earlier draft of this
section said "everywhere" and was wrong. Three domains, each keeping its own convention:

| Domain | Form | Because |
|---|---|---|
| Crates, programs, modules | `snake_case` | Rust's own convention, and what the tree already does |
| `script/` and `scripts/` entry points | `hyphens` | shell commands are hyphenated everywhere (`apt-get`, `pkg-config`, `docker-compose`); an underscore in a command name reads as a mistake |
| Ordinary markdown (`notes/`, `design/`) | `hyphens` | filenames become URL slugs in every static site generator, and hyphens are word separators in a URL where underscores are joiners |
| Repo-root markdown | `SCREAMING_SNAKE_CASE` | **GitHub behaviour, not style.** It recognises `README.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, `CONTRIBUTING.md` and links them in its UI; get the name wrong and the Security tab does not find your policy |
| A directory holding a Rust package | named **exactly as the package**, so `snake_case` | the directory and the package are one thing with one name; thirteen under `crates/` already do this |
| Any other directory | `hyphens` if it needs two words | a directory is a path element, and paths are hyphenated outside this repository |

The directory rows are the same principle one level out, not a new tier: a package directory is a
Rust name, and everything else is a path. Three directories violated them when this was written
(`fs-server/`, `tools/redoxfs-host/`, and `user-std/`, whose package was called `hellostd` and
matched neither); **milestone 63 fixed all three on 2026-08-01**, along with about twenty other
names. The rule is now descriptive of the tree rather than aspirational.

**This is not the two-tier rule calef rejected**, and the difference is the one he identified. That
split was *within* one domain, keyed on an **unstable** property: `wc` moved from internal plumbing
to prompt-typed pipeline stage inside a day. These splits are *across* domains on a **stable**
property. A file either is a Cargo target or is an executable in `script/`; `script/test` will never
become a `[[bin]]`.

It is also the same guard rail as "standard terms are already right", applied to **form** rather than
vocabulary. We do not rename `elf`, and we should not respell `supply-chain` either: a name whose
shape a reader already knows from outside costs them nothing.

**One constraint to know:** `crickerfs` caps archive names at `NAME_LEN = 32` bytes, raised from 24 on
2026-08-01 so `os_primitives_benchmarker` would fit. It can be raised again, and there is no data
migration because every image regenerates from that crate, but it costs directory entries per block.
(The old warning that it also costs kernel stack was stale: `Fs` stopped holding entries as a fixed
array when the FS-server stack bug was fixed. See notes/crickerfs.md.) Do not let it pick a name; do
not spend a format change on bytes nothing needs.

## The syscall surface is a boundary, not a habit

Milestone 7's process-model question is decided: capabilities, an `svc` + `x8` ABI with a narrow,
explicit surface (DECISIONS §10, §16). The discipline that remains: the surface stays small and
every method is deliberate. New methods are fine within the established capability model (object
revocation added `Untyped::SPLIT` and `DESTROY` this way); **record each new method's semantics in
`DECISIONS.md`, not just in code.** A method that does not fit the model, or a brand-new syscall
number, is a design fork, raise it before building it.

## Testing

`script/test` (a thin wrapper over `cargo xtask test`) boots the kernel under QEMU and reports
pass/fail via semihosting. The `script/*` commands are the normalized "Scripts to Rule Them All"
front door (`setup`, `test`, `server`, `console`, ...); they delegate to `cargo xtask`, which is
still the engine and exposes more (`gdb`, `objdump`, `image`). See notes/scripts.md.

Tests should prove something specific that nothing else would have done for us. The four in
`main.rs` are the model: `.bss` was zeroed (nobody else would have), `sp` is 16-byte aligned
(a bug here is a mystery crash), we're at EL1 (we are where we think we are). Don't add
filler tests.

Pure logic (allocator algorithms, page-table math, scheduling policy, filesystem parsing)
belongs in crates that compile for the **host**, so most tests run in milliseconds without
an emulator.

## Commits

One purpose per commit. The message explains **why**, not what (the diff shows what). If a
commit records a correction or a surprise, say so in the message. See the milestone 1
history for the shape.

**Commit early and push, then curate before reporting.** These two rules read as opposites and are
not, and the resolution is a criterion rather than a compromise: **`git blame` is what a commit is
for.** A reader tracing why a line looks the way it does must land on a commit that explains it.

So while working, commit whenever a piece works and push whenever a commit exists, because a pushed
branch survives a dead session, a killed process and a laptop that will not wake, and nothing else
does. On 2026-08-04 a lane sat on seven modified files with **zero commits for hours**; had that
worktree been pruned the work was gone, and it was caught by inspection rather than by any
mechanism. Uncommitted work in a lane worktree is the one thing no part of this system protects.

Then, before reporting, **squash the checkpoints into the purposes** and force-push. A checkpoint is
for the lane's own safety and has no reader; a purpose commit has one.

**Squash against the base commit you branched from, never against `origin/main`**, and this trap was
sprung the day the rule above was written (2026-08-04). Agent worktrees share one `.git`, so
`origin/main` moves under a lane while it works: a developer that ran `git reset --soft origin/main`
to squash silently staged **four other lanes' files as its own**, including a deletion, and caught it
only by reading `git status` before committing. Record the base SHA when the branch is cut and squash
against that. The wider rule it belongs to: in a worktree, `origin/*` is not a fixed point.

**Never squash across purposes, and never squash-merge a branch.** Milestone 96's lane put the
loader unification in its own commit *ahead of* the migration precisely so that a boot failure could
not be ambiguous between two changes, which is the whole reason that structure exists. A
squash-merge would have destroyed it. The merge commit carries the pull request's title, so
`git log --first-parent` already reads as one entry per piece of work while the detail stays
reachable underneath.

The exceptions worth keeping unsquashed: a commit that records a correction or a surprise, and a
commit whose separateness is itself the argument (96's loader, above).

## Comments

The kernel is commented far more heavily than production code would be, deliberately. A
comment should explain a constraint the code can't show: *why* `sp` must be set before the
first `bl`, *why* `.bss` needs zeroing by hand, *why* the baud divisors are ignored by QEMU
but needed by a real Pi. Cross-reference the notes (`See notes/stack.md`) so the code and
the glossary stay stitched together.

Do not write comments that restate the next line.

## Style

calef's global preferences apply, and they matter here because the notes are prose he'll
reread for months:

- No em-dashes. Use commas, periods, semicolons, or parentheses.
- No "delve", "comprehensive", "landscape", "moreover", "furthermore", "notably", "it's
  worth noting", "straightforward".
- No sycophantic openers, no filler conclusions that restate what was just said.
- Plain, direct language. Vary sentence length. Write like a person.

## Never leave QEMU running

A cricker-os kernel that has finished its work calls `arch::halt()`, which is `loop { wfi }`.
It never exits. So QEMU never exits either, unless something kills it or the kernel asks the
host to terminate via semihosting (which only the test build does).

Two consequences:

1. **Every interactive/demo QEMU run must be bounded** (see the note in Environment below).
2. `halt()` must use **`wfi`, not `wfe`.** QEMU implements `wfi` as a real vCPU halt and the
   host thread sleeps; it merely spins on `wfe`. A halted kernel using `wfe` burns **99.7% of
   a host core**. With `wfi` it is 0.0%.

## Environment

- macOS on Apple Silicon (itself aarch64, which is a nice coincidence: kernel assembly is
  the same ISA the laptop runs)
- QEMU via Homebrew, `qemu-system-aarch64`
- Rust nightly, pinned in `rust-toolchain.toml` (needed for `custom_test_frameworks`)
- Target: `aarch64-unknown-none-softfloat`
- `timeout(1)` does not exist on macOS, and **`perl -e 'alarm N; exec @ARGV'` DOES NOT WORK
  ON QEMU.** QEMU installs its own `SIGALRM` handler and swallows the alarm, so the process
  runs forever. This is not theoretical: it leaked eleven QEMU processes over one day of
  development, burning a combined 729% CPU, the oldest with eight hours of CPU time on it.

  Use `scripts/qemu-bounded.sh <seconds> <cmd...>` instead. It uses SIGTERM, which QEMU does
  honour, and it detaches the killer so it survives a pipeline whose reader (`head`) exits
  early.

  **After any session that ran QEMU, check `pgrep -x qemu-system-aarch64` and clean up.**

  **That check is not sufficient after you kill a harness, and on 2026-08-02 it took four attempts
  to notice.** Killing a loop script does not kill its descendants: `pkill -f hunt-...` left
  `cargo xtask test` running, which kept starting fresh QEMUs. So every check honestly reported "no
  qemu" and the next command found one holding `target/crickerfs.img`, which then failed unrelated
  test runs with `Failed to get "write" lock` and looked like a bug in the code under test.

  Two habits fix it. **Ask who holds the file, not whether a process matches a name**:
  `lsof target/crickerfs.img` names the holder even when your pattern does not. And **kill the tree
  at its root**: walk `ps -o pid,ppid,command` up to the harness and kill that, or the loop simply
  starts another child. `pgrep -l qemu` is also worth preferring to `pgrep -x qemu-system-aarch64`,
  because it matches both architectures and does not depend on getting the full name right.
