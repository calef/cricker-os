# Harts and PEs: the unit that runs code

Both terms answer the same question: what, precisely, is *one thing that executes an
instruction stream*? "Core" and "CPU" are too ambiguous to build specs on, because one
physical core can present two or more independent instruction streams (Intel's
hyper-threading is the famous case: two register files and program counters sharing one
core's execution units). Both architectures therefore coined a precise word.

**Hart** is RISC-V's: a **har**dware **t**hread, one independent instruction stream with
its own register file and PC. A core might contain one hart or several; the ISA and the
firmware only ever talk about harts. This is why OpenSBI's CPU-management extension is
called **HSM** (Hart State Management), why our riscv kernel starts secondary CPUs with
`sbi_hart_start`, and why the boot notes talk about surviving the **hart lottery**
(OpenSBI races every hart at reset; the winner boots, the losers park in HSM STOPPED
until asked for).

**PE** (Processing Element) is ARM's word for exactly the same idea, coined for the same
reason. The ARM ARM defines architecture behavior per-PE; PSCI's `CPU_ON` starts a PE.

On every machine this project targets they are one-to-one with cores: QEMU's `virt`
boards with `-smp 4` give four single-hart/single-PE cores, and the VisionFive 2's JH7110
is four U74 cores of one hart each. So in this repo "core," "hart," and "PE" name the
same schedulable thing, and the notes use whichever word the surrounding spec uses:
hart on the RISC-V side, core or PE on the ARM side.

One place the distinction earned its keep here: the 2026-07-28 benchmark repair. Under
`-icount`, QEMU's virtual-instruction clock is shared by *all* harts, so a benchmark
running beside three idle harts counted their `wfi` wakeups as its own time. "Pinned to
one hart" (`-smp 1`) is the fix, and saying "hart" rather than "core" is the reminder of
what the clock actually counts: instruction streams, not silicon packages.
