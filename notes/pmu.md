# The PMU, and the two clocks in an aarch64 core

The **Performance Monitoring Unit (PMU)** is counting hardware built into the CPU core, separate from
the part that runs instructions. Its job is to tally low-level events as the core executes: clock
cycles, instructions retired, cache misses, branch mispredictions, TLB misses, and dozens more. It is
the core keeping score on itself.

Picture a bank of hardware counters wired into the pipeline. Each can be told "count event 0x08"
(instructions retired) or "count event 0x03" (L1 data cache misses), and it ticks every time that
event fires, at hardware speed, with no software in the loop. This is the machinery behind `perf` on
Linux and Instruments on macOS.

## The counter that matters here: the cycle counter

aarch64 gives the PMU one always-present counter, `PMCCNTR_EL0`, that counts **CPU clock cycles**. On
an Apple M-series core near 4 GHz that is one tick roughly every **0.25 ns**. You read it with a single
`mrs` (move-from-system-register), which itself costs only a handful of cycles.

That resolution and that cheapness are why cycle-accurate microbenchmarks reach for it. To time one
operation:

```
t0 = read PMCCNTR
do_the_thing()        // one syscall, one IPC, one page map
t1 = read PMCCNTR
cost = t1 - t0        // cycles, resolvable to nearly a single cycle
```

A whole seL4 IPC is only ~200-400 cycles, so a single-shot measurement of it *needs* this resolution.
That is exactly how `sel4bench` works (notes/benchmarks.md), and exactly why it could not run on this
Mac.

## Two clocks, and why the difference is the whole story

An aarch64 core has two unrelated counters, and confusing them is a category error:

| | PMU cycle counter (`PMCCNTR_EL0`) | Generic timer (`CNTVCT_EL0` / `CNTPCT_EL0`) |
|---|---|---|
| counts | CPU clock cycles | a fixed reference tick |
| rate | the CPU clock (~4 GHz), and it **varies** with frequency scaling | fixed, advertised in `CNTFRQ_EL0` (24 MHz here) |
| resolution | ~0.25 ns | ~41 ns |
| what it is for | profiling, microbenchmarks | wall-clock timekeeping |

The **generic timer** is the OS's clock: a steady reference tick used to tell time and schedule
deadlines (`CNTPCT`, `CNTP_CVAL`; see interrupts.md). It is what our own bench reads, through
`user_rt::now` at EL0 (abi.md opened `CNTKCTL_EL1.EL0VCTEN` for exactly this). It is coarse, ~41 ns per
tick, so one IPC reads as "1 tick, maybe 2." We beat the coarseness by timing a **loop of thousands**
of operations and dividing; the per-op cost falls out cleanly and the tick noise averages away.

The PMU cycle counter is the opposite trade: fine enough to time a single operation, but it counts
*cycles*, not time, and the cycle rate moves with clock scaling (DVFS), so turning cycles into
nanoseconds needs the current frequency, which is not fixed.

Two ways to measure a fast operation, then: **one shot at high resolution** (PMU, sel4bench) or **a
long loop at low resolution** (generic timer, ours). Both are valid; they fail under different
conditions.

## Why virtualization keeps the PMU out of reach

The generic timer is *architected* state, part of the CPU's published contract, so a hypervisor is
expected to pass it through, and Apple's HVF does: `CNTVCT` reads work fine inside a guest. That is why
our bench runs under QEMU-HVF at all.

The PMU is different. It is microarchitectural, core-private, and awkward to expose safely: it can leak
information across VM boundaries, and it is real work to save and restore across guest switches. So it
is commonly left unvirtualized:

- **QEMU-TCG** (pure emulation, our deterministic `icount` mode) has no real cycles to count, it just
  translates code, so `PMCCNTR` returns quantized junk (we saw 0 and 1000).
- **Apple HVF** does not virtualize the guest PMU, so a guest's `PMCCNTR` reads are unstable.

Either way a single-shot cycle measurement has no usable clock. This is why `sel4bench` (single-shot,
PMU) cannot produce numbers on this Mac while our bench (long loop, generic timer) can, and it is the
same constraint design/roadmap.md flags for any plan that wants real cycle counts: they wait on real
silicon. A Raspberry Pi has a real PMU because it *is* real silicon, not a guest, which is where the
seL4 comparison goes (milestone 24).

The lesson worth keeping: **the coarse, boring generic timer is the one that survives virtualization.**
Choosing it plus long loops, back at milestone 19e, is what makes our cross-OS numbers possible on a
laptop instead of only on hardware. See notes/benchmarks.md for how the two instruments are used, and
notes/abi.md for how EL0 got read access to the generic timer.
