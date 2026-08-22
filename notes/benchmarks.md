# Benchmarks with teeth

*(Milestone 21. `script/bench`, `kernel/src/bench.rs`, and `bench/baseline-aarch64.txt`.)*

## Why two instruments

One tool cannot both *gate commits* and *tell the truth about magnitudes*, because the properties
that make each possible exclude the other:

| | icount (default) | HVF (`--real`) |
|---|---|---|
| what runs | TCG translation, `-icount shift=0,sleep=off` | the kernel, natively on the M-series core |
| virtual time | a deterministic function of instructions executed | the hardware counter, 24 MHz |
| numbers are | **exact and reproducible** (byte-identical runs, verified) | **real** (caches, TLBs, branch predictors are the host's) |
| numbers mean | path length; magnitudes are fiction (TCG models no caches, no TLB) | nanoseconds; determinism is gone (a desktop OS underneath) |
| job | regression gating: `--check` fails on >2% drift from the committed baseline | knowing what a path actually costs |

The gating story answers "identify the introduction of performance problems proximate to the
changes that introduce them" structurally: `bench/baseline-aarch64.txt` is committed, `--check` fails on
drift, and updating the baseline (`--save`) is a deliberate act made **in the commit that moved
the numbers**. The baseline's git history is the performance record, each delta beside its cause.

## What is measured

Five paths, the ones a microkernel lives on. Warmups run untimed; iteration counts are fixed and
recorded in the output, so a baseline is self-describing.

| bench | one iteration is |
|---|---|
| `yield_switch` | one voluntary yield in a two-thread ping-pong: two context switches |
| `ipc_rtt` | the classic number: send + recv round trip, two rendezvous, two wakes |
| `call_reply` | the one-endpoint service shape: mint a one-shot Reply cap, rendezvous, reply, consume |
| `spawn_reap` | thread lifecycle end to end: spawn, exit, reaped, table back to baseline |
| `map_new` | one fresh page into an address space: retype from the region, walk, leaf write |

## The exit trick

Semihosting does not work under HVF (the `hlt #0xf000` traps to the guest; xtask's `test()` has
known this since HVF support landed). So the bench kernel **never exits**: it prints
`bench: done` and parks in `wfi`, and `xtask bench`, which owns the QEMU child and reads its
output, kills it on the marker. One mechanism, both accelerators, and a forgotten bench QEMU
burns nothing while it waits (the `wfi` rule from CLAUDE.md).

Milestone 81 measured what that sentence had only asserted, and reused the trick: the trap raises a
real synchronous exception into the guest's own vector table (`EC 0x00`, Unknown reason), so a
kernel that *does* call `semihosting::exit` under HVF panics, and its panic handler takes the same
trap again, forever. The HVF test leg takes its verdict from the transcript for exactly this
reason. See notes/hvf-leg.md.

## The first real numbers, for the record (2026-07-23, M-series host, HVF, **debug build**)

IPC round trip ~705 ns; call/reply ~886 ns; yield round trip ~437 ns; spawn-to-reap ~2.8 µs;
fresh-page map ~634 ns. **These are debug-build numbers**, which is easy to miss and cost this note
a wrong comparison for twelve days (see the calibration section below). Statistical, single run,
shared machine: shapes, not gospel. The 24 MHz
counter grain (~42 ns) means per-iteration ticks are coarse; totals over 1000+ iterations are
what to read. Cycle-exact PMU numbers arrive with milestone 16's real silicon, which inherits
this harness and swaps the clock.

## Calibration: what these numbers mean next to L4's (corrected 2026-08-04)

IPC cost is *the* microkernel number because IPC multiplies through the whole architecture:
Mach's ~100 us IPC discredited microkernels in the 1980s, and Liedtke's L4 rehabilitated them
with ~250-cycle IPC on a 486, the "sub-microsecond" banner seL4's few-hundred-cycle fastpath
still carries. So this is the paragraph a reader quotes, and from 2026-07-23 to 2026-08-04 it was
wrong.

### The three errors in the old paragraph, and why they hid each other

The first version of this section converted `ipc_rtt`'s ~705 ns at an assumed 3.2 GHz to ~2,200
cycles and reported us "4 to 7 times heavier" than an L4-lineage fastpath's 300 to 600. Three
independent defects, and the reason nobody caught them is that they do not point the same way:

1. **Wrong plane.** `ipc_rtt` is kernel-side. Two kernel threads call `sched::ipc_send/recv`
   directly, in one address space, taking no trap. L4's published numbers are user-to-user across
   address spaces with the trap included. The benchmark that pays what theirs pays is
   `ipc_rtt_el0`, and it has existed since the EL0 primitive suite landed; this note reports it
   further down the page, and the calibration never followed. **Fixing this alone makes us look
   worse.**
2. **Wrong build.** ~705 ns is a **debug** figure. L4's are optimized builds, and the debug-to-
   release tax on the IPC path is ~6.7x (measured, in the cross-OS section below). **Fixing
   this alone makes us look better**, by more than the plane correction costs.
3. **Wrong convention, which is the one nobody had noticed at all.** seL4 publishes **one-way**
   costs, the call and the reply timed as separate operations. Ours is a **round trip**. Comparing
   our round trip against their one-way figure doubles the ratio for free.

Errors 1 and 3 inflate the ratio and error 2 deflates it, so "4 to 7 times heavier" landed in a
plausible-looking place by cancellation rather than by being right. That is the failure mode worth
remembering: an arithmetic chain over figures taken from different runs can be wrong in three ways
and still read as sober.

### One clean run, both planes (2026-08-04)

`cargo xtask bench --release --real`, Apple M3 (Mac15,3, 4 P-cores + 4 E-cores), HVF, `-smp 1`,
five boots back to back off one build. Host load average was ~3.2 of 8 cores throughout (other
agent lanes were active on this machine), which is not a silent machine; the run-to-run spread
below is the evidence that it did not matter much here. Both figures come from **the same boot** on
every boot, which is the property the old comparison lacked.

| bench | plane | ns/iter, median of 5 | the five | what one iteration includes |
|---|---|---|---|---|
| `ipc_rtt` | kernel-side | **46** | 46, 46, 46, 46, 49 | two rendezvous, one address space, no trap |
| `ipc_rtt_el0` | EL0 to EL0 | **350** | 347, 348, 350, 357, 369 | two rendezvous, two address spaces, four `svc`s |
| `null_syscall` | EL0 | 27 | 27, 27, 27, 27, 28 | one trap and return, for scale |

These agree with the 2026-07-29 refresh (~50 and ~361) to within run-to-run noise, so nothing has
moved; what was missing was never a measurement, it was the paragraph.

### The comparison, done on the right number

**Cycles here are arithmetic, not a reading.** HVF passes through no PMU (notes/pmu.md), so a cycle
count is nanoseconds times an *assumed* clock. The old paragraph assumed 3.2 GHz, which is not this
machine: the host is an Apple M3, 4.05 GHz on a P-core and 2.75 GHz on an E-core, and nothing pins
the QEMU vCPU thread to either. So the range, not a point:

| | ns | cycles at 2.75 GHz | cycles at 4.05 GHz |
|---|---|---|---|
| `ipc_rtt_el0` round trip | 350 | ~960 | ~1,420 |

seL4 publishes, for the same-core different-address-space path, **413 cycles for the IPC call and
426 for the IPC reply**, one-way each (sel4.systems/performance.html). Which machine that is matters
and the page does not put it up front: it is the **Jetson TX1, a Cortex-A57 at 1.9 GHz**, in seL4's
default configuration, and it is the *only* aarch64 platform the performance page carries
(notes/aarch64-board-survey.md, read 2026-08-13). A round trip in our sense is their call plus their
reply, **~839 cycles**.

**So the corrected figure is roughly 1.1x to 1.7x an L4-lineage round trip, not 4 to 7 times.** And
converting the *debug* EL0 number instead (~2272 ns) gives ~6,200 to ~9,200 cycles and a ratio of
~7x to ~11x, which is the same class of mistake in the other direction: a debug number in a release
comparison.

### Why that is not a win, and what is still not apples-to-apples

Recording a better ratio than the page used to claim is worth less than recording what it does not
mean, so the caveats are the substantive half of this section.

- **Cortex-A57 is 2015 silicon and the M3 is 2023.** A cycle is not a neutral unit across a decade
  of microarchitecture: the M3 is far wider and deeper, and an IPC path is mostly serially
  dependent loads, stores and unpredictable branches, exactly the workload a bigger out-of-order
  window helps. **A large part of the gap closing is the machine, not the kernel.** The only fix is
  the same kernel measured on comparable silicon, which is what milestone 24's board and milestone
  74's PMU are for.
- **Their fastpath is on and we have none.** seL4's published figures are its best case, with the
  `Call`/`ReplyRecv` fastpath enabled. Ours is the fully general path every time: scheduler lock,
  proved rendezvous, generational Tid checks. Read the ratio as "the general path is within a small
  factor of a tuned fastpath on this silicon", which is a statement about headroom, not about
  having matched them.
- **Their round trip is two syscalls and ours is four.** seL4 fuses send-and-wait into `Call` and
  reply-and-wait into `ReplyRecv`; our EL0 path issues `SEND`, `RECV`, `SEND`, `RECV`. At ~27 ns
  (~110 cycles) per trap, the two extra crossings are ~220 cycles, a sixth to a quarter of our
  round trip, and they are self-inflicted rather than structural. The kernel-side `call_reply` bench
  measures the fused shape, but **there is no EL0 twin of it**, so the structurally matched
  comparison to seL4's published pair is not currently measured at all. That gap is named here
  rather than papered over.
- **Different measurement methods.** seL4 times a single operation through the PMU with the caches
  hot and a measured overhead subtracted; we average a 5,000-iteration loop against a 41.67 ns
  counter tick. Both are legitimate (notes/pmu.md, the two-clocks section) and they do not fail the
  same way.
- **We run under a hypervisor and they do not.** The tax on this particular path is small (no
  devices touched, so essentially no VM exits; the cost is indirect, via stage-2 TLB pressure and
  host cache pollution), which is precisely why the bench loops keep devices out. Small is not zero.
- **The clock is assumed.** Every cycle figure above inherits a ~1.5x uncertainty from not knowing
  which core type the vCPU thread ran on. Milestone 74 (cycle counters) exists to retire this, and
  until it lands no cycle ratio from this note should be quoted tighter than "same order".

**The kernel-side number stays, and it is a real thing to know.** `ipc_rtt` at 46 ns (~130 to ~190
cycles) is the honest cost of the kernel's own rendezvous, and it is the right instrument for the
job it has: the icount tripwire gates on it, and a change to the kernel's IPC code moves it next to
its commit. It is not a comparison number. Putting it beside an 839-cycle round trip would compare a
path with no trap and no address-space switch against one that has both, which is the original error
of this section; deleting it would be a second error, because the gate needs it and the ~300 ns gap
between the two planes *is* the trap cost, which is itself a number worth having.

What the comparison legitimately supports, in one sentence: the Mach failure mode is nowhere in
sight, the general path is viable at this price, and whether a fastpath is ever worth its complexity
is a question for these measurements rather than for L4 envy.

### Where the two nanosecond figures for `ipc_rtt` came from

This note has carried **~705 ns** (2026-07-23) and **~951 ns** (2026-07-25) for the same kernel-side
benchmark, which reads as a contradiction and is not one. Both are debug, both are single runs, and
they are different binaries on different days: the 19f object-capability refactor landed between
them and genuinely grew the scheduler and thread hot paths, on top of the whole-crate codegen drift
this note documents below. Neither was wrong when it was written. **The defect was leaving both on
the page with no dates and no build profile attached**, which is what let a comparison quote the
older one for eleven days. Every figure in this note now carries its build; that is the fix.

## What the icount instrument cannot see

Cache misses, TLB behavior, branch prediction: TCG models none of them, so a change that is
count-neutral but cache-hostile passes `--check` silently. That is the known limit, stated in
the roadmap block too; the `--real` numbers are the net that catches what counts cannot, read by
a human rather than a gate.

## A correction: the counts drift across builds, so "attributable to the commit" was too strong

The original milestone-21 note said a count change "is a change in a code path, attributable to the
commit that made it." Building the EL0 primitive suite disproved the attribution half, and the
machine's verdict is worth writing down (milestone 25 folds in the fix).

icount is deterministic **per binary** (byte-identical runs, verified twice). It is **not** stable
across different binaries. Adding the `null_syscall_el0` bench (which touches no other benchmark's
code) moved `yield_switch` -7% and `ipc_rtt` +1.8% at the same time. Two controlled facts pin the
mechanism:

- A **dead** function added to `bench.rs` moved nothing. So it is not raw code *layout* (addresses
  don't change instruction counts anyway).
- The shifts are **non-uniform and opposite in sign** across benchmarks. So it is not a common-mode
  offset that could be subtracted out.

What is left is the compiler's **whole-crate decisions**: adding live code that calls into shared
functions (`sched::spawn`, `user::run`) changes inlining and monomorphization elsewhere, so *other*
functions' executed-instruction counts move, each its own way. Mixed into the session's drift was
also a **real** increase from the 19f object-capability refactor (the scheduler and thread hot paths
genuinely grew); the point is that the instrument cannot separate that from the codegen churn.

**The fix (milestone 25):** demote `--check` from a 2% gate to a **coarse 10% tripwire**. It still
catches a gross regression ("you 3x'd IPC"), which is real value, but it no longer pretends to
attribute a 3% wiggle to the commit in front of it. The **`--real` medians, read by a human, are the
fine signal**, and a few-percent codegen shuffle is already in their noise. Ideas we did *not* take,
and why: pinning hot-path layout (fragile, and layout was not even the cause); per-operation deltas
that cancel fixed overhead (the shift is in the measured body, not fixed overhead, so it would not
cancel); common-mode subtraction (the shifts are not common-mode). Recorded here rather than quietly
re-baselined, because the machine overruled the claim.

## Compute vs. OS primitives: two benchmarks that measure different things (milestone 19e)

The microbenchmarks above are the *right* kind for a microkernel: IPC, context switch, the paths a
microkernel lives on. But "run a real workload" (19e) wanted a whole compute program, and thinking
through how to compare it across OSs turned up a distinction worth pinning down, because it decides
what any cross-OS comparison can and cannot show.

**Compute is OS-independent.** A tight compute loop, once it is running in userspace, does not touch
the OS: the CPU executes the same instructions no matter who scheduled it. So a compute benchmark
(CoreMark, Dhrystone) run on nife, macOS, and Linux on the same core comes out *nearly
identical*, and the small gaps are compiler codegen or allocator noise, not OS quality. That is a
real result ("we add no hidden compute overhead") but a null one by design. It cannot show OS
strengths or liabilities, because the OS is not in the loop.

**OS primitives are where an OS shows itself.** Syscall entry, context switch, IPC round-trip, page
map, page fault, thread spawn: these *are* the OS, and they are what distinguish Linux from macOS
from us. But the same source cannot measure them across three OSs, because "the same syscall" does
not exist on all three: you invoke each OS's own primitive (`getpid` on Linux, a Mach/BSD call on
macOS, our `svc` null-invoke). So the OS-revealing benchmark is a **matched harness per OS** (one
metric definition, three native implementations), which is exactly what lmbench is and how the
L4/seL4 papers compare to Linux. Our own microbenchmarks above are the nife side of it.

### The CoreMark workload (`crates/coremark`, `user/src/coremark.rs`)

19e's real workload is CoreMark, the three work items of a CoreMark iteration (a linked-list sort, a
small-matrix multiply, a state machine over a byte buffer), each folded into a CRC so the compiler
cannot delete the work and a run self-validates. It runs as a spawned EL0 program against the native
ABI: init builds the `"coremark"` binary, grants it one endpoint, and it computes and SENDs the run's
CRC home. `coremark::PINNED_CRC_64` (`0x7954` for 64 iterations) is asserted by both the host crate
test and the kernel test, so the same computation gives the same answer on the host and on the
kernel's target, which is the property a cross-OS comparison rests on.

It is a **Rust reimplementation, not EEMBC-certified CoreMark**: a certified score needs the
unmodified reference C. The Rust choice buys the thing that matters for *our* comparison, that the
identical source compiles for nife, macOS, and Linux, so the compute run is one program on
three OSs. This binary reports correctness, not yet a score; timing a run needs a userspace clock
(enabling the EL0 virtual-counter read, as Linux does for its vDSO), which lands with the cross-OS
suite rather than here.

### The measurement plane: kernel-side (gating) vs EL0 (cross-OS)

A subtlety that decides comparability, found while starting the primitive suite. The microbenchmarks
at the top of this note run in **kernel context**: the bench threads are kernel threads calling
`sched::yield_now` and `sched::ipc_send/recv` directly, so they measure the kernel-internal path
length of each operation. That is exactly right for their job (regression gating: a code-path change
moves the count next to its commit). But it is **not** what lmbench measures. lmbench runs a
*userspace* program making real syscalls, so its numbers include the EL0→EL1 trap and return that a
kernel-side benchmark skips entirely.

So the cross-OS primitive numbers have to be measured **from EL0**, a userspace program that self-
times a loop of real `svc` syscalls, to be comparable to lmbench. That is why milestone 19e opened
EL0 access to the virtual counter (`CNTKCTL_EL1.EL0VCTEN`; `user_rt::now`/`cntfrq`; notes/abi.md):
userspace self-timing is the prerequisite for a fair comparison. The CoreMark workload is the first
program to use it, self-timing its run and reporting `[crc, ticks, freq]`; the EL0 primitive
benchmarks (null syscall, context switch, IPC round-trip, page map, all measured the lmbench way)
build on the same `user_rt::now`. The existing kernel-side suite stays, for gating; the EL0 suite is additive, for
cross-OS honesty. The two will differ by roughly the trap cost, and that difference is itself a
number worth having.

### The first EL0 numbers (nife, M-series host, HVF, debug build)

The `os_primitives_benchmarker` program (`user/src/os_primitives_benchmarker.rs`), spawned by the bench boot, self-times each primitive
from EL0 and reports it as a normal bench line. So far:

| primitive | HVF ns/iter | what one iteration is |
|---|---|---|
| `null_syscall` | ~42 | one `svc` that the kernel rejects immediately: trap + dispatch + return |
| `ctx_switch` | ~692 | one `SYS_YIELD` to a peer *process* and back: two switches, address space included |
| `ipc_rtt_el0` | ~2272 | a `SEND` to a server process and a `RECV` of its reply: two rendezvous, four `svc`s |
| `map_el0` | ~909 | `invoke(aspace, MAP_INTO, va, frame, RO)`: trap + cap resolve + walk + PTE + record |

