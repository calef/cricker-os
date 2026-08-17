# The aarch64 board for the seL4 comparison

*(Milestone 25's leftover. Surveyed 2026-08-14; prices and stock checked that day. This note is the
selection block in milestone 87's shape; the milestone file itself is the integrator's to mint, and
this note's name is provisional.)*

Milestone 25 is done except for `sel4bench`, which times single operations through the PMU cycle
counter that neither QEMU-TCG nor Apple HVF provides (notes/benchmarks.md, notes/pmu.md). Milestone
74 is the driver that will read that counter; this survey picks the board it reads it on. The
purpose constrains the choice more than usual: the point is nife and seL4 **on the same
silicon**, so a board seL4 merely boots on fails the milestone even if our port to it is easy.

## The deciding criterion: where sel4bench really runs

"seL4 supports it" spans three tiers, and only the top two serve this milestone. All three are read
from seL4's own trees, not from the marketing page:

- **Published.** The seL4 performance page carries exactly **one aarch64 platform: the Jetson TX1**
  (Cortex-A57 at 1.9 GHz, IPC call 413 cycles, reply 426, regenerated 2026-08-13). Those are the
  numbers notes/benchmarks.md already compares against.
  Source: <https://sel4.systems/performance.html>.
- **CI-run on hardware.** Every push to sel4bench master builds and runs on the Foundation's
  machine queue. The aarch64 platforms in the run matrix: **tx1, tx2, odroid_c2, imx8mm_evk**
  (the rest are armv7a, x86, riscv). Results land in seL4/sel4bench-results (private); only TX1's
  reach the public page.
  Source: `.github/workflows/sel4bench.yml` `hw-run` matrix in <https://github.com/seL4/sel4bench>,
  and `sel4bench-hw/builds.yml` in <https://github.com/seL4/ci-actions>.
- **Builds only.** sel4bench CI builds images for essentially every armv8a platform in
  `seL4-platforms/platforms.yml`, including **rpi4, odroid_c4, and rpi5**, but never runs them:
  RPI4 is marked `no_hw_test: true` (the Foundation runs no hardware CI on it at all, sel4test
  included), and RPI5's entry says in a comment "the seL4 foundation has no RPI5 hardware".
  ODROID-C4 has two units in the machine queue for sel4test but is absent from the bench matrix.
  Source: `seL4-platforms/platforms.yml` and `sel4bench/build.py` in
  <https://github.com/seL4/ci-actions>.

A builds-only board would mean producing the seL4 side's numbers ourselves on a configuration the
Foundation has never validated, which is the "seL4 only boots there" failure with extra steps.

## Requirements, traced to the tree

- **GICv2.** `kernel/src/drivers/gic.rs` speaks GICv2 only: memory-mapped GICD plus banked GICC,
  no redistributor, no system-register CPU interface. **The kernel does not drive GICv3**, and that
  is a real constraint this survey records: a GICv3-only board (any recent Rockchip or i.MX8M)
  buys a new interrupt-controller driver before the first interrupt.
- **PL011 or NS16550 UART.** `kernel/src/drivers/pl011.rs` and `ns16550.rs`; the 16550 driver
  already parameterizes `reg-shift` and `reg-io-width` (built for the JH7110's DW-8250), which is
  exactly the shape Tegra's 8250-compatible UART needs.
- **DTB boot.** The device-tree front door (milestone 60) is how the kernel learns the machine;
  `kernel/src/smp.rs` reads `/psci` from it.
- **PSCI for SMP.** `smp.rs` starts secondaries with PSCI `CPU_ON` on the conduit the DTB names
  (`hvc` or `smc`, `arch/aarch64/mod.rs`) and explicitly does not speak `spin-table`. The Pi
  family's stock firmware default is spin-table (the 16a note records this), so a Pi is
  single-core for us until TF-A's upstream BL31 port is installed as the armstub.
- **A PMU readable at EL1.** Milestone 74's aarch64 half reads `PMCCNTR_EL0`; PMUv3 is architected
  on every core surveyed here, so this differentiates nothing below, but firmware must not inhibit
  it (a to-verify item).
- **A reachable serial console** (notes/target-hardware.md's third question), priced into the
  all-in cost.
- **RAM** is a non-constraint for the bench boot; anything at 1 GB or above clears it.

## The candidates

Prices and availability checked 2026-08-14.

| board | CPU | RAM | GIC | UART | PSCI | sel4bench tier | price, availability |
|---|---|---|---|---|---|---|---|
| **Jetson TX1 dev kit** | 4x A57 @ 1.9 GHz | 4 GB | GIC-400 (v2) | 16550-compat @ `0x70006000` | yes, TF-A BL31 (upstream tegra210 port) | **published** | ~$80-105 used, eBay; EOL at NVIDIA |
| Jetson TX2 dev kit | 4x A57 + 2x Denver | 8 GB | v2 | 16550-compat | yes, TF-A | CI-run | ~$150-350 used; EOL |
| ODROID-C2 | 4x A53 | 2 GB | GIC-400 (v2) | Amlogic meson (neither of ours) | yes, BL31 | CI-run | discontinued (S905 EOL); used only, spotty |
| i.MX8MM EVK | 4x A53 | 2 GB | **GIC-500 (v3)** | i.MX UART (neither of ours) | yes, TF-A | CI-run | **$388-462 new** (Future, DigiKey); some 15-week lead times |
| Raspberry Pi 4B | 4x A72 | 4/8 GB | GIC-400 (v2) | PL011 | via TF-A armstub (upstream); stock default is spin-table | builds only (`no_hw_test`) | $55-75 new, in stock everywhere |
| Raspberry Pi 5 | 4x A76 | 4-16 GB | GIC-400 (v2) | PL011 (debug header) | TF-A rpi5 exists; stock is spin-table | builds only; Foundation has no hardware | $60-80 new |
| ODROID-C4 | 4x A55 | 4 GB | GIC-400 (v2) | Amlogic meson | yes, BL31 | sel4test hardware, **not** in bench matrix | ~$50-65 new, stock thinning (DRAM prices) |

Sources for the board facts: TX1 serial and 16550 compatibility
(<https://jetsonhacks.com/2015/12/01/serial-console-nvidia-jetson-tx1/>, J21 pins 8/9/10, `ttyS0`),
TF-A Tegra and Raspberry Pi 4 platform docs
(<https://trustedfirmware-a.readthedocs.io/en/latest/plat/nvidia-tegra.html>,
<https://trustedfirmware-a.readthedocs.io/en/latest/plat/rpi4.html>, the rpi4 port patches the DTB
to advertise PSCI and enters the kernel at EL2), seL4's per-board pages
(<https://docs.sel4.systems/Hardware/>), BCM2712's GIC-400
(<https://forums.raspberrypi.com/viewtopic.php?t=371974>), i.MX8MM's GICv3
(<https://community.arm.com/support-forums/f/architectures-and-processors-forum/54702/gicv3-interrupt-configuration-i-mx-8m-mini---arm-cortex-a53>),
Hardkernel's shop pages for C2/C4 status, and eBay listings for the used prices.

## The port cost, per candidate

What is genuinely new for nife on each board, given rule 1's board boundary and the
VisionFive 2 experience (a board directory, not a diff across the tree). Every board below shares
three items, so they are the baseline rather than rows: an **EL2 to EL1 entry drop** in `boot.s`
(every real bootloader here enters at EL2; QEMU's ELF path enters at EL1, so this code does not
exist yet), a **board memory map** (DRAM base, MMIO windows), and the **bench runbook** work 16a
already modeled. The aarch64 `image_header.s` (Linux Image header) already exists, so U-Boot
`booti` and the Pi's `kernel8.img` load path are covered.

| board | UART work | GIC work | boot handoff | SMP | net new beyond the baseline |
|---|---|---|---|---|---|
| TX1 | none expected: `ns16550` with `reg-shift=2`, board clock for the divisor | none: GICv2 at a new base, driver takes addresses | U-Boot from SD (`fatload` + `go`/`booti`), stock on the dev kit | PSCI from BL31, `smc` | ~nothing but constants and the baseline |
| TX2 | same as TX1 | same | NVIDIA boot chain, more layers | PSCI | heterogeneous clusters to pin around |
| ODROID-C2/C4 | **new meson UART driver** (small, a few hundred lines) | none | U-Boot | PSCI | the UART driver |
| i.MX8MM EVK | **new i.MX UART driver** | **new GICv3 driver** (distributor + redistributors + system-register interface; the largest single item on this table) | U-Boot | PSCI | two drivers, one of them big |
| Pi 4 | none: PL011 (route it to GPIO 14/15, `dtoverlay=disable-bt`) | none | GPU firmware loads `kernel8.img` from FAT32; TF-A `bl31.bin` as armstub for PSCI | PSCI only with TF-A installed | ~nothing but config and the baseline |
| Pi 5 | none (PL011 debug header) | none | as Pi 4, newer chain | as Pi 4 | RP1 southbridge if IO beyond UART is ever wanted |

The honest reading: **the Pi 4 and the TX1 are the two cheap ports**, and they fail in opposite
directions. The Pi 4 is the cheapest port with the weakest seL4 story; the i.MX8MM EVK is the
strongest purchasable seL4 story with the most expensive port.

## The recommendation: a used Jetson TX1 developer kit

The argument, in one paragraph. The TX1 is the silicon under the only published aarch64 seL4
numbers, the very 413-plus-426-cycle pair notes/benchmarks.md already compares against, and that
page was regenerated from CI the day before this survey, so the platform is actively benchmarked,
not historically. Measuring nife on it retires the largest caveat in the benchmarks note ("a
large part of the gap closing is the machine, not the kernel ... the only fix is the same kernel
measured on comparable silicon"): comparable becomes identical, and our own sel4bench run can be
sanity-checked against the Foundation's published figures before we trust it against ourselves.
Meanwhile the port cost is near the floor: GICv2 at a new base, a 16550 our driver already
parameterizes for, the architected timer, DTB boot from stock U-Boot, and PSCI from the upstream
TF-A BL31 the board ships with. The used-market risk is the same one milestone 87 priced
deliberately for the x86 machine: eBay's money-back guarantee bounds "does it work" to return
friction, and the new-market alternative costs $300 more plus two drivers.

If the answer is no: the fallback that keeps the deciding criterion is the **i.MX8MM EVK** at
$388-462 new plus a GICv3 driver and an i.MX UART driver, which converts a $105 purchase into a
multi-week port. The fallback that keeps the price is the **Pi 4**, which converts the milestone
into self-refereed seL4 numbers on a configuration seL4's own CI never exercises.

**Runners-up, with reasons:**

- **i.MX8MM EVK**: the only first-class bench platform purchasable new, and the GICv3 driver it
  forces is work the tree eventually wants anyway (every modern aarch64 SoC is v3). Stays the
  recorded alternative if used TX1s dry up or new-hardware assurance becomes the point.
- **Raspberry Pi 4B**: the best board for every purpose *except this one*: cheapest, best
  documented, our exact driver set, and already notes/target-hardware.md's pick for the Pi-port
  milestone. Buy it when that milestone runs; it does not carry the seL4 comparison.
- **Jetson TX2**: everything the TX1 offers at a higher used price, plus heterogeneous
  Denver-and-A57 clusters that complicate a clean single-core comparison. Buys nothing here.
- **ODROID-C2**: CI-run and GICv2, but discontinued with spotty used supply, and the meson UART is
  a new driver. Dominated by the TX1 on every axis that matters here.
- **ODROID-C4**: purchasable and GICv2, Foundation sel4test hardware, but not in the bench matrix,
  so it shares the Pi 4's disqualification while still costing a UART driver.
- **Raspberry Pi 5**: seL4 platform support exists but the Foundation owns no hardware; weakest
  evidence tier of all while costing Pi-4-class work.

## All-in cost

| item | price |
|---|---|
| Jetson TX1 developer kit, used (eBay, listings 2026-08-14) | ~$80-105 |
| 3.3 V USB-TTL serial cable (J21 header, 115200) | ~$12 |
| microSD card | ~$10 |
| smart plug for remote power cycling (the milestone-87 pattern) | ~$15 |
| **total** | **~$120-145; budget $150-200 for listing variance** |

## Unknowns, to verify at purchase

- **The listing's contents.** Kits vary; the carrier board is required (the J21 serial header and
  the SD slot live on it), and the PSU and antennas are frequently missing. Verify photos.
- **The flashed boot chain.** Which L4T revision is on it decides whether U-Boot comes up at all;
  reflashing an old JetPack needs an old Ubuntu host, a real detour. Prefer a listing that shows a
  boot log, or budget the detour.
- **Entry state from U-Boot** (`go` vs `booti`, EL2 expected, DTB pointer register). The EL2 drop
  is planned work regardless; the specifics are read at the bench, per the VisionFive 2 runbook
  pattern.
- **PSCI visible to a non-Linux payload** on the shipped firmware revision (expected from the TF-A
  tegra210 BL31, verified with a probe boot), and **`PMCCNTR_EL0` readable at EL1** with the
  shipped secure-world settings.
- **Clock pinning.** The published seL4 numbers are at 1.9 GHz; confirm what pins the A57 cluster
  clock during a bare-metal boot (DVFS is normally the OS's job, so likely whatever the boot chain
  left, but confirm rather than assume).
- **sel4bench from SD.** The Foundation's CI boots over its machine-queue rig; our path is SD plus
  U-Boot per seL4's TX1 page (which also warns stock U-Boot lacks TFTP). Confirm the sel4bench
  image runs that way before trusting our numbers against the published ones.
- **A57 errata**: nothing benchmark-relevant recorded in the standard A57 errata list, but check
  the Tegra 210 errata sheet at bring-up rather than trusting this survey's negative.

## BUGS

- The support tiers trust seL4's CI configuration as read on 2026-08-14. The results repo for the
  CI-run tier is private, so "CI-run" for TX2, ODROID-C2 and i.MX8MM is inferred from the workflow
  matrix rather than from seen numbers; only TX1's are public.
- Prices are a day's snapshot of a used market. The TX1 rows will drift; re-check before ordering.
- None of these boards advances the aarch64 IOMMU story (BCM2711 has no SMMU, and Tegra's SMMU is
  NVIDIA's own design, not an SMMUv3), so milestone 16b's aarch64-on-silicon half remains ungated
  by this purchase, whatever is chosen.
- This note records that the kernel drives GICv2 only. If that ever changes, the candidate table's
  i.MX8MM disqualifier weakens and the survey should be re-read before being cited.
