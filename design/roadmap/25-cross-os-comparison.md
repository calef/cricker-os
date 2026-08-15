# 25. Cross-OS performance comparison (extends 21)

**Status: PARTIAL.**

**Gate: HARDWARE, MILESTONE 74.** Everything but `sel4bench` is done, and `sel4bench` times single
operations through a PMU cycle counter that neither QEMU-TCG nor HVF provides. The hardware is no
longer 16a's board: milestone 127 bought the machine under the only published aarch64 seL4 numbers
(a sealed TX1 kit, 2026-08-15, arriving ~08-19 to -26), so this gate now counts down in shipping
days rather than waiting on a decision. Milestone 74 is the driver that reads that counter, and 74's block states the
dependency this one does not.

**In brief.** EL0-measured primitive benchmarks (syscall, context switch, IPC, map, spawn) the lmbench way, so the numbers include the trap the kernel-side benchmarks skip; then line them up against lmbench (Linux, macOS guests) and `sel4bench` (seL4), at a matched virtualization tier, with release builds. Fold in the icount codegen-sensitivity fix.

**Why it matters.** **turns perf claims into cross-OS numbers**: where does a Rust capability microkernel stand next to Linux, macOS's XNU, and seL4 on the primitives that define an OS. **Largely done**: four EL0 primitives (null syscall, context switch, IPC, page map) on both instruments, a release build path, and the three-way comparison (cricker-os vs Linux-under-HVF vs native macOS) with cricker-os winning null/IPC ~5x. `spawn` landed too (its real prerequisite was never retype, which had already shipped, but **object revocation**, reclaiming a child's TCB/aspace/endpoint so a spawn loop can repeat; that shipped as its own milestone, notes/object-revocation.md, and the EL0 `lat_proc` bench, `spawn_el0`, is in the suite and the committed baseline). **Remaining**: only `sel4bench` (built and booting for qemu-arm-virt, but it times single ops via the PMU cycle counter, which neither QEMU-TCG nor Apple HVF provides, so it is **deferred to real hardware**, the milestone-16 machine, which has a real PMU; this validates our CNTVCT + long-loop design). notes/benchmarks.md
