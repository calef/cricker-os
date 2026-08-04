# 29. The framebuffer is a bigger grant, not an exemption (milestone 29, the display ladder's rung one)

**Built 2026-07-29**, both ISAs, in QEMU. The demonstrator's first pixels: a userspace virtio-gpu
driver that puts a known image in a scanout framebuffer, confined exactly like the disk and net
drivers, plus a *separate* client process that draws into a shared surface through a capability. Font
rendering, the VT state engine, scrollback, and input are deliberately not in this rung; they arrive
as clients of the contract this one draws. See notes/framebuffer-contract.md.

**The split is the deliverable, not the picture.** The driver holds the `Virtio` capability, the
device's interrupt, and the whole DMA region; the client holds an endpoint and the pixels and is
handed no physical address at all. It cannot program a queue, ring a doorbell, or see a descriptor
ring (they are in a page it is not mapped), so the worst a hostile client can do is draw nonsense.
Rung two (the compositor, milestone 33) takes the client's place unchanged, which is why the contract
is written down as a note and a host-tested crate (`crates/gfx_proto`) rather than left implicit in
two programs.

**The memory decision, stated as a rule because it will recur.** A 128x64 surface at 4 bytes a pixel
is 32 KiB, and every other driver here gets one 4 KiB DMA page. The tempting shortcut was to let the
framebuffer live outside the registered DMA region, since it is "just pixels". That is exactly
backwards: it would put the one device that reads bulk memory outside the confinement everything else
is inside. So the region is **wider, not special**: `1 + SURFACE_FRAMES` contiguous frames, page 0 for
the rings and control buffers (driver-private) and pages 1.. for the surface, registered whole.
**A device that needs more memory gets a bigger grant, never an exemption.** The block server's
two-page region (§27 era) was the first instance; this is the general form.

**`crates/dma_validator` needed no change,** and that is a property rather than luck: it bounds
`addr..addr+len` inside a region whose size is a parameter, so the region growing ninefold left the
proof covering it. Recorded because the increment was explicitly allowed to stop and ask if the
framebuffer's size had required touching a proved crate, and it did not.

**The confinement hazard a GPU adds, and the barrier that actually stops it.** This is the first
device here whose DMA addresses do not all arrive in descriptors. A virtio-gpu's *backing* addresses
ride in a `RESOURCE_ATTACH_BACKING` **command payload**; the kernel bounds the descriptor carrying
that command, but the addresses inside it are bytes it does not parse. It should not start parsing
them, because that would put device knowledge in the transport, which is the line §18 draws, and it
would be a per-device arms race. So the **IOMMU** (§20) is the barrier for this class of address, and
it is proved rather than assumed: `the_iommu_refuses_the_gpu_a_framebuffer_outside_the_drivers_grant`
gives an attacker exactly the honest driver's authority, points a resource's backing at a frame the
kernel left out of its domain, and asserts the IOMMU recorded a fault there. Both ISAs.

Two consequences follow and are recorded so they are not discovered later. `iommu_platform=on` carries
more weight for the GPU than for the disk (drop it and the disk still has the shadow ring; the GPU has
nothing). And **on a board with no IOMMU this hazard is open**: the VisionFive 2 has none, so a display
driver on first silicon is either trusted or the transport grows a virtio-gpu-aware check. That is a
decision for whoever sequences 16a, not one this milestone gets to make silently.

**Correction, on the record: a device's "command accepted" is not evidence of a DMA.** The escape test
first asserted that the device *refused* the out-of-grant backing. It did not. QEMU's DMA layer
answers a translation failure by handing the device a bounce buffer rather than failing the mapping,
so the command returns OK while the bytes the device gets are not the victim frame's. The confinement
held; only the error reporting did not survive the trip. The test now asserts on the IOMMU's fault
queue, the hardware's own account, and the response code is printed for the record. An earlier
iteration also aimed the escape at "the frame just past my region", which was wrong because the
kernel's shadow page is allocated immediately after the region and *is* in the domain; the kernel now
picks the victim frame and hands it to the attacker.

**A found limit, recorded for whoever handles faults in production: the RISC-V IOMMU's fault queue
overflows silently.** The driver gives it 128 records and never clears the queue's overflow bit, so a
flood of faults latches the overflow and no further fault is recorded at all. Found the right way: the
escape test's first version attached a 4096-byte backing, produced a flood, and the *next* test in the
suite (§20's `the_iommu_faults_a_dma_that_escapes_the_domain`) then reported the IOMMU as not confining
the device. Mitigated locally (the escape attaches four bytes, so one translation and one fault; the
test drains the queue afterwards) rather than by changing the arch driver, which is a different lane.
What is left for a fault-handling milestone: clear the overflow bit when draining, and decide what a
production kernel does when a confined device faults. See notes/framebuffer-contract.md.

**Correction: the PCI transport was synthesizing a device id nobody had checked.** `Transport::Pci`
answered a driver's virtio-mmio `DeviceID` read with a hardcoded 2 ("I am a block device") for every
device on the bus. Harmless while only the disk and NIC rode it, since neither reads the register, but
it is a manufactured fact of the shape the runners were taught to fail loudly on. The GPU driver is
the first that checks what it is talking to, and it found the lie. The transport now carries the
virtio device type recovered from the PCI id (`0x1040 + type`).

**PCIe only, and that is the honest parity statement.** Neither `virt` board has a virtio-gpu on its
virtio-mmio bus in any configuration, so unlike the disk and the NIC there is no mmio twin to prove
the transport seam over twice. The parity that §19 demands is aarch64 `virt` and riscv `virt`, and
both carry `virtio-gpu-pci` over the §18 transport, proven by **one arch-neutral test** rather than
two copies that can drift.

**What the pixels are proven by, in two halves, because one half cannot reach the whole path.**

*In the guest, the framebuffer.* The pattern is a per-coordinate function rather than a fill (a blank,
filled, transposed, one-row-shifted, or one-pixel-shifted surface all fail), the digest is position
sensitive, and two independent witnesses in two address spaces report it (the client from its mapping
after the flush, the driver from a different mapping after the device reported the transfer complete),
both compared against a value the kernel computed itself.

*From the host, the scanout.* An in-guest test cannot go further: `-display none`, and nothing in the
guest can read QEMU's host-side surface back, so a wrong pixel format or scanout rectangle would pass
the guest's half while showing garbage on a screen. So the **host** proves that half. QEMU's monitor
works headlessly, and `cargo xtask` drives it beside the ordinary test run (no second boot: the pattern
stays on the scanout until QEMU exits, so nothing needs synchronizing), dumps the scanout with
`screendump`, and compares the PPM against `gfx_proto::pixel` pixel for pixel. **Both ISAs, and the
checker has its own negative control** (`cargo test -p xtask`: it must reject black, red/blue-swapped,
row-shifted, one-pixel-wrong, and the default 640x480 console). The geometry is part of the assertion,
which means a dump that is 128x64 at all is evidence `SET_SCANOUT` reached the device.

One ordering fact is load-bearing and deliberately fail-loud: the confinement test resets the device,
which destroys the scanout, so it must run before the pixel test; it is named
`a_backing_outside_the_grant_is_refused_by_the_iommu` to sort first, and a reordering fails the scanout
check rather than quietly skipping it. What remains unproven is only what QEMU cannot answer: that a
physical panel would show this, which is a silicon question. See notes/framebuffer-contract.md.

**Deferred, untouched:** the VT engine's language (libghostty-vt in Zig through its C ABI, or `vte` in
Rust as the single-toolchain fallback). This rung needs neither, and the contract carries pixels, not
text, so either slots in above it later.
