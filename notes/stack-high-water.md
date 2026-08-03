# Stack high-water: measuring kernel stack depth

Milestone 84. The FS-server stack bug (notes/crickerfs.md, notes/fs-server.md) was a kernel stack
overflow found the expensive way, and until this instrument existed nothing measured depth on any
kernel stack: "the stacks are big enough" was an argument. This note records the instrument, the
inventory it covers, and the numbers it measured.

## The instrument

The classic watermark, because the classic one is right: paint every kernel-owned stack with a
64-bit pattern (`0x5AFE_57AC_5AFE_57AC`) before anything uses it, run the suite, then scan each
stack upward from the bottom for the first word that is no longer the pattern. Bytes between there
and the top are the stack's high-water mark. The scan is an iterative loop with no locals of size,
so it needs no meaningful depth itself.

Test builds only, deliberately. Painting 16 KiB on every thread spawn would perturb the spawn
benchmark, and the report goes through the test output channel anyway. The code is in
`kernel/src/stack.rs` (paint, scan, report), with call sites in `kernel_main` (boot stack),
`smp::bring_up_secondaries` (secondary stacks), and `thread::KernelStack` (thread stacks, painted
at allocation, scanned in `Drop`); `sched::scan_live_thread_stacks` covers the stacks nothing ever
reaps. All of it is portable code; the only arch-specific piece is `arch::current_sp()`, which
already existed for the canary.

## The inventory, from the linker scripts and boot code

| Stack | Where declared | Size | Guarded? | Painted |
|---|---|---|---|---|
| Boot stack (boot core) | `link-aarch64.ld` / `link-riscv64.ld`, `__stack_bottom`..`__stack_top` | 64 KiB | guard page below | at `stack::init` time, canary to a margin below live `sp` |
| Secondary stacks (per core) | `SECONDARY_STACKS` in `kernel/src/smp.rs`, `.bss` | 64 KiB x MAX_CPUS | **no guard page** | whole slot, before `CPU_ON` |
| Kernel thread stacks | `KernelStack` in `kernel/src/thread.rs` | 16 KiB (4 pages) | guard page below | whole stack, at allocation |

There are no separate interrupt or exception stacks on either ISA, verified in the arch code rather
than assumed: aarch64's `vectors.s` builds its 272-byte frame on `SP_EL1`, which is whatever kernel
stack was live (the hardware banks `SP_EL0` away, so a user program's `sp` never enters into it);
RISC-V's `trap.s` stays on the interrupted `sp` for an S-mode trap and switches to the thread's
kernel-stack top (via the per-hart `sscratch` stash) for a U-mode trap. So trap depth lands on, and
is measured as part of, whichever stack above the trap interrupted.

The boot core's slot in `SECONDARY_STACKS` exists and is never used (the boot core runs on the
linker-script stack); the report skips it. On RISC-V the boot hart is whichever one OpenSBI's
lottery picked, so "the boot core's slot" is not always slot 0, and the skip follows
`arch::boot_cpu_id()`.

## Honest limits

- **A watermark sees only exercised paths.** An unexercised deep path stays invisible, the same
  limit coverage has. The static complement (`-Zemit-stack-sizes` worst-case accounting) breaks on
  indirect calls and has not been built.
- **The boot stack has a floor.** It is painted from `kernel_main`, a few frames deep, up to a
  512-byte margin below the live `sp` (the margin keeps the paint loop's own callee frames, real
  calls in a debug build, out of the painted region). Depth used before that moment and never
  reached again is invisible, and no measured value can come out below the floor. The report prints
  the floor next to the number.
- **A frame whose deepest word happens to equal the paint pattern** reads one word shallow. A
  64-bit pattern makes this vanishingly unlikely.
- **Live-stack scans race their owners.** A secondary or a live thread may deepen its stack after
  the scan passes; the snapshot is a lower bound taken at end of suite. Reaped thread stacks are
  scanned in `Drop`, after the owner is provably off them, so those are exact.

## Why the numbers are host-load-immune

