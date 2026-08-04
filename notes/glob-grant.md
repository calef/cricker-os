# Globbing, and the property that the expansion you see is the grant

Milestone 47's globbing lane. Built 2026-07-31. [glob.md](glob.md) is the matcher, a total function
on two byte strings with no filesystem in it; this note is the layer that turns a match into an
**authority**, and the demonstration that hangs on it.

The code is `crates/grant_plan/src/expand.rs` (the expander and the name set, host-tested),
`crates/fs_proto`'s `nameset` module (the wire encoding), `user/src/fs_nameset_caretaker.rs` (the
caretaker), `user/src/swish.rs` (`echo`, and the grant path), `user/src/rm.rs` (the namespace mode),
and `kernel/src/user/fs_service.rs`'s `start_granted_set`, and `kernel/src/user/glob_grant_tests.rs`.

Read [dir-capability.md](dir-capability.md) first for the rights ladder and `fs_subtree_caretaker`,
and [rm.md](rm.md) for why `rm` is a program with a directory grant. This lane is the wiring the
other two left as the obvious next thing.

## What a match grants, which was the whole question

`rm *.txt` matching five hundred files has to convey exactly those five hundred and nothing else.
The roadmap's four candidates and its verdict, which this lane implements rather than revisits:

| answer | verdict |
|---|---|
| five hundred file capabilities | honest, and it exhausts capability slots |
| the directory plus a name list | cheap, and it **over-grants catastrophically** |
| make `rm` a builtin | dodges the question, and costs `rm` as a program |
| **a directory capability attenuated to a name set** | the principled one |

The finding that makes it tractable is that this is a small change: `fs_file_caretaker` already
serves *a namespace of exactly one name*, so globbing generalizes the namespace and nothing else.
Same caretaker shape, same `fs_proto` protocol above and below, **nothing new in the kernel**.

That generalization is in the type system rather than only in the prose. `grant_plan::DirGrant` used to
carry `name: &[u8]`; it now carries `names: NameSet`, and a literal operand is the set of one.

## The property worth the lane

**The expansion you see is the grant.** `echo *.txt` prints literally the authority that `rm *.txt`
would transfer, because the matched set *is* the namespace the caretaker will serve.

Unix cannot make that claim, and the reason is worth being precise about rather than waving at.
Unix's `rm` gets its authority from the uid it inherits; the glob only tells it which of its existing
powers to use. So `echo *.txt` on Unix prints a list of names that happens to be what `rm *.txt`
would delete, which is a coincidence of good behaviour by `rm`, not a fact about what it was handed.
Here it is the same object: the shell expands once, and the names it printed are the names the
caretaker is built from.

It only means anything if both go through **one** expander, which is why `grant_plan::expand::Expander`
exists and why both `echo` and the grant planner drive it. The guest test then checks the pairing
from the other end: a shell rooted in the fixture's `globset` runs `echo gl-*.txt` and plans
`rm gl-*.txt`, and reports whether the names agree. The two share the expander and **not** the
plumbing (`echo` goes text → words → expand → print; the grant goes parse → positionals → expand →
`plan` → the grant's names → render), so a planner that narrowed, reordered or added to a set shows
up as a disagreement. Falsified deliberately: pointing the grant path at a wider pattern turns the
report from `0x3f` into `0x3d`.

## The shell expands before it plans, and that changed `plan_against`

The shell expands first, which is also what Unix does, so there is no divergence to earn. The
consequence is structural: **`grant_plan::plan` must see the expanded set rather than the pattern**, since
the endowment is the set.

`plan_against` used to fill its slots by splitting a slice of tokens. It now fills them by **index**,
and takes an `Expansion` keyed to that index. The alternative was to let the planner work out which
slot an expansion belonged to, which would have been the parser classifying tokens again, one layer
down and less visibly (the thing milestone 47 deliberately stopped doing when it removed `file:`).

Two guards fall out, and both exist so authority cannot move silently:

- **A pattern with no expansion behind it is refused** (`Refusal::Unexpanded`), never granted as a
  literal name.
- **A token with no magic never consults the expansion at all**, so a name that was typed always
  designates itself and no caller can substitute a set for it.

The first guard matters more than it looks, and the reason is a **correction**. The obvious argument
for refusing an empty match is that bash's pass-the-pattern-through is harmless here because a name
containing `*` would be refused downstream. It would not be: neither `grant_plan::file_name_fits` nor the
FS server's `check_component` rejects `*` (they refuse the empty name, `.`, `..`, `/`, `\`, `:` and
NUL, and nothing else). Checked rather than remembered, and there is a host test pinning it. What
that leaves is a worse cost: passing the pattern through would build a grant whose namespace is **a
name nobody has**, useless today and live the moment anything creates a file called `*.rs`. A grant
should not be able to acquire a referent after it is written.

