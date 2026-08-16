# 75. Directories under `design/` and `notes/` carry provenance in their own README

**Status: DECIDED.** calef, 2026-08-16, adopting the recommendation below: **a directory under
`design/` or `notes/` carries its name's provenance as a line in its own `README.md`.** Package
directories stay out, because a package directory's name is the package's name and milestone
115's existing mechanism already covers it at the crate header. Everything else the naming tenet
says about directories is a *form* rule (hyphens, or `snake_case` when it holds a Rust package)
and is untouched by this.

**Applied the same day** to the three such directories that exist: `design/decisions/`,
`design/roadmap/`, and `notes/`. The fourth is the case that exposed the gap,
`design/audit-reports/`, which **does not exist yet**: milestone 92 creates it, and its ratified
name, with `audit-trail` and bare `audits` refused for the reasons `notes/naming.md` records,
lands in its README in the same commit that creates the directory. That is written here because
a decision whose application waits on a milestone is exactly the kind that gets lost.

**No gate, deliberately, and this is rung three rather than rung two.** `script/names --check`
fails a crate or program with no provenance block because there are 126 of them and forgetting is
the normal case. There are three directories. A checker for three things is a checker nobody
maintains, and the failure it would prevent is a missing line in a file whose first paragraph a
reader is already looking at. If this category grows past a handful, the gate becomes worth
writing, and the reason to write it will be that somebody forgot.

**What.** Milestone 115 records a name's provenance at the name: a crate's `lib.rs` header, a
program's module doc, a `script/` entry point's comment block. `script/lint` fails a name with no
provenance block, and `script/names` derives the table. The mechanism covers **crates, programs and
`script/` entry points**, which is 126 names.

**The gap.** `design/audit-reports/` was ratified on 2026-08-04, with `audit-trail` and bare `audits`
refused for recorded reasons. It is a **directory**, so the mechanism has nowhere to put it. The lane
recorded it in `notes/naming.md`'s BUGS rather than stretching the schema, which was right: a
mechanism that half-covers a category is worse than one that says plainly what it does not cover.

**Why it is not obvious.** A directory has no header file to carry a line, so covering it needs a
different home (a `README.md` in the directory, or a convention file). And the naming tenet already
says directories follow their own rule (`hyphens` if they need two words, `snake_case` if they hold a
Rust package), which is a **form** rule rather than a provenance one, so the two are not the same
question wearing different clothes.

**The recommendation.** Cover directories under `design/` and `notes/` only, via a line in the
directory's own `README.md`, and leave package directories out because their name is the package's
name and is already covered there. That is three or four directories today, so it is cheap, and it
closes the case that exposed the gap. If that looks like scope creep for one directory, the honest
alternative is to leave the BUGS entry standing and revisit when a second case appears.

**Not blocked.** Milestone 115 ships either way; this decides whether its BUGS entry becomes a
follow-up or stays a recorded limitation.
