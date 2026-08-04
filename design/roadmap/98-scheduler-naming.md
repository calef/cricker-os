# 98. The scheduler that stopped scheduling: name what `SCHED` actually guards

**Status: NOT-STARTED.** Raised 2026-08-04 by Chris as an abbreviation question (`sched` or
`scheduler`?), and rewritten the same hour when the question "what does `sched` schedule?" turned up
a better answer: **increasingly, nothing.**

**Gate: DECISION.** The naming question is explicitly Chris's: the type and the static hold a
thread table and an endpoint registry, and the block rejects `ObjectRegistry` and `Objects` without
picking a replacement. The module keeps `sched`, which really does schedule.

**The finding, in the struct's own words.** `Scheduler`'s comment says it outright: "Neither the run
queue nor `current` live here any more: both moved to per-CPU storage (`cpu::PerCpu`, §11 steps 3a
and 3b)... What stays is genuinely whole-machine: **the thread table and the endpoints**." The
scheduling state left in §11's per-CPU migration. What the type holds today is a thread table and an
endpoint registry, which is an **object registry**, not a scheduler.

**Two names, and only one of them is wrong.**

- The **module** `sched` is fine. `schedule()`, the preemption, and the round-robin policy the
  module doc describes all live there and all genuinely schedule. Keeping `sched` also keeps a word
  every kernel reader arrives knowing (POSIX ships `sched.h`; Linux keeps `kernel/sched/`), which is
  the guard rail that spared `elf`, `pci` and `dtb`.
- The **type and the static** are misnamed. `SCHED` guards threads and endpoints, which is why
  notes/sched-lock-inventory.md classified the lock's hot path as **IPC** rather than as scheduling.
  That note's real finding was "this is not a scheduler lock", and the naming consequence went
  unnoticed when it was written.

**It also explains an oddity that reads as bizarre until you know.** Capability operations
(`grant`, `current_cap`, `delete_current_cap`) take the *scheduler* lock, for no reason connected to
scheduling: CSpaces live inside thread-table entries, and the table is in the registry. Under the
right name that stops being a puzzle.

**Measured, and the rewrite shrank it by an order of magnitude.** The abbreviation question would
have touched **915 `sched::` call sites across 70 files**. This one touches **88 `SCHED` references
inside `kernel/src/sched.rs`, 12 `Scheduler` mentions, and one `rank::SCHED`**, because the module
path does not change. Roughly a hundred sites in one file, not nine hundred across seventy.

**The naming question that remains is Chris's**, and it is a real one because the thing is a pair
rather than one concept: a thread table and an endpoint registry, held together only by both being
whole-machine and both being under one lock. `ObjectRegistry` claims more generality than it has
(there are two kinds, not any kind); `Objects` is vague in the way §39 warns about; something
naming the pair is honest but long. Propose with what it holds, and wait.

## Scope note

Pure rename, no behaviour change, milestone 69's proof obligation applies, and the lock rank keeps
its position in the ordering whatever it is called. The note filename that started this
(`notes/sched-lock-inventory.md`) follows the code and is renamed last, not first. **Do not do half
of it**: a tree where the type is renamed and the static still says `SCHED` is worse than either
consistent answer. The abbreviation question is answered by this milestone's premise and does not
need its own entry: `sched` stays, because the module really does schedule.