Depth is a property of the code and the suite: the same calls push the same frames whether the host
is idle or thrashing. The one timing-dependent contribution is where on a stack an interrupt's
frame lands, which varies with interrupt arrival, so the numbers jitter by roughly a trap frame
plus the handler path, not with runner load. The measured spread across runs is below, and the
assertion margin has to cover it.

## The numbers

Debug build (the test profile), QEMU `virt`, `-smp 4`, full suite (223 tests on aarch64).

| Stack | aarch64 run 1 | aarch64 run 2 | riscv64 run 1 | size |
|---|---|---|---|---|
| boot | 53808 (82%) | 53808 (82%) | 54216 (82%) | 65504 painted |
| core 1 | 8504 (12%) | 8504 (12%) | 8448 (12%) | 65536 |
| core 2 | 8504 (12%) | 8504 (12%) | 8448 (12%) | 65536 |
| core 3 | 8504 (12%) | 8504 (12%) | 8448 (12%) | 65536 |
| thread max | 11352 (69%, 420 stacks) | 11352 (69%, 420 stacks) | 11672 (71%, 415 stacks) | 16384 |

Boot paint floor: 640 bytes (aarch64), 1024 bytes (riscv64); every measured boot number is far above
its floor, so the floor is not what is being read.

A second riscv64 run (the gate run for the assertion below) reproduced its column exactly, from a
*different boot hart*: OpenSBI's lottery booted hart 3 rather than hart 0, the report skipped the
boot hart's unused slot as designed, and the three secondary numbers were 8448 again.

Three things the table says beyond the values. **The numbers are exactly reproducible**: the two
aarch64 runs agree byte for byte on every stack, including all three secondaries, and they were taken
under host load averages of 33 and 9 (a concurrent cargo-mutants lane was saturating all eight cores
during the first). Depth really is a property of the code and the suite, not the runner; the
interrupt-timing jitter the design worried about does not reach the deepest byte on this suite.
**The two ISAs agree to within about 400 bytes** on every stack, which is what "same code, same
suite, different frame layouts" should produce. And **the boot stack is at 82%**, much closer to its
guard page than anything else in the kernel; if the suite's deepest test chain grows, the boot stack
is where the growth lands (see the gate below for what fails first).

The boot stack is the deep one, and the reason is structural: `test_main` runs on the boot
context, so the boot stack carries the deepest call chain of the entire suite, every test body
included. The secondary stacks carry only idle loops, trap frames, work stealing, and the SMP
probes. Thread stacks carry every spawned kernel thread and every process's kernel side.

The FS server's *user* stack has its own watermark already (`the_fs_servers_stack_still_has_headroom`,
in `kernel/src/user/tests.rs` and its RISC-V twin in `riscv_virtio_tests.rs`); this instrument is the
kernel-stack complement.

## The gate

The numbers were stable enough to gate on immediately (identical runs under a 3.5x load difference;
~400-byte cross-ISA spread), so the threshold assertion landed in the same milestone, checked in
`report_high_water` after the printing so a trip always arrives with its numbers. One shared set of
limits on both ISAs, per the parity gate:

| Stack | limit | over observed max | what a trip means |
|---|---|---|---|
| boot | 61440 | +7224 (13%) | the suite's deepest chain grew ~7 KiB; one page left before the guard |
| secondary | 16384 | ~2x | something new is running deep on an idle-and-traps stack that has **no guard page** |
| thread | 14336 | +2664 | some kernel thread is 2 KiB from its guard; the FS-server incident's class |

The margins are deliberately margins over *observed* depth, not fractions of the stack: the
observed spread is a few hundred bytes, so a few thousand bytes of allowance absorbs toolchain
drift while still failing long before the guard page would. If a nightly bump trips one of these
with an honest, reviewed growth, raise the limit with the new measurement in hand; that is the
gate working, not failing.

## BUGS

- Depth reached before `paint_boot_stack` runs (a handful of early-boot frames) and never reached
  again is invisible, bounded below by the printed paint floor.
- A stack whose deepest word happened to store the paint value reads one word shallow.
- The live scans at end of suite are snapshots; a thread that deepens after being scanned is
  under-read by that run. Reaped thread stacks are exact.
- The instrument is `cfg(test)` only: a shell or bench boot measures nothing.
