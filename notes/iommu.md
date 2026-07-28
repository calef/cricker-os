# Confining DMA with an IOMMU

notes/dma.md closed the DMA hole in software: the kernel validates every descriptor and the device
reads a shadow copy the driver cannot touch. This note is the hardware version, milestone 16b: an
IOMMU that confines the device generically, with no block-device or even transport knowledge, on
both architectures behind one seam. The shadow ring stays, demoted to defence in depth.

## What an IOMMU is, and why it is the clean answer

The MMU confines a process: a thread at EL0/U-mode cannot touch a byte it was not mapped. But a
device is a second bus master. It reads descriptors and does DMA against raw *physical* addresses,
and page-table permissions do not apply to it (notes/dma.md spells this out). An IOMMU is the MMU
for devices: it sits between the device and memory and translates every address the device emits,
through page tables the kernel programs, confining the device to exactly the frames in those tables.
An address the device was never granted has no mapping, so the IOMMU faults instead of letting the
DMA through. It is generic: the kernel needs to know nothing about what the device is, only which
frames it may reach.

## The seam: one domain builder, two page-table formats

The payoff of the format-generic `paging` crate (DECISIONS §17) shows up a second time here. An
IOMMU walks the *CPU's own* page-table format: the aarch64 SMMUv3 walks VMSAv8-64, the ratified
RISC-V IOMMU (v1.0.1) walks Sv39. Those are the same two formats `Mapper` already builds for process
address spaces. So a device's DMA domain is not a new kind of table. It is a `Mapper` filled with an
**identity map** (IOVA == PA) over exactly the frames the device is allowed to reach:
`paging::domain::build_identity_domain`, host-tested on both formats.

Identity, because the userspace virtio driver already puts *physical* addresses into its descriptors
(its DMA region's frames, and the kernel's shadow page). With an IOMMU in front, the device emits
those same numbers as IOVAs, and the domain translates each to the identical PA. The driver's ABI
does not change, because IOVA == PA means the addresses it computes still name the right memory. The
domain is an allow-list of frames, expressed as a page table.

`kernel/src/iommu.rs` is the portable seam over that builder. `confine(rid, regions)` allocates a
root frame, calls `build_identity_domain` through `DmaFormat` (an arch alias: `Aarch64` here, `Sv39`
there), and hands the root to the arch driver's `attach`. One call site, two formats, which is the
whole point.

One asymmetry worth stating: single-stage RISC-V translation (`iosatp`, no process context) faults
on a leaf PTE whose U bit is clear, because a device does not "request supervisor privilege." So the
domain is built with `Flags::user_data`, which sets U on Sv39 and read/write on both formats. It is a
device's data window, so user-accessible read/write with no execute is exactly right.

## The two arch drivers, structural twins

Under `arch/` per rule #1: `kernel/src/arch/{aarch64,riscv64}/iommu.rs`. They rhyme deliberately.
Each is driven almost entirely through memory, not registers:

| role | SMMUv3 (aarch64) | RISC-V IOMMU (riscv) |
| --- | --- | --- |
| per-device table, keyed by requester id | stream table (STE) | device directory (device context) |
| the IOMMU's copy of TTBR/`satp` | context descriptor (CD) | device context `fsc`/`iosatp` |
| how config changes take effect | command queue + CMD_SYNC | command queue + IOFENCE.C |
| where faults are reported | event queue | fault queue |

