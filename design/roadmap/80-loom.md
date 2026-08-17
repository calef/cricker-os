# 80. Loom: the hand-rolled atomic protocols, model-checked

**Status: BUILT** 2026-08-13 (pull request #123, merge `49a0a892`). Raised 2026-08-03, same survey as
79. The status read `IN-PROGRESS since 2026-08-04, a developer holds it on milestone/80-loom` for four
days after that merge; found 2026-08-17 by the status-accuracy sweep. §76's defect class.

**The pilot found a real bug, which is the outcome this milestone existed to be capable of.** A torn
read in the clock page's seqlock: the writer had no opening fence, so a reader could observe the
generation counter's claim before the data stores it was supposed to precede. Fixed rather than merely
found, at `crates/clock_proto/src/lib.rs:335`, a `fence(Ordering::Release)` between the
compare-exchange and the plain stores, with the comment recording that `AcqRel` and `SeqCst` on the
claim were both tried and both still tear, and naming Linux's `smp_wmb()` in `write_seqcount_begin` as
the same move. Recorded in notes/memory-ordering.md:139. Nothing in this tree could have falsified it
before: that is the whole argument of the paragraphs below, and it held on the first protocol tried.

**The pilot's "then decide" was decided, and then acted on twice.** The method survived, so
`crates/wake_handshake` (2026-08-14) and `crates/canary_gate` (2026-08-15) were retrofitted by later
lanes. The pilot itself is `crates/steal_request`, lifted out of `kernel/src/cpu.rs` so it is
host-testable, with six harnesses including
`two_thieves_race_and_exactly_one_claim_is_granted` and
`a_second_victim_cannot_serve_the_same_request`. Entry point `script/interleaving-check`; loom is a
`cfg(loom)`-gated dependency so it never enters the shipping graph. See notes/interleaving.md.

**Deliberately not a gate**, which is a recorded decision rather than an unfinished deliverable: a
loom model's search cost is exponential in the interleavings, so its runtime is a step function, and
notes/interleaving.md's BUGS argues that a gate whose cost is a step function is a gate that gets
skipped. Revisit when there is a CI job for it.

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
