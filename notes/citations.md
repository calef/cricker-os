# Citations that name what they cite

A citation in this tree points into one of two numbered records: a decision (`§N`, in
`design/decisions/`) or a milestone (`milestone N`, in `design/roadmap/`). There are about 2,150 of
the first and 2,860 of the second, roughly a fifth of them in code comments. They are load-bearing:
the documentation is part of this project's deliverable, so a footnote that lands in the wrong place
is a defect in the product, not an untidiness.

Two gates already check that a citation **resolves**. `script/decisions --check` proves a cited `§N`
has an index row; `script/roadmap --check` proves a cited `milestone N` has one. Neither can prove
it resolves to the thing its author meant, and both say so in their own headers.

`script/citations` is the third gate, and it reads the target.

## Why this needed a gate at all

Twenty-eight comments across `crates/`, `kernel/`, `notes/` and `user/` credited the two-tier `^C`
design to "milestone 24". Milestone 24 is *A second aarch64 board: Virtualization.framework*. The
`^C` design is DECISIONS §24.

It was never a renumbering. The first of them was written on 2026-07-28 and the roadmap already said
Virtualization.framework that day, so it was wrong at birth and spread by copy-paste. Both existing
gates reported clean the entire time, because both numbers exist.

The tell, once you know to look for it, is that several sites read `milestone 24, DECISIONS §24`:
one thing cited twice under two schemes. `milestone N` and `§N` are two numbering schemes over the
same integers, and they agree by accident for the early numbers (milestone 12 and §12 really are
both Call/Reply IPC) and diverge after. That is exactly the trap. Reading them as one scheme is
right often enough to survive.

## The convention

**A citation may carry a gloss, and a gloss is checked.**

```
milestone 24 (a second aarch64 board)
DECISIONS §24 (interrupting the foreground process)
```

Both are self-checking: the parenthetical can be compared against the record's own title, which is
the comparison neither existing gate can make. Write the citation without a gloss and nothing new
happens; write one and it binds.

### The tolerance rule

