# 80. Loom: the hand-rolled atomic protocols, model-checked

**Status: NOT-STARTED.** Raised 2026-08-03, same survey as 79.

**Gate: NONE.** The pilot is one protocol on the host and the candidates are named: the per-CPU
run-queue handoff, the reaper handoff, and the IPC sender queue. The board arriving ~2026-08-21 is
the reason to do this before then, not something it waits on.

CLAUDE.md's fourth rule says assume weak memory ordering, and no gate in this tree can currently
falsify a violation of it. QEMU's TCG executes guest atomics conservatively and explores almost none
of the orderings the architecture permits, so an acquire that should be an acquire-release passes
`script/test`, the cpu matrix, and every CI leg, then fails on real silicon at a rate and location
that will not reproduce under emulation. The VisionFive 2 arrives ~2026-08-21; this class of bug is
the worst thing it could find, because a board failure with no emulator reproduction is a debugging
session with no instrument.

Loom runs a concurrent test on the host and exhaustively explores interleavings and the reorderings
the C11 model permits, including relaxed-ordering surprises. The precondition is the project's own
rule: the protocol under test must be pure logic in a host-reachable crate, with its atomics behind
`cfg(loom)` type aliases, and no `asm!` fences in the path (loom cannot model those, which is a
forcing function in the same direction rule 7 already pushes).

The work is a pilot on **one** protocol, chosen for being hand-rolled rather than spin-locked;
candidates are the per-CPU run-queue handoff (DECISIONS §28), the reaper handoff, and the IPC sender
queue. Deliverables: the protocol lifted (if needed) into a host-testable form, loom tests over it,
and a note recording the method and whether the second protocol is worth the retrofit.

## Scope note

Loom models C11, not the ARM or RISC-V memory model, so it narrows the gap rather than closing it;
litmus-level confidence would need herd7-style tooling and is not this milestone. Milestone 81 is
the complementary leg: real silicon executing the real orderings, unsearched but genuine.