`Endowment` also stopped borrowing from the command line, because a name a pattern produced comes out
of a directory listing rather than out of the line. That makes explicit what `FileGrant::dir` already
did by hand: a planned grant carries values, so nothing that happens afterwards can change what it
means.

## Expansion costs `ENUMERATE`, and that is the whole bill

Expanding a pattern is listing a directory, so it costs the authority to list a directory: the rung
[dir-capability.md](dir-capability.md) already separates out. The shell's globbing witness is granted
`ENUMERATE | DESCEND | READ` and **no `REMOVE` at all**, which is the point of `echo` being the half
that demonstrates this: showing the authority costs none of it.

## `fs_nameset_caretaker`, and why it is a third caretaker

There were three candidate shapes, and this was a real fork rather than a formality.

- **A generalization of `fs_file_caretaker`.** The roadmap's phrasing invites it, and it does not
  work. `fs_file_caretaker` serves the *file* protocol: its client `OPEN`s a fixed handle, `CREATE`
  is `ENOTDIR`, and every directory verb falls through to `EBADF`. Teaching it a set means teaching
  it `READDIR`, `UNLINK` and `RMDIR`, which is not generalizing a file caretaker, it is writing a
  subtree caretaker and calling it a generalization.
- **A mode on `fs_subtree_caretaker`.** Tempting, because `fs_subtree_caretaker` already does the
  handle-namespace translation. Refused, and its own design property is the reason:
  **`fs_subtree_caretaker` performs no rights checks at all**, so there is no branch in it that can
  be wrong. A name filter is a check, and one that must be consulted on every name-taking verb
  (`OPEN`, `CREATE`, `OPENDIR`, `MKDIR`, `UNLINK`, `RMDIR`, and both halves of `RENAME`). A mode
  would trade that program's one strong property for a switch, and put a forget-a-verb surface in
  the caretaker that most deliberately has none.
- **A third caretaker.** Taken. It also has a structural reason and not only a stylistic one: **the
  two grants have different shapes.** One name rides in two `START` argument words; a set does not
  fit in any number of registers, so this program is started with a frame as well. Bolting that onto
  `fs_subtree_caretaker` would make every subtree grant carry machinery it does not use.

The honest cost is about thirty lines of handle table duplicated from `fs_subtree_caretaker`. That
is the price of keeping "this caretaker checks nothing" true of the one that says so.

Milestone 61 removed the *other* duplication, which was the dangerous one. Which verbs got asked
"is this name in the set" used to be a list of match arms in this program, so a name-taking verb
added to the contract would have arrived **unfiltered** and a set capability would quietly have
reached a name the pattern never matched. It is now `fs_proto::verb`'s `takes_name()`, one row per
verb, in a host-testable crate: the filter covers a new verb from the moment its row exists. What is
*not* shared is the attenuation. `fs_subtree_caretaker` consults no policy at all and still does
not, which is exactly the property a mode would have destroyed.

The distinction that milestone made load-bearing is `Operand::Name` versus `Operand::Payload`. The
four extended-attribute verbs carry a name in the shared page and it is **not** a name in the
directory, so they pass the filter without being compared against the set. Filtering them would
have refused a program its own file's attributes because `user.com.apple.metadata` is not one of
the names the pattern matched, which is a category error the table now forecloses.

### One rule, and having only one is the design

> **A name that is not in the set does not exist here.**

