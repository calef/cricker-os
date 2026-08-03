# 76. Split the roadmap: `design/roadmap/README.md` as index, one file per milestone

**Status: BUILT (2026-08-03, the day it was raised; the postscript below says why it jumped the
queue).** Raised 2026-08-03 by Chris, after a discussion of whether the roadmap should
move to GitHub issues at all. It should not, and the reasons are recorded below because the question
will come back. What it should do is stop being one 5,375-line file.

## Why not issues, since that was the alternative considered

The friction was real: every roadmap edit takes a branch, a PR and a merge. Three arguments against
moving were made and **two of them did not survive checking**, which is worth recording so nobody
re-runs them:

- *"It would break thousands of validated citations."* **False.** `script/roadmap --check` reads one
  file and its prose check is scoped to that same file, so of 2,179 `milestone N` citations in the
  tree, the ~1,988 in code comments are validated by nothing today.
- *"The gates would need network calls."* **Mostly false.** `DECISIONS.md` was never part of the
  proposal, so `script/decisions --check` is untouched. Only `script/roadmap --check` would need the
  API, and the prose checks would simply stop covering roadmap text rather than start calling out.
- *"Consumption."* **This one holds, and it decides it.** The roadmap is read constantly, locally, by
  grep and awk, and by every lane, offline, at the commit it is working from. Answering "what is
  needed before the board" was one `awk` across 64 detail blocks. Against an API that is a fetch of
  the whole corpus, which to work with efficiently would be written to disk and grepped: the file,
  rebuilt. `git log -p` showing how an argument evolved has no equivalent either.

## Why one file is nonetheless the wrong shape

Four structural defects landed in the documentation on 2026-08-03 alone, and **every gate reported
clean through all four**:

1. §61 was appended below `DECISIONS.md`'s `## Reading` closer.
2. Milestone 69's table row said `NOT-STARTED` while its own detail block said `BUILT`.
3. A prose line wrapped so that `## Reading, §61` began a line, rendering as an H2 nobody wrote.
4. **Milestones 68 through 75 are all filed under `## The rival worth understanding, not building`**,
   an essay about seL4, because `cat >>` appends to the end of a file and the `##` sections are
   interleaved among the `### N.` blocks.

The fourth is eight instances of one mistake, made by the integrator, invisible to
`script/roadmap --check` because it verifies that a block **has** a table row and never **where** the
block sits. That is the same well-formed-but-wrong blind spot CLAUDE.md already records for citations.

A split does not detect those. It makes three of the four impossible: there are no sections to file a
block under, the filename is the identity, and `cat >>` into `design/roadmap/74-cycle-counters.md`
can only add text to milestone 74.

It also removes a conflict that already happened: **PR #19 and PR #20 collided on `design/roadmap.md`**
solely because each marked its own milestone `BUILT`.

## The shape (Chris, 2026-08-03)

