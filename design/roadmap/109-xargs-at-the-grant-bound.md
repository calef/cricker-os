# 109. `xargs`: batching a grant too large to hand over

**Status: BUILT** 2026-08-04 (PR #111). Built as a shell prefix word rather than a program, deliberately: a batching program would have to hold the union of every batch, which is the thing that cannot be handed over. **One limit, named here because a reader will meet it**: `xargs <program>` still stops after planning batch one, because the shell cannot yet ask init to mint a per-batch caretaker. `xargs echo` and `xargs caps rm` run end to end. The missing delegation chain is milestone 47's, not this one's. Raised 2026-08-04. Milestone 47 (navigation and naming) names this twice in
its own block and will close without it, and both glob notes end on the same sentence.

**The finding, with the number.** A glob expansion grants at most **eight names**.
`grant_plan::expand::MAX_NAMES` is 8 and `fs_proto::nameset::MAX_NAMES` matches it, pinned by a host
test. A directory with nine matching files cannot be globbed at all: the answer is
`Refusal::TooManyNames` at the prompt, with nothing spawned. `notes/glob-grant.md`'s BUGS section
states it first, `notes/glob.md` repeats it, and milestone 47's own block says "`xargs` is still not
built: the answer at the bound is a refusal."

**Eight is measured, not chosen, and that matters for what the fix can be.** Sixteen was the number
the argument produced and the machine refused it: a name set travels **by value** through the
expander, the `Expansion`, `designate`'s return and the `Endowment`, four stack frames a debug build
does not collapse, and the shell ran off the bottom of its stack planning a single grant. Twice.
Eight names of sixteen bytes is 152 bytes a copy. So `notes/glob-grant.md` is right that "lifting the
number means giving the shell an allocator or the grant a different carrier, not editing the
constant", and raising the bound is not this milestone's answer.

**Why `xargs` here is a better idea than `xargs` in Unix**, in milestone 47's words, which are worth
quoting rather than paraphrasing:

> **`ARG_MAX` becomes a capability limit rather than a buffer limit.** Unix's "argument list too
> long" is why `xargs` exists; here the ceiling is that you cannot hand a child a hundred thousand
> capabilities. The same failure with a more honest cause, and it wants the same answer (batching),
> so `xargs` earns its place for a better reason than Unix had.

Unix's bound is an implementation artifact of a fixed `exec` buffer. Ours is a statement about
authority: a batch is a set of capabilities somebody has to hold, show and account for, and
`caps rm *.txt` prints the set before anything runs precisely so a grant nobody can read is never
made. Batching is then not a workaround for a limit; it is the shape a bounded delegation takes.

**What it costs, and the property that must survive.** The refusal is currently loud, immediate and
total, and `notes/glob-grant.md` says why that shape was chosen: "A glob that quietly granted a
prefix of what it matched would be the worst thing this lane could produce, because the printed
set and the granted set would disagree." An `xargs` that runs a command eight names at a time must
keep that property per batch, so the user sees each batch's set, and must not blur into a partial
grant that looks like a whole one. The failure mode to design against is a batch four succeeding
after batch three failed.

## Scope note

**A program, not a bound change.** The eight stands. If the carrier ever changes (an allocator in the
shell, or a set that travels by reference), that is its own decision with its own argument, and
`xargs` is still wanted afterwards because the ceiling moves rather than disappearing.

**Only the first pattern on a line is expanded today**, which `notes/glob-grant.md` records, and it
interacts: an `xargs` whose input is a second operand meets a shell that has no second name slot.
Check that interaction before designing the command line, because it may decide whether `xargs`
reads a set or is handed one.

**The name is provisional and calef's call**, like every program name in this tree. `xargs` is what
milestone 47 and both notes call it, and it is a standard term a reader already knows from outside,
which is the strongest argument any name gets here.
