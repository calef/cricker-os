# 74. Cycle counters: SBI PMU on RISC-V, `PMCCNTR_EL0` on aarch64

**Status: NOT-STARTED.** Raised 2026-08-03, from an audit of what milestone 16a actually needs. Its
deliverable includes "the benches on real cycles via the SBI PMU extension", and **nothing in the tree
implements it.** `PMU` appears only in device-tree test fixtures and in this file.

**Gate: MILESTONE 75, HARDWARE.** The aarch64 half must not land until 75 answers whether EL0 may
read the counter at all. The riscv64 half is buildable now and not verifiable until silicon,
because QEMU-TCG models an instruction counter that has nothing to do with cycles.

## What we read today, and why it is not cycles

Both ISAs read a **fixed-rate reference counter**, not a cycle counter:

| | aarch64 | riscv64 |
|---|---|---|
| today | `CNTVCT_EL0` + `CNTFRQ_EL0` | the `time` CSR (`rdtime`) |
| counts | a fixed tick, 62.5 MHz under QEMU | a fixed tick |
| resolution | ~41 ns on real silicon | comparable |
| the cycle counter we lack | `PMCCNTR_EL0` | SBI PMU, or the `cycle` CSR when `mcounteren` permits |

notes/pmu.md already sets this out for aarch64 and calls confusing the two a category error. The
generic timer is the OS's clock; the PMU counts CPU cycles at ~0.25 ns resolution and its rate moves
with frequency scaling. **Two ways to measure a fast operation: one shot at high resolution (PMU,
which is what sel4bench does) or a long loop at low resolution (the generic timer, which is what we
do).** Both are valid and they fail under different conditions.

## Why it matters more than "another counter"

The thesis claim is a cross-OS comparison, and **the literature it is compared against is denominated
in cycles**, not nanoseconds. notes/benchmarks.md does the conversion by hand and draws the honest
conclusion:

> seL4 publishes, for the same-core different-address-space path, 413 cycles for the IPC call and
> 426 for the IPC reply, one-way each ... So the corrected figure is roughly 1.1x to 1.7x an
> L4-lineage round trip, not 4 to 7 times.
>
> -- notes/benchmarks.md

**This file has now quoted that paragraph wrongly twice, which is why the quote above carries an
attribution line a gate can check.** The first version quoted the retracted arithmetic (*"At ~3.2
GHz, 705 ns is ~2,200 cycles round trip... we are 4 to 7 times heavier"*) as the current record,
after milestone 101 had re-measured it and found three errors. The replacement written on 2026-08-04
was a **paraphrase presented as a quotation**: it read "At ~3.2 GHz, 350 ns is roughly 960 to 1,420
cycles round trip", and those words appear in no note.

**It also put back the one assumption the correction had removed.** The note says in as many words
that the old paragraph's 3.2 GHz "is not this machine"; the 960-to-1,420 range is 350 ns against the
**M3's two clocks**, 2.75 GHz on an E-core and 4.05 GHz on a P-core, and nothing pins the vCPU
thread to either. At 3.2 GHz, 350 ns is ~1,120 cycles, a single number rather than a range. So the
sentence attached a correct range to the clock that had just been rejected, and read as sober while
doing it.

The mechanism rather than the apology: a prose block quote of another document is a citation no gate
resolves, and `script/citations` (milestone 97) checks one **only when it carries a `-- path`
attribution line**. Neither wrong version had one, so both passed every gate in the tree, twice, in
the file notes/citations.md already uses as its worked example. The binding form costs one line and
is rung two of CLAUDE.md's ladder instead of rung four.

The figure above is still arithmetic performed on a nanosecond measurement using an assumed clock
rate, which is the whole point of this milestone. Measuring cycles directly turns the project's
most-cited number from a derived figure into a read one, and it is the number a reader from the L4
world will look for first.

## Two things block on it

- **16a** cannot deliver "benches on real cycles" without it.
- **Milestone 25's `sel4bench`** is built and booting but was deferred to real hardware precisely
  because it times single operations through `PMCCNTR_EL0`, which neither QEMU-TCG nor Apple HVF
  provides. notes/pmu.md's last section explains why virtualization keeps the PMU out of reach: the
  generic timer is architected state a hypervisor must present, and the PMU is not.

## Parity makes this two ISAs, not one

§19 is a gate, and it bites here in an unobvious direction. The milestone reads as RISC-V work because
16a is the RISC-V board, but **`PMCCNTR_EL0` is equally unimplemented**, so a RISC-V-only cycle
counter would create a parity gap in the one subsystem whose entire purpose is cross-machine
comparison. Both sides are small and they are not symmetrical in shape:

- **aarch64**: enable the counter (`PMCR_EL0`), open it to EL0 (`PMUSERENR_EL0`), read `PMCCNTR_EL0`.
  Register writes, no firmware call. **Whether that EL0 opening happens at all is milestone 75's
  decision, not this one's**, and the aarch64 half should not land until it is answered: the counter
  is ~160x finer than the one §10 already excepted, so it does not inherit that exception.
- **riscv64**: the SBI PMU extension (EID `0x504D55`), which discovers counters, configures an event,
  and starts and stops it. The tree already makes SBI calls (`SBI_HSM_EID`, `SBI_IPI_EID`,
  `SBI_RFENCE_EID`, SBI TIME), so the plumbing exists and this is a fourth extension rather than new
  machinery.

## What can be done before the board, and what cannot

**Buildable now:** both drivers, the `Isa`-style capability probe, the benchmark harness change, and
the aarch64 path end to end (Apple Silicon has a real PMU; whether macOS lets a guest reach it is the
open question notes/pmu.md raises).

**Not verifiable until silicon:** the RISC-V numbers. QEMU-TCG models an instruction counter that has
nothing to do with cycles, so a green test under emulation proves the plumbing and says nothing about
the measurement. Say so in the note rather than publishing an emulated cycle count.

## Scope note

**Do not turn this into a profiling framework.** One counter, read before and after, on two ISAs. The
PMU can count dozens of events and the temptation to expose them generically should wait for a second
consumer, which is CLAUDE.md's rule against speculative trait-ification. `sel4bench` comparability is
the requirement; anything beyond it is scope.
