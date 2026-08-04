# 20. IOMMU-backed DMA isolation: one seam, two arch drivers (milestone 16b)

**Status: DECIDED.**

**Built 2026-07-28**, on both ISAs in QEMU emulation. Milestone 9's shadow ring (notes/dma.md)
confined DMA in software: the kernel validates every descriptor and the device reads a copy the
driver cannot touch. An IOMMU does it in hardware, generically, with no transport knowledge: it
sits between a device and memory and translates every address the device emits through page tables
the kernel programs. §16b makes that real on both boards, with the shadow ring demoted to defence in
depth.

**The seam is the payoff.** Each architecture's IOMMU translates with its own CPU's page-table
format: the SMMUv3 walks VMSAv8-64, the ratified RISC-V IOMMU (v1.0.1) walks Sv39. Those are the two
formats the `paging` crate already builds for process address spaces (§17), so a device's DMA domain
is not a new kind of table. `paging::domain::build_identity_domain` fills a `Mapper` with an
identity map (IOVA == PA) over exactly the frames a device may reach, and nothing else;
`crate::iommu::confine` calls it through `DmaFormat`, an arch alias, so one call site builds a
VMSAv8-64 domain on aarch64 and an Sv39 domain on riscv. This is the page-table format seam (§17)
paying off a second time: proven once, it now backs both process isolation and device isolation.

**Two arch drivers, structural twins**, under `arch/` per rule #1
(kernel/src/arch/{aarch64,riscv64}/iommu.rs). Each owns its register file and the in-memory
structures the hardware is driven by: a per-device table (the SMMU's stream table, the RISC-V
IOMMU's device directory) keyed by the PCIe requester id, a per-device context (the SMMU's STE plus
context descriptor, the RISC-V device context, each the IOMMU's copy of the CPU's TTBR/`satp`), a
command queue for invalidations, and a fault/event queue where a blocked transaction is recorded.
`init` installs an all-invalid table and enables translation, so every device is denied by default
until `confine` writes its entry; `attach` points a device at a domain and invalidates the caches;
`take_fault` drains the fault queue.

**The requester id is the key.** A PCIe function stamps `bus:8 | dev:5 | fn:3` on every transaction
(`Bdf::requester_id`), and both boards publish an identity `iommu-map` in the device tree, so that
id is exactly what the IOMMU looks a device up by. It is threaded from `pci::find_block_device`
through `virtio::register` (a new `Option<u32>` argument: `Some` for a PCI device, `None` for
virtio-mmio, which no IOMMU fronts on either board). `confine` runs at register time, before the
device is entered in the transport table and before it is ever rung, so the domain is installed the
moment the device could DMA. New lock rank `IOMMU` (54), a leaf below `VIRTIO`: the domain's
page-table frames are allocated before the lock is taken, so it is never held across an allocation.

**Discovery differs by arch; the rest is portable.** The SMMUv3 is a device-tree platform node
(`smmu_region`, mapped by `mmu::init`); the RISC-V IOMMU is itself a PCI function (`riscv-iommu-pci`,
1b36:0014), so `pci::init_iommu` enumerates it and places its BAR from a now-shared cursor before
handing the base to the driver. `init` is therefore called per-arch in boot; `active` / `confine` /
`take_fault` are the portable surface.

**Loud on bypass.** Every virtio-pci device needs `iommu_platform=on`, which puts it behind the
IOMMU and makes it offer VIRTIO_F_ACCESS_PLATFORM (bit 33); the driver negotiates that bit only when
offered, so the same binary drives the bare mmio disk and the IOMMU-fronted PCIe disk. A device
without the flag silently bypasses translation, the same manufactured-fact hazard the runners
already fail loudly on. The guard is the confinement test,
`the_iommu_faults_a_dma_that_escapes_the_domain`: it points a confined device's available ring at a
frame the domain does not map, kicks it, and asserts the IOMMU recorded a fault at that frame. If
translation were absent (a missing `iommu=smmuv3` / `riscv-iommu-pci`, or a dropped
`iommu_platform=on`), the escaping read would succeed and no fault would appear, so the test fails
rather than passing on a fiction. It runs on both ISAs.

**QEMU vs ours.** The RISC-V IOMMU emulation is newer than the SMMUv3's, so the record says which is
which: both behaved exactly as their specs describe, and no bug (QEMU's or ours) surfaced during the
build. The existing disk and both attacker suites pass behind the IOMMU on both ISAs (aarch64 118
kernel tests, riscv 60), and the shadow ring stays as defence in depth.

**Honest limits.** QEMU tier only; silicon carries the riscv driver over when a board ships the
ratified spec (the emulate-then-carry pattern the kernel was built on). The domain is an identity map
over frame-granular regions, so it cannot confine below a page. Fault reporting is drained by the
confinement test; routing faults to a handler in a production boot is future work. The IOMMU buys
generality (no transport knowledge in the kernel), not the absence of a trusted DMA policy: the
kernel still programs the domain. See notes/iommu.md.
