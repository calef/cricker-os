# 53. The board's own peripherals: network and storage on real silicon

**Status: PARTIAL.** The storage half is built for QEMU as of 2026-08-15 (pull request #193): the
`nvme` crate (queue mechanics, host-tested, 5 Kani harnesses), a rule-2 kernel driver confined
through the IOMMU before enable, class-code enumeration over §18, and an end-to-end boot test on
both ISAs. What remains of the milestone: the network half (the JH7110's GMAC), the board-side
PLDA XpressRICH root complex that carries the NVMe driver to the real M.2 slot, and the EL0
question, which is §86 (PROPOSED). Scope and honest limits: notes/nvme.md, BUGS included.

**Gate: NONE.** Both halves of the old gate are gone. The hardware cleared 2026-08-14: the board
is on the desk and 16a boots it through the full tour, so the two drivers finally have silicon to
exist on. The storage fork was decided by calef on 2026-08-15: **NVMe first**, because the backup
workload (milestone 55) measures sustained sequential write and endurance, which SD media fails,
and because a real PCIe root-complex driver compounds into milestone 87's x86 machine where an
MSHC driver serves one slot on one board. SD/eMMC stays in scope as the later path, undecided only
in its ordering against the network driver.

**In brief.** Milestone 16a boots a VisionFive 2 (firmware contract, NS16550, PLIC, Sv39). It does not
give the board a network or a disk. Everything above needs both, and **this is where virtio stops
carrying us**: every driver we have talks to QEMU's paravirtual devices, and real silicon has none.

**What it needs.**

- **Ethernet.** The JH7110 uses a Synopsys DesignWare GMAC (`dwmac`). Our net_stack (smoltcp) is
  device-agnostic above the driver, so this is a driver, not a stack rewrite. Rule 2 applies: it takes
  a base address and knows nothing else.
- **Storage**, and there is a real choice here. The SD/eMMC controller is the simplest path; **NVMe
  over PCIe** is the better one, because §18's PCIe transport already exists and NVMe would give the
  backup target actual throughput. Deciding which comes first is a fork, and it should be decided on
  measurement of what the backup workload needs rather than on what is easiest.
- **Persistence proven the hard way.** RedoxFS on the real device, with crash consistency tested by
  **actually cutting power**, which is a test QEMU cannot run.

**The parity note this milestone must carry.** These drivers are board-specific and aarch64 has no
equivalent board yet, so rule 5's "a scope note records the gap and the plan" applies rather than its
"ships on every architecture". Say so explicitly; do not let it look like an oversight.

**Effort: not estimated.** Two device drivers against real hardware with no emulator to iterate
against is a different activity from everything done so far, and estimates calibrated on QEMU work do
not transfer.
