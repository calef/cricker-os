# Counted claims: a number in the prose is a claim, and a gate can keep it

*The name `counted-claims.md`, and the phrase "counted claim" it introduces, are **provisional**.
A lane ships a provisional name and says so; naming is calef's (AGENTS.md).*

DECISIONS §39 says a name is a claim, made before a reader sees a line of code. So is a number, and
a number is the kind of claim a machine can check. That difference is the whole of this convention:
names need a person, counts do not.

## The evidence

Three claimed counts were tested against the tree on 2026-08-14. **All three were wrong**, and two
of them disagreed with each other as well as with the tree.

| the claim | where it lived | the tree, that day |
|---|---|---|
| "the 23 `#!/bin/sh` scripts" | `script/lint`, `.github/workflows/ci.yml` | 36 |
| "107 harnesses across 19 crates" | `notes/verification.md` | 110 across 19 |
| "112 Kani proof harnesses" | `CLAUDE.md` | 110 |

Every one of them had been right when it was written. That is the finding rather than a detail:
**each number is a snapshot of whenever somebody last counted, and nobody re-derives one.** The `23`
had drifted by thirteen and sat in two files, neither of which knew about the other.

Two days later, when this convention was built, the harness count had moved again, to 119 across 21.
It moves whenever anyone lands a proof, which is what makes a hand-maintained number hopeless and a
derived one free.

## The convention

Put an HTML comment after the number, naming the count it vouches for:

```markdown
**119 harnesses** <!--count:kani-harnesses--> across 21 crates <!--count:harness-crates-->
```

`script/lint` finds every marker, re-derives that count from the tree, and fails on a mismatch,
naming both values and the file and line. The marker is plain HTML-comment syntax, so it is invisible
in rendered markdown and legal as ordinary text inside a YAML or shell `#` comment, which is why the
same spelling works in `.github/workflows/ci.yml` as in a note.

**The number a marker vouches for is the last integer on the line before it**, commas allowed, so
`1,303 commits <!--count:...-->` works. Last rather than first, so that a line carrying two counts
binds each marker to the number just in front of it. `notes/unsafe-obligations.md` is the live case
and it writes crates before harnesses, the opposite order to everywhere else, which is exactly why
the rule is positional rather than clever.

**A marker inside a fenced block or a backtick span is ignored.** The block above is an example of
the convention, not an assertion about the tree, and so is the one in the roadmap; a marker written
as inline code is being named rather than used. Fenced blocks only, though: an indented code block
is not recognised (see BUGS).

## The registry

The other half lives in `script/lint`, one entry per name. Each entry carries **the question it
answers, in the prose's own words**, and a derivation.

| name | the question it answers | derived from |
|---|---|---|
| `kani-harnesses` | how many Kani proof harnesses the tree carries, which is what `script/verify` proves | `#[kani::proof…]` alone on its line, in `crates/**/*.rs` |
| `harness-crates` | how many crates carry at least one Kani proof harness | distinct crate directories among those files |
| `sh-scripts` | how many `#!/bin/sh` scripts there are under `script/` and `scripts/`, which is the set shellcheck gates | files whose first line is exactly `#!/bin/sh` |
| `longest-markdown-line` | how long the repository's longest markdown line is, in bytes, which is what `manual::render::LINE_MAX` is sized against | tracked `*.md`, vendor excluded |
| `syscalls` | how many syscall numbers the ABI defines, which is the whole width of the trap | `pub const SYS_*: u64` in `crates/abi/src/lib.rs` |
| `rights-bits` | how many named single-bit rights a capability can carry | `pub const NAME: Rights = Rights(1 << N)` in `crates/capability` |
| `loom-harnesses` | how many loom harnesses the tree carries, which is what `script/interleaving-check` runs | `loom::model(` calls in `crates/**/*.rs` |
| `loom-crates` | how many crates carry at least one loom harness | distinct crate directories among those files |

The last four arrived with the 2026-08-17 documentation sweep, whose lens was the ABI. `syscalls`
exists because the ABI crate's own front page, the kernel's syscall module, an `Error` variant's
description and two notes all said the surface was **three** calls; `SYS_CAP_DELETE` had landed
three weeks earlier and moved none of them. `rights-bits` exists because `ENUMERATE` made the rights
four and `notes/capability-lifecycle.md` still said three the next day. The `loom-*` pair is the one
this note asked for by name: it cited `notes/interleaving.md` as the claim a marker could never
reach, and measuring it in order to mark it found it wrong in both halves.

**The `longest-markdown-line` entry is the one with a consumer rather than a reader**, and it is
worth understanding before you add another. `manual::render::LINE_MAX` is 2048 because the longest markdown line is 1841 <!--count:longest-markdown-line-->,
and a document over `LINE_MAX` is truncated. So that number is a **margin**, and a lane that spends
it silently makes the renderer wrong about a file nobody has written yet. Every other entry here
describes the tree; this one guards it. If you can find another number in that class, it is worth
more than any number of descriptive ones.

**The question is not decoration, it is the entry's most important field.** "How many `#!/bin/sh`
scripts" has at least three defensible answers: `ls script/` gives 37, the shellcheck glob
`script/* scripts/*.sh` gives 40, and "files that literally begin `#!/bin/sh`" gives 40 today and
could give fewer tomorrow. A gate that answers a subtly different question than a human would is
worse than no gate, because it fails a document that is right, and the way it gets fixed is by
somebody deleting the marker.

## EXAMPLES

### Adding a new marked count

Take the tree's harness count as the worked example, from nothing to gated.

**1. Decide the question, and write it as a sentence.** Not "harnesses" but "how many Kani proof
harnesses the tree carries, which is what `script/verify` proves". If you cannot write the sentence
without an "or", you have two counts.

**2. Write the derivation and check it against the real shapes.** Not against what you expect the
shapes to be:

```sh
$ grep -rho '#\[kani::proof[a-z_]*' --include='*.rs' . | sort | uniq -c
 123 #[kani::proof
$ grep -rc '#\[kani::proof' --include='*.rs' crates | awk -F: '{s+=$2} END {print s}'
119
```

The gap between 123 and 119 is the whole reason for this step: two of the four are in the vendored
RedoxFS, which this suite has never proved, and two are inside doc comments in
`scripts/kani-lint-shim/` that describe the attribute rather than use it. A derivation that counted
them would be confidently wrong, in the direction nobody checks.

**3. Add the registry entry** to the `==> counted claims` block in `script/lint`, with a docstring
saying what shape it assumes:

```python
REGISTRY = {
    'kani-harnesses': (
        'how many Kani proof harnesses the tree carries, which is what script/verify proves',
        lambda: sum(_harness_hits().values()),
    ),
}
```

**4. Mark the sites.** Put the marker after the number, on the same line:

```markdown
This project has **119 Kani harnesses** <!--count:kani-harnesses-->, and DECISIONS §14 says …
```

**5. Run the gate.** It prints one line on success and names both values on failure:

```
$ script/lint
==> counted claims
counted claims: 8 markers over 3 counts, all agreeing with the tree
```

```
$ script/lint          # after somebody lands a proof and does not touch the prose
==> counted claims
lint: a counted claim disagrees with the tree:
  notes/fuzzing.md:13: says 119, the tree says 120 (kani-harnesses: how many Kani proof harnesses
  the tree carries, which is what script/verify proves)

Fix the number, or fix the derivation in script/lint if the tree is right and the gate is asking
the wrong question. See notes/counted-claims.md.
```

### What it found on its first runs

**Two things, and the second one caught the gate's own author.**

The `harness-crates` derivation reads the tree, and `script/verify` reads a hand-kept table of crates
to prove. They disagreed by one: **`mdns_proto` landed with milestone 55 carrying three harnesses and
was never added to that table**, so nothing proved them and nothing said so. The suite went green
*faster*, which `notes/verification.md` already names as the dangerous failure mode; it simply arrived
through the crate list rather than through the shard packer that note was worried about.

That is the argument for deriving rather than maintaining, made by the mechanism on the day it was
built. The row was added; the omission is recorded in `script/verify`'s comment where the next person
to edit that table will read it.

Then, documenting this very check in `notes/scripts.md`'s `script/lint` table row, the addition took
that row from 1835 bytes to 2108 and **overflowed `manual::render::LINE_MAX`**, which is 2048 because
1835 was the measurement it was sized against. The renderer truncated, and the failure arrived as a
`manual` render test asserting that no character is dropped, quoting text three hundred lines further
down the file. Nothing connected the two. `longest-markdown-line` is in the registry because of that
half hour: the number was in a doc comment, it was load-bearing, and nothing re-derived it.

## What this is not

**It is a ratchet, not a sweep.** An unmarked number stays unchecked. The population grows as people
touch numbers, not in one pass, and there is deliberately no attempt at tree-wide markup.

