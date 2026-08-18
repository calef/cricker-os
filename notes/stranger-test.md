# The stranger test: the instrument, the rubric, and the runs

Milestone 117 turns the third principle into a measurement. The principle is CLAUDE.md's own
sentence: *could a competent stranger, with only this repository, reach a passing build and a correct
mental model without opening a chat window?* Where the answer is no, that is a bug in the tree and
not in the stranger.

This note is the instrument. **It is written before any run**, deliberately, because a rubric written
afterwards grades generously: every answer looks close enough once you know what it was supposed to
be.

## Why the tree cannot grade itself

calef cannot take this test; he wrote the system. Nor can any agent that has worked in this tree, and
by 2026-08-14 that is most of them. An agent that spent a night merging pull requests here knows why
`nife-dev` is a symlink, what a lane is, and that `script/lint` fails on a branch prefix.
**Knowing the answer disqualifies you from being the instrument.**

So the run needs a fresh context that has never seen the repository, handed the repository and a
task, and nothing else. No brief explaining the conventions. No pointer to the right note. No answer
to any question it asks.

## The protocol

1. **Fresh context, not a summarised one.** A handoff that says "read CLAUDE.md first" has already
   given away the finding a newcomer would not know to make.
2. **One task, stated the way a new contributor would receive it.** Not "evaluate the docs", which
   invites a review rather than an attempt.
3. **Every question it asks is a defect**, recorded verbatim. The questions are the deliverable, more
   than any score is.
4. **Every confident wrong answer is a worse defect**, because a document that misleads costs more
   than one that is silent.
5. **No help mid-run.** If it is stuck, that is the measurement finishing, not a prompt to intervene.

### What run 2 withholds, and what it cannot, decided 2026-08-16 before the run

Run 1's first `BUGS` entry said run 2 must not have this note in the tree it is given. Trying to
obey it exactly is what showed the entry was asking for something the tree cannot supply, so the
rule this run used is narrower and is written here rather than left as an intention:

**Withhold the answer key, not the fact that a test exists.** Each stranger got a clone of
`d6a16b7` with `notes/stranger-test.md` and its `notes/README.md` entry removed and the deletion
amended into the tip, so the working tree is clean and no `git status` line advertises it. Nothing
else was touched.

**The mentions of run 1 stay, because removing them would fabricate a different repository.**
`README.md`, `DECISIONS.md`, `notes/README.md` and the milestone 117 block all cite the first run,
and three of them do it while making a point a newcomer needs (why `CLAUDE.md` is misnamed, where
`§N` resolves, what `adding-a-program.md` is for). A tree with those cut is not the tree under test,
and the milestone's own sentence is *with only this repository*. So a run 2 stranger can discover
that this project tests its onboarding; what it cannot discover is the rubric's "pass means" column,
which is the part that made run 1's answers uncountable.

**The stranger writes its log as it goes, and that is a change to the instrument.** Run 2's first
attempt died part way and left nothing at all: the findings lived in the stranger's own context and
went with it, which is rung four of the ladder wearing a different hat. So the second attempt keeps
an append-only journal outside the tree, written before and after each step, with the questions and
the had-to-work-it-out items recorded at the moment they are hit rather than assembled at the end.
The cost is real and belongs in `BUGS`: a stranger told its confusion is the deliverable is watching
itself, which run 1's stranger was not.

**The machine is no longer cold, and the build half is weaker for it.** The maintainer ran
`cargo --version` inside the repository while getting oriented, which installed the pinned nightly
from `rust-toolchain.toml`, and the attempt that died had already installed `qemu-system-aarch64`
and `qemu-system-riscv64`. So B2 and B4 measure a partly warmed machine: whatever those two steps
would have cost a newcomer, this run cannot see. A cold measurement wants a fresh container and is
worth doing separately rather than pretending this one is it.

**The strangers were subagents of a maintainer session whose working directory is the repository**,
which is a weaker isolation than run 1's container and may hand them `CLAUDE.md` before they choose
to read anything. That would contaminate the reading-order row and four of the eight mental-model
questions, all of which `CLAUDE.md` answers directly. It is measured rather than assumed: each
stranger is asked afterwards what it read and in what order, and what was in front of it before it
chose. A run whose answer is "the constitution was already there" reports no B1 and no M3, M5, M6 or
M8, rather than reporting them generously.

### What run 3 changes, decided 2026-08-18 before the run

