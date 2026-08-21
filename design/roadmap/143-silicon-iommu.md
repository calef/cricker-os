# 143. Silicon IOMMU: carrying 16b's driver to a board that ships the ratified spec

**Status: NOT-STARTED.**

**Gate: HARDWARE.** The board does not exist. No RISC-V SoC on the market today ships the ratified
RISC-V IOMMU specification (v1.0.1, ratified 2024). The VisionFive 2 (JH7110, 2022-era silicon)
predates the spec and has no IOMMU at all. This milestone is the carry-over step that 16b's block
always recorded as "a hardware fact nobody can schedule."

Split out of milestone 16 on 2026-08-20, because 16a (first silicon on the VisionFive 2) is PARTIAL
with lane-sized engineering remaining, while this is an indefinite hardware wait. The two were
bundled under one number since the original milestone was written, and the split makes the hardware
fact honest: 16's remaining work is engineering that can be done now; this is a purchase that has
not been made and cannot be scheduled.

## What this is

16b built IOMMU-backed DMA isolation in QEMU emulation on both ISAs: SMMUv3 on aarch64 and the
ratified RISC-V IOMMU (v1.0.1) on riscv. The portable DMA-domain seam (`crate::iommu` over
`paging::domain`), both arch drivers, boot bring-up, the `iommu_platform=on` enablement with the
confinement test, and the disk and attacker suites all pass behind the IOMMU on QEMU.

This milestone is the carry-over: when a board ships the ratified RISC-V IOMMU spec, 16b's riscv
driver runs on it. The emulate-then-carry pattern is the one the kernel was built on: the driver
was written against the ratified spec in emulation, so the carry is "boot it and fix what the
silicon got wrong," not "write it for the first time."

## What would trigger it

A RISC-V board or SoC that:
1. Ships the ratified RISC-V IOMMU specification (v1.0.1 or later).
2. Exposes it as a PCI function (the QEMU emulation enumerates it this way; the spec allows it).
3. Speaks the rest of the firmware contract the kernel already handles (OpenSBI, SBI HSM, PLIC,
   Sv39 or Sv57).

As of 2026-08-20, no such board is available at any price a demonstrator can justify. The closest
candidates are future StarFive SoCs (the JH8800 series is announced but not shipping) and the
revflex/lowrisc class boards, none of which have a confirmed IOMMU implementation.

## What it would cost

Near the floor, because the driver is built. The work is:
1. Boot the kernel on the board (the firmware contract is already spoken).
2. Enumerate the IOMMU as a PCI function (16b's bring-up path already does this).
3. Run the disk and attacker suites behind the IOMMU on silicon.
4. Fix what the silicon got wrong, and record whether it was our bug or the board's (the same
   discipline 16b applied to QEMU's emulation).

One lane, assuming the board boots. The unknown is whether the silicon matches the spec the driver
was written against, which is the thing only the board can answer.

## What this does NOT include

- **SMMUv3 on aarch64 silicon.** That is a separate hardware wait (a Pi 5 or similar ARM board
  with SMMUv3). 16b's aarch64 IOMMU driver carries over the same way, but the aarch64 board story
  is weaker (notes/target-hardware.md flags it) and not bundled here.
- **The shadow descriptor ring.** It stays as defence in depth everywhere, on silicon and in
  emulation, regardless of whether the IOMMU is present.

## Prior art

This is the emulate-then-carry pattern: seL4 developed against QEMU and carried to hardware; the
same pattern this kernel's riscv port used (Sv39 in QEMU, then the VisionFive 2). 16b's block has
the full argument.
