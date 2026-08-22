# 117. The stranger test: could someone build this and understand it without asking

**Status: PARTIAL.** Minted 2026-08-05 by calef, to put the third principle to a test rather than
leave it as an aspiration. The rubric was written 2026-08-14 (notes/stranger-test.md), run 1 went the
same day, run 2 went 2026-08-16 (pull request #219), runs 3 and 4 went 2026-08-18, and **run 5 went
2026-08-18, the first conducted through `script/stranger-test` rather than rebuilt by hand.** Five
runs, eleven defects fixed between the first two, and the two documents the block predicted have now
been seen twice: `CONTRIBUTING.md` at the repository root, and a reading order at the top of
`README.md`, both still **provisional**, because a reading order is a claim about what matters and
those are calef's.

**Gate: NONE.** Run 4 said the blocker was the missing recurrence mechanism rather than the
worklist; that mechanism was built the same day and **run 5 is the evidence it works**. Run 5 then
said the only remaining thing was a cadence, and **calef decided it on 2026-08-18: monthly.**

**Update, 2026-08-22 (`## The second handoffs lane` below): five of run 5's seven handoffs are now
landed, and the cadence has been checked and is honest that nothing is owed yet** (`script/stranger-
test --due` reports 26 days remaining as of this writing, four days into the thirty). What remains is
the milestone's own sentence, *fix what the run finds, then run it again*, and "again" is now purely a
matter of the calendar rather than of anyone's attention: the mechanism built 2026-08-18 is the thing
that will say when. Status stays `PARTIAL` because the mechanism has not yet fired a real run 6 under
the cadence, which is the one thing that has never happened and the one thing that would move this to
`BUILT`.

**This block previously said "a gate would be the thing that moves this, and writing one is not a
lane's call either, because it would be a gate on somebody's attention rather than on the tree."
The first half was right and the second was overtaken.** A gate on somebody's attention is exactly
what a cadence is, and deciding how much attention the answer is worth was calef's call rather than
a reason no gate could exist. The sentence stayed true for four days and was answered in one.

**Run 4 scored eight of eight on the mental model and passed B1 for the first time**, which is the
two provisional documents working. `CONTRIBUTING.md` is directly responsible for two rows: the
stranger quoted its definition of a `BUGS` section instead of inducing one from instances as run 3
had to, and it ran the whole of `script/gates` unprompted on that file's own vocabulary rather than
stopping at `script/test`. The reading order moved `AGENTS.md` from twelfth to seventh and
`notes/capabilities.md` from never-opened to eighth. **The failure it leaves is that
`CONTRIBUTING.md` is item 2 of 8 and was read sixteenth of twenty-two**, arriving after the gates it
describes had already been run.

**The build was green on arrival with no change to the tree:** `script/test` exit 0 first try, 1312
passed and 0 failed on both ISAs, then `script/fmt --check` and `script/lint` also exit 0. The
machine was loaded by four other lanes at a load average between about 3 and 17.5 on 8 cores, which
is far below run 3's 45 to 63, and **no timing assertion fired, so the load-average diagnostic that
run 3's handoff bought is still unexercised.**

**The isolation fix worked and the leak moved into the tree, which is the finding this block should
carry forward.** Run 3's log files told its stranger it was stranger 3; run 4's logs went to a
sibling directory and its first `ls -la` returned only the clone. It knew anyway, within half an
hour, from `notes/adding-a-program.md`'s `BUGS` table, whose rows name run 2's `doubler` and run 3's
`triangle` because each of those lanes recorded its walk exactly as this project's conventions
require. **Four runs, four isolation failures, and this is the first one that is a consequence
rather than a mistake**: the instrument's own good practice generates the leak, it compounds one row
per run, and every obvious remedy is worse than the leak. The stranger's disclosure is unusually
specific about the cost: it *"performed"*, chose its deliverable partly for how it would read, and
adopted the tree's prose style as mimicry. It did not invent a breakage, skip a gate, or open the
withheld note.

**What run 4 found, and none of it was fixed in its own lane**, per the same rule runs 3 and 4 both
followed:

- **`script/lint`'s naming worklist under-counts by exactly the `provisional` names** and then names
  the command that prints the other number: `82 still want calef` against `UNRATIFIED (86 of 162)`,
  and a census line that sums to 158 of 162. It bites precisely the state a newcomer is told to use.
  Recorded in notes/naming.md's `BUGS`.
- **The two archives boot different binaries under the name `init`**, `hello` on aarch64 and
  `builder` on riscv64, stated in a comment 200 lines from the table that needs it. A placement
  finding rather than an absence, and run 3's closing diagnosis reproduced by a different reader.
- **`cargo xtask build` and `script/lint` are both blind to a missing riscv initrd row**, so the two
  commands run most often cannot catch the mistake this page warns about hardest. **And nothing
  counts programs**: 1312 tests before adding one and 1312 after.
- **Removal is the same eight places and has no page.** A half-removed program is a `PROG_COUNT` too
  large and an init slot no variant claims.
- **`notes/adding-a-program.md` was right about everything, including a line number.** First clean
  walk of four, against its own `BUGS` prediction and against the stranger's own written prediction.

**The one criticism no previous run made**, and it is aimed at the habit rather than at a document:
*"the project's habitual response to a structural problem is another document, which its own ladder
names as the worst available move... The documentation is doing work that a data structure should be
doing, and it is doing it beautifully, which is exactly what stops anyone from fixing it."* Its
three instances are all things this tree has written about at length and not changed: `AGENTS.md`'s
misleading name, this page's fourth rewrite, and the eight-place program problem whose one-place fix
is handoff 4 below.
**The harness.** For the first time that claim costs one command rather than an afternoon.
`script/stranger-test` landed 2026-08-18: a lane that wants to run the test cuts nothing by hand, so
"a lane could start today" is now true in the sense the token means. What the milestone still owes is
a run conducted **through** the harness and recorded in notes/stranger-test.md, plus whatever that run
finds. No run has used it, so this block does not get to count the instrument as the measurement.

**What blocked this milestone was never the worklist.** Every run produced a fixable list, three of
the four had their lists fixed, and the status did not move, because the milestone's own sentence is
*fix what the run finds, then run it again* and "again" had no mechanism. Four lanes each rebuilt
the harness by hand and **each got the isolation wrong somewhere the others had not**, which is the
tell CLAUDE.md names for being on too low a rung: a fact that exists only in an operator's memory.

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
   predates both. ***Done 2026-08-18. The sibling directory worked; the tree leaked
   instead, and both documents earned their place.***

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

## What run 4 hands off, 2026-08-18

**Status does not move: still `PARTIAL`, and run 4 is the clearest evidence yet that the worklist is
not what is holding it.** Every run has produced a fixable worklist, three of the four have had
their worklists fixed, and the status has been `PARTIAL` throughout because the milestone's gate is
`NONE`. **The recurrence mechanism is the milestone**, and it is the one thing four runs have
declined to build.

1. **Make the test recur without being remembered.** This is the blocker and it is now four runs
   old. The harness is a clone, one deletion amended into the tip, one `claude --safe-mode`
   invocation from the clone's parent, and a debrief; run 4 also added a `pkill` shim so a stranger
   following `README.md`'s own quit instruction cannot kill another lane's emulator. That is a
   script somebody could write in an afternoon and nobody has, four times. **Until it exists, 117
   cannot move**, because the milestone's own sentence is "fix what the run finds, then run it
   again" and the "again" has no mechanism.
2. **Decide which number `script/names`' worklist line should print.** A gate that says `82 still
   want calef (script/names --unratified)` beside a command that says `86` is under-reporting the
   `provisional` names, which are the ones whose own author has said they are wrong. Fifteen minutes
   of code and one decision about what the worklist is for. Recorded in notes/naming.md's `BUGS`.
3. **Say at the riscv initrd table that it boots a different `init`.** A comment, on the row that
   needs it rather than 200 lines away. Five minutes, and the fact is already written, which is what
   makes it a placement bug rather than a documentation one.
4. **Handoff 4 from run 3 is unchanged and is now nominated by two successive strangers**: adding a
   program should not need eight hand-maintained lists. Run 4 added the removal direction to the
   case, and made the sharper version of the argument: `cargo xtask build` and `script/lint` are both
   blind to the riscv omission, and nothing in the suite counts programs, so no gate can close the
   gap that prose is currently holding shut.
5. **Run 5 should be told it is being measured, rather than hidden from it.** The tree's own record
   now leaks that fact within half an hour and cannot stop; pretending otherwise buys nothing and
   costs the disclosure. What still must be withheld is the answer key, which has held four times.
## The recurrence lane, 2026-08-18

**Status stays `PARTIAL`, and the gate is no longer `NONE`.** This lane built `script/stranger-test`
and ran nothing through it, on purpose: conducting run 5 in the lane that wrote the instrument would
make the harness's author the harness's only evidence.

**What it holds.** The clone goes inside the stranger's working directory rather than being it,
which is the isolation run 3 established and the only reason the mental-model rows are scorable. The
answer key is withheld the way runs 2 through 4 withheld it, extended to cover markdown links to the
note, because `script/lint` fails on a relative link that does not resolve and a red gate the
harness caused is a worse contamination than the edit. Logs go in a sibling directory with no run
number in any path element. `pkill` and `killall` are shadowed to this clone's QEMU, which run 4 did
by hand and recommended keeping. The isolation is probed before the run and the run stops if the
probe does not answer NONE. The stranger is told it is being measured, which is run 4's handoff 5
acted on. The account-wide `nife-dev` link is recorded before the run and restored after, since a
stranger's `script/test` takes it and its tree is disposable in a way a lane's is not.

**Three defects in the harness were found by testing it rather than by reading it**, which is the
argument for `--smoke` being part of the deliverable rather than scaffolding. The first clone leaked
this project's live branch names, including this lane's, to `git branch -a`. The reflog was cleared
before the withholding amend instead of after, so it carried a line reading `commit (amend)` beside
the tip's subject. And the rubric extractor matched every `| M<n> |` row in the note, which handed a
stranger runs 3 and 4's *scores* as part of its own question list; the smoke run's reply quoted them
back, which is how it was caught.

**What it does not do**, in its own `BUGS` section where a reader meets it: it cannot make the
operator un-read `AGENTS.md`; it cannot stop the tree leaking that a test exists, and it adds one
more leak by being a `script/` entry point; it does not schedule itself, which is a cadence decision
and calef's; and the withheld note is still one `git show HEAD~1:<path>` away, which is measured
rather than closed, since the summary reports whether the transcript ever mentions it.

**The name `stranger-test` is provisional**, as `script/` names are calef's. It takes its noun from
the note that named the instrument first, so the command and the record share a word.

## What run 5 found and hands off, 2026-08-18

**Status does not move: still `PARTIAL`**, for the reason in the second paragraph of this block
rather than for run 4's reason. Run 4's handoff 1 is the one this run closes and it closes it only
half way: the mechanism exists, it has now been exercised by somebody who did not write it, and
nothing schedules it.

**What run 5 measured that no previous run could.** It is the first run through
`script/stranger-test`, the first whose stranger was told at turn zero that it was being measured,
and the first on an uncontended machine (load average 2.44 on 8 cores at launch, no other lane, no
QEMU). 201 turns, about 29 minutes, `$10.56`, plus a `--smoke` run at `$0.09` that exercised every
stage first. Six of eight mental-model rows answered, one partly, one absent. Full record and the
scored rubric in notes/stranger-test.md.

**The result that matters most is not on the rubric.** The harness isolates a tree and cannot
isolate a machine. `nife-dev` is an account-wide `rustup` link, so the clone's first `script/test`
compiled `std` out of another worktree's farm, and `std-aborts` failed **naming two files and two
line numbers inside a checkout the stranger had been told did not exist for it**. Both fixes the
error message suggested would have written a false statement into `ABORTS_ACCEPTED`. The stranger
diagnosed it only by grepping for the foreign path, which led it to `script/stranger-test` itself,
so **the harness contaminated the run and then rescued it**. Recorded in notes/std.md's `BUGS` and
in `script/stranger-test`'s.

**`script/apropos` did not fire, and that is this run's cleanest negative result.** It landed
2026-08-18 because three runs could not reach `notes/net.md`, `notes/capabilities.md`, any
`design/decisions/` file, or `crates/abi/src/lib.rs`. Run 5 never ran it, still reached no
`design/decisions/` file, and still never opened `notes/net.md`. It had the name in front of it
three times: in `ls script/`, in the guest builtin in `crates/swish/src/lib.rs`, and in
`SHELL_CHECK_SCRIPT`'s transcript. The only page that says what it does is `notes/scripts.md`, which
five runs have not opened. Recorded in `script/apropos`'s `BUGS`.

**`AGENTS.md` was never opened**, by a stranger that read `CONTRIBUTING.md` third and shipped a
working program on both ISAs. Run 4 read `AGENTS.md` seventh and complained that `CONTRIBUTING.md`
was sixteenth; fixing the second appears to have cost the first, because a reader who meets a
shorter document summarising a longer one stops. Its own reason: *"66 KB is a large upfront cost
when a task is in front of you, and everything I actually needed turned out to be reachable from
code."* This is the same criticism run 4 made of the tree's prose habit, arriving independently and
with a falsifiable instance attached.

**What run 5 hands off.** None of it was done in its lane, per the same rule runs 3 and 4 followed.

1. **Assert that `std_aborts()`' dep-info paths are under `farm_dir()`.** One comparison, and it
   turns a false accusation about somebody else's source into a true statement about the machine.
   Say the recovery (`rm -rf std_exerciser/target`) in the failure message, since the failure caches
   and re-running reproduces it in thirty seconds, which reads as stable rather than stale. This is
   the cheapest item here and it is on the customer path by way of trust: a gate that names a file
   and a line and is wrong costs more than a gate that says nothing.
2. **Decide whether an argument-plus-input manifest is wanted.** Today it is possible to declare and
   it turns `the_arg_line_follows_the_manifest_for_every_program` red, in `crates/swish`, which the
   person adding the program has no reason to open. The test sweeps the enum and asks the manifest
   about the argument, then hard-codes `Holdings::default()` and the line `"<name> 21"`. Either the
   sweep learns the rest of the manifest or the combination is refused at `plan_against_with` with a
   comment, the way file-plus-input already is. **This is a fork rather than a lane**: a comment
   there rules out file-plus-input on `ArgSpec` grounds and says nothing about this case, so a
   newcomer cannot tell headroom from a deliberate gap. Recorded in notes/adding-a-program.md's
   `BUGS`.
3. **`CONTRIBUTING.md` says `script/gates` runs three stages and it runs five.** Five minutes. The
   two it omits are `script/icount` and `script/test --hvf`, and the HVF leg is the slowest and the
   only stage that flaked in this run. It matters more than an ordinary drift because that sentence
   is what earned run 4's best unforced result.
4. **Two stale counts, each five minutes**: `script/setup`'s comment names `nightly-2026-07-26` when
   the pin is `nightly-2026-08-18`, and `README.md`'s reading order counts 403 markdown files and
   143 notes when there are 413 and 145. Both are the failure this tree already names, a duplicated
   fact rotting, and both were the first things run 5 wrote down.
5. **Say where `script/apropos` is, somewhere a newcomer reaches.** Its own `BUGS` now records that
   five runs missed it; where it should be named is a claim about the reading order, so it is
   calef's rather than a lane's. **Until it is answered, the tool built to close the
   `design/decisions/` gap has closed nothing**, which is measurable and was measured.
6. **Handoff 4 from runs 3 and 4 is unchanged and is now nominated by three successive strangers**:
   adding a program should not need eight hand-maintained lists. Run 5 adds that the eighth is not
   even a constant, since it depends on the manifest shape.
7. **The cadence for running this is calef's and nothing else can move the status.**
   ***Answered 2026-08-18: monthly, and built the same day.*** Two things in this handoff were
   wrong and are worth correcting rather than deleting. Milestone 129 is **not** the machinery: it
   is nife's own cron, and this run happens on a host with a `claude` CLI and two QEMUs, so the
   machinery is the audit cadence's, one directory over. And the `$10.56` is the CLI's
   `total_cost_usd` for the **stranger process alone**; it excludes the lane that pre-registers,
   watches, debriefs, scores and writes up the run, which is the larger half. Quoting it without
   that caveat under-prices a run by more than half, and this block had already done so twice.


## The cadence lane, 2026-08-18

**Status stays `PARTIAL`, and for the first time the reason is not "nothing schedules a run".**
calef decided the cadence (monthly) and this lane made it mechanical, in the shape §74 and
`script/audits` already established for security audits rather than in a second one invented for the
same job.

**What it is.** `script/stranger-test --due` exits 1 when a run is owed and prints the command to
run and what a run costs. `.github/workflows/stranger-cadence.yml` asks it at 08:00 UTC on Mondays,
in its own workflow rather than in `script/lint`, because a run coming due is information about the
tree and not a defect in whichever commit happened to be pushed that week. `script/stranger-test
--check` is the structural half and **is** in `script/lint`: the cadence sentence appears exactly
once, the run headings are numbered from 1 without a gap, and their dates are in order. A malformed
record is a defect in the commit that malformed it, and it does not fail loudly, it silently moves
the date the tripwire measures from.

**Where the numbers come from, which is the part worth copying.** Both live in
notes/stranger-test.md and nowhere else: the interval is the sentence a reader meets
(`**A run is due every 30 days.**`, matched literally) and the date is the newest
`### Run <n>, <date>:` heading, which every run already writes because that is how the note records
a run. Nothing is maintained for the tripwire's benefit, so the schedule and the record cannot
disagree. A hand-maintained date in a workflow or a script would have been a fact kept in two
places, which is the failure this tree has watched often enough to have named.

**Both cadence modes are handled above every line that clones or spawns anything**, which is a
guard rather than tidiness. This script's *default* mode spends money, unlike `script/audits`, whose
every mode is a report. So `--due` needs no `claude`, no clone and no toolchain (the weekly job runs
it in seconds on a bare checkout), and a workflow that loses its flag cannot fall through into a
run.

**The honest limit, stated in the script, the workflow, the note and here.** This is
**notification, never execution**. A run spawns a `claude` process, spends real budget, needs a
machine with the pinned toolchain and both QEMUs, and ends in a debrief a person scores against the
rubric. Nothing in CI can do any of that. **Red means run the test.**

**Watched firing and watched staying quiet**, because a mechanism nobody has seen fire is not a
mechanism. Against the real record (run 5, 2026-08-18) it exits 0 and says "not due for another 30
days". Against a copy of the note with the run headings shifted back it exits 1 at 30 and 31 days
and 0 at 28 and 29, so the boundary is where the sentence says it is. Six malformations of the
record were fed to `--check` and each was rejected with the reason: the cadence sentence deleted,
the cadence sentence written twice, a run number skipped, the runs out of date order, no run
heading at all, and `2026-02-31`.

**What this does not do**, and it is the residual every record-derived signal has: the tripwire
believes the headings. A run conducted and never written up leaves the check red, which is the right
direction to be wrong in since the note is the only place a run exists. A heading nobody earned
turns it green, which is the same hole `script/audits` names in the same words: closing a cadence by
editing the record instead of doing the work is available, cheap, and the one thing that makes the
whole mechanism a lie.

**What would make this `BUILT`.** Run 5's handoffs landed, and run 6 conducted under the cadence
rather than because somebody thought of it. The second is the one that has never happened.

## The second handoffs lane, 2026-08-22

**Status stays `PARTIAL`.** This lane took run 5's seven handoffs, landed the ones that were code
or prose fixes, gave the one design fork nominated by three successive strangers a tracked home, and
checked whether the cadence built 2026-08-18 actually owes a run. It found run 5 had not, in fact,
fully handed off: five of seven items were still open four days later, which is what this lane closes.

1. **`std_aborts()`'s dep-info paths are now asserted to be under this worktree's own `farm_dir()`,
   done.** `foreign_std_sources()` in `xtask/src/main.rs` canonicalizes every path
   `compiled_std_sources()` found and compares it against the farm, and `std_aborts()` calls it before
   doing anything else. A contaminated build now fails with the machine explanation run 5 asked for
   (the account-wide `nife-dev` link pointed at a different worktree) and the recovery command (`rm
   -rf std_exerciser/target && cargo xtask std-exerciser`), instead of naming a file and a line inside
   a checkout that was never handed to the caller. Not yet exercised against a real contaminated farm
   in this lane, since reproducing run 5's race would mean deliberately racing another worktree's
   `xtask std-src`; the existing `std-aborts` host coverage and a clean `script/test` are what verify
   it here.
2. **The argument-plus-input manifest fork: unchanged, correctly.** It was already recorded in
   `notes/adding-a-program.md`'s `BUGS` with the file and the test it turns red, and that recording is
   the tracked home the finding needs; deciding whether the combination is wanted is a design fork
   above a lane, not a gap this lane could close by picking an answer.
3. **`CONTRIBUTING.md` now says `script/gates` runs five stages, done.** It names `script/icount` and
   `script/test --hvf` by name and points at notes/load-sensitive-assertions.md for the HVF leg's
   known flake, instead of undercounting by two.
4. **The two stale counts, done, and both had drifted further since run 5 measured them.**
   `script/setup`'s comment now says `nightly-2026-08-22`, the pin as of this lane rather than the
   `nightly-2026-07-26` it had said since before run 5 (the pin had already moved twice more in the
   four days between). `README.md`'s reading order now says 425 markdown files and 145 notes; the
   actual count had grown to 413/145 by run 5's own measurement and 425/145 by this one, while the
   page still said 403/143. The fact that a count fixed by run 5's handoff lane on 2026-08-18 (see
   `## The handoffs lane, 2026-08-18` above, a different count of a different kind) had already gone
   stale again by 2026-08-22 is the same finding this tree keeps making about duplicated facts, not a
   new one.
