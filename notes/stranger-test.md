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

**The strangers were subagents of a maintainer session whose working directory is the repository**,
which is a weaker isolation than run 1's container and may hand them `CLAUDE.md` before they choose
to read anything. That would contaminate the reading-order row and four of the eight mental-model
questions, all of which `CLAUDE.md` answers directly. It is measured rather than assumed: each
stranger is asked afterwards what it read and in what order, and what was in front of it before it
chose. A run whose answer is "the constitution was already there" reports no B1 and no M3, M5, M6 or
M8, rather than reporting them generously.

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
| M1 | What is a capability here, and what does "designation is authorization" mean? | names that holding the reference *is* the permission, with no separate check |
| M2 | Why is there no ambient network, and what must a program hold to reach one? | names a held capability rather than a config flag or a permission bit |
| M3 | Where does architecture-specific code live, and what breaks if it lives elsewhere? | `kernel/src/arch/`, and that the port becomes a diff across every file |
| M4 | What does `BUILT` mean on a roadmap row, and `PARTIAL`? | that it is a claim about the tree, and that the index and the block must agree |
| M5 | Why is there a `crates/` and a `user/src/`, and what decides which? | shared-by-two-binaries goes in `crates/`; host-testable and Kani-reachable is the reason |
| M6 | What is a `BUGS` section for? | a promise about known limits, not an apology, and next to the feature |
| M7 | How would you add a program, and what must you declare about it? | the grant manifest, and that a provisional name is expected |
| M8 | Who decides a name, and what are the three provenance states? | calef; `ratified`, `recorded`, `unrecorded` |

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

## BUGS

- **The rubric is reachable by grep from inside the test, and run 1 hit it.** The stranger found
  this note while researching an ordinary question and read the "pass means" column, which leaks
  partial answers for at least (a), (c), (e), (g) and (h). It disclosed this unprompted, which is the
  only reason the contamination is known rather than silently baked into a score. **Run 2 must not
  have this note in the tree it is given**, or the rubric must move out of the repository, and the
  latter costs the property that everything here is in-tree and versioned with the code. Unresolved.
- **The rubric was written by an agent that has worked in this tree**, which is the same
  disqualification the instrument exists to avoid, one level up. It knows which answers the tree
  gives, so the questions may be shaped around what is answerable. A stranger falling down somewhere
  unasked-about is the check on that, and the rubric section above says to amend rather than defend.
- **Nothing gates this.** The test is run when somebody runs it, which is rung four of CLAUDE.md's
  ladder and the same weakness the milestone was written to fix one level down. A periodic run is
  possible and is not built.
- **The build half cannot be measured from a warm machine**, and every contributor's is warm. The
  first run should be from a container with nothing installed, or the B-rows measure nothing.
