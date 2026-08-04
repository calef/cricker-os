# 114. Split `DECISIONS.md`, and give a decision a status

**Status: NOT-STARTED.** Raised 2026-08-04 by Chris, asking whether decisions should be managed the
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
