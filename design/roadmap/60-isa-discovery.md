# 60. ISA discovery: read the machine instead of assuming it

**Status: BUILT, both ISAs.** The gap found while answering milestone 59's question was that
**nothing in the tree read the ISA**: no `riscv,isa`, no `riscv,isa-extensions`, no `mmu-type`, and
on aarch64 only the one `ID_AA64MMFR0_EL1.PARange` field `TCR_EL1.IPS` needs. One `Isa` record per
architecture now, in [`crates/isa`](../../crates/isa) with the kernel halves in
`kernel/src/arch/*/isa.rs`, populated once at boot and printed at boot. See
[notes/isa-discovery.md](../../notes/isa-discovery.md). The rest of this entry is the brief it was
built to; what it found is at the bottom.

## Why the device tree, and why there is no shortcut

**RISC-V deliberately has no `CPUID`.** `misa` exists but is coarse, is permitted to read as zero, and
says nothing about post-2015 extensions. The architected answer is the device tree
(`riscv,isa-extensions`, `mmu-type` for Sv39 versus Sv48) plus SBI for firmware-provided facilities.
We already parse DTB (`crates/dtb`), so this is parsing plus somewhere to put the answer.

## The shape, and the trap

**One `Isa` record, populated once at boot, printed at boot.** The trap is `if isa.has_x()` sprouting
across the kernel, which turns a fact into a hundred branches. The places that genuinely vary are few
and nameable: TLB flush strategy, ASID width, Sv39 versus Sv48, IOMMU presence. Keep it to those.

**Do not build a chip-abstraction framework on one board.** CLAUDE.md's rule against speculatively
trait-ifying applies with force here: the second real board should tell us what the abstraction is.
One record and four call sites is not a framework, and that is the point.

## Discovery has three tiers and we should be explicit about which is which

1. **The device tree** declares what firmware claims.
2. **A targeted probe** measures what the silicon does. `probe_asid_bits()` (built 2026-07-31) is the
   pattern: write ones, read back what stuck.
3. **Trap-and-detect** executes an instruction and catches the illegal-instruction fault. Last resort,
   needs the exception path, and we should not need it.

**Keep the probes even once the tree is parsed.** The tree is a claim and the probe is a measurement,
and when they disagree the machine wins. That is not a hypothetical here: this project has already
been wrong about a QEMU boot register it believed the documentation about.

## Truthfulness (§42's habit, applied to hardware)

If something required is absent, **say so and stop**, rather than running degraded and reporting
success. §42 makes a filesystem declare what it offers and be honest about it; a kernel that silently
assumes Sv39 on an Sv48 machine is the same violation one layer down.

## BUGS

- **Discovery does not make us portable**, it makes us honest. Knowing an extension is missing and
  doing something useful about it are different milestones.
- **The device tree can lie**, or firmware can describe a machine it is not. Tier 2 exists for that.

**Effort: not estimated.** Parsing is small; how many call sites genuinely need to vary is the unknown,
and milestone 59 is what answers it.

**What 59 answered, 2026-08-01: zero, on the five CPU models QEMU offers.** The suite passes
unchanged from `sifive-u54` (bare RV64GC) to `rva23s64` (vector, `zicond`, pointer masking), so
nothing in the kernel currently needs to branch on a discovered fact. That does **not** retire this
milestone, and the reason is the sharpest thing 59 found: **QEMU reports `satp.ASID: 16 bits
implemented` on every model**, including `sifive-u54`. The one place we already know a real chip may
differ is the one place no emulator can tell us about, so discovery's value is not the branching, it
is being able to say what the machine is instead of assuming it. See
[notes/cpu-models.md](../../notes/cpu-models.md).

## What it found, 2026-08-03

**Four call sites vary, and two of this entry's four candidates dropped out.** Real: the ASID width
on both ISAs (`crates/asid` assumes 8 and RISC-V guarantees none); `TCR_EL1.IPS` from `PARange`,
which predates the milestone and is now read once into the record; and two refusals, riscv64's
Sv39-or-stop and aarch64's 4 KiB granule. Not real: **TLB flush strategy varies nowhere**, because
the unconditional `sfence.vma` is unconditional by design and removing it is its own milestone; and
**IOMMU presence is already discovered**, by the `smmuv3@` node and by PCI enumeration, so the record
would add a second way to ask one question. A fifth call site was never reached.

**aarch64 is covered, not scoped out**, and doing both is what produced the sharpest finding.
**ARM has a tier this entry's list is missing**: `MIDR_EL1` and the `ID_AA64*` space are the CPU
describing *itself*, architected and mandatory, so the aarch64 half is a decoder where the RISC-V
half is a parser over a property firmware wrote. RISC-V removed that tier on purpose. There is no
trait between them and should not be one until a second real board says what the abstraction is.

**Two corrections from the machine, both after the host tests were green.** The SBI spec version is
24 bits of minor and 7 of major, not 16 and 16, so QEMU's `0x0300_0000` (SBI 3.0) decoded as 0.0 and
the boot line called firmware that had answered perfectly well silent. And **QEMU `virt` declares
`mmu-type = "riscv,sv57"`**: the machine we have developed on for two milestones is two page-table
levels *wider* than the kernel, which is the opposite of the failure this entry was written to
anticipate.

**Three shapes that would have broken on the VisionFive 2**, all now covered by host tests: the
deprecated `riscv,isa` string (the modern properties are Linux 6.6 and later), `g` as an
abbreviation for `imafd_zicsr_zifencei` rather than an extension name, and the trap that requiring
`zicsr`/`zifencei` would **refuse to boot on that board**, because both were carved out of base `I`
in 2019 and an older string simply does not list them. `m`, `a` and `c` are what gate instead.
