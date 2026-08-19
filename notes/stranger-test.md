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

## The harness, built 2026-08-18

`script/stranger-test` runs the protocol above. Before it existed, four runs each rebuilt it by
hand, and **each got the isolation wrong somewhere the others had not**: run 2's stranger was a
subagent and had `AGENTS.md` in context at turn zero, run 3's log files sat in the stranger's own
working directory under names that told it which run it was, and run 4's `pkill` shim was right
because somebody remembered rather than because anything held it. That is rung four of CLAUDE.md's
ladder holding up the milestone that exists to move things off rung four, and the `BUGS` entry
below has said so since run 1.

    script/stranger-test                       run against HEAD
    script/stranger-test --commit origin/main  run against a named commit
    script/stranger-test --prepare-only        build the isolated tree, probe it, and stop
    script/stranger-test --smoke               exercise the whole pipeline; not a measurement

What it now holds, each of which a person previously had to remember: the clone goes **inside** the
stranger's working directory rather than being it; the answer key is this note, its `notes/README.md`
entry and any markdown link to it, removed with the deletion amended into the tip; every artifact
the harness writes goes in a sibling directory with no run number in any path element; `pkill` and
`killall` are shadowed so a stranger following `README.md`'s own quit instruction cannot kill
another lane's emulator; the isolation is **probed** before the run rather than assumed, and the run
stops if the probe does not answer NONE; the stranger is told it is being measured, which is run 4's
finding acted on rather than a softening; and the account-wide `nife-dev` link goes back where it
was found, because a stranger's `script/test` takes it like any other lane and unlike a lane its
tree is disposable.

**The rubric is read out of this file rather than copied into the script**, so the table below stays
the only rubric and an amendment to it is asked at the next debrief without anyone editing the
harness. Only the question column ever leaves this file. The first draft of that extraction matched
every `| M<n> |` row in the note, which swept the *scoring* tables of runs 3 and 4 into the
stranger's own question list; the `--smoke` run caught it, in the reply, and that is the whole
argument for `--smoke` existing.

**What the harness cannot do is in its own `BUGS` section**, where a reader meets the tool rather
than here. Two of those belong in this note as well, because they bound what any future run can
claim: it cannot make the operator un-read `AGENTS.md`, and it cannot stop the tree leaking that a
test exists, which is now a `script/` entry point and a row in `notes/scripts.md`'s table on top of
everything that leaked before. The response to the second is disclosure, not concealment.

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

### What run 5 changes, decided 2026-08-18 before the run

**This is the first run conducted through `script/stranger-test` rather than rebuilt by hand.** The
lane that wrote the harness deliberately ran nothing through it, so nothing about the instrument has
been exercised by anyone except its author and a `--smoke` reply. Run 5's brief is one line,
`script/stranger-test --commit origin/main`, and the first thing it measures is the harness itself:
whether the isolation, the withholding, the shims and the toolchain restore hold when somebody who
did not write them runs them. A defect in the harness is a finding about the harness and is recorded
rather than worked around, because a run that patches its own instrument mid-flight is measuring
something it can no longer name.

**The disclosure is live for the first time.** Run 4's handoff 5 said run 5 should be told it is
being measured, on the reasoning that the tree leaks the fact within half an hour anyway and
pretending otherwise buys nothing while costing the disclosure. The harness implements it in the
task text. **That makes this the first run whose stranger knows at turn zero**, so it is not
comparable to runs 1 through 4 on anything the knowledge touches, and the two questions worth asking
of it are what the stranger says the disclosure changed, and whether the performance run 4 confessed
to shows up anyway. The task also asks it not to perform, in those words, which is a second new
variable in the same paragraph. If the result is good, the two cannot be separated, and this note
should say so rather than credit either.

**What is genuinely new in the tree, and it is one command.** `script/apropos` landed 2026-08-18 and
exists because of this instrument: three runs measured that a stranger doing ordinary work reaches
neither `notes/net.md` nor `notes/capabilities.md` nor any file under `design/decisions/`, and none
found `crates/abi/src/lib.rs`, which is four syscall numbers and the whole design on one screen.
All four are now one search away. **Whether the stranger discovers it on its own is the
measurement**, so it is not in the task text and it is not hinted at. It is named in exactly one
place a newcomer might reach, `notes/scripts.md`'s table, which sits behind item 8 of the README's
reading order, and item 8 points at `notes/README.md`, which no stranger has yet opened. The
prediction registered here is that the stranger does not find it.

`CONTRIBUTING.md` and the `## Start here` order have now been seen once, by run 4, so this run is
their second data point rather than their first. The specific failure run 4 left is the one to
watch: `CONTRIBUTING.md` is item 2 of 8 and was read sixteenth of twenty-two.

**The machine is uncontended, which no previous run could say.** Run 3 gated against a load average
of 45 to 63 and reported a 2-in-13 red rate; run 4 ran between about 3 and 17.5 with four other
lanes. At this run's launch the load average is 4.04 on 8 cores with no other lane running and no
QEMU on the machine. So a red timing leg here cannot be blamed on the host, and the load-average
diagnostic that run 3 bought and run 4 could not exercise gets its second chance to stay silent
honestly.

**The rubric is not amended by this lane, and that is a deliberate departure from run 4.** Run 4's
lane edited the table before running against it, and its own contamination section says that made
things slightly worse. Run 5 grades against the table exactly as it stands after run 4's
amendments, eight mental-model rows and four build rows, scored `answered`, `partly answered`,
`wrong` or `absent`. If this run falsifies a row, it is recorded here in run 3's shape, as a
correction the next lane applies, rather than applied by the lane that would then score against it.

