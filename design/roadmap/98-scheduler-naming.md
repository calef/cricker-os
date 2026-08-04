# 98. `sched` or `scheduler`: settle the abbreviation, once, everywhere

**Status: NOT-STARTED.** Raised 2026-08-04 by Chris, from a note filename. `notes/sched-lock-inventory.md`
looked like it wanted expanding, and the reason it should not be expanded alone is this milestone:
the note names a real identifier, so the filename cannot be fixed without deciding the identifier.

**The inconsistency, which is one layer below the filename.** The type is spelled out
(`pub struct Scheduler`), and the module and the static are not (`kernel/src/sched.rs`, `static
SCHED`, `rank::SCHED`). All three name the same thing at three different lengths. Nothing is
broken; a reader just meets the same concept spelled two ways depending on where they enter.

**Measured before it is argued**, because the cost is most of the decision: **915 `sched::` call
sites and 109 `SCHED` references across 70 files**. That is the largest mechanical rename this tree
has considered, several times milestone 63's twenty-odd names.

**The case for leaving it.** `sched` is arguably in the group CLAUDE.md protects rather than the
group it dislikes. POSIX ships `sched.h`, `sched_yield` and `sched_setscheduler`; Linux keeps its
scheduler in `kernel/sched/`. A reader arrives knowing the word, which is the exact test that
spared `elf`, `pci`, `dtb` and `gpt`. Under that reading `sched` is not an abbreviation needing a
decoder like `capsh` or `uheap`; it is the field's own short form, and the tenet says a name a
reader already knows costs nothing.

**The case for changing it.** The tenet also says names are claims, and this tree spells out
`Scheduler` where it had a free choice, which is evidence about what the author found clearest.
Milestone 63 expanded `capsh`, `uheap` and `vt` on exactly this reasoning, and a reader who has
just learned that this project expands its abbreviations then meets `sched` and cannot tell which
rule is in force.

**What settling it means either way.** If `sched` stays, that is a decision to record next to the
protected-terms list, so the question stops being reopened, and `notes/sched-lock-inventory.md`
keeps its name with the reason attached. If it goes, the rename is `sched.rs` to `scheduler.rs`,
`SCHED` to `SCHEDULER`, `rank::SCHED` to `rank::SCHEDULER`, every `sched::` call site, and the note
filename last, following the code rather than leading it.

## Scope note

**Do not do half of it.** The failure this milestone must not produce is a tree where the module is
`scheduler` and the lock is still `SCHED`, which would be worse than either consistent answer. One
mechanical commit, gated on both ISAs, with no behaviour change and milestone 69's proof obligation
applying. Sequence it when no lane holds unmerged work under `kernel/src/`, for the reason the
roadmap split cited: a 70-file mechanical rename conflicts with everything in flight.