Reading it, writing it, creating it, removing it and renaming onto it are all `ENOENT`, because in
this scope there is no such name and nothing consulted a permission. That is `fs_file_caretaker`'s
sentence (DECISIONS §27) over a set instead of over one name, and it is why there is no per-verb
policy here to get wrong.

The filter applies **at the granted directory and nowhere else**. A handle minted below it, by
descending into a matched directory (which needs a `-r` grant's `DESCEND`), is unfiltered. That is
right rather than a gap: the pattern selected top-level names, and what is under a directory it
selected was never a question the pattern asked.

`RENAME`'s **destination** is the check that would have been easy to miss. Renaming a matched name
onto an unmatched one destroys a name the capability was never granted, which is an escape even
though nothing was opened. So both names must be in the set, and the consequence is declared rather
than worked around: a set is a *fixed* namespace, so a set capability cannot move a name out of it,
and `mv *.txt` is not something this shape can express.

### `READDIR` is answered from the set

At the granted directory the caretaker does not ask the server at all: the set **is** the namespace,
so there is nothing to filter out of a listing that is not already absent from this one. That avoids
the cursor problem a filtering caretaker would have (the client's index and the server's would
diverge the moment an entry was dropped) and costs no round trip.

It is deliberately not gated on the `ENUMERATE` the grant carries, and that is not a widening: what a
listing here reveals is exactly the set, which the command line already printed before the caretaker
existed.

The price is that a set record carries the entry's **type**, decided at expansion time from what the
directory said. That is the same resolve-at-grant-time rule the rest of a grant follows.

## `rm` is told "everything you can see"

A set grant has no single name to put in the `START` words, so `rm` is started with a grant whose
name is **zero bytes long** (`fs_proto::grant::WHOLE_NAMESPACE`). A name cannot be empty, so that
spelling was free. It means the operand is the namespace, and `rm` learns the names by enumerating
its own capability.

It sweeps in **one listing with no rounds**, unlike the recursive walk, and the difference is a fact
about the namespace rather than an optimization: `empty()` must re-read from cursor 0 because
removing a name shifts a real directory's entries, while a set namespace is fixed, so re-reading
would hand the loop the names this run has already taken away.

## The two costs, designed rather than discovered

### A pattern that matched nothing

**Refused, at the prompt, with nothing spawned.** zsh's default rather than bash's, and here the
model forces it: the expansion is the grant, so an empty expansion is an empty grant and running the
command would be running it with an authority nobody named. The pass-through alternative is worse
than it looks; see the correction above.

`echo` gives the same answer, and it has to. If `echo` printed the pattern where `rm` refuses it, the
two would disagree about what the line designates, which is the one thing this pairing exists to rule
out.

### `ARG_MAX` as a capability limit

You cannot hand a child a hundred thousand names. `nameset::MAX_NAMES` is **8**, and exceeding it is
`Refusal::TooManyNames`: a loud refusal at the prompt, never a truncation. A glob that quietly
granted a prefix of what it matched would be the worst outcome this mechanism has, because the
printed preview and the actual transfer would disagree and only the printed one is checkable.

**Eight is a measurement, and sixteen was the first answer.** The reasoning said sixteen names of
sixteen bytes is 256 bytes at each end and both ends can hold that. The machine disagreed twice: the
shell ran off the bottom of its stack planning one grant, by 256 bytes with two extra pages and by
768 more with four, presenting both times as a data abort on the shell's own `sp` followed by the
60-second lost-wakeup watchdog (the test was still waiting for a report from a process that had
died). The cause is a set travelling **by value** through four frames a debug build does not
collapse: the expander holds one, `Expansion` carries one into `plan`, `designate` returns one, and
the `Endowment` that comes back carries one more.

The fix was the one `spawn_fs_client`'s own comment already prescribes: its four-page cap says a
client needing more is a client whose frames want looking at, not a number that wants raising. So the
bound came down instead of the cap going up.

## `xargs`: batching at the bound (milestone 109)

The eight stands, and the answer at the bound is no longer a refusal. `xargs` is a **third prefix
word** in the shell (`caps` and `time` are the other two): its operand is a whole command line, and
it re-dispatches that line once per batch of what its pattern matched. The name is provisional, like
every name here.

Transcribed from a real prompt, `--features shell` on aarch64, over the `globmany` fixture's eleven
names:

```sh
$ echo globmany/m-*.txt
  that pattern matched more names than one grant can carry (at most 8)
$ xargs echo globmany/m-*.txt
  batch 1: m-00.txt m-01.txt m-02.txt m-03.txt m-04.txt m-05.txt m-06.txt m-07.txt
m-00.txt m-01.txt m-02.txt m-03.txt m-04.txt m-05.txt m-06.txt m-07.txt
  batch 2: m-08.txt m-09.txt m-10.txt
m-08.txt m-09.txt m-10.txt
  11 names, in 2 batches
```

And the half that says what would actually move, which is the acceptance evidence: the authority per
batch is exactly that batch, not the eleven the pattern matched and not the directory they live in.

```sh
$ xargs caps rm globmany/m-*.txt
  batch 1: m-00.txt m-01.txt m-02.txt m-03.txt m-04.txt m-05.txt m-06.txt m-07.txt
  rm would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    cap 2  endpoint  dir      /globmany  (the directory holding m-00.txt ... m-07.txt)
           ...and nothing under it: no -r, so it cannot even look
  reading the command is reading its whole authority.
  batch 2: m-08.txt m-09.txt m-10.txt
  rm would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    cap 2  endpoint  dir      /globmany  (the directory holding m-08.txt m-09.txt m-10.txt)
           ...and nothing under it: no -r, so it cannot even look
  reading the command is reading its whole authority.
  11 names, in 2 batches
```

(The two `cap 2` rows are elided in the first block only; the prompt prints all eight names.)

### Milestone 47's claim, quoted and then corrected

> **`ARG_MAX` becomes a capability limit rather than a buffer limit.** Unix's "argument list too
> long" is why `xargs` exists; here the ceiling is that you cannot hand a child a hundred thousand
> capabilities. The same failure with a more honest cause, and it wants the same answer (batching),
> so `xargs` earns its place for a better reason than Unix had.

The conclusion is right and the reason recorded for it is the weaker of the two available. "A more
honest cause" is a claim about why the *limit* exists, and a nicer-sounding limit is still a limit.

The claim worth making is about the batching. **Unix's `xargs` is a workaround with no upside: if
`ARG_MAX` were infinite, nobody would ever type it.** Here, batching bounds the authority in flight.
A single grant of five hundred names would mean one process holding five hundred names at once; a
sweep means at most eight are reachable by anything at any moment, and each batch's caretaker dies
with its batch. **We would want this even if the carrier were unlimited**, which is the whole
difference, and it is why the scope note's "the ceiling moves rather than disappearing" understates
the case: a better carrier would make `xargs` optional and would not make it pointless.

### Why it is a prefix word and not a program, which is the fork

A batching **program** has to hold, at once, at least the union of every batch it hands out. There is
no carrier for that union: that is the premise of the milestone, since a set larger than eight is
exactly what cannot be handed over. So an `xargs` program could only be given the **directory** plus
a pattern, and that is the second row of this note's own table (`the directory plus a name list`,
*over-grants catastrophically*) wearing a different hat. It would hold authority over every name in
the directory, including the ones the pattern did not match, in order to hand out subsets of the ones
it did.

