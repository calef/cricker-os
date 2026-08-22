# 148. A noise bound, not a noise measurement

**Status: NOT-STARTED.** Minted 2026-08-21 by calef, from the same HPC-differentiation question as
milestone 147. Every HPC center already measures operating-system noise with FTQ/FWQ-style
microbenchmarks (Fixed Time Quantum / Fixed Work Quantum: sample how much work a tight loop
completes per fixed interval, and read the jitter in the histogram). None of the general-purpose
kernels those benchmarks run against can turn a good histogram into a proof, because their scheduler
and interrupt paths are not verified and their source of possible preemption is not exhaustively
enumerable by a reader.

**Gate: NONE.** The first increment needs nothing that does not exist: milestone 51's clock service
is enough to port FTQ today, on both boards, with no PMU and no new kernel surface. The *bound*
argument that follows the measurement is where this milestone earns its number, and it needs no
decision to start either: it needs the kernel's own preemption sources enumerated, which is reading
code that is already written.

**In brief.** OS noise is the interference a running compute thread experiences from anything other
than its own instructions: timer ticks, interrupt delivery, other threads, softirq-shaped kernel
work. At HPC scale it is not a curiosity: Petrini et al.'s ASCI Q study showed noise amplifying
across a barrier-synchronized job of thousands of nodes into slowdowns wildly disproportionate to
the noise's own size on one node, which is why lightweight kernels (IBM's CNK, Cray's CNL) exist as
a category. Every tool that quantifies it today (Netgauge's FTQ port, LLNL's `system-noise` FWQ
suite, the Linux `osnoise` tracer) reports **what happened on one run**, at whatever confidence a
histogram carries. This milestone's claim is different in kind: **enumerate every kernel-side event
that can ever preempt a running thread, and bound each one's worst-case cost**, the way milestone
132's fastpath-footprint gate already turned "the IPC path is fast" from a benchmark into an
enumerated, gated call-graph walk.

**Context, not evidence**: Brown, "RISC-V for High Performance Computing" (CUG '25, ACM
3757348.3757367) is the source calef read that raised the HPC-differentiation question behind both
this milestone and 147. It does not measure OS noise (its performance-tooling gap is about hardware
counters and profilers, milestone 147's territory, not about scheduler/interrupt jitter), so it is
cited here only for the board it happens to already report real numbers on: **the VisionFive V2's
JH7110, the same board calef has wired to his bench for milestone 16a**, appears in the paper's
Table 2 (NPB Class B, Mop/s) alongside the SG2042, VisionFive V1, SiFive U740 and others. That table
is a useful sanity check for milestone 149's NPB comparison, not for this milestone's noise claim;
the paper is silent on OS noise entirely, which is worth stating plainly so this milestone's
citation of it does not overclaim support it does not offer.

## Two phases, and the first needs no decision

### Phase A: measure, and compare (buildable today)

- **Port FTQ** (BSD-licensed, plain C, the serial variant needs no threads) as a nife user program
  against milestone 51's clock service. `notes/benchmarks.md` already has the discipline (matched
  virtualization tier, same device, same noise controls) this needs to reuse rather than invent.
- **Run it on both boards** (aarch64 QEMU/HVF, riscv64 VisionFive 2) and against Linux on the same
  hardware, the same controlled-comparison shape milestone 140's ext2 stratum argues for filesystem
  work: **holding the machine constant isolates the kernel**, which is the only comparison worth
  publishing.
- **The expected finding, stated as a prediction so it is falsifiable rather than assumed**: nife's
  noise floor should be near the width of its own timer tick and IPC latency, because there is no
  softirq-shaped deferred work, no opaque driver thread pool, and no per-CPU load-balancer running
  underneath a compute thread the way Linux's does. If the measurement disagrees, that disagreement
  is the finding, not a reason to discard the run.

### Phase B: enumerate, and bound (the differentiator)