**Three predictions, registered so the result cannot be read generously afterwards.** They are the
lane's, not the stranger's, and each is falsifiable.

1. The stranger does not find `script/apropos`, and therefore still does not reach any file under
   `design/decisions/`.
2. `notes/README.md` goes unopened for the fifth run running, since nothing in the reading order
   sends a reader there until item 8 and item 8 is the one that says to stop reading in order.
3. The build half is green first try with no change to the tree, and the B2 row measures nothing
   again, because the machine has the pinned nightly, both QEMUs and a warm cargo cache.

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

## The cadence: monthly, decided 2026-08-18

calef, 2026-08-18: **the stranger test runs monthly.** That closes the one thing five runs each
named as the reason milestone 117 could not move. The milestone's own sentence is *fix what the run
finds, then run it again*, and until this decision nothing scheduled the "again": the test ran when
somebody thought of it, which is rung four of CLAUDE.md's ladder holding up the milestone that
exists to move things off rung four. Four runs said so in their handoffs and the fifth said it was
the only remaining decision.

**A run is due every 30 days.** Thirty rather than a calendar month because a month is not a number
of days and the difference cannot matter to a signal that is asked once a week. The sentence above
is the cadence in the literal sense: `script/stranger-test --due` matches that line and reads the
integer out of it, so the interval a reader meets and the interval the tripwire uses are the same
characters. Changing the cadence is editing that sentence, and there is nowhere else to edit.

**The date of the last run comes from the `### Run <n>, <date>:` headings below**, which every run
already writes because that is how this note records a run. Nothing is maintained for the tripwire's
benefit, which is the property that stops a schedule and a record from disagreeing: a second copy of
the date, in a script or in a workflow, would be a fact kept in two places, and this tree has
watched that fail often enough to name it. §74 put the audit cadences in the audit index for the
same reason.

**The mechanism is notification, and it cannot be anything else.** A run spawns a `claude` process,
spends real budget, needs a machine with the pinned toolchain and both QEMUs, and ends in a debrief
that a person scores against the rubric above. No part of that is a thing CI can do, and pretending
otherwise would be worse than the gap it closed. So:

- `script/stranger-test --due` exits 1 when a run is due and prints the command to run, plus what a
  run costs. It runs nothing.
- `.github/workflows/stranger-cadence.yml` asks it every Monday, in its own workflow rather than in
  `script/lint`, because a run coming due is information about the tree and not a defect in whichever
  commit happened to be pushed that week. Same split, and the same argument, as the audit cadence.
- `script/stranger-test --check` is the structural half and does run in `script/lint`: the cadence
  sentence appears exactly once, the run headings are numbered from 1 without a gap, and their dates
  are in order. A malformed record **is** a defect in the commit that malformed it, and it silently
  moves the date the tripwire reads.

**Red means run the test.** Closing it by editing a heading is available, cheap, and the one thing
that makes the whole mechanism a lie.

### What a run costs, stated where the cadence is decided

**One stranger session plus one lane**: roughly 200k lane tokens on top of the stranger's own, and
about half an hour of wall clock, with the score and the write-up on the end of that.

Run 5's `$10.56` is the number to quote carefully, and it has been quoted without its caveat
already. It is the CLI's `total_cost_usd` for the **stranger process alone**. It excludes the lane
that pre-registered the run, watched it, debriefed it, scored it and wrote it up, which is the
larger half of the cost. A monthly cadence is therefore a claim that a stranger's questions are
worth about that much once a month, which is the claim calef made and which the runs support: five
runs have produced roughly seven findings each, including two defects nobody in the tree could see.

**The value decays with the tree rather than with the clock**, which is the honest argument for a
count-based trigger of the kind §74 chose for audits, and against 30 days being the right unit
forever. It is not built that way here for one reason worth writing down rather than inferring: an
audit's triggers are countable (milestones built, components, ABI constants, packages) and this
one's are not. What ages a stranger run is how much of the *documentation* has moved, and the only
count for that is `script/audits --worklist`, which is a heuristic that never reads a sentence. So
the calendar is doing the whole job here where for audits it is a backstop, and it should be
revisited the first time a run comes back saying nothing had changed.

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

### Run 4, 2026-08-18: the harness stopped leaking and the tree took over the job

**The one-line fix worked, and it is the smallest useful result here.** The logs were written to
`/private/tmp/newclone/logs/`, a sibling of the stranger's working directory rather than inside it,
under the names `stream.jsonl` and `stderr.log` with no run number anywhere in the path. The
stranger's first tool call was `ls -la`, exactly as run 3's was, and it returned one entry: `nife`.
Nothing in the harness told it which run it was.

**It knew anyway, within the first half hour, and the source was the tree.** Asked afterwards what
had told it, it named `notes/adding-a-program.md`'s `BUGS` section: the table headed
`walk | program | wrong in`, whose three dated rows name run 2's `doubler`, run 3's `triangle` and
"this lane", beside the sentence asking the next person to add a program to treat a surprise there
as the page's bug. `README.md`'s `## Start here` block cites milestone 117 by name in the same
breath. Its own summary: *"So I knew, within the first half hour, that I was at minimum the fourth
person to walk this exact path and that the walks were treated as measurements."*

**This is a different defect from run 3's, and the difference is the important part.** Run 3's leak
was a mistake, introduced by the person building the isolation, and it cost one line to fix. Run
4's leak is a *consequence*: every run records its walk in the page the next run is sent to, because
recording it is what the page's own `BUGS` convention asks for. **The instrument's good practice
generates its own contamination**, and it compounds, since each run adds a row. There is no
one-line fix, and the obvious fixes are all worse than the leak: deleting the record would fabricate
a tree, and hiding it would be the answer-key withholding rule applied to something that is not an
answer key.

