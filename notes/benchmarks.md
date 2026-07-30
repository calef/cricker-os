# Benchmarks with teeth

*(Milestone 21. `script/bench`, `kernel/src/bench.rs`, and `bench/baseline.txt`.)*

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
changes that introduce them" structurally: `bench/baseline.txt` is committed, `--check` fails on
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

## The first real numbers, for the record (2026-07-23, M-series host, HVF)

IPC round trip ~705 ns; call/reply ~886 ns; yield round trip ~437 ns; spawn-to-reap ~2.8 µs;
fresh-page map ~634 ns. Statistical, single run, shared machine: shapes, not gospel. The 24 MHz
counter grain (~42 ns) means per-iteration ticks are coarse; totals over 1000+ iterations are
what to read. Cycle-exact PMU numbers arrive with milestone 16's real silicon, which inherits
this harness and swaps the clock.

## Calibration: what these numbers mean next to L4's

IPC cost is *the* microkernel number because IPC multiplies through the whole architecture:
Mach's ~100 us IPC discredited microkernels in the 1980s, and Liedtke's L4 rehabilitated them
with ~250-cycle IPC on a 486, the "sub-microsecond" banner seL4's few-hundred-cycle fastpath
still carries. Our ~705 ns round trip sounds like that club; two corrections before believing it:

1. **Count cycles, not nanoseconds.** At ~3.2 GHz, 705 ns is ~2,200 cycles round trip, where an
   L4-lineage fastpath does 300 to 600. Per cycle we are 4 to 7 times heavier, and honestly so:
   we take the fully general path every time (scheduler lock, proved rendezvous, generational
   Tid checks) and have deliberately built no fastpath. The nanoseconds look good because the
   silicon is a monster.
2. **Our bench excludes what theirs includes.** L4's numbers are user-to-user, traps included.
   Ours ping-pongs kernel threads calling `sched::ipc_*` directly: no `svc`, no exception
   entry, no trap frame. A true EL0-to-EL0 benchmark (the right follow-up; it needs one
   `CNTKCTL_EL1` bit so EL0 can read the counter) will measure meaningfully higher.

The hypervisor's tax on this particular path is small (no devices touched, so essentially no VM
exits; the cost is indirect, via stage-2 TLB pressure and host cache pollution), which is
precisely why the bench loops keep devices out. What the comparison legitimately supports: the
Mach failure mode is nowhere in sight, the architecture is viable at this price on the general
path, and whether a fastpath is ever worth its complexity is now a question for these
measurements rather than for L4 envy.

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
(CoreMark, Dhrystone) run on cricker-os, macOS, and Linux on the same core comes out *nearly
identical*, and the small gaps are compiler codegen or allocator noise, not OS quality. That is a
real result ("we add no hidden compute overhead") but a null one by design. It cannot show OS
strengths or liabilities, because the OS is not in the loop.

**OS primitives are where an OS shows itself.** Syscall entry, context switch, IPC round-trip, page
map, page fault, thread spawn: these *are* the OS, and they are what distinguish Linux from macOS
from us. But the same source cannot measure them across three OSs, because "the same syscall" does
not exist on all three: you invoke each OS's own primitive (`getpid` on Linux, a Mach/BSD call on
macOS, our `svc` null-invoke). So the OS-revealing benchmark is a **matched harness per OS** (one
metric definition, three native implementations), which is exactly what lmbench is and how the
L4/seL4 papers compare to Linux. Our own microbenchmarks above are the cricker-os side of it.

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
identical source compiles for cricker-os, macOS, and Linux, so the compute run is one program on
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

### The first EL0 numbers (cricker-os, M-series host, HVF, debug build)

The `elbench` program (`user/src/elbench.rs`), spawned by the bench boot, self-times each primitive
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

