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

The crate is `isa`, settled by calef on 2026-08-03. It is an abbreviation, and it is in the group
`DECISIONS.md` §39 protects rather than the group it rejects: a standard term of art a reader
already knows from outside this project, like `elf`, `dtb` and `pci`. Renaming it would cost a
reader the recognition the tenet exists to buy.

## Four lookup tables, and why none of them is a `match`

`TABLE` (extensions), `SBI_TABLE`, `IMPLEMENTATIONS` (SBI firmware) and `aarch64::IMPLEMENTERS`
(chip vendors) are all arrays with a `const` assertion over them, and all four could have been a
`match`. The reason they are not is a bug class a `match` cannot be checked for: **a duplicated key
compiles, silently, and makes the second arm unreachable forever.** In a vendor table that is a chip
whose name can never be printed; in the extension table it would be a fact the kernel can never
report. As arrays the duplicate is a compile error, and the lookup being a loop over data means a
test can walk the rows rather than copy them into a test file, which is the difference between
checking a table and restating it.

The SBI extension ids go one better and are **derived** from their tags (`eid("TIME")`), so the only
thing anybody can get wrong is a four-character string rather than a hex constant. That one is worth
the extra step because a mistyped id probes an extension that does not exist, gets "no", and refuses
to boot on correct firmware. The vendor codes are not derivable: most are the ASCII of the vendor's
initial and enough are not (`0x50`, `0x56`, `0xc0`) that the rule would have more exceptions than
cases.

## One line nothing covers, on purpose

`bit(n)`, the helper that turns an index into an `Extensions` bit. Every caller is a `const`
initializer, so the compiler folds it and the body never runs; a coverage report will always show it
unreached. The only assertion available is that `bit(3)` is `1 << 3`, which restates the body, so it
has a comment saying why it is uncovered instead of a test that moves a number.

`eid` is the instructive contrast. It is const-only too, and it *is* called at runtime by a test,
because there the thing that can be wrong is the tag rather than the arithmetic, and const
evaluation and runtime evaluation are two implementations of one function that should agree.

## The sequel: PSCI and the CPU list (milestone 100)

Milestone 60 read what the *part* is. It left the one subsystem that assumes a *board*: SMP
bring-up. Three facts were compiled in, and all three are stated by the same device tree everything
else was already reading.

| Fact | Where it was hardcoded | Where the machine states it |
|---|---|---|
| The PSCI conduit is `hvc` | `psci_cpu_on`'s `hvc #0` | `/psci`'s `method` |
| The `CPU_ON` function id is `0xc4000003` | a `const` in the same function | `/psci`'s `cpu_on`, or the 0.2 standard when `compatible` claims it |
| The cores are `0..MAX_CPUS` | `bring_up_secondaries`'s loop | `/cpus` |
| RISC-V's counter runs at 10 MHz | `TIMEBASE_HZ`, a `const` | `/cpus/timebase-frequency` |

The fourth row is the one that names the pattern rather than an instance. `arch/riscv64/timer.rs`
carried the comment *"hardcoded until the DTB parse lands"*, and the DTB parse landed with milestone
60; the comment outlived it by two months. aarch64's twin has always read `CNTFRQ_EL0` and asserted
it nonzero, so the two ISAs disagreed about whether the machine gets to say how fast its own clock
runs. That is a rule-5 parity gap, and it was invisible because both answers were 10 MHz on the only
machines anyone ran.

### The two failure shapes were not the same, and the plan had to say so

**The core list was a guaranteed silent no-op.** A machine with more than four cores had cores 4 and
up never started, and there was no error available, because nothing asked about them. That is the
worst shape a failure can take: success reported, work not done, and the symptom arriving later as a
scheduler that never balances.

**The conduit was board-specific and untested.** `smp.rs` did have a degradation path, written for
one case (a core that is absent, `PSCI {ret}; not present?`), and a machine whose firmware answers on
`smc` is under no obligation to produce a PSCI error code from an `hvc` it never agreed to serve. It
would more likely be an undefined-instruction trap. Calling both "silent" was close enough for a
warning and too loose for a plan.

### What is read, and what is still assumed

Read: `/psci`'s `method` and `cpu_on`, and every `cpu@` node's `reg`, `status` and `enable-method`.
The decoding is in `crates/isa` (`cpu_list` for the roster, `aarch64::Psci` for the conduit), so it
is host-testable; the kernel halves are the reads and the refusals, exactly the split milestone 60
set up.

Still assumed, **and now checked rather than assumed silently**: that a core's hardware id equals its
logical id. Using a hardware id that differs would need three other things to move with it, and none
of them is in this milestone. `cpu::PERCPU`, the secondary stacks and the RISC-V trap stashes are
arrays indexed by logical id; the GICv2 targets an SPI by CPU-interface number and `send_sgi` by that
same number; the PLIC's S-mode context is `2 * hart + 1` and the SBI IPI mask is a bitmap of hart
ids. So `bring_up_secondaries` reads the real ids, compares each to its index, and **refuses by name**
the ones that differ:

```
  smp: cpu 2 has hardware id 0x100, which is not its index.
       This kernel indexes per-CPU state, interrupt targets and IPI masks by
       logical id, so it cannot use that core yet. Not started.
```

A clustered aarch64 board (`MPIDR_EL1.Aff1` in use) is exactly that machine. This turns a guaranteed
silent no-op into a named refusal and a smaller machine, which is the whole point; making the kernel
actually *use* such a core is a milestone of its own, and it is a bigger one than this was.

### What the boot says now

```
  smp: psci over hvc, CPU_ON 0xc4000003 (the node's own id, from the device tree)
  smp: 4 core(s) in the device tree
  smp: 4 core(s) online
```

On RISC-V the first line reads `sbi hsm hart_start (the only mechanism RISC-V defines)`, and the
asymmetry is real rather than a gap: RISC-V has one bring-up mechanism, no conduit to choose and no
function id to look up, because a kernel entered in S-mode has firmware under it by construction.

### The fixture that made the `smc` half testable

QEMU `virt` states `method = "hvc"`, so for a while it looked as though the other branch could only
be exercised by hand-writing a tree. It cannot, quite: `-machine virt,virtualization=on` puts
something at EL2 and QEMU's own PSCI moves to EL3, so **the same board, one option different, states
`smc`**. That dump is `crates/dtb/tests/fixtures/qemu-aarch64-virt-smc.dtb`, and the host test that
compares the two is the whole finding in one assertion: the conduit is not a property of aarch64, of
QEMU, or of the `virt` board.

It does not make the `smc` **call** tested. That configuration enters the kernel at EL2 and this
kernel expects EL1, so nothing here boots it. Parsed, not called; the `arch::psci_cpu_on` BUGS block
says so where a reader meets the function.

## BUGS

- **Discovery does not make the kernel portable, it makes it honest.** Knowing an extension is
  missing and doing something useful about it are different milestones. Today the kernel does
  exactly one thing with a missing requirement: it says so and stops.
- **A core whose hardware id is not its logical id is refused, not used** (milestone 100). See
  above; the reason is four other subsystems that index hardware by logical id.
- **The `smc` conduit is parsed and never called.** No machine on this laptop boots the
  configuration that would exercise it.
- **`cpu_list` reads `/cpus/timebase-frequency` and falls back to the first hart's own.** The RISC-V
  binding permits it per hart, and a machine whose harts genuinely differ would be misread. The
  kernel treats the counter as machine-wide regardless, so this is a limitation of the model rather
  than of the parse.
- **`user_rt::cntfrq` still returns 10 MHz on RISC-V, and it is now the last copy.** The kernel reads
  the real rate; **userspace cannot**, because there is no register to read and no channel to hand it
  down. Closing it is an ABI addition (an aux-vector entry at process start, the way Linux passes
  `AT_HWCAP`), which is a design fork rather than a fix, and `notes/riscv-parity-scope.md` already
  names it as workstream E's prerequisite for honest cross-arch benchmark numbers. Worth knowing that
  the kernel and its userspace now disagree about where that number comes from.
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