Each driver: `init` installs an all-invalid table and enables translation, so an unattached device is
denied by default (the SMMU needs an explicit `GBPA.ABORT` for the pre-enable window; the RISC-V
IOMMU's reset `ddtp` mode is already `Off`, which blocks, so it fails closed by architecture). `attach`
writes the device's table entry pointing at a domain root, then invalidates the cached STE/CD/context
and the translation cache so a re-attach cannot use a stale table. `take_fault` pops one fault record.

## The requester id is the key

A PCIe function stamps a 16-bit **requester id** (`bus:8 | dev:5 | fn:3`) on every memory transaction
it issues (`pci::Bdf::requester_id`). Both `virt` boards publish an identity `iommu-map` in the
device tree, so that id is exactly what the IOMMU looks a device up by (SMMU StreamID, RISC-V
`device_id`). It is threaded from `pci::find_block_device` through `virtio::register`, which gained an
`Option<u32>` argument: `Some(rid)` for a PCI device, `None` for virtio-mmio (no IOMMU fronts the
mmio bus on either board). `confine` runs inside `register`, before the device is entered in the
transport table and before it is ever rung, so the domain is installed the moment the device could
DMA. The regions a virtio device's domain covers are its DMA region (data buffers, used ring) and the
kernel's shadow page (the descriptor table and available ring the device reads).

## Discovery differs by arch; the rest is portable

The SMMUv3 is a device-tree platform node, found in `memory::smmu_region` and mapped by `mmu::init`;
`arch::iommu::init(base)` is called from the aarch64 boot when the node is present. The RISC-V IOMMU
is itself a PCI function (`riscv-iommu-pci`, Red Hat 1b36:0014), so `pci::init_iommu` enumerates it,
places its BAR from a now-shared cursor (two functions need BARs placed, so the cursor can no longer
reset per device), and hands the base to `arch::iommu::init`. The BAR-placement change is the one
piece of PCI plumbing this milestone touched.

## Making the machine tell the truth: the confinement test

`iommu_platform=on` is what puts a virtio-pci device behind the IOMMU: the device then emits IOVAs the
IOMMU translates, and offers VIRTIO_F_ACCESS_PLATFORM (feature bit 33), which the driver negotiates
only when offered (so one driver binary works on the bare mmio disk and the IOMMU-fronted PCIe disk).
A device *without* the flag silently bypasses translation. That is the same manufactured-fact hazard
the runners already fail loudly on for a missing disk: a plausible-looking success that is actually a
configuration gap.

So the confinement is not trusted, it is proven. `the_iommu_faults_a_dma_that_escapes_the_domain`
(kernel/src/virtio.rs, runs on both ISAs) confines the real PCIe disk, then points its available ring
at a frame the domain does not map and kicks it. The device's first act on the kick is to read the
available ring, at an out-of-domain IOVA, so the IOMMU faults and records it; the test drains the
fault queue and asserts a fault at that exact frame. If translation were absent (a missing
`iommu=smmuv3` / `riscv-iommu-pci`, or a dropped `iommu_platform=on`), the escaping read would succeed
and no fault would appear, so the assertion fails rather than passing on a fiction. The test goes
around the software shadow ring deliberately (the shadow ring would refuse an out-of-region
descriptor before the device was ever rung, so a normal path can never reach the hardware): the point
is to prove the hardware would stop the device even if that software were bypassed.

## QEMU vs ours

The RISC-V IOMMU emulation is newer than the SMMUv3's, so the honest record notes which is which:
during this build both behaved exactly as their specs describe, and no bug (QEMU's or ours) surfaced.
The full existing suite runs behind the IOMMU on both boards: the disk read, both DMA-escape attacker
tests, and the new hardware-fault test (aarch64 118 kernel tests, riscv 60).

## Honest limits

- **QEMU tier only.** Silicon carries the riscv driver over when a board ships the ratified spec, the
  emulate-then-carry pattern the kernel was built on. Parity is claimed at the QEMU tier.
- **Frame-granular.** The domain is an identity map over whole frames, so it cannot confine below a
  page. That is fine for a device's DMA region, which is page-aligned by construction.
- **No fault handler yet.** `take_fault` is drained by the confinement test; routing IOMMU faults to
  a handler in a production boot is future work.
- **Not free even when you have one.** Someone still programs the domain that confines the device,
  and that someone is the kernel. The IOMMU buys generality (no transport knowledge needed), not the
  absence of a trusted DMA policy. The shadow ring (notes/dma.md) stays as defence in depth: the
  transport still refuses a format it cannot police, so a regression in the IOMMU path is caught by
  the software layer and vice versa.
