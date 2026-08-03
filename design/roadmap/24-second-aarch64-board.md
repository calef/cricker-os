# 24. A second aarch64 *board*: Virtualization.framework (optional)

**Status: OPTIONAL.**

**In brief.** Boot under Apple's Virtualization.framework, not QEMU's `virt`: a virtio-console driver (VZ has no PL011), VZ's interrupt/memory layout and boot handoff, device discovery through the machine VZ presents

**Why it matters.** proves the `arch/` **board** boundary on a second machine of the *same* ISA (cheaper than 16's silicon, distinct from 20's second ISA), and lets cricker-os run under the same VMM as macOS/Linux guests. Optional; portability exercise, **not** a benchmarking prerequisite (guest-internal microbenchmarks are VMM-independent)
