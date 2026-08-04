# 115. The names that were ratified, and the ones that were refused

**Status: NOT-STARTED.** Raised 2026-08-04 by Chris, asking whether anything tracks the names he
has ratified. Nothing does, and the same day produced the evidence for why it should.

**Gate: NONE.** Nothing blocks a start: the backfill is mechanical, the worklist it produces is the
deliverable, and the tenet it records already exists.

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

**The deliverable, and its first draft was wrong in an instructive way.** That draft proposed one
ratified-names table in notes/naming.md. Chris rejected it on 2026-08-04 for scaling like the
original `design/roadmap.md` and `DECISIONS.md`, which is exactly right and is the third instance of
that pattern in three days. Size is the smaller half; the **conflict shape** is the real one, since
every lane that adds a name would edit the one file, which is what produced three §-number
collisions in a day. The fix is the one this tree reached twice the same afternoon: **do not
maintain a record, derive one.**

1. **Provenance lives at the name.** A crate's `lib.rs` header, a program's module doc and a
   script's comment block already say what the thing is; each gains a line saying when its name was
   ratified and what was refused. `job_undertaker` carries why `job_killer` was refused;
   `crates/system_initializer` carries why `system_builder` was. That is this project's own
   posture, the reason beside the thing, and it fixes the failure that prompted the milestone:
   a refusal is most useful to whoever is about to propose the same name, and that person is reading
   the file where the name would go, not a registry.
2. **A lint checks presence, never content.** Every crate, program and `script/` entry point carries
   a provenance line or the build fails: 42, 54 and 27 today. Adding a name touches exactly one
   file, so two lanes naming two things cannot collide.
3. **The table is a query.** `script/names` (provisional) collects the lines into the view a reader
   wants, computed rather than maintained, so it cannot drift from the tree. Same family as
   `script/roadmap`, `script/decisions` and `script/catch-up`.
4. **The maintainer writes the provenance at ratification**, in the same commit that applies the
   name, when the alternatives are still in mind. A convention, so it is Chris's to land in
   CLAUDE.md.

Refusals of names that were never adopted anywhere (`caretaker` for the steward role, `Project
Manager` for the maintainer) belong where that thing is defined, which for the roles is CLAUDE.md
and already says so.

## Scope note

**Not a rename pass.** Nothing in the tree changes name because of this milestone; the backfill
records what is already true, including names that predate the tenet and were never explicitly
ratified (say so in the table rather than inventing a ratification). Where the history does not say
who chose a name or why, the honest entry is that it is unrecorded, which is itself a finding about
how much of the tree's vocabulary arrived unexamined.

The lint's blind spot, stated up front: it can check that a name is *in* the table, never that the
table's reason is still true. That is the same limit `script/decisions --check` records about
citations, and milestone 97 is the neighbouring case.