The posture is `script/names --check`'s, which is the closest relative in the tree: that gate insists
a name carry a provenance *state* and never that the state be `ratified`, because a gate demanding
ratification would block every unrelated merge behind a review queue. The same restraint here.
**Insist a marked number be right; never insist that every number be marked.**

Two neighbouring classes were measured and deliberately excluded, so nobody builds them here by
mistake:

- **TODOs that name a milestone.** Mechanically checkable and not worth a mechanism: the whole tree
  has two, and one of them is a retrospective *about* a TODO that was already removed. A gate with
  one live subject is a gate nobody remembers exists.
- **Prose asserting the system lacks something it now has.** `notes/stack.md` carried "we don't have
  that" about guard pages from milestone 1 until 2026-08-14; milestone 4 built them and milestone 90
  finished the job. Real, common, and **not checkable**: it needs a reader who knows the system, which
  is milestone 117, the stranger test.

## Three relations, and equality is the wrong one for anything that moves

```markdown
**124 harnesses** <!--count:kani-harnesses-->                  a census: must EQUAL the tree
**over 100 harnesses** <!--count-at-least:kani-harnesses-->    a floor: fires when the count DROPS
**at most 100** <!--count-at-most:unsafe-density-outside-arch--> a ceiling: fires when it RISES
```

**Why the second exists, measured 2026-08-17.** `kani-harnesses` was marked at four claim sites and
the count moved three times in one day. So every lane that added a harness had to edit four files,
one of them `README.md`, which nearly every other lane touches as well. **The convention had
manufactured a merge hotspot that the tree itself did not have**, and it did it to the file with the
most traffic in the repository.

Reading what each site actually claimed split them cleanly, and that is the reusable part:

| Site | What it claims | Relation |
|---|---|---|
| `notes/verification.md` | the canonical census, with its own history | equality |
| `notes/unsafe-obligations.md` | "exactly five Kani items, `any` (287), `proof` (124)" | equality |
| `README.md` | coverage and scale, in a table row about `script/verify` | **floor** |
| `notes/fuzzing.md` | comparative context for a different technique | **floor** |

**For a monotonically growing quantity, a floor is the true claim and an exact equality is a
maintenance tax that buys nothing.** "Over 100 harnesses" is what those two sentences were really
asserting; the exact figure was incidental to the point they were making, and it cost a four-file
edit every time somebody proved something new.

**The ratchet is intact, which is the only test that matters.** A floor that goes false still fails
the build, and going false means harnesses were **deleted**, which is precisely the event worth
catching and the one an equality check was drowning in noise. Rounded to 100 it will not move for
months, and when it does, moving it is a deliberate act rather than bookkeeping.

### The ceiling, which is the floor read upside down (milestone 134, 2026-08-18)

The floor suits a quantity where **more is better** and the bad event is a deletion. Some quantities
are the other way up, and unsafe is this tree's clearest one: nobody wants more of it, every
addition should carry a reason, and the bad event is an upward drift that no single commit looks
responsible for. calef asked it as one question, *"is that something we should be monitoring and
driving in a particular direction over time?"*, and a **direction** is exactly what the other two
relations cannot express. Equality says "this is the number"; a floor says "at least"; only a
ceiling says "and it should not grow".

**A ceiling is written above the tree, and the headroom is measured rather than guessed.** In the
fourteen days to 2026-08-18, 38 non-merge commits changed the number of `unsafe {` outside
`kernel/src/arch/`. A ceiling written at the tree's exact value would have fired on all 38 and sent
every one of them to edit the same line of the same note, which is the merge hotspot this convention
already manufactured once with `kani-harnesses`. The exception is a population small and
consequential enough that every addition deserves the stop: `unsafe-thread-safety-claims` is written
at the tree's exact value, and it moved twice in three weeks rather than 38 times in two.

**And the quantity often has to change to make a ceiling honest.** The obvious ceiling on unsafe is
a count of blocks, and it is wrong for a tree that is still being built: outside `arch/` the count
went 171 to 747 in a month while the **density** fell from 228 to 93 per 10,000 lines. A count
ceiling would have fired on almost every lane, which is the signature of a check that gets deleted;
a density ceiling holds a trend that is already going the right way and stays silent for a lane
adding a driver at the tree's own rate. Choosing the relation is half the work and choosing the
quantity is the other half.

**Deliberately not built: an auto-fix.** A `--fix` that rewrote marked numbers was considered and
refused. This gate's failure message offers two responses, and they are not equally likely to be
right: *fix the number*, or *fix the derivation if the tree is right and the gate is asking the
wrong question*. An auto-fix biases every disagreement toward the first, and the second is where the
real bugs are; the `mdns_proto` shard hole was found exactly that way. With two exact sites left,
hand-editing is cheap and thinking is the point.

