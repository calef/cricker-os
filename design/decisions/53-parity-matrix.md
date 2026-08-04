# 53. Parity is a matrix, not a pair

**Status: DECIDED.**

Milestone 59. `CRICKER_CPU`, `xtask test --cpu`, `script/cpu-matrix`, a CI job of its own. See
`notes/cpu-models.md`.

**§19 says a kernel capability ships on every supported architecture, proven by the same suite.
That is now too weak a claim to be the strongest one available**, because it was only ever proven
on the friendliest machine each architecture has.

## What was actually being tested

`qemu-system-riscv64 -cpu rv64` is QEMU's **maximalist** model: it advertises 42 named extensions,
`sv57` paging, hardware A/D update, the hypervisor extension. The board this project is aimed at,
a VisionFive 2, is a SiFive U74: **RV64GC, `sv39`, no `svadu`**. Every RISC-V result in this tree's
history was taken on a machine strictly more capable than the target.

## The decision

**The suite runs across CPU models, and a model is a first-class axis alongside the ISA.** Five
today: `rv64`, `sifive-u54` (the U74's family), `rva22s64` and `rva23s64` (profile models), and
`thead-c906`, which earns its place by being a **real shipped chip with real divergences** rather
than a profile nobody manufactures.

It is a **separate CI job**, not extra work inside `script/test`. The matrix is the same suite five
times, so folding it in would make every developer's gate four times longer for a check whose whole
value is that it rarely changes anything; on its own runner it costs no wall clock anyone waits for.
On every push rather than nightly, because what breaks it is a change to `kernel/src/arch/riscv64/`,
which arrives in a pull request, and a nightly reports the failure a day after the merge that caused
it.

## Narrow the emulator, never fork it

**A forked QEMU is a machine that exists nowhere.** It proves nothing about the real chip and
nothing about the standard emulator, which is the worst of both, and this project pins QEMU
(`.qemu-version`, built from source in CI) precisely so that instruction counts mean the same thing
on a laptop and a runner. `-cpu` already narrows; use it.

## The preflight, which is what makes the result mean anything

QEMU's `riscv,isa` device-tree string is **a claim**. If a future QEMU kept the claim and stopped
enforcing it, all five runs would pass while proving nothing, and nobody would notice because the
matrix would still be green.

So `script/cpu-matrix` **measures** it before trusting it: a Zba `sh1add` executed in M-mode under
`-bios none`, `mtvec` pointed at a `wfi` so a trap and a clean execution both park, `-d int`
reporting whether the trap happened. It is two-sided: `rv64` **must** execute it and `sifive-u54`
**must** refuse. A one-sided check would pass on an emulator that had stopped narrowing anything.

This is §42's habit applied to a tool rather than a filesystem: a thing that declares a capability
must be made to demonstrate it.

## The result, and the near-miss worth keeping

**211 passed on all five models**, so we are already portable to the board's ISA, measured rather
than predicted. That is a real result and it was not guaranteed: `riscv64imac` being a strict subset
of RV64GC covers what the *compiler* emits and says nothing about hand-written `asm!` or CSR
accesses.

The near-miss: `rv64` implements `svadu`, hardware update of the accessed and dirty bits.
`sifive-u54` and `thead-c906` implement nothing of the kind. **A kernel that left A/D clear would
pass on the generic model and page-fault forever on the board.** `crates/paging` sets them eagerly,
so it was already closed, but nothing in the suite would have caught it.

## BUGS

- **A narrower QEMU model is still QEMU.** `sifive-u54` will not reproduce the JH7110's cache
  behaviour, its real memory map, or its errata. This catches the ISA-and-CSR class and is not a
  substitute for the board.
- **The ASID width is not modelled per CPU.** Every model reports 16 bits, including `sifive-u54`,
  so `the_hardware_has_at_least_the_asid_bits_the_allocator_assumes`, the one test written *for* the
  board, has no machine that can fail it. The board will be the first. The unconditional
  `sfence.vma` in `write_satp` stays until it can be retired against real silicon.
- **Vendor extensions are advertised, not exercised.** `thead-c906` passing says nothing about the
  C906's non-standard page-table attribute bits, which `-machine virt` does not enable.
- **Five times the suite is five times the exposure to a flaky test.** Milestone 62.