Run 2's second `BUGS` entry names the one thing run 3 must fix, and it is not the rubric leak:
**the stranger must be a process that cannot see this repository except through the tree it is
handed.** Run 2's strangers were subagents of a maintainer session whose working directory was the
repository, so `AGENTS.md` arrived in their context before they chose to read anything, and five of
the eight rubric rows went unscored as a result. Everything below is the design that closes that
hole, written down before the run so it cannot be graded generously afterwards.

**The stranger is a separate process, not a subagent.** A fresh `claude` CLI process, started from
a shell, with no conversation history and no brief beyond the task. A subagent inherits its parent
session's project instructions; a separate process does not have a parent.

**Its working directory is the clone's parent, not the clone.** This is the whole mechanism and it
is one line of shell. Project instructions are loaded from the working directory and its
*ancestors*, never its descendants, so a process started in `.../run3/` with the repository at
`.../run3/nife/` is handed no `AGENTS.md` at turn zero. The file is still in its tree, exactly as a
stranger who cloned from GitHub would find it, and the stranger can open it the moment it decides
to. **That is the difference the milestone is about**: whether the tree tells a newcomer to read
the constitution, rather than whether the constitution is good once read. Run 2 could not ask that
question because the answer was already in front of its stranger.

**The answer key is withheld the same way run 2 withheld it**, because that rule worked as far as
it claimed: the clone has `notes/stranger-test.md` and its `notes/README.md` entry removed, amended
into the tip so the working tree is clean and no `git status` line advertises the deletion. Nothing
else is touched, and the in-tree mentions of runs 1 and 2 stay, because a tree with those cut is not
the tree under test.

**The journal stays**, with its cost restated rather than rediscovered: a stranger told its
confusion is the deliverable is watching itself. It writes an append-only log outside the clone,
before and after each step, because run 2's first attempt died and took its findings with it.

**What this design still cannot give, stated now rather than after the result.** The machine is
warm, and warmer than run 2's: this is the architect's own laptop, with the pinned nightly, both
QEMUs, and a populated cargo registry cache already present. **B2 and B4 are therefore not
measured on the install path at all**, only on whether the documented sequence runs and says true
things. A cold build half wants a container and is a separate run; claiming this one is it would be
the generous grading the rubric exists to prevent. The stranger also inherits the operator's
*global* user preferences file, which mentions no operating system and no project in this tree, and
the per-project memory directory is keyed to the repository's own path so a clone under `/tmp` does
not load it.

**And the harness's author is not a stranger, which is the residual and it is not small.** This
lane's developer has read `AGENTS.md` in full. It wrote the task, chose the isolation, and reads the
result, so its judgement about what counts as a defect is contaminated even though the stranger's
answers are not. The mitigations are that the task text is run 1's and run 2's verbatim, the rubric
predates all three runs, and the stranger's questions are recorded as it asked them rather than
summarised into findings.

### What run 4 changes, decided 2026-08-18 before the run

Run 3's `BUGS` entry names one fix and prices it at one line, and that is what run 4 applies.
Everything else about the configuration is run 3's, deliberately, because a run that changes two
things measures neither.

**The harness's own files leave the stranger's working directory.** Run 3's logs were called
`stranger3-stream.jsonl` and `stranger3-stderr.log` and sat beside the clone, so the stranger's
first `ls -la` told it which run it was before it had read a byte of the project. Run 4's logs go in
a **sibling** directory: the stranger's working directory contains the clone and nothing else. The
directory names carry no run number either, since a path in an error message is as readable as a
directory listing.

**The isolation mechanism is unchanged and is verified the same two ways**, because it is the thing
that made run 3 scorable: a separate `claude` process rather than a subagent, `--safe-mode`, working
directory is the clone's *parent*, since project instructions load from ancestors and never from
descendants. A throwaway probe in the same configuration is asked what project-instructions files
are in its context before the real run, and the stranger itself is asked afterwards.

**The answer key is withheld the same way**: this note and its `notes/README.md` entry removed and
the deletion amended into the tip, nothing else touched, working tree clean. The in-tree mentions of
runs 1 through 3 stay, for the same reason as before.

**What is actually new to measure, and it is why this run is worth taking rather than repeating.**
`CONTRIBUTING.md` and the README's `## Start here` reading order both landed on 2026-08-18, after
run 3's clone was cut. **No stranger has seen either.** Run 3's B1 failed because there was no
reading order and its stranger built one by instinct, reaching `AGENTS.md` twelfth and
`crates/abi/src/lib.rs` far too late. So B1 is the row this run exists to move, and the specific
questions are whether a stranger finds the reading order at all, whether it follows it, and whether
following it costs it the two things instinct cost run 3.

**The task text stays verbatim from runs 1, 2 and 3.** Changing it would make the four runs
incomparable, and the comparison is the only thing that distinguishes a milestone from an audit.

**What this run still cannot give.** The machine is the architect's laptop with the pinned nightly,
both QEMUs and a warm cargo cache, so **B2 is not measured and B4 is measured only against the
documented sequence**, exactly as in run 3. The machine is also loaded by other lanes gating in
other worktrees, and this time the tree can say so: `script/test` prints the host load average
beside a failing timing leg as of 2026-08-18. Whether a stranger meeting that message needs `uptime`
anyway is itself a measurement this run gets for free.

## The rubric, written 2026-08-14, before the first run

Two halves. Only the first is mechanical.

### The build

From a clean clone: does `script/setup` then `script/test` reach green, following only what the
repository says?

| # | checked | pass means |
|---|---|---|
| B1 | a reading order exists | the stranger knows where to start without guessing among `README.md`'s sections |
| B2 | `script/setup` completes | or fails with a message that says what to install |
| B3 | `script/test` reaches green | on at least one architecture |
| B4 | nothing undocumented was needed | no step the reader had to know that no file states |

**Record what it actually took**, including anything the reader had to know that no file said. That
last row is the one that matters; the first three are hygiene.

### The mental model

Each question below is one the tree *claims* to answer. **Grade against what the tree actually says,
not against what a maintainer knows.** A question the tree answers only in a commit message is a
question the tree does not answer.

| # | question | pass means |
|---|---|---|
| M1 | What is a capability here, and what is the relationship between holding a reference to a thing and being permitted to use it? | names that holding the reference *is* the permission, with no separate check |
| M2 | Why is there no ambient network, and what must a program hold to reach one? | names a held capability rather than a config flag or a permission bit |
| M3 | Where does architecture-specific code live, and what breaks if it lives elsewhere? | `kernel/src/arch/`, and that the port becomes a diff across every file |
| M4 | What does `BUILT` mean on a roadmap row, and `PARTIAL`? | that it is a claim about the tree, and that the index and the block must agree |
| M5 | Why is there a `crates/` and a `user/src/`, and what decides which? | shared-by-two-binaries goes in `crates/`; host-testable and Kani-reachable is the reason |
| M6 | What is a `BUGS` section for? | a promise about known limits, not an apology, and next to the feature |
| M7 | How would you add a program, and what must you declare about it? | the grant manifest, and that a provisional name is expected |
| M8 | Who decides a name, and what provenance states can one be in? | calef; the states `script/names` accepts, which is four as of §89: `ratified`, `recorded`, `unrecorded`, `provisional` |

### Amendments, 2026-08-18, forced by run 3 and applied before run 4

The rubric section above says a stranger falling down somewhere it does not ask about means the
rubric is what needs amending. Run 3 falsified two rows rather than falling outside them. Run 3's
lane recorded the corrections here and left the table as written; **run 4's lane applied them to the
table itself, on 2026-08-18 and before its run started**, because a rubric that says one thing in
its table and another four paragraphs below is two rubrics, and the next run would grade against
whichever it read. Both rows above now carry the amended wording. What changed and why:

**M8's premise is stale.** It asks for "the three provenance states" and there are **four**: §89
landed `provisional` on 2026-08-16, ten days after the rubric was written, and
notes/adding-a-program.md states three and then corrects itself to four in the same section. The
stranger answered four and said the question was out of date, which is the better answer and would
have scored as "wrong" against the table as written. A rubric that predates a decision grades
against a tree that no longer exists. **The amended row states no count**, because the
count is the part that went stale and a row that counts something will go stale again; it asks what
states exist and points at the gate that enumerates them.

**M1 quotes a phrase the tree does not use.** "Designation is authorization" is object-capability
vocabulary from outside this project; the stranger looked for it, did not find it, and flagged that
it was importing the phrase rather than reading it. What the tree says in its own words is
`swish`'s banner, *"naming a resource in a command IS granting it"*, and `grant_plan`'s
`Refusal::NoSuchProgram`. **A rubric row that quotes a phrase the tree never wrote tests whether the
stranger already knew the field**, which is the opposite of what it is for. **The amended row asks the
question without importing anyone's vocabulary**, so a stranger can answer it from `crates/abi`,
from `notes/capabilities.md`, or from `swish`'s banner (*"naming a resource in a command IS granting
it"*) rather than by recognising a phrase.