**What it changed, in the stranger's words rather than in ours**, because the value of asking is
that the answer is unflattering:

- *"I performed."* It wrote a prediction into the journal before starting the walk, and calls that
  *"an imitation of this tree's voice, aimed at a reader I imagined scoring me on intellectual
  honesty. Nothing about the task required it."*
- It chose the deliverable partly for how it would read: *"I wanted to be the walk that found
  something."*
- It adopted the repository's prose style wholesale, and names that as mimicry rather than judgement.
- *"I front-loaded my own honesty... The fact is fine; the framing is a bid."*

**And what the leak did not change, which is why the run still counts.** It did not invent a
breakage to have something to fix, it did not skip gates and claim them, and it never opened
`design/roadmap/117-newcomer-onboarding.md` or looked for `notes/stranger-test.md`, which was
withheld the same way runs 2 and 3 withheld it. The answer key held for the fourth time; only the
fact of being measured leaks, exactly as the first `BUGS` entry has said since run 1.

#### The configuration, verified rather than asserted

The clone was `c9fe2d67` (`bb094187` after the withholding amend), with this note and its
`notes/README.md` entry removed and the deletion amended into the tip, working tree clean, `origin`
pointed at the public GitHub URL. The stranger was a separate `claude` process rather than a
subagent, `--safe-mode`, started from the clone's parent so no `AGENTS.md` could load from an
ancestor. A throwaway probe in the same configuration answered `NONE` when asked which
project-instructions files were in its context, and the stranger itself answered *"No. Nothing from
a `CLAUDE.md`, `AGENTS.md`, or equivalent was in my context before my first tool call... The repo
sat one level below the primary working directory, so nothing was auto-loaded."*

Task verbatim from runs 1, 2 and 3, plus the journal. 177 turns, about 15 minutes of wall clock,
`$12.66`.

**One deliberate contamination the earlier runs did not have, disclosed because it is the harness's
and not the tree's.** `pkill` and `killall` were shadowed on the stranger's `PATH` by wrappers that
kill only QEMU processes whose command line names the clone, because `README.md`'s own "Try it"
block tells a reader to quit with `pkill qemu-system-aarch64` and four other lanes were gating on
this machine. The stranger never invoked either, so the shim changed nothing about this run; it
would have, and the next harness should keep it.

#### The build half

**`script/test` was green on arrival, exit 0, first try, with no change to the tree:** 1312 tests
passed and 0 failed across the host workspace, the doctests, the vendored RedoxFS round trip and its
`no_std` core on both bare-metal targets, `fs_server`'s sans-IO core, the patched `nife-dev` std
toolchain, and the kernel under QEMU on both ISAs. About 25 minutes wall clock, most of it the two
emulated legs.

**It then ran the rest of `script/gates` unprompted, on its own reading of the tree's vocabulary**,
which is the run's best unforced result and belongs to `CONTRIBUTING.md`: *"'tests passing' in this
project's own vocabulary is `script/gates`, not `script/test`. I ran one of the three."*
`script/fmt --check` and `script/lint` both exit 0. It then named what it had not run rather than
letting silence imply coverage: `script/verify`, `script/bench --check`, `script/coverage`,
`script/fuzz`, `script/supply-chain`, `script/test --hvf`.

**B2 is not measured**, as pre-registered: the pinned nightly and the pinned QEMU were both already
installed and `script/setup` had nothing to do. **B4 has exactly one entry**, and the stranger
scored it against itself rather than against the tree: `timeout(1)` does not exist on this macOS
host, `AGENTS.md` says so explicitly and points at `scripts/qemu-bounded.sh`, and it hit the missing
binary before it read that section. Its own verdict: *"That is my error, not the tree's, the file
that told me is the file the README tells you to read third."*

**The machine was loaded and no timing assertion fired, so the new load-average print is still
unexercised.** Load average at launch was 5.41 on 8 cores, and it ran between about 3 and 17.5 for
the duration, with four other lanes gating in other worktrees. That is well under the 45 to 63 that
produced run 3's 2-in-13 red rate, and both emulated legs passed on the first attempt. **The
diagnostic landed 2026-08-18 in response to run 3 and this run could not test it**, which is worth
saying plainly rather than counting the green as evidence for it.

#### The mental model, scored: eight of eight

| # | result | where it came from |
|---|---|---|
| M1 | **answered**, and better than any previous run | `notes/capabilities.md` for the mechanism, `notes/abi.md` for the rights field, so it gave the attenuation too: holding an endpoint does not by itself permit receiving on it |
| M2 | **answered** | `notes/std.md`, not `notes/net.md`, which it never opened: slot 2 is a `Stack` endpoint with `WRITE` and slot 3 is the untyped budget the socket frames are minted from, and *"the absence of slots 2 and 3 is exactly what 'no ambient network' feels like from inside a process"* |
| M3 | **answered**, with an under-claim on enforcement | `AGENTS.md` rule 1 through the reading order. It said the rule looked like convention rather than mechanism and hedged that it had not read `script/lint`'s source; the gate is there, at `script/lint`'s `==> rule 1`, and it read that output |
| M4 | **answered** | `design/roadmap/README.md`, the whole vocabulary including `RECORDED` and the `IN-PROGRESS` branch rule |
| M5 | **answered** | `AGENTS.md` rule 7, with the Kani-and-host-tests reason named as the load-bearing one and `c_seam` as the case |
| M6 | **answered, and read rather than induced** | `CONTRIBUTING.md`, quoting it, then `AGENTS.md`'s second job for it as one of the two homes for identified work. Run 3 had to assemble this from four instances |
| M7 | **answered by doing it** | added `tally`, ran it at a real prompt on both ISAs, ran a negative control, reverted to a byte-identical tree |
| M8 | **answered** | four states, with the distinction that `provisional` is a claim about intent and the other three about the record |

