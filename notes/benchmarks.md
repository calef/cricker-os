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

## What the icount instrument cannot see

Cache misses, TLB behavior, branch prediction: TCG models none of them, so a change that is
count-neutral but cache-hostile passes `--check` silently. That is the known limit, stated in
the roadmap block too; the `--real` numbers are the net that catches what counts cannot, read by
a human rather than a gate.