## BUGS

- **An unmarked number is unchecked, by construction.** The gate reports nothing about it, so "lint
  passed" never means "every number in the tree is right". It means every *marked* number is. This is
  the ratchet working as designed and it is the first thing to know about it.

- **A floor says less than it looks like it says.** `over 100` beside a tree of 124 is true and
  uninformative, and a reader who wants the number has to go to `notes/verification.md`. That is the
  trade: two files stop being a collision point and one indirection appears. If a floor ever drifts
  so far below the truth that it misleads (say the tree reaches 400), raise it, because nothing warns you,
  because a floor with slack is exactly what the relation is for.

- **A marker can lie about what it counts.** Nothing checks that `<!--count:kani-harnesses-->` sits
  beside a sentence about harnesses rather than about crates; the gate matches the number, not the
  noun. Same limit `script/names --check` records: presence is checkable, meaning is prose, and prose
  is checked by reading.

- **A marker works in a `.rs` doc comment, and the fence exemption does not follow it there.** The
  2026-08-17 sweep put four in `crates/abi/src/lib.rs` and `kernel/src/syscall.rs`, because the
  claims it was gating live in the boundary artifact rather than in a note, and an HTML comment is
  invisible in rustdoc. The gate finds them: it greps every tracked file. But the fenced-block skip
  is implemented for `.md` only, so a marker inside a ```` ```text ```` block in Rust source **is**
  checked as a live claim. Nothing in the tree does that today. Keep markers in the prose of a doc
  comment, never inside its fences, and note that the file's own examples cannot demonstrate the
  convention the way a note's can.

- **Only fenced blocks and backtick spans are exempt.** A marker inside a fence, or inside inline
  code, is being shown rather than asserted, and the gate skips it: prose explaining the convention
  has to be able to spell it, the same exemption `script/lint`'s rejected-vocabulary check makes for the
  documents that argue about the word. A marker inside a **four-space indented** code block is *not*
  recognised and will be checked as a live claim. Write examples in fences.

- **One number per marker, one marker per number, on one line.** A marker on the line after its
  number fails with "no number before it on this line", which is a confusing message for what is
  really a line-wrapping accident. Reflow the paragraph so the two sit together.

- **A count spelled in words is read, but only a small one, and never a composed one.** The marker
  took digits only until the 2026-08-17 documentation sweep, which needed words: the claim it went to
  gate was "the surface is three calls", and a small count in prose about a design is written as a
  word far more often than as a numeral. Cardinals up to `twenty`, plus the round tens and
  `hundred`, are understood; **`twenty-one` is not**, and a marker on one fails with "no number
  before it" rather than passing. That is deliberate rather than unfinished. A count large enough to
  want composing is one a person writes in digits anyway, and a parser that accepted "one hundred
  and twenty" would be guessing at prose. This changed nothing about the ratchet, because only a
  *marked* number is ever read: admitting words cannot make the gate fire on an ordinary sentence
  that says "the three of them".

- **An untracked file is not scanned.** The gate runs `git grep`, so a marker in a file that has not
  been `git add`ed yet is silently unchecked until it is staged. It starts being checked at the moment
  it could reach anyone else, which is the right moment, but it does mean a local run can pass on a
  marker that CI will reject.

- **Some counts are too expensive to derive here.** `script/lint` runs on every build, so anything
  needing a compile (the frame sizes from `script/stack-frame-check`, the benchmark numbers) does not
  belong in the registry. Refuse the entry rather than quietly making the gate slow; a hated gate gets
  routed around, and a routed-around gate protects nothing.

- **A wall clock is not a count.** `notes/verification.md`'s timing table stays a dated measurement,
  because re-deriving it means running the suite. Dating a measurement is the honest alternative to
  gating it, and the two should not be confused.

- **Counts that span the tree are still the integrator's at merge** (AGENTS.md). Two lanes can each
  mark a number correctly for their own branch and disagree after the merge. The gate turns that from
  a silent wrong number into a failing group build, which is the improvement rather than a cure.

- **`AGENTS.md`'s method figures are unmarked**, including a Kani harness count that was wrong on the
  day it was tested. A lane may not edit that file, so the first tranche could not reach it. Marking
  that paragraph is a judgement about how much machinery a piece of rhetoric should carry, and it is
  the obvious next tranche.
