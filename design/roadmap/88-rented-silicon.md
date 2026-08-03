# 88. cricker-os on rented silicon: Oracle's free tier first, Graviton metal for the PMU

**Status: NOT-STARTED.** Raised 2026-08-03, from the observation that several open milestones are
hardware-gated and a cloud instance is hardware without a purchase. Oracle-first is Chris's call
(2026-08-03): the always-free A1 shape (4 Ampere cores, 24 GB) makes the recurring cost zero, so
the rented-silicon story starts with no bill to watch.

**What this buys that no machine on the desk can.** Every benchmark in this tree so far runs on
hardware the reader has to take our word about. A cloud instance is the same silicon for everyone:
"cricker-os against Linux on the same free shape, here is the image, rerun it on your own free
account" is a credibility claim no desk machine can make, and it is the demonstrator thesis
(DECISIONS §14) applied to the *audience* rather than the code. Second, an always-free instance
can stay up, which opens a door no burst rental does: a public, always-on cricker-os demo
instance, its own decision later because exposing this kernel to the internet is a security
posture question, not a deployment step. Third, milestone 25's deferred `sel4bench` needs a real
PMU, no VM exposes one, and Graviton `.metal` rents one by the hour; that stage stays AWS.

**Why Oracle is friendlier to this kernel than EC2, and the honest unknowns.** OCI's A1 VMs are
KVM guests with paravirtualized devices, and this tree already drives virtio-net and virtio-blk
over the PCIe transport (DECISIONS §18), so the existing drivers plausibly work where EC2 would
demand a new ENA driver on day one. "Plausibly" is load-bearing: whether A1's launch mode
presents virtio to a custom non-Linux image, which UART its serial console emulates, and whether
the custom-image import path (QCOW2) accepts an arbitrary UEFI payload are all facts to measure
on arrival, not assume. The known caveats, recorded so nobody rediscovers them: always-free A1
capacity is famously scarce in popular regions, Oracle reclaims idle always-free instances unless
the account is upgraded to pay-as-you-go (which keeps the free tier free but adds a card), and
the 4 OCPUs can be split across at most two instances.

**What it costs in engineering, named up front.** No cloud takes a kernel image; it takes a disk
image that boots via UEFI, so the kernel needs a boot path it does not have (an EFI stub or a
bootloader stage). Server aarch64 VMs describe the machine with **ACPI, not a device tree**,
which is a new discovery front door (milestone 60 built the DTB one). None of this is wasted
motion: the UEFI work is what optional milestone 24 (Virtualization.framework) also needs, and it
is provider-neutral by construction, so Graviton, Azure and Google's Ampere shapes become
reachable with the same boot path.

**The staging that keeps it honest**, each stage a deliverable on its own:

1. Boot under UEFI locally (QEMU `virt` with AAVMF firmware), serial byte out. No cloud yet.
2. The always-free A1 shape: custom image imported, a byte on the OCI serial console. This is the
   milestone's "printed a byte over serial" moment, at $0.
3. The bench suite against Linux on the identical free shape, published with the image, so the
   reader's rerun is also $0.
4. Graviton `.metal` by the hour, for the PMU: the `sel4bench` comparison milestone 25 deferred.
   The only paid stage, used in bursts.
5. Stretch, each its own decision later: virtio networking on the A1 instance with the drivers
   the tree already has, and the public demo instance question.

## Scope note

Nothing here retires the VisionFive 2 or the milestone 87 machine: aarch64 cloud has no IOMMU
control from inside a VM and no physical peripherals; it complements the boards, it does not
replace them. (RISC-V rental is milestone 89's subject, not this one's.) Nothing in this
milestone may regress the QEMU boot: DTB stays the first-class discovery path, ACPI is a second
front door beside it, gated by the same parity rule as everything else (§19).
