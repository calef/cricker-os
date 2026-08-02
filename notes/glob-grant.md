# Globbing, and the property that the expansion you see is the grant

Milestone 47's globbing lane. Built 2026-07-31. [glob.md](glob.md) is the matcher, a total function
on two byte strings with no filesystem in it; this note is the layer that turns a match into an
**authority**, and the demonstration that hangs on it.

The code is `crates/grant_plan/src/expand.rs` (the expander and the name set, host-tested),
`crates/fs_proto`'s `nameset` module (the wire encoding), `user/src/fs_nameset_caretaker.rs` (the
caretaker), `user/src/swish.rs` (`echo`, and the grant path), `user/src/rm.rs` (the namespace mode),
and `kernel/src/user.rs`'s `fs_service::start_granted_set` and `glob_grant_tests`.

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

## What the tests prove, and from where

**Host, in milliseconds** (`cargo test -p grant_plan -p fs_proto`): the expander's set is what matched and
not what did not; an empty match and an oversized one are refusals; a matched name too long to grant
refuses the whole expansion rather than dropping one name; the dot rule; a pattern only in the last
component; the planner grants the set the expander produced, unnarrowed and unreordered; a literal
operand ignores any expansion offered with it; the set encoding round-trips and refuses rather than
truncating; and the fixture is matched by the pattern it is staged for, which is what makes the
kernel test's literal set provably the expansion.

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

- **The bound is eight names.** A directory with nine matching files cannot be globbed at all; the
  answer is a refusal, not a partial grant. `xargs` is the roadmap's eventual answer and it earns its
  place for a better reason than Unix had, but it is not built. Lifting the number means giving the
  shell an allocator or the grant a different carrier, not editing the constant.
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
  wires no FS service, §27's amendment), and they agree with each other, which is the property this
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
// kernel/src/user.rs, glob_grant_tests
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
cargo test -p grant_plan          # the expander, the bound, and the empty match
cargo test -p fs_proto       # the set encoding, and the fixture pinned against the matcher
```

## See also

- [glob.md](glob.md): the matcher, and the four scope decisions this lane inherits.
- [dir-capability.md](dir-capability.md): the rights ladder, `fs_subtree_caretaker`, and why the
  endpoint is the boundary.
- [rm.md](rm.md): `rm` as a program, and why `-r` widens the grant.
- [grant-expression.md](grant-expression.md): the command line as a grant expression, and
  `fs_file_caretaker`.
- Milestone 47's "Globbing, which decides how every multi-file operation grants" in
  `design/roadmap.md`.
