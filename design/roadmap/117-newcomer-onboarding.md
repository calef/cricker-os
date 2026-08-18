# 117. The stranger test: could someone build this and understand it without asking

**Status: PARTIAL.** Minted 2026-08-05 by calef, to put the third principle to a test rather than
leave it as an aspiration. The rubric was written 2026-08-14 (notes/stranger-test.md), run 1 went the
same day, **run 2 went 2026-08-16** (pull request #219), and **run 3 went 2026-08-18**. Three runs,
eleven defects fixed between the first two, and the two documents the block predicted now exist:
`CONTRIBUTING.md` at the repository root, and a reading order at the top of `README.md`, both landed
by run 3's lane and both **provisional**, because a reading order is a claim about what matters and
those are calef's.

**Gate: NONE.** The instrument exists and has now been run three times. What is missing is a
mechanism that runs it without being remembered, which is rung four holding up the milestone that
exists to fix rung-four problems.

**Run 3 closed the isolation question, which was the only thing making run 2 unscorable.** Run 2's
strangers were subagents of a maintainer session whose working directory was the repository, so
`AGENTS.md` arrived in their context at turn zero and five of the eight rubric rows went unscored.
Run 3's stranger was a separate process, started with `--safe-mode` from a directory whose *child*
was the clone, since project instructions load from ancestors and never from descendants. It was
verified twice rather than assumed, and **all eight rows plus B1 are scored**: five answered, one
partly, one absent, one induced from instances rather than read.

**It also found the harness leaking harder than the tree does**, which is the finding this block
should carry forward rather than bury. The run's log files sat in the stranger's own working
directory under the names `stranger3-stream.jsonl` and `stranger3-stderr.log`, so its first `ls -la`
told it that it was stranger 3. It disclosed that first when asked, and said the knowledge is why it
added a program rather than only describing one. **Three runs, three different isolation failures,
every one of them in the harness rather than in the repository.** Run 4's fix is one line: the logs
go in a sibling directory.

**What run 3 found, and none of it was fixed in its own lane**, because a run that stops to fix
things stops measuring and its findings stop being traceable:

- **`notes/adding-a-program.md` is stale again**, two days after run 2 corrected it. Milestone 130
  deleted the `mkinitrd()` shape its step 4 describes, and `manifest()` is a seventh `grant_plan`
  edit the list of six omits. Recorded in that page's own `BUGS`. **The page has now been rewritten
  by two successive strangers and gone stale between them**, which is the argument that this is not
  a documentation problem: one fact lives in five hand-maintained places, two of the seven edits are
  compiler-forced, and the two that fail *silently* are both in the unenforced five.
- **`script/test` is intermittently red on a contended host**, at a measured 2 in 13 aarch64 legs
  plus two reds of the sibling assertion, at a load average of 45 to 63 caused by other lanes gating
  on the same machine. The tree already knows this; what run 3 adds is an independent reproduction,
  the observation that the sibling's eight-retry loop also exhausts, and that **the panic message
  contradicts the note that explains it** so a developer meeting the failure is told it is the
  kernel's bug. Recorded in notes/load-sensitive-assertions.md's `BUGS`.
- **Nothing tells anyone the machine is shared with other lanes**, which is the single most
  load-bearing fact about any timing result and cost the stranger an hour.
- **A stranger doing ordinary work reaches no file under `design/decisions/`**, and reaches neither
  `notes/net.md` nor `notes/capabilities.md`. Two of those carry rubric answers, which is why M2 is
  absent and M1 only partial.
- **The rubric itself has aged.** M8 asks for three provenance states and §89 made it four; M1
  quotes a phrase this tree has never written. Amended in the note rather than defended.

**Why this stays PARTIAL.** The instrument is proven and the two predicted documents exist, but the
milestone's own sentence is *"fix what the run finds, then run it again"*, and run 3's findings are
recorded rather than fixed. What is left is small, specific, and listed above.

**This block said "Run 2 is the remaining half" and was falsified thirty-nine minutes later**, when
pull request #219 merged without touching this file, and it stayed wrong for a day. That is the
second time this one milestone's status has gone stale in exactly the same way, which is the
strongest single argument in the tree for §76's defect class being structural rather than careless.
Found 2026-08-17 by the status-accuracy sweep; the `IN-PROGRESS` token was additionally false
because no branch existed, the three lanes having been `milestone/117-stranger-test`,
`fix/stranger-run-findings` and `claude/milestone-117-7ik9e6`, all merged and all deleted.

**The column said `NOT-STARTED` until 2026-08-16, with run 1 already recorded in three other files.**
That is §76's failure again and it is worth naming here rather than quietly correcting: the gate
compares the index row against this file's own status line, both of which said the same wrong thing,
so agreement is not accuracy. Nothing in the tree can see that a milestone has moved except a person
who moves it.

**The question, which is the principle's own wording:** *could a competent stranger, with only this
repository, reach a passing build and a correct mental model without opening a chat window?* Where
the answer is no, that is a bug in the tree and not in the stranger.

## Why this needs a milestone rather than good intentions

Every principle in CLAUDE.md names a mechanism that holds it when nobody is watching, and this one's
mechanism was missing. "Write good docs" is rung four of the ladder: a note, relying on somebody
remembering. This milestone is the gate.

**It also cannot be self-assessed, and that is the whole difficulty.** calef cannot take this test;
he wrote the system. Nor can any agent that has worked in this tree, which by 2026-08-05 is most of
them: an agent that spent a night merging pull requests here knows why `nife-dev` is a symlink,
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
  know whether to start at "Try it", "Quick start", or "The notes are the point". *(Landed
  2026-08-18 as a `## Start here` section, provisional. Run 3 is the evidence it was needed and the
  evidence it is not enough: its stranger built a good order by instinct, reached `AGENTS.md`
  twelfth from a pointer at line 226, and reached `crates/abi/src/lib.rs`, four syscall numbers and
  the whole design on one screen, far too late to help it.)*
- **No `CONTRIBUTING.md`.** GitHub links it from the pull request UI and it does not exist, so the
  answer to "how do I propose a change here" is nowhere. `CLAUDE.md` is the closest thing and it is
  addressed to an agent, not to a person. *(Landed 2026-08-18. It links to `AGENTS.md` rather than
  restating it, because the two have different readers and milestone 118 is shrinking `AGENTS.md`;
  a second copy would be work in the other direction. Untested: no stranger has seen it, since run
  3's clone predates it.)*
- **119 notes with no path through them.** `notes/README.md` is an index, which answers "what exists"
  and not "what do I read first".
- **The conventions that are load-bearing and unstated for a human**: that a lane is a worktree plus a
  branch, that `origin/*` moves under a worktree, that names are ratified, that a `BUGS` section is
  a promise rather than an apology.

**Fix what the run finds, then run it again with a second stranger.** One pass measures; two passes
show whether the fixes worked, which is the difference between an audit and a milestone.

## What run 3 hands off, 2026-08-18

None of this was done in run 3's lane, on purpose. Each is specified precisely enough to act on, and
each is recorded next to the feature as well as here, so a reader who never opens this block still
meets it.

1. **Print the host load average beside a timing-assertion failure.** An afternoon, and it is the
   cheapest item here by a wide margin. notes/load-sensitive-assertions.md has already established
   that host load is the discriminator for this whole family and has already measured the rate
   against a load average; the harness is in a position to sample `uptime` and does not. Today a
   contended host produces a message that blames the kernel and the reader has to think of `uptime`
   unprompted, which cost run 3 an hour. ***Done 2026-08-18, and it took an afternoon.***
2. **Correct `notes/adding-a-program.md` step 4**, and count `manifest()` as the seventh
   `grant_plan` edit. Five minutes, and it belongs to whoever lands the next program, per that
   page's own convention. ***Done 2026-08-18, and the five minutes was the wrong estimate for the
   right reason: walking the page rather than reading it found three more defects.***
3. **Stop `timer.rs`'s panic message asserting a false dichotomy.** Fifteen minutes, both ISAs. Do
   not widen the bound; the tree has rejected that twice and is right. The sentence just should not
   claim something the code cannot support. ***Done by milestone 62, which deleted the assertion
   carrying the sentence on both ISAs rather than rewording it, the bound untouched as asked.***
4. **Adding a program should not need five hand-maintained lists.** A milestone, and it is the one
   run 3's stranger nominated as the highest-value thing a newcomer could offer. A `Prog` variant
   could carry its archive name and its manifest as data and both initrd tables could be generated
   from it. This is a design fork rather than a lane: it is why a page two strangers have now
   rewritten went stale between them, and by the ladder's own reading it is a rung-one answer to a
   problem currently answered at rung four.
5. **Run 4, with the harness's logs in a sibling directory**, and with `CONTRIBUTING.md` and the
   reading order in the tree it is handed. Neither has been seen by a stranger: run 3's clone
   predates both.

## The handoffs lane, 2026-08-18

**Status does not move: still `PARTIAL`.** This lane took the three cheap items run 3 recorded and
did not fix, and none of them is the milestone's own sentence ("fix what the run finds, then run it
again"). Handoffs 4 and 5 above are untouched: run 4 is still owed, and so is the design fork.

- **Print the host load average beside a timing-assertion failure: done.** `HostLoad` in
  `xtask/src/main.rs` samples `uptime` every five seconds for the length of an emulated leg and
  reports min/mean/peak, the core count and the oversubscription factor when the leg goes red, on
  both the TCG and the `--hvf` legs. Recorded in notes/load-sensitive-assertions.md, under the
  diagnostic section it belongs to, with its own `BUGS`.
- **Correct `notes/adding-a-program.md` step 4, and count `manifest()` as the seventh `grant_plan`
  edit: done, and the page was wrong in three further places** nobody had found. `cargo xtask build`
  claimed to pack both archives and packs one, the `SHELL_CHECK_SCRIPT` example it gives does not
  compile against that array's type, and the page said which edits exist without ever saying which
  ones the machine catches. The page was verified by walking it with a scratch program, added and
  removed, rather than by reading it.
- **Stop `timer.rs`'s panic message asserting a false dichotomy: already done by milestone 62**, in
  the branch that was live while this lane ran. 62 deletes `the_handler_keeps_up_when_no_lock_is_held`
  on both ISAs, which is where that sentence lived, and replaces the sibling test's exhausted-budget
  panic with an `UNMEASURED` report. Nothing was re-fixed here.

**One thing measured here that changes what a reader should believe.** Run 3 recorded that a missing
`from_name()` arm fails silently. It does, but only on a condition nobody had stated: `PROG_COUNT` is
the keystone, and `prog_id_round_trips`' own doc comment claimed the opposite of what it does. A
variant added with its three compiler-forced arms and none of `from_id`, `from_name` or `PROG_COUNT`
**compiles and passes every host test**. The doc comment is corrected in place and step 6 now carries
the measured table.

That sharpens handoff 4 rather than answering it: the mechanism it asks for would make all three of
those unnecessary, and Rust offers no way to count an enum's variants without a derive macro this
tree has not taken, so the gap cannot be closed with a gate.

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
