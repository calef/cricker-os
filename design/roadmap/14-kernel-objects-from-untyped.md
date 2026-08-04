# 14. Kernel objects from untyped: remove the kernel heap

**Status: BUILT.**

**In brief.** Retype TCBs, endpoints, page tables; delete the kernel heap

**Why it matters.** **critical path:** a verifiable kernel cannot allocate. **Built:** the kernel has no allocator; see design/kernel-objects-from-untyped.md

**Deliverable.** Retype TCBs, endpoints, and page tables out of untyped memory, the way milestone 11
already does for user pages, and delete the kernel heap and slab.

**Why.** This finishes §10's deferred axis. Milestone 11 stopped the kernel allocating for *user*
memory; the kernel's own objects still come from its heap. It is also the real prerequisite for the
"small enough to verify" endgame: seL4's proof leans on a kernel that never allocates. Biggest item
here, and the seL4 long tail by reputation.

**On the critical path (§14).** The gate this used to sit behind ("is verifiability actually the
goal?") is resolved: it is. So this is no longer an optional purity win. A verifiable kernel cannot
allocate dynamically, so removing the heap is a prerequisite for verifying the kernel at scale rather
than only its pure-logic crates. It still also buys the smaller payoff on its own terms: the
kernel-heap-exhaustion class disappears entirely.
