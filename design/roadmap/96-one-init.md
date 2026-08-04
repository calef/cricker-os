# 96. One init: the spawn service written twice

**Status: BUILT** 2026-08-04 (PR #93). Raised 2026-08-04 by milestone 50's closure lane, which found it the
expensive way.

**The finding.** There are two inits. `user::initrd()` loads `"init"`, which on aarch64 is
`hello.rs`'s init role and on riscv64 is `system_initializer`, and **the spawn service is written
twice in roughly 140 near-identical lines**. The lane found it because `date` hung at an
interactive prompt with no fault and no message: the fix had landed in one init and not the other.
Along the way it also corrected notes/pipes.md, which claimed `script/shell-check` runs "the real
`system_initializer`" on both legs; it does not.

**Why this is more than tidiness.** Every capability the shell delegates passes through this code,
so a divergence between the two copies is a divergence in what authority a program receives, and
it presents as a boot that reaches userspace and prints nothing. That failure mode has now cost
three separate lanes an evening each, twice through this duplication and once each through init's
cspace size and the shell's stack size (see the sizing pattern in the same reports).

**The work.** One spawn service, in a crate both inits depend on, per CLAUDE.md rule 7: what two
binaries share is a crate, never a duplicated file. What legitimately differs between the two
boards (slot numbers, which servers exist, the clock's position) becomes data the crate takes,
not code it repeats. The parity gate makes the test easy to state: the same shell-check line must
pass on both legs, which is exactly what tonight's divergence broke.

## Scope note

Not a rewrite of either init: the boot sequences differ for real reasons and stay. Only the
duplicated construction logic moves. Milestone 22's lane also noted the loader now exists in three
places with a fault slot each (`supervision_proto` plus each init's `build_child`) and
deliberately did not unify them mid-flight; that unification belongs here, where a boot failure
cannot be ambiguous between two changes.