The EL0 round trip also has a kernel-side twin, the milestone-21 `ipc_rtt` (~951 ns), which measures
the same rendezvous *without* the EL0↔EL1 crossings. The ~1.3 µs gap between them is exactly the trap
cost of the four `svc`s a real round trip pays, which is the reason the EL0 numbers, not the
kernel-side ones, are what compare to lmbench. All debug builds; the cross-OS comparison wants release
builds on all sides. These line up against lmbench's `lat_syscall` / `lat_ctx` / `lat_pipe` and
`sel4bench`.

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
which is the same silicon and the same bandwidth for all three. cricker-os's lean mechanism is real (the
91 ns), but on the operation an application actually pays for, getting a usable page, it does not and
cannot win, because the win would have to come from zeroing memory faster than the other two, and nobody
can. A fair EL0 map that *does* provision a fresh page waits on retype-from-untyped reaching userspace
(a later milestone); until then the kernel-side `map_new` is the honest stand-in for the comparison.

### The first cross-OS numbers (cricker-os vs Linux vs macOS)

`bench/host/` holds the host side of each metric: `null_syscall.rs` (a raw `getpid` through the
syscall gate, not libc's cached `getpid` which never traps), `ipc_rtt.rs` (a pipe round trip between
two forked processes, lmbench's `lat_pipe`), `ctx_switch.rs` (the derived context switch),
`mmap.rs` (first-touch fault-in, lmbench's `lat_mmap`), and `spawn.rs` (fork+exit, lmbench's
`lat_proc`). Two ways to run them: natively on macOS
(`rustc -O ... && ./bin`), and on **Linux at the same tier** as cricker-os, `bench/host/run_linux.sh`
cross-compiles a static musl binary (`linux_all.rs`, the five metrics combined), packs it as `/init`
in a one-file initramfs, and boots it under QEMU-HVF, the exact machine cricker-os boots on. So Linux
and cricker-os sit on the **same M-series core at the same virtualization tier**; native macOS is the
bare-metal ceiling.

Run cricker-os optimized (`cargo xtask bench --release`, which builds an opt-level-3 kernel and
userspace and implies `--real`), and compare on the same core:

| metric | cricker-os **release** (HVF) | Linux (static musl, HVF) | macOS/XNU (native) |
|---|---|---|---|
| null syscall | **~27 ns** | ~139 ns | ~76 ns |
| context switch (per switch, derived) | **~28 ns** | ~415 ns | ~818 ns |
| IPC round trip | **~337 ns** | ~1723 ns | ~2620 ns |
| map a fresh page (provision + map) | ~552 ns (`map_new` + trap) | ~534 ns | ~556 ns |
| map mechanism only (aliased, no zeroing) | ~91 ns (`map_el0`) | n/a (fault always zeroes) | n/a |
| spawn (build + run + reap + reclaim) | **~7.7 µs** (`spawn_el0`) | ~19.7 µs (fork+exit) | ~291 µs (fork+exit) |

**cricker-os wins four and ties one, and saying which is which is the point.** Same M-series core, same
HVF tier as Linux, both optimized. It is **~5x faster than Linux at the null syscall** (27 vs 139) and
**~5x faster at the IPC round trip** (337 vs 1723), it beats native macOS at both, and it builds a
process faster than either (spawn, below). These are seL4-class microkernel numbers, an IPC round trip
in the low hundreds of nanoseconds, next to the reference OS on the same silicon. **Map is a deliberate
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
the parent (its address space copy-on-write, its descriptor table, its signal state), where cricker-os
**builds a fresh minimal process from nothing**. A capability-microkernel process is a lighter object
than a Unix one, so the gap is mostly that structural difference, not a faster version of the same work.
We use `fork`+`exit`, not `fork`+`exec`, precisely to keep the Unix side as light as it gets (no binary
loaded); it still carries the weight of duplication that cricker-os's from-scratch build does not. The
number stands, with its meaning stated: building a process is cheap when a process is a small thing.

The **context switch** is the softest of the three and its number the least load-bearing. No OS lets
you time a bare switch, so it is *derived*: on the host, `bench/host/ctx_switch.rs` measures a
two-process pipe round trip (two switches plus two pipe passes) and subtracts a self-pipe pass (a
`write`+`read` with no switch), leaving one switch = `round_trip/2 - self_pipe`. cricker-os's
`ctx_switch` bench is a yield round trip (two switches plus two `SYS_YIELD`s); subtracting the trap
(`~2 x null_syscall`) leaves ~28 ns per switch. The subtraction is approximate and the *mechanisms
differ* (our lightweight yield versus a pipe pass), so read the ~15x gap to Linux as directional, not
exact. It points the same way the other two do, and that consistency, three metrics, three methods,
all favoring the minimal kernel, is the real signal.

The story the debug build told first was the *opposite* at IPC, and the gap between them is the whole
lesson. Debug cricker-os: null syscall ~42 ns, ctx switch ~692 ns, IPC ~2272 ns. So `-O0` was a ~1.5x
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
port (design/roadmap.md milestone 24): a Raspberry Pi has a real PMU, sel4bench runs on it natively, and
it is the board cricker-os is heading toward anyway. The build recipe, reproducible when a Pi is on hand
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
  on both), **`sel4bench`** for seL4. We write the cricker-os side (the microbenchmarks above,
  extended to match the metric set), not the whole thing.
