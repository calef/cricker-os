# 59. The CPU-model matrix: stop testing against one generous emulator

**Status: BUILT (2026-08-01).** `script/cpu-matrix` runs the riscv64 suite against `rv64`,
`sifive-u54`, `rva22s64`, `rva23s64` and `thead-c906`; **211 tests pass on every one**, so the cheap
experiment below came out the reassuring way and "we are already portable to the board's ISA" is now
measured rather than predicted. `script/test` grew `--arch` and `--cpu`, both defaulting to today's
behaviour, and CI grew a `cpu-matrix` job of its own. The full result, the preflight that keeps the
matrix from being theatre, and an honest BUGS list are in [notes/cpu-models.md](../../notes/cpu-models.md).
The one thing it did **not** de-risk is the ASID width: every model reports 16 implemented bits, so
the test written for the board still has no machine that can fail it.

Raised by Chris on 2026-08-01, asking whether we should modify QEMU to match
the chip, detect features, or something else.

**The answer to the first is no.** A forked emulator is a machine that exists nowhere: it proves
nothing about the real chip and nothing about the standard emulator, which is the worst of both. We
also pin QEMU for benchmark determinism (`.qemu-version`, and CI builds it from source), so a fork
multiplies that maintenance. QEMU already lets us **narrow** rather than patch, and narrowing is the
whole milestone.

## What we actually run against today

`qemu-system-riscv64 -machine virt -cpu rv64 -bios default`. **`rv64` is QEMU's maximalist model**: it
enables essentially every ratified extension QEMU implements. The VisionFive 2's JH7110 is a SiFive
U74, which is **RV64GC**. So the emulator will accept things the board will not, and every RISC-V
result we have was taken on the permissive one.

## The reassuring part, stated before the worrying part

We build for **`riscv64imac`**: no `F`, no `D`. RV64GC is IMAFDC. **We are already a strict subset of
the board's ISA**, so the compiler cannot emit an instruction the U74 lacks. That is a real result
and it narrows this milestone considerably.

What it does **not** cover is the part that is hand-written: `asm!` in `arch/riscv64/`, CSR reads and
writes that QEMU may implement more permissively than SiFive, and implementation-defined widths. That
is the exposure, and it is exactly the class a narrower `-cpu` catches.

## The work

Run the existing suite against more than one CPU model. QEMU ships **`sifive-u54`** (the U74's
family) and the profile models **`rva22s64`** and **`rva23s64`**; `thead-c906` is a useful hostile
case because it is a real chip with real divergences.

**This reframes what parity means.** Today parity is two ISAs (DECISIONS §19). With hardware arriving
it should be *the same suite across CPU profiles*, because "aarch64 and riscv64 both pass" stops being
the strongest available claim once we know riscv64 was only ever tested on the friendliest model.

## Why this comes BEFORE discovery (milestone 60)

Because it needs no discovery to run, and **what it breaks tells us what is worth discovering.**
Building an `Isa` record first means guessing which facts matter; running the matrix first means the
machine names them. That is the same posture as the ASID probe and the device-tree-pointer correction:
measure, then write down what the measurement said.

The cheap experiment is one command and it may well pass, in which case the result is "we are already
portable to the board's ISA", recorded with the evidence.

## BUGS

- **`sifive-u54` in QEMU is still QEMU.** It will not reproduce the JH7110's cache behaviour, its real
  memory map, or its errata. This catches the ISA-and-CSR class and is not a substitute for the board.
- **A green matrix is not a portable kernel.** It is the absence of one specific class of failure.

**Effort: small**, and it is the highest ratio of de-risking to work of anything before the board
lands (~2026-08-21).
