# 114. Split `DECISIONS.md`, and give a decision a status

**Status: BUILT (2026-08-04, the day after it was raised; the postscript records what the numbers turned out to be).** Raised 2026-08-04 by Chris, asking whether decisions should be managed the
way milestones now are: a directory, an index, one document each, and a status.


**Measured against the case that justified milestone 76**, because the argument is the same argument
and the numbers should have to carry it:

| | `design/roadmap.md` at its split | `DECISIONS.md` today |
|---|---|---|
| lines | ~6,200 | **5,320** |
| entries | 88 blocks | **71 sections** |
| citations tree-wide | 2,255 | **2,017** |
| churn | nine entries in one day | **126 commits in ten days** |

The conflict evidence is already in CLAUDE.md and did not need re-deriving: **three section-number
collisions in one day**, which is exactly what the roadmap split made structurally impossible by
giving each entry its own file and leaving the number to the integrator at merge.

**Two things this adds that the roadmap split did not need.**

**A decision has no status today, and that is why supersession rots.** A milestone says NOT-STARTED
or BUILT; a decision says nothing about whether it still holds. Milestone 94's sweep found §11
carrying a paragraph superseded by §28, invisible to every gate because `script/decisions --check`
verifies that a cited `§N` resolves to *some* section, never that the section still means what it
said. A lifecycle (`PROPOSED`, `DECIDED`, `SUPERSEDED BY N`, `AMENDED`) makes supersession a checked
fact. The vocabulary is provisional and Chris's.

**It absorbs `design/open-decisions.md`.** That file was created hours before this milestone, and it
holds decisions in an early lifecycle state: waiting on Chris, with options and a recommendation. A
`PROPOSED` decision is the same object one step earlier, so keeping two systems for one concept is
the duplication milestone 96 spent a day removing from the inits. One directory, one index, one
lifecycle, and an answered decision changes status in place rather than moving between files.

**What must survive, and it is the whole risk.** 2,017 `§N` citations must keep resolving, exactly
as the roadmap split preserved `milestone N`. **Do not renumber**, move content verbatim, and make
the diff reviewable as a move rather than an edit; milestone 76's lane proved its split byte-for-byte
by inverting every mechanical adjustment and reproducing the original file, and that standard applies
here.

## Scope note

**Sequence it with milestone 97**, which is already about citations naming what they cite. The two
interact in a way worth exploiting rather than colliding with: once a decision is a file with a
title, a citation's parenthetical name can be checked against that file, which is the enforcement 97
wants and cannot have while every section lives in one document. Doing 114 first makes 97's lint
cheaper; doing them in the wrong order means writing the check twice.

Sequence it also **when no lane holds unmerged `DECISIONS.md` edits**, for the reason the roadmap
split cited about a 70-file mechanical change. That is a real constraint here, since a decision
section is the most likely thing for a design-fork lane to be writing.

## Postscript: what the build found (2026-08-04)

Three of this entry's numbers were taken from the milestone 76 comparison rather than from the
tree, and two were wrong. Corrected from the merged tree, per CLAUDE.md:

| | this entry said | the tree had |
|---|---|---|
| lines | 5,320 | 5,320 |
| sections | 71 | **67** |
| `§N` citations | 2,017 | **2,136 total, 1,862 outside the file itself** |

The split is `design/decisions/`, one `NN-slug.md` per decision and a `README.md` index, which is
the parallel this entry predicted. Location, filenames and the four status tokens are all
**provisional**; Chris settles names.

**The move was proved the way milestone 76 proved its own.** A reassembly script inverts every
mechanical adjustment (heading promotion, the `---` separators, relative links deepened one
directory, four same-file anchors that became file links) and rebuilds the original from the split
tree alone: 368,440 bytes, identical. The preamble and the closing bibliography are read back out of
the index README rather than out of the script, so the proof covers them rather than trusting them.

**One byte of content changed, and finding it is the strongest argument this entry could have
made.** §67 exists twice in the file: once where it belongs, and once inserted into the MIDDLE of
§61's BUGS sentence by commit 255879e, because that insertion anchored on the string `## Reading`
and the first occurrence of that string in the file was a wrapped prose line inside §61. The
sentence it cut in half reads "`script/decisions --check` cannot see a section in the WRONG PLACE,
only a missing number". §61 predicted the defect, in the sentence the defect broke, and every gate
reported clean for a day. The split restores the sentence and deletes the misplaced copy; that is
the whole content diff.

**The status did what the entry expected.** Eleven decisions carry a revision their opening
paragraph does not mention, and §26 is the case that proves the point: its first line still said
"not yet built" while three blocks below it recorded milestone 22 building it in two phases.

**`design/open-decisions.md` is absorbed**, its six entries becoming §68 to §73 with status
`PROPOSED`, content intact. `CLAUDE.md` still names that path and needs a one-line update, which a
lane must not make.

**The gate was rewritten and proved against eleven injected defects**, eleven caught. Its header now
states what it still cannot check, which is the well-formed-but-wrong citation, and points at
milestone 97. The interaction this entry's scope note predicted holds: 97's check is now a
comparison against a file's own H1 rather than a scan for where §43 begins inside a 5,000-line
document.