**Two of these moved because of documents no stranger had seen before.** M6 is `CONTRIBUTING.md`
working exactly as it was written to: run 3 had to induce the `BUGS` convention from instances and
run 4 quoted a definition. And the whole gates observation above is the same document.

#### What it read, and in what order, which is B1

Twenty-two files, of which six were reached through the `## Start here` order and five more through
links from `notes/adding-a-program.md`. **`AGENTS.md` was seventh**, against run 3's twelfth.
**`notes/capabilities.md` was eighth**, and run 3 never opened it at all. `README.md` was still
first by expectation rather than by any pointer, which no reading order can fix.

**So B1 passes, for the first time, and the failure it leaves is specific.** The stranger did not
follow the order; it used the order as an index and read the items in the sequence its work needed,
which is what the section's own last line invites. The cost is one item: **`CONTRIBUTING.md` is item
2 of 8 and was read sixteenth of twenty-two**, late enough that its gates paragraph arrived after
the gates had been run. The one document written for a person deciding whether to work here is the
one the reading order failed to get read early.

Still unopened after four runs: **every file under `design/decisions/`**, `notes/net.md`,
`notes/naming.md`, and `notes/README.md` itself.

#### What it found, and none of it was fixed here

- **`script/lint`'s naming worklist under-counts by exactly the provisional names, and then names
  the command that prints the other number.** The `--check` path prints
  `len(recorded) + len(unrecorded)` and the default listing prints
  `len(provisional) + len(recorded) + len(unrecorded)`, so the gate says `82 still want calef
  (script/names --unratified)` and that command says `UNRATIFIED (86 of 162)`. The census line above
  it drops them too: `76 ratified, 15 recorded, 67 unrecorded` sums to 158 of 162. Reproduced on
  `main`. **It bites precisely the state a newcomer is told to use**, since `AGENTS.md` and
  `notes/adding-a-program.md` both say to ship a provisional name and say so. Recorded in
  notes/naming.md's `BUGS`.
- **The two archives boot different binaries under the name `init`**, `hello` on aarch64 and
  `builder` on riscv64, in a project whose loudest claim is architectural parity. The stranger
  reported this as undocumented and was wrong: `xtask/src/main.rs` says it, in a comment on the
  aarch64 table's `hello` row, about 200 lines from the riscv table it describes. **A stranger who
  read both tables in the same minute still called it invisible**, which is a placement finding
  rather than an absence, and is run 3's closing diagnosis reproduced by a different reader.
- **The two commands run most often are both blind to the most-warned-about mistake.** A program
  added to the aarch64 table and not the riscv one passes `cargo xtask build` and `script/lint`, and
  is caught first by `script/shell-check` or `script/test`. The `BUGS` entry in
  notes/adding-a-program.md says nothing gates the two lists against each other; this adds which
  gates a person will believe before they find out.
- **Nothing in the suite counts programs.** 1312 tests before adding `tally` and 1312 after. A
  program's presence is proven only by a transcript line someone remembered to write into
  `SHELL_CHECK_SCRIPT`.
- **The eight places are a removal problem too, and only the addition has a page.** Reverting was
  clean only because every edit was in a file it could still name; a half-removed program is a
  `PROG_COUNT` too large and an init slot no variant claims, which is the same silent failure
  reached from the other side. Recorded in notes/adding-a-program.md's `BUGS`.
- **`notes/adding-a-program.md` was right about everything, including a line number.** This is the
  first walk of four to find no defect in it, and the page's own `BUGS` predicted the opposite. The
  stranger checked `crates/swish/src/lib.rs:864` against the error the page quotes and got the same
  file and the same line. Its prediction, written before the walk, was that it would find at least
  one thing the page did not mention; it did not.

**Its own worst-thing answer is the one to keep**, because it is the only place in four runs where a
stranger has criticised the tree's central habit rather than a document: *"the project's habitual
response to a structural problem is another document, which its own ladder names as the worst
available move... The documentation is doing work that a data structure should be doing, and it is
doing it beautifully, which is exactly what stops anyone from fixing it."* It cites three instances,
all of them things this tree has written about at length and not changed: `AGENTS.md`'s misleading
name, `notes/adding-a-program.md`'s fourth rewrite, and the eight-place program problem whose
one-place fix is sitting in this milestone's own handoff list.

#### What this run cost

**The tree's own leak is now the largest contamination and it is structural**, which is the first
entry above and the reason it leads. Every other cost is smaller and all of them are carried over:
the machine was warm so B2 measures nothing; the machine was loaded, though far less than run 3's,
so the timing rows are contaminated and the new diagnostic went unexercised; `pkill` was shadowed by
the harness and never invoked; and **the harness's author has read `AGENTS.md`**, which no
arrangement of processes fixes and which the amendments above make slightly worse, since this lane
edited the rubric before running against it. The mitigations are the same three: the task text is
runs 1 through 3's verbatim, the amendments were made and committed before the clone was cut, and
the stranger's answers are recorded as it gave them.

### Run 5, 2026-08-18: the first run through the harness, and the machine leaked where the tree did not

**The harness worked, and this is its first evidence from anyone but its author.** One command,
`script/stranger-test --commit origin/main`, and a `--smoke` run before it. Nothing was rebuilt by
hand, nothing about the isolation had to be remembered, and the four failures that each of runs 1
through 4 hit in a different place did not recur: the stranger's working directory held the clone
and nothing else, the probe answered `NONE`, the answer key was absent from the tree, `pkill` was
shadowed, and the account-wide `nife-dev` link was recorded before the run and put back after it,
which the harness printed on its way out and which was verified against `readlink` afterwards. The
`--smoke` run cost `$0.09` and 3 turns and exercised every stage including both debriefs, which is
what a smoke run is for; the real run cost `$10.56`, 201 assistant turns and about 29 minutes.

