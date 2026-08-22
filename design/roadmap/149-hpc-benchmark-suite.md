# 149. A common HPC benchmark suite, in Rust, on nife and on Linux

**Status: NOT-STARTED.** Minted 2026-08-21 by calef, from the differentiation question milestones
147 and 148 also came from: what would make the HPC comparison concrete rather than aspirational.
Those two milestones are new capability-shaped ideas nobody else can offer; this one is the opposite
kind of value and just as necessary: **run the benchmarks an HPC reader already recognizes**, in
the same language on both sides, on the same hardware, so a number means something the moment it is
read rather than needing this project's own vocabulary explained first.

**Gate: NONE.** Two of the three suites below already exist as real, published Rust ports with no
nife-specific work required to obtain them. The gate is picking which kernels to port to nife's PAL
(milestone 64's question, asked per-benchmark) and running them, not designing anything new.

**In brief.** HPC has a small, well-known set of standard benchmarks (see the table below), and
because the comparison this project makes is always cross-OS rather than cross-language, **the
requirement is not "write benchmarks" but "run the same binary's logic on nife and on Linux and read
both numbers off the same instrument."** Rust already has usable, real implementations of the two
most load-bearing suites:

| Suite | What it measures | Rust status | Fit |
|---|---|---|---|
| **NPB (NAS Parallel Benchmarks)** | 5 kernels + 3 pseudo-apps: floating point (EP), irregular memory/communication (CG), long-distance communication (FT), integer sort (IS), multi-grid (MG), and three CFD solvers (BT, SP, LU) | **NPB-Rust** (GMAP/PUCRS, published paper, arXiv:2502.15536): sequential + Rayon-parallel, both build today, MIT/Apache license per the repo | Best fit. Standard, recognized, a real academic port already validated against Fortran/C++ NPB, `#[cfg(class=...)]`-selectable problem sizes A through F |
| **STREAM** | Sustained memory bandwidth (Copy/Scale/Add/Triad) | `stream-benchmark` crate exists but is GPL-licensed (a problem for this tree's dependency rule, see below); the algorithm is ~40 lines and trivial to write clean-room | Second easiest. Milestone 138's own read-gap numbers already give nife a memory-throughput story; STREAM would be the standard vocabulary for the same claim |
| **HPCC (HPC Challenge)** | Bundles HPL (LINPACK, dense linear algebra), DGEMM, STREAM, PTRANS, RandomAccess | No usable Rust port found; HPL/DGEMM in particular assume a tuned BLAS, which is its own dependency question | Lowest priority. HPL alone is what most people mean by "the LINPACK number," but a from-scratch BLAS-backed solver is a different-sized project than this milestone |

**The sequencing this table implies**: NPB first (it is nearly free; the Rust already exists and
is published), STREAM second (small enough to write clean, and it lines up with milestone 138's
existing throughput work), HPCC/HPL last and possibly out of scope entirely unless a later
milestone wants to build or bind a BLAS.

## Why NPB-Rust specifically, and what porting it costs

NPB-Rust's own paper is unusually good evidence for exactly the question this project keeps asking
about Rust versus Fortran/C++ in HPC (milestone 64's "does `std` on the native ABI mean real crates
run here" applied to a whole benchmark suite instead of one crate): sequential Rust came out 1.23%
slower than Fortran and 5.59% faster than C++, geometric mean, across all eight benchmarks, with the
paper's own hypothesis tests behind the numbers. That is a citation this tree can point at rather
than a claim it has to make itself.

**What each kernel needs from nife's PAL, read off milestone 64's own gap list:**

- **EP** (embarrassingly parallel, floating point only) is the cheapest port: no file IO, no
  threads for the sequential build, pure computation. This is milestone 64's own "candidate probe...
  a pure-computation crate with no IO, to establish the floor," except the candidate already exists
  and is a recognized HPC benchmark rather than an arbitrary probe.
- **CG, FT, MG, IS** (the other four kernels) are still sequential-buildable with no thread
  dependency; they are irregular-memory and communication-pattern kernels that exercise nife's
  memory subsystem and (per milestone 138) its still-open read-gap work, which makes them a second,
  independent measurement of exactly the throughput claim 138 is already chasing.
- **BT, SP, LU** (the three pseudo-applications) are the largest and, per the NPB-Rust paper itself,
  the ones requiring `unsafe` blocks to bypass Rust's parallel-iterator ownership rules for their
  non-sequential dimension traversal; a real signal about what porting them to a capability system
  costs, since some of the same shapes that needed `unsafe` under Rayon may need it again here for
  different reasons (a capability system with no ambient shared mutable state is a different
  starting point than a thread pool over a flat address space).
- **The Rayon-parallel variants needed milestone 64's rank-3 fork resolved first, and it is now
  decided against them (§105, 2026-08-22): `thread::spawn` stays declined for want of a customer.**
  Rayon's work-stealing pool is exactly the kind of consumer that decision named as the thing staying
  out of scope. **Sequential NPB-Rust is buildable the moment `File::open`-free, thread-free `std` is
  enough** (which milestone 64 says EP-shaped code already is); the parallel variants are out of scope
  until §105 is revisited, not blocked on anything new this milestone introduces.

## STREAM: write it clean rather than take the GPL crate

The one Rust STREAM implementation found (`stream-benchmark` on crates.io) is GPL-licensed, which
decision 46's dependency rule does not forbid outright but does not fit this tree's normal
MIT/Apache-2.0 posture either, and the algorithm itself is short enough that decision 46's own test
answers cleanly: **is the spec the whole of correctness?** For STREAM, yes, four array operations
(Copy, Scale, Add, Triad) over a large enough working set to exceed cache, timed with the same
"1 shot at high resolution or a long loop at low resolution" tradeoff milestone 74 already
documents for cycle-accurate timing. Writing STREAM from its own published specification (McCalpin's
original paper defines the four kernels exactly) is a case for rule 4's "write when the spec is
complete and checkable," not rule 2's vendor-a-subsystem case.

**This also gives milestone 138's throughput work a standard unit.** 138's own numbers (5.13x faster
4 KiB reads after the record-level fix, the 128 KiB-per-4-KiB-request finding) are nife-specific
comparisons against ext4; a STREAM number is the figure an HPC reader already has a mental model
for, on this hardware, without reading any of this project's own notes first.

## The comparison this buys, concretely

**Same board, same binary logic, two kernels underneath it.** For each ported kernel:

1. Build the sequential Rust source unmodified (or with the smallest possible PAL-shaped patch) for
   both `x86_64-unknown-linux-gnu`/`aarch64-unknown-linux-gnu` and nife's own target.
2. Run at a matched problem class (NPB's Class A/B/C sizing already exists for exactly this: "a
   standard test problem," in the suite's own words, chosen so a small board and a workstation are
   comparing the same fixed problem rather than an arbitrary one each ran unsupervised).
3. Report wall time and, where milestone 74/147 land, cycle counts, in `notes/benchmarks.md`'s
   existing discipline (matched virtualization tier, same device, same noise controls milestone
   140's ext2 stratum already names).

**The honest framing, stated up front so it does not need correcting later**: this is not a claim
that nife outperforms Linux on HPC kernels; nothing about a capability microkernel with a young
filesystem stack and no vectorized math library predicts that, and milestone 138's own numbers show
real remaining overhead. **The claim is narrower and more useful: the same recognized, cited
benchmark suite runs on both, and the comparison is legible to a reader who has never heard of nife
before landing on this number.** That is a different kind of evidence than 21/25's existing primitive
benchmarks (syscall, IPC, context switch) provide, and it is evidence an HPC audience specifically
reaches for first, per every source this note's own research turned up (HPCC, NPB, and STREAM are
the three suites cited across every HPC benchmarking survey found).

**A concrete published baseline already exists for the board on calef's bench**, which makes this
milestone cheaper to run honestly than it otherwise would be: Brown, "RISC-V for High Performance
Computing" (CUG '25, ACM 3757348.3757367), Table 2, reports single-core NPB Class B performance
(the geometric mean of all five kernels and three pseudo-applications, in Mop/s) for six RISC-V
platform side by side; the **VisionFive V2's JH7110 scores 121.62 Mop/s**, about 25.7% of a single
SG2042 core's throughput. This number comes from the official NPB *Fortran* reference (not NPB-Rust),
run under GCC 13.2 on Fedora, and therefore is not directly comparable to a Rust port on the same
hardware under nife (language, compiler, OS kernel, and problem class all differ in ways that
matter). What it does buy is a **sanity boundary**: a Rust port on the same board running at
~120 Mop/s or above is consistent with the published hardware limit; running at, say, 12 Mop/s would
be a finding worth explaining. The paper also provides the same comparison for a single SG2042 core
(472.97 Mop/s), the Banana Pi F3 / SpacemiT K1 (146.83 Mop/s), and others, which means a reader who
lands on this milestone's eventual benchmark table can place it against the published RISC-V/HPC
literature without visiting a second source.

## What this does not decide

- **Whether NPB's parallel variants ever land on nife.** They were gated on milestone 64's
  `thread::spawn` fork, now decided against them for now (§105): out of scope until a real
  shared-memory-threading customer appears.
- **HPCC/HPL's fate.** A from-scratch BLAS is a project-sized undertaking on its own (dense
  linear-algebra kernels are exactly the "spec is the whole of correctness" boundary case decision
  46 draws for crypto in the other direction: LAPACK-grade numerics correctness is won by decades of
  exposure, not by reading BLAS's specification once), and this milestone deliberately does not
  scope that in. If HPL is ever wanted, it is its own milestone with its own dependency decision.
- **Whether to publish results as marketing before the comparison is honest.** The BUGS section
  below is deliberately specific about what would make a published number misleading.

## BUGS

- **A Rust-versus-Fortran comparison is not the comparison this project needs, and NPB-Rust's own
  paper is about the wrong axis.** The paper compares languages on one OS; this milestone needs the
  same language on two OSes. Reading NPB-Rust's numbers as "Rust HPC performance" and stopping there
  would answer a question nobody here is asking.
- **Nothing here is measured yet.** Every claim in this file is about what exists to build from
  (NPB-Rust's real existence, STREAM's short spec, HPCC's absence), not about a number produced on
  nife or on the boards this project owns.
- **The sequential-only scope is a real limitation for an "HPC" claim, not a stylistic choice.**
  HPC benchmarks exist to characterize parallel and distributed performance; a sequential EP run
  says something honest about single-core floating point and nothing about the multi-node or
  multi-core story any real HPC center actually cares about. Milestone 64's thread fork is now
  decided against building shared-memory threads for now (§105), which makes the parallel half's
  absence a scope decision rather than an open gap, and that should not be quietly forgotten once
  the sequential numbers look good.
- **No estimate of effort.** Porting NPB-Rust's sequential kernels is bounded by how much of `std`
  each one touches (milestone 64's own measurement method: build it, let the failures name the
  work), and this milestone has not yet run that measurement against any of the eight kernels.
