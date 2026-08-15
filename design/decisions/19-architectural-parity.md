# 19. Architectural parity is a tenet; the targets are aarch64, riscv64, and x86_64

**Status: DECIDED.**

**Decided 2026-07-27** (calef), promoting what practice had already become. The RISC-V work
began as a portability proof and ended at full parity (notes/riscv-parity-scope.md: SMP, the
suite, the shell, the benchmarks, the disk, the DMA confinement, and now the §18 transport and
the coming §16b IOMMU work, all on both ISAs). The tenet makes that the standing rule rather
than a happy outcome:

**Parity is a gate, not an aspiration.** A kernel capability ships on every supported
architecture, proven by the same test suite, or the gap is recorded in a scope note with what
is missing, what it proves, and the plan, the way riscv-parity-scope.md did it. An
architecture is a new `arch/` directory (rule #1), never a fork of the feature matrix. Where a
capability is genuinely asymmetric (a board has no device to prove it on), the record says so
loudly; the false parity-C blocker showed what a quiet gap costs.

**The target set is explicit: aarch64, riscv64, x86_64.** Status, honestly: aarch64 is where
the kernel grew up; riscv64 is at parity; **x86_64 is a declared target that does not exist
yet** (milestone 20 always named it as the reach past RISC-V; this section makes it a
commitment rather than a mention). What x86_64 will stress, known now: a different boot world
(UEFI/ACPI, not device tree; no OpenSBI/PSCI analog), the APIC instead of GIC/PLIC, a third
page-table format behind the `paging` seam (which two IOMMUs are about to prove out anyway),
and TSO memory ordering, where rule #4's weak-first discipline finally pays out in the other
direction: code proven on weak machines is correct on TSO, and nothing about x86 development
could have said the reverse. The PCIe transport (§18) is already x86's native bus, and the
ECAM bridge on both `virt` boards is the same `pci-host-ecam-generic` shape x86 machines
present through ACPI.
