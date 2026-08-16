# Stack high-water: measuring kernel stack depth

Milestone 84. The FS-server stack bug (notes/nifefs.md, notes/fs-server.md) was a kernel stack
overflow found the expensive way, and until this instrument existed nothing measured depth on any
kernel stack: "the stacks are big enough" was an argument. This note records the instrument, the
inventory it covers, and the numbers it measured.

## What "high water" means, because it inverts twice

In plain words, **`high_water` is the most bytes the stack ever used**. Not its depth now, not the
space still free: the maximum it ever reached, at some instant nobody was watching.

That is the whole reason painting works. A stack rises and falls thousands of times a second and no
sampling scheme would catch the peak. But the deepest frame *destroys the paint* at its low point,
and that damage outlives the frame by the whole run, so one scan afterwards recovers a maximum
nobody had to observe.

```text
  top     (high address)   <- sp starts here
    |
    |   overwritten: the paint is gone
    |   <- high_water measures THIS span
    |
    p     <- deepest any frame ever reached
    |
    |   still painted: nothing ever came this far
    |   <- this is the headroom
    |
  bottom  (low address)
```

Two things flip in the reading, which is why this section exists:

- **The painted bytes are the ones that were never used.** Paint is evidence of *absence*.
  `high_water` counts the bytes where the paint is **gone**.
- **Stacks grow down, and the number counts up.** The deepest point is the *lowest* address, and
  `high_water` returns `top - p`, a magnitude, so bigger means deeper means closer to the guard
  page.

The shortcut worth remembering: **paint left is room to spare.** The boot stack's 53,808 of 65,504
means 53,808 bytes were used at the deepest moment and 11,696 bytes still hold
`0x5AFE_57AC_5AFE_57AC` and never saw a frame.

## The instrument

The classic watermark, because the classic one is right: paint every kernel-owned stack with a
64-bit pattern (`0x5AFE_57AC_5AFE_57AC`) before anything uses it, run the suite, then scan each
stack upward from the bottom for the first word that is no longer the pattern. Bytes between there
and the top are the stack's high-water mark. The scan is an iterative loop with no locals of size,
so it needs no meaningful depth itself.

Test builds only, deliberately. Painting 24 KiB on every thread spawn would perturb the spawn
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
| Kernel thread stacks | `KernelStack` in `kernel/src/thread.rs` | 24 KiB (6 pages; 16 KiB until 2026-08-15) | guard page below | whole stack, at allocation |
| Interrupt stacks (per core) | `kernel/src/interrupt_stack.rs`, `.interrupt_stacks` | 16 KiB x MAX_CPUS | guard page below | whole region, at `interrupt_stack::init` |

The secondary row said `.bss` and **no guard page** when this note was written, and that asymmetry
is what milestone 90 closed; the section below records how, and the numbers it did not change.

**The last row is new on 2026-08-16 and this paragraph used to say the opposite**, so it is worth
being exact about what changed rather than editing the claim away. Until milestone 124 there were no
separate interrupt or exception stacks on either ISA, verified in the arch code rather than assumed:
aarch64's `vectors.s` built its 272-byte frame on `SP_EL1`, which is whatever kernel stack was live
(the hardware banks `SP_EL0` away, so a user program's `sp` never enters into it), and RISC-V's
`trap.s` stayed on the interrupted `sp` for an S-mode trap. So trap depth landed on, and was measured
as part of, whichever stack the trap interrupted, which is exactly how a preemption came to be billed
to the thread it preempted (notes/stack.md).

**Both halves of that are still true of the trap frame**, which is the part to keep: the frame is
still built on the interrupted stack, because a preempted thread's frame must survive until that
thread runs again. What moved to the new row is the handler above the frame, on a trap taken from
kernel mode. A trap from user mode does not switch at all.

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
a guard page). On aarch64 it lands at `__stack_top`, 0x4010c000..0x40194000; on riscv64 at
0x80272000..0x802fa000 (eight slots since the 2026-08-14 `MAX_CPUS` bump; the ranges here were
0x400fc000..0x40140000 and 0x80266000..0x802aa000 at four). `MAX_CPUS` stays in Rust and is **not**
written again in either linker
script, which is the drift `cseam` teaches to avoid; a test holds the emitted region against the
reserved one from the other side.

**`(NOLOAD)` is load-bearing, and one line of the linker script explains half a megabyte.** A
zero-initialized Rust static in an explicitly named section becomes PROGBITS, and the flat binary
that QEMU loads would then carry 544 KiB of zeroes. Marking the output section `(NOLOAD)` makes it
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