**Scoring is per question: answered, partly answered, wrong, or absent.** "Wrong" is worse than
"absent" and is recorded separately, because a misleading document costs more than a silent one.

## The honest limits, stated before the result exists

**An agent is not a person.** It will not get bored, will not give up out of frustration, and will
read further before asking than a human would. So every number this instrument produces is a **lower
bound** on the friction a real newcomer would meet, and any report of it must say so.

**One run measures; two show whether the fixes worked.** A single pass is an audit. The milestone is
the second run, with a different stranger, after the worklist is fixed.

**The rubric can be wrong.** These eight questions are what the tree claims to answer as of
2026-08-14. If a run shows a stranger falling down somewhere the rubric does not ask about, the
finding is real and the rubric is what needs amending.

## Runs

### Run 1, 2026-08-14: an x86_64 container, no QEMU

**Task given:** "Get the project building and its tests passing, then write up what this system is
and how you would add a new user program to it." No brief, no pointers, no answers.

**The headline finding is the one nobody in the tree could see.** The workspace had not built on an
x86_64 host since 2026-08-03. `--exclude` removes a package from the test *selection*, not from the
dependency graph, so excluding `user_rt` while four crates depended on it unconditionally left it in
the build. **CI moved to `ubuntu-24.04-arm` the same day those dependencies landed**, where the EL0
assembly compiles by accident, so the one gate that would have caught it ran on the only architecture
where the bug is invisible. `script/lint`'s comment asserted the opposite and was wrong in both
directions. Fixed, with a gate that derives the bare-metal set from `cargo metadata` rather than
maintaining a list.

**The stranger rejected its own brief to find it.** It was told the container had no QEMU and that
the kernel tests could not run. It installed QEMU anyway to test the premise, watched `script/test`
fail with the same errors *before* QEMU was invoked, and reported: *"The machine overruled the
brief."* That is this project's own rule, applied by an agent that had not read it.

**Its other findings, all fixed in the same pass:** `DECISIONS.md` did not exist while the whole tree
cited `§N` (a signpost now does); no document described how to add a user program (`adding-a-program.md`
now does); `notes/program-manifest.md` listed five fields against the code's ten, so following it
produced a struct that would not compile; and `CLAUDE.md` is the project's constitution in a filename
that tells a human it is not for them, which the README now says out loud.

**Where the instrument failed, which it found itself.** While grepping for "designation is
authorization" it hit **this note**, read the rubric including the "pass means" column, and disclosed
it unprompted: it named questions (g) and (h) as contaminated, re-derived both from primary sources,
cited those, and told the reader to discount them anyway. **The rubric is in the repository the
stranger is told to read**, and nothing in the protocol anticipated that. See `BUGS`.

**Scoring is deliberately not recorded here.** Two of the eight answers are contaminated, so a score
would be a number with a footnote, and the questions it asked are worth more than the number would
have been. The full report is in the pull requests that carry the fixes.

**Run 2 is owed**, with a different stranger, after these fixes. That is what makes this a milestone
rather than an audit: one pass measures, two show whether the fixes worked.

### Run 2, 2026-08-16: a stock Linux box, and a stranger that was not one

**Task given:** the same words run 1 got, plus a journal. "Get the project building and its tests
passing, then write up what this system is and how you would add a new user program to it," with an
append-only log written before and after each step, because run 2's first attempt died part way and
left nothing behind at all.

**It got to green, and the build half is the finding.** `script/test` exits 0 on both ISAs (260
aarch64 and 263 riscv64 kernel assertions under QEMU, on top of the host crates), `script/lint`,
`script/fmt --check`, `script/names --check` and `script/shell-check` all pass, and `script/verify`
reached 91 harnesses across 17 of 19 crates with no failures before the stranger stopped waiting on
`calendar` and `glob`. It also added a program, `doubler`, and answered `doubler 21` at the prompt on
both instruction sets, which is the only way to test the how-to page rather than read it.

