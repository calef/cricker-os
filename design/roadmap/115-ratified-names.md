# 115. The names that were ratified, and the ones that were refused

**Status: NOT-STARTED.**
**Gate: NONE.** Nothing blocks a start; the table can be written from git history and the tenet
already exists.

Raised 2026-08-04 by Chris, asking whether anything tracks the names he has ratified. Nothing does,
and the same day produced the evidence for why it should.

**The incident.** A lane proposed `system_builder` for the crate milestone 96 extracted; the
maintainer endorsed it; Chris overruled it to `system_initializer`. Only afterwards did the
maintainer find that **milestone 63 had already refused `system_builder`**, for a reason neither had
located: `user/src/builder.rs`'s own header calls itself "a minimal init: the system builder", so two
programs would claim one phrase. The refusal was recorded in one table cell inside one milestone
block, invisible at the moment it was needed. A blind rename then swept the old name out of that very
row, and the record of the refusal was nearly destroyed by the rename it should have prevented.

**The refusals are the valuable half**, and today produced six worth keeping: `job_killer` (claims an
authority the program is specifically denied), `system_bootloader` (claims a position in the boot
sequence it does not occupy, and milestone 88 will need the real one), `script/sanitize` (reads as
*input* sanitization in a project about confining hostile input), `script/brief` (collides with
briefing a developer, a term of art this tree minted the same afternoon), `caretaker` for the steward
role (spent on capability-narrowing programs), and `Watcher`/`Project Manager` for the same role
(both understate a delegated merge authority). None of that is written down anywhere a future
proposer would look.

**The deliverable, three parts, and the third is what makes it hold:**

1. **A ratified-names table** in notes/naming.md: the name, what it is, the date, and the
   alternatives refused with their reasons. Backfill it from git history and from milestone 63's
   table, which is the largest existing batch.
2. **The maintainer appends at ratification**, in the same commit that applies the rename, when the
   reasoning is freshest and the alternatives are still in mind. That is a convention, so it belongs
   in CLAUDE.md and is Chris's to land.
3. **A lint that every crate, program and `script/` entry point appears in the table.** 42 crates, 54
   programs and 27 scripts today. This is the part that turns a virtuous habit into a gate: an
   unratified name cannot merge, and a proposer meets the refusal at proposal time rather than after.

## Scope note

**Not a rename pass.** Nothing in the tree changes name because of this milestone; the backfill
records what is already true, including names that predate the tenet and were never explicitly
ratified (say so in the table rather than inventing a ratification). Where the history does not say
who chose a name or why, the honest entry is that it is unrecorded, which is itself a finding about
how much of the tree's vocabulary arrived unexamined.

The lint's blind spot, stated up front: it can check that a name is *in* the table, never that the
table's reason is still true. That is the same limit `script/decisions --check` records about
citations, and milestone 97 is the neighbouring case.
