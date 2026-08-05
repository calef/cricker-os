# 117. The stranger test: could someone build this and understand it without asking

**Status: NOT-STARTED.** Minted 2026-08-05 by Chris, to put the third principle to a test rather than
leave it as an aspiration.

**Gate: NONE.** The instrument exists and the first run costs one lane.

**The question, which is the principle's own wording:** *could a competent stranger, with only this
repository, reach a passing build and a correct mental model without opening a chat window?* Where
the answer is no, that is a bug in the tree and not in the stranger.

## Why this needs a milestone rather than good intentions

Every principle in CLAUDE.md names a mechanism that holds it when nobody is watching, and this one's
mechanism was missing. "Write good docs" is rung four of the ladder: a note, relying on somebody
remembering. This milestone is the gate.

**It also cannot be self-assessed, and that is the whole difficulty.** Chris cannot take this test;
he wrote the system. Nor can any agent that has worked in this tree, which by 2026-08-05 is most of
them: an agent that spent a night merging pull requests here knows why `cricker-dev` is a symlink,
what a lane is, and that `script/lint` fails on a branch prefix. **Knowing the answer disqualifies
you from being the instrument.**

## The instrument: a stranger with no context

Spawn an agent that has **never seen this tree**, hand it the repository and nothing else, and give
it a task. No brief explaining the conventions, no pointer to the right note, no answer to any
question it asks. Its confusion is the measurement.

Three things make this a real test rather than theatre:

- **It must be a fresh context**, not a summarised one. A handoff that says "read CLAUDE.md first"
  has already given away the finding that a newcomer would not know to.
- **Every question it asks is a defect**, recorded verbatim. The questions are the deliverable, more
  than the score is.
- **Every wrong answer it is confident about is a worse defect**, because a document that misleads
  costs more than one that is silent. Milestone 97 found six citations pointing at the wrong record;
  the same failure in prose is what this looks for.

## The two halves, and only one is mechanical

**The build.** From a clean clone on a machine with nothing installed: does `script/setup` then
`script/test` reach green, following only what the repository says? This is checkable and probably
partly broken, because nobody has run it from cold in weeks and every contributor's machine is
already warm. Record what it actually took, including anything the reader had to know that no file
said.

**The mental model.** Harder, and it needs a rubric written before the run so the result cannot be
graded generously afterwards. A candidate set, each a question the tree *claims* to answer:

- What is a capability here, and what does it mean that designation is authorization?
- Why is there no ambient network, and what would a program have to hold to reach one?
- Where does architecture-specific code live, and what breaks if it lives elsewhere?
- What does `BUILT` mean on a roadmap row, and what does `RECORDED` mean?
- Why is there a `crates/` and a `user/src/`, and what decides which a thing goes in?
- What is the frame in a socket contract, and why does a listener not have one?
- How would you add a program, and what would you have to declare about it?

Grade against what the tree actually says, not against what a maintainer knows. A question the tree
answers only in a commit message is a question the tree does not answer.

## What the work will be, and it is not writing prose

The output is a **worklist of defects**, and the shapes are predictable enough to name now:

- **Entry point.** `README.md` has eleven sections and no stated reading order. A stranger does not
  know whether to start at "Try it", "Quick start", or "The notes are the point".
- **No `CONTRIBUTING.md`.** GitHub links it from the pull request UI and it does not exist, so the
  answer to "how do I propose a change here" is nowhere. `CLAUDE.md` is the closest thing and it is
  addressed to an agent, not to a person.
- **119 notes with no path through them.** `notes/README.md` is an index, which answers "what exists"
  and not "what do I read first".
- **The conventions that are load-bearing and unstated for a human**: that a lane is a worktree plus a
  branch, that `origin/*` moves under a worktree, that names are ratified, that a `BUGS` section is
  a promise rather than an apology.

**Fix what the run finds, then run it again with a second stranger.** One pass measures; two passes
show whether the fixes worked, which is the difference between an audit and a milestone.

## Scope note

**Not a documentation rewrite.** The tree's documentation is unusually good and this milestone must
not turn into polishing it. The deliverable is the set of places a stranger *actually* fell down,
which is a much smaller and much more specific list than a review would produce.

**Not a beginner's tutorial either.** The reader is a competent systems engineer who has never seen
this project. The test is not whether they understand a page table; it is whether this repository
tells them what *this* system does with one.

**The honest limit, stated up front**: an agent is not a person, and its failure modes are not
identical. It will not get bored, will not give up out of frustration, and will read further than a
human would before asking. So the result is a **lower bound** on the friction a human would meet, and
the milestone should say so wherever it reports a number.
