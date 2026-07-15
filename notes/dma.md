# Confining DMA without an IOMMU

The security audit's most severe finding: a userspace virtio driver could make the device DMA over
any physical memory. This note is how that hole was closed.

## Why it was a hole

The whole kernel is built to confine what a process can touch, and the MMU enforces it perfectly:
a process at EL0 cannot read, write, or execute a byte it was not mapped. But **the device is not
a process, and it is not behind the MMU.** A virtio block device is a second bus master: it reads
descriptors and does DMA against raw *physical* addresses, and page-table permissions — W^X, the
AP bits, the TTBR0/TTBR1 split, everything — simply do not apply to it.

The driver writes those physical addresses into the queue's descriptors. In milestone 9 the driver
owned the device registers and rang it directly, so a *hostile* driver could put the physical
address of the kernel image (or another process's frames) into a descriptor, mark it
device-writable, and issue a read. The device would DMA disk contents straight over kernel memory.
Nothing faulted, because the device honours no permissions. The driver was confined; the device it
drove was not.

Milestone 9's isolation was real for a driver *bug* (a bad address points at the driver's own
unmapped memory and faults it) and false for a driver *malice* (a deliberate kernel address just
succeeds). That is the gap.

## Why not an IOMMU

An IOMMU is the *clean* answer: it sits between the device and memory and translates every address
the device emits, confining it to a region the kernel programmed — **generically, with zero device
knowledge in the kernel.** That is why real systems use one, and it is what DECISIONS §10 meant by
"they had to bolt the isolation on afterwards with an IOMMU."

It is not reachable from here. QEMU `virt`'s SMMUv3 only covers the PCIe bus, not the platform
virtio-mmio devices we use. Getting behind it would mean switching to virtio-pci and writing a
PCIe enumerator, an SMMUv3 driver, and the virtio-pci transport — three substantial subsystems for
one gap. And real cheap hardware (a pre-4 Raspberry Pi) often has no usable IOMMU either, so the
software approach is the more broadly useful skill.

One honest nuance: an IOMMU is not *free* even when you have one. Someone still programs the
stage-2 tables that confine the device, and that someone is the kernel. The IOMMU buys
*generality* (no transport knowledge needed), not the absence of a trusted DMA policy.

## The fix: the kernel mediates the two DMA-critical powers

Without an IOMMU, something trusted must check every address the device will touch and refuse
anything outside the driver's region. The kernel takes back exactly two powers and leaves the rest
to the driver:

1. **The ring addresses.** The kernel programs `QUEUE_DESC/DRIVER/DEVICE` to fixed offsets in the
   driver's DMA region (`SETUP_QUEUE`). The driver never chooses them, so the rings themselves are
   always inside the region.
2. **The "go" signal.** The driver cannot write `QUEUE_NOTIFY`; it calls `NOTIFY`, and the kernel
   **validates every newly-published descriptor before ringing the device.** If any descriptor's
   `addr..addr+len` falls outside the region, the kernel refuses and the device is never told to
   go.

Everything else stays in the userspace driver: feature negotiation, the block request format,
sectors, reading results. The kernel owns the virtio **transport** (the descriptor and available
ring layout, enough to validate DMA) and knows nothing about **block devices**. The driver reaches
the device only through a `Virtio` capability; the device registers are no longer mapped into it.

This is a software stand-in for an IOMMU. It is less general (it understands the transport) but it
closes the hole: the device can only ever DMA within the driver's own region, so a hostile driver
can, at worst, corrupt itself.

## The validator

`kernel/src/virtio.rs::validate_avail` is the security-critical code. On `NOTIFY` it walks the
available ring from the last-validated index to the current one, and for each new head follows the
descriptor chain, checking that every `addr..addr+len` (with overflow rejected) lies within
`[dma_base, dma_base + dma_size)`. The chain walk is bounded by the queue size, so a malicious
`next`-pointer cycle cannot hang the kernel. It is written to take the ring addresses and a
read-word pair, so a test builds a fake region and exercises it directly.

## The proof

Three tests, and one of them is the attack:

- `the_validator_refuses_a_descriptor_that_escapes_the_dma_region` — a unit test that builds a fake
  region and checks a good chain passes, a descriptor pointing at kernel memory is refused, one
  running past the end is refused, and a cycle terminates.
- `the_kernel_refuses_a_dma_descriptor_that_escapes_the_drivers_region` — **end to end**: a
  malicious driver at EL0 holds a real `Virtio` capability, points a descriptor at the kernel
  image, and submits. The kernel refuses; the driver reports it.
- `a_userspace_driver_reads_a_file_from_a_virtio_disk` — the legit path still reads a file off the
  disk through the validated transport.

Verified the confinement can fail: with the region check stubbed to accept everything, the attack
goes through and the end-to-end test fails.

## The tradeoff, stated plainly

This moves the virtio *transport* into the kernel, which slightly walks back milestone 9's "the
driver operates the device." That is a real cost, taken deliberately, and it is defensible:
confining DMA *is* a transport concern, and the kernel still knows nothing about block devices. The
alternative — trusting the driver with all of physical memory — is what a monolithic kernel does
with an in-kernel driver, and the whole point of this project is not to.
