# 87. The x86_64 bare-metal machine

**Status: NOT-STARTED.** Raised 2026-08-03. The selection is made and recorded here; the milestone
completes when the machine is on the desk and has printed a byte over serial.

**Gate: HARDWARE.** As of 2026-08-18 it is the *second* kind the vocabulary now names: **the
machine is here and the serial module is not** (calef). The OptiPlex arrived; the Dell C4PDJ and the RS-232 chain have not.
That matters more than a partial delivery usually would, because this milestone's completion
condition is *"has printed a byte over serial"* and the box has no other way to say anything during
early bring-up. There is no console before the UART works, so nothing here can start.

**What a lane could do meanwhile: nothing on this block, and something adjacent.** The x86_64 port
is milestone 19's scope and is explicitly not gated on the purchase, because it starts under QEMU
TCG the way riscv64 did. So the useful move while the post is in transit is 19 under emulation, not
19 on this desk.

**Purchased 2026-08-15 (calef), the rest in transit**: the OptiPlex 7050 Micro
(i5-7500T, 16GB, 256GB NVMe, with its AC adapter, $139), the Dell C4PDJ serial module with its
cable ($18.88, the with-cable check the earlier draft flagged, answered by the listing's own
title), and the dev-side RS-232 chain (FTDI USB adapter at 1.5 ft, $15.96, plus a StarTech NM9FF
null-modem barrel, $7.98, chosen over a cable so the desk carries eighteen inches of serial, not
three feet). About $182 all-in against the $194 estimate, twelve days after selection; the $129
machine tier had aged out and nothing else moved. This milestone completes when the machine is on
the desk and has printed a byte over serial; the x86_64 port itself is milestone 19's scope and is
not gated on the purchase, because it starts under QEMU TCG the way riscv64 did. One bench note
for arrival, recorded here because both kits share the desk: this port is real RS-232 and the
boards' adapters are 3.3 V TTL, and the two chains must never swap; label them.

Milestone 19 names x86_64 as the third ISA, and the second ISA's lesson (milestone 16, the
VisionFive 2) is that the board should be chosen and ordered before the port needs it, from
requirements the port derives rather than from specs. Bare-metal bring-up is a loop of hang,
power-cycle, retry, so the machine must be dedicated and consequence-free; cordoba is disqualified
for bare metal on exactly those grounds (it is the production server, and a 2013 desktop board has
no BMC, no serial-over-LAN, no remote power), though it remains the KVM and VT-d *virtualized* test
host for the same port.

The requirements, each traced to something this tree already does:

- **A real 16550 COM port.** Early bring-up output exists before anything else works, and QEMU's
  q35 machine emulates the same legacy UART, so one driver spans emulator and silicon. This is the
  NS16550/PL011 pattern both existing ISAs follow, and it eliminates most modern consumer hardware.
- **VT-d**, because IOMMU-backed driver isolation (milestone 16) is a parity theme (§19), and the
  x86 side of the DMA-confinement story needs real hardware eventually.
- **A NIC QEMU can stand in for.** QEMU 11.0.2 (checked against the pinned binary, not the docs)
  emulates two modern Intel families: `e1000e` (I217/I218/I219) and `igb` (82576, whose driver
  family covers i210/i211/i350). It does **not** emulate `igc` (i225/i226), and upstream has
  nothing in flight. An i226 machine is therefore acceptable but taxed: the driver core gets
  written against QEMU's `igb` (igc is igb's descendant, so rings and descriptors carry over) and
  the igc deltas are ported on hardware. A minimal driver is 1,500-3,000 lines against Intel's
  public datasheet; the plumbing around it (PCI decode, DMA confinement, the userspace net server)
  already exists.
- **Four real cores** for the per-CPU scheduler, and any Intel core has the PMU that milestone 25's
  `sel4bench` comparison was deferred to real hardware for.
- **Remote power cycling** by smart plug, not by management firmware. A plug is $15 and works on
  anything.

**The selection: a used Dell OptiPlex 7050 Micro plus the Dell C4PDJ serial module** (calef,
2026-08-03, settled after a full pass over the new market): i5-7500T with 16GB was $129 with the
module at $35, ~$194 all-in with the dev-side serial gear and the smart plug. The used-hardware
risk was weighed deliberately and priced: eBay's money-back guarantee bounds "does it work" to
return friction, and at real configured prices every new machine cost $150-350 more. The 7050
keeps the fastest cores in the field and the I219 NIC in QEMU's `e1000e` family, so the
one-driver-spans-emulator-and-silicon property holds with no caveats. The module is Dell P/N
**C4PDJ** (fits 3050/7040/7050 MFF, snaps into the rear punch-out, cables to a motherboard
header; check the listing includes the cable); used units essentially never ship with it, so buy
it separately rather than hunting for a factory-configured unit.

The market at selection time, so the next reader knows what was weighed. The closest contender
was a **new Protectli VP2430** ($300 configured with coreboot): a real vendor with published
datasheets, open-source firmware aligned with `measured_boot`'s future on x86, console cable
included, but i226-V NICs in the `igc` family QEMU does not emulate, and $150 over the used
route; it stays **the recorded alternative** if the used machine disappoints or when open
firmware becomes the point. Configured industrial N100 boxes on Amazon ran $500-730 and are
dominated by the VP2430 at every point. A used PC Engines apu2 deserves a correction from the
first draft of this entry: its i210 NICs are `igb` family, so QEMU's igb model gives it the
one-driver property this entry originally credited only to the 7050; it stays a runner-up for its
EOL status and slow Jaguar cores, not its NIC. If netboot iteration becomes worth it, cordoba
hosts the PXE/TFTP end.

## Scope note

This milestone is the machine, the serial link proven, and nothing else; the port itself is
milestone 19's remaining scope and is not gated on this purchase, because the port starts under
QEMU TCG the way riscv64 did. Buying early is cheap insurance against the VisionFive 2 pattern
(ordered 2026-07, arrives ~2026-08-21) of the board being the long pole.