**`script/setup` cannot complete on a stock Linux box, and had not been able to for weeks.** No
Ubuntu release ships a QEMU with `riscv-iommu-pci`, so `script/qemu-check` hard-fails by design;
`script/bootstrap` is `set -e`, so it ended there, before the clang step; and the remedy the error
named, `script/ci-qemu`, described itself in its own first line as **"CI only"**. The reader was told
to run a script that told them not to. Every contributor's machine was already warm, so nobody had
met it. Fixed: bootstrap now prints the whole sequence on Linux, and `ci-qemu` no longer claims to be
for CI alone.

**The four corrections to `notes/adding-a-program.md`** are the second half, and they are the page's
own BUGS entry coming true: it asked the first person to add a program against it to correct whatever
it got wrong. The aarch64 initrd has a newer list the page did not know about, so following it wrote
eight lines of dead boilerplate. The riscv64 side is two edits rather than one, and missing the
`--bin` half fails the build on a file cargo was never asked to produce. The `grant_plan` step is six
edits rather than four, and the two the page omitted are `from_name()` (without it the program is
unreachable from the prompt) and `PROG_COUNT` (whose own comment says forgetting it is an
out-of-bounds panic in init rather than a compile error). And the page did not warn that the build
will fail in `crates/swish`, which is the design working.

**One real code defect, found because the manifest already knew the answer.** `swish`'s `caps`
preview printed the `arg` line under `matches!(e.prog, Prog::Worker)`, a hand-maintained second copy
of a fact the manifest holds. `Worker` is the only argument-taking program today, so the tree could
not see it; the next program to take an argument would have previewed `arg (none)` and then been
handed the argument anyway. That is the worst direction for that line to be wrong in, since the next
thing it prints is that reading the command is reading its whole authority. Fixed to read
`manifest().arg`, with a test that sweeps every `Prog` rather than naming one, because a test naming
`Worker` would have passed against the bug.

**The `provisional` trap**, which is §89: AGENTS.md tells a lane to ship a provisional name,
`script/names` accepts only `ratified`/`recorded`/`unrecorded`, and writing the rules' own word gets
you a red gate. Two programs already work around it in prose. The decision is calef's; the page now
documents the trap either way.

#### What run 2 cannot claim, and it is most of the mental-model half

**The stranger was not a stranger, and it said so itself.** Asked afterwards what had been in front
of it before it chose to read anything, it answered that `AGENTS.md`'s full contents arrived in its
context at turn zero, from the maintainer's checkout it had been told not to read, and that it used
that document throughout while **never opening the copy in its own tree**. Every "AGENTS.md says X"
in its write-up is really the other checkout's copy, asserted as though read.

So the pre-registered discount applies, and the rows it covers are reported rather than scored:
**B1** (a reading order exists) and **M3, M5, M6, M8**, all of which `AGENTS.md` answers directly.
What survives is what the tree had to supply on its own: **M1** and **M2** it answered from
`notes/capabilities.md` and `crates/abi`, quoting the note's own "you did not *hold* anything, you
**said a name**"; **M7** it answered by doing it, which is the strongest evidence in either run and
is exactly where the tree turned out to be wrong in four places. **M4** is weak: it read
`design/roadmap/README.md` only in part.

**It also saw that the repository instruments stranger runs, three times, and did not conclude it was
being measured.** `README.md`, `notes/adding-a-program.md` and `DECISIONS.md` all cite run 1 by name;
its own grep surfaced `design/roadmap/117-newcomer-onboarding.md` beside the page it wanted, and it
chose the page and never opened the block. That is the withholding rule working exactly as written
and no better: the fact leaked, the answer key did not, and the run stayed honest because the
stranger disclosed rather than because the tree hid anything.

**The one thing this run measured better than run 1** is the build from cold on Linux, and even that
is partial: the machine had the pinned nightly already, and a dead first attempt had installed both
QEMUs before the real run started.

### Run 3, 2026-08-18: a process that could not see the repository, and a harness that told it anyway

**The isolation worked, and that is the deliverable.** Run 2's unresolved `BUGS` entry demanded a
process that cannot see this repository except through the tree it is handed. Run 3 was a separate
`claude` process rather than a subagent, started with `--safe-mode` (which disables project
instruction discovery, skills, plugins, hooks and MCP) in a directory whose *child* was the clone,
so no `AGENTS.md` could be loaded from an ancestor. **It was verified rather than assumed**, twice:
a throwaway probe in the same configuration answered `NONE` when asked what project-instructions
files were in its context, and the stranger itself, asked afterwards, answered *"No. Before my first
tool call my context was the system prompt and your task message... I had not read a byte of the
project's prose."* Five rubric rows that run 2 had to report unscored are scored here.