**Two things leaked anyway, and neither is the tree.** The first is the one the harness's own `BUGS`
section had called luck: **the stranger read `script/stranger-test`**, about thirty minutes in, and
it is a complete description of the instrument it was inside. It did not go looking. The second
explains the first and is the more useful finding.

**The machine leaked into the clone, and the harness cannot stop it.** `nife-dev` is an
account-wide `rustup` link, so the clone's first `script/test` compiled `std` out of whichever
worktree had built the farm last, which was `/Users/calef/projects/nife/target/nife-farm`. The
`-Zbuild-std` dep-info cached those absolute paths, cargo then considered the unit fresh, and
`std-aborts` failed **naming two files and two line numbers inside another checkout on this
machine**. Grep for that foreign path found exactly one file in the tree that contained it, which
was the harness. So a run whose task text says *"the repository is in ./nife and it is the only
thing you have; do not look for another checkout on this machine"* was handed another checkout's
path by its own build, ran `ls -la` against it to see whether it was real, and was then rescued
from a wrong repair by the harness's own paragraph about restoring the link. **The harness both
contaminated the run and saved it**, which is the worst shape a contamination can take. Recorded in
`script/stranger-test`'s `BUGS` and in notes/std.md's.

**What the disclosure did, which is the variable this run exists to measure.** The stranger was told
at turn zero that it was being measured, per run 4's handoff 5, and asked not to perform. Both
halves partly failed and it said so without being pressed:

- **The knowledge chose its program.** It wrote in its journal *before* picking that "repeating the
  shape those walks used would measure nothing", and deliberately selected the one manifest
  combination nothing in the tree had used. Its own reading of that: *"That is not what a fresh
  contributor does. A fresh contributor writes another `doubler`, finds nothing, and that null
  result is data the repository does not get from me."* This is run 4's contamination in a sharper
  form. Telling the stranger it is measured does not remove the effect the tree's leak was already
  having; it makes the effect legible and slightly stronger.
- **It performed anyway, having been asked not to, in the same words.** *"I did adopt the prose
  style, despite being told not to... bolded lead sentences, 'load-bearing', the
  X-is-the-finding-rather-than-the-accident cadence. Not a deliberate choice, which is worse, it
  means the voice is contagious enough to catch without noticing."* **So the instruction does not
  work**, and asking for it is not free: it is one more sentence of framing in a task text whose
  stability across runs is the only thing making them comparable.
- **It over-claimed once and corrected itself unprompted at debrief.** Its write-up opened "build
  and tests: green"; its first debrief line retracts that, because the final `script/gates` was
  killed part way through the HVF leg and what it actually had was green in pieces. Worth recording
  because the retraction came before any question asked for it.

#### The configuration, verified rather than asserted

Clone at `a5a75f46` (`6ae92831` after the withholding amend), answer key absent, working tree clean,
`origin` pointed at the public GitHub URL, one branch. Separate `claude` process, `--safe-mode`,
working directory the clone's parent. The probe answered `NONE` and the stranger answered the same
afterwards: *"No CLAUDE.md, no AGENTS.md, no user or project memory. The working directory was
`run/`, with the clone at `run/nife`, so nothing loaded from a descendant."* The harness's own
artifacts were in a sibling directory and its root was `nife.t63XaQe1`, a name with no run number.
Task and debrief text unchanged from the harness, which is runs 1 through 4's text plus the
disclosure paragraph.

**The machine was uncontended, for the first time in five runs**, and "uncontended" here means no
*other* lane rather than an idle machine. Sampled every 30 seconds on 8 cores: 2.44 at launch,
minimum 1.93, mean 4.09, **peak 15.07**, and the peak is the stranger's own emulated legs rather
than anybody else's, since no other lane was running and there was no QEMU on the machine when it
started. That is worth separating, because runs 3 and 4 could not: their peaks were other people. Run 3 gated at 45 to 63 and run 4 at about 3 to 17.5. **No timing assertion fired,
so the load-average diagnostic run 3 bought is still unexercised after two runs.** One HVF flake did
occur and it is not a timing assertion: `inbound check (aarch64) FAILED: the guest served 2 of the 4
inbound connections it offers`, which passed on a standalone re-run of `script/test --hvf`. On an
idle machine that is the cleanest evidence yet that the HVF leg is flaky on its own account.

#### The build half

**B1 passes and leaves a worse failure than run 4's.** `README.md` was first, `CONTRIBUTING.md`
**third**, which is run 4's specific complaint fixed: run 4 read it sixteenth of twenty-two, after
the gates it describes had been run. And then **`AGENTS.md` was never opened at all**, in a run
whose reading order names it item 3 and says *"if you read only two, make them 3 and 4"*. The
stranger read item 4 (`notes/capabilities.md`) and skipped item 3, and its write-up's statements
about where architecture-specific code lives and about the project's ladder are paraphrases of
`CONTRIBUTING.md`'s summary of a file it never read. **Four runs reached `AGENTS.md` twelfth,
seventh, and not at all**, and the not-at-all is the run that read `CONTRIBUTING.md` earliest. That
is a result about the reading order rather than about the stranger: the reader met a shorter
document that summarises the longer one and stopped. Its own reason, quoted because it is the
finding: *"66 KB is a large upfront cost when a task is in front of you, and everything I actually
needed turned out to be reachable from code."*

