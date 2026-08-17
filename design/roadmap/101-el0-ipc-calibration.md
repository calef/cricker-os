# 101. The L4 calibration, read from the IPC number that pays for the trap

**Status: PARTIAL** since 2026-08-04 (PR #104). Raised 2026-08-04 as "build the EL0-to-EL0 IPC
benchmark nobody owns", and that is not what the tree says. **The benchmark exists and has been
published for days.** The milestone survived because the comparison built on it did not follow, and
the corrected comparison is considerably worse for us than the one that was on the page.

**Gate: MILESTONE 74, HARDWARE.** What remains is one step, reading cycles from a PMU instead of
deriving them from nanoseconds and an assumed clock. That is milestone 74's deliverable by name, and
it rides milestone 16's silicon. This line read `NONE` until 2026-08-16, which was true while a
paragraph was still waiting to be rewritten and stopped being true when it was: nothing in this
milestone is startable today, and a gate of `NONE` was advertising the opposite to
`script/roadmap --ready`.

## What has landed, and what it cost us to say

The measurement and the paragraph are done (PR #104, commit `b4178513`). One release-build run of
both IPC benchmarks on one machine, both planes from the same boot, five boots back to back:

| bench | plane | ns/iter, median of 5 | what one iteration includes |
|---|---|---|---|
| `ipc_rtt` | kernel-side | 46 | two rendezvous, one address space, no trap |
| `ipc_rtt_el0` | EL0 to EL0 | 350 | two rendezvous, two address spaces, four `svc`s |

Against seL4's published pair for the same-core cross-address-space path, 413 cycles for the call
and 426 for the reply, one-way each, so **~839 cycles** for a round trip in our sense: the corrected
figure is **roughly 1.1x to 1.7x an L4-lineage round trip**. The range is the clock, not the
measurement. HVF passes through no PMU, so a cycle count here is nanoseconds times an *assumed*
clock, and the host M3 runs 2.75 GHz on an E-core and 4.05 GHz on a P-core with nothing pinning the
vCPU thread to either.

**Three errors were found where one was expected**, and they are the reason the original "4 to 7
times heavier" read as sober: wrong plane (kernel-side, no trap), wrong build (debug against their
release), and wrong convention (our round trip against their one-way figure). Two inflate the ratio
and one deflates it, so the number landed somewhere plausible by cancellation. notes/benchmarks.md
carries all of it, with the four caveats that matter more than the ratio does: 2015 silicon against
2023, their fastpath on and ours nonexistent, their two syscalls against our four, and an assumed
clock. **None of that makes this a win**, and the note says so in a section headed that way.

## The prediction of record, left standing

Everything under this heading is the block as it was raised on 2026-08-04, before anything was
measured. **It is wrong**, and it is kept rather than quietly fixed, for the same reason
notes/benchmarks.md keeps the plausible first reading of the RISC-V `map_new` movement: the record
of having been confidently wrong is the useful artifact, and a page that only ever shows corrected
numbers teaches a reader nothing about how much to trust the next one.

**It forecast 12-24x. The measured answer is ~1.1x to ~1.7x**, an error the same size as the one it
was written to correct, in the other direction. It was written to catch an overstatement and
produced a larger one.

> **What the source actually says.** notes/benchmarks.md:66 names a true EL0-to-EL0 benchmark as
> "the right follow-up" and says it "needs one `CNTKCTL_EL1` bit so EL0 can read the counter". Both
> halves are stale. ...
>
> **The numbers, from the note.**
>
> | what | ns/iter (HVF, debug) | what it includes |
> |---|---|---|
> | `ipc_rtt` (kernel-side, milestone 21) | ~951 | the rendezvous, no trap |
> | `ipc_rtt_el0` (EL0, the primitive suite) | ~2272 | two rendezvous and four `svc`s |
>
> **And then the L4 section compares the kernel-side one anyway.** It converts ~705 ns at ~3.2 GHz
> to ~2,200 cycles and reports us "4 to 7 times heavier" than an L4-lineage fastpath's 300 to 600.
> Run the same arithmetic on the number that includes the trap: ~2272 ns at ~3.2 GHz is roughly
> **7,300 cycles**, which is **12 to 24 times** an L4 fastpath, not 4 to 7. The two nanosecond
> figures come from different runs ... so the ratio wants one clean run rather than a subtraction
> across sessions, and it is not going to move the conclusion by a factor of three.

Two elisions above, both marked: the block's list of what the note already contained (answered in
the table below instead) and its aside on where the 705 and 951 figures came from. Everything else
is its own wording, kept because paraphrasing a retracted claim is how this same file's sibling went
wrong twice (design/roadmap/74-cycle-counters.md).

