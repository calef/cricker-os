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
| Secondary stacks (per core) | `SECONDARY_STACKS` in `kernel/src/smp.rs`, `.secondary_stacks` | 64 KiB x MAX_CPUS | guard page below (milestone 90) | whole stack, before `CPU_ON` |
| Kernel thread stacks | `KernelStack` in `kernel/src/thread.rs` | 16 KiB (4 pages) | guard page below | whole stack, at allocation |

The secondary row said `.bss` and **no guard page** when this note was written, and that asymmetry
is what milestone 90 closed; the section below records how, and the numbers it did not change.

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

## The guard page under each secondary stack (milestone 90)

The inventory above found an asymmetry rather than assuming symmetry, and the asymmetry was real: the
boot stack and every kernel thread stack had an unmapped page beneath them, and the per-CPU secondary
stacks did not. A secondary that ran deep did not fault. It wrote over whatever `.bss` sat below,
which is the milestone 3 failure mode (notes/stack.md) on a core that is not the one running the
tests.

**Why it could not just be skipped where it stood.** The stacks were a plain array in `.bss`, and
`map_everything` maps `.data`..`__bss_end` in a **single** call. There was nowhere to put a hole. So
the fix is a move, and the move is what the milestone is: the array now carries
`#[unsafe(link_section = ".secondary_stacks")]`, and each linker script anchors a page-aligned
`(NOLOAD)` region around whatever it emits. The mapper then walks the slots in a loop, mapping only
each stack and never naming the guard, which is the same thing the boot stack's `__stack_guard` gets
by being skipped between `.bss` and `__stack_bottom`.

**The layout, per core** (`kernel/src/smp.rs`):

```
  slot n:  [ guard 4 KiB, unmapped ][ stack 64 KiB, kernel_data ]   stride 68 KiB (0x11000)
```

The region is `MAX_CPUS` slots, page-aligned at both ends, and it sits inside `__image_start`..
`__image_end`, so `image_size` in the arm64 Image header still covers it (the bootloader will not
drop a device tree on a stack) and the direct map still skips it (there is no second, mapped alias of
a guard page). On aarch64 it lands at `__stack_top`, 0x400fc000..0x40140000; on riscv64 at
0x80266000..0x802aa000. `MAX_CPUS` stays in Rust and is **not** written again in either linker
script, which is the drift `cseam` teaches to avoid; a test holds the emitted region against the
reserved one from the other side.

**`(NOLOAD)` is load-bearing, and one line of the linker script explains a quarter megabyte.** A
zero-initialized Rust static in an explicitly named section becomes PROGBITS, and the flat binary
that QEMU loads would then carry 272 KiB of zeroes. Marking the output section `(NOLOAD)` makes it
`SHT_NOBITS` again: the ELF grew by nothing (`objcopy -O binary` still emits 421,888 bytes on
aarch64). The cost of the whole feature is 16 KiB of address space and **zero physical frames**.
Nothing zeroes the region either, which a stack does not need and the paint pass overwrites anyway.

**The proof is a page-table walk, not an overflow.** `every_secondary_stack_sits_on_a_guard_page` (in
`smp.rs`, portable, so it runs on both ISAs) asks the live tables, through the root read back out of
`TTBR1_EL1` / `satp`, for each core's guard page and each side of it: the guard must not translate,
the stack's bottom and top must. Deliberately not a deliberate overflow: a test that faults the
kernel to pass is a test the suite cannot survive, and what would actually go wrong here is someone
mapping the region as one range again, which the walk catches and an overflow test would too, but
without killing the machine. `mmu::verify` checks the same thing per core before installing the map,
where the boot stack's guard has always been checked, so a release build refuses to run on a map that
lost the holes.

**What it does not cover.** A secondary runs on the **coarse boot map** from `secondary_boot` until
`mmu::init_secondary`, and on that map the guard page is inside a 2 MiB block and is mapped. That is
a handful of instructions of Rust, and the boot stack's own guard has exactly the same window; it is
noted here rather than fixed because closing it means fine-grained tables before the MMU is on.

**Sizing was not the finding, and it is not taken here.** The secondaries run at 12% of 64 KiB. The
move does make shrinking cheap in a way it was not before: the size is one constant in `smp.rs`,
slot stride follows it, and nothing else in the image moves, so 16 KiB per secondary (4x the measured
depth, matching the thread stacks) would return 192 KiB of address space and cost one edit. Recorded
as an option; the guard, not the size, was the gap.

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

### The guard-page move changed nothing (milestone 90)

Re-measured after the secondary stacks left `.bss` for their own region, full suite, both ISAs, one
run each (host load average ~8):

| Stack | aarch64, before | aarch64, after | riscv64, before | riscv64, after |
|---|---|---|---|---|
| boot | 53808 | **53808** | 54216 | **54216** |
| core 1 / 2 / 3 | 8504 | **8504** | 8448 | **8448** |
| thread max | 11352 (420 stacks) | **11352 (420 stacks)** | 11672 (415 stacks) | **11672 (415 stacks)** |

Byte for byte, including the paint floors (640 and 1024) and the stack counts. That is the expected
result and it is worth stating why: depth is decided by which calls run, and moving a stack's base
address changes no call. Anything else would have meant the move perturbed the code, and the number
to explain would have been the difference. The suite grew by the two tests this milestone added
(aarch64 223 to 225, riscv64 224 to 226), and even that did not move the boot stack's deepest byte,
which says those tests are nowhere near the deepest chain.

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
| secondary | 16384 | ~2x | something new is running deep on an idle-and-traps stack |
| thread | 14336 | +2664 | some kernel thread is 2 KiB from its guard; the FS-server incident's class |

The margins are deliberately margins over *observed* depth, not fractions of the stack: the
observed spread is a few hundred bytes, so a few thousand bytes of allowance absorbs toolchain
drift while still failing long before the guard page would. If a nightly bump trips one of these
with an honest, reviewed growth, raise the limit with the new measurement in hand; that is the
gate working, not failing.

The secondary row's original entry read "an idle-and-traps stack that has **no guard page**", and
said in the same breath that this assertion was the only tripwire there. Milestone 90 made that
false, and the honest restatement is that all three rows now do the same job: they are the alarm
that fires in the run that *drifts*, tens of kilobytes before the MMU would fire in the run that
dies. That is worth having on top of a guard page, not instead of one, and it is the only one of
the two that a release build does not get.

## BUGS

- Depth reached before `paint_boot_stack` runs (a handful of early-boot frames) and never reached
  again is invisible, bounded below by the printed paint floor.
- A stack whose deepest word happened to store the paint value reads one word shallow.
- The live scans at end of suite are snapshots; a thread that deepens after being scanned is
  under-read by that run. Reaped thread stacks are exact.
- The instrument is `cfg(test)` only: a shell or bench boot measures nothing. The guard pages are
  not: they are in every build, which is what milestone 90 bought.
- **The guards are absent on the coarse boot map**, so a secondary is unprotected between
  `secondary_boot` and `mmu::init_secondary`, and the boot core between `_start` and `mmu::init`.
  Both windows are a few frames deep and neither has ever been the problem, but neither is zero.
- **Nothing checks the guards after boot except the suite.** `mmu::verify` runs once, before the
  map is installed; a later mapping that filled a guard page in (nothing does this today, and the
  mapper refuses to overwrite) would not be noticed until the test build ran.
- The boot core's slot in the region is mapped and never used: `MAX_CPUS` slots exist, one is
  wasted so that slot index can stay CPU id. 68 KiB of address space, no frames.