- **The peers.** seL4 is the direct one: a capability microkernel that targets the *same* QEMU
  `aarch64 virt` machine we do, so it runs on the identical instrument (QEMU-HVF) and publishes
  comparable cycle counts. L4Re/Fiasco and Genode are more effort for less marginal insight.
- **Match the virtualization tier.** QEMU with `-accel hvf` *is* virtualization (Hypervisor.framework
  on the real core), not emulation, so cricker-os and Linux run virtualized under QEMU-HVF; macOS runs
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
runner defaults to `-smp 4` (`CRICKER_SMP:-4`, matching the SMP tests), and the bench never overrode
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

The riscv bench path already pins `CRICKER_SMP=1`, with a comment describing this exact failure
("a `wfi` jumps virtual time to the next timer tick, inflating the spawn primitives to
timer-quantized nonsense"). That fix landed with the riscv icount bench (commit 494514b) and was
never mirrored to aarch64. So the aarch64 icount instrument has been measuring four-hart noise since
milestone 21; the old baseline was noise too, internally consistent enough to pass `--check` until a
day of merges moved the interleaving far enough to trip it. **The fix is one line**, the aarch64
icount path now sets `CRICKER_SMP=1` like riscv, and the baseline is re-saved at single hart. Real
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

**They never gate, and never touch `bench/baseline.txt`.** Two structural reasons. First, they run
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
same-machine seL4 number is deferred to real hardware: the instrument underneath, not cricker-os, sets
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
CALLs the block server (`client -> fs -> blk -> fs -> client`), netd CALLs the NIC driver
(`client -> netd -> driver -> netd -> client`). `relay_rtt` (kernel-side, `bench.rs`) is exactly that
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

**`fs_read`: the real RedoxFS read, whole path, and why it cannot be the isolated number.** This is
the flagship: a client opens a file through a granted **directory capability** and reads a block, over
the real confined stack (a block server driving the RedoxFS disk by DMA, the vendored RedoxFS engine
mounting it over blk IPC on its own heap). It runs on the `--real --smp` boot, where the whole stack
is proven by the fs-server test, and it reports:

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
self-skips (the `online_count() > 1` gate) everywhere but `--real --smp`, so `bench/baseline.txt`
never sees it.

**netd's socket round trip: measured, but not as a third icount bench, and here is why.** The net
path has the same shape as the FS path (a confined server the client reaches only through a granted
`Stack` capability), and its per-request IPC tax is the same `relay_rtt` topology. But a netd
*socket* round trip is even less gate-able than `fs_read`: netd only reaches its serve loop after a
DHCP handshake, and its RECV path drives smoltcp's own retransmit and delay-ACK timers (notes/net.md),
so the path is DHCP- and timer-driven, deterministic under neither `-icount` nor, at the socket level,
even a warm HVF loop. So netd's socket contract is proven and timed end to end by the existing net
tests (`a_client_resolves_dns_through_the_socket_contract`, `a_client_echoes_over_tcp_...`, both ISAs,
both transports), not duplicated as a bench that could only report device-and-timer latency. The bare
EL0 round trip those build on, `ipc_rtt_el0` above, is the raw baseline; the `relay_rtt` delta is the
confined-server tax netd pays on top of it, the same as the FS server. Recording it this way, one
gated topology tax plus the two real servers measured where each is sound, is the honest fit to what
the two instruments can and cannot see.