Two sanity checks pass. A context switch is ~16x a null syscall (two traps, the scheduler, two
register save/restores, and a TTBR0/ASID change, versus one bare trap). And the round trip lines up
against its parts: ~two context switches (2 × 692) plus four traps (4 × 42) plus dispatch ≈ 2272.

The EL0 round trip also has a kernel-side twin, the milestone-21 `ipc_rtt` (~951 ns in this same
2026-07-25 debug run; the ~705 ns from 2026-07-23 is a different debug binary, see the calibration
section), which measures the same rendezvous *without* the EL0↔EL1 crossings. The ~1.3 µs gap between
them is exactly the trap cost of the four `svc`s a real round trip pays, which is the reason the EL0
numbers, not the kernel-side ones, are what compare to lmbench. **All debug builds, and every figure
in this subsection is one**; the cross-OS comparison and the L4 calibration both want release builds
on all sides, and quoting a debug figure into either is the mistake the calibration section above
records. These line up against lmbench's `lat_syscall` / `lat_ctx` / `lat_pipe` and `sel4bench`.

**Map (lmbench's `lat_mmap`) behaves differently from the other three, and it is the primitive where
the honest answer is a tie, not a win.** It taught three things.

First, it *consumes resources per call*: every `MAP_INTO` writes a page-table entry and a revocation
record, paid from the target space's untyped region, so unlike a null syscall or a yield it cannot loop
forever. The loop is bounded (500 maps, one L3 table's worth); the kernel-side twin `map_new` maps 64.
And there is no unmap in the surface yet, so each VA is used once.

Second, the debug and release numbers diverged by ~10x, far more than any other primitive, and that
divergence is the whole lesson. `map_el0` **aliases one existing frame** at every VA, so it does no
page allocation and no zeroing: it is trap + capability resolve + walk + PTE write + a `record_mapping`
append. That append scans the head log page for a free slot, an ~128-entry linear walk on average, and
in a debug build that unoptimized scan *dominated* the number (~909 ns). Release compiles the scan down
to almost nothing, and the true cost of the mapping mechanism shows through: **~91 ns**. The kernel-side
`map_new`, by contrast, is ~524 ns in release and barely moved from debug, because its cost is the 4 KiB
**page zeroing** a fresh frame needs (`retype_page` hands back a zeroed page), which is memory-bandwidth
bound and the optimizer cannot speed it up.

Third, and this is why map is a tie: **`map_el0` and the host `lat_mmap` do not measure the same thing.**
The host number is a first-touch page fault, which allocates and zeroes a fresh page; `map_el0` aliases
a frame and skips both. So `map_el0` ~91 ns is the *pure mapping mechanism*, and it is genuinely lean,
but it is not comparable to Linux's ~534 ns, most of which is the page zeroing our aliasing avoids. The
apples-to-apples comparison is our `map_new` (fresh page, allocate + zero + map), ~524 ns, plus one trap
(~28 ns) for the EL0 crossing the host's fault includes: ~552 ns, against Linux ~534 ns and macOS ~556
ns. That is a **three-way tie**, and it makes sense: page provisioning is dominated by zeroing 4 KiB,
which is the same silicon and the same bandwidth for all three. nife's lean mechanism is real (the
91 ns), but on the operation an application actually pays for, getting a usable page, it does not and
cannot win, because the win would have to come from zeroing memory faster than the other two, and nobody
can. A fair EL0 map that *does* provision a fresh page waits on retype-from-untyped reaching userspace
(a later milestone); until then the kernel-side `map_new` is the honest stand-in for the comparison.

### The first cross-OS numbers (nife vs Linux vs macOS)

`bench/host/` holds the host side of each metric: `null_syscall.rs` (a raw `getpid` through the
syscall gate, not libc's cached `getpid` which never traps), `ipc_rtt.rs` (a pipe round trip between
two forked processes, lmbench's `lat_pipe`), `ctx_switch.rs` (the derived context switch),
`mmap.rs` (first-touch fault-in, lmbench's `lat_mmap`), and `spawn.rs` (fork+exit, lmbench's
`lat_proc`). Two ways to run them: natively on macOS
(`rustc -O ... && ./bin`), and on **Linux at the same tier** as nife, `bench/host/run_linux.sh`
cross-compiles a static musl binary (`linux_all.rs`, the five metrics combined), packs it as `/init`
in a one-file initramfs, and boots it under QEMU-HVF, the exact machine nife boots on. So Linux
and nife sit on the **same M-series core at the same virtualization tier**; native macOS is the
bare-metal ceiling.

Run nife optimized (`cargo xtask bench --release`, which builds an opt-level-3 kernel and
userspace and implies `--real`), and compare on the same core:

| metric | nife **release** (HVF) | Linux (static musl, HVF) | macOS/XNU (native) |
|---|---|---|---|
| null syscall | **~27 ns** | ~139 ns | ~76 ns |
| context switch (per switch, derived) | **~28 ns** | ~415 ns | ~818 ns |
| IPC round trip | **~337 ns** | ~1723 ns | ~2620 ns |
| map a fresh page (provision + map) | ~552 ns (`map_new` + trap) | ~534 ns | ~556 ns |
| map mechanism only (aliased, no zeroing) | ~91 ns (`map_el0`) | n/a (fault always zeroes) | n/a |
| spawn (build + run + reap + reclaim) | **~7.7 µs** (`spawn_el0`) | ~19.7 µs (fork+exit) | ~291 µs (fork+exit) |

**nife wins four and ties one, and saying which is which is the point.** Same M-series core, same
HVF tier as Linux, both optimized. It is **~5x faster than Linux at the null syscall** (27 vs 139) and
**~5x faster at the IPC round trip** (337 vs 1723), it beats native macOS at both, and it builds a
process faster than either (spawn, below). These are seL4-class microkernel numbers, an IPC round trip
in the low hundreds of nanoseconds, next to the reference OS on the same silicon. **"seL4-class" is a
claim about magnitude and nothing more**; what it is worth measured against seL4's own published
cycles, and the four ways that comparison is not apples-to-apples, is the calibration section above.
Quoting this sentence without that one is how the last overstatement happened. **Map is a deliberate
non-win**: provisioning
a page is dominated by zeroing 4 KiB, which is bandwidth-bound and identical across the three, so all
land near ~550 ns. The lean mapping *mechanism* (91 ns, measured by aliasing to strip the zeroing) is
real and worth recording, but it is not a page an application can use, so it does not go in the win
column. The map row above compares like with like (`map_new` provisions a fresh page, as the host fault
does); the ~91 ns sits below it as the mechanism floor, not as a headline.

**Spawn is a real win, and an honest caveat.** `spawn_el0` builds a whole child from EL0 (`SPLIT` a
region, retype an address space and a TCB, map code and a stack, configure, start), runs it to exit,
reaps it, and `DESTROY`s its region, all in a self-timed loop that only repeats because object
revocation reclaims each child (notes/object-revocation.md). At ~7.7 µs it beats Linux `fork`+`exit`
(~19.7 µs) by ~2.6x and macOS by ~38x, on the same core, and it does so while paying **more** boundary
crossings than Unix: ~10 `svc`s per spawn against `fork`+`wait`'s two. That the heavier-trapping side
still wins is the honest part of the result. The caveat is the operations differ: `fork` **duplicates**
the parent (its address space copy-on-write, its descriptor table, its signal state), where nife
**builds a fresh minimal process from nothing**. A capability-microkernel process is a lighter object
than a Unix one, so the gap is mostly that structural difference, not a faster version of the same work.
We use `fork`+`exit`, not `fork`+`exec`, precisely to keep the Unix side as light as it gets (no binary
loaded); it still carries the weight of duplication that nife's from-scratch build does not. The
number stands, with its meaning stated: building a process is cheap when a process is a small thing.

The **context switch** is the softest of the three and its number the least load-bearing. No OS lets
you time a bare switch, so it is *derived*: on the host, `bench/host/ctx_switch.rs` measures a
two-process pipe round trip (two switches plus two pipe passes) and subtracts a self-pipe pass (a
`write`+`read` with no switch), leaving one switch = `round_trip/2 - self_pipe`. nife's
`ctx_switch` bench is a yield round trip (two switches plus two `SYS_YIELD`s); subtracting the trap
(`~2 x null_syscall`) leaves ~28 ns per switch. The subtraction is approximate and the *mechanisms
differ* (our lightweight yield versus a pipe pass), so read the ~15x gap to Linux as directional, not
exact. It points the same way the other two do, and that consistency, three metrics, three methods,
all favoring the minimal kernel, is the real signal.

The story the debug build told first was the *opposite* at IPC, and the gap between them is the whole
lesson. Debug nife: null syscall ~42 ns, ctx switch ~692 ns, IPC ~2272 ns. So `-O0` was a ~1.5x
tax on the bare syscall (which still won) but a **~6.7x tax on IPC** (which lost to Linux at 1723 ns
until this). The heavier a path, the more the optimizer matters, and the IPC path, two context
switches plus four traps plus the rendezvous, is heavy. The null-syscall win survived the debug
handicap; the IPC win was hidden by it. Measuring both builds is why we can say which.

Honest caveats remain. A semantic one for IPC: our endpoint is a synchronous three-word rendezvous, a
Unix pipe is a buffered byte stream through a kernel buffer, so this is our native IPC against Unix's
*standard* IPC (`lat_pipe`), not XNU's fastest (a Mach port would likely beat the pipe). And the host
context switch still wants lmbench's ring method to isolate cleanly. `sel4bench` (the one peer that
would tell us how close to the state of the art these numbers are) is the remaining comparison.

### seL4: built and booting, but stopped by the PMU wall (deferred to real hardware)

`sel4bench` was built (seL4 kernel + the benchmark app suite, for `qemu-arm-virt` aarch64, `RELEASE`
and `FASTPATH` on, i.e. seL4 at its best) and it boots on this Mac under both QEMU-TCG and QEMU-HVF.
It **cannot produce valid numbers here**, and the reason is worth recording because it is the same
constraint the roadmap called out for our own silicon-cycle plans.

sel4bench times a **single operation** per sample (one `seL4_Call`, `RUNS` samples) and reads the
**PMU cycle counter**, `PMCCNTR_EL0`, before and after (notes/pmu.md explains the PMU and why it is the
counter that does not survive virtualization). That needs a real, high-resolution cycle counter
(~0.25 ns per tick at ~4 GHz). Neither virtualization mode on this host provides one:

- **QEMU-TCG** does not model a cycle counter; `PMCCNTR` returns quantized junk (we saw 0 and 1000),
  and sel4bench's own stability check refuses to continue ("*Benchmarking overhead of a call is not
  stable*").
- **QEMU-HVF** on Apple Silicon does not virtualize the guest PMU, so `PMCCNTR` is unstable there too,
  and the same check stops the run.

The only counter HVF passes through is the architected virtual counter, `CNTVCT_EL0`, at the host's
24 MHz `CNTFRQ`, which is **41 ns per tick**, far too coarse to resolve one ~50 ns IPC in a single
shot. (`CONFIG_ALLOW_UNSTABLE_OVERHEAD` forces sel4bench past the check, but then the numbers are the
same junk, so it buys nothing.)

**This validates our own measurement design rather than undermining it.** Our bench works under HVF for
exactly the reason sel4bench does not: we read `CNTVCT` (which HVF passes through) and we time a **loop
of thousands** of operations per sample, so the coarse 41 ns tick is averaged away. sel4bench's
single-shot-PMU method is precisely what cannot survive this virtualization tier. Getting a same-machine
seL4 number would mean either rewriting sel4bench to our method (CNTVCT plus batched loops, real surgery
on its measurement core) or giving it a real PMU.

**So the seL4 comparison is deferred to real hardware**, which also aligns with the planned second-board
port (design/roadmap/24-second-aarch64-board.md): a Raspberry Pi has a real PMU, sel4bench runs on it natively, and
it is the board nife is heading toward anyway. The build recipe, reproducible when a Pi is on hand
(rebuild with the Pi `PLATFORM` instead of `qemu-arm-virt`), via the official seL4 Podman image:

```
podman pull docker.io/trustworthysystems/sel4        # ~3.6 GB, bundles repo/cmake/ninja/aarch64-gcc
mkdir sel4bench && cd sel4bench
podman run --rm -v "$PWD":/sel4bench:Z docker.io/trustworthysystems/sel4 bash -lc '
  cd /sel4bench
  repo init -u https://github.com/seL4/sel4bench-manifest.git && repo sync -j4
  mkdir build && cd build
  ../init-build.sh -DPLATFORM=qemu-arm-virt -DAARCH64=TRUE -DSIMULATION=TRUE   # -DPLATFORM=rpi4 for a Pi
  ninja'
# image at build/images/sel4benchapp-image-arm-qemu-arm-virt; run with build/simulate (qemu) or on the Pi
```

### The cross-OS comparison, when we build it

- **Reuse an existing primitive suite** where one exists: **lmbench** on Linux and macOS (it builds
  on both), **`sel4bench`** for seL4. We write the nife side (the microbenchmarks above,
  extended to match the metric set), not the whole thing.
- **The peers.** seL4 is the direct one: a capability microkernel that targets the *same* QEMU
  `aarch64 virt` machine we do, so it runs on the identical instrument (QEMU-HVF) and publishes
  comparable cycle counts. L4Re/Fiasco and Genode are more effort for less marginal insight.
- **Match the virtualization tier.** QEMU with `-accel hvf` *is* virtualization (Hypervisor.framework
  on the real core), not emulation, so nife and Linux run virtualized under QEMU-HVF; macOS runs
  as a guest under Apple's Virtualization.framework (same underlying hypervisor, different VMM shell);
  native macOS is the bare-metal ceiling reference. For guest-internal microbenchmarks the VMM layer
  is off the hot path (no VM exit on a null syscall or context switch), so the QEMU-vs-VZ difference
  is a footnote, not a confound.
- **XNU is a hybrid, name it.** macOS's kernel has a Mach microkernel core but runs BSD and drivers
  *in* the kernel, so most macOS syscalls are in-kernel BSD calls and Mach IPC is not on the hot path
  the way our endpoints are. Comparing "our IPC" to "macOS syscall latency" measures two different
  things; saying so is part of the honesty.

## 2026-07-28: the day `--check` failed on every primitive, and why it was the harness

Roughly eight merges landed on `main` in one day (milestone 32 phase 1 block writes, 16b IOMMU, 28
line discipline, 27 std, 30 net in three stages, 31 capability shell, and 22 phase A's fault
endpoint with the DESTROY force-kill amendment). None ran `bench --check`, because bench is not in
the `script/test` gate. When the dust settled, `--check` failed IMPROVED on four primitives and
REGRESSED on two, all far past the 10% tripwire:

| primitive | old baseline (smp=4) | HEAD (smp=4) | reported delta |
|---|---|---|---|
| call_reply | 1,876,614 | 965,679 | **-49%** |
| spawn_reap | 1,769,595 | 175,185 | **-90%** |
| ctx_switch | 6,308,105 | 3,648,857 | **-42%** |
| ipc_rtt_el0 | 21,364,834 | 11,950,853 | **-44%** |
| map_el0 | 408,897 | 575,752 | **+41%** |
| spawn_el0 | 3,132,783 | 3,633,897 | **+16%** |

Improvements that large and that uniform are suspicious on their face; a real -90% on spawn is not
something eight unrelated merges hand you for free. Bisecting the merge points (one icount run each,
deterministic) turned up the tell straight away: **coremark, which is pure compute and touches no OS
primitive, moved +63%** at the capability-shell merge (20.9M to 34.1M ticks), and `spawn_reap` did
not creep, it *teleported*, reading 1.77M at the base, 172k three merges later, 3.5M one merge after
that. A compute loop cannot legitimately move 63% because a kernel merged a socket API. The numbers
were not measuring what they claimed to.

### Root cause: the aarch64 icount bench ran `-smp 4`, and CNTVCT is global under icount

The bench reads `CNTVCT_EL0` (`arch::timer::now()`) around each loop. Under `-icount shift=0` all
vCPUs share **one** deterministic virtual-instruction clock, so that counter advances with the
*global* instruction stream across every hart, not just the core running the benchmark. The aarch64
runner defaults to `-smp 4` (`NIFE_SMP:-4`, matching the SMP tests), and the bench never overrode
it. So each measured window silently counted three other harts: their idle loops, and, worse, under
`-icount` an idle secondary hart parked in `wfi` **jumps virtual time forward to the next timer
tick**, dumping a large quantized lump of ticks into whatever window happened to be open. Add the
load-balanced spawner (a thread that spreads children across cores) and the count for `spawn_reap`
or `ipc_rtt_el0` becomes a function of how four harts happened to interleave, which any code change
perturbs. The result is deterministic per binary (so `--check` "worked" and the old baseline looked
stable), but it is not the path length of the primitive. It is the machine's four-hart idle pattern,
sampled.

The proof is a re-run at `-smp 1`. Single hart, the counter advances only with the bench thread, and
the same four commits that swung wildly at `-smp 4` go flat:

| primitive | baseline | iommu(16b) | cap-shell(31) | HEAD | smp=1 spread |
|---|---|---|---|---|---|
| coremark | 20,914,947 | 20,913,678 | 20,913,678 | 20,913,678 | ~0.006% |
| spawn_reap | 166,860 | 170,952 | 170,952 | 175,890 | +5.4% |
| ctx_switch | 2,664,204 | 2,679,734 | 2,680,270 | 2,685,272 | +0.8% |
| ipc_rtt_el0 | 9,603,751 | 9,694,986 | 10,005,385 | 10,101,111 | +5.2% |
| call_reply | 956,768 | 956,769 | 956,893 | 963,080 | +0.6% |
| spawn_el0 | 1,574,632 | 1,609,770 | 1,750,084 | 1,754,438 | +11% |
| ipc_rtt | 861,095 | 864,935 | 861,346 | 922,720 | +7.2% |
| null_syscall | 427,706 | 457,705 | 457,705 | 457,705 | +7.0% |

coremark is invariant to five decimal places, which is the sanity check the smp=4 run failed. The
`-42%` to `-90%` improvements and the `+41%` regression were **entirely the smp=4 artifact**; they do
not attribute to any merge's code because they are not code, they are the four-hart interleaving that
the old baseline froze one sample of and today's merges reshuffled.

### This was a known bug on one ISA and an unfixed one on the other

The riscv bench path already pins `NIFE_SMP=1`, with a comment describing this exact failure
("a `wfi` jumps virtual time to the next timer tick, inflating the spawn primitives to
timer-quantized nonsense"). That fix landed with the riscv icount bench (commit 494514b) and was
never mirrored to aarch64. So the aarch64 icount instrument has been measuring four-hart noise since
milestone 21; the old baseline was noise too, internally consistent enough to pass `--check` until a
day of merges moved the interleaving far enough to trip it. **The fix is one line**, the aarch64
icount path now sets `NIFE_SMP=1` like riscv, and the baseline is re-saved at single hart. Real
per-core magnitudes still come from `--real` (HVF), where each core keeps its own counter and
parallel harts do not inflate elapsed time, so SMP there is not a confound.

### What actually moved, once the noise is gone

At `-smp 1` every primitive is within ~11% of the old (contaminated) baseline's *intent*, and the
movement that is real is small and mostly explained:

| primitive | true delta (smp=1) | cause | assessment |
|---|---|---|---|
| ipc_rtt | +7.2% | the IPC mailbox widened 3 words to 5 (milestone 22 §26, the fault-message carrier); every `ipc_send`/`ipc_recv` now copies five words via `wide()` | real, small, expected; the step lands exactly at the M22 merge (cap-shell 861k to HEAD 923k) |
| ipc_rtt_el0 | +5.2% | same mailbox widening, on the EL0 path | real, small |
| spawn_el0 | +11% | the M31 SPLIT rights-inheritance change (child budget gets full delegable rights); spawn_el0 does a SPLIT + retype per iteration | real, small; the step lands at the cap-shell merge (1.61M to 1.75M) |
| null_syscall | +7.0% | one-step at the blk-write/iommu merges, then flat; kernel layout/codegen drift in the syscall entry path, not a redesign | codegen drift, in the noise the note below already documents |
| spawn_reap, map_new, map_el0, call_reply, ctx_switch | +0.6% to +5% | whole-crate codegen churn across eight merges | codegen drift, expected and sub-tripwire |
| coremark, yield_switch | ~0% | pure compute / tight kernel yield, no structural change | invariant, as they should be |

None of these needed a merge reverted or a path investigated. The only defect the episode exposed was
the harness itself. No bench was measuring fiction in the sense of an elided loop or an early exit,
the loops all still do their work; the fiction was the *counter*, reading four harts where the
benchmark meant one. The new baseline (smp=1) is the first aarch64 icount baseline that measures the
primitive rather than the machine, and it now agrees in shape with the riscv one.

## The one bench that is legitimately multi-hart (DECISIONS §28, the placement win)

Every primitive above is hart-pinned, and it has to be: the icount instrument boots `-smp 1`, because
under `-icount` all vCPUs share one virtual clock and an idle hart's `wfi` jumps that clock forward
(the 2026-07-28 finding above). So the deterministic suite measures per-core path length and is blind,
by construction, to §28's whole job: spreading work across the four harts. The `smp_*` benches
(`kernel/src/bench.rs::smp_throughput`) are the one measurement that shows it, and their methodology is
different on purpose, so this section is where the difference is written down.

Run them with `script/bench --real --smp` (HVF, 4 harts). Plain `--real` is single-hart on purpose
(per-core primitive magnitudes; see the refresh section below), so `smp_throughput` self-skips there.

**They never gate, and never touch `bench/baseline-aarch64.txt`.** Two structural reasons. First, they run
only when `online_count() > 1`, which is only the `--real --smp` boot; under the icount instrument
(`-smp 1`) and the default single-hart `--real` run, `smp_throughput` returns immediately, so no
`smp_*` line is ever emitted there and the committed baseline never sees them (verified: `--check`
output has no `smp_*` rows). Second, a wall-clock throughput number is not even defined under `-icount`
(one shared clock), and TCG serialises all vCPUs onto one host thread, so there is no real parallelism
to measure. Only HVF gives each core its own counter and genuine concurrent execution. These are
statistical HVF magnitudes read by a human with loose bounds, exactly like the other `--real` numbers,
not a tick baseline.

**Two workloads, because they tell opposite and both-true stories.**

| bench | workload | one batch |
|---|---|---|
| `smp_compute_*` | N independent CPU-bound grinders, no syscalls | `solo` = 1 worker; `all` = 16 workers, each the same fixed grind |
| `smp_pipe_*` | N independent synchronous IPC ping-pong pairs | `solo` = 1 pair; `all` = 16 pairs, each 2000 round trips |

The scaling factor for either is the `solo` throughput divided by the `all` throughput, i.e. read it
from the totals (`iters / ticks`), not from the coarse `ns/iter` column. `smp_cores` records the
ceiling (4 on this boot).

**Compute scales, ~3.5x on 4 cores, and that is the §28 placement win.** Numbers (HVF, release,
min-of-4 batches, five boots):

```
smp_compute_solo   ~8,886 ticks / 300,000 iters      (one core's grind rate)
smp_compute_all   ~40,000 ticks / 4,800,000 iters    (16x the work, across the machine)
```

Sixteen workers is sixteen times the work; run one at a time it would take `16 x 8,886 = 142,176`
ticks, and it finishes in ~40,000, a **3.5x speedup** (≈89% of the 4x ceiling). The lost ~11% is real
and expected: 16 does not divide into 4 waves cleanly (the last wave runs four workers where earlier
waves were full), plus spawn, reap, and the barrier. A CPU-bound worker makes no cross-core wake once
placed, so the host keeps every busy vCPU on a real core, and what is left to measure is exactly
placement filling the machine. This is the number no hart-pinned primitive can show.

**Synchronous IPC pipelines do NOT scale under HVF, they go slightly backwards, and the reason is the
host, not the scheduler.** Numbers (same conditions):

```
smp_pipe_solo   ~2,900 ticks / 2,000 rtts    (~59 ns/round trip, one warm core, all local)
smp_pipe_all  ~250,000 ticks / 32,000 rtts   (~322 ns/round trip aggregate)
```

The aggregate per-round-trip is *slower* than a single pair's, a ~0.18x "speedup". That looks alarming
until you see why, and the why is a virtualization property. A single pair, with the other three cores
idle and the main thread blocked, co-locates by §28's local-wake rule and does every rendezvous on one
warm core, no cross-core traffic at all, so it runs at the `ipc_rtt` rate (~59 ns). Sixteen pairs get
scattered across the cores by placement, and whenever placement or stealing splits a pair across two
cores, its next rendezvous is a cross-core wake, an SGI to a vCPU the host has descheduled because the
guest looked idle a moment earlier. Waking a descheduled vCPU costs host reschedule latency that the
co-located pair never pays. So the IPC-heavy parallel workload spends its time in HVF's wake path, not
in the kernel. This is the **same** reason the icount suite is pinned to one hart and the same reason a
same-machine seL4 number is deferred to real hardware: the instrument underneath, not nife, sets
the ceiling. On real silicon with four dedicated cores and no descheduling, the pipelines would scale
the way compute does here; measuring that is a real-hardware follow-up (milestone 16), and the bench is
already written to report it when the wakes are cheap.

Getting the solo baseline honest took one correction worth recording, because it is the same class of
error as the smp=4 counter bug. The first version had the main thread **busy-yield** on a done counter
instead of blocking on a `RECV`. A runnable main plus the pair is three threads the scheduler scatters,
so even the *solo* pair took cross-core wakes and clocked ~60x slower than `ipc_rtt`'s identical pair,
and the derived scaling came out **superlinear** (greater than the core count), which is not physical.
Blocking the main thread (the `ipc_rtt` shape) fixed it: solo returned to the ~59 ns rate and scaling
fell back under the ceiling where it belongs. A non-physical speedup is a bug in the measurement, never
a win; it went in the bin, not the baseline.

## 2026-07-29: real-magnitude refresh on settled main (HVF, release), and the per-core default

The recorded `--real` magnitudes above predated the §22/§26/§27/§28/§30/§31/§32 wave, so they were
rerun on settled `main`. Two harness changes came out of it, and they are the frame for the numbers.

**`--real` is now single-hart by default.** A primitive magnitude is a per-core number, and the
cross-OS table reads it that way (against Linux `fork`, lmbench, seL4, all per-core). The wave made the
default `--real` boot `-smp 4`, and the machine showed why that is the wrong default for a primitive:
the reap-heavy ones inflate and go noisy under cross-core reap lag that has nothing to do with per-core
cost. `spawn_el0` reads **~4.4 us on one hart and ~13.6 us on four** (and swings widely there);
`spawn_reap` is ~1.3 us on one hart and 11-160 us on four. So `--real` now pins `-smp 1` like the icount
instrument, for the same reason, and `--real --smp` boots the whole machine for the throughput bench
above. The single-hart run is the per-core signal; the four-hart run is for scaling, not for reading a
primitive's latency.

**The refreshed per-core numbers** (HVF, `--release`, `-smp 1`, medians of 5 boots, ns/iter):

| primitive | 2026-07-29 (per-core) | previously recorded | what moved, and why |
|---|---|---|---|
| `null_syscall` (EL0) | ~27 | ~27 | unchanged |
| `ipc_rtt_el0` (EL0) | ~361 | ~337 | **+7%**, the milestone-22 §26 mailbox widening 3->5 words; matches the icount +5% exactly, real and expected |
| `ctx_switch` (EL0, round trip) | ~112 | ~28/switch (~56 rt) | ~29 ns/switch derived, unchanged |
| `map_el0` (mechanism, aliased) | ~92 | ~91 | unchanged |
| `map_new` (provision + map) | ~470 | ~524 | within run-to-run noise; still zeroing-bound |
| `spawn_el0` (EL0, build+run+reap+reclaim) | ~4,400 | ~7,700 | **lower**, see below |
| `spawn_reap` (kernel-side) | ~1,300 | ~2,800 (debug) | lower; the old figure was a debug single-run |
| `ipc_rtt` (kernel-side) | ~50 | ~705 (debug) | the gap is the debug->release tax, not a change |
| `call_reply` (kernel-side) | ~66 | ~886 (debug) | same, debug->release |
| `yield_switch` (kernel-side) | ~32 | ~437 (debug) | same, debug->release |
| `coremark` (per iteration) | ~8,700 | n/a | pure compute, invariant across the wave (the smp=4 artifact check) |

Two lines need a word.

`ipc_rtt_el0` is the one clean, real movement: **+7%**, and it lands exactly where the icount baseline
put it (+5%), which is the §26 fault-message carrier widening the mailbox from three words to five so
every send and recv copies five. Small, expected, paid for a feature, and the two instruments agree,
which is the cross-check working.

`spawn_el0` reads **lower** now (~4.4 us) than the recorded ~7.7 us, and honesty demands the caveat
rather than a victory lap. The icount path length for spawn_el0 rose ~11% over the wave (the §31 SPLIT
rights inheritance), so this is **not** a path-length speedup. It is that spawn is the noisiest
primitive (it reaps a child every iteration) and the recorded 7.7 us was a single, busier-machine
sample; the settled per-core median is ~4.4 us with low variance, and the same primitive is ~13.6 us at
four harts. Read 4.4 us as the refreshed stable per-core figure, not as a claim that spawn got faster.
The cross-OS story is unchanged either way: still faster than Linux `fork`+`exit`, with the "a
capability process is a lighter object than a Unix one" caveat that has always stood.

Nothing here needed a path investigated. The only structural change was the harness (`--real` boots
one hart now), and the one real code-attributable movement (`ipc_rtt_el0` +7%) is the mailbox, agreeing
across both instruments.

## The service-path benchmarks: what a userspace-server architecture costs (2026-07-29)

The microkernel bet is that filesystems, network stacks, and drivers belong in confined userspace
processes, not the kernel. The skeptic's fair question is the price: a request that a monolith
serves with one syscall now crosses into another process, maybe through a third. Two benches answer
it, and the split between them is the honest part, because the two servers this project actually
runs sit on opposite sides of a measurement line.

**`relay_rtt`: the confined-server tax, isolated and gated.** Real services fan out: the FS server
CALLs the block server (`client -> fs -> blk -> fs -> client`), net_stack CALLs the NIC driver
(`client -> net_stack -> driver -> net_stack -> client`). `relay_rtt` (kernel-side, `bench.rs`) is exactly that
two-hop topology, a client through a relay to a backend and back, and it sits on the icount baseline
next to the one-hop `ipc_rtt`:

| bench | topology | icount ticks/iter |
|---|---|---|
| `ipc_rtt` | client <-> server (one hop) | ~982 |
| `relay_rtt` | client -> relay -> backend -> relay -> client (two hops) | ~1,961 |

The two-hop path is ~2.0x the one-hop, and the **difference, ~980 ticks, is what one confined
intermediary that delegates to a backend costs**: two extra context switches and two extra
rendezvous per request. That is the architecture's per-request tax over a monolith, isolated from any
device, deterministic, and gated by `--check` so a regression in the IPC/switch path shows up against
its commit. Adding `relay_rtt` shifted the other kernel-side IPC benches a few percent (`ipc_rtt`
+6%) through whole-crate codegen, all sub-tripwire, the churn this note documents above; the baseline
was re-saved to absorb it in the commit that added the bench.

**`broker_rtt`: what the queue broker costs when both ends are up** (milestone 23, DECISIONS §41).
The same question one rung up. Milestone 23's latency ladder has two rungs built, and this is the
number that makes "opt-in per channel, never the default" a rule rather than a preference.

The **default rung has no benchmark of its own, because it has no cost of its own**, and that is the
milestone's headline rather than a dodge. A client holds a capability to a stable *endpoint*, and
whoever is parked in `RECV_CAP` on it answers; a swap changes who that is. No process stands in the
data path, so the steady state is `call_reply` exactly, and the swap adds nothing to it. The kernel's
own sender queue is what buffers the down window: requests that arrive while nobody is receiving park
there and the replacement drains them.

The **opt-in rung** is `broker`, interposed so a producer never blocks on an absent consumer. It is
the same client and the same backend as `call_reply` with a process in between, so the difference is
the whole tax:

| bench | topology | icount ticks/iter (aarch64) | HVF ns/iter |
|---|---|---|---|
| `call_reply` | client <-> server, one endpoint (**and the swap's steady state**) | ~1,007 | ~1,172 |
| `broker_rtt` | client -> broker -> backend -> broker -> client | ~2,010 | ~2,368 |

**1.99x, and about 1.2 microseconds of real time per request on this laptop.** RISC-V agrees to
within a percent (169,282 vs 338,000 ticks/1000, 2.00x). That is the price of decoupling two
components' lifecycles, and it is why the broker is wired per channel: paying it on every IPC would
trade the project's measured round-trip advantage for a feature used during swaps. It sits on the
icount baseline for both ISAs so a regression surfaces against its commit, like `relay_rtt`.

Two honest notes. `broker_rtt` and `relay_rtt` measure the same shape (one confined intermediary) in
the two different idioms the codebase actually uses, `CALL`/`Reply` and `SEND`/`RECV` pairs, and they
land within 2.5% of each other, which is a small cross-check that the Reply-capability path costs
about what a pre-wired reply endpoint does. And the broker's *pass-through* is what is measured here:
during a down window it does strictly less work per request (one rendezvous, an enqueue, an immediate
answer), which is the point, but it is not the number to quote, because the steady state is where a
channel spends its life.

Adding this bench shifted the other kernel-side numbers by a couple of percent through whole-crate
codegen (`spawn_reap` -2.3%, `ipc_rtt_el0` +0.3%), all sub-tripwire, the churn this note documents
above; both baselines were re-saved in the commit that added it.

**`fs_read`: the real RedoxFS read, whole path, and why it cannot be the isolated number.** This is
the flagship: a client opens a file through a granted **directory capability** and reads a block, over
the real confined stack (a block server driving the RedoxFS disk by DMA, the vendored RedoxFS engine
mounting it over blk IPC on its own heap). It runs on the `--real --smp` boot, where the whole stack
is proven by the fs_server test, and it reports:

```
fs_read   ~9.8M ticks / 2000 reads   ~204 us/read   (HVF, --release --smp, stable across runs)
```

**204 microseconds is device latency, and saying so is the point.** A read is not served warm from a
cache; it goes to the block server, which does a DMA transfer and waits on the disk's completion
interrupt, ~200 us per block under HVF. That swamps the FS-server's own IPC-contract tax (the extra
`client -> fs` hop and the engine's dispatch), which `relay_rtt` puts at a few hundred *nanoseconds*.
So `fs_read` is the honest **whole-path** cost of a userspace file read, not an isolated server tax,
exactly the case milestone 21's rule names: when device latency swamps the isolation, measure the
whole path and say so rather than report a fictional isolated number. The clean isolation of the file
server's own cost was attempted and abandoned for this reason: a warm cache read and a raw blk-IPC
read differ by that few-hundred-ns layer sitting on top of a ~200 us block read with its own
run-to-run spread, so the delta is in the device noise. The isolated per-hop tax lives in `relay_rtt`
instead, where it is measurable; `fs_read` is what a real file read actually costs, dominated by the
disk the way it would be on any OS. And it is `--real`-only and never gated for the same reason the
number is large: the mount and every read are interrupt-driven, not deterministic under `-icount`, so
gating on `fs_read` would enshrine the non-determinism the 2026-07-28 lesson warns against. It
self-skips (the `online_count() > 1` gate) everywhere but `--real --smp`, so `bench/baseline-aarch64.txt`
never sees it.

**net_stack's socket round trip: measured, but not as a third icount bench, and here is why.** The net
path has the same shape as the FS path (a confined server the client reaches only through a granted
`Stack` capability), and its per-request IPC tax is the same `relay_rtt` topology. But a net_stack
*socket* round trip is even less gate-able than `fs_read`: net_stack only reaches its serve loop after a
DHCP handshake, and its RECV path drives smoltcp's own retransmit and delay-ACK timers (notes/net.md),
so the path is DHCP- and timer-driven, deterministic under neither `-icount` nor, at the socket level,
even a warm HVF loop. So net_stack's socket contract is proven and timed end to end by the existing net
tests (`a_client_resolves_dns_through_the_socket_contract`, `a_client_echoes_over_tcp_...`, both ISAs,
both transports), not duplicated as a bench that could only report device-and-timer latency. The bare
EL0 round trip those build on, `ipc_rtt_el0` above, is the raw baseline; the `relay_rtt` delta is the
confined-server tax net_stack pays on top of it, the same as the FS server. Recording it this way, one
gated topology tax plus the two real servers measured where each is sound, is the honest fit to what
the two instruments can and cannot see.

## 2026-07-29: both baselines re-saved for one instruction on the exception-return path

Milestone 22 phase B.2 fixed a real race in the exception-return path (notes/exceptions.md: staging
`SPSR_EL1`/`ELR_EL1`, or `sepc`/`sstatus` on RISC-V, is not atomic with respect to a nested exception).
The fix is one instruction at the top of the restore, masking interrupts, and it therefore lands on
**every return from an exception**, which is the hottest path either instrument measures.

The movement is well under 1% and both baselines were re-saved in the commit that caused it, per the
milestone-21 discipline. aarch64 `null_syscall` 457503 -> 458753 ticks over 20000 iterations (+0.3%),
`ipc_rtt_el0` +0.2%, `ctx_switch` +0.1%; RISC-V shows the same order, mixed in sign (`null_syscall`
+0.3%, `ipc_rtt` -2.2%), which is the build-to-build icount drift the 2026-07-28 attribution section
already documents rather than anything the change did.

Worth stating plainly, because it is the kind of number that invites a wrong conclusion: this is not a
regression that was traded for correctness. One masked-interrupt instruction per exception return is
what the *correct* version of that path always cost, and the previous numbers were measuring a version
that could return a new process to its entry point at the wrong exception level.

Measured-boot (phase B.1) moved nothing on either ISA, which is expected: the bench boot enters no boot
program, so the SHA-256 over init never runs there.

## 2026-08-04: the RISC-V baseline re-saved for a win this instrument cannot see

Milestone 58 removed the unconditional `sfence.vma` from every RISC-V context switch. That is a full
TLB flush per switch, gone. The numbers went **up**:

| benchmark | before | after | delta |
|---|---|---|---|
| ctx_switch | 471,827 | 477,635 | +1.2% |
| ipc_rtt_el0 | 1,738,256 | 1,766,199 | +1.6% |
| yield_switch | 179,097 | 179,217 | +0.07% |

**This is the clearest case yet of icount measuring the wrong thing, and it is worth keeping as the
worked example.** icount counts guest instructions retired. A TLB flush is *one instruction*; its
entire cost is the misses that follow, and QEMU's TCG refills its softmmu TLB with host-side work
that retires no guest instructions at all. So the instrument charged us for what we added and
credited us nothing for what we removed.

What we added, per switch: an atomic load and a branch for the ASID-width gate, and a `csrr satp`
plus a compare for the "already installed?" early return `switch_user_root` gained (aarch64 has had
it since milestone 15, and it fires on every switch between two kernel threads, which is most of them
on an idle machine). Three or four instructions, traded for not throwing the TLB away.

The baseline was re-saved in the commit that caused it, per the milestone-21 discipline. aarch64 was
left alone: the only aarch64 change in that milestone is a `dsb ishst` at the top of `flush_asid`,
which runs at address-space teardown and not on any measured path, and its numbers moved by less than
the run-to-run drift already documented above.

**The number that would settle it needs hardware with a real TLB.** `--real` runs under
Hypervisor.framework, which executes the host's own ISA, so there is no accelerated RISC-V leg to
take it on; the VisionFive 2 is where this gets measured. Recorded here rather than deferred silently,
because a milestone whose stated win is a benchmark improvement and whose benchmark got slower is
exactly the result that has to be written down.

## 2026-08-15: `map_new` moved 15.6% on RISC-V, and the movement was a bug rather than a cost

Every section above this one is about the instrument measuring a *cost*: a correctness fix that had
to be paid for, or a win it could not see. This is the first time it caught a **defect**, and it did
so on a benchmark nobody was looking at, in a change whose entire test suite was green.

Four pull requests from the VisionFive 2 lane family failed `script/bench --riscv --check` on the
same benchmark by the same amount, and a fifth from the same stack did not:

| tree | riscv64 `map_new` | note |
|---|---|---|
| `main` | within `2362 ± 236` | its push run executed the riscv leg rather than skipping it |
| #172, #173, #175, #176 | **2731 to 2732** (+15.6%) | tolerance is `±236`, so this fails by a wide margin |
| #178 | **2362, exactly** | `kernel/src/smp.rs` byte-identical to #176's |

The stack's change routes every per-cpu loop through `smp::online_cpus()` and masks the RISC-V
RFENCE calls with `online_harts_mask()`, so a machine whose online set is `{1,2,3}` stops being
indexed as `0..count` (see notes/visionfive2.md and `crates/cpu_set`). **The bench boots a single
hart.** On one hart the online mask names one cpu, the shootdown has nobody to send to, and the
correct instruction count for the masked version is the count for the unmasked one. 2362 is the
right answer and 2731 is not.

### The first reading was wrong, and it is kept because it is the plausible one

+370 ticks over 64 iterations is **5.8 instructions per map**, which is exactly the shape of "consult
a mask instead of a count on every TLB flush". That reading says the regression is the price of the
correctness fix, and it leads directly to the action `script/bench` itself recommends on failure:
rerun with `--save` and commit the new baseline with the change that moved it.

What refuted it is the third row of the table. **#178's `kernel/src/smp.rs` is byte-identical to
#176's**, and #178 measures 2362. A cost carried by the code cannot be absent from a tree that
contains the same code. So the extra instructions are not the mask being consulted; they are work the
mask should have prevented and did not, on the trees that lack the later registration fix. The best
available reading is remote RFENCEs being issued on a single-hart machine, from a mask that
over-reports which harts are online.

**Rebaselining would have been the expensive mistake, and it was one command away.** It would have
written 2731 into `bench/baseline-riscv64.txt`, and every future run of the tripwire would then have
been silent on precisely the defect it had just caught. That is worse than never having had the
check: a green tripwire is read as evidence, and this one would have been evidence of nothing.

### What is proven, what is inferred, and what would settle it

**Proven.** The four trees measure 2731 to 2732 and #178 measures 2362, against a baseline `main`
still satisfies. The `smp.rs` files are identical. All of it is from CI's own runs rather than
computed or scaled, per the discipline the 2026-08-04 riscv64 re-save established.

**Inferred.** That the delta is remote RFENCEs fired against an over-reporting mask. Nothing here
counted a fence. The evidence for it is that the number returns to the baseline **exactly** rather
than approximately, which is what a path that stops executing looks like and not what a cheaper path
looks like.

**What would settle it**: count RFENCE issues on a single-hart boot, or print `online_harts_mask()`
at bench time on both trees and compare. Neither is built, and this is recorded as a reading rather
than a mechanism until one of them is.

### Why this is the strongest argument yet for the job's wall-clock

Nothing else noticed. `build + test (host + QEMU)` passed on #176. So did `cpu matrix (riscv64
across QEMU CPU models)`, which exists specifically to boot RISC-V across CPU models. The kernel
worked; it just did more than it needed to, on the one configuration where the extra work is
provably unnecessary. A correctness bug that leaves behaviour correct is invisible to every test in
the tree by construction, and an instruction counter is the only instrument here that can see it.

Milestone 21's stated purpose for the tripwire is catching "the *introduction* of performance
problems proximate to the changes that introduce them". This is the same mechanism catching
something better, and the case is worth citing the next time the job's five minutes come up for
debate.

## 2026-08-17: the RFENCE probe was built, and it refuted the reading above

The section above ends by naming what would settle its inference: "count RFENCE issues on a
single-hart boot, or print `online_harts_mask()` at bench time on both trees and compare. Neither is
built, and this is recorded as a reading rather than a mechanism until one of them is." Both are
built now (`arch::riscv64::remote_fence_count`, the `bench-probe:` lines beside `map_new`, and
`rfence_self`), and the answer is not the one the reading predicted.

### What the probe measures

| tree | `map_new` | remote fences in the timed window | `online_harts_mask` | `rfence_self` |
|---|---|---|---|---|
| `main` (53ca491) | 2362 | **0** | `0x1` | 9.81 ticks/call |
| pre-fix (593c00e) | 2361 | **0** | `0x1` | 8.91 ticks/call |
| **PR #176's head** (f601c6b) | **2362** | **0** | `0x1` | 8.91 ticks/call |

The third row is the one that matters. **That is the tree CI measured at 2731**, fetched from
`pull/176/head` and run with the probe cherry-picked onto it, and here it measures the baseline
exactly, issues **zero** remote RFENCEs across `map_new`, and reports a mask with one bit set. The
mask does not over-report and there are no fences to be the cost.

### The arithmetic error that made the wrong reading plausible

The section above computes "+370 ticks over 64 iterations is **5.8 instructions per map**". **A tick
is not an instruction.** The bench counter is `rdtime` at the machine's timebase, which on QEMU's
`virt` is ~10 MHz, and `-icount shift=0` makes one instruction one nanosecond of virtual time. So
one tick is **~100 instructions**, and the delta was ~577 instructions per map, not 5.8.

That kills the reading the section called "the plausible one" from the other direction than it
thought. Consulting a mask instead of a count is a handful of instructions, which is ~0.05 ticks and
invisible at this resolution; it was never a candidate for a 5.8-tick move. The section rejected it
for the right reason (a cost cannot be absent from a tree containing the same code) while its stated
arithmetic was two orders of magnitude out.

### What one RFENCE actually costs, and why it does not fit either

`rfence_self` prices the firmware path by naming this hart in the SBI hart mask, a legal call the
firmware serves with a local fence. It costs **8.9 to 9.8 ticks**, so one extra remote RFENCE per
map would have moved `map_new` by ~570 ticks over 64 iterations. The observed move was 369, or
**0.65 fences per map**, which is not a whole number of anything.

### What is proven, what is not, and what to run next

**Proven.** Three trees, including the accused one, issue zero remote RFENCEs during `map_new` and
carry a correct single-bit mask. One RFENCE costs ~9 ticks. The 2026-08-15 inference, that the delta
was remote RFENCEs fired against an over-reporting mask, **is refuted for PR #176's tree.**

**The confound, and why it turned out to be small.** The three runs above are on **QEMU 8.2.2 with
`-device riscv-iommu-pci` removed**, because the QEMU available in that container is not the pinned
11.0.2 and does not implement the device. CI measured 2731 on 11.0.2 with the IOMMU present, so the
worry was that the regression is a property of the machine rather than of the branch.

**CI has since run this probe on the pinned machine and the worry mostly dissolves.** On QEMU 11.0.2
with the IOMMU present, `main` reports `map_new_remote_fences 0`, `online_harts_mask 0x1`, and
`map_new 2362`.

The interesting part is the pair of numbers either side of that machine change:

| | container (8.2.2, no IOMMU) | CI (11.0.2, IOMMU) |
|---|---|---|
| `map_new` | 2362 | **2362** |
| `rfence_self` | 8.91 ticks/call | **11.70 ticks/call (+31%)** |

**`map_new` is bit-identical across the machine change while the RFENCE benchmark moves 31%.** That
is a cross-check nobody designed and it is the strongest single piece of evidence here: a benchmark
made entirely of SBI calls is visibly sensitive to the firmware and emulator, and `map_new` is
completely insensitive to them, which is what a path that makes **no SBI calls at all** looks like.
The fence counter says zero and the machine-sensitivity says zero independently.

So the container's reading of `pull/176/head` at 2362 is very unlikely to be an artifact of the
machine, and the refutation stands rather than being provisional on it.

**What is now genuinely unexplained.** Not the mechanism, but the observation: what CI measured at
2731 to 2732 on four branches, reproducibly, in August. Nothing in this section explains it, and
there is no live hypothesis left. The remaining experiment that would speak to it is the probe run
against `pull/176/head` **in CI**, on the pinned machine, which is a thing a lane can do deliberately
and nothing does by accident. Until then the 2026-08-15 section's diagnosis should be read as
withdrawn rather than replaced.

### The part of the old section that survives intact

**Rebaselining would still have been the expensive mistake.** Everything above changes what the
delta *was*; nothing changes that writing 2731 into the baseline would have silenced a real
difference nobody had explained. The tripwire's value here was never the diagnosis, which was wrong.
It was that the number moved, refused to be quiet about it, and stayed unexplained until somebody
measured. That is what a tripwire is for, and it is the reading in the section above, not the
instrument, that this correction lands on.

## The kernel's memory footprint, and the cache question Mach got wrong (2026-08-17)

calef asked how large the kernel is in memory, then asked the better question: what should it be to
avoid the cache thrashing that made Mach slow and that L4 and seL4 were built to fix. The two
questions have different answers, and the gap between them is the point of this section.

Measured on `main` at 6e97bb1, release profile, both ISAs.

### The image, which is the number that does not matter

| | aarch64 | riscv64 |
|---|---|---|
| `.text` | 172,032 | 143,360 |
| `.rodata` | 32,768 | 36,864 |
| `.data` | 45,056 | 45,056 |
| `.bss` | 40,960 | 28,672 |
| **kernel proper** | **290,816 (284 KiB)** | **253,952 (248 KiB)** |
| `.secondary_stacks` | 557,056 | 557,056 |
| `.interrupt_stacks` | 163,840 | 163,840 |
| **total** | **1,011,712 (988 KiB)** | **974,848 (952 KiB)** |

The flat binary QEMU loads is **249,856 bytes** on aarch64 and **225,280** on riscv64, which is
exactly `.text + .rodata + .data` to the byte. That confirms the rest is `NOBITS`: reserved at
runtime, zero file bytes.

**The stacks are 70% of the total and are not a code-size fact.** `.secondary_stacks` is
`MAX_CPUS` (8) times `SECONDARY_STACK_SLOT` (64 KiB plus a 4 KiB guard) = 557,056, and
`.interrupt_stacks` is 8 times (`interrupt_stack::SIZE` 16 KiB plus a 4 KiB guard) = 163,840. Both
are reserved for eight cores whether eight exist or not, so a single-hart boot never touches seven
eighths of 704 KiB. Runtime allocation is separate again: page tables, and 28 KiB per thread for a
kernel stack (`thread::STACK_PAGES` is 6, plus a guard page).

### What Mach actually paid for, which is a different quantity

Liedtke's *On µ-Kernel Construction* (SOSP 1995) argued that Mach's IPC cost was dominated by the
**cache working set of the hot path**, not by anything inherent to microkernels. A kernel that
touches a lot of memory per IPC evicts the *application's* working set, so the cost appears as
capacity misses spread through the workload rather than as time spent in the kernel. L4 was written
against that: the original i386 kernel was around 12 KB of hand-written assembly, sized so the hot
path lived in L1. seL4 keeps the same idea as a deliberately maintained **fastpath** for the common
IPC case, which exists to bypass the general dispatch.

So the image size above is close to irrelevant to this question. `.text` that never runs during an
IPC costs nothing in cache.

### Our hot path, by symbol size (aarch64 release)

| on the path | bytes |
|---|---|
| `exception_dispatch` | 124 |
| `syscall::dispatch` | **2,024** |
| `sched::current_cap` | 376 |
| `sched::ipc_send` | 952 |
| `sched::schedule` | 1,240 |
| `sched::finish_switch` | 816 |
| **one-way send** | **5,532 (5.4 KiB)** |
| plus `sched::ipc_recv` (1,320) | **6,852 (6.7 KiB) round trip** |

`switch_to` and `dispatch_on_interrupt_stack` are assembly and report no symbol size, so they are
missing from the sum and the real figure is a little higher.

Data touched per round trip: two `TrapFrame`s (288 bytes, asserted in
`arch/riscv64/exceptions.rs`), two `Thread`s (744 bytes, from milestone 106's census), the endpoint,
and the run queue. Roughly **2 to 3 KiB, about 40 cache lines of 64 bytes.**

**`syscall::dispatch` is 37% of the instruction path on its own**, and it is a general decoder over
every object type and method. That is precisely the thing seL4's fastpath exists to skip, and it is
the first place to look if this ever needs to shrink.

### Which cache we are optimizing for, and why the constraint has loosened but not vanished

calef's point, and it is right in one direction and wrong in the other.

**Right: capacity grew.** Liedtke was writing against i486 and Pentium L1 caches of about 8 KB,
which is why a 12 KB kernel was a tight fit for the whole thing. The machines this project actually
runs on:

| machine | L1i | L1d | L2 | role here |
|---|---|---|---|---|
| SiFive U74 (VisionFive 2) | 32 KB | 32 KB | 2 MB | first silicon |
| Cortex-A57 (Jetson TX1) | 48 KB | 32 KB | 2 MB | milestone 127, the seL4 comparison |
| Apple M-series P-core | far larger | far larger | many MB | the bench and dev host |

That is four to six times Liedtke's budget on the small machines and far more on the host, so the
absolute room is genuinely larger.

**Wrong, or at least incomplete: the penalty grew faster than the capacity.** A main-memory miss in
1995 cost single-digit cycles against a slow clock; on a modern core it is a few hundred. Capacity
went up perhaps six times on the machines we care about while the cost of exceeding it went up by
considerably more. What changed in our favour is not headroom so much as **L2**: a large on-die L2
means overflowing L1 now costs tens of cycles rather than a trip to DRAM, which is a real safety net
that Liedtke's low-end targets did not have.

**These board figures should still be confirmed against the silicon**, and the TX1's are worth
taking from the machine when it arrives rather than from a datasheet summary.

### Where the frontier actually is, which sharpens the paragraph above (2026-08-18)

calef asked the obvious follow-up: where are frontier RISC-V, ARM and x86_64 caches now. Looked up
rather than recalled, because the paragraph above was written from memory and one of its claims does
not survive contact with the numbers.

| core | ISA | L1i | L1d | L2 (per core) |
|---|---|---|---|---|
| AMD Zen 5 | x86_64 | **32 KB** (8-way) | 48 KB (12-way) | 1 MB (16-way) |
| Intel Lion Cove / Cougar Cove | x86_64 | **64 KB** | 192 KB, plus a 48 KB L0 | 2.5 to 3 MB |
| Arm Cortex-X925 | aarch64 | **64 KB** (4-way) | | 2 or 3 MB, private |
| SiFive P870 | riscv64 | **64 KB** | | configurable, 4 MB in their example |
| Apple M4 / M5 P-core | aarch64 | **192 KB** | 128 KB | shared, several MB |

**The correction: L1i has not been growing.** The paragraph above says capacity grew four to six
times and implies the trend continues. It does not. Frontier L1i clusters at **64 KB**, Zen 5 is
still at **32 KB** and unchanged from Zen 4, and only Apple is an outlier at 192 KB. L1i is
latency-critical and area-expensive, so it has sat between 32 and 64 KB across every vendor for
roughly a decade. **What actually ballooned is L2**, from nothing or a small off-die cache in
Liedtke's era to 1 to 3 MB private per core today.

So the constraint loosened once, decades ago, and then stopped. The right reading is the one the
paragraph above reaches for and undershoots: the win is not L1 headroom, it is that **L2 turned an
L1 overflow from a DRAM trip into tens of cycles**. The fastpath discipline still applies to L1i,
and the number to respect there is 32 to 64 KB, not something that grows every generation.

**One figure was rejected rather than repeated.** A search result claimed the Ventana Veyron V2 has
512 KB of L1 instruction cache. No shipping core has an L1i anywhere near that, and it is
inconsistent with every other datapoint in the table, so it is almost certainly a garbled or
mis-attributed number. Recorded here as refused rather than silently dropped, because the next
person to look this up will hit the same result.

**This does not move the target.** The machines this project runs on are not frontier parts: the
U74's 32 KB L1i remains the binding constraint, and it happens to sit exactly at the bottom of the
frontier range anyway. A 4 KiB fastpath is about an eighth of that, a sixteenth of a 64 KB frontier
L1i, and a rounding error against Apple's 192 KB.

### The target

Expressed as a fraction of the **smallest L1i among machines we actually run on** (32 KB, the U74),
so it tracks the board list rather than a number somebody liked:

- **IPC fastpath instructions: under 4 KiB**, about an eighth of that L1i.
- **Data touched per IPC: under 1 KiB**, about 16 cache lines.
- **The whole-kernel image: no target at all.** Optimising it would be optimising the wrong thing.

We are at 5.4 KiB and roughly 40 lines, which is the right order of magnitude and not comfortable.
The reasoning behind the fraction is Liedtke's rather than a round number: the constraint is not
that the kernel fits, it is that the kernel leaves most of L1 intact for the application, because
cache pollution is what Mach actually charged its users.

### Tracking it: `script/fastpath-footprint`, built 2026-08-18

*Milestone 132 owns this gate; design/roadmap/132-the-fastpath-footprint.md carries the reasoning,
the BUGS and the trigger that would turn the gap below into scheduled work.*

The paragraph this replaces proposed a gate and named the thing blocking it: **which symbols the hot
path is**, given assembly with no symbol sizes and inlined callees with no symbols at all. That
question is answered below and the gate exists.

**The mechanism.** `script/fastpath-footprint` walks the call graph out of the disassembly, exactly
as `script/stack-depth-check` already does for stack chains, and reports two numbers per ISA:

- **`ipc_fastpath`**, the transitive closure of non-cold calls from the IPC and switch roots
  (`ipc_send`, `ipc_recv`, `schedule`, `finish_switch`, `current_cap`).
- **`syscall_entry`**, the trap vector plus the exception dispatcher plus `syscall::dispatch`,
  summed **flat with no closure**. A syscall traverses one path through a decoder, so closing over
  `dispatch` would pull in every object and method in the ABI and measure the syscall surface rather
  than this path. Its own bytes are on every syscall, so they count; its other arms are not, so they
  do not.

Gated at **5% growth** against `bench/fastpath-<arch>.txt`, tighter than the icount tripwire's 10%
because these numbers are static: icount drifts when the compiler remakes inlining decisions for
unrelated reasons, where a symbol size moves only when the code moves.

### The numbers, on `main` at 2026-08-18

| | aarch64 | riscv64 |
|---|---|---|
| `ipc_fastpath` (closure, 9 symbols each) | 5,780 | 5,074 |
| `syscall_entry` (flat) | 4,168 | 2,692 |
| **total, an upper bound** | **9,948 (9.71 KiB)** | **7,766 (7.58 KiB)** |

The closure's aarch64 members: `ipc_recv`, `schedule`, `ipc_send`, `finish_switch`, `wake`,
`current_cap`, `kmem::recycle`, `memcpy`, `switch_to`. Every one of them is defensible as something
an IPC round trip actually runs, which is the test the root list has to pass.

**Against the target above, we are over it.** The target is a fastpath under 4 KiB, an eighth of the
U74's 32 KB L1i; `ipc_fastpath` alone is 5.6 KiB and the total with entry is 9.7. That is the gap
the gate now holds still while somebody decides whether to close it, and `syscall::dispatch` at
2,024 bytes remains the largest single item and the obvious candidate, being exactly what seL4's
hand-written fastpath exists to skip.

**One finding worth keeping, because it is what made the number honest.** Closing naively from
`finish_switch` returned **11.2 KiB**, because that function's reap branch drags in
`KernelStack::drop`, `untyped::destroy`, `revoke_region`, `delete_frame_caps` and the unmap path.
Those run when a thread *exits* and never during an IPC. Classifying the teardown family as cold
took the figure to 5.6 KiB. A gate shipped at 11.2 would have been measuring thread death and
calling it IPC, and would have been quiet about a doubling of the real path.

### BUGS

- **It is an upper bound, not a footprint.** Whole symbol sizes are summed, so a cold tail parked at
  the end of a hot function counts even though its cache lines are never fetched. The direction is
  deliberate: a tripwire that over-counts consistently still catches growth.
- **Indirect calls are invisible**, the same blind spot `script/stack-depth-check` records. A call
  through a function pointer contributes nothing to the closure.
- **The riscv64 tail instruction is assumed to be 4 bytes.** That ISA mixes 2- and 4-byte
  instructions, so the last instruction of each symbol may be over-counted by two bytes. Conservative,
  and lost in the noise at this scale.
- **The cold list is a judgement, and a wrong entry is silent.** If a symbol that an IPC really does
  reach ever matches the cold pattern, it drops out of the number with no warning. The list is in the
  script with a reason per family for exactly this reason.
- **It is not a cache measurement.** See below.

### What cannot be measured yet, and the machine that changes it

**Every number above is static footprint.** It bounds the problem and does not measure it. The
icount instrument models no caches at all (this file says so in its own opening), and the HVF runs
are on the one machine whose L1 is large enough to hide the effect entirely.

**The TX1 is the machine where this becomes measurable**, and the convergence is worth noting: 48 KB
L1i, a real PMU, and it is already the platform whose published seL4 numbers milestone 25 compares
against. Milestone 74's cycle counters plus PMU cache-miss events on that board would turn this
section from arithmetic into a measurement, taken next to the kernel it is being compared with.

## Filesystem throughput, and what is honestly comparable (milestone 38, 2026-08-18)

DECISIONS §34's condition 2, and the question its block puts plainly: **does the userspace-server
architecture cost throughput once the device dominates?** The FS server has had a per-request number
since milestone 32 built it, `fs_read` at ~204 us, and no MB/s figure at all, so the objection a
microkernel skeptic actually presses had never been answered with a measurement.

**The answer is no, and it is not close.** The confined-server tax is about a microsecond per
request (`relay_rtt`, above). A 4 KiB file operation through this stack costs one and a half to
three and a half *milli*seconds. The architecture is three and a half orders of magnitude below the
thing being measured, and it could be free without moving any figure on this page.

**A sharper answer replaces it, and it decomposes onto one constant.** Every 4 KiB read of an
ordinary file costs **32 block reads**, because RedoxFS stores a file in 128 KiB records and reads
the record whole. One block read through our confined block server costs ~46 us, which is **the same
as Linux's** on the same device at the same tier. So our block path is at parity and the entire gap
is one design constant in the store we vendored.

### What one transfer is, and why that is most of the story

A `fs_proto` `READ` or `WRITE` moves its payload through the **one page the client shares with the
server**, so 4096 bytes is the protocol's ceiling per request. Every figure below is therefore a
request rate wearing throughput's clothes, and any system whose client may pass a 64 KiB buffer is
being asked an easier question. That is priced explicitly in the ext4 table below, the same way
bench/host/pipe_throughput.rs prices `pipe_16` against `pipe_64k`.

The workload is 256 transfers of 4 KiB, so 1 MiB per phase, over one file, in four phases:
sequential write (which is also the file's creation), sequential read, random read, random write,
plus two phases that exist to decompose the others. Offsets in the random phases come from a
fixed-seed xorshift64, page-aligned and inside the file, so the sequence is identical on every run
and on every system. 1 MiB is small for a throughput benchmark and is bounded by the 16 MiB fixture
image rather than chosen.

**Two properties of RedoxFS had to be defeated before any of this measured anything**, and the first
draft of the bench fell into both:

- `Transaction::write_node` compares before it writes, so a write whose bytes match what is already
  there does nothing at all. A benchmark sending one constant page reported random writes as fast as
  random reads, because none of them were writes.
- Records are 128 KiB and are **lz4-compressed**, so 32 identical pages inside one record compress
  to almost nothing. An incompressible page is not enough; each page has to differ from the last.

So every write carries a freshly generated, incompressible page. That costs 512 stores, it is inside
the timed window because a client that writes data has to produce the data, and it is measured on
its own (`fs_payload_fill`) so a reader can take it back out: **820 ns per page, 0.03% of a write**.
Both host benchmarks generate their payload the same way for the same reason.

### The three systems, and which two are at the same tier

| | what runs | tier |
|---|---|---|
| **nife** | `cargo xtask bench --release --real --smp`; the FS server on RedoxFS over blk IPC | QEMU-HVF, `virt`, `-cpu host`, `-m 256M`, `-smp 4`, `virtio-blk-device` on a raw host image |
| **Linux / ext4** | `bench/host/run_linux_fs.sh`, a static PID 1 in a one-file initramfs | **the same**: same machine model, same `-cpu host`, same memory, same core count, same device model, same raw image file, same QEMU default `cache=writeback` |
| **macOS / APFS** | `bench/host/macos_fs.rs`, natively | **not matched**: no virtualization, no virtio, the real NVMe. A reference ceiling, not a competitor |

The first two differ in the filesystem and in nothing underneath it, which is the point of the work
that went into the second row. The third is here for the reason milestone 25 kept native macOS in
the primitive comparison, and carries the same warning: it says what the hardware can do when
nothing is in the way, and any ratio against it measures the tier as much as the filesystem.

**Getting Linux there was most of the work and is worth recording**, because the obvious way does
not work. Alpine's `virt` kernel builds neither ext4 nor `virtio_blk` in; both live in
`modloop-virt`, a squashfs. So `run_linux_fs.sh` lifts seven modules out of that squashfs (with
podman, since macOS has neither `mke2fs` nor `unsquashfs`) and the bench loads them itself with
`finit_module` before it mounts anything. Two failed boots, one answering ENODEV and one ENOENT, are
what that paragraph cost.

### nife: the four phases, and the two that decompose them

Median of the four rounds that passed the noise control (below), all on 2026-08-18, aarch64 release
under HVF on four harts. Run-to-run spread in the last column, and it is small enough that the
figures mean something.

| bench | ns per 4 KiB | MiB/s | spread | = block reads |
|---|---|---|---|---|
| `fs_seq_write` | 2,566,304 | **1.52** | 15% | 55.5 |
| `fs_seq_read` | 1,509,270 | **2.59** | 4.5% | 32.6 |
| `fs_rand_read` | 1,484,338 | **2.63** | 2.2% | 32.1 |
| `fs_rand_write` | 3,427,674 | **1.14** | 6.5% | 74.1 |
| `fs_record_read` (decomposer) | 1,479,768 | 2.64 | 3.5% | 32.0 |
| `fs_read` (control; a 69-byte inline read) | 206,902 | n/a | 0.7% | 4.5 |
| `fs_payload_fill` (the client's own page fill) | 820 | 4,764 | 3.4% | 0 |

The last column is the whole analysis and it is arithmetic on one measured constant: **46.2 us per
4 KiB block through the confined block server**, taken from `fs_record_read` divided by the 32 blocks
a 128 KiB record holds. Everything else falls out of it and nothing was fitted:

- **A read is 32 blocks, flat.** Sequential, random and record-aligned reads agree to within 3%,
  which is also the proof that there is no cache and no readahead anywhere in this path: `IpcDisk`
  is a bare `Disk` with no `DiskCache` around it.
- **`fs_record_read` was added to show the opposite and refuted itself**, which is why it stayed.
  The prediction was that a read at the start of a record would fetch one block where a read at the
  end fetches 32, since `read_node_inner` asks for `BlockLevel::for_bytes(offset_in_record + len)`.
  It measures the same as the others because `read_record` reads the block the pointer *stores* and
  only then checks the requested level: a fully written record is stored at level 5, so every read
  fetches all 128 KiB.
- **An inline read is 4.5 blocks**, which is the tree walk with no record at the end of it. `motd` is
  69 bytes and lives inside its node.
- **A sequential write is 55 blocks and a random write is 74.** A write is a read-modify-write of a
  copy-on-write record: the random case reads 32, writes 32, and spends the rest on the tree and the
  allocator, while the sequential case is cheaper because it is *growing* the record from level 0.

### Linux and ext4, at the same tier

Five rounds on a quiet machine. **Median and minimum are both given, and the minimum matters here**:
the host figures swing by two to five times run to run while nife's move by a few percent, so a
median of five is a pessimistic reading of Linux and the minimum is the fair estimate of what the
machine can do. Both are printed rather than one being chosen.

| variant | seq write | seq read | rand read | rand write |
|---|---|---|---|---|
| **buffered** (what a program gets) | 2,068 / 1,812 | 547 / 449 | 530 / 352 | 2,105 / 1,412 |
| **`O_DIRECT`** (no page cache) | 63,688 / 44,717 | 91,694 / 27,149 | 48,120 / 26,580 | 50,481 / 28,636 |
| **`O_DIRECT` + `O_DSYNC`** (durable per write) | 284,264 / 262,118 | 63,406 / 32,830 | 41,499 / 34,676 | 97,274 / 74,516 |
| **raw `/dev/vda`, `O_DIRECT`** (no filesystem) | 42,104 / 33,691 | 53,296 / 38,730 | 47,890 / 45,307 | 49,426 / 47,467 |

ns per 4 KiB, median / minimum. And the variant that prices our protocol's page limit, the same ext4
with the same flags and a unit a real program would use, in ns per **64 KiB** transfer:

| variant | seq write | seq read | rand read | rand write |
|---|---|---|---|---|
| **`O_DIRECT`, 64 KiB unit** | 66,737 / 50,009 | 103,411 / 44,473 | 69,073 / 50,648 | 69,522 / 66,483 |

**A 64 KiB request costs about what a 4 KiB one does**, so sixteen times the payload arrives for the
same price: 600 to 900 MiB/s against 40 to 80. That number is the size of the prize a multi-page
transfer on `fs_proto` would be chasing, and it is why the 4 KiB cap is the first caveat rather than
the last.

**One ordering artefact, stated because it moves a number.** The `rawdev` rows run last, after the
64 KiB variant has written 16 MiB, so the host is still flushing when they start; they are an upper
bound on the device floor rather than a clean reading of it. The cleanest floor available is
`O_DIRECT` sequential read at 27 us.

### macOS and APFS, natively, which is a different tier

| variant | seq write | seq read | rand read | rand write |
|---|---|---|---|---|
| **buffered** | 3,992 / 3,645 | 1,768 / 466 | 1,223 / 338 | 5,739 / 1,859 |
| **`F_NOCACHE`** | 46,460 / 10,319 | 65,843 / 25,898 | 57,128 / 48,210 | 27,241 / 20,454 |
| **`F_NOCACHE` + `F_FULLFSYNC`** | 3,458,029 / 3,092,685 | 57,277 / 37,153 | 71,118 / 51,681 | 2,845,194 / 2,326,291 |

ns per 4 KiB, median / minimum, five rounds. `F_FULLFSYNC` is macOS's real barrier (`fsync` there
does *not* flush the drive cache), and asking for one on every 4 KiB write costs three milliseconds
on an NVMe SSD. That figure lands in the same range as our sequential write, and **it is not a win**:
we are not issuing a device flush at all, so the two are doing different work and the coincidence is
one of magnitudes rather than of guarantees.

### Where the milliseconds go

**The confined-server tax: about a microsecond.** `relay_rtt` measures the exact topology this path
uses, a client through a confined intermediary to a backend, at ~980 icount ticks. Against a 1.5 ms
read that is **0.07%**. This is the number the "userspace servers are too slow" objection is about,
and it is invisible at this scale. It cuts both ways: no amount of tuning the IPC path moves any
figure in these tables.

**The block path: at parity with Linux.** Our 46.2 us per 4 KiB block, measured through a userspace
block server that owns the DMA and answers over IPC, against Linux's own 4 KiB reads at this tier:
38.7 to 53.3 us raw, 27 to 92 us through ext4. **A confined userspace block driver is not costing us
the device.** That was the result this milestone was least confident of in advance and it is the one
worth quoting.

**The store's geometry: 32x, and it is all of the rest.** A 4 KiB read fetches 128 KiB. Everything in
the nife table is that constant times a small integer. It belongs to the store we vendored rather
than to anything this project designed, and notes/fs-server.md records it as a `BUGS` entry beside
the server.

### The numbers a skeptic will quote, at 4 KiB

| | nife | ext4 `O_DIRECT` | ext4 buffered | raw virtio |
|---|---|---|---|---|
| sequential read | 1.51 ms | 92 us (min 27) | 0.55 us | 53 us |
| sequential write | 2.57 ms | 64 us (min 45) | 2.1 us | 42 us |

**Against buffered Linux we are about three orders of magnitude behind on reads, and that is the
honest number for "how fast is a program's file IO".** It is the page cache, and it is real: a system
with no cache anywhere reads a hot file at device speed. Recording it without softening is what makes
the rest of this page worth reading.

### What is not apples to apples, listed rather than implied

The map bench's tie and the spawn bench's "lighter object than a Unix process" are the model here:
the caveats are the substantive half, and a figure quoted without them is worth less than no figure.

1. **The 4 KiB unit is ours by constraint and theirs by choice.** A `fs_proto` request cannot carry
   more than a page. The 64 KiB row prices exactly that, at about sixteen times.
2. **We have no cache and they have several.** No `DiskCache`, no readahead, no metadata cache; the
   identical cost of our sequential, random and record-aligned reads is the proof. `O_DIRECT` and
   `F_NOCACHE` remove the page cache on the other side but not the in-kernel metadata caching that
   lets ext4 map a block without reading one.
3. **Our write is between Linux's two.** Every `fs_proto` write goes through a RedoxFS transaction
   that commits to the header ring before the reply, so the filesystem's own state is durable per
   request the way `O_DSYNC` makes ext4's; but no `VIRTIO_BLK_T_FLUSH` is issued unless a client asks
   (`fs_proto::fs::SYNC`, milestone 55), so the bytes sit where `O_DIRECT` alone leaves them. Both
   rows are printed and neither is *the* comparison.
4. **The copy counts differ, in our favour.** A completed read lands in the page the client already
   shares with the server, so the bytes can be used in place; buffered Linux copies into the caller's
   buffer. `O_DIRECT` closes most of that gap by DMA-ing into the user buffer, one more reason it is
   the row to read.
5. **macOS is not at this tier at all**, per its table.
6. **1 MiB per phase is small**, bounded by the fixture image. Enough to make the per-request costs
   clear, not enough to say anything about behaviour at scale.
7. **The machine was shared**, per the next section.

### The noise floor, and how a round earns its place

**This machine is shared with other agent lanes and was not quiet for most of the day**, at load
averages between 15 and 70, including a lane deliberately running eight stress processes for its own
measurement. Numbers taken through that are not numbers, so each system carries a control:

| system | control | its quiet value |
|---|---|---|
| nife | `fs_read` | ~204 us, recorded 2026-07-29, before this milestone existed |
| Linux | `rawdev_seq_read` | 38.7 us, the best observed |
| macOS | `nocache_seq_read` | 25.9 us, the best observed |

The nife figures are the median of the four rounds (of five) whose `fs_read` landed within 2% of
that pre-existing quiet value, on a machine at load 4 to 9. An earlier five-round series taken hours
apart at load 14 to 18 agrees with them to within 6% on every phase, which is the evidence that the
control selects for a real condition rather than for luck. The host figures were taken on the same
quiet machine and are reported as median and minimum because they are the noisier of the two.

**A round that failed its control is discarded, not averaged in.** That is selection toward the
unloaded machine, it is stated rather than smoothed away, and the alternative on a shared machine is
a number nobody can reproduce.

### One tooling bug found on the way, because it changes what a bounded run costs

`scripts/qemu-bounded.sh` killed its watchdog subshell when the guest finished on its own and left
the `sleep` inside it running. An orphaned `sleep` holds the write end of the pipe it inherited, so
**every bounded run whose output is piped blocked for the full bound** however quickly the guest
exited: the Linux comparison boots a guest that powers itself off in about fifteen seconds and each
round took five minutes. Fixed by killing the watchdog's children first (`pkill -P`). Every caller
that pipes a bounded run was paying this.

## The record level, swept: what a 128 KiB record actually costs (milestone 138, 2026-08-18)

Milestone 38 ended on one term: every 4 KiB file request moves 128 KiB, because RedoxFS stores a file
in 128 KiB records and reads a record whole. Milestone 138 lists three ways to fix that and says none
of them can be argued until somebody measures throughput against the record level, because nobody
had. This is that sweep.

**The headline.** Taking the record from 128 KiB down to one block makes a 4 KiB read **5.6 times
faster** and a 4 KiB write **3.0 to 3.8 times faster**. It does not deliver the 32x, and why it does
not is the useful part: **the 32x was never all of the cost.** A request also pays a fixed ~208 us
that no record level touches, and once the record is one block that fixed cost is **80% of what is
left**.

### How it was measured, and what to do about a machine that was not quiet

`bench/record-level-sweep.sh`, on milestone 38's existing harness rather than a new one: the same six
phases of `fs_test_client`'s throughput role, the same 256 transfers of 4 KiB per phase, the same
fixed-seed offsets, the same fresh incompressible payload per write. Nothing about the benchmark
changed, so every figure here is comparable to the milestone 38 table above.

**How a level gets set, since nothing can ask for one.** The script edits `RECORD_LEVEL` in
`vendor/redoxfs/src/lib.rs`, rebuilds, runs, and puts the constant back; it refuses to start if that
file is already dirty, so it cannot restore over an edit it did not make. `cargo xtask bench
--release --real --smp` regenerates the RedoxFS image on every run, so each point is a whole
filesystem built at that level rather than a mixed one. **The tree's committed value is unchanged at
5.** This sweep measures; it does not decide.

**The machine was loaded, so the control does more here than select rounds.** Twenty passes ran over
the six levels, interleaved (one pass sweeps 0 through 5, then the next), on a host at load averages
of 5.5 to 21 with three other lanes gating. That is outside the load 4 to 9 milestone 38 took its own
figures at, and keeping only the rounds that pass a 2% control would have left one or two per level.

So the primary figure below is a **ratio**: each phase divided by the **same round's** `fs_read`.
`fs_read` is the right denominator rather than an arbitrary one, because it reads `motd`, which is 69
bytes and lives inline in its node, so it fetches no record at all and its cost is **independent of
the level being swept**. That is a property of the code (`read_node_inner` returns from the inline
branch before it reads `record_level()`) and it is visible in the data: `fs_read` measures 203 to
208 us at every level on every quiet round. Dividing by it cancels whatever the host was doing to the
guest that second.

**Two runs taken at an ordinary load are the evidence that the normalisation invents nothing.** Before
the sweep, at load 6.9 and 7.3, single runs at level 5 and level 0 gave a sequential read of
1,466,327 ns and 257,893 ns. The ratio method over twenty passes at loads from 5.5 to 21 puts them at 1,453,963 and
258,582: **0.8% and 0.3% apart**. The raw minimums are printed beside the normalised figures, and
where the two disagree the disagreement is the noise rather than a finding.

### The sweep

ns per 4 KiB, with MiB/s in brackets. Level 5 is the tree's shipped value, so that row is milestone
38's table measured again by a different method on the same day; it agrees with it to within 6%.

| record level | record | seq read | rand read | record read | seq write | rand write |
|---|---|---|---|---|---|---|
| **0** | 4 KiB | 258,582 (15.1) | 261,918 (14.9) | 262,883 (14.9) | 790,317 (4.9) | 881,395 (4.4) |
| **1** | 8 KiB | 280,262 (13.9) | 281,607 (13.9) | 279,648 (14.0) | 769,266 (5.1) | 911,009 (4.3) |
| **2** | 16 KiB | 355,002 (11.0) | 359,628 (10.9) | 359,342 (10.9) | 870,776 (4.5) | 1,079,208 (3.6) |
| **3** | 32 KiB | 517,314 (7.6) | 521,923 (7.5) | 518,700 (7.5) | 1,058,267 (3.7) | 1,398,535 (2.8) |
| **4** | 64 KiB | 836,690 (4.7) | 839,899 (4.7) | 840,427 (4.6) | 1,520,484 (2.6) | 2,056,411 (1.9) |
| **5** | 128 KiB | 1,453,963 (2.7) | 1,458,916 (2.7) | 1,458,735 (2.7) | 2,408,470 (1.6) | 3,331,724 (1.2) |

The raw minimum of every round at each level, with no normalisation at all, in ns:

| record level | seq read | rand read | record read | seq write | rand write |
|---|---|---|---|---|---|
| 0 | 256,296 | 251,037 | 251,311 | 767,722 | 868,865 |
| 1 | 268,126 | 268,806 | 264,532 | 720,203 | 879,846 |
| 2 | 338,488 | 333,764 | 336,788 | 805,264 | 1,018,608 |
| 3 | 506,940 | 512,699 | 513,431 | 1,043,946 | 1,367,542 |
| 4 | 810,257 | 830,180 | 823,514 | 1,505,937 | 2,016,396 |
| 5 | 1,445,499 | 1,462,345 | 1,399,981 | 2,357,163 | 3,260,817 |

### One straight line fits all six points, and that is the result

The three read phases measure the same thing at every level, which is milestone 38's no-cache finding
holding at every record size. Fitting `cost = a + b x 2^level` by least squares:

| phase | `a`, fixed per request | `b`, per 4 KiB block | residual at levels 0..5 |
|---|---|---|---|
| `fs_seq_read` | 207,679 ns | 38,980 ns | +4.6 -1.9 -2.4 -0.4 +0.6 -0.1 % |
| `fs_rand_read` | 210,769 ns | 39,036 ns | +4.6 -2.6 -2.0 -0.2 +0.5 -0.1 % |
| `fs_record_read` | 209,832 ns | 39,059 ns | +5.3 -3.0 -1.9 -0.7 +0.7 -0.1 % |
| `fs_seq_write` | 672,199 ns | 53,720 ns | +8.1 -1.3 -1.9 -4.1 -0.7 +0.7 % |
| `fs_rand_write` | 769,202 ns | 80,049 ns | +3.6 -2.0 -0.9 -0.8 +0.3 +0.0 % |

**Read a request as two terms**: about **208 us** the record level does not touch, plus **39.0 us for
every 4 KiB block the record holds**. At level 5 the second term is 32 blocks and swamps the first; at
level 0 it is one block and the first term is 80% of the total.

**That corrects a constant this page has been quoting.** Milestone 38's **46.2 us per block** came
from dividing one measurement by 32, so it charged the per-request metadata walk to the blocks. It is
an average rather than a marginal cost. The marginal cost of a block through the confined block server
is **39.0 us**, and the per-request metadata walk is a separate 208 us, which is about 5.3 blocks at
that price. Both readings describe the same measurement; the sweep can separate them because it has
six points and a slope. The parity claim survives and gets slightly stronger: 39.0 us against Linux's
38.7 to 53.3 us for a raw 4 KiB virtio read at this tier.

**The writes decompose the same way, and they say out loud what a copy-on-write write is.** A random
write's slope is **80.0 us per block, 2.05 times the read slope**: the record is read and then
written, exactly as copy-on-write says. A sequential write's slope is lower, **53.7 us**, because a
growing record doubles its stored level rather than rewriting a full one, which is the mechanism
behind milestone 38's "a sequential write is 55 blocks and a random write is 74". Both write
intercepts are far larger than the read's, 672 us and 769 us against 208: that is the transaction,
which allocates, rewrites the node and commits to the header ring on **every request**, and no record
level touches it either.

**The one place the fit is visibly wrong is worth more than the fit.** Level 0 reads land about 5%
above the line, consistently, in all three read phases. That is the metadata cost of small records
arriving on schedule. A node carries 128 direct record pointers, so at level 0 they address the first
512 KiB of a file and the throughput file is 1 MiB: **half of its reads need an indirect block read
first**. At level 5 the whole file is eight direct pointers and there is no indirection at all.

### What this says about milestone 138's three options

| | read | seq write | rand write | against today |
|---|---|---|---|---|
| **today**: 4 KiB request, 128 KiB record | 2.7 MiB/s | 1.6 | 1.2 | 1x |
| **option 2 alone**: 4 KiB request, one-block record | **15.1** | 4.9 | 4.4 | 5.6x read, 3.0x write |
| **option 1 alone**: 64 KiB request, 128 KiB record | **43** | 26 | | 16x |
| **options 1 and 2**: 64 KiB request, 64 KiB record | **75** | 41 | | 28x |
| the block path's own ceiling | ~100 | | | 37x |
| ext4 `O_DIRECT` at a 64 KiB unit, same tier | ~940 | ~940 | ~900 | |

**Only the first two rows are measured**, and the difference matters. Rows three and four are the
measured cost of **one request that fetches one record of that size**, which is what a 64 KiB request
would cost if the contract could carry one. The derivation rests on something milestone 38 measured
rather than assumed: `read_record` reads the block the pointer **stores** and only then checks the
level asked for, so how many bytes a request asks for does not change what the store fetches. The
extra cost of moving 64 KiB rather than 4 KiB into the client's pages is bounded by `fs_payload_fill`,
which paints a page in 820 ns: sixteen pages is **13 us against 837**, under 2%.

- **Option 2, a record level matched to the transfer unit.** 5.6x on reads and 3.0x on writes,
  measured. It is the only one of the three that needs no agreement between two programs. Its costs
  are below, and the largest of them is not on milestone 138's list.
- **Option 1, a multi-page transfer on the file contract.** 16x on its own, which is more than option
  2 buys, because it amortises **both** terms of the model over sixteen times the payload rather than
  only the record term. It is a wire change.
- **Both.** 28x, and this is the combination worth wanting. Note what happens to the record level once
  a request carries 64 KiB: level 4 and level 0 cost the **same** 837 us for that 64 KiB, because the
  fixed cost is per request and the block count is identical either way. **With a multi-page transfer
  the record level stops mattering for aligned bulk IO**, and option 2 collapses into "do not fetch
  more than the request asked for", which every level from 0 to 4 satisfies.
- **Option 3, replace the store.** This sweep gives it nothing. The 32x it was blamed for is a
  parameter the format already carries, and what survives after that parameter is turned down is
  208 us of RedoxFS re-reading its own metadata from the device on every request, about 5.3 block
  reads. That is the absence of a cache, which milestone 138 puts out of scope on purpose, and
  swapping the store to acquire one is a strictly larger change than adding one. **Nothing measured
  here is evidence that RedoxFS is the problem.**

**And the wall behind all three, which is new.** The ceiling row is not rhetorical. `IpcDisk::read_at`
chunks every record into **one `fs_proto::blk` request per 4 KiB block**, because the block contract
shares exactly one page with the block server, the same limit the file contract has. A 128 KiB record
read is therefore 32 device round trips rather than one 128 KiB transfer, and 39.0 us is the price of
a round trip rather than of the bytes. At that price **no request size and no record level can exceed
about 100 MiB/s**. Linux moves 64 KiB through one virtio request for 67 us at this same tier, which is
where the remaining order of magnitude lives, and it is one layer below the one milestone 138 is
about.

### The 208 us, identified: five blocks, and they are the same five every time

The sweep above left the fixed term attributed rather than measured, as "RedoxFS re-reading its own
metadata, about 5.3 block reads". calef's question was whether that is inherent to RedoxFS's design
or the absence of a cache any store would need. It is the second, and the measurement is a count
rather than a time, so it is not close.

**Every 4 KiB read of an ordinary file makes exactly five single-block reads below the record, and
they are the same five block numbers on every request.** Not 5.3 on average, and not a different five
each time: five, and the same five, in every phase measured.

They are one call, `Transaction::read_tree_and_addr`, which `Server::read` reaches once per request
through `read_node`:

| # | block | what it is |
|---|---|---|
| 1 | `header.tree` | the node tree's L3 root, fixed for the whole filesystem until something commits |
| 2 | L2 | one block per 16.7 M node ids |
| 3 | L1 | one block per 65,536 node ids |
| 4 | L0 | one block per 256 node ids |
| 5 | the node | the file's own `Node`, one block, one per file |

`TREE_LIST_SHIFT` is 8 (`vendor/redoxfs/src/tree.rs`), so the fanout is 256 per level and the first
four are shared by every file whose node id falls in the same 256. In the fixture below, two 1 MiB
files and a `motd` share blocks 1 through 4 and differ only in block 5.

**How it was counted.** A `Disk` implementation over an in-memory image that logs every `read_at`,
built as a temporary probe in `fs_server`'s host tests and reverted before this note was committed
(see the reproduction below). `BlockDisk` splits a `Disk` call into whole-block transfers, one
`fs_proto::blk` request each, exactly as `IpcDisk` does on device, so `ceil(len / 4096)` per call is
the number of block-server round trips the request costs on the machine. The block *numbers* differ
on device (a different image, behind a partition offset); the counts and the repetition do not.

Fixture: a 32 MiB image, `FileSystem::create`, two 1 MiB incompressible files and one inline `motd`,
reopened through `Server::open`, then 256 requests of 4 KiB per phase, logged per request.

| phase | single-block reads per request | distinct such blocks | record read |
|---|---|---|---|
| 1 MiB file, 256 sequential 4 KiB reads | **5.00** | **5** | 1 call, 32 blocks |
| 1 MiB file, 256 random 4 KiB reads | **5.00** | **5** | 1 call, 32 blocks |
| a second 1 MiB file, 256 sequential | **5.00** | **5** | 1 call, 32 blocks |
| alternating between the two files | **5.00** | **6** (4 shared) | 1 call, 32 blocks |
| `motd`, 64 reads, inline, no record | **5.00** | **5** | none |

**99.6% of those reads were of a block already read in the same phase** (1,275 of 1,280). Zero writes
happened during any read phase, so nothing was invalidating anything.

**And it does not move with the record level**, which is what makes it the fixed term rather than
part of the slope. Re-run at `RECORD_LEVEL` 1 and 2: 5.00 per request, 5 distinct blocks, unchanged,
with the record read falling to 2 and 4 blocks as the model says.

Level 0 is the exception and it is the residual the sweep already found. There the record read is
itself one block, so the probe's classifier folds it in, and the figure is **6.50** per request
sequential and 6.53 random. That decomposes as five tree blocks, one record, and **0.5 indirect
pointer blocks**: a node holds 128 direct record pointers, a 1 MiB file at level 0 is 256 records, so
half of its reads need an indirect block first. That is the 5% level-0 residual in the fit above,
measured directly rather than inferred, and it is a property of the *small* record rather than of the
walk.

**What a perfect cache removes.** Five block reads at the measured marginal 39.0 us is **195 us
against the fitted 208 us intercept, 94% of it.** The remaining ~13 us is the file-IPC round trip and
the server's own work, which no cache touches. A second measurement says the same thing from the
other direction and was already on this page: `fs_read` reads an inline `motd`, does exactly these
five reads and nothing else, and costs 203 to 208 us.

The cache is not a large object. Four of the five blocks are the tree spine and are shared by every
file; a filesystem with 65,536 nodes has a spine of 1 + 1 + 1 + 256 = **259 blocks, about 1 MiB**,
and the fifth block is the node, one per open handle, which a server holding handles could keep
without a cache at all.

**So option 3 gets nothing here either, and now for a measured reason rather than an argued one.**
The question was whether the 208 us is structural. The walk is structural in one narrow sense, that
the format fixes the depth at four levels plus the node, and a store with a shallower id-to-node map
would do fewer reads. That is not what makes it cost 195 us. It costs 195 us because **the same five
blocks are fetched off the device 256 times in a row**, and every store that maps an id to a node has
a path from a root to that node which it would also fetch. Replacing RedoxFS buys a rewrite and
arrives needing the identical cache. **Nothing measured here is evidence that RedoxFS is the
problem**, which is what the sweep said and this now says with the block numbers in hand.

**What this measurement does not settle**, stated because it is the half a count cannot reach: it
says a cache removes 94% of the fixed term on this workload, not that a cache is cheap to build. A
cache in this server has coherency and confinement questions of its own, and milestone 138 puts it
out of scope on purpose. This is an argument about which milestone owns the 208 us, not a design for
one.

**Reproducing it.** The probe was a `Disk` recorder in `fs_server`'s `mod tests` plus a driver that
clears the log per request and histograms block number against read count; it is not in the tree,
because it is a one-question instrument and the tree already carries the two facts it produced. The
whole of it is `read_tree_and_addr`'s five `read_block` calls, so a reader who wants the result
without the probe can read `vendor/redoxfs/src/transaction.rs:498` and count. `git log` for this
section has the probe in its message.

**Conditions.** Host measurement only, no emulator, so no QEMU ran and nothing about it competes with
another lane. Load average 2.4 at the start. That matters less here than anywhere else on this page:
every number in this section is a count of block reads, and a count does not move with load. The one
time in it, 39.0 us per block, comes from the sweep above and carries that sweep's conditions.

### What option 2 costs, and why level 1 is the interesting answer rather than level 0

Two of the three costs milestone 138 named are avoided by not going all the way down.

**Compression is given up at level 0 and only at level 0.** RedoxFS compresses a record when its
stored level is above zero (`write_node_inner_records`: `if decomp_level.0 > 0`), so a one-block
record is never compressed and an 8 KiB record still is. Level 1 keeps lz4 and reads **8% slower than
level 0**, which is nothing against the 5.6x either of them buys, and it writes sequentially
*faster* (769,266 ns against 790,317).

**More records means more block pointers, and the sweep shows it** in the level 0 residual above. It
grows with the file rather than staying put: an 8 MiB Time Machine band file is 64 records at level 5
and every one of them direct, against 1,024 records at level 1 of which 87% need an indirect block
read, which is one more 39.0 us round trip on a 280 us request, about 14%. Level 1 halves the number
of records against level 0 for the same reason it keeps compression.

**Copy-on-write means a write reads its record first**, and that cost *falls* with the level rather
than rising: it is the 80.0 us per block slope, and at level 0 there is one block to read instead of
32. It is a cost of the large record, not of the small one.

**And the space cost, which is the one this sweep can put a number on.** The same 560 KiB of
documentation imported into a fresh 16 MiB image, counted as non-zero 4 KiB blocks:

| record level | 0 | 1 | 2 | 3 | 4 | 5 |
|---|---|---|---|---|---|---|
| blocks used | 200 | 172 | 165 | 162 | 160 | 145 |
| against level 5 | **+38%** | +19% | +14% | +12% | +10% | 1x |

That is compression and metadata together, on text, which is the payload most favourable to lz4.
Level 0 gives up both and pays 38%; level 1 keeps compression and pays 19%, and the 19% is the
pointers rather than the entropy. **An incompressible payload would show only the pointer half**, so
a backup workload should expect something closer to the 10% at level 4 than to the 38%, and this
sweep did not measure that case.

### How a level is chosen, and the correction the code forced

**The premise milestone 138's block rests on is true and incomplete.** `record_level` is a per-node
field in the on-disk format (`vendor/redoxfs/src/node.rs`), `Node::new` sets it once at creation, and
both data paths honour the node's value rather than the crate constant (`transaction.rs`,
`read_node_inner` and `write_node_inner_records`). Directories get 0 already. All of that reads
exactly as the block says.

**What the block gets wrong is "not a fork of the vendored crate".** Three things in the engine put a
per-file level out of reach today:

- `Node::new` takes no level and there is no setter. The constant is the only source.
- `RecordRaw::empty` and `HTreeNode::empty` both refuse a level **above** `RECORD_LEVEL`, and
  `read_block` allocates its buffer through `T::empty(ptr.addr().level())`. So **lowering the constant
  makes every record already stored at a higher level unreadable**, with `ENOENT`, on an image that
  was perfectly good before.
- Nothing in `fs_proto` can name a level, so the FS server would have nothing to pass down even if the
  engine took one.

So option 2 has two shapes and they are not the same decision. **Lowering the default** is one line
and a one-way door for every existing image: no migration exists, and it costs nothing today only
because every image in this tree is regenerated from source. **A genuine per-file choice** is what the
format supports and the crate does not: `Node::new` would have to take a level, the two `empty` guards
would have to compare against a maximum rather than against the default, and a creating client would
need a way to say which level it wants. That is a divergence carried in `patches/` plus a contract
change, which is a larger thing than the block priced.

### The workload question, answered by reading rather than by guessing

**The code quoted below was changed on 2026-08-19 (milestone 55) and the section is left standing,
because the reasoning is what makes the change legible.** `smb_server`'s two `min`s now read
`fs::TRANSFER_MAX` rather than `fs_proto::PAGE`, so a Mac writing a megabyte arrives as 16 requests
rather than 256. Measured through a real SMB client: **write 4.8x, read 2.4x**, against the 8.02x
and 5.67x step 3 measured on the contract itself, with the residual now owned by the socket
contract's own 4080-byte chunking. The table and the reasoning are in notes/smb.md's throughput
section. What follows is the finding as it stood, which is what made that milestone exist.


Milestone 138 asks whether 4 KiB is the atypical case, since a Time Machine backup writes band files,
which are large and sequential, and a 128 KiB record is plausibly right for those.

**It is not the atypical case. It is the only case this system has.** `user/src/smb_server.rs` chunks
every SMB read and every SMB write into `fs_proto::PAGE`-sized requests, in a loop, because that is
what a `fs_proto` request carries:

```rust
let want = (out.len() - done).min(fs_proto::PAGE);      // read
let chunk = (data.len() - done).min(fs_proto::PAGE);    // write
```

A Mac writing a megabyte into a band file therefore arrives at the store as **256 separate 4 KiB
writes**, each a read-modify-write of a whole 128 KiB record, with nothing between the two to coalesce
them: there is no cache in the FS server, and a RedoxFS transaction's `write_cache` lives and dies
inside one request.

**So the reframing inverts.** The large record is not right for the customer path and wrong only for
the benchmark. It is wrong for both, for the same reason, which is that the transfer unit is 4 KiB
everywhere in this system while the record is 32 times that. A 128 KiB record starts to make sense the
day a request can carry one, which is option 1, and not before.

One honest qualification: **a band file is written sequentially and grown**, and that is the cheapest
thing a 128 KiB record does, because a growing record doubles its stored level instead of rewriting
128 KiB from the first page. It is already in the numbers, as the gap between the sequential and
random write columns at every level. It is a discount on a bad price rather than a case for the price.

What that costs a real backup, at today's setting and at the two that are one decision away: a 100 GB
first backup is **17.6 hours** of sequential writing at 1.62 MiB/s, **5.8 hours** at option 2's 4.94, and
**42 minutes** at the 41 MiB/s of options 1 and 2 together. Those are the write path alone, with no
network, no SMB, and no second copy of anything.

### BUGS

- **Nothing here was measured at a level other than the tree's own in a build anyone kept.** Every
  point except level 5 came from a throwaway build with the vendored constant edited, and the whole
  filesystem in that build used that level. A per-file level, which is the shape option 2 would
  actually ship, has never been built or measured.
- **The only correctness these runs prove is the harness's own.** The throughput client checks the
  bytes it read back and the file's length after each phase, and every level passed that at every
  round, but `script/test` was not run at any level below 5. A level that measures well is not
  thereby a level the confinement, crash and recovery suites pass at.
- **The record-aligned phase stays aligned by luck rather than by design.** `fs_record_read` reads at
  multiples of 128 KiB, which is a multiple of every record size at or below level 5, so it means the
  same thing at every point in this sweep. It would stop meaning it the moment anyone swept above
  level 5, and nothing checks that; `fs_proto::fixture::throughput::RECORD` says so in its own
  comment.
- **The machine was not quiet and the headline figures are normalised rather than raw.** The method is
  above and the raw minimums are printed beside them. Both agree with the two runs taken at an
  ordinary load, which is the evidence that the normalisation is honest, and it is weaker evidence
  than a quiet machine would have been.
- **Option 1 is priced by derivation and not by measurement**, because it cannot be measured without
  being built: no request in this system can carry more than a page. The assumption it rests on is
  named where the price is, and it is one milestone 38 measured rather than assumed.
- **The space figures count non-zero blocks in a fresh image, which is not the allocator's own
  answer.** A block a record has vacated keeps its old bytes, so the count would drift upward on an
  image that had been rewritten; these images were made, imported into once, and never written
  again, which is the case where the count and the allocation agree. RedoxFS's own free-block count
  (`fs_proto::fs::STATFS`) would be exact and needs a guest or a verb the host tool does not have.

## Step 1 taken: the record is 8 KiB, and 72% of a read is now the part it does not touch (milestone 138, 2026-08-18)

The sweep above measured. This is the step that shipped, and it is the first of milestone 138's four.
`RECORD_LEVEL` in the vendored engine goes from 5 to 1, so a file this build creates stores 8 KiB
records instead of 128 KiB, and a 4 KiB request stops moving 32 blocks to serve one.

### Before and after, on a machine that was actually quiet

Six interleaved passes over levels 5, 1 and 0 (`sh bench/record-level-sweep.sh 1 5 1 0`, six times),
on the same harness and the same six phases as milestone 38 and as the sweep above. **The
normalisation the earlier sweep needed is not needed here**: the `fs_read` control measured 202,246,
203,389 and 202,352 ns at levels 5, 1 and 0, a spread of 0.6%, so these are raw means of six rounds
rather than ratios. Load ran 3.8 to 6.6, inside milestone 38's own 4 to 9.

ns per 4 KiB, mean of six, MiB/s in brackets:

| phase | before (level 5) | after (level 1) | speedup | level 0, for comparison |
|---|---|---|---|---|
| `fs_seq_read` | 1,458,124 (2.68) | **283,974 (13.76)** | **5.13x** | 261,310 (14.95) |
| `fs_rand_read` | 1,453,688 (2.69) | **282,773 (13.81)** | **5.14x** | 260,966 (14.97) |
| `fs_record_read` | 1,448,318 (2.70) | **281,014 (13.90)** | **5.15x** | 260,582 (14.99) |
| `fs_seq_write` | 2,399,611 (1.63) | **796,930 (4.90)** | **3.01x** | 803,598 (4.86) |
| `fs_rand_write` | 3,289,133 (1.19) | **904,858 (4.32)** | **3.63x** | 872,925 (4.47) |

**The model reproduced independently.** Fitting `cost = a + b x 2^level` through levels 1 and 5 (the
two the six-point sweep put on the line) gives a fixed term of **205.7 us** and a per-block term of
**39.1 us** for a sequential read, against the earlier sweep's 207.7 and 39.0 from six points on a
loaded machine. The write terms land at 690.1/53.4 and 745.9/79.5 against 672.2/53.7 and 769.2/80.0.
Nothing was tuned to make those agree; they are two different runs on two different days.

**Level 0 is still above the line and by the same amount**, +6.7 to +8.1% across the five phases
against the earlier sweep's +4.6 to +8.1%, which is the indirect-pointer cost the sweep identified
arriving twice.

### Why level 1 and not level 0, verified rather than inherited

The sweep recommended level 1. Measured here, the trade it named holds:

- **Reads are 8.7% slower at level 1 than at level 0** (283,974 against 261,310), which is 8.7% of a
  5.6x. The earlier sweep said 8%.
- **Sequential writes are marginally *faster* at level 1** (796,930 against 803,598, 0.8%), and
  random writes 3.7% slower. The direction on sequential writes matches the earlier sweep, which saw
  2.7% the same way.
- **Level 1 keeps lz4 and level 0 does not**, because RedoxFS compresses a record only when it is
  larger than one block. The space cost the sweep measured is +19% at level 1 against +38% at level
  0, on text; that figure was not re-measured here and is cited from the sweep.

So level 1 buys back half the space for 8.7% of the speed, and it is the answer. **A note on what
that 8.7% is**: it is not the compression. It is the second block, at 39.1 us, which the two-term
model predicts to within a percent without knowing anything about lz4.

### The residual: what step 1 leaves behind

Milestone 138 asks every step to report the per-request cost no record level removes, because the
accumulation of those is the answer to whether this architecture has a disk-read liability it cannot
overcome. After step 1:

| | total | fixed term | record term |
|---|---|---|---|
| 4 KiB sequential read | 283,974 ns | **205,698 (72%)** | 78,277 (28%) |
| 4 KiB sequential write | 796,930 ns | **690,085 (87%)** | 106,845 (13%) |
| 4 KiB random write | 904,858 ns | **745,907 (82%)** | 158,952 (18%) |

**The reversal is the result.** Before this step the record term was 86% of a read (32 blocks at
39.1 us) and the fixed term 14%; after it the fixed term is 72% and the record is 28%. Step 1 did not shrink the residual at all,
which is the point of reporting it: it made the residual the whole problem.

**What that residual is made of** is already counted, and not by this step. Of the read's 206 us,
about 195 us is five single-block reads of the same five blocks on every request, which is step 2's
target (see the section above and design/roadmap/138-file-io-throughput.md). The remaining ~13 us is
the file-IPC round trip and the server's own work, and that is the number to keep watching: nothing
in milestone 138's four steps removes it, and it puts a fully cached 4 KiB read at about 300 MiB/s.

**The write residual is larger and has a different owner.** 690 us on a sequential write is the
transaction: allocate, rewrite the node, commit to the header ring, on **every 4 KiB request**. Step
2's block cache does not touch it, because those are writes rather than reads. Step 3's multi-page
transfer does, and by the most of anything on the list, because it amortises one transaction over
sixteen pages instead of one. **After step 1 the write path's fixed cost is 87% of a write and is the
largest single unaddressed term in this whole measurement.**

### What step 2 looks like now, against measured numbers rather than the model

Milestone 138's table modelled step 2 as "on its own worth 15%; with a small record it is 4.7x". With
step 1 shipped and measured, that can be restated against real numbers rather than a calibration:

- A 4 KiB read is **283,974 ns**, of which **205,698** is the fixed term and **~195,000** of that is
  the five repeated block reads. If a cache removed all five, a read would be about **89,000 ns**,
  which is **3.2x again** and **16x against where milestone 138 started**. That is a smaller multiple
  than the block's 4.7x, because the block's model was built on level 0's numbers and this shipped at
  level 1.
- The same cache before step 1 would have taken a read from 1,458,124 to 1,263,000, which is **15%**,
  exactly as the block said. **The two steps are multiplicative and step 1 is what makes step 2
  worth doing**, which the block predicted and this measurement confirms with the level-1 numbers.
- It does **not** help the write path, whose 690 us residual is a transaction rather than a set of
  reads. Nothing on milestone 138's list addresses that except step 3.

### BUGS

- **The space cost of level 1 was not re-measured for this step.** The +19% figure is the sweep's,
  taken by counting non-zero blocks in a fresh image after importing 560 KiB of text, and text is the
  payload most favourable to lz4. An incompressible payload would show only the pointer half. A
  backup workload is the incompressible case and nobody has measured it.
- **`fs_record_read` reads at multiples of 128 KiB and the record is now 8 KiB.** That is still a
  record boundary, which is the only property the phase needs, and keeping the constant is what makes
  every figure this phase has produced comparable across the whole sweep. It is no longer named after
  the record size, and `fs_server` now asserts at compile time that the two divide; before this step
  `fs_proto` called the mismatch "the one soft spot in this module" and nothing checked it.
- **Only levels 5, 1 and 0 were measured here.** The six-point sweep above is what establishes the
  line; this run confirms two points on it and one off it, and it would not have caught a
  non-linearity at 2, 3 or 4.
- **These are the same 4 KiB transfers milestone 38 chose.** Step 3 changes the transfer unit, and
  when it does, none of the ratios on this page survive as ratios: the sweep already showed that at a
  64 KiB request every level from 0 to 4 costs the same, so step 1's 5.13x is a number about the
  contract as it is today rather than a permanent property of the store.

## Step 3 taken: a file request carries 64 KiB, and the read path lands on the block contract (milestone 138, 2026-08-19)

Taken **before step 2, deliberately**. Step 1 measured that after it a write's fixed term was 690 us
per 4 KiB, 87% of the request, and that step 2's read cache does not touch a write at all. Only this
step does, and a backup is writes.

`fs_proto::fs::TRANSFER_PAGES` goes from an unwritten 1 to 16, so the region a client and the FS
server share is 64 KiB of contiguous pages and a `READ` or `WRITE` may carry all of it in one
request. Nothing in the packed request word changed: the length field has been 40 bits since
milestone 32, and the page was always what bounded a transfer.

### Before and after, on a machine that was quiet again

Six interleaved rounds at each point (`sh bench/transfer-size-sweep.sh 6 1 16`), on milestone 38's
harness and the same six phases as everything above. **The `fs_read` control measured 203,976 ns at
one page and 203,326 at sixteen, a spread of 0.3%**, so these are raw means of six rounds with no
normalisation. Load ran 3.6 to 5.2, inside milestone 38's own 4 to 9. The benchmark holds **bytes
moved** constant (1 MiB per phase) rather than the transfer count, so both points move the same file.

| phase | 4 KiB per request | 64 KiB per request | speedup |
|---|---|---|---|
| `fs_seq_write` | 732,541 ns (5.33 MiB/s) | **1,461,394 ns (42.77)** | **8.02x** |
| `fs_rand_write` | 899,257 (4.34) | **1,991,506 (31.38)** | **7.22x** |
| `fs_seq_read` | 275,860 (14.16) | **778,354 (80.30)** | **5.67x** |
| `fs_rand_read` | 281,685 (13.87) | **830,341 (75.27)** | **5.43x** |
| `fs_record_read` | 282,167 (13.84) | **840,962 (74.32)** | **5.37x** |

The ns column is per **request** and a request is sixteen times larger on the right, so it is the
MiB/s that compares. Per 4 KiB of payload a sequential write went from 732,541 ns to **91,337**.

### The two-term model reproduced from a completely different sweep, and that is the check

The record-level sweep fitted `cost = fixed + blocks x per_block` by varying the **record size**.
This one varies the **transfer size**, which changes the same two terms through a different variable
and had no way to be tuned to agree. A 4 KiB read at record level 1 fetches 2 blocks and a 64 KiB
read fetches 16, so:

- `275,860 = F + 2B` and `778,354 = F + 16B` give **B = 35.9 us** and **F = 204,076 ns**.
- Step 1's fit, from six record levels on a different day, gave **39.1 us** and **205,698 ns**.

The fixed term agrees to **0.8%**. Nothing was fitted to make that happen, and it is the strongest
evidence this page has that the two-term model is the real shape of a request rather than a curve
drawn through five points.

### The residual: the composition inverted, and the new owner is the block contract

Milestone 138 asks every step what it leaves behind. Step 1's answer was that the residual became the
whole problem. Step 3's is that **the residual changed owner**:

| 4 KiB sequential read | total | fixed term | block term |
|---|---|---|---|
| before (one page) | 275,860 ns | **204,076 (74%)** | 71,784 (26%) |
| after (sixteen pages, per request) | 778,354 ns | 204,076 (**26%**) | **574,278 (74%)** |

That is step 1's table read backwards. A read is no longer dominated by the per-request walk; it is
dominated by **sixteen single-block trips through `fs_proto::blk`**, which is the block contract's
one-page limit and is the thing notes/fs-server.md's BUGS section has recorded as a ~100 MiB/s
ceiling since milestone 38. **`fs_seq_read` now measures 80.30 MiB/s, which is 80% of that ceiling.**

So: nothing left on milestone 138's list moves a bulk read much. The next read win is the block
contract, and that is step 4, which is a `BUGS` entry rather than a milestone.

**The write residual moved, and it is the largest movement milestone 138 has produced.** The 690 us
transaction (allocate, rewrite the node, commit to the header ring) is charged **once per request**,
so it went from 690 us per 4 KiB to **43 us per 4 KiB**. A sequential write is 8.02x, which is more
than any other number in this milestone, and it is exactly what step 1 predicted the transaction term
would do under a larger transfer.

### What step 2 is worth now, re-priced against measurement for the second time

Step 1 re-priced the metadata cache from the block's modelled 4.7x to a measured 3.2x. Step 3
re-prices it again, and this time the direction is down and the reason is that the two steps target
the same term:

- **On a 64 KiB read it is worth about 1.33x.** The five repeated block reads are ~195 us and they
  are per request; against a 778 us request that is 25% rather than 69%.
- **On a 4 KiB read it is still worth about 3.2x**, unchanged, because nothing about that request
  changed.
- **On writes it is still worth nothing**, for step 1's reason: they are writes.

The block's table said steps 1 and 2 were multiplicative and that neither was worth much alone. That
was right about those two. It did not say that step 3 would take most of what step 2 was going to
get on the bulk path, and it does: **step 2's value is now a function of which request size the
workload uses.** For milestone 55's backup, which reads and writes in 64 KiB units over SMB, step 2
is worth a third; for a small-file or metadata-heavy workload it is worth three times. It is still
worth building and it is no longer the headline.

### What was not measured, and is the first thing to run next

**The record level was not re-swept at 64 KiB.** The sweep above (2026-08-18) found that with a
multi-page transfer, levels 4 and 0 cost the **identical** 837 us per 64 KiB, which predicts that
step 1's 5.13x does not survive as a ratio at this transfer size. This run holds the level at 1 and
varies only the transfer, so it cannot confirm or refute that. `sh bench/record-level-sweep.sh 3 0 1
5` with `TRANSFER_PAGES` at 16 is the experiment and it is one command.

### BUGS

- **Sixteen timed requests per phase.** Bytes moved are held at 1 MiB and the fixture image bounds
  that, so a 64 KiB point is 16 iterations where a 4 KiB point is 256. Each is long enough that the
  counter resolution is not the issue, but the sample is small; the standard deviations in the sweep
  are 0.5% at one page and 0.7 to 4.5% at sixteen, and `fs_seq_write` is the noisy one.
- **`fs_payload_fill` grew sixteenfold in absolute terms and is unchanged per byte** (4,969 MiB/s
  against 4,692). It is inside the write phases' timed window, so subtract it: it is 0.9% of a 64 KiB
  write, the same fraction it was of a 4 KiB one.
- **These are still not apples to apples with a buffered Linux read**, and the caveat did not go away
  when the number got bigger. It got smaller: 64 KiB is the buffer size milestone 38's ext4
  comparison used, so the *transfer size* half of the mismatch is now closed and the page-cache half
  is not. ext4 buffered remains three orders of magnitude away and the reason is still structural.
- **The write comparison is `O_DSYNC`-shaped, unchanged.** Every `fs_proto` write still commits a
  RedoxFS transaction before it replies; what changed is how much payload one commit covers.

## Step 4 taken: the blk contract carries 16 blocks, and the win is smaller than the block count
## predicts (milestone 138, 2026-08-19)

Step 3's own residual pointed here: after it, `fs_seq_read` was 74% single-block trips through
`fs_proto::blk`, one per filesystem block, against a ~100 MiB/s ceiling notes/fs-server.md's `BUGS`
section had already named. `fs_proto::blk::TRANSFER_BLOCKS` goes from an unwritten 1 to 16, so the
region the FS server and the block server share is 64 KiB of contiguous pages, `IpcDisk` batches
contiguous whole-block runs into one blk `CALL` (up to 16 blocks), and the block server issues one
virtio descriptor for the whole batch instead of one per block. The crash injector and the
write-verify diagnostic keep the pre-step-4, one-block-per-`CALL` path unconditionally, because
neither tolerates a request the device completes as one indivisible unit; see `fs_server`'s
`IpcDisk::write_at` for the argument.

### Before and after, ten interleaved rounds each, on a shared and noisy machine

`sh bench/blk-transfer-sweep.sh 10 1 16`. This machine was not quiet: `uptime`'s one-minute load sat
15 to 21 throughout the run (several other lanes building and testing concurrently, the tree's normal
condition per `AGENTS.md`), well above every earlier sweep's 3.6 to 9. **The `fs_read` control is the
signal that says the numbers are still usable**: 8 of 10 rounds at each point landed within 6% of the
203,000 to 208,000 ns quiet baseline the earlier sweeps established, and the analysis below is the
**median** of all 10 rounds at each point, which is what the discipline milestone 38 set (discard
rather than average a loaded round) becomes when almost every round is close and one or two are not.
The file transfer size is unchanged at `fs::TRANSFER_PAGES = 16` (64 KiB, step 3's setting) for both
points; only `blk::TRANSFER_BLOCKS` varies.

| phase | 1 block per blk CALL | 16 blocks per blk CALL | speedup |
|---|---|---|---|
| `fs_seq_write` | 1,544,228 ns | **1,335,376 ns** | **1.16x** |
| `fs_rand_write` | 2,101,313 | **1,539,488** | **1.37x** |
| `fs_seq_read` | 811,724 | **527,420** | **1.54x** |
| `fs_rand_read` | 848,706 | **546,541** | **1.55x** |
| `fs_record_read` | 843,448 | **545,040** | **1.55x** |

### Why this is nowhere near 16x, and it is a finding about steps 1 and 3, not a flaw in step 4

The naive model says "sixteen round trips become one, so this should be close to 16x." It is not,
and the reason is that steps 1 and 3 already changed what is inside the batch. `fs::TRANSFER_PAGES`
is 16 (64 KiB per file-level request), but `RECORD_LEVEL` is 1 (step 1): an 8 KiB record, two
blocks. RedoxFS's engine (`Transaction::read_node`/`read_node_inner`, `vendor/redoxfs/src/transaction.rs`)
walks the tree **once per file-level `Server::read`/`write` call** (`read_tree`, five single-block
reads: the L3/L2/L1/L0 spine and the target node), then loops over however many records that one
call's transfer spans, reading each record's body with its own `Disk::read_at` call sized to the
record. A 64 KiB request therefore spans **8 records**, and step 4 batches each record's own body
(2 blocks, one call instead of two) but cannot batch **across** records, because each record's body
arrives through a separate `Disk::read_at` the engine issues on its own. Per record: 5 metadata
reads (unbatchable, one call each, unaffected by step 4) + 1 data call (was 2). Read call count per
64 KiB request: 8 x 6 = 48, down from 8 x 7 = 56, a call-count ratio of 1.17x, which lines up with
`fs_seq_write`'s measured 1.16x almost exactly. Reads show a larger ratio (~1.55x) than writes
(~1.16 to 1.37x) because a read's total cost is almost entirely blk calls, so the same eight
eliminated round trips are a larger fraction of it; a write pays extra, unbatched cost in the
transaction commit that dilutes the same absolute saving. The absolute savings agree with the
per-block marginal cost this page already measured twice (35.9 to 39.1 us): eliminating 8 round
trips at ~37 us each predicts **~296 us** saved, and `fs_seq_read`/`fs_rand_read`/`fs_record_read`
each saved 284,304 to 302,165 ns, matching to within 6%.

**The finding, stated as a sentence a reader can act on**: step 4's batching is bounded by how big
a record is, not by how big the file-level request is, because RedoxFS only ever asks its `Disk`
for one record's worth of bytes at a time. Step 1 chose an 8 KiB record specifically to keep lz4
compression, and that choice caps what step 4 alone can batch per record at two blocks. **The
majority of what remains (5 of 6 to 7 calls per record) is the tree walk**, which is exactly what
step 2, next, targets.

### Crash consistency, re-run at the new geometry

`fs_server/tests/crash_consistency.rs`, unchanged pass: 0 silently wrong. It is honest to say why it
could not have changed, the same reason step 3's re-run gave: the host-side crash model drives
`BlockDisk<Recording>` directly, below `IpcDisk`'s batching, so nothing this step touches is on that
model's path. The **device-level** crash injector (`fs_server/src/bin/fs_server.rs`'s `inject`
module) is on `IpcDisk`'s own path and is the reason `write_at` keeps an unconditional one-block-per-
`CALL` fallback whenever it might be armed; that fallback is byte-for-byte the code this milestone's
earlier steps already exercised, so the crash test's own coverage of the real device is unchanged by
this step, not merely re-run.

### BUGS

- **The machine was loaded for this measurement**, `uptime` load 15 to 21 throughout, several other
  lanes building and testing concurrently. The `fs_read` control and the two-term model's own
  internal consistency (the predicted ~296 us saving matching the measured 284 to 302 us across three
  independent phases) are the evidence this is a real signal and not noise, but it is not the quiet
  single-tenant machine the record-level and transfer-size sweeps had.
- **`blk::TRANSFER_BLOCKS` was chosen equal to `fs::TRANSFER_PAGES` (16) for symmetry with step 3**,
  not because 16 is where step 4's own curve bends. Nobody has swept it independently the way
  `bench/record-level-sweep.sh` swept the record level; `sh bench/blk-transfer-sweep.sh` takes any
  list of block counts and would answer this in one run.
- **A larger record level would let step 4 batch more per record**, and nobody has re-measured that
  trade now that step 4 exists. `RECORD_LEVEL` is still 1 for lz4's sake (step 1's reasoning); this
  step does not revisit it.

## Step 2 taken: a 64-block metadata cache, and it is the biggest single number in this milestone
## (milestone 138, 2026-08-19)

Step 4's own finding said where the residual now lives: 5 of every 6 to 7 blk calls per record are
`Transaction::read_tree_and_addr`'s tree walk, issued fresh on **every** `Server::read`/`write`/...
call even when the immediately preceding call resolved the identical node
(notes/fs-server.md, "the same five blocks every time", first identified 2026-08-18 and never
addressed until now). `fs_server::CachedDisk` (`fs_server/src/lib.rs`) wraps `IpcDisk` in a small
direct-mapped, write-through cache of single-block reads, 64 slots (`CACHE_SLOTS`,
`fs_server/src/bin/fs_server.rs`), about 257 KiB. Only a `buffer.len() == BLOCK` `Disk::read_at`
consults it; a record body (already the batched call step 4 built) bypasses it entirely. A write
updates or invalidates the written block's slot, and only after the inner disk confirms the write
landed, never before: RedoxFS's copy-on-write allocator never rewrites a live address in place, so
the only way a cached address's content can change is through that same write path, which is what
makes a bare write-through cache correct here with no generation counter. Six host tests
(`fs_server/src/lib.rs`) check hit/miss, write-through freshness, short-write invalidation,
slot-collision safety and multi-block bypass in milliseconds, no emulator.

### Before and after, eight interleaved rounds each

`sh bench/cache-slots-sweep.sh 8 1 64`, on the same shared, noisy machine step 4 was measured on
(`uptime` load 15 to 19). Capacity 1 is not quite "off" (the same block asked for twice in a row
still hits), but the tree walk touches five *different* blocks per call, so a one-slot cache thrashes
across a single walk and rarely survives to the next one; the sweep's own doc names this. Both
`fs::TRANSFER_PAGES` (64 KiB) and `blk::TRANSFER_BLOCKS` (16) are at their step-3/step-4 settings for
both points, so this isolates the cache's marginal contribution on top of everything already shipped.

| phase | 1 slot (~off) | 64 slots | speedup |
|---|---|---|---|
| `fs_read` (control; repeated inline read) | 210,490 ns | **9,474 ns** | **22.2x** |
| `fs_seq_write` | 1,387,898 | **936,583** | **1.48x** |
| `fs_rand_write` | 1,487,736 | **1,087,904** | **1.37x** |
| `fs_seq_read` | 514,419 | **329,168** | **1.56x** |
| `fs_rand_read` | 542,605 | **331,732** | **1.64x** |
| `fs_record_read` | 560,088 | **341,504** | **1.64x** |

**`fs_read`'s 22.2x is the number that most needs its own explanation**, because it is the control
this whole milestone has used to prove there was no cache. `motd` is 69 bytes and lives *inline* in
its node, so a read of it needs no separate record read at all: the tree walk **is** the whole
request. With the cache warm, every read after the first answers from memory, so `fs_read` collapses
to close to the bare IPC/server floor this page has separately estimated at ~13 us; 9,474 ns is
better than that estimate, and the gap is plausibly the cache lookup being cheaper than a `CALL`
plus whatever margin the estimate carried. **This is also the fact that retires the "no cache
anywhere" claim** milestone 38 demonstrated and this page and notes/fs-server.md both stated as an
architectural property: it was true of the build measured then and it is not true of the build this
tree ships now. See notes/fs-server.md's own correction, in place, for what still holds (a
*different* file's first access, or any file's first access in a fresh session, is still fully
uncached) and what does not.

### The combined effect of all four steps, against milestone 38's original baseline

Multiplying the separately-measured per-step ratios would compound noise from four different days
and machine states; a single head-to-head between milestone 38's original number and this tree's
current, fully-stepped build is the honest total. `fs_seq_read`: milestone 38's 1,509,270 ns per
4 KiB request (2.68 MiB/s) against this run's 329,168 ns per 64 KiB request (189.9 MiB/s): **70.9x**,
all four steps combined, on one machine, in one comparable unit (MiB/s, since the transfer size
itself changed at step 3). That is the number for "how much of the 32x-and-then-some this milestone
set out to close has actually closed": most of it, on this metric, and the remaining gap to buffered
Linux (7,141 MiB/s) is the page-cache gap this milestone was never scoped to close (see "What is out
of scope, deliberately", below).

### Crash consistency, re-run at the new geometry

`fs_server/tests/crash_consistency.rs`, unchanged pass: 0 silently wrong, for the same structural
reason step 3's and step 4's re-runs gave: the host-side model drives `BlockDisk<Recording>` below
`CachedDisk`, so the cache is not on that model's path at all. The property that matters at the
**device** level is different and is argued rather than merely re-run: milestone 37's recovery mount
is a **fresh process**, so it constructs a fresh, cold `CachedDisk` and can never observe anything
the killed process's cache held. A cache that somehow survived a process's death would be the
correctness hazard; one that cannot outlive the process that built it is not.

### What was not measured, and is the first thing to run next

**The two caches were not swept against each other.** `blk::TRANSFER_BLOCKS` and `CACHE_SLOTS` were
each swept alone, at the other's shipped value. Whether a smaller blk batch with a larger cache (or
the reverse) reaches the same total for less memory or less DMA-region size is an open question
`bench/cache-slots-sweep.sh` and `bench/blk-transfer-sweep.sh` can both answer, run together, but
nobody has run them together yet.

**64 slots was chosen against the tree spine's own size** (five blocks, times roughly twelve for
several open handles) **and against milestone 37's smaller crash-test heap budget**, not against a
sweep of the capacity itself. `sh bench/cache-slots-sweep.sh N 1 4 16 64 256` would show where the
curve actually bends; the two points measured here are the shipped value and an approximate floor.

### BUGS

- **The machine was loaded for this measurement too**, `uptime` 15 to 19 throughout. The `fs_read`
  control's absolute value (9,474 to 15,107 ns across 8 rounds at 64 slots) is small enough that a
  scheduling hiccup could move it proportionally more than it moves a millisecond-scale phase; the
  headline 22.2x is a median of 8 rounds for that reason, not a single measurement.
- **The cache is not sized against a real deployment's node count**, only against this milestone's
  test fixtures. `notes/fs-server.md`'s own note (from the original "same five blocks" finding) says
  a 65,536-node filesystem's full spine is 259 blocks; 64 slots is comfortably enough for the working
  set *one open file* needs to stay hot, and thrashes if enough distinct files are open at once to
  collide across the tree's shared upper levels. Nobody has measured that case.
- **A collision evicts silently.** Two block numbers that hash to the same slot (`block % 64`) take
  turns being cached; correctness does not depend on the hit rate (a miss is always safe, just
  slower), but a workload that happens to alternate between two colliding blocks gets none of this
  step's benefit and there is no instrumentation that would show a reader why.

## The two controlled comparisons nobody has run (2026-08-19)

**Every filesystem comparison above is uncontrolled**, and this section exists so that is a known
limitation rather than a thing a reader works out. The published pairing is nife-on-RedoxFS against
Linux-on-ext4, where **the operating system and the filesystem differ at once**. A gap can be
attributed to either, which is why milestone 138's answer to *"does this architecture have a
disk-read liability that cannot be overcome"* is assembled from decomposition (the block server is at
parity with raw virtio, the per-request residual is ~13 us, everything else found so far is an
implementation choice) rather than read off one number.

Two comparisons would control it. Both were calef's, on 2026-08-19, and neither has been run.

### Linux-on-ext2 against nife-on-ext2

Holds the **filesystem** constant and leaves the architecture. Wanted when milestone 140's ext2
stratum exists, and argued in that block: the ext2 row isolates the operating system, the nife column
isolates the filesystem, and today's diagonal isolates neither.

Its honest ceiling: our ext2 would be new against a thirty-year-old one, so the result bounds what
the architecture can cost rather than deciding it.

### Redox-on-RedoxFS against nife-on-RedoxFS

**The sharper of the two, and the reason is how we got RedoxFS.** We vendor it, so this is not an
equivalent implementation, it is *the same code*. Every difference is ours: the IPC, the scheduler,
the block driver, the shared-page contract. There is no filesystem-maturity caveat to make, because
it is their filesystem.

It also asks a different question from the Linux comparison. **Linux tells us whether the
architecture is viable; Redox tells us whether we are a good instance of it**, against a project
about a decade older than this one.

**And it has a falsifiable prediction attached, which is what makes it worth running rather than
merely interesting.** `redoxfs::DiskCache` is std-only and is never wrapped around `IpcDisk` here
(measured while identifying the 208 us). Redox has `std`. So Redox is expected to run that cache and
this system is known not to. If Redox is faster by roughly the 195 us the metadata walk costs, that
confirms milestone 138's step 2 from an independent direction. **If it is faster by substantially
more than that, something is wrong somewhere nobody has looked**, and that is the more valuable
outcome.

**Caveats, both directions:**

- **Pin the same RedoxFS revision.** This tree carries five divergences against the vendored engine,
  including step 1's `RECORD_LEVEL_MAX`. Comparing against a different revision measures the
  divergence rather than the operating system.
- **Schemes are not capabilities.** Redox's IPC has different semantics, so "the difference is ours"
  is not the same as "the difference is implementation quality". Some of it is design, and a report
  that elides that is overclaiming in whichever direction the number happens to point.
- **The cost is the setup, not the measurement**: Redox booted at the same tier, same machine model,
  same device, same payload, with the noise control this page already uses. That is the work.