**A histogram is a claim about one run; an enumeration is a claim about every run.** Phase B is
walking the kernel's own preemption sources and pricing each one, which is possible here specifically
because the kernel is small enough to read completely (the same property that makes milestone 132's
call-graph walk and milestone 84's stack high-water mark tractable at all):

- **Every interrupt source the confined scheduler can take**: the timer tick (bounded, periodic,
  known period), IPC delivery (milestone 101 already measures this path's cost), and device
  interrupts routed to a confined driver (milestone 108's frame-capability drivers name exactly which
  ones exist). List them exhaustively; a fourth one appearing later is this gate's job to catch.
- **A worst-case cost per source**, not an average. The fastpath-footprint gate's method (walk the
  release disassembly rather than trust a benchmark that could get lucky) is the right instrument
  here too: an interrupt handler's own worst-case path length is a static property of the compiled
  binary, not a sampled one.
- **A published bound**, in the same register-of-measures discipline milestone 134 already keeps:
  "a compute thread on this kernel is preempted for at most N cycles per tick, from these M
  enumerated sources, and no others exist" is a sentence CrayPat, VTune and Linaro Forge cannot let
  their host kernel say, because none of those kernels are small enough for a reader to check the
  word "only."

## Why this is the differentiator and not just a second benchmark

CrayPat, VTune and Forge all report *what a profiled run showed*. The HPC noise literature already
treats that as insufficient for the barrier-synchronization case precisely because noise on one node
is not linear in its effect at scale (Petrini's finding), so a center wants a **guarantee**, not a
sample. No general-purpose kernel offers one, because none is verified and none is small enough for
a human to enumerate its own preemption sources with confidence. **nife's verification path (Kani
over the capability core, loom over the concurrency-sensitive paths, the fastpath-footprint gate's
call-graph method) is not a testing convenience here: it is the mechanism that makes Phase B's
"and no others exist" a checkable claim rather than an assertion.** That is the sentence to put next
to the histogram, and it is the sentence a lightweight-kernel HPC customer has never been able to
get from Cray, Intel, or Linaro's tools, because those tools profile a kernel none of them verify.

## What each phase needs to answer

- **What counts as "noise" versus legitimate scheduled work.** A thread yielding voluntarily is not
  noise; a thread preempted involuntarily is. The enumeration in Phase B should distinguish them
  explicitly, because conflating them is how FTQ-style tools sometimes over-report on systems that
  are, in fact, behaving correctly.
- **Whether the bound is per-tick or amortized.** A single expensive but rare event (say, a TLB
  shootdown, milestone 58) may cost more than a tick's worth of budget once, which is a different
  claim than "no tick ever exceeds N".
- **Where confined drivers fit.** Milestone 108's frame-capability drivers each own an interrupt
  source; Phase B's enumeration is only complete if it reads every driver's interrupt registration,
  not just the kernel's own timer and IPC paths.

## What this does not decide

- **Multi-node noise amplification.** Petrini's finding is about noise compounding across a
  barrier-synchronized job at scale; nife has no cluster story yet (milestone 54's SMB and milestone 146's NFS are
  single-node file service, not job scheduling), so this milestone is single-node evidence for a
  claim that would need a cluster to demonstrate fully. Recorded as the natural follow-on once nife
  runs on more than one board at a time.
- **Real-time scheduling classes.** A bound on preemption cost is adjacent to real-time guarantees
  but is not one; this milestone does not add a scheduling policy, only a measured and enumerated
  ceiling on the one that exists today.

## BUGS

- **Nothing here has run on real hardware yet.** Phase A's comparison needs the VisionFive 2 (present)
  and a matched Linux boot on the same board (not yet arranged), the same "controlled comparison"
  gap milestone 140's ext2 stratum names for its own benchmark.
- **Phase B's enumeration is a claim about today's kernel and will rot the moment a new interrupt
  source is added without updating it.** This is the same defect class milestone 125's counted-claims ratchet
  exists to catch elsewhere in the tree; Phase B should ship its own version of that check (a
  registered list of preemption sources the build fails to compile against if one exists that the
  list does not name) rather than a one-time document, or it becomes exactly the kind of prose claim
  that goes stale silently.
- **No effort estimate.** Phase A is a port with a precedent (FTQ already exists, benchmarks.md's
  discipline already exists); Phase B has no precedent in this tree beyond the two static-analysis
  gates (fastpath-footprint, stack high-water) it most resembles, and both of those took real
  iteration to land honestly rather than optimistically.