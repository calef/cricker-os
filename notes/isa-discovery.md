# ISA discovery: reading the machine instead of assuming it

Until milestone 60 this kernel ran on what the target triple implied plus exactly one runtime
measurement. Nothing read `riscv,isa-extensions`, nothing read `mmu-type`, and on aarch64 the only
`ID_AA64*` field anyone touched was the `PARange` that `TCR_EL1.IPS` needs. That is fine on an
emulator, which is configured to be whatever you ask for. It stops being fine on a board.

The shape is one record per architecture, populated once at boot, printed at boot, in
[`crates/isa`](../crates/isa) with the kernel halves in `kernel/src/arch/*/isa.rs`.

## What the boot says now

RISC-V, on QEMU `virt` with `-smp 4`:

```
  paging      : fine-grained W^X Sv39 tables installed, satp switched (paging on: true)
  isa         : rv64i m a c f d h zicsr zifencei sstc zicbom
              : 4 hart(s), mmu sv57 declared and sv39 in use, satp.ASID 16 bits measured
  firmware    : OpenSBI 0x10007, SBI 3.0, TIME IPI RFENCE HSM
```

aarch64, on QEMU `virt` with `-cpu cortex-a72`:

```
cricker-os
  exception level : EL1
  cpu             : Arm part 0xd08 r0p3
                  : 44-bit PA, 48-bit VA available, 16-bit ASIDs, granules 4K 64K
```

Everything on those lines was previously either unknown to the kernel or assumed.

## The three tiers, and the one RISC-V does not have

The roadmap entry names three ways to learn what a machine is, in decreasing order of how much you
should like them: a **firmware claim** (a device-tree property), a **targeted measurement** (write a
value, read back what stuck), and **trap-and-detect** (execute it and catch the illegal-instruction
fault). We do the first two and none of the third.

Building both ISAs at once exposed a tier the list is missing, and it is the reason the two halves
of the crate look nothing alike.

**aarch64 has a tier 0: the CPU describes itself.** `MIDR_EL1` and the `ID_AA64*` space are
architected, mandatory, and read straight off the part in front of you. Not hearsay, and not a
measurement anyone had to design. So the aarch64 half is a **decoder**: three `mrs` reads and a
handful of shifts.

**RISC-V removed that tier deliberately.** `misa` exists, is coarse, is permitted to read as zero,
and cannot name a multi-letter extension, which is every extension ratified after 2015. So the
architected answer is a property firmware wrote into the device tree, and the RISC-V half is a
**parser**. It keeps its tier-2 probe (`satp.ASID`) even now that the tree answers, because a claim
and a measurement are different things and when they disagree the machine wins.

That asymmetry is worth stating plainly rather than smoothing over with a trait: the same question
has genuinely different best answers on the two architectures.

## How many call sites actually vary

The entry's own effort note said the unknown was how many call sites genuinely need to branch on a
discovered fact, and measuring that honestly was part of the deliverable. **The answer is four, and
two of the entry's four candidates turned out not to be among them.**

| Candidate | Verdict |
|---|---|
| ASID width | **Real, both ISAs.** `crates/asid` hands out 255 numbers on the stated assumption the hardware can tell them apart. riscv64 measures it (`probe_asid_bits`); aarch64 reads `ID_AA64MMFR0_EL1.ASIDBits`. |
| Sv39 versus Sv48 | **Real, as a refusal.** riscv64 stops on an `mmu-type` narrower than Sv39; aarch64's twin is the 4 KiB granule, which `TGran4` may refuse and every page table here depends on. |
| `TCR_EL1.IPS` from `PARange` | **Real, and it predates this milestone.** The one place the kernel already read the machine, now read once into the record. |
| TLB flush strategy | **Varies nowhere.** The unconditional `sfence.vma` in `write_satp` is unconditional by design, and removing it is its own milestone gated on the ASID probe. `Svinval` is recorded and acted on by nothing. |
| IOMMU presence | **Already discovered, elsewhere.** The `smmuv3@` device-tree node and PCI enumeration answer this, and the record would add a second way to ask the same question. |

Plus the requirement checks, which are branches that go exactly one way: RISC-V refuses a machine
that is not RV64, that lacks `m`/`a`/`c`, whose MMU is narrower than Sv39, or whose firmware says it
does not implement one of the four SBI extensions the kernel calls unconditionally.

**A fifth call site was never reached.** Four is what the entry predicted the ceiling should be, and
the two that dropped out are the more interesting result: a fact you already discover another way,
and a fact nothing branches on yet, both look like they need a record and do not.

## What the machine corrected, twice

Both after the host tests were green, which is the whole argument for booting the thing.

**The SBI spec version is 24 bits of minor and 7 of major.** Not the obvious 16 and 16. QEMU's
firmware reports `0x0300_0000`, which is SBI 3.0; decoded as 16/16 that is version 0.0, and since no
conforming firmware reports 0.0 the kernel used it as the signal for "the base extension did not
answer". So the boot line reported firmware that had answered perfectly well as silent, and the test
that asserts OpenSBI is present failed with a message about firmware from 2020. Found by printing
the raw word.

**QEMU `virt` declares `mmu-type = "riscv,sv57"`.** The machine this project has developed on for
two milestones is two whole page-table levels *wider* than the kernel, and nothing anywhere said so.
The worry going in was a board too narrow for us. This is the other direction, and it is invisible
without reading the property, which is the point.

## Three things that would have broken on the board

None of these were hypothetical worries; each is a shape the VisionFive 2 or its firmware has.

