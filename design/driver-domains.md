# Design proposal: driver domains, and the DMA-confinement design space

**Status:** open idea. Not decided. It is the principled version of a hole we already closed in
software (see notes/dma.md), and it would only be worth building alongside a real SMMU driver and a
decision to run cricker-os at EL2.

**Owner:** Chris

---

## What problem this is in the space of

Two problems that turn out to be the same problem:

1. **Confining a driver's DMA.** A device is a second bus master. It does DMA against physical
   addresses and honours no page-table permissions, so a hostile driver that can aim the device
   can make it read or write any physical memory. The MMU, which confines everything else, does
   nothing here.
2. **Isolating a driver's *faults* at full strength.** Milestone 8/9 already made a driver a
   process, so a driver bug is a crashed process. But a crashed driver today is not *restarted*,
   and a driver still shares the kernel's fate through any authority it holds (its interrupt, its
   MMIO, its DMA region).

Both are the question "how much can an untrusted driver hurt the rest of the system," and the
strongest answer the industry has is: **put the driver in its own virtual machine.**

## The three points in the design space

### 1. Software-mediated validation (what cricker-os does today)

The kernel keeps the two DMA-critical powers (the queue's ring addresses and the notify) and
validates every descriptor stays inside the driver's DMA region before the device sees it. See
notes/dma.md. It closes the hole with no new hardware, at the cost of putting the virtio
**transport** in the kernel. It is device-family-specific: the kernel understands the virtqueue
layout, and a different device class (a NIC, say) would need its own validator.

Cheapest, works everywhere, least general.

### 2. An SMMU (ARM's IOMMU)

The hardware answer. The SMMU sits between devices and memory and translates every address a device
emits through per-device (per-StreamID) tables the kernel programs. Point a driver's device at its
own region and the SMMU confines it **generically**, with zero device knowledge in the kernel. This
is what a real system does, and it is what DECISIONS §10 meant by "they had to bolt the isolation
on afterwards with an IOMMU."

Not reachable from our current wiring: QEMU `virt`'s SMMUv3 only covers the PCIe bus, so it would
mean switching to virtio-pci and writing a PCIe enumerator and an SMMUv3 driver. Most general, real
hardware, real work.

### 3. Driver domains (this proposal)

Run each driver in its **own virtual machine**, with cricker-os as the hypervisor at EL2. The
driver at guest EL1/EL0 talks to a device, and its DMA is confined by the SMMU's stage-2 tables
that cricker-os, as the hypervisor, programs for that domain. A compromised or crashed driver takes
down its VM and nothing else; cricker-os restarts the VM. This is the Xen "driver domain" / stub
domain model, and the QubesOS "sys-net / sys-usb" model: the most dangerous, most bug-prone code
(drivers) runs in disposable, DMA-confined boxes.

Most isolation, most infrastructure.

## Why the driver-domain point is compelling

- **It confines DMA generically, like an IOMMU, but self-programmed.** cricker-os owns the SMMU
  stage-2 for each domain, so a driver's device is boxed into that domain's memory. No per-device
  validator in the kernel. The virtio transport could go back into the (untrusted) driver, undoing
  the one compromise notes/dma.md made.
- **It is the strongest fault isolation there is.** A driver domain is a hard boundary: separate
  address space *and* separate exception-level context. A driver that corrupts itself corrupts a
  VM, which cricker-os tears down and respawns. This is the "kill a driver, watch it come back"
  demo from the application-ideas discussion, at its strongest.
- **It composes with the capability model.** A domain is handed exactly the device MMIO, interrupt,
  and DMA window it needs, as capabilities, and nothing else. A driver domain is a process with a
  harder wall.
- **It walks cricker-os toward what real high-assurance systems look like.** seL4 is often deployed
  as a hypervisor running Linux driver VMs; the microkernel-as-hypervisor is a well-trodden
  high-assurance pattern.

## What it requires, and what blocks it

- **cricker-os must run at EL2.** Today it boots to EL1 on purpose. Becoming a hypervisor means a
  second exception-level story: EL2 setup, `VTTBR_EL2` stage-2 tables per domain, trapping and
  emulating (or paravirtualizing) the guest's device access, virtual interrupt injection (the GIC
  virtualization extensions, `ICH_*` list registers), and a vCPU/scheduling model for domains. This
  is a large body of new mechanism, comparable in size to the whole EL0 story we already built.
- **It still needs an SMMU driver.** This is the crucial non-obvious point, and it is why driver
  domains are *not* a shortcut past option 2. CPU stage-2 (`VTTBR_EL2`) confines the **CPU**, not
  device DMA. Device DMA is confined only by the **SMMU's** own stage-2. So a hypervisor confines a
  passed-through device by programming the SMMU, exactly as a non-hypervisor kernel would. Driver
  domains are "option 2 (an SMMU driver) plus being a hypervisor," strictly more than option 2, not
  less.
- **Under HVF it is impossible as-is.** On the dev Mac, EL2 belongs to Hypervisor.framework, and
  HVF does not expose nested virtualization, so cricker-os cannot become a hypervisor while itself
  running as an HVF guest. This would be a bare-metal-only (QEMU with `virtualization=on`, or a
  real board) capability. See notes/virtualization.md for why we are a guest under HVF.
- **The device transport must be reachable behind the SMMU.** On QEMU `virt` that again points at
  virtio-pci rather than virtio-mmio, dragging in a PCIe enumerator.

## Where this sits relative to the microkernel philosophy

There is a real tension worth naming. A microkernel's whole thesis is a *small* privileged core.
Becoming a hypervisor adds EL2 mechanism, a vGIC, stage-2 management, and a vCPU model to that core,
which cuts against "small." The counter-argument, and the reason seL4 does exactly this, is that the
added mechanism is *general* (it isolates any guest, not any specific device), so the privileged
core stays conceptually simple even as it grows: it is still "isolate and route," now with VMs as
the unit of isolation. Whether that trade is worth it for cricker-os depends entirely on whether
driver isolation at VM strength is a goal, or whether process-strength isolation plus software DMA
validation is enough. For a learning OS, the latter has already taught the lesson; the former is a
second, larger course.

## Open questions

- Is the goal driver *confinement* (which software validation already gives) or driver *fault
  recovery and disposability* (which needs the VM boundary)? Only the second justifies the cost.
- Full VMs, or a lighter "protection domain" that is a separate address space *and* a separate
  SMMU context but shares cricker-os's scheduler, without a full vCPU/EL2 story? The lighter form
  might get most of the DMA benefit for much less mechanism, and is arguably where the real design
  work is.
- Paravirtual device backends (cricker-os presents a virtio backend to the driver domain, and owns
  the real device) versus device pass-through with SMMU confinement. The first keeps the real
  device out of the untrusted domain entirely; the second is closer to true driver isolation but
  needs the SMMU.

## Verdict

Parked, and honestly the most interesting unbuilt direction in the project, but it is a milestone in
its own right, not a fix. It only makes sense to reach for it once (a) there is a real SMMU driver,
which is the same work option 2 needs, and (b) there is a reason to run cricker-os at EL2 on bare
metal. The software validation in notes/dma.md closed the actual hole; this is the principled shape
the hole would take if the project decided driver isolation at VM strength was a goal worth its
weight. If it is ever built, notes/dma.md's "the software version of the hypervisor's DMA-remapping
role" becomes the real thing.