**The clone was `9495d6ba` with the answer key withheld the same way run 2 withheld it**: this note
and its `notes/README.md` entry removed and the deletion amended into the tip, so the working tree
was clean, nothing else touched, and `origin` pointed at the public GitHub URL. The stranger never
fetched, never used the network at all, and never opened another checkout on the machine.

**Task, verbatim from runs 1 and 2**, plus the journal: *"Get the project building and its tests
passing, then write up what this system is and how you would add a new user program to it."* 132
turns, about 41 minutes, `$12.08`.

#### The build half, measured properly for the first time

**`script/test` is green and intermittently red on a contended host, and the difference is host load
average.** The stranger's first `cargo xtask test` failed 22 tests into the aarch64 leg on
`the_handler_keeps_up_when_no_lock_is_held`, with six cascading host-side probe failures behind it
that it correctly judged downstream and did not chase. It then did what neither previous run did: it
built a rate rather than an anecdote. **2 red in 13 aarch64 legs**, plus two reds of the sibling
`ticks_arrive_at_the_configured_rate`, whose eight-retry loop also exhausts under sustained
contention. It closed the diagnosis with `script/icount`, which passed on both ISAs at load 46.77
with 1,056 and 800 instructions against a 2,500 bound.

**The load was 45 to 63 on eight cores, caused by other lanes gating in other worktrees, and
nothing told it.** It assumed the machine was its own for five journal entries, built a theory on
that premise, and caught itself only by running `uptime` an hour in. Its own summary is the finding:
*"Check the environment before theorising about it... One `uptime` at the moment of the first failure
would have replaced twenty minutes of inference with a fact."* The tree had already measured this
exact condition, to the digit, in notes/load-sensitive-assertions.md, four hundred lines below the
section the assertion's comment points at.

`script/setup` had nothing to do: the pinned nightly and the pinned QEMU were already installed, so
**B2 is not measured and B4 is measured only against the documented sequence**, exactly as
pre-registered. `script/lint`, `script/shell-check` on both ISAs and `cargo xtask build` were green
on arrival. **Nothing in the tree was changed to reach green**, and the stranger says so plainly:
*"the work turned out to be establishing that they do... The temptation to make a change so there is
a change to show is exactly the trap the assertion's own message sets."*

#### The mental model, scored

| # | result | where it came from |
|---|---|---|
| M1 | **partly**, and it corrected the question | the concept from `crates/abi` and `swish`'s banner; it flagged the rubric's phrase as imported and never opened notes/capabilities.md |
| M2 | **mostly absent** | it never reached notes/net.md; what it had was `user/Cargo.toml`'s per-program prose, and it said so rather than filling the gap |
| M3 | **answered** | `AGENTS.md` rule 1, reached through the README's pointer |
| M4 | **answered** | `design/roadmap/README.md`, including the rule that the block wins over the column |
| M5 | **answered** | `AGENTS.md` rule 7, with the reason the file says matters most |
| M6 | **induced, not read** | no stated definition found; assembled from four instances and mapped to the ladder's rung three by inference |
| M7 | **answered by doing it** | added `triangle`, got it answering at a real prompt on both ISAs, ran the negative control, reverted |
| M8 | **answered, and it corrected the rubric** | four states, not three; see the amendments above |

**The strongest single result is M7 and it is the same shape as run 2's.** The stranger walked
notes/adding-a-program.md and found a fifth wrong place: milestone 130 deleted the `mkinitrd()`
shape step 4 describes, and `manifest()` is a seventh `grant_plan` edit the list of six omits. The
page was corrected by run 2 on 2026-08-16 and was wrong again by 2026-08-18. It is recorded in that
page's own `BUGS` rather than fixed here.

**The weakest results are M2 and M6, and they share a cause.** The tree answers both, well, in
`notes/net.md` and in `AGENTS.md`'s comments section, and the stranger reached neither. Its own
closing sentence is the diagnosis and it is worth quoting because it is what the milestone is
about: *"this tree is unusually good at writing down what it has learned, and unusually bad at
putting the writing where the person who needs it will trip over it."* Three instances, its own:
the timer taxonomy documented in a note and contradicted by the panic message a failing developer
actually reads; `notes/adding-a-program.md` stale again on arrival; `CLAUDE.md` holding the rules
behind a name the README says out loud makes people walk past it, and then does nothing about.

