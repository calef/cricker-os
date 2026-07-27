# PCIe, and driving a disk over it

The PCIe transport (DECISIONS §18): how the kernel finds a device on a PCI bus, brings it up, and
runs the same userspace virtio driver over it that runs over virtio-mmio. Scoped in
notes/pcie-transport-scope.md; this note is the "what actually happened and what the words mean"
companion after the build.

## The shape of PCI, in one screen

A PCI function is addressed by **BDF**: bus (8 bits), device (5), function (3). Every function
owns 4 KB of **configuration space**, and **ECAM** (Enhanced Configuration Access Mechanism) is
the modern way to reach it: one flat memory window, the function's page at
`base + (bus << 20 | dev << 15 | fn << 12)`. No magic I/O ports, no indirection registers; config
space is just memory-mapped bytes, which is why an empty slot "reads all-ones" (nobody drives the
bus, the read floats high) and why enumeration is a loop, not a protocol.

The first 64 bytes of config space are standardized: vendor/device id (how you recognize what it
is), the command/status registers, and six **BARs** (Base Address Registers). A BAR answers "where
do this function's actual registers live in memory?" and it is writable: firmware assigns each
function an address by writing one. Sizing is the famous dance: write all-ones, read back which
bits stuck (the low bits that stay zero encode the size and alignment), restore. Past the header,
optional features hang off the **capability list**, a linked list in config space; virtio-modern
puts everything it needs there as vendor capabilities: which BAR (and offset) holds the
common-config block, the notify doorbell, the ISR byte, the device config.

Two command-register bits matter here. **Memory-Space Enable** makes the BARs decode at all.
**Bus-Master Enable is DMA permission at the bus level**: a device without it cannot issue a
single memory transaction. The kernel grants it last, after the confined transport is registered,
because it is the bus-level twin of the authority the confinement layer polices.

## What the kernel is, on this bus

With `-bios default`, OpenSBI does no PCI setup: every BAR arrives zero. So the kernel is the
firmware here: it sizes each BAR and places it in the board's 32-bit PCI memory window
(`PCI_BAR_BASE`, bump-allocated, size-aligned). On a UEFI machine the firmware would have done
this and the kernel would only read; both paths go through the same code, because `read_bars`
reports assigned bases and the kernel places only the zeros.

Division of labor, same as the mmio side: the **pci crate** is pure decode logic (ECAM math,
enumeration, BAR sizing, capability parsing, the INTx swizzle), host-tested against a fake config
space; **kernel/src/pci.rs** supplies the volatile accessors and the policy (which device, where
BARs go, which bits to set); the driver stays in userspace, unchanged.

## The transport seam

virtio is one device model over multiple buses. The queue machinery (descriptor table, available
and used rings, DMA against physical addresses) is bus-independent; only "where are the
registers and what do they look like" differs. `virtio::Transport` is that difference, contained:
the mmio variant passes the vocabulary through; the pci variant translates each register name to
the virtio-pci common-config layout, the ISR byte (whose *read* is the ack, deasserting INTx),
and the notify doorbell (`notify_base + queue_notify_off * multiplier`, resolvable only with the
queue selected). Registers pci has no equivalent for (magic, version, device id) are synthesized,
so the driver's sanity checks mean the same thing on both buses.

Everything above the seam is one copy: the shadow ring, the descriptor validator, the queue
layout contract, the userspace driver binary. The DMA confinement was written once and now
polices two buses, which is the demonstrator's argument in miniature.

## INTx

The legacy PCI interrupt is four shared wires (INTA..INTD) routed up through the bridge with a
standard rotation ("swizzle"): device `d` pin `p` lands on line `(d + p - 1) % 4`, then the board
maps the four lines onto interrupt controller inputs (32..35 on riscv `virt`). It is
level-triggered: the line stays asserted until the ISR byte is read. That plugs directly into the
kernel's existing model: the PLIC delivery masks the source, the driver's WAIT wakes, its
INTERRUPT_STATUS read (the ISR, via the transport) deasserts the line, and its Irq-capability ACK
re-enables the source. MSI-X (the device writes a message to raise an interrupt, many vectors,
no sharing) is the modern mechanism and a deliberate later step; nothing we drive needs it.

The swizzle and the ECAM base are hardcoded constants with **witnesses**: host tests parse the
riscv fixture's device tree and hold the constants against the machine's own `reg` and all
sixteen `interrupt-map` entries (crates/pci/tests/qemu_virt_dtb.rs), the UART pattern.

## What is proven, and where the edges are

The riscv suite's `a_userspace_driver_reads_a_file_over_the_pcie_transport` runs the whole line:
ECAM enumeration finds 00:01.0, the kernel places its BARs, sets up queue 0 through
common-config, the driver (byte-identical to the mmio one) submits a request past the shadow-ring
validator, the doorbell rings, and the completion arrives as INTx through the PLIC.

Both boards run it: riscv (INTx via the PLIC) and aarch64 (INTx via the GIC, SPIs 3..6, and the
**highmem** ECAM at 0x40_1000_0000; the machine names the node `pcie@10000000` after the low MMIO
base, and trusting the name instead of the `reg` is a mistake the witness test now guards). The
per-arch cost of the second board was constants plus two map entries, the portability claim in
concrete form.

Edges, honestly: bus 0 only is mapped and enumerated (QEMU `virt` is flat; widening is one
constant); INTx only, no MSI-X; the modern function only (`disable-legacy=on` in the runner,
because QEMU's default virtio-blk-pci is transitional and we do not drive the legacy layout);
both boards keep their working mmio paths alongside. The DMA
confinement is unchanged in spirit and in code; what PCIe adds to the trust story is Bus-Master
Enable, the bus-level DMA switch the kernel now controls explicitly.
