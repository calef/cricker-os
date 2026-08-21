# 16. Real hardware + IOMMU-backed driver isolation (recast 2026-07-27: RISC-V first)

**Status: PARTIAL.**

**Gate: HARDWARE.** Not the "no board" kind: the board is on the desk. **The remaining work needs
somebody sitting at it** (flashing an image, watching a serial console, power-cycling a wedged
board), which is the second sense the gate vocabulary now names, and it is why this sat on
`--ready` for days offering work no lane could take. 16b is built, the board arrived 2026-08-14, and the bench delivered first silicon
the same day: eleven boots across two sessions, ending with the full tour completing on three harts
("nife: the capability core runs on RISC-V"), the four predicted code changes landed plus the
ones the bench actually demanded, and notes/visionfive2.md carrying the whole narrative. What
remains of 16a is engineering, not hardware, and each piece is lane-sized: the on-board test-suite
exit (the `sifive_test` device does not exist on silicon; the note proposes a UART pass/fail
marker), the DTB-driven UART IRQ for the driver demo (the 10-versus-32 BUGS entry), and the
real-cycle benches, which ride milestone 74's SBI PMU half. Carrying 16b's IOMMU driver to silicon
is now milestone 143, split out 2026-08-20, because it waits on a board that ships the ratified
RISC-V IOMMU spec and no such board exists today.

**In brief.** **16a:** first silicon on a VisionFive 2-class board, whose firmware contract (OpenSBI, SBI HSM, NS16550, PLIC, Sv39) is exactly what the kernel already speaks. **16b:** IOMMU-backed DMA isolation against QEMU's emulation of the **ratified RISC-V IOMMU** (v1.0.1) first, over the §18 PCIe transport; silicon when a board ships it

**Why it matters.** isolation in hardware, under real workloads; the second ISA becomes the first silicon, and the IOMMU work stops waiting on a purchase

The milestone was always two things bundled, first silicon and DMA isolation in hardware, and
the recast splits them, because each is better served on the RISC-V side now.

**16a: first silicon, on a VisionFive 2-class RISC-V board.** The riscv port's firmware contract
on real boards is IDENTICAL to what the kernel runs today: OpenSBI, SBI HSM bring-up (the hart
lottery is already survived, on the record), NS16550, PLIC, Sv39. A ~$60-100 board boots the
exact contract we speak; the aarch64 side fits real boards worse (a Pi wants TF-A for PSCI, its
default is spin-table, and its IOMMU story is the weak spot notes/target-hardware.md already
flags). Deliverable: boot, UART, SMP, the test suite where semihosting allows, and the benches
on real cycles via the SBI PMU extension. Caveat, stated now: sel4bench's platform coverage is
thinner on RISC-V than ARM, so the milestone-25 seL4 comparison may still eventually want an ARM
board; that purchase moves to "when 25's leftover justifies it".

**16b: IOMMU-backed DMA isolation, in emulation, on BOTH boards** (parity, calef's direction
2026-07-27). Each `virt` board emulates its architecture's native IOMMU: SMMUv3 on aarch64
(`-machine virt,iommu=smmuv3`, mature) and the ratified RISC-V IOMMU (v1.0.1) on riscv (newer;
its bugs may be QEMU's, and the record should say which is which). Both sit in front of PCIe,
which §18 drives on both boards; both need `iommu_platform=on` per virtio device, and a device
without it silently bypasses translation, the same manufactured-fact hazard the runners now
fail loudly on. The two IOMMUs are structural siblings, and the deep symmetry is the payoff:
each translates with its own CPU's page-table format (VMSAv8-64; Sv39), so the format-generic
`paging` crate, the seam that was HAL leak #2, builds IOMMU domains with the same proved code
that builds process address spaces. Shape: one portable DMA-domain seam, two arch IOMMU
drivers under `arch/` (device table, command queue, fault queue each), the `Virtio` capability
unchanged above, the disk and attacker suites running behind the IOMMU on both ISAs, and the
shadow ring demoted to defence in depth everywhere. Silicon carries 16b's riscv code over when
a board ships the ratified spec; that is the emulate-then-carry pattern the kernel was built
on. Parity is claimed at the QEMU tier; 16a's silicon is one board first, honestly.

**Built 2026-07-28** (16b, both ISAs in emulation; DECISIONS §20, notes/iommu.md). The portable
DMA-domain seam (`crate::iommu` over `paging::domain`), the two arch drivers (SMMUv3, RISC-V IOMMU
v1.0.1), boot bring-up (SMMU from the device tree, RISC-V IOMMU enumerated as a PCI function), the
`iommu_platform=on` enablement with the confinement test as the loud-on-bypass guard, and the disk
and both attacker suites passing behind the IOMMU on both boards (aarch64 118 kernel tests, riscv
60). Both emulations behaved to spec, no QEMU-vs-ours bug surfaced. Shadow ring kept as defence in
defence in depth. Remaining under 16: **16a** (first silicon on a RISC-V board) is still the hardware step;
16b's riscv driver carries over when a board ships the ratified spec, which is milestone 143.

**Why.** This is where the discussion's strongest pro-microkernel argument finally becomes true
for us. Today driver isolation is real only because of the shadow descriptor ring we wrote
(notes/dma.md); an IOMMU makes it real in hardware, with the software ring demoted to defence in
depth.

**Prior art.** design/driver-domains.md already works the principled version (a driver per VM,
stage-2 behind a hypervisor). Hardware-gated there; 16b's emulation-first path is not.

**Also closes an integrity window (milestone 22's precondition).** Before DMA is confined in
silicon, a malicious device can DMA over any RAM the kernel has not walled off, *including the
initrd holding init before the kernel has loaded and measured it*. Software confinement (the shadow
ring) governs a driver the kernel already trusts to run; it does nothing about a device corrupting
init's bytes at rest. So verifying init (22) is only airtight once 16 removes the way to tamper with
it underneath the check.
