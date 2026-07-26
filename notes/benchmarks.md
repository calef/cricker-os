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
benchmarks (null syscall, context switch, IPC round-trip, all measured the lmbench way) build on the
same `user_rt::now`. The existing kernel-side suite stays, for gating; the EL0 suite is additive, for
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

Two sanity checks pass. A context switch is ~16x a null syscall (two traps, the scheduler, two
register save/restores, and a TTBR0/ASID change, versus one bare trap). And the round trip lines up
against its parts: ~two context switches (2 × 692) plus four traps (4 × 42) plus dispatch ≈ 2272.

The EL0 round trip also has a kernel-side twin, the milestone-21 `ipc_rtt` (~951 ns), which measures
the same rendezvous *without* the EL0↔EL1 crossings. The ~1.3 µs gap between them is exactly the trap
cost of the four `svc`s a real round trip pays, which is the reason the EL0 numbers, not the
kernel-side ones, are what compare to lmbench. All debug builds; the cross-OS comparison wants release
builds on all sides. These line up against lmbench's `lat_syscall` / `lat_ctx` / `lat_pipe` and
`sel4bench`.

### The first cross-OS numbers (cricker-os vs Linux vs macOS)

`bench/host/` holds the host side of each metric: `null_syscall.rs` (a raw `getpid` through the
syscall gate, not libc's cached `getpid` which never traps) and `ipc_rtt.rs` (a pipe round trip
between two forked processes, lmbench's `lat_pipe`). Two ways to run them: natively on macOS
(`rustc -O ... && ./bin`), and on **Linux at the same tier** as cricker-os, `bench/host/run_linux.sh`
cross-compiles a static musl binary, packs it as `/init` in a one-file initramfs, and boots it under
QEMU-HVF, the exact machine cricker-os boots on. So Linux and cricker-os sit on the **same M-series
core at the same virtualization tier**; native macOS is the bare-metal ceiling.

Run cricker-os optimized (`cargo xtask bench --release`, which builds an opt-level-3 kernel and
userspace and implies `--real`), and compare on the same core:

| metric | cricker-os **release** (HVF) | Linux (static musl, HVF) | macOS/XNU (native) |
|---|---|---|---|
| null syscall | **~27 ns** | ~139 ns | ~76 ns |
| context switch (per switch, derived) | **~28 ns** | ~415 ns | ~818 ns |
| IPC round trip | **~337 ns** | ~1723 ns | ~2620 ns |

**cricker-os wins all three, and the two clean ones win decisively**: same M-series core, same HVF
tier as Linux, both optimized. It is **~5x faster than Linux at the null syscall** (27 vs 139) and
**~5x faster at the IPC round trip** (337 vs 1723), and it beats native macOS at both. These are
seL4-class microkernel numbers, an IPC round trip in the low hundreds of nanoseconds, put next to the
reference OS on the same silicon.

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