**Unix can make `xargs` a program precisely because its `xargs` holds nothing.** A name in a pipe is
text there, and `rm`'s authority comes from its uid, so the batcher in the middle is not a party to
the delegation at all. Here a name in a pipe is still text, which means the analogue of
`find . | xargs rm` would spawn an `rm` holding **no authority whatsoever**: the pipe cannot carry
the thing that has to be batched. What can mint a per-batch caretaker is whatever holds the directory
capability with the right to delegate it, so the batching belongs where the authority already is.

It is not the table's third row either (*make `rm` a builtin*, "dodges the question"). `rm` stays a
program taking an attenuated grant. What became a builtin is the **iteration**, which is a property
of how the shell delegates and not of anything any command does. That is exactly what `caps` and
`time` are: prefix words about how a line is run.

**And it is opt-in rather than automatic.** A shell that silently swept a large match would make
`caps rm *.txt`'s single printed grant a lie, and would turn one command into N spawns with a
possible partial effect without the user asking for either.

### The resume point is a name, not a cursor

This is the decision the rest of batching hangs on, and the one Unix never has to make.

A sweep cannot expand the whole match and then chunk it, because holding the whole match is the thing
that is impossible. So batch *k+1* has to re-enumerate the directory, and it needs to know where to
carry on from. A **cursor** into the listing is invalidated by exactly what a batched command is
usually doing: `rm` takes eight names away and every later entry shifts. This note already records
that hazard from the other end, where `rm`'s namespace sweep re-reads from cursor 0 because a set
namespace is fixed and a real directory is not.

