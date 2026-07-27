# Scoping a PCIe transport (and virtio-pci)

**Built (2026-07-27): all four phases.** DECISIONS §18 records the decisions (INTx first, one
driver behind a kernel-side transport seam, mmio stays on aarch64); notes/pcie.md is the
after-the-build companion. One premise of this scope died on contact with the machine: parity C
was never actually blocked on PCIe (see the correction in notes/riscv-parity-scope.md), so the
transport was built on its own justification below, and C completed over both transports. The
rest of this note is the scope as written before the build.

Parity workstream C stalled because QEMU's riscv `virt` does not put a virtio-blk device on the
virtio-mmio slots; it uses **PCIe**, the transport real hardware uses. This scopes a PCIe root
complex and a virtio-pci transport as its own workstream. It is bigger than a virtio detail: a PCI
enumerator is the door to NVMe, real NICs, and anything past a handful of hand-placed MMIO devices,
and it is table stakes for "runs real workloads."

**It is portable.** Both QEMU `virt` boards expose a `pci-host-ecam-generic` host bridge; only the
ECAM base, the BAR windows, and the interrupt routing differ, and all three come from the device
tree. So this subsystem proves itself on aarch64 and RISC-V from one implementation, the way the
capability core does.

**What does not change:** the virtqueues, the descriptor/available/used rings, the DMA against
physical addresses, and therefore the whole DMA-confinement design (the kernel-owned shadow ring,
the address validator). PCIe is an enumeration-and-addressing layer *under* the same virtio you have.
The concentrated new work is PCI config-space enumeration and virtio-pci capability parsing.

Concrete addresses (QEMU riscv `virt`, from the DTB `pci@30000000`):

```
ECAM config space   0x3000_0000, size 0x1000_0000   (256 buses x 4 KB per function)
32-bit MMIO window   0x4000_0000, size 0x4000_0000   (where BARs are assigned)
64-bit MMIO window   0x4_0000_0000                    (large BARs)
INTx -> PLIC          irqs 0x20..0x23 (32..35), by (device, pin) via interrupt-map
host bridge          compatible = "pci-host-ecam-generic", bus-range 0..255
```

Effort is session-sized: **S** = part of a session, **M** = one to two, **L** = several.

---

## Phases

### P1. PCI config space + enumeration — M, low-medium risk.