5. **Where `script/apropos` should be named: unchanged, correctly.** Its own `BUGS` section already
   records that five runs have missed it and says why it is calef's call rather than a lane's: naming
   a place in the reading order is a claim about what matters. Nothing here decides that.
6. **Handoff 4/6, nominated by three successive strangers, now has a tracked home: minted as
   milestone 150**, "Adding a program should not need eight hand-maintained lists"
   (`design/roadmap/150-program-declaration-data.md`, provisional number; 147, 148 and 149 were
   already claimed by other lanes' pull requests, one of them merged into this lane's own base
   commit). `notes/adding-a-program.md`'s `BUGS` section now points at 150 instead of at this
   milestone's own handoff list, which was a circular reference: the mechanism it asked for is not
   something milestone 117 itself builds.
7. **The cadence: already answered and built by 2026-08-18, unchanged.** Confirmed rather than
   re-done: `script/stranger-test --due` reports "not due for another 26 days" as of 2026-08-22, four
   days into the thirty since run 5. No run was conducted in this lane, on purpose, because inventing
   one the cadence does not yet owe would be exactly the failure `## The cadence lane` above names:
   closing a cadence by doing work that was not due rather than by the calendar actually asking for
   it. `script/stranger-test --check` and `script/roadmap --check` both pass against the record as
   this lane leaves it.

**What remains for `BUILT`.** Nothing left in this lane's control. The next thing that can move this
milestone is a run 6, conducted when `script/stranger-test --due` next exits 1, roughly 2026-09-17 on
the current record. Everything else run 5 asked for either has a tracked home now or was never a
lane's call to make.

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