Re-measured after the secondary stacks left `.bss` for their own region, full suite, both ISAs, two
runs each (host load average ~8):

| Stack | aarch64, before | aarch64, after | riscv64, before | riscv64, after |
|---|---|---|---|---|
| boot | 53808 | **53808** | 54216 | **54216** |
| core 1 / 2 / 3 | 8504 | **8504** | 8448 | **8448** |
| thread max | 11352 (420 stacks) | **11352 (420 stacks)** | 11672 (415 stacks) | **11672 (415 stacks)** |

Byte for byte, including the paint floors (640 and 1024) and the stack counts, and byte for byte
across the two runs of each ISA as well. OpenSBI's lottery picked hart 0 for one riscv64 run and
hart 3 for the other, so the second run's three numbers came from *different harts on different
slots of the new region*, and were 8448 again. That is the same cross-hart reproduction milestone 84
saw, now over the moved stacks.

The stability is the expected result and it is worth stating why: depth is decided by which calls
run, and moving a stack's base address changes no call. Anything else would have meant the move perturbed the code, and the number
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
| thread | 18432 (14336 until 2026-08-15) | +6760 over observed; ~+3 KiB over worst-case stacking | some kernel thread is 6 KiB from its (24 KiB stack's) guard |

The margins are deliberately margins over *observed* depth, not fractions of the stack: the
observed spread is a few hundred bytes, so a few thousand bytes of allowance absorbs toolchain
drift while still failing long before the guard page would. If a nightly bump trips one of these
with an honest, reviewed growth, raise the limit with the new measurement in hand; that is the
gate working, not failing.

**The thread limit is the one that has to be sized against stacking, not against the observed
number, and the 2026-08-15 CI overflows are why** (the full story is in notes/stack.md). The
observed high-water is what the suite's runs happened to catch; the honest worst case is the
deepest standing path (~11.7 KiB) plus a blocked thread's resident residue (`ipc_recv` +
`SCHED.lock` + `schedule` + the switch, ~1.4 KiB) plus one preemption landing at the deepest
instant (~2.3 KiB), about 15.5 KiB, which is why two CI runs overflowed a 16 KiB stack that a
green high-water report said was 71% used. A loaded host does not change any depth; it multiplies
timer interrupts per guest instruction until one lands on the worst-case alignment. The instrument
measures truly; it just only measures the alignments that occurred. This is the same lesson as the
reap-frame incident one section down, one level up: there a single frame outran the margin, here
the *sum of independent layers* did, and neither is visible in a single row of this table.

The secondary row's original entry read "an idle-and-traps stack that has **no guard page**", and
said in the same breath that this assertion was the only tripwire there. Milestone 90 made that
false, and the honest restatement is that all three rows now do the same job: they are the alarm
that fires in the run that *drifts*, tens of kilobytes before the MMU would fire in the run that
dies. That is worth having on top of a guard page, not instead of one, and it is the only one of
the two that a release build does not get.

## The other half of the instrument: what one frame costs, statically (2026-08-13)

A watermark says how deep the suite **went**. It cannot say which function is expensive, and that is
the question you have when a stack overflows, because the fix is either "raise the limit" or "shrink
the offender" and the watermark does not distinguish them.

The compiler will tell you, and it needs no emulator, which matters because it works from a laptop or
a container with no QEMU:

```sh
RUSTFLAGS="-Z emit-stack-sizes" \
  cargo test -p kernel --target aarch64-unknown-none-softfloat --no-run
# then read the .stack_sizes section of the reported artifact:
#   llvm-objcopy --dump-section .stack_sizes=ss.bin <artifact> /dev/null
#   llvm-nm -C --defined-only -S <artifact>        # to name the addresses
```

Each entry is a function address and its frame size in bytes. **Measure the test build, not
`cargo build`**: half the kernel's spawn paths and every test body are `cfg(test)`, so the plain
binary is missing exactly what you are chasing. That mistake cost an hour on 2026-08-13, and the tell
was `llvm-nm | grep <a test-only symbol>` returning nothing.

### What it found, and why the 71% row above was a warning nobody read

The deepest frame in the kernel was **`sched::reap_region_objects` at 6816 bytes**, of which 6144 was
three scratch arrays sized to their table maxima:

