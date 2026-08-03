# 90. A guard page under the per-CPU secondary stacks

**Status: NOT-STARTED.** Raised 2026-08-03, from milestone 84's stack inventory, which found the
asymmetry rather than assumed the symmetry: the boot stack has a guard page below it, every
dynamic thread stack has a guard page below it, and the per-CPU secondary stacks are a plain
array in `.bss` with kernel data directly beneath. A secondary that runs deep does not fault; it
scribbles.

Milestone 84's high-water assertion (secondary limit 16 KiB against 64 KiB stacks) is the only
tripwire, and it is honest about its own reach: the instrument is `cfg(test)`, so a release build
carries no protection at all, and the assertion fires after the damage on the run that finally
goes deep, not before it. A guard page fails the first overflowing access instead, at the cost of
one unmapped page of address space per CPU and zero physical frames.

The work: move the secondary stacks out of `.bss` into a dedicated region with an unmapped page
below each stack, on both ISAs (§19; the mechanism is the same one the boot stack already uses,
so this is extending an existing pattern, not inventing one). Prove the guard exists by walking
the page tables in a test and asserting the mapping is absent, not by overflowing a kernel stack
on purpose: a test that faults the kernel to pass would be a test the suite cannot survive.
Milestone 84's numbers say the secondaries run at 12% (8.5 KiB of 64), so the guard is insurance,
not a fix for a near-miss; the honest framing is that this closes the one place where the
kernel's stack story says "trust the tripwire" instead of "the MMU catches it".

## Scope note

Sizing stays as it is: 64 KiB per secondary was not the finding, the missing guard was. If the
region move makes shrinking cheap, record the option; do not take it here. The thread-stack and
boot-stack guards are prior art in this tree; cite where the pattern lives when extending it.
