# 89. Scaleway EM-RV1: a second RISC-V implementation, rented

**Status: NOT-STARTED.** Raised 2026-08-03 alongside milestone 88, when the cloud-hardware survey
turned up the one thing "cloud has no RISC-V" was wrong about: Scaleway rents real riscv64
silicon (Elastic Metal RV1: T-Head TH1520, four C910 cores, 16 GB, 128 GB eMMC) at EUR 0.042 an
hour or EUR 15.99 a month.

**Gate: HARDWARE, MILESTONE 16.** A rented Elastic Metal RV1, which needs calef's account before
anything runs, and the block sequences it after the VisionFive 2's first results because a second
data point is only interpretable once there is a first. The feasibility probe, whether it boots a
custom kernel at all, is one EUR 0.042 hour and the block says that part is reasonable any time.

**What a second implementation is for.** The cpu matrix's BUGS note records the questions QEMU
structurally cannot answer: no `-cpu` value varies the `satp.ASID` width, so
`the_hardware_has_at_least_the_asid_bits_the_allocator_assumes` has never run its failing branch,
and `thead-c906`'s non-standard page-table attribute bits (`xtheadmae`) are advertised by the
model but never exercised. The TH1520's C910 cores are that vendor family on real silicon. One
board answers these questions once; a second implementation tells us whether the answer was the
architecture's or that board's. This is the same reasoning as the cpu matrix itself, one rung up:
from "five QEMU models" to "two vendors' silicon".

**What is honestly unknown, and it is the first stage:** whether EM-RV1 boots a custom kernel at
all. It is a Scaleway Labs product with a curated image list; the boot mechanism (U-Boot? UEFI?
iPXE?), serial console access, and any custom-image path are facts to establish with one
EUR 0.042 hour before anything else is planned. If the answer is "Linux images only", this
milestone closes as RECORDED with that finding, and the outcome is worth exactly one hour.

## Scope note

Sequenced after the VisionFive 2's first results on purpose: the board is bought, arrives
~2026-08-21, and answers the U74's questions directly; this milestone's value is the *second*
data point, which only becomes interpretable once there is a first. The hourly price makes the
feasibility probe (stage one) reasonable any time; the rest waits. Nothing here regresses QEMU or
the board path, same parity rule as everything else (§19).