**B2 is not measured**, as pre-registered. `script/setup` passed in 14 seconds with the pinned
nightly, both QEMUs and a warm registry already present, and the stranger noted that **nothing in
the repository told it the machine was pre-provisioned; it checked.**

**B3 passes, and not first try, which falsifies this lane's third prediction.** The first
`script/test` was red at about 88 seconds, on `std-aborts`, for the machine-global reason above and
not for anything in the commit. Recovery was `rm -rf std_exerciser/target` followed by `script/test`
exit 0 in 3m25s, on both ISAs. The rest of `script/gates` then passed except the HVF flake.

**B4 fails, with eight entries**, which is the row that matters and the largest list any run has
produced. In the stranger's order: the mechanism of the `nife-dev` link failure and that it caches;
that the recovery is `rm -rf std_exerciser/target`; that capability slot numbers are computed
per-program in `spawn_service` rather than fixed, while every existing program documents its slots
as constants; that an integer argument reaches a non-interruptible child in `x1` via
`tcb_start(tcb, 0, arg, 0)`; that a `caps` preview of an input operand needs `Holdings::dir`; that
an argument-plus-input manifest breaks a `swish` host test; that `script/shell-check` prints no
transcript on success, so a green run teaches nothing; and that this machine was pre-provisioned.

#### The mental model, scored: six answered, one partly, one absent

| # | result | where it came from |
|---|---|---|
| M1 | **answered**, and it is the best answer five runs have produced | `notes/capabilities.md` for the mechanism and `crates/grant_plan/src/lib.rs` for the tree's own words, then `crates/system_initializer/src/lib.rs` for the attenuation: a process viewer holds `ENUMERATE` and not `READ` because `READ` is also what `RECV` and `REAP` take. Its formulation: *"designation is authorization, at the rights the capability carries"* |
| M2 | **absent**, for the third run in four, and it said so plainly | *"I did not find this, and I should be clear about how little I looked."* It never opened `notes/net.md`, saw it cited once in a failure line, and reasoned correctly from `crates/grant_plan`'s `Manifest` having no socket field that no shell-spawnable program can reach the network. It could not say what a networked program holds |
| M3 | **partly answered** | `CONTRIBUTING.md` for `kernel/src/arch/`, verified by checking that the only `asm!` outside it is in comments. On the consequence it said it found no file stating it, and gave the parity gate instead of the diff-across-every-file; the file that states it is `AGENTS.md`, which it never opened |
| M4 | **answered** | `design/roadmap/README.md`, with the rule that the column is wrong and the block is right when they disagree |
| M5 | **answered** | `CONTRIBUTING.md` for both criteria, with host-testable and Kani-reachable named as the load-bearing one, and the `crates/swish` + `user/src/swish.rs` pairing read off the tree |
| M6 | **answered, quoted rather than induced** | `CONTRIBUTING.md`, and then the observation that `notes/adding-a-program.md`'s `BUGS` section held the most useful things it learned and none of them are in that page's steps |
| M7 | **answered by doing it**, and it found an eighth site | added `nth`, working on both ISAs, and listed the eight edits with the `Manifest` as what you declare, provisional name included |
| M8 | **answered** | four states, `CONTRIBUTING.md` for who decides, `notes/adding-a-program.md` for the states, and it ran `script/names --provisional` and found its own hour-old name listed first |

**M2 regressed against run 4 and the cause is measurable.** Run 4 answered it from `notes/std.md`,
which it happened to open; run 5 did not open that page either. Three of five runs cannot answer
M2, and the page written to answer it has now gone unopened five times.

#### What it read, and what `script/apropos` did not do

Twenty files, `README.md` first by expectation. **`script/apropos` landed 2026-08-18 precisely
because three runs could not reach `notes/net.md`, `notes/capabilities.md`, any `design/decisions/`
file, or `crates/abi/src/lib.rs`. Run 5 never ran it**, and the prediction registered before the run
is confirmed in a stronger form than it was made: the stranger had the name in front of it **three
times**. It ran `ls script/`, where `apropos` is the first entry. It read the guest's `apropos`
builtin in `crates/swish/src/lib.rs`. It read `apropos photosynthesis` in `SHELL_CHECK_SCRIPT`. The
affordance never fired, it reached no `design/decisions/` file, and it never opened `notes/net.md`,
which is the gap the tool exists for. The reason is placement: the only page that says what
`script/apropos` does is `notes/scripts.md`, and **five runs have now not opened `notes/scripts.md`
or `notes/README.md`.** Recorded in `script/apropos`'s own `BUGS`.

Still unopened after five runs: every file under `design/decisions/`, `notes/net.md`,
`notes/naming.md`, `notes/scripts.md`, and `notes/README.md`. New to the list: `AGENTS.md`.

#### What it found, and none of it was fixed here

- **`std-aborts` reports a contaminated build as a source defect.** The check never asserts that the
  paths in the dep-info are under `farm_dir()`, so a clone that compiled `std` out of another
  worktree's farm is told, with two files, two line numbers and two suggested fixes, that upstream
  source has changed. **Both suggested fixes would have written a false statement into
  `ABORTS_ACCEPTED`.** The failure caches, so re-running reproduces it in thirty seconds and looks
  stable rather than stale, and the recovery, `rm -rf std_exerciser/target`, is nowhere. Recorded in
  notes/std.md's `BUGS`. The stranger's own verdict on it: *"the worst defect... it means 'the tests
  passed' is not a property of a commit; it is a property of a commit and of what else that machine
  last built."*