So a batch is **the eight smallest matches strictly greater than the previous batch's last name**, in
byte order. A name is stable under removal and under insertion both, so the rule terminates whether
the command destroys what it was handed or leaves it alone, and the watermark is one 16-byte name
rather than a structure. The host tests sweep a static directory and a shrinking one and assert the
same thing of both: every name exactly once.

The order is a **total order chosen for resumability**, not a display preference, which is why an
unbatched expansion still comes out in listing order. Sorting that one too would be a gratuitous
change to what `echo *.txt` prints.

### One command with a partial effect, and where it stops

A batched line is the one thing no unbatched line here can be: **one command that can half-happen.**
Batch one's `rm` has taken its names before batch two starts, and there is no rollback for a removal.

So the sweep **stops at the first batch that does not run**, and says where the boundary fell:

```
  batch 3 did not run: 16 names were handed over in 2 batches, and nothing after them was attempted
```

A sweep whose *first* batch did not run says nothing at all, because the refusal printed above it is
the whole story and "0 names in 0 batches" is a sentence about an event that did not happen. Neither
does a sweep of exactly one batch: that is the line the user would have typed without `xargs`, and a
footer under it would claim an event.

Unix carries on past a failed invocation and reports 123 at the end. That is its mechanism talking:
its `xargs` cannot know what a child did to the names it was handed, so trying the rest is as good as
anything. Here each batch's set is printed **before** that batch runs, so a stop can name the
boundary, and what happened is a *prefix* of the match: a thing a person can hold in their head and
resume from. Carrying on would leave an arbitrary subset that only the transcript could reconstruct.
Batch four succeeding after batch three failed is the outcome the design is against.

**The child sees none of this.** Each batch is a complete invocation with a complete grant, and
nothing tells it that it is batch three of seven. Unix agrees by accident, because each `exec` is a
fresh process; here it follows from the model, since the grant *is* the argument and a batch's grant
is whole.

### Where it lives, and why that is one place rather than a command path

The sweep is a policy on `Nav::expand`, the shell's single expander, and not a second way to run a
line. Two things fall out. **Every command path inherits batching for free**: `echo`, an invocation,
a `caps` preview and a pipeline stage all reach a pattern through that function, so none of them
knows a sweep is running. And **a batched line cannot disagree with an unbatched one about what a
pattern designates**, because membership is decided in `Expander::offer` for both; only the
collection policy differs (`grant_plan::expand::Expander::batch` takes the eight smallest and reports
that more remain, where `Expander::new` refuses the ninth).

The refusals survive batching, deliberately. A matched name that cannot travel in a grant still stops
everything, because a batch missing a name the pattern matched is the silent prefix this whole
mechanism exists to refuse. Batching splits an authority; it never shrinks one.

## What the tests prove, and from where

**Host, in milliseconds** (`cargo test -p grant_plan -p fs_proto`): the expander's set is what matched and
not what did not; an empty match and an oversized one are refusals; a matched name too long to grant
refuses the whole expansion rather than dropping one name; the dot rule; a pattern only in the last
component; the planner grants the set the expander produced, unnarrowed and unreordered; a literal
operand ignores any expansion offered with it; the set encoding round-trips and refuses rather than
truncating; and the fixture is matched by the pattern it is staged for, which is what makes the
kernel test's literal set provably the expansion.

