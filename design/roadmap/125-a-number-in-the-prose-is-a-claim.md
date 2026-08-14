# 125. A number in the prose is a claim, and nothing re-derives it

**Status: NOT-STARTED.** Minted 2026-08-14 by Chris, after asking whether the tree needed a milestone
to audit its documentation for outdated content. It does not, quite: the audit is the wrong shape for
the class of staleness that actually recurs, and this is the right one.

**Gate: NONE.** The mechanism is a `script/lint` check and a marker convention. Nothing blocks a start.

## The evidence, because the answer turned on it

Three claimed counts were tested against the tree on 2026-08-14. **All three were wrong.**

| claim | where it lives | the tree |
|---|---|---|
| "the 23 `#!/bin/sh` scripts" | `script/lint`, `.github/workflows/ci.yml` | 36 |
| "**107 harnesses across 19 crates**" | `notes/verification.md` | 110 across 19 |
| "112 Kani proof harnesses" | `CLAUDE.md` | 110 |

The last two disagree with each other as well as with the tree, which is the finding rather than a
detail: **each number is a snapshot of whenever somebody last counted, and nobody re-derives one.**
The `23` had drifted by thirteen and sat in two files.

DECISIONS §39 says a name is a claim. So is a number, and it is the kind of claim a machine can check,
which is what separates this milestone from an audit.

## Two other classes, measured and deliberately excluded

**TODOs that name a milestone.** Mechanically checkable (a TODO citing a BUILT milestone is provably
stale) and **not worth a mechanism**: the whole tree has two, and one of them is a retrospective in
`notes/teardown.md` *about* a TODO that was already removed. A gate with one live subject is a gate
nobody remembers exists.

**Prose that asserts the system lacks something it now has.** `notes/stack.md` carried "**We don't
have that**" about guard pages, with a TODO for milestone 4, from milestone 1 until 2026-08-14.
Milestone 4 built them for the boot stack and milestone 90 finished the job. This class is real and
**not checkable**: it needs a reader who knows the system. **Milestone 117, the stranger test, is the
milestone for it**, because a stranger working through the tree is exactly who trips over a claim that
stopped being true. This milestone should not duplicate it.

## What to build

A count that can check itself, and the marker is what makes it checkable:

```markdown
**110 harnesses** <!--count:kani-harnesses--> across 19 crates
```

`script/lint` finds every marker, re-derives that number from the tree, and fails on a mismatch with
both values and the file. The registry of derivations lives in the gate, one entry per name, each a
command or a short expression over the tree.

**Start with the counts that already exist and are already wrong**: `kani-harnesses`,
`harness-crates`, `sh-scripts`. `CLAUDE.md`'s block of figures (crates, user programs, lines of Rust,
commits) is the obvious next tranche and is deliberately not first, because that paragraph is prose
about a method rather than a reference table, and marking it up needs a judgement about how much
machinery a piece of rhetoric should carry.

**It is a ratchet, not a sweep.** An unmarked number stays unchecked, so the population grows as
people touch numbers rather than in one pass. A marked number can never drift again. The one-time
correction falls out as a side effect: you cannot land the gate without fixing what it finds.

## The adjacent finding, which is the same shape one level out

While `notes/stack.md` was being edited on 2026-08-14 it turned out to contain, from milestone 3:

> **`into_iter()` on a large array is a real kernel footgun.** Use `iter()` and borrow.

That note is *about* `allocator_never_hands_out_the_kernel` in `kernel/src/memory.rs`, whose
`[None; 1024]` had overflowed the stack, corrupted `.text` and hung the kernel. The array was shrunk
to 64 and **the `into_iter()` was left in place**. At 64 elements it still built a **4224-byte**
temporary, over the 4096-byte guard page, and `script/stack-frame-check` caught it eleven months later.

So the doc was right, stayed right, and the code drifted back under its own advice while nothing
looked. **A written rule with no mechanism decays**, and it decays the same way whether the rot is in
the prose or in the code the prose describes. That is the argument for this milestone's shape:
converting claims into checks, rather than reading claims and correcting them.

## What "done" means

Every count named in the registry marked at its sites and passing; the three wrong numbers above
corrected; `notes/` documenting the marker convention so the next person adds one without asking; and
a recorded note that the gate covers marked counts only, which is the honest boundary of what it
buys.

## Prior art

**Code to use:** none needed. This is a grep, a small registry and a comparison, in the same shape as
`script/lint`'s existing script-docs and naming-provenance checks.

**A design to copy:** `script/names --check`'s posture, which is the closest relative in the tree. It
insists a name carry a provenance *state* and never that the state be `ratified`, because a gate that
demanded ratification would block every unrelated merge behind a review queue. The same restraint
applies here: insist a marked number be right, never that every number be marked.

**A mistake to avoid:** a gate that re-derives a count by a method subtly different from the one a
human would use, so it fails on a correct document. The `#!/bin/sh` count is the warning: `ls script/`
gives 32, the shellcheck gate covers `script/* scripts/*.sh` which is 37 files, and "how many `#!/bin/sh`
scripts" has at least three defensible answers. **The registry entry must say which question it
answers**, in the same words the prose uses.

## BUGS

- **Only marked counts are checked**, so an unmarked wrong number is invisible and the gate reports
  nothing about it. That is the ratchet working as designed, and it means "the gate passes" never
  means "every number in the tree is right".
- **A marker can lie about what it counts.** Nothing checks that `<!--count:kani-harnesses-->` sits
  beside a sentence about harnesses rather than about crates. Same limit `script/names --check`
  records: presence is checkable, meaning is prose, and prose is checked by reading.
- **Some counts are expensive to derive.** Anything needing a build (the frame counts from
  `script/stack-frame-check`, for instance) does not belong in `script/lint`, which runs constantly.
  The registry should refuse an entry it cannot compute cheaply rather than quietly slow the gate.
- **Counts that span the tree are the integrator's at merge** (CLAUDE.md), and a gate does not change
  that: two lanes can each mark a number correctly for their own branch and disagree after the merge.
  The gate turns that from a silent wrong number into a failing check, which is the improvement, not
  a cure.