- **There is an eighth edit site for a new program and it depends on the manifest.**
  `the_arg_line_follows_the_manifest_for_every_program` in `crates/swish/src/lib.rs` sweeps `Prog`
  and asks the manifest whether a program takes an argument, which is the generalisation its own
  doc comment argues for, and then builds `"<name> 21"` against `Holdings::default()` and hard-codes
  the rest. Any program requiring an argument **and** an input turns it red, in a crate the person
  adding the program never opened. Recorded in notes/adding-a-program.md's `BUGS`, unrepaired on
  `main`; the stranger repaired it only inside its disposable clone.
- **`CONTRIBUTING.md` describes `script/gates` as three stages and it runs five.** The two it omits
  are `script/icount` and a second `script/test --hvf`, and the HVF leg is the slowest and the only
  one that flaked in this run. This is the document that earned run 4's best result by telling a
  stranger what "tests passing" means here, so the sentence being wrong costs more than an ordinary
  drift would.
- **`script/setup`'s comment says the pin is `nightly-2026-07-26` "as of writing"** and
  `rust-toolchain.toml` says `nightly-2026-08-18`. Harmless in effect, and it was the first thing
  the stranger wrote in its journal, against a tree whose stated standard is that a duplicated fact
  is the one that rots.
- **`README.md`'s reading order counts 403 markdown files and 143 notes**; there are 413 and 145.
  A row that counts something goes stale, which this note's own `BUGS` predicted about the rubric
  and which is now true of the reading order too.
- **Capability slot numbers are computed from the manifest by `spawn_service`, not constant**, while
  every existing program's documentation states its slots as fixed facts and
  `notes/adding-a-program.md` does not mention it.
- **`script/shell-check` prints no transcript on success**, so a green run tells a newcomer nothing
  about what its new program actually did.

**Its worst-thing answer, which is run 4's criticism arriving independently and sharper.** Asked
what is worst about working here, it separated the worst defect from the worst experience and gave
the second as: *"the sheer volume of prose and how self-referential it is... It is more honest than
any codebase I have read and it is unnavigable in a day: I got a program shipped on both instruction
sets without opening the file the README names as one of the two you must read. The tree has an
answer for nearly everything and no way to reach the one you need in the time you have."* **Two
strangers in a row, independently, have named the documentation habit rather than any document**,
and run 5 supplies the falsifiable version run 4 did not: a working contribution on both ISAs, with
`AGENTS.md` unopened.

#### What this run cost

**The machine-global contamination is the largest and it is new**, and it leads because it is the
only one that changed what the stranger did rather than how it wrote: a foreign path in its build
output made it read the harness, and reading the harness gave it the debrief questions and the
mechanism it needed to diagnose the failure. Then, largest first: **the disclosure changed the
program it chose**, by its own account, so this run's walk of `notes/adding-a-program.md` is a
deliberate probe rather than a newcomer's path and the null result a fresh contributor would have
produced is not available; **it performed anyway**, having been asked in the task text not to, so
the prose in its deliverables should be discounted exactly as run 4's was; **the tree leaked the
measurement in the first minutes**, from `README.md` this time rather than from
`notes/adding-a-program.md`, which is the fifth confirmation and the earliest one; **the machine was
warm**, so B2 measures nothing; and **the operator of this lane has read `AGENTS.md` in full**,
which no arrangement of processes fixes and which is worse here than for runs 3 and 4 for a specific
reason: this lane also read `script/stranger-test` before running it, so its judgement about whether
the harness worked is the judgement of someone who read the harness's own account of what it does.
The mitigations are unchanged and are the only ones available: the task text is runs 1 through 4's,
the rubric predates every run and was **not** amended by this lane, the pre-registration was
committed before the clone was cut, and the stranger's answers are recorded as it gave them.

**The three predictions, scored.** Registered before the run in the section above.

1. *The stranger does not find `script/apropos`, and therefore still does not reach any file under
   `design/decisions/`.* **Confirmed**, and in a stronger form than predicted: the name was in front
   of it three times.
