# 147. A profiler that holds exactly the counters it was granted

**Status: NOT-STARTED.** Minted 2026-08-21 by calef, from a question about HPC differentiation:
what would a performance-analysis story look like that CrayPat, Intel VTune and Linaro Forge cannot
tell, given that all three are built on their host OS's ambient perf-counter interface
(`perf_event_open` or a vendor driver reachable by any sufficiently-privileged process).

**Gate: MILESTONE 75, DECISION.** Milestone 75 already asks the prior question (whether the cycle
counter is ambient or a capability) for one consumer (`sel4bench`). This milestone is what that
decision buys once there is a second consumer: a profiler. It cannot be scoped until 75 answers
what the grant unit even is, and it adds its own decision, below, about what a profiling session may
name.

**In brief.** Every HPC profiler this note surveyed reads hardware counters through host-OS ambient
authority: `perf_event_open` targets any pid the caller's privilege reaches, gated only by
`perf_event_paranoid` (a global sysctl, not a per-target grant) or `CAP_PERFMON` (a capability in
name that in practice is "root, or nothing"). A profiling tool with that access on a shared HPC node
can, in principle, sample a neighboring tenant's job. This is not a hypothetical: cache-timing and
counter-based side channels between co-scheduled tenants are the reason `perf_event_paranoid`
exists at all, and its own defence is coarse: disable unprivileged profiling everywhere, which is
why HPC centers frequently run it at `-1` in practice for exactly the tools this milestone answers.

**Independent confirmation this is a real, named gap rather than a nife-specific concern**: Brown,
"RISC-V for High Performance Computing" (CUG '25, ACM 3757348.3757367), a survey grounded in
EPCC's own RISC-V ecosystem lab, names immature performance-profiling tooling as one of a handful
of high-priority action items for the whole RISC-V/HPC community: *"the lack of mature RISC-V
profiling tooling is a significant weakness for HPC... there are no mainstream performance tools
that support RISC-V, which is due to a mixture of the software side by the tool developers but also
the hardware events that are made available by RISC-V CPUs."* That second clause matters for scope
here: the paper's own read is that the gap is not purely a software/OS-integration problem the way
this milestone frames it (ambient perf interface versus capability), but partly a hardware one:
some RISC-V silicon may simply not expose the event set (cache misses, branch mispredicts, and so
on) a profiling session would want to name. Milestone 74's own ISA-discovery pattern (`Isa`,
built at boot, probing what is actually present rather than assuming a fixed event catalogue) is
the right shape to inherit for this: **a counter-set capability should be able to name only the
events the running silicon actually reports**, discovered rather than assumed, and a probe that
asks for an unsupported event should refuse cleanly rather than silently reading zero.

**nife can make a stronger claim than "disabled by default": a profiling session can hold a
capability that names exactly one confined subtree and no other**, so a job's own profiler can read
its own counters and provably cannot reach a neighbor's, without a global sysctl standing in for the
missing per-target authority.

## What already exists to build on

- **Milestone 74** ports the actual cycle-counter drivers (SBI PMU on riscv64, `PMCCNTR_EL0` on
  aarch64), scoped deliberately narrow: "one counter, read before and after, on two ISAs... do not
  turn this into a profiling framework."
- **Milestone 75** is the capability-vs-ambient decision for that one counter, for one consumer
  (`sel4bench`), and it already names the shape this milestone would reuse: "a capability type, a
  grant in the spawn path, and a trap-and-check on the register read."
- Both of those explicitly parked the general case: 74's scope note refuses to become a profiling
  framework "for a second consumer", and this is that second consumer, arriving on schedule rather
  than by scope creep.

## What this milestone owns, once 75 answers the first question

**If 75 chooses option 2 (a capability)**, this milestone is the generalization from "the benchmark
harness holds one token for one counter" to "a profiling session holds a token naming a target and a
counter set":

- **The grant names a target subtree**, not a global "may profile" bit. §47's `enumerate`-style
  narrowing is the model: a supervisor handing a profiler a capability over its own child's
  supervision subtree, and nothing above or beside it. `caps <profiler>` prints exactly which
  processes it may read, the way milestone 126's `ps` prints exactly which subtree it may enumerate.
- **The grant names a counter set**, since the PMU can count dozens of event types and a profiling
  session legitimately wants several (cycles, instructions, cache misses, branch mispredicts) rather
  than the one counter 74 ports. This is where 74's scope note's restraint gets spent: the type
  should be able to hold a *set* from day one so it is not re-designed the day a second event type is
  asked for, but the set is still a capability's contents, not an ambient enable bit.
- **The read is a syscall against a held capability**, not a register the kernel opened to EL0
  wholesale. This is the fork 75 already frames as "trap-and-check on the register read": the
  profiler never gets ambient `PMUSERENR_EL0`/`scounteren` access; every read is checked against the
  capability it presented.

**If 75 chooses option 1 (ambient) or option 3 (kernel-mediated)**, this milestone still has a
question to answer: whether a profiling *session* (as opposed to the single ported counter) should
reopen the ambient-vs-capability fork on its own evidence, since a general profiler is a much larger
side-channel surface than one benchmark harness reading one counter. Recorded here so the fork does
not silently inherit 75's answer for a different-sized risk, which is exactly the mistake 75 itself
was carved out to avoid making about the generic timer.

## The demonstration this buys

**A confined tenant profiling its own job and provably nothing else** is a claim CrayPat, VTune and
Forge structurally cannot make, because none of them run under a kernel where "may this profiler
observe that target" is a checked capability rather than a privilege level. The demo: two confined
workloads on one board, a profiler holding a capability over only one of them, and a negative
control: the profiler attempts to read the other's counters and is refused at the type level, the
same shape milestone 123's demonstration already asks every capability claim to carry.

## What this does not decide

- **The wire format for reporting samples to a human-facing tool.** CrayPat's `.ap2` format and
  Linaro's merged multi-rank view are UI questions once the counter-read primitive exists; this
  milestone is the primitive, not the report.
- **Whether sampling (statistical, periodic interrupt) or counting (start/stop, exact) is the first
  mode.** Milestone 74 ports a counting read; sampling needs an overflow interrupt path this
  milestone does not open. Recorded as follow-on scope.
- **Multi-node aggregation.** Linaro Forge's distinguishing feature at scale is merging profiles
  across thousands of MPI ranks; this milestone is single-node, single-board, and says nothing about
  a cluster story, which nife does not have yet in any form.

## BUGS

- **This is a decision-shaped milestone riding on another decision-shaped milestone (75).** Nothing
  here is buildable until 75 resolves, and 75 itself has been NOT-STARTED since 2026-08-03. That is
  not a defect in this file; it is why the gate says so rather than hiding the dependency in prose.
- **The side-channel argument is asserted from the literature, not measured on this board.** Whether
  a confined nife process can actually distinguish a neighbor's cache behavior through any channel
  this kernel leaves open (not just the PMU) is a claim milestone 43's audit lens should eventually
  aim at, and this milestone does not attempt that broader audit.
- **No effort estimate.** The capability type, spawn-path grant, and trap-and-check are each small in
  isolation (milestone 75 says as much for the single-counter case); the counter-set generalization
  and the enumeration-style target-naming are the parts with no precedent in this tree to price from.