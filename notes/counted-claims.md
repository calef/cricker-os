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

**The fourth entry is the one with a consumer rather than a reader**, and it is worth understanding
before you add a fifth. `manual::render::LINE_MAX` is 2048 because the longest markdown line is 1835,
and a document over `LINE_MAX` is truncated. So that number is a **margin**, and a lane that spends
it silently makes the renderer wrong about a file nobody has written yet. Every other entry here
describes the tree; this one guards it. If you can find another number in that class, it is worth
more than three descriptive ones.

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

## BUGS

- **An unmarked number is unchecked, by construction.** The gate reports nothing about it, so "lint
  passed" never means "every number in the tree is right". It means every *marked* number is. This is
  the ratchet working as designed and it is the first thing to know about it.

- **A marker can lie about what it counts.** Nothing checks that `<!--count:kani-harnesses-->` sits
  beside a sentence about harnesses rather than about crates; the gate matches the number, not the
  noun. Same limit `script/names --check` records: presence is checkable, meaning is prose, and prose
  is checked by reading.

- **Only fenced blocks and backtick spans are exempt.** A marker inside a fence, or inside inline
  code, is being shown rather than asserted, and the gate skips it: prose explaining the convention
  has to be able to spell it, the same exemption `script/lint`'s rejected-vocabulary check makes for the
  documents that argue about the word. A marker inside a **four-space indented** code block is *not*
  recognised and will be checked as a live claim. Write examples in fences.

- **One number per marker, one marker per number, on one line.** A marker on the line after its
  number fails with "no number before it on this line", which is a confusing message for what is
  really a line-wrapping accident. Reflow the paragraph so the two sit together.

- **A count spelled in words is invisible.** `notes/interleaving.md` says "sixteen harnesses across
  three crates" and no marker can help it. Digits or nothing.

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