**The batching, on the host too** (`cargo test -p grant_plan -p swish -p fs_proto`, milestone 109):
the batches partition the match with no name taken twice or dropped; a batch is the smallest names in
order rather than the first the directory yielded; the watermark is exclusive; `more` says whether a
batch follows; **a sweep of a directory that shrinks under it takes every name exactly once**, which
is the case a cursor would get wrong; an empty batch ends a sweep but an empty *first* batch is still
`NoMatch`; the unnameable-match refusal survives batching; the sweep's account and its three wordings;
and the `globmany` fixture is over the bound and splits exactly where the shell-check gate's two
literal strings say it does.

**At a real prompt, both ISAs** (`script/shell-check`): unbatched, the pattern is refused; batched,
its second batch is `m-08.txt m-09.txt m-10.txt`, which pins the resume rule from one line (an
off-by-one, a batch that restarted, and a batch that took the first eight the directory yielded all
print something else); and `xargs caps rm` shows the second batch's grant naming those three names
and nothing more.

**In the guest, both ISAs** (`glob_grant_tests`, one `#[test_case]` because the two phases are one
argument and their order is load-bearing):

1. A real shell in a real `fs_subtree_caretaker` expands one pattern two ways and reports agreement,
   plus the three refusals that stop the agreement being vacuous.
2. **`rm` is the attacker.** Told to remove `gl-three.log` through the set capability: the file
   exists, sits one directory entry away from the two names in the set, and the caretaker one hop up
   holds a capability that could remove it. `ENOENT`. Nothing in `rm` decided not to try.
3. And the grant works: `rm` in namespace mode removes exactly the two names, which is what stops
   claim 2 being equally true of a capability that reaches nothing.

**From outside the guest entirely** (`xtask::redoxfs_glob_grant_took_exactly_the_match`): a different
process, on the host, with the pinned engine, reading the image the run left behind. The two matched
names are gone, the two unmatched ones are still there, and the unmatched directory still holds its
file. What the guest displayed is what disappeared.

## BUGS

Known limitations, next to the feature rather than only in a tracker.

- **The bound is eight names, and an unbatched line over it is still refused.** A directory with nine
  matching files cannot be handed to one invocation; the answer is a refusal, not a partial grant.
  `xargs` sweeps it instead (milestone 109, above), and that is opt-in on purpose. Lifting the number
  means giving the shell an allocator or the grant a different carrier, not editing the constant.
- **A sweep cannot run anything that takes a directory grant yet**, which is the same missing
  delegation chain as the entry below: `xargs rm *.txt` plans and prints each batch's grant and then
  stops at the first one, because the shell cannot build the caretaker. What is proven at a real
  prompt today is the half that costs no authority (`xargs echo`) and the half that previews it
  (`xargs caps rm`). The loop above it is the same loop either way.
- **The sweep's stop rule can only see failures the shell prints.** A refusal, a spawn failure and a
  filesystem error stop it; a child that ran and did the wrong thing does not, because there is no
  exit status on this path for any program but `worker` and `budgeter` (`swish::write_outcome`). So
  "batch three did not run" is honest and "every batch before it did what it was asked" is not a
  claim this makes. Exit statuses would fix it and are their own piece of work.
- **A sweep re-enumerates the directory once per batch**, so it costs batches × the directory rather
  than one listing. That is the price of never holding the whole match, and it is the same trade
  `rm`'s recursive walk already makes.
- **A name created mid-sweep above the watermark joins it; one created below does not.** The same
  class of caveat as any concurrent directory walk, and it is a consequence of resuming by name
  rather than a bug in doing so.
- **Only the first pattern of a batched line is batched**, which is the same limitation as the entry
  below (only the first pattern on a line is expanded at all) and not a second one.
- **A literal operand's type bit is `false` because nothing enumerated it.** Only a set the shell
  expanded carries the types it observed. Today no wiring serves a single-name grant through
  `fs_nameset_caretaker`, so nothing reads that bit; if one ever does, its listing would call a
  directory a file.
- **A set capability cannot move a name out of its set**, so `mv *.txt elsewhere/` is not expressible.
  Argued above under `RENAME`.