**Why it was wrong, since that is the part worth carrying forward.** The arithmetic is fine: 2272 ns
at 3.2 GHz really is ~7,300 cycles, and ~7,300 against 300-600 really is 12 to 24. Every step is
checkable and every step is right. It fixed the plane error, inherited the build error without
noticing it (2272 ns is a debug figure, and the debug-to-release tax on this path is ~6.7x,
measured), inherited the convention error without noticing it either (their 300-600 is one-way), and
inherited the 3.2 GHz clock from the paragraph it was correcting. **A correction that reuses its
target's assumptions is not a correction**, and the last clause, "it is not going to move the
conclusion by a factor of three", is the tell: it forecloses the result before measuring, which is
the one thing a milestone whose entire deliverable is a measurement must not do.

## The two stale claims, verified against the code

The block above asserted two things about the tree. Both were checked line by line on 2026-08-16,
and both had been false for longer than the block was.

| the claim | the tree |
|---|---|
| a true EL0-to-EL0 benchmark is "the right follow-up" | `ipc_rtt_el0` is `kernel/src/bench.rs:442`: two EL0 processes, two endpoints, a client self-timing `SEND`-then-`RECV` against a server process, reported at line 491 |
| it "needs one `CNTKCTL_EL1` bit so EL0 can read the counter" | the bit is set in `kernel/src/arch/aarch64/timer.rs:134` (`EL0VCTEN`, bit 1), with the RISC-V twin `scounteren.TM` at `kernel/src/arch/riscv64/timer.rs:181` |

**The sentence those halves came from no longer exists**, and that is the correction this block was
still describing as pending. Commit `b4178513` deleted it from notes/benchmarks.md in the same
change that rewrote the calibration, so from 2026-08-04 this file was quoting a sentence that was
already gone and calling it "the source". The citation `notes/benchmarks.md:66` now lands in the
middle of the corrected section, which is the well-formed-but-wrong failure mode `script/roadmap`
records as invisible to it.

**One caveat on the RISC-V twin, because "it was opened by milestone 19e" is too clean.** The
aarch64 bit was genuinely set then. The RISC-V side was *documented* as set and was not: `crates/user_rt/src/lib.rs:394`
records that the claim "was aspirational until 2026-07-30", and U-mode `rdtime` worked only because
QEMU's OpenSBI leaves the bit permitted. Firmware default, not a choice this project made, and it
would have failed on a board whose firmware clears it.

## Scope note

**Do not delete or demote the kernel-side numbers.** They are the gating instrument: icount
regression tripwires run against them, and notes/benchmarks.md explains why a kernel-internal path
length is the right thing for a gate and the wrong thing for a comparison. Both planes stay; only
the comparison moved.

**Cycles are still arithmetic here, and no ratio in this file should be quoted tighter than "same
order".** Every cycle figure inherits a ~1.5x uncertainty from not knowing which core type the vCPU
thread ran on. That is milestone 74's job, and it is why this milestone is not BUILT.

**The remaining step is milestone 74's, not a second lane on this one.** Folding it into
milestone 25 was the alternative this block named, and the reason for keeping it separate is now on
the record rather than open: the paragraph correction is what should not have waited for a board,
and it did not.
