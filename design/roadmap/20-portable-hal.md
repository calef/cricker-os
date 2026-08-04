# 20. A portable HAL, proven on a second architecture

**Status: BUILT.**

**In brief.** Make `arch/` a real HAL; bring up RISC-V then x86_64

**Why it matters.** the "portable verified core" claim; reach the demonstrator earns

**Reach the demonstrator earns (§14), with a thesis-relevant core.** A second ISA is reach work, and
§14 parks reach. What pulls part of it back in-scope is one demonstrator claim: **the verified
capability core is architecture-independent**, the same machine-checked confinement running S/U on
RISC-V, ring-3 on x86, and EL0 on ARM. seL4 (verified on both ARM and RISC-V) is the precedent.

**Deliverable, in two parts.**

1. **Make `arch/` a real HAL.** Today it is a `#[cfg(target_arch)]` re-export whose contract is
   "fails to compile if something is missing." Turn it into a genuine machine-dependent layer: split
   the aarch64 descriptor format out of the `paging` crate (a generic level-walk plus a per-arch entry
   codec, the way Linux folds page-table levels), put device discovery behind a "here is the hardware"
   interface (device tree today, ACPI/PCI later), and make the arch surface explicit. This is the
   reusable half and most of the value; a second ISA is what proves the split is honest. The
   seam-*naming* subset that needs no second architecture is broken out as **20a** and can start now;
   the abstraction *shapes* (the codec and discovery interfaces) wait for RISC-V, because deriving
   them from one ISA is the wrong-abstraction trap DECISIONS warns against.
2. **Bring up a second ISA, then a third: RISC-V first, x86_64 second.**

**Why RISC-V first.** It is structurally close to aarch64, so it reuses the most and needs the
smallest new `arch/` subtree: device tree and virtio-mmio port unchanged, the weak-memory discipline
keeps paying off (RVWMO, like ARM), and Sv39/Sv48 is the same MMU shape. What is new is small and
clean (SBI boot, one trap vector, PLIC/CLINT, `ecall`), with no GDT/TSS, ACPI, PCI, or real-mode SMP
trampoline. It de-risks the HAL split cheaply and stays in the verification ecosystem (a formal Sail
ISA spec, seL4's verified RISC-V port).

**Why x86_64 second.** The hard proof: the HAL must survive a genuinely different model (CISC, strong
TSO memory, GDT/TSS, ACPI + PCI, port I/O, the `syscall` + swapgs trampoline, INIT-SIPI-SIPI SMP). If
the abstraction survives x86, it is real rather than an accident of two similar RISC ISAs. It is also
the reach: x86_64 is what most machines are. The file-by-file map is worked out (see the chat where
this milestone was proposed).

**Scope and the honest cost.** In scope: the HAL, and enough of each ISA to boot, confine a ring-3/U
process, and run the test suite. Out of scope and still parked: hardware breadth (every driver on
every board). It buys no proof coverage, the proofs live in the machine-independent crates, which
already do not care about the ISA, and it enlarges the unverified TCB (one hand-written
boot/MMU/trap/syscall layer per arch, the least-verifiable code). That is why it sits late, after the
core is verified (18, 14) and a workload runs (19). Not a new architecture: real-hardware aarch64
(Raspberry Pi) is the cheapest portability proof of all, same ISA on real silicon, and it lives in
milestone 16, not here.

**Prior art.** notes/portability.md: Linux's `arch/` with folded page-table levels, NetBSD's MI/MD
split, NT's HAL from day one. seL4's dual-arch verified port is the "portable verified core"
precedent.