A title is long (*"Interrupting the foreground process: two-tier, shell-held, no new kernel
surface"*) and a citation should be able to name a distinctive fragment rather than the whole thing.
A gloss passes on either of two tiers.

**Tier 1, the title.** Every content word of the gloss appears in the record's H1, order-free and
ignoring a short stopword list. A gloss abbreviates and reorders, so `§31 (the foreign-language
seam)` matches *"The foreign-language seam: C holds no capabilities and makes no policy"*, and
`milestone 55 (Time Machine over SMB3 with Apple's extensions)` matches a title that differs from it
by one preposition. 76 of the tree's 106 glosses pass here.

**Tier 2, the quotation.** The gloss appears as a contiguous phrase in the record's body, matched on
normalized whitespace. This is a different act rather than a weaker tier 1: `§4 ("a driver never
reaches into a kernel global")` quotes what a decision *says* rather than what it is *called*, and
no title check can accept it. Contiguous is deliberate. "Every word appears somewhere in a
5,000-word document" is not a test, it is a coincidence generator.

Three shapes are not glosses and are recognised rather than judged: a repo path (`milestone 15
(design/roadmap/15-asids.md)`), a cross-reference to the other record (`milestone 12 (§12)`), and a
bare date. A path is held to a *harder* standard than a gloss, because it is exact: it must exist,
and a path into a numbered record must carry the number that cited it.

### The diagnostic that matters

When a gloss fails, the script asks whether it matches **the same number under the other scheme**,
and says so when it does. That turns the symptom into the fix, and it is the defect this whole thing
was built for:

```
citations: notes/credentials.md:346: §49's gloss (users, login, and attribution) is not
    grounded in 49-removal-and-recursion.md
      §49 is: Removal is a directory operation, and `-r` widens the grant rather than...
      but it matches milestone 49: Users, login, and attribution: what identity is for...
      Two schemes, one number. This is the milestone-24 defect.
```

## Attributed quotations

The second half. A block quote may name the file it came from, and the passage must still be there:

```
> Kernel memory is never demand-paged. Kernel pages are mapped eagerly.
>
> -- design/decisions/09-irq-safe-locking.md
```

Matching is on **normalized whitespace**, which is the only reason this is affordable. A note wraps
at 100 columns and a block quote of it wraps at 96 after the `> ` prefix, so byte equality would
fail on every correct quote in the tree and the check would be thrown away inside a week. An
ellipsis (`...`) splits the quote into segments that must appear in order, so a quote may skip the
middle of a paragraph.

### Why it is worth the false-positive cost, argued rather than assumed

The obvious objection is reflow: rewrap the source and every quote of it breaks. That objection is
answered by the normalization, and what remains is the case where the source's **words** changed,
which is precisely what the check is for.

The defect it answers is real and recent. `design/roadmap/74-cycle-counters.md` carried a block
quote attributed to `notes/benchmarks.md` (*"At ~3.2 GHz, 705 ns is ~2,200 cycles round trip... we
are 4 to 7 times heavier"*). Milestone 101 re-measured and retracted that arithmetic; the paragraph
no longer exists in the note. The roadmap was quoting a retraction as the current record, and
nothing could see it, because a prose block quote attributed to another file is a citation that no
gate resolves.

So: yes, verify it. The cost is bounded by the normalization and the payoff is a class of rot that
is otherwise invisible.

## EXAMPLES

Report every citation, classified:

```
$ script/citations
73 decisions, 115 milestones

GLOSSED CITATIONS (106)
  cross-reference  6
  date             1
  exempt           1
  path             13
  quotation        9
  title            76

ATTRIBUTED QUOTATIONS (1)
```

Gate it, which is what `script/lint` runs:

```
$ script/citations --check
citations: 106 glossed citations, all grounded (76 by title, 9 by quotation, 13 by path,
    6 cross-references, 1 dates, 1 exempt)
citations: 1 attributed quotations, all still present in their sources
```

The breakdown sums to the total on purpose. A summary line whose parts do not add up is a number
nobody can check, which is the defect `script/lint`'s shellcheck line was fixed for.

Citing the decision rather than the milestone, in a code comment:

```rust
/// Granted a per-job interrupt channel so `^C` can reach it (DECISIONS §24). A
```

Quoting a decision's own words, which tier 1 cannot accept and tier 2 can:

```markdown
DECISIONS §19 (parity is a gate, not an aspiration) applied to the toolchain.
```

Note that §19's *title* says parity is a **tenet**. The body says it is a gate. Two sites in the
tree glossed it as "architectural parity is a gate", which is neither, and both now quote the body:

> **Parity is a gate, not an aspiration.** A kernel capability ships on every supported
>
> -- design/decisions/19-architectural-parity.md

That block is not an illustration. It is the tree's one live attributed quotation, and `script/lint`
re-resolves it against `19-architectural-parity.md` on every run; edit that sentence in the decision
and this page fails the build until it is brought back into agreement.

## BUGS

**A gloss is optional, so an unglossed citation is checked by nothing here.** This is the honest
limit and it was a deliberate choice, measured rather than assumed. Requiring a gloss on the first
mention of each number in each file means **2,911 sites**, every one of which has to be read to know
what its author meant (that is the whole premise; a pattern-applied gloss would be a confident
falsehood next to every wrong number). That is not a sweep this project can do correctly in one
pass, and a mechanically-inserted gloss would make the 28 milestone-24 defects *look* verified. So
the rule is "what you write is checked", not "you must write it". The number is recorded here so
that whoever revisits it is arguing against a measurement.

**A wrong citation whose gloss is also wrong in the same direction passes.** The first of these
grounds in nothing and fails; the second grounds fine and is still in the wrong place if it sits
next to interrupt code:

```
milestone 24 (the ^C work)
milestone 24 (a second aarch64 board)
```

The check proves the gloss and the number agree, never that either matches the surrounding prose.
(Writing this paragraph is what proved the fence exemption above is load-bearing: the first line
failed the gate until it went inside a fence.)

**Tier 2 is only as strong as the target document is short.** A long decision file contains many
phrases, so a wrong-but-plausible quotation of one has more room to land in another. Nothing in the
tree does this today; it is the direction the rule is weakest in.

**A gloss may span at most one line break.** Long glosses in Rust doc comments wrap, and the
scanner joins one continuation. Two is not read, and a three-line gloss is invisible. The cap exists
because an unbounded one lets a stray `(` on line 40 pair with a `)` on line 900 and swallow the
file. This is a real limit, not a theoretical one: the line-based first draft of this script read
**zero** wrapped glosses and missed one of the milestone-24 defects for exactly that reason.

**Fenced code blocks in markdown are skipped entirely.** A page documenting this convention has to
be able to show a wrong citation, and this page does. The cost is that a genuine citation inside a
fence is unchecked. `script/lint`'s TODO gate exempts *all* of markdown for the same reason, so this
is the narrower version of a carve-out the tree already makes.

**Lettered milestones resolve to their base number.** `milestone 19e` is checked against milestone
19's file, because there is no `19e` file. A gloss describing what 19e specifically did will not be
found in 19's text and has to be phrased as commentary instead.

**Nothing checks the index rows' own titles against the files they link to**, which is
`script/decisions`' and `script/roadmap`' stated non-goal (a row may abbreviate). A row that
abbreviates *wrongly* is still invisible.

## What it found on the first run

- **28** comments crediting the `^C` work to milestone 24 rather than §24.
- **31** glosses that could not be grounded, of which **6** were the wrong record (two `§49` for
  milestone 49, one `§51` for milestone 51, one `milestone 6` for `§6`, one `milestone 8` that is
  not a filesystem, and one roadmap index row repeating the `§51` error), and the rest were
  paraphrases or commentary that read as a gloss.
- One stale `design/roadmap.md` path, which has not existed since milestone 76 split the roadmap
  into a directory.
- Two claims a decision does not make: `§32` glossed as `Endpoint::REAP`, a method name §32 never
  spells (it reaches reaping through §16's `Untyped::DESTROY`), in two places.

## The same shape in another medium

Worth naming, because it says what the general defect is and this gate only covers part of it.

Milestone 112 (the SAFETY comments that bind nobody) found a `// SAFETY:` comment in
`user/src/net_transport.rs` whose paragraph described `invoke` and capability validation, sitting
above a `write_volatile` into a DMA page. It described a different operation entirely, and it passed
`clippy::undocumented_unsafe_blocks` for as long as the file existed, because that lint asks whether
a comment is *present*, never whether it is *about the thing underneath it*.

That is the same failure as `milestone 24` resolving to a board: a reference that is well-formed and
wrong, sitting under a check that only asks whether it resolves at all. **A comment that names its
subject can be compared against it; one that does not is unverifiable by construction.** Every gate
in this file is an instance of that one idea.

SAFETY comments are milestone 112's territory and not this script's, deliberately. The point of
recording it here is that the citation gate is one member of a family, and whoever adds the next
member should recognise the shape rather than rediscover it.

## See also

- [Naming things](naming.md): why `§N` and `milestone N` are different numbers over the same
  integers.
- [The `script/` entry points](scripts.md): where this sits among the gates.
- `script/decisions` and `script/roadmap`: the two gates that check a citation resolves, and whose
  headers name this blind spot.