| local | size |
|---|---|
| `doomed: [u64; MAX_THREADS]` | 1024 |
| `doomed_eps: [u64; MAX_ENDPOINTS]` | **4096** |
| `waiters: [u64; MAX_THREADS]` | 1024 |

Now put that next to this note's own thread-stack row. The measured high-water was **11672 of 16384
bytes, 71%**, leaving **4712 bytes**. The reap frame wanted **6816**, which is **2104 bytes more than
the entire remaining headroom**. Any chain that reached the measured peak and then entered a reap
could not fit, and would land on the guard page.

That is what happened. Milestone 108's branch faulted one CI run in five with `FAR_EL1` exactly on
the guard page of thread stack slot 87, and the tests running were the supervision and reap ones. The
branch was held on suspicion of having introduced it; the static measurement says otherwise, because
comparing its test binary against `main`'s function by function shows **the largest single frame
growth in the whole milestone is 128 bytes**. It added one more spawned program to a margin that was
already 2104 bytes short.

**The 71% row had been sitting in this note since milestone 84.** A percentage reads as comfortable,
and 4712 bytes of headroom reads as comfortable, right up against a single frame that needs more than
all of it. The lesson is that a high-water percentage and a frame inventory answer different
questions and neither is safe alone.

### The fix, and the shape worth copying

`doomed_eps` existed because `remove` mutates the table and you cannot remove while iterating it, so
the names were collected first. Rescanning for one at a time removes the array entirely: the frame
went **6816 to 2560 bytes**, and it now fits inside the measured headroom with 2152 bytes to spare.
The cost is O(live endpoints) per removal on a teardown path with a 512-slot table, which is not
where this kernel's time goes.

**The general shape: a `[T; MAX]` local sized to a table maximum is a stack allocation wearing the
clothes of a bound.** `MAX_ENDPOINTS` is 512 because that is a sensible ceiling on live endpoints, and
nothing about that number was ever a claim about how much stack a function may use. The two got tied
together by the convenient shape, and the connection was invisible until something measured it.

## BUGS

- Depth reached before `paint_boot_stack` runs (a handful of early-boot frames) and never reached
  again is invisible, bounded below by the printed paint floor.
- **The static frame sizes above are per function, not per call chain.** `-Z emit-stack-sizes` says
  what one frame costs; it does not say which frames stack on top of each other, so it cannot give a
  worst-case depth on its own. Pairing it with the watermark is what makes either number actionable,
  and a tool that walks the call graph closes the gap. **`script/stack-depth-check` is that tool**
  (2026-08-16), written after this entry and `script/stack-frame-check`'s twin of it had both stood
  unbuilt through two rounds of guard-page faults. It reads direct calls out of the disassembly,
  hangs these frame sizes on the graph, and takes the longest path from the entry points a kernel
  thread stack starts at. Its first answer: 13792 bytes worst case on aarch64, 13344 on riscv64.
  **Do not compare its `thread_entry` chain against a watermark**, which is the mistake two drafts
  of that comparison made: this note's number is whatever was on the stack, nested trap frames
  included, and that chain is the thread's own work with no trap on it. Measured over 31 aarch64
  runs the watermark read 9536, 9640 and 10600 against a 9456 chain and a 13792 composed bound, and
  the excursions are the size of a trap frame plus a handler. The comparison that means something is
  against the composed number.
  Its answer is still a lower bound rather than an upper one, because indirect calls are invisible
  to it and **assembly has no `.stack_sizes` entries at all**, so `switch_to`'s 96-byte frame,
  `user_entry_trampoline`'s 272-byte reservation and `spawn_into`'s closure slot are uncounted.
- **~~Nothing gates frame size.~~** `script/stack-frame-check` does, since 2026-08-13, at the
  4096-byte guard page. The entry is kept because the sentence that follows it is still the
  argument for the gate: the 6816-byte frame was legal, compiled without a warning, and was found
  only because a stack overflowed and somebody went looking.
- **The two thresholds are the same number for different reasons, and only one of them needs
  margin.** This note's thread row gates a measurement at 14336 and needs margin because the next
  run may go deeper. `script/stack-depth-check` gates a worst-case bound, which already covers every
  path it can see, so it fails at the stack size and only *warns* at 14336. Raise them together or
  they stop describing the same stack.
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
  wasted so that slot index can stay CPU id, and any seat the machine does not fill (the constant
  is a ceiling since the 2026-08-14 bump to eight) idles the same way. 68 KiB of address space
  each, no frames.
