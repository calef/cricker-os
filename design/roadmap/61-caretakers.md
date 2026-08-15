# 61. The caretakers: one verb table, and names that say what you get

**Status: BUILT, both ISAs.** Three pieces, three commits, in the order below.

**In brief.** The **rename** first, because these files were being touched anyway: `fwarden` ->
`fs_file_caretaker`, `dwarden` -> `fs_subtree_caretaker`, `swarden` -> `fs_nameset_caretaker`,
`cwarden` -> `c_confiner`, `cshim` -> `c_shim`, `conx`/`cconx` -> `rust_swappable`/`c_swappable`,
`await_*` -> `wait_for_*`, and the C symbols to `c_seam_*`. That is 532 tokens rather than four
filenames, and `c_confiner` deliberately did not take the caretaker noun in its prose. Then
**`fs_proto::verb`**: one row per opcode saying what a request's words mean and which rights the
server demands, with `const assert!`s that make a verb without a row a **compile error**; the three
caretakers dispatch off it and stop being three hand-written matches. Then **extended-attribute
forwarding**, the gap that raised the milestone, with three witnesses: a per-file grant that reads
its file's attributes and cannot write them (and a writable twin that can, as its control), the
three subtree rights configurations one bit wider, and a name-set grant that reads its file's
attributes and still cannot name the entry beside it. Notes: fs-server.md, dir-capability.md,
glob-grant.md, xattr.md, grant-expression.md, naming.md.

**What the table does not share is the attenuation**, which is what let the three programs stay three
programs after the refutation below. A lookup that picks a length or a zero cannot refuse anything,
so `fs_subtree_caretaker` still performs no checks at all.

**One thing found rather than built, recorded in `verb::file_grant::POLICY`'s BUGS and in
notes/grant-expression.md:** writing the per-verb rows down exposed that `fs_file_caretaker` answers
`EBADF` to every directory verb except `CREATE`, because they all fell through one `_ =>` arm shared
with "you named a handle I never minted". `ENOTDIR` is very likely right for all seven by exactly the
argument `CREATE` already makes. Behaviour was preserved, because changing it changes the wire.

**Renamed and rescoped 2026-08-01** after calef asked why there are three of these and whether we
expect more. Investigating that **refuted the collapse this milestone was first drafted around**, and
the refutation is worth keeping because it is already argued in the tree.

## The collapse that does not work

The three serve near-identical verb surfaces (`subtree` and `nameset` are identical at 18 verbs), so
"one program parameterized by how the namespace is described" looks obviously right. It is wrong, and
`swarden.rs`'s own header carries a section titled *"Why this is a third warden and not a mode on the
second"* saying why:

**`dwarden` performs no checks at all, and that is its design.** One `OPENDIR` at startup, with the
server intersecting the granted rights and minting a restricted handle; everything after is reached
through that handle, so the attenuation lives in what the server minted rather than in any branch. A
name filter is a check, consulted on **every** name-taking verb. Adding a mode would trade that
program's one strong property for a switch, and put a forget-a-verb surface in the program that most
deliberately has none.

So the two serve the same verbs **by opposite means**. They stay separate.

`fwarden` is different again: it translates between two *protocols*, directory in and file out, which
is why it must inspect. Its narrow 11-verb surface is deliberate, and the tell is the errno.
`CREATE` answers `ENOTDIR`, not `EACCES`, because a file capability is not a directory: the request
does not mean anything, rather than meaning something that was refused. **The verb surface is part of
what the capability is**, not a filter over a wider one.

## What the milestone is

1. **A verb table in `fs_proto`**, so a verb is taught once rather than three times. This survives
   the refutation: the duplication is real even though the programs must stay distinct. Today a new
   verb is simply absent from a caretaker and the capability silently is not there, which is exactly
   how the xattr gap happened.
2. **Extended-attribute forwarding**, the gap that raised this. All three answer `EOPNOTSUPP`, so a
   program behind a per-file grant cannot read its own file's attributes.
3. **The rename**, because these files are being touched anyway and doing it twice is worse.

## The rename

