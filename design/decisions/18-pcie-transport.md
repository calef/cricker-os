# 18. The PCIe transport: one driver, two buses, the seam in the kernel

**Decided 2026-07-27, built the same night** (notes/pcie.md, notes/pcie-transport-scope.md). A PCI
root complex (ECAM enumeration, BAR placement, virtio-pci capability parsing, INTx through the
PLIC) and a virtio transport seam, so the same userspace block driver runs over virtio-mmio and
virtio-pci unchanged. The three decisions the scope note flagged, resolved as it recommended:

1. **INTx before MSI-X.** Legacy INTx is a wire into the PLIC, which is exactly the interrupt
   model the kernel already has (`bind_irq`, the `Irq` capability, WAIT/ACK). MSI-X is a later
   enhancement for a device that needs many vectors; nothing tonight does.

2. **One driver, two transports, and the seam lives in the kernel.** `virtio::Transport` answers
   the virtio-mmio register vocabulary against whichever bus the device sits on; the pci variant
   translates each name to the virtio-pci common-config layout, the read-to-ack ISR, and the
   resolved notify doorbell. The mmio vocabulary is canonical because it is what `abi::virtio`
   already exposes; nothing else about the choice is load-bearing. Everything above the seam (the
   shadow ring, the validator, the queue-layout contract, the userspace driver) is one copy and
   did not change. The security consequence is the point: the DMA confinement is written once and
   polices both buses, and PCI Bus-Master Enable (DMA permission at the bus level) is granted
   last, after the confined transport is fully described.

3. **virtio-mmio stays.** Neither board's working mmio path is migrated; PCIe runs **alongside**
   it, and the portability claim is proven rather than promised: the same crate and seam drive
   the disk on riscv (INTx via the PLIC, irqs 32..35) and on aarch64 (INTx via the GIC, INTIDs
   35..38, the highmem ECAM at 0x40_1000_0000). The per-arch cost was exactly the predicted
   constants-plus-map change, which is rule #1 doing its job on a whole subsystem.

**Build-vs-reuse, recorded late.** The `pci` crate was built rather than adopting `pci_types`
(the kernel-agnostic config-space/BAR/capability crate several Rust OS projects use), and the
call was made without the survey pass the reuse convention (notes/prior-art.md) requires; this
paragraph is the record arriving a day after the code, noted so the omission is visible rather
than smoothed over. The defense, worth about sixty percent: the closure-injection shape (every
function takes read/write closures, so the logic host-tests against a fake config space) and the
witness tests wanted an API of our own, and the whole crate is ~400 lines covering exactly what
we drive (type-0 headers, memory BARs, virtio vendor caps, the INTx swizzle). The counterweight:
`pci_types` covers most of that decode, and under the rule as written, a maintained no_std crate
should have been the default for peripheral plumbing outside the TCB. Verdict: keep ours (the
swap would trade witness-tested, zero-churn code for a dependency, backwards at this point), and
let the rule bind prospectively, milestone 16's parsing needs being the next real test.

**What the kernel is on this bus:** with `-bios default`, OpenSBI does no PCI setup, so the kernel
is the firmware: it sizes and places BARs itself. The hardcoded window/irq constants are held by
host-run witnesses against the machine's own device tree (the ECAM `reg`, all sixteen
`interrupt-map` entries), the UART's hardcode-with-a-witness pattern.

**Correction, on the record.** Parity C was recorded as blocked ("QEMU's riscv virt has no mmio
disk; it prefers PCIe"). It was not: the runners silently dropped `CRICKER_DISK` when the image
file did not exist, the machine was asked nothing, and the honest-looking "device-id 0" readings
were a diskless boot. Both runners now fail loudly on a missing disk file, parity C completed over
mmio in an evening, and the PCIe transport kept its own justification (the door to NVMe and real
NICs, the transport real hardware uses) rather than a manufactured one. The false record and its
correction are kept in notes/riscv-parity-scope.md because the mechanism, a silent no-op
manufacturing a plausible machine fact, is the instructive part.