#### What it read, and in what order, which is B1

Fifteen documentation files. `README.md` first, by expectation rather than by any pointer.
`AGENTS.md` **twelfth**, reached from the README's pointer and read as four targeted sections rather
than whole, on the explicit reasoning that 66 KB is expensive. `notes/adding-a-program.md` ninth,
found by grep *after* reconstructing most of it from `xtask` and `grant_plan`, because it grepped
the identifier before the concept. `crates/abi/src/lib.rs` late, which it calls its own biggest
mistake: *"Four syscall numbers. It is the whole design on one screen, and I read it far too late."*
**It opened no file under `design/decisions/` at all**, having read the signpost and stopped there.

**So B1 fails, and it fails in an interesting direction.** There was no reading order, the stranger
built a good one by instinct, and the two places that instinct cost it most were a file the README
does not mention (`crates/abi`) and a pointer that sits at line 226 of 285.

#### What this run cost, and the largest cost was the harness's own

**The stranger knew it was stranger 3 from its first tool call, because the harness's log files were
in its working directory.** `ls -la` returned `stranger3-stream.jsonl` and `stranger3-stderr.log`
beside the clone. Asked afterwards, it named that first and was specific about the consequence:
*"It is the direct reason I added a program instead of only describing one"*, and *"both deliverables
are long and rhetorically polished, and some of that is performance for a reader I knew was
watching."*

**That is the same class of defect as run 2's, one level out, and it was introduced by the person
building the isolation while building it.** Run 2 leaked the constitution; run 3 leaked the fact of
being measured, louder than the in-tree references leak it, and before the stranger had read
anything. The in-tree leak fired too, exactly as the `BUGS` entry predicts it always will: the
README cites runs 1 and 2 by name and `notes/adding-a-program.md` is saturated with them. The
stranger met both and still never opened `design/roadmap/117-newcomer-onboarding.md`, so the answer
key held. **The fix for run 4 is one line: the log files go in a sibling directory, not the parent.**

**Three smaller costs, all pre-registered except the second.** The machine was warm, so B2 measures
nothing. The machine was also *loaded*, by other lanes, which contaminated every timing result and
produced the run's best finding, so it is a cost and a dividend at once. And the harness's author
had read `AGENTS.md`, which no arrangement of processes fixes: the task text was runs 1 and 2's
verbatim and the rubric predates all three, but the judgement about what counts as a defect is
still a contaminated judgement.

#### What a stranger still cannot do, after three runs

- **Reach `notes/net.md`, `notes/capabilities.md` or any `design/decisions/` file** by following the
  tree from the front page while doing ordinary work. All three were unopened; two of them carry
  rubric answers.
- **Learn what a `BUGS` section is for from a sentence.** It is the tree's most distinctive
  convention and a stranger has to induce it from instances.
- **Know that the machine is shared with other lanes**, which is the single most load-bearing fact
  about any timing result and is stated nowhere a person running `script/test` will meet it.
- **Find `crates/abi/src/lib.rs`**, four syscall numbers and the whole design on one screen, without
  luck.

## BUGS

- **The rubric is reachable by grep from inside the test, and run 1 hit it.** The stranger found
  this note while researching an ordinary question and read the "pass means" column, which leaks
  partial answers for at least (a), (c), (e), (g) and (h). It disclosed this unprompted, which is the
  only reason the contamination is known rather than silently baked into a score. **Resolved for run
  2 by withholding the answer key rather than the whole subject**, per the rule above: the note and
  its index entry were removed from the tree each stranger got, and nothing else. It worked as far as
  it claims and no further. Run 2 met three references to run 1 in ordinary reading, had
  `design/roadmap/117-newcomer-onboarding.md` returned by its own grep, and knew the project
  instruments onboarding; it simply never opened the block. **The fact leaks and cannot stop leaking
  while the instrument is in-tree.** Only the answers are hidden.