- Map the ECAM window device-typed (base and size from the DTB `pci-host-ecam-generic` `reg`). A
  config address is `ecam + (bus << 20) | (dev << 15) | (fn << 12) | offset` (ECAM's flat layout).
- Config accessors (`cfg_read8/16/32`, `cfg_write*`), then enumerate: walk the buses (QEMU `virt` is
  flat on bus 0, but honour `bus-range` and recurse through bridges for generality), reading the
  **vendor/device id** at each `(bus, dev, fn)`. Virtio devices are vendor `0x1AF4`; virtio-blk is
  device `0x1042` (modern) or `0x1001` (transitional).
- **Proves:** the kernel finds the disk by enumeration, not by probing a fixed address: "PCI: virtio
  block device 1af4:1042 at 00:01.0". This is the PCIe analog of `virtio::find_block_device`, and it
  belongs in the same place (bus enumeration is a bootstrap role the kernel already owns for mmio).

### P2. BARs + virtio-pci capability parsing — M, medium risk.

- Read the device's **BARs** (config 0x10..0x24): each holds a memory region's base; size it the
  standard way (write all-ones, read the writable-bits mask back, restore). QEMU pre-assigns BARs in
  the MMIO window, so no allocation is needed, just reading. Handle 32- and 64-bit BARs.
- **Enable the device**: set Memory-Space Enable and Bus-Master Enable in the command register (0x04).
  Bus-Master is what lets the device DMA at all, and is the PCI-level version of the authority the
  kernel already guards.
- Walk the **capability list** (status register capabilities bit -> cap pointer at 0x34 -> linked
  list). Find the virtio vendor capabilities (cap id `0x09`) and read each one's `cfg_type`: common
  config (1), notify (2), ISR (3), device config (4). Each capability names `(bar_index, offset,
  length)`, and notify adds a `notify_off_multiplier`.
- Resolve those to physical addresses in the BAR window: the virtio common-config block, the notify
  register, the ISR, and the device-specific config.
- **Proves:** the kernel locates the same registers virtio-mmio handed it directly, now through the
  PCI indirection. "virtio-pci: common-cfg at 0x4000_1000, notify at 0x4000_3000 (mult 4), isr at ..."

### P3. Interrupt routing (INTx via the PLIC) — S-M, medium risk.

- Read the device's Interrupt Pin (config 0x3D: 1 = INTA). Resolve INTA for this device through the
  DTB `interrupt-map` / `interrupt-map-mask` (device number and pin select a PLIC irq in 32..35 on
  riscv `virt`). Then reuse the existing path: `bind_irq` the PLIC irq to an endpoint, grant the
  driver an `Irq` capability.
- **INTx first, not MSI-X.** Legacy INTx is a wire that routes through the PLIC, which is exactly the
  interrupt model you already have (the userspace UART driver's `WAIT`/`ACK`). MSI-X (the device
  writes a message to raise an interrupt) is more scalable and more modern, but it is a separate,
  larger piece; add it later if a device needs many vectors.
- **Proves:** a virtqueue completion on the PCIe device reaches a userspace driver as a message,
  through the same `Irq` capability mechanism as an mmio device.

### P4. Wire virtio-pci to the driver and the DMA confinement — L, medium-high risk.

- The virtio-pci **common-config** register layout differs from the flat virtio-mmio block
  (`queue_select`, `queue_size`, `queue_desc/driver/device`, `queue_notify_data`, `device_status`,
  ...). The kernel's transport ownership (it holds the queue addresses and the notify, for
  confinement) and the driver's register access both need a **transport abstraction**: a small trait
  or enum with the handful of operations (select a queue, set its ring addresses, notify, read/ack
  the ISR, read/write status and features), implemented once for mmio and once for pci. Everything
  above it - the shadow ring, the address validator, the block request format, the userspace driver's
  logic - stays identical.
- The DMA confinement is unchanged in spirit: the kernel still owns the ring addresses and the notify,
  still validates every descriptor against the driver's DMA region, still copies into a kernel-private
  shadow the device actually reads. The only additions are PCI Bus-Master enable (P2) and reading the
  queue registers from the common-config block instead of the mmio offsets.
- **Proves:** a userspace driver reads a file off a **PCIe** virtio disk - the aarch64 mmio milestone
  (`a_userspace_driver_reads_a_file_from_a_virtio_disk`), now over PCIe, and on both architectures.

---

## Decisions to raise before building

1. **INTx vs MSI-X.** Recommend INTx first (matches the PLIC/`Irq`-cap model; MSI-X is a later
   enhancement).
2. **One driver, two transports, or two drivers.** Recommend a thin transport abstraction so the
   virtqueue/DMA/confinement logic and the userspace block driver are shared between mmio and pci.
   This keeps the intricate, security-critical code (the validator, the shadow ring) in one place.
3. **Keep virtio-mmio?** aarch64's mmio path works and its tests pass; no need to migrate it. PCIe
   becomes the RISC-V disk path and a demonstrated portable subsystem. Both could eventually use PCIe,
   but there is no reason to disturb a working, tested mmio path to do it.

## Sequence and dependencies

```
P1 (enumerate) -> P2 (BARs + caps) -> P3 (INTx) -> P4 (transport + driver)
```

Strictly linear: each phase needs the previous. P1-P3 are the new PCI machinery (bounded, well
specified). P4 is where it meets the existing virtio and is the largest, because it introduces the
transport seam and adapts the confinement. Overall size is comparable to the SMP workstream: a real
subsystem, not a tweak.

## Relation to other work

- **Parity C** completes as a consequence of P1-P4 (a virtio disk on riscv, over PCIe).
- **DMA isolation (task #12, SMMU/IOMMU).** The software confinement here is the stand-in for an
  IOMMU. A real PCIe IOMMU (or the RISC-V IOMMU) would sit in front of Bus-Master DMA and is the
  hardware version of the same guarantee; the enumeration and BAR work here is a prerequisite for
  ever driving one.