`warden` is a synonym we invented for a pattern that has a name. `DECISIONS` §31 already cites
the right one, **caretaker** (Mark Miller's term), while the code says warden; §50 settled that
using the existing name claims "this is that", and inventing a synonym asserts novelty where there
is none.

Names say **what the holder ends up able to do**, so a reader can predict the surface without opening
the file:

| Current | Proposed | A reader should predict |
|---|---|---|
| `fwarden` | `fs_file_caretaker` | a file; cannot list or create |
| `dwarden` | `fs_subtree_caretaker` | a directory and everything under it |
| `swarden` | `fs_nameset_caretaker` | exactly these names, in one directory |
| `cwarden` | `c_confiner` | **not a caretaker**: holds a region and confines foreign code |

`dwarden` is the one that buys correctness rather than clarity: it is named for what it **holds**,
while both siblings are named for what they **serve**, and since all three hold a directory the
current name distinguishes nothing.

## Settled 2026-08-01

- **The family noun is `caretaker`.** Settled when calef chose `wait_for_caretaker` over
  `await_warden`: a helper cannot be named for a pattern its callees are not. §50's rule is the
  reason (use the name the literature already has; a synonym asserts novelty where there is none),
  and `DECISIONS` §31 has cited Miller's term correctly since milestone 31 while the code said
  warden.
- **The `await_*` helpers become `wait_for_*`.** `await` reads as async/await, which this project
  rejected at a design fork, and there is no async here. Four of them travel together
  (`wait_for_service`, `wait_for_caretaker`, `wait_for_compositor`, `wait_for_ready`) plus the
  `warden_ready` parameter, because three renamed and one not is worse than either consistent state.
- **`wait_for_caretaker`, not `wait_for_caretaker_ready`.** It waits for the caretaker to be
  *serving* rather than to exist, and the shorter name does not say so; the doc comment carries that
  precision. Taken because the whole family shares the ambiguity and resolves it the same way, and
  parallelism with `wait_for_service` is worth more than the extra word.
- **`cwarden` becomes `c_confiner`**, out of the caretaker family entirely: it holds a **region**
  and confines foreign code rather than attenuating a directory capability to a narrower one.

## The names, settled 2026-08-01

| Current | Settled | A reader should predict |
|---|---|---|
| `fwarden` | **`fs_file_caretaker`** | a file; cannot list or create |
| `dwarden` | **`fs_subtree_caretaker`** | a directory and everything beneath it |
| `swarden` | **`fs_nameset_caretaker`** | exactly these names, in one directory |
| `cwarden` | **`c_confiner`** | not a caretaker: holds a region, confines foreign code |

**The `fs_` prefix is the resolution of a real objection rather than decoration.** calef raised that
"subtree" means three things around here: `supervision_proto` *is* the supervision tree, `CLAUDE.md`
uses "the tree" throughout to mean this repository, and git has its own `subtree`. The first answer
was to put the disambiguation in the doc comment. Carrying it in the name is strictly better, and
`fs_subtree` cannot be misread as either of the others.

`fs_` and not `file_`, because `file` is already one of the qualifiers and `file_file_caretaker` is
the reductio. It is also **not a new convention**: `fs_proto`, `fs_server` and `fs_service` already
use `fs` as this project's filesystem marker, so this applies an existing one where it was missing.

An earlier draft of this block settled on bare `file_` / `subtree_` / `nameset_`, on the objection
that a domain prefix breaks parallelism. calef's answer removed the objection rather than ignoring
it: apply the prefix to **all three**. That also leaves the four programs on one scheme (domain,
then what it serves, then what it is) instead of two unrelated ones, and it groups them in `ls`,
which matters in a `user/src/` holding 48 programs and no subdirectories.

**Why these qualifiers.** `file_` rather than `one_file_`, because cardinality is not the
interesting property: you cannot enumerate at all, so "one versus few" never arises. `nameset_`
rather than `glob_`, because §52 records that a BFS-style query result and a glob result are the
**same object** granted by the same attenuation, so the name is about a designated set of names and
globbing is merely its only caller today. `subtree_` rather than `directory_`, because all three
**hold** a directory capability, so naming one of them for what it holds distinguishes nothing, which
is the exact defect `dwarden` has and this rename exists to fix.

**Two costs, recorded rather than discovered later.** `fs_subtree_caretaker` and
`fs_nameset_caretaker` are 20 bytes against `nifefs`'s archive limit (`NAME_LEN`), which was 24
when this was written, so four bytes of headroom and a four-part name would not fit; that constraint
was load-bearing and is what led to raising the limit to 32 on 2026-08-01 (notes/nifefs.md).
And `fs_file_caretaker` says filesystem twice, which is the price of the scheme being uniform.

The rename also resolves an inconsistency already in the source: `dwarden.rs`'s header says
"attenuated to one **subtree**" while its second paragraph says "narrows it to one **directory**".

## The C-seam family converts in the same pass (calef, 2026-08-01)

| Current | Settled |
|---|---|
| `cwarden` | `c_confiner` |
| `cshim` | `c_shim` |
| `crates/c_seam` | already done, 2026-08-01 (rule 7) |
| `user/c/c_seam.c` | **`user/c/c_seam.c`**, and this one is a repair |

**That last row fixes a split the integrator created.** `c_seam` was chosen over `c_abi` partly
*because* it keeps the Rust and C halves paired, and then only the Rust half was renamed when rule 7
turned it into a crate. So the pairing argument is currently false in the tree: `crates/c_seam`
faces `user/c/c_seam.c`.

The pairing is not cosmetic. Both files state the same constants **by hand**, because a C compiler
cannot see Rust, and `crates/c_seam`'s test reads the C source with `include_str!` to prove they
agree. Two names that no longer match make the duplication look accidental rather than mechanical,
which is exactly what that test exists to contradict. **Renaming the C file means updating the
`include_str!` path**, and the test failing is how a mistake there would announce itself.

`user/build.rs`'s `C_SOURCES` table names both the source and the program it compiles into
(`("c/c_seam.c", "cshim")`), so it changes on both counts in one edit.

## The live-replacement pair, settled 2026-08-01

| Current | Settled |
|---|---|
| `conx` | **`rust_swappable`** |
| `cconx` | **`c_swappable`** |
| `user/c/conxsvc.c` | **`user/c/c_swappable.c`** |

`conx` was the most opaque name in the tree: **no recorded expansion anywhere**, not §41, not
`notes/live-replacement.md`, not the commit that introduced it. These are milestone 23's swappable
component in two implementations, and a client that does not notice the swap.

**`rust_` breaks a precedent deliberately, and the exception is the point.** When `c_seam` was
settled, the argument was that Rust is the constant and the foreign language is the variable, so
naming only the variable is economical: it is why there is no `rust_kernel` or `rust_shell`. That
holds where the language is **incidental**. Here it is the **subject**: this pair exists so that a
Rust component can be replaced by a C one while `chatty` keeps calling, and the language is the whole
reason there are two of them.

Symmetry does real work too. `swappable` plus `c_swappable` would read as *the* swappable one and a
C variant, implying a default. That is actively wrong: `conx` is the incumbent and `cconx` the
replacement **only until the swap**, after which the roles invert. Neither is the default, and a
symmetric pair says so.

**One cost, recorded so nobody infers a family that is not there.** `c_` will then mean "written in
C" across two unrelated milestones: `c_shim`, `c_seam` and `c_confiner` are milestone 36's
foreign-language seam (§31), while `c_swappable` is milestone 23's replacement demo. The prefix
means the same thing in both cases; the milestones are not related. Worth a line in
`notes/naming.md`.

## BUGS

- **A table is a new place to be wrong, and a wrong row is wrong in three programs at once.** It is
  pure data in a host-testable crate, so Kani and host tests can reach it, which a hand-written match
  in a `no_std` binary cannot.
- **It does not make the caretakers interchangeable**, and after the refutation above it must not
  try to. Only the verb dispatch is shared; what each attenuates to stays hand-written.

**Effort: medium.** The table is small; teaching three programs and proving each on both ISAs is the
work, and the rename touches roughly 180 references.

## The draft this replaced

The original framing, kept because the refutation above is only legible next to it. calef asked on 2026-08-01 whether xattr support in the wardens deserved a
milestone. It does, but the useful milestone is the general one, and the xattr gap is its proof.

## The immediate gap

`fwarden`, `dwarden` and `swarden` answer `EOPNOTSUPP` to all four extended-attribute verbs
(milestone 57). A program behind a per-file grant cannot read its own file's attributes. That is
uniform and §42-honest, and it is still a capability the confined program should have.

## The general problem, which is why this is a milestone and not a chore

**Each warden is a hand-written `match` over the verb.** Milestone 57 added four verbs, so closing
this means twelve new match arms across three programs, and **the next contract addition will cost
the same again.** The contract is around twenty verbs now. Nothing makes a warden and the contract
agree, so the way this fails is that a new verb is simply absent from a warden and the capability
silently is not there. That is exactly how the xattr gap happened: the verbs landed, the wardens were
not taught, and nothing failed.

## Why "just forward everything" is the wrong fix

Worth stating plainly, because it is the obvious idea and it is a security hole. **The enumeration is
doing real work.** `fwarden` substitutes its own handle for the caller's, refuses anything that is not
`grant::HANDLE`, and enforces direction so a read grant cannot write. A blind proxy would forward the
caller's handle and hand back the wide capability the warden exists to attenuate.

## The shape

**A verb table in `fs_proto`**: each verb declares its argument shape (does it name a handle, a name,
a length?) and the right it requires. The warden's loop becomes generic over the table, and adding a
verb becomes **one row in the contract** rather than three match arms in three programs.

This inverts the failure mode, which is the actual deliverable. Today, forgetting a warden yields a
capability that is quietly missing. With a table, a verb with no row is a build failure, and a verb a
warden should refuse is an explicit row saying so, which is a decision somebody wrote down.

## This is not the speculative abstraction CLAUDE.md warns against

The rule is not to build an abstraction before the requirements are known. We now have three wardens,
about twenty verbs, and a fresh instance where a whole contract addition reached none of them. That is
the second data point, not a guess about the future.

## Scope

The table, the three filesystem wardens, and xattr forwarding as the thing that proves it. Each
warden needs its own answer for the write verbs: a read-only grant must not forward `SETXATTR`, and
that is per-warden policy rather than something the table decides.

**`cwarden` stays out.** It confines a C component and is not a filesystem proxy; it shares the name
and not the mechanism.

## BUGS

- **A table is a new place to be wrong, and a wrong row is wrong in three programs at once.** The
  mitigation is that it is pure data in a host-testable crate, so it is reachable by both host tests
  and Kani, which a hand-written match in a `no_std` binary is not.
- **It does not make the wardens interchangeable.** They differ in what they attenuate to, and that
  stays hand-written; only the verb dispatch is shared.

**Effort: medium.** The table is small; teaching three wardens and proving each on both ISAs is the
work.