**The deprecated `riscv,isa` string.** `riscv,isa-extensions` and `riscv,isa-base` arrived in Linux
6.6 (2023). A vendor tree older than that carries only `riscv,isa = "rv64imafdc"`, so a parser that
reads the modern properties and gives up finds nothing at all. Both forms parse, and
`Isa::legacy_isa_string` records which one answered.

**`g` is an abbreviation, not an extension.** `rv64gc` means `imafdc` plus `zicsr` and `zifencei`. A
parser that looks `g` up in a table finds nothing and reports a machine with no multiplier, which
would then fail the `m` requirement and refuse to boot on a perfectly good core.

**Requiring `zicsr` would refuse the board.** Both `zicsr` and `zifencei` were carved out of the
base `I` extension in 2019, so a string written before that (or by firmware that never caught up)
simply does not list them while the hardware has them. The kernel uses both on every trap and in
`sync_icache`, and neither is in `REQUIRED`, because a check that fails on the machine you are
buying is worse than no check. `m`, `a` and `c` carry no such ambiguity and are what gate the boot.

## Why every CPU node, not `cpu@0`

A RISC-V machine may be heterogeneous, and the JH7110 on a VisionFive 2 is: application cores beside
a smaller monitor core, described as separate `cpu@` nodes with different `riscv,isa` strings. A
kernel that reads the first node and then schedules a thread onto any hart has read the wrong node
some of the time.

So `dtb` grew [`node_props`](../crates/dtb/src/lib.rs), which answers for every matching node rather
than the first, and the record carries two sets:

- **`common`**, the intersection over every hart that describes itself. This is what "an instruction
  the kernel may emit" actually means.
- **`any`**, the union. Without it the intersection silently hides heterogeneity, and you cannot
  tell "this machine has no FPU" from "one of its harts has no FPU". The boot line prints the
  difference when there is one.

`mmu-type` is taken the same way: the narrowest any hart declares, because Sv57 on one core is no
use to a thread the scheduler might place on the Sv39 one.

The test fixture for this (`crates/isa/tests/fixtures/mixed-cpus.dts`) is **hand-written and says so
in its own header**. It is modelled on the shape of a heterogeneous RISC-V SoC; the values are
invented. When the board arrives, dump its real tree and add it beside this one.

## Silence is not a failure

The truthfulness habit here cuts both ways, and getting only one direction right is how a discovery
layer becomes a liability.

A machine missing something the kernel needs is refused, loudly, with the missing thing named. A
machine that simply **does not describe itself** is not. A device tree with no `riscv,isa` at all
describes a machine that is nonetheless executing the code asking the question, and firmware too old
to implement the SBI base extension cannot be asked what it implements. Treating either as a failure
would refuse to boot on hardware that works.

So `Isa::missing_requirements` reports nothing about what the tree does not mention,
`Isa::described` says how many harts spoke, `Sbi::answered` says whether the firmware could be
asked, and the boot line says "the device tree does not describe this machine's ISA" or "SBI base
extension did not answer, so nothing here is verified" rather than reporting zeroes as facts.

## The trap, and why there is no trait

`if isa.has_x()` sprouting across the kernel turns a fact into a hundred branches, and a chip
abstraction built on one board is the wrong abstraction built early. Two guards, both structural:

The record is `Copy` with public fields and exactly **one verb**, `missing_requirements`, which is
the only thing a call site is meant to branch on. Everything else exists for the boot print.

And there is **no trait**, no `Cpu` abstraction, nothing shared between `isa::riscv64` and
`isa::aarch64` but the module tree. Two records that share no code is the honest shape when two
architectures answer the same question by unrelated mechanisms. The second real board is what should
tell us what the abstraction is, if there is one.

## Naming

The crate is `isa`, which is an abbreviation, and it is in the group `DECISIONS.md` §39 protects:
a standard term of art a reader already knows from outside this project, like `elf`, `dtb` and
`pci`. It is provisional until Chris settles it.

## BUGS

- **Discovery does not make the kernel portable, it makes it honest.** Knowing an extension is
  missing and doing something useful about it are different milestones. Today the kernel does
  exactly one thing with a missing requirement: it says so and stops.
- **A `status = "disabled"` CPU node is counted in the intersection.** Firmware sometimes describes
  a core the OS will never run on, and including it narrows `common` further than it needs to be.
  That is the safe direction and it is not free: a board describing a disabled core with no FPU
  would make us report no FPU. Left alone until a real board shows the case.
- **`VARange` reporting 52 on aarch64 does not mean the kernel could use 52.** `ARMv8.2`-LVA needs a
  64 KiB granule, and `ARMv8.7`-LPA2 is a separate feature bit this record does not read. Reported
  because it is what the machine says; acting on it is a milestone, not a branch.
- **`OpenSBI`'s implementation version prints as raw hex** (`0x10007`). The encoding is
  implementation-defined, so decoding it as `1.7` would be a guess that happens to be right for one
  vendor, which is the sort of guess `implementer_name` deliberately refuses elsewhere.
- **Nothing here has met a real board.** Every fixture is QEMU's or hand-written. That is the
  limitation the milestone exists to prepare for, not one it removes.

## See also

- [The CPU-model matrix](cpu-models.md), milestone 59, which found that **zero** call sites needed
  to branch across five QEMU CPU models and that QEMU reports 16 `satp.ASID` bits on every one of
  them, including `sifive-u54`. That is what made discovery worth building anyway: the one place a
  real chip may differ is the one place no emulator can tell us about.
- [ASIDs](asids.md) for what the ASID width is load-bearing for.
- [The device tree](device-tree.md) for the parser this reads through.
- [The RISC-V port](riscv-port.md) for the SBI calls whose extensions are now probed.
