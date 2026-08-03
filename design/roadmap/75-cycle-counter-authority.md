# 75. Who may read the cycle counter, and by what authority

**Status: NOT-STARTED.** Carved out of milestone 74 on 2026-08-03, at Chris's direction, because it
is a decision rather than a driver and it should not be settled inside a benchmarking milestone by a
bullet point.

## The question

Milestone 74 needs EL0 to read a cycle counter. On aarch64 the mechanism is `PMUSERENR_EL0`, and
reaching for it looks like precedent: the kernel already opens `CNTVCT_EL0` to EL0 through
`CNTKCTL_EL1.EL0VCTEN`, and notes/abi.md argues that exception carefully. **The claim to examine is
that the second opening inherits the first one's argument.** It does not, and the reason is a number.

## Why it is not the same decision

The existing argument, from notes/abi.md, is sound and worth quoting because the new case tests
exactly where it stops:

> It is an exception made with eyes open. A monotonic counter grants no authority to *affect*
> anything, only to observe the passage of time, so it does not mint the kind of ambient authority
> §10 rejects (which was about *reaching resources* you were not handed). What it does cost is a
> timing side channel, and that is a real cost every OS offering userspace timing accepts.

Both halves still hold for the PMU: it is a read, and it affects nothing. What changes is the size of
the cost that was accepted:

| | generic timer, open today | PMU cycle counter |
|---|---|---|
| resolution | **~41 ns** (24 MHz) | **~0.25 ns** |
| ratio | | roughly **160x finer** |

**The generic timer's coarseness is doing security work, whether or not it was chosen for that.** A
41 ns tick blunts a large class of cache-timing measurement simply by being unable to see it; a
quarter-nanosecond counter is the instrument those attacks want. So "we already accepted a timing
side channel" is true and is not the same as "we already accepted this one". A decision to grant the
finer one should be made on its own evidence, and recorded, rather than inherited.

## Three options, and the second is the one this OS is for

1. **Ambient, like the generic timer.** Open it to every EL0 program. Simplest, matches Linux, and
   spends the §10 exception a second time on a much better side channel.
2. **A capability.** The benchmark harness holds a token that permits the read; nothing else can. This
   is the answer the whole system is built to give, and **notes/abi.md already anticipated it**: "A
   stricter build could revoke even this and route time through a capability; we have not, and this
   note is the record of that." The consumer is narrow (the primitive suite and `sel4bench`
   comparability), which is what makes gating cheap here and would not have been true for the wall
   clock.
3. **Kernel-mediated.** EL0 asks the kernel to time an operation; the counter never opens. Strongest,
   and it defeats the purpose, because the measurement then includes the syscall it is trying to
   measure. Recorded so it is visibly rejected rather than overlooked.

Option 2 costs a capability type, a grant in the spawn path, and a trap-and-check on the register
read. It also produces a demonstration the project can use: **a fine-grained timer is exactly the
resource a capability system should be able to hand out deliberately**, and it would be the first one
whose justification is a side channel rather than a resource.

## Parity, and a caveat about where this bites

RISC-V's equivalent is `scounteren`'s cycle bit, and the kernel already manipulates `scounteren` for
`rdtime`. So the same decision applies on both ISAs and lands in the same two files (§19). One
asymmetry worth knowing before designing: on RISC-V the counter may also be reachable through SBI PMU
without any CSR opening at all, so "closed to EL0" and "unreadable" are not the same statement there.

## Scope note

This is a decision milestone. It should produce a `DECISIONS` section and a short implementation, not
a security framework. If option 2 is chosen it must **not** grow into a general "sensitive register"
capability class on one consumer, which is CLAUDE.md's rule against speculative abstraction. Milestone
74 should not land its aarch64 half until this is answered, because the wrong answer is the one that
is hardest to walk back: an ambient opening, once shipped, is a thing programs come to depend on.