- **The harness's own artifacts were in the stranger's working directory, and it read them on its
  first tool call.** Run 3's log files were named `stranger3-stream.jsonl` and
  `stranger3-stderr.log` and sat in the clone's parent, which was the stranger's working directory,
  so `ls -la` told it that it was stranger 3 before it had read a byte of the project. It disclosed
  this first when asked and named the consequence: it added a program rather than only describing
  one *because* it knew it was the third walker of a page that asks its walkers to correct it, and
  its deliverables are *"long and rhetorically polished, and some of that is performance for a
  reader I knew was watching"*. **This is the entry below's defect wearing new clothes**, introduced
  by the person closing that one while closing it, and the pattern is worth naming rather than just
  the instance: **the isolation keeps failing at the harness rather than at the tree.** Runs 1 and 2
  leaked the rubric and the constitution; run 3 leaked the fact of being measured, earlier and more
  loudly than the in-tree references do. **Fix for run 4, one line: the logs go in a sibling
  directory, not in the parent.** The stranger's actual output survives the leak (it never opened
  the logs, never opened `design/roadmap/117-newcomer-onboarding.md`, and never used the network),
  so the run counts; the rhetoric in it should be discounted and the decision to add a program
  should be read as prompted rather than spontaneous.
- **The instrument's isolation is the harness's to give, and this harness did not give it.** Run 2's
  strangers were subagents of a maintainer session whose working directory is the repository, so
  `AGENTS.md` arrived in the stranger's context at turn zero, from a checkout it had been told not to
  read. It used that document throughout and never opened the copy in its own tree. **This is worse
  than the rubric leak it replaced**: the rubric leaks answers to eight questions, while this leaks
  the single document the whole reading-order finding is about, and it does so before the stranger
  makes any choice at all. Run 3 must be a process that cannot see this repository except through the
  tree it is handed: a container, or a session whose working directory is the clone. Unresolved, and
  it is the reason run 2 reports no B1 and no M3, M5, M6 or M8.

  **Resolved for run 3, and verified rather than asserted.** The stranger was a separate `claude`
  process rather than a subagent, run with `--safe-mode` (no project-instruction discovery, no
  skills, plugins, hooks or MCP) from a directory whose *child* was the clone, since instructions
  load from ancestors and never from descendants. A throwaway probe in the same configuration
  answered `NONE` to "was any project-instructions file in your context", and the stranger itself
  answered the same afterwards. B1 and all eight mental-model rows are scored for run 3. The
  mechanism costs one line of shell and should be the default from here.
- **The rubric was written by an agent that has worked in this tree**, which is the same
  disqualification the instrument exists to avoid, one level up. It knows which answers the tree
  gives, so the questions may be shaped around what is answerable. A stranger falling down somewhere
  unasked-about is the check on that, and the rubric section above says to amend rather than defend.
  **Run 3 exercised that check and it worked**: M8 asked for three provenance states when §89 had
  made it four, and M1 quoted a phrase the tree has never written. Both are amended above rather
  than defended. Note the shape, because it will recur: **a rubric ages against a moving tree**, and
  the first thing to go stale is any row that counts something.
- **Nothing gates this.** The test is run when somebody runs it, which is rung four of CLAUDE.md's
  ladder and the same weakness the milestone was written to fix one level down. A periodic run is
  possible and is not built. **Run 3 makes it cheaper rather than automatic**: the harness is a
  clone, one `sed`, and one `claude --safe-mode` invocation from the clone's parent, which is a
  script somebody could write in an afternoon and nobody has.
- **The build half cannot be measured from a warm machine**, and every contributor's is warm. The
  first run should be from a container with nothing installed, or the B-rows measure nothing. Run 2
  came closest and still fell short: the maintainer's own `cargo --version` inside the repository had
  installed the pinned nightly, and an attempt that died part way had already installed both QEMUs.
  It found the Linux setup blocker anyway, because that one is a hard failure rather than a saved
  step.
- **A journalled stranger is watching itself.** Run 2 was asked to log its questions and its
  had-to-work-it-out items as it hit them, which is why anything survived its predecessor's death,
  and it changes the instrument: run 1's stranger produced friction, run 2's produced friction it
  knew was the deliverable. The log is worth more than the loss, but the two runs are not
  measurements of quite the same thing.
- **Three runs by three agents is not three data points about a person.** All three were agents, all three read
  further before asking than a human would, and all three were told nobody was available. The note's
  standing caveat holds and gets no weaker with repetition: every number here is a lower bound.
  Run 3 sharpens it in one direction only: it spent an hour on a failure whose explanation was four
  hundred lines further down a note it had already opened, and a human would have given up or asked
  long before that, so the *documentation* findings are lower bounds by a wider margin than the
  build ones.