- `design/roadmap/README.md` holds the table and the surrounding prose. GitHub renders a directory's
  README automatically, so browsing to `design/roadmap/` shows the index, and this is the pattern
  `notes/README.md` already sets, which `script/lint` already enforces ("every notes/*.md must appear
  in notes/README.md").
- `design/roadmap/74-cycle-counters.md` per milestone. Hyphenated, per CLAUDE.md's rule for ordinary
  markdown. Numbers run 12 to 76 today and one block is sub-lettered (`20a`), so `20a-name-the-seams.md`
  is the shape for those.
- The three `##` essays currently interleaved among the blocks are design prose, not milestones, and
  become their own files under `design/`: "One decision this roadmap still forces", "The display
  ladder", and "The rival worth understanding, not building".

## What the gate must grow

`script/roadmap --check` reads a directory instead of a file, and gains **the check whose absence let
defect 2 through**: a milestone's status in the index and in its own file must agree. It should also
keep the existing checks, which stay meaningful across files: the status vocabulary, every file having
an index row, and every `milestone N` referenced in prose resolving.

## And the prose check widens to the whole tree (Chris, 2026-08-03)

Today `script/roadmap --check` validates `milestone N` references **only inside `design/roadmap.md`**.
`script/decisions --check` already does the tree-wide version for its own citations, via `git grep`.
So two citation schemes of identical shape and identical risk get opposite treatment:

| citation | validated | occurrences in code |
|---|---|---|
| `§N` into `DECISIONS.md` | **tree-wide** | 817 |
| `milestone N` into the roadmap | **roadmap.md only** | ~1,988 |

The objection to closing that gap is that a stale citation in a code comment becomes a build failure.
Chris's answer: **that is the feature.** The documentation is versioned with the code so it cannot
describe a system that no longer exists, and a comment pointing at a milestone that was renumbered or
never existed is exactly the drift the gate is for. It is the same argument DECISIONS §61 makes about
lints, and the same one CLAUDE.md makes about citations being invisible when well-formed and wrong.

**It costs nothing to adopt: the tree passes today.** Checking every `milestone N` occurrence outside
`vendor/` and `patches/` against the table, for N >= 12, produced **zero unresolved citations**. So
this is a ratchet in §38's shape and not a cleanup, and it can ship with the gate rewrite rather than
waiting behind it.

Two details for whoever implements it. The existing `n >= 12` floor stays, because milestones below 12
predate the table and live in git history and `DECISIONS.md`. And the regex must keep matching
`milestone 16a` as 16, since sub-lettered blocks exist (`20a`).

## Backfill milestones 1 to 11, and drop the `n >= 12` floor (Chris, 2026-08-03)

The floor exists because the table started at 12 when it moved out of `DECISIONS.md`, not because the
early history is lost. It is not lost. **The original plan survives verbatim in the first commit**,
`b7f10e7` ("Record architecture decisions and the milestone plan", 2026-07-12), as a `## Milestones`
table in `DECISIONS.md` carrying 1 through 10 with a title and a "what it teaches" column. Milestone
11 was added two days later in `491f23d` as "Untyped memory: the kernel stops allocating".

So 304 live citations to milestones 1 to 11 are unvalidated for a reason that stopped being true.
Backfilling them and removing the floor takes the gate from 1,875 checked citations to all 2,179.

**Record the outcomes, not the plans, and the reason is that they differ.** Most of the early
milestones have a commit that titles them, and those titles say what actually happened:

- "Milestone 2: exception vectors, and a fault that tells you what it was"
- "Milestone 3: hand out physical memory, and detect a smashed stack"
- "Milestone 5: the GIC and the timer. The kernel is preemptible."
- "Milestone 11: untyped memory, and the number that proves the kernel stops allocating"

Where plan and outcome disagree, the disagreement is the history worth keeping. **Milestone 8 was
planned as "virtio-blk driver + read-only filesystem" and landed as "the console driver leaves the
kernel"; virtio-blk moved to 9**, which had been "Processes: spawn, exit, wait". A backfill that
copied the original table would record a plan that was overtaken and silently misdate the driver work.

Two need reconstruction rather than copying, because no commit titles them: **milestone 1** (the
earliest commits predate the convention, though "Boot to Rust on QEMU virt and print to the PL011
UART" is the commit and matches the plan exactly) and **milestone 7**, the capability decision point,
which is the densest citation target in the tree at 79 references and whose outcome is DECISIONS §10
rather than a single commit.

Mark all eleven `BUILT`. They are, and the evidence is the kernel.

## Scope note

**File moves, no content edits**, with milestone 69's proof obligation: reassembling the files must
reproduce the original byte for byte, apart from the fixed placement of blocks 68 to 75. **Twenty-nine files**
outside the roadmap link to `design/roadmap.md`, including `README.md`, `SECURITY.md`,
`DECISIONS.md`, several crates and several design notes; every one must land on the index. Relative links inside the blocks point
at `../notes/`, which becomes `../../notes/` one directory down, and `script/lint` already checks that
every relative markdown link resolves, so a missed one fails loudly rather than quietly.

## Postscript: why it jumped the queue, and what the build found (2026-08-03)

Raised in the morning, bumped to the front the same day, Chris's call. The single file did not
wait for the split: nine milestone entries landed in it in one day (a postscript to 78, and 79
through 87), it passed 6,200 lines, and it produced repeated merge conflicts between same-day
pull requests (#46 against #47, then the #52/#53 supersede sequence), which is exactly the
conflict class this entry's own rationale predicted. The entry was still NOT-STARTED while its
motivating failure was recurring, so it stopped queueing.

The build held to the scope note, with the differences recorded:

- The proof obligation was met in the strong form: a reassembly script inverted every mechanical
  adjustment (heading promotion, inserted status lines, deepened links, the phrase fixes where
  prose named the old single-file geometry, the linkified table) and reproduced the pre-split
  file byte for byte, all 468,082 bytes, before the old file was deleted.
- "Twenty-nine files link to the old path" was undercounted the way tree-spanning counts usually
  are: the merged tree had 44 live references across 34 files. Taken from grep at build time, per
  CLAUDE.md.
- The backup-server ladder essay was a fourth interleaved essay this entry's list of three
  missed (it is `###`-level, so the survey of `##` headings did not see it); it moved to
  `design/` with the others.
- Every milestone file now opens with a machine-checked `**Status:` line mirroring its index
  row, which is the agreement check the gate needed; where an old block narrated status in prose,
  the prose stays as narrative below the line.

The status-agreement check, the one-milestone-per-file checks, and the tree-wide citation check
were each proven against injected defects (a pasted `### N.` block, a second H1, a flipped
status, a stray file, a rowless file, two files claiming one number, a citation to a milestone
that does not exist): seven injections, seven failures reported, none missed.