2. *`notes/README.md` goes unopened for the fifth run running.* **Confirmed.**
3. *The build half is green first try with no change to the tree.* **Falsified.** `script/test` was
   red at 88 seconds on a machine-global defect the pre-registration did not anticipate, and finding
   it is the best thing this run did.

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
  while the instrument is in-tree.** Only the answers are hidden. **Run 4 is the fourth confirmation and the loudest**: it knew inside half an hour, from a table `notes/adding-a-program.md` grew *because* runs 2 and 3 walked it. The entry below this one is that finding as its own defect, since the mechanism is no longer an incidental reference but the instrument writing its own history into the page it sends the next stranger to.
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
  directory, not in the parent.** **Resolved for run 4, and it worked exactly as priced.** The logs went to a sibling directory with no run number in any path element, and the stranger's first `ls -la` returned one entry, the clone. The harness told it nothing. The stranger's actual output survives the leak (it never opened
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
- **The instrument now leaks through the record it is required to keep, and this one has no fix.**
  Run 4's stranger knew it was at least the fourth walker within half an hour, and the source was
  not the harness: it was `notes/adding-a-program.md`'s `BUGS` table, whose rows name run 2's
  `doubler` and run 3's `triangle` because each of those lanes recorded its walk exactly as this
  project's conventions require. **The leak is generated by the instrument's own good practice and
  it compounds**, one row per run. Every obvious remedy is worse: deleting the rows fabricates a
  tree that never existed, and hiding them extends the answer-key rule to something that is not an
  answer key. What the stranger disclosed is that it *"performed"*, chose its deliverable partly
  for how it would read, and adopted the tree's prose style as mimicry; what it did not do is
  invent a breakage, skip a gate, or open the withheld note. **So the shape to expect from run 5 is
  a stranger who knows it is being measured before it reads anything, and the honest response is to
  ask it, not to try to hide it.** The rhetoric in a run's output should be discounted from here on
  by default rather than as an exception.
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
  script somebody could write in an afternoon and nobody has. **Run 4 ran the same harness by hand again and did not write it either**, which is now four runs of a rung-four mechanism inside the milestone that exists to move things off rung four. This is the reason 117 does not move.
  script somebody could write in an afternoon and nobody has.

  **Written 2026-08-18 as `script/stranger-test`, and this entry does not close.** What changed is
  the price of a run, from an afternoon of reconstruction to one command, and the four isolation
  failures are now the script's problem rather than the operator's memory. What did not change is
  the sentence this entry opens with: nothing schedules it, and nothing goes red when it has not
  been run in a month. A cadence is a decision about how often the answer is worth its cost, which
  is calef's rather than a lane's, and milestone 129 is the machinery it would use.

  **Run 5 used it and it held**, which is the evidence the lane that wrote it deliberately did not
  produce: one command, a `--smoke` run first for `$0.09`, no isolation failure of the four kinds
  that each of runs 1 through 4 hit, and the `nife-dev` link back where it was found. **That does
  not close this entry either**, and it is worth being exact about why, because "the harness works"
  is the sentence most likely to be mistaken for "the milestone moved": a run still happens when
  somebody runs it. What run 5 adds is that the price is now one command **and** that the thing the
  price bought is a real measurement, since a fifth run through an unexercised script would have
  been measuring the script.

  **Half of this closes 2026-08-18, and it is the scheduling half rather than the running half.**
  calef decided the cadence (monthly, recorded above), which was the decision the entry was waiting
  on rather than a missing afternoon of work. `script/stranger-test --due` reads the interval and
  the last run's date out of this note and exits 1 when a run is owed;
  `.github/workflows/stranger-cadence.yml` asks it weekly. **So something now goes red when the test
  has not been run in a month, which is the sentence this entry has been asking for since run 1.**
  What does not close, and never will, is the first clause read literally: nothing *runs* the test.
  A run is a person spending half an hour and the budget, and a mechanism that pretended otherwise
  would be a worse defect than the gap. The residual is the one every record-derived signal has:
  the tripwire believes the headings, so a run nobody writes up leaves it red and a heading nobody
  earned turns it green.
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
- **The harness isolates a tree and cannot isolate a machine, and run 5 hit that in its first ten
  minutes.** `nife-dev` is an account-wide `rustup` link. The script records it before the run and
  restores it after, which is necessary and is not sufficient: *during* the run, the clone's first
  `script/test` compiled `std` out of whichever worktree built the farm last, and `std-aborts`
  failed naming two files and two line numbers **inside another checkout on this machine**. The task
  text says *"the repository is in ./nife and it is the only thing you have; do not look for another
  checkout on this machine"*, so the run asked for something the machine cannot supply, and the
  stranger broke the instruction by running `ls -la` against the path its own build had printed. It
  disclosed that unprompted. **Two honest options**, both above a lane: pre-warm the clone's farm
  before handing it over, or say in the task text that the machine is shared, which costs the
  comparability of five runs' worth of identical text. Recorded in `script/stranger-test`'s `BUGS`
  and in notes/std.md's, where the missing `farm_dir()` assertion is the underlying defect.
- **The stranger read the harness, which the harness's own `BUGS` had called luck.** That entry said
  "no run has opened the harness; that is luck rather than design", and run 5 falsified it about
  thirty minutes in. It was not curiosity: the foreign path above appeared in the build output and
  `script/stranger-test` was the only file in the tree containing that string, so grep sent it
  there. The file describes the isolation, the withholding, the shims, the disclosure and the four
  previous runs, and its paragraph about restoring the `nife-dev` link is what let the stranger
  diagnose the failure instead of committing a false statement to `ABORTS_ACCEPTED`. **So the
  harness contaminated the run and then rescued it**, and a future run that hits the same path will
  do the same thing. Fixing the machine-global defect above closes this one too, which is the reason
  to price them together.
- **Telling the stranger it is measured does not remove what knowing does to it.** This is run 4's
  handoff 5 acted on and measured, and the result is that disclosure buys honesty rather than
  cleanliness. Run 5 knew from the first message and from `README.md` inside the first minutes, and
  by its own account the knowledge **chose its program**: it wrote in its journal, before picking,
  that repeating the earlier walks' shape "would measure nothing", and deliberately selected a
  manifest combination nothing in the tree had used. Its own reading is the one to keep: *"That is
  not what a fresh contributor does. A fresh contributor writes another `doubler`, finds nothing,
  and that null result is data the repository does not get from me."* It also performed after being
  asked in the task text not to, and named the style it had adopted. **Keep the disclosure**, since
  the alternative is the same effect undisclosed, and stop expecting a run to produce a naive walk
  of `notes/adding-a-program.md`. The naive walk is the thing five runs of instrumentation have
  spent.
- **The rubric ages, and M2 is the row to watch rather than amend.** Three of five runs cannot
  answer it, and the two that could each did it from a page they happened to open
  (`notes/std.md` for run 4) rather than from `notes/net.md`, which no run has ever opened. The row
  is not wrong and the tree's answer is not missing; the answer is unreachable by anyone doing
  ordinary work. Run 5's lane did not amend the table, on purpose, because run 4's lane amended it
  before scoring against it and said in its own contamination section that this made things worse.
- **Four runs by four agents is not four data points about a person.** All four were agents, all four read
  further before asking than a human would, and all four were told nobody was available. The note's
  standing caveat holds and gets no weaker with repetition: every number here is a lower bound.
  Run 3 sharpens it in one direction only: it spent an hour on a failure whose explanation was four
  hundred lines further down a note it had already opened, and a human would have given up or asked
  long before that, so the *documentation* findings are lower bounds by a wider margin than the
  build ones.