- **Only the first pattern on a line is expanded.** No manifest declares two name slots, so a second
  operand of any kind is already an unplaceable token and a refusal; the day one does, this needs a
  second expansion rather than a loop over the same one.
- **No `**`, no qualifiers**, permanently and by decision respectively. [glob.md](glob.md) carries
  both arguments: `**` is a traversal feature (descending means holding a capability), and a
  qualifier needs type, mtime and size per candidate, which turns one enumeration into N `FSTAT`s and
  needs a read right beyond enumerate. Neither is a scheduling excuse; both are authority questions.
- **The interactive prompt still holds no directory**, so at a real keyboard `echo *.txt` says "this
  shell holds no directory capability; there is nothing here to name" and `rm *.txt` says "you hold
  no such capability". Both sentences are **true rather than placeholders** (the interactive boot
  held no filesystem when this was written, §27's amendment), and they agree with each other, which is the property this
  lane cares about. What is missing is a boot that wires an FS service into the interactive system.
- **The shell cannot build the caretaker either.** `spawn` refuses a directory grant with "this
  shell cannot yet", so the set grants that exist are wired by the kernel test. The mechanism is
  proven on both ISAs; the delegation chain is the same one notes/grant-expression.md assesses for
  the clock.
- **The set is not consulted below the granted directory, including for attributes.** A handle
  minted by descending into a matched directory is unfiltered, which is argued above for the naming
  verbs and is the same answer for the four attribute verbs milestone 61 forwarded: what is under a
  directory the pattern matched was never a question the pattern asked.

## EXAMPLES

At a prompt with a directory capability, the pairing that is the whole point:

```sh
$ echo gl-*.txt
  gl-one.txt gl-two.txt
$ caps rm gl-*.txt
  rm would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    cap 2  endpoint  dir      /  (the directory holding gl-one.txt gl-two.txt)
           ...and nothing under it: no -r, so it cannot even look
  reading the command is reading its whole authority.
$ echo gl-*.rs
  no name here matches that pattern, so there is nothing to grant
```

Expand once and grant what was shown, from the shell's side:

```rust
// user/src/swish.rs: one expander, two callers
let shown = nav.expand(b"gl-*.txt")?;              // what `echo` prints
let e = grant_plan::plan(&spec, holdings(&nav), Expansion::at(0, shown))?;
assert_eq!(e.dir.unwrap().names, shown);           // and what `rm` would hold
```

Wire a set grant and attack it:

```rust
// kernel/src/user/glob_grant_tests.rs
let report = fs_service::start_granted_set(
    blk_server_image(),
    program("fs_server").unwrap(),
    program("fs_nameset_caretaker").unwrap(),
    program("rm").unwrap(),
    fs_service::SetGrant {
        dir: tree::GLOBSET,
        names: &[(b"gl-one.txt", false), (b"gl-two.txt", false)],
        rights: dir::REMOVE,                       // take names out, and nothing else
        role: fs_proto::grant::spec(0, 0),         // no name: the operand is the namespace
        arg: 0,
        arg2: 0,
        stack_pages: 4,
    },
)?;
```

Run it:

```sh
script/test                  # both ISAs, plus the post-run host check on the image
script/shell-check           # both ISAs: the sweep, at a real prompt, through the real init
cargo test -p grant_plan          # the expander, the bound, the empty match, and the batches
cargo test -p swish          # the sweep's account and its wording
cargo test -p fs_proto       # the set encoding, and both fixtures pinned against the matcher
```

## See also

- [glob.md](glob.md): the matcher, and the four scope decisions this lane inherits.
- [dir-capability.md](dir-capability.md): the rights ladder, `fs_subtree_caretaker`, and why the
  endpoint is the boundary.
- [rm.md](rm.md): `rm` as a program, and why `-r` widens the grant.
- [grant-expression.md](grant-expression.md): the command line as a grant expression, and
  `fs_file_caretaker`.
- Milestone 47's "Globbing, which decides how every multi-file operation grants" in
  `design/roadmap/47-navigation-and-naming.md`.
