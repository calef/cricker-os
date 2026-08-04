# 4. MMU on: page tables, the kernel heap, the high half

**Status: BUILT.**

Backfilled 2026-08-03 from history (milestone 76). The one early milestone that landed as named
steps rather than one commit, all on 2026-07-13:

- `fbf5ed6` "step 1": aarch64 page tables, host-tested, which is the first appearance of the
  method the whole project now runs on (pure logic in a host-testable crate, the machine gets the
  thin shell).
- `474ace4` "steps 2+3": the MMU is on, with W^X and the guard page milestone 3's stack overflow
  had been waiting for.
- `085da23`: the kernel heap; Vec and Box work again.
- `085991f` "complete": the kernel runs in the high half, out of TTBR1.

The plan row (`b7f10e7`) was "MMU on: page tables, address spaces, kernel heap," and the outcome
matches, with the heap later removed on purpose by milestone 14, which the record here should not
obscure: the heap this milestone added is the one "kernel objects from untyped" spent a milestone
taking back out.

The same day's follow-on commits are part of the milestone's honest record: `ce55137` proved TLB
invalidation works rather than asserting it, `499be5e` added a slab allocator "because the
measurement said to," and `2ea4c6a` fixed `halt()` to use `wfi` after an idle kernel burned a
whole host core, the finding CLAUDE.md still carries.
