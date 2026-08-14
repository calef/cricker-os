# The VisionFive 2: first silicon

Milestone 16a's board facts and bench runbook. Everything here was established from documentation
before the board was ever powered on, and every fact names its source. What documentation could not
establish is in "To measure at the bench" at the end, deliberately, rather than guessed. The board
arrived 2026-08-14.

The one-sentence summary: **the JH7110 is startlingly close to QEMU's `virt` machine** (UART base,
PLIC base, CLINT base, OpenSBI, SBI HSM, Sv39 all match), and the differences that remain are
exactly four: where DRAM starts, how the UART's registers are strided and clocked, how the PLIC
numbers its contexts, and the monitor core that must not be started.

Sources cited throughout:

- **[QSG]** StarFive, VisionFive 2 Single Board Computer Quick Start Guide,
  https://doc-en.rvspace.org/VisionFive2/PDF/VisionFive2_QSG.pdf
- **[dtsi]** Linux, `arch/riscv/boot/dts/starfive/jh7110.dtsi` (SoC) and `jh7110-common.dtsi`
  (board), mainline as of 2026-08-14
- **[uboot-doc]** U-Boot, `doc/board/starfive/jh7110_common.rst` and `visionfive2.rst`
- **[uboot-img]** U-Boot, `arch/riscv/lib/image.c` (mainline; verified identical logic in
  StarFive's `JH7110_VisionFive2_devel` vendor branch)
- **[uboot-bootm]** U-Boot, `arch/riscv/lib/bootm.c`
- **[uboot-pxe]** U-Boot, `boot/pxe_utils.c`
- **[uboot-cfg]** U-Boot, `include/configs/starfive-visionfive2.h`
- **[linux-hdr]** Linux, `Documentation/arch/riscv/boot-image-header.rst`,
  `arch/riscv/include/asm/image.h`, `arch/riscv/kernel/head.S`

## The boot chain

Four stages live in the board's SPI flash and run before any byte of ours [uboot-doc]:

1. **BootROM** (on-die, 32 KB at 0x2A00_0000) reads the boot-mode pins and picks the media.
2. **U-Boot SPL** (flash offset 0x0) runs from SRAM at 0x0800_0000, initializes DRAM and PLLs.
3. **OpenSBI** (`fw_dynamic`, inside `u-boot.itb` at flash offset 0x100000) takes M-mode and stays
   resident as the SBI.
4. **U-Boot proper** runs in S-mode at 0x4020_0000 and loads the payload from microSD or TFTP.

So the contract our kernel meets on the board is the one it already speaks on QEMU `virt`: entered
in S-mode with OpenSBI behind the SBI calls, `a0` = boot hart id, `a1` = device-tree pointer.
U-Boot's jump is literally `kernel(gd->arch.boot_hart, images->ft_addr)` [uboot-bootm], so the
OpenSBI register contract survives U-Boot unchanged.

**One difference in who the boot hart is**: on QEMU `virt` every hart is identical and OpenSBI's
lottery picks any of 0..3. On the JH7110 hart 0 is the S7 monitor core (see "Harts" below), so the
boot hart will be one of the U74s, harts 1..4.

## The Image header, and the load-address trick

U-Boot's `booti` refuses a payload that does not carry the RISC-V Linux Image header: a 64-byte
prelude whose one checked field is the u32 magic 0x05435352 ("RSC\x05") at offset 0x38 [uboot-img].
The header format is Linux's [linux-hdr]; `kernel/src/arch/riscv64/boot.s` now emits it (milestone
16a), and QEMU never reads it (the ELF goes in via `-kernel`), so it is 64 dead bytes there.

`booti` then **relocates the image to `ram_base + text_offset`** whenever the loaded file sits in
RAM, which it always does [uboot-img]:

```c
if (force_reloc ||
   (gd->ram_base <= image && image < gd->ram_base + gd->ram_size)) {
    *relocated_addr = gd->ram_base + lhdr->text_offset;
}
```

That line is why the kernel needs **no board relink**. The kernel is linked for physical
0x8020_0000 (`link-riscv64.ld`), which on QEMU `virt` is DRAM base + 2 MiB. The VF2's DRAM starts
at 0x4000_0000 [dtsi], so our header states `text_offset = 0x40200000` and `booti` moves the image
to 0x4000_0000 + 0x4020_0000 = 0x8020_0000, the linked address, which is comfortably inside DRAM on
every VF2 variant (even the 2 GB board's RAM runs to 0xC000_0000).

**This is an exception and a foot gun, on the record.** Linux uses `text_offset = 0x200000` ("2 MiB
into RAM, wherever RAM is"); ours means "0x8020_0000 absolute, on any board whose RAM starts at
0x4000_0000". A future board with a different DRAM base gets the wrong address from this header.
The alternatives, if that day comes: a board-specific link (PHYS_START, plus the boot page table's
gigapage index in `arch/riscv64/mmu.rs`, plus this header value), or teaching `boot.s` to run at an
arbitrary 2 MiB-aligned load address the way Linux does. Both were deliberately not built for one
board that does not need them.

## DRAM

| | QEMU `virt` | VisionFive 2 |
|---|---|---|
| DRAM base | 0x8000_0000 | 0x4000_0000 [dtsi] |
| Kernel runs at | 0x8020_0000 | 0x8020_0000 (via the header, above) |
| Size | whatever `-m` says | 2/4/8 GB by variant; the 4 GB board's node is `reg = <0x0 0x40000000 0x1 0x0>` [dtsi] |

Consequences the kernel already handles: RAM extent comes from the DTB `/memory` node
(`kernel/src/memory.rs`), not from a constant, so the base difference is discovered rather than
assumed. And, since 2026-08-14, the boot page table maps **gigapage 1 as well** (0x4000_0000..
0x8000_0000, `arch/riscv64/mmu.rs`), so a DTB at U-Boot's default `fdt_addr_r` = 0x4600_0000
[uboot-cfg] is readable before the fine tables exist; it used to fault there before the trap path
could print. Still out of reach: `$fdtcontroladdr` (the control DTB) near the top of RAM, above
gigapage 2 on every variant and above 4 GiB on an 8 GB board. The runbook below moves the DTB to
0x8600_0000 (inside gigapage 2) for exactly that case, and keeping the `fdt move` in the manual
first boot also removes one variable from a first bring-up.

An 8 GB board's RAM also spans past 4 GiB (0x4000_0000 + 8 GiB = 0x2_4000_0000), and the JH7110
additionally aliases DRAM uncached at 0x24_0000_0000 [uboot-doc]; the alias appears in no `/memory`
node and needs nothing from us.

## The UART

Same base address as QEMU `virt`, different silicon behind it. The JH7110's UART0 is a Synopsys
DesignWare DW_apb_uart, an 8250 derivative [dtsi]:

| | QEMU `virt` NS16550 | JH7110 UART0 |
|---|---|---|
| compatible | `ns16550a` | `starfive,jh7110-uart`, `snps,dw-apb-uart` [dtsi] |
| base | 0x1000_0000 | 0x1000_0000, size 0x10000 [dtsi] |
| reg-shift | 0 (byte registers, consecutive) | **2** (registers 4 bytes apart) [dtsi] |
| reg-io-width | 1 | **4** (32-bit accesses) [dtsi] |
| clock | 3.6864 MHz (QEMU ignores the divisor anyway) | **24 MHz** [uboot-cfg] |
| PLIC irq | 10 | **32** [dtsi] |

What `drivers/ns16550.rs` grew on 2026-08-14, **built and QEMU-proven; the JH7110 side of each is
still a bench question**, because QEMU emulates none of this silicon:

1. **A register stride and access width, carried as data.** The driver's `Shape` holds
   `reg-shift` and `reg-io-width`, defaulting to QEMU's byte wiring. On the board LSR lives at
   byte offset 0x14, not 5, and the old byte access at offset 5 read the middle of the IER word,
   so the THRE poll span on garbage.
2. **The divisor from the stated clock, and only from a stated clock.**
   `console::configure_from_dtb` programs `clock-frequency / (16 x 115200)` rounded (24 MHz gives
   13, actual rate 115385, 0.16% high; the two expected divisors are proved at compile time in the
   driver) and **leaves the divisor and line controls alone when the tree states no clock**.
   Mainline JH7110 trees express the UART clock as a `clocks` phandle this kernel does not
   resolve, and U-Boot has already programmed 115200 8N1 on any board that showed a prompt, so
   not touching it is correct there too; a divisor guessed against the wrong clock is 1.5 Mbaud
   garbage at the far terminal, which is the failure this rule exists to avoid. QEMU's tree states
   3.6864 MHz, so the suite now programs divisor 2 where it used to write a constant 1; QEMU
   ignores both.
3. **The DW busy quirk**, keyed on the `snps,dw-apb-uart` compatible: a DW_apb_uart ignores an LCR
   write while busy and latches a "busy" interrupt, so `init` drains the transmitter (LSR.TEMT,
   bounded) before touching LCR.
4. **The shape is adopted before the first `println!`.** `kernel_main` calls
   `console::configure_from_dtb(dtb)` immediately after `console::init`, so no output is ever
   produced with a stale stride. The node is matched by its name, `serial@10000000`, pinned beside
   the equally hardcoded base address; the jh7110 fixture test
   (crates/isa/tests/riscv64_jh7110.rs) is the witness for both.

With that built, the honest first-boot expectation moves up one rung: **the banner should
appear**, provided the DTB U-Boot hands us is readable (see DRAM above) and the silicon matches
the dtsi's description. The triage ladder below still covers every way that can fail.

## Harts, the PLIC, and the CLINT

**Five harts, one of which must not be started.** The JH7110 is 1x SiFive S7 (hart 0) + 4x U74
(harts 1..4). The S7 is `rv64imac_zba_zbb`, has **no MMU** and no S-mode, and its cpu node says
`status = "disabled"` [dtsi]. The U74s are `rv64imafdc_zba_zbb`, `mmu-type = "riscv,sv39"` [dtsi],
exactly the kernel's contract.

**Correction (2026-08-14): the roster half of this was already built when this note first claimed
otherwise.** `CpuList` has read `status` since milestone 100, and `smp::bring_up_secondaries`
refuses a disabled hart by name rather than starting it; what `sbi_hart_start` would answer for
hart 0 stays on the bench list only to confirm the refusal is the right call. What was genuinely
missing, and was built 2026-08-14: the **ISA record** (`isa::riscv64`) counted disabled harts, so
the S7's `rv64imac` narrowed the machine's common extensions, and an S7 whose tree spells its MMU
as `riscv,none` would have read as a machine that cannot run us at all. A disabled hart now
contributes nothing to the record, host-proven against the hand-written jh7110 fixture
(crates/isa/tests/riscv64_jh7110.rs). One roster limitation remains, recorded in the BUGS below:
`cpu::MAX_CPUS` is 4 and this SoC describes five harts.

**Second bench stop (2026-08-14): the vendor tree lies about the S7, twice, and the fix above
never fires on the real board.** Everything in this note cited from [dtsi] describes mainline; the
tree the flashed firmware actually hands over was read at the U-Boot prompt (`fdt print`) and says
something else. Measured: **all five cpu nodes carry `status = "okay"`**, and cpu@0 carries
`riscv,isa = "rv64imacu"` with `mmu-type = "riscv,sv39"`. So the vendor tree marks the S7 okay and
claims it has an Sv39 MMU, and both are false: the S7 has no MMU and no S-mode. With `status`
telling that lie, hart 0 came up startable, the kernel handed it to `sbi hart_start`, and **vendor
OpenSBI died on it**: an M-mode load access fault at OpenSBI's own scratch area, `mepc` inside
OpenSBI, reported for hart 0, immediately after our bring-up call. If a boot ends in an OpenSBI
trap dump whose `mepc` is in firmware and whose hart is 0, this is what it looks like; the kernel
code that caused it is a `hart_start` the roster should never have issued.

The one truthful property on that node is the ISA string itself, and it answers by omission:
`rv64imacu` is the old spelling that lists **privilege letters** in the single-letter run, and it
spells `u` (user) without `s` (supervisor). The U74s beside it say `rv64imafdcbsux`, four single
letters `b s u x` at the tail, `s` present. A hart without S-mode cannot run this kernel whatever
the rest of its node claims, so since 2026-08-14 **startability requires supervisor mode, read
from the hart's own `riscv,isa`** (`isa::riscv64::supervisor_mode_claim`, enforced in
`smp::read_cpu_list`), and such a hart is likewise kept out of the machine record's intersection
and `mmu` (`isa::riscv64`). The boot line names the exclusion in the machine's own terms: "cpu 0's
riscv,isa names user mode and not supervisor".

The rule needs a witness, and this is the part worth remembering before generalizing it: **a
missing `s` alone proves nothing.** Modern ISA strings spell no privilege letters at all (Linux
rejects them; QEMU dropped `s`/`u` in 5.1), so QEMU `virt` today says `rv64imafdch_...` and the
mainline jh7110 dtsi says `rv64imac_zba_zbb`/`rv64imafdc_zba_zbb`, silent about privilege on
machines that have S-mode. Absence of `s` is a denial only when a bare `u` in the same
single-letter run proves the writer was spelling privilege modes; otherwise it is silence, and
silence is not evidence of absence. Multi-letter `_s`-prefixed extensions (`_sstc`, `_svadu`,
QEMU's own string is full of them) never count: only the run before the first `_` is scanned.
Host-proven on both generations of spelling and both JH7110 trees
(crates/isa/tests/riscv64_isa_strings.rs, riscv64_jh7110.rs, riscv64_jh7110_vendor.rs): the
mainline fixture's S7 is excluded by `status`, the vendor fixture's by its ISA string, and the
same conclusion arrives through the two trees' different lies.

**Third bench stop (2026-08-14): the online set is {1,2,3}, and the kernel indexed it as
{0,1,2}.** With the S7 refused, the machine's online cpus are harts 1..3 (hart 4 is past
`MAX_CPUS`, see BUGS), the first time this kernel ever ran with a set not contiguous from zero;
on QEMU `virt` the set is always {0..n-1}, so every `0..online_count()` loop and every
`rng % online_count()` pick had been right by coincidence. On the board, spawn placement's
modulo-count produced index 0 and placed `init` into parked slot 0's inbox, which nothing drains,
ever. It took three boots to pin: the placement is randomized, so roughly one placement in three
landed dead, and the symptoms disagreed with each other (outlaw's 3-then-0 syscall counts one
boot, `init` hanging outright the next two). The thread-dump diagnostic added for it (commit
`3833422`, watching the threads while the demo waits) showed the shape in one line: a parked
core's inbox holding a runnable thread. Fixed on the critical path in commit `1329874`
(`smp::online_cpus()` / `nth_online`, placement and steal converted), and the remaining
count-as-index sites, wake targeting, both ISAs' IRQ-affinity round-robins, the hang watchdog's
liveness scan and the suite's own per-core loops, were swept in the follow-up branch, with the
{1,2,3} shape host-proven in `crates/cpu_set` since QEMU cannot boot it.

**The PLIC is at QEMU's address with a different context map.** `sifive,plic-1.0.0` at 0xC00_0000,
136 sources [dtsi]. On QEMU `virt` every hart has an M and an S context and hart h's S context is
`2h + 1`, which is the formula `kernel/src/smp.rs` uses. On the JH7110 the disabled S7 contributes
only an M context, so the layout per the dtsi's `interrupts-extended`
(`<&cpu0_intc 11>, <&cpu1_intc 11>, <&cpu1_intc 9>, <&cpu2_intc 11>, <&cpu2_intc 9>, ...`) is:
context 0 = hart 0 M, then for U74 hart h in 1..4, context `2h - 1` = M and context `2h` = S.
**Hart h's S context is `2h` on this board, not `2h + 1`.**

Built 2026-08-14: the mapping comes from the DTB now. `isa::plic::PlicContexts` decodes
`interrupts-extended` (entry k is context k; interrupt 9 marks an S context; phandles resolve to
harts through each cpu's `riscv,cpu-intc` child), `arch::irq::init_contexts` records it at boot,
and the `2h + 1` formula survives only as the fallback for a tree that does not state the layout.
The PLIC node itself is found by its `sifive,plic-1.0.0` compatible rather than by name, because
the JH7110 spells the node `interrupt-controller@c000000` where QEMU says `plic@c000000`, and the
old `plic@` name-prefix read found nothing there. QEMU-proven in both directions: the kernel suite
asserts the live `virt` tree reproduces `2h + 1`, and the host fixtures hold the JH7110's `2h`
answer with no S context for hart 0 (crates/isa/tests/riscv64_plic_contexts.rs). What QEMU cannot
prove, the real PLIC honoring context `2h`, is a bench fact like everything else here.

**The CLINT is at QEMU's address.** `starfive,jh7110-clint` at 0x200_0000 [dtsi]; timer and IPI go
through SBI anyway, so this is OpenSBI's problem, not ours.

**Timebase is 4 MHz** (`/cpus/timebase-frequency` [dtsi]), against QEMU `virt`'s 10 MHz. Already
handled: `arch/riscv64/timer.rs` reads the rate from the DTB and panics rather than assumes.

## PCIe

The JH7110's PCIe is a PLDA XpressRICH controller (`starfive,jh7110-pcie` in mainline trees).
That is not the `pci-host-ecam-generic` device QEMU's `virt` boards expose, and it has no driver
here; driving it is its own milestone, not a bench fix.

Since 2026-08-14 the kernel's PCIe windows (the ECAM config space and the 32-bit memory window
BARs are placed in) come from the device tree: `memory::init` reads the generic-ECAM node's
`reg` and `ranges`, `mmu::map_everything` maps the windows only when the node exists, and every
probe in kernel/src/pci.rs reports nobody home when it does not (notes/pcie.md). On this board
there is no such node, so nothing PCIe is mapped or touched, which is the honest statement of
where the PLDA controller stands.

Before that the windows were QEMU constants, and the first bench boot paid for it: the BAR
constant 0x4000_0000 is this board's DRAM base (see the DRAM table above), so `map_everything`
tried to lay a device mapping over memory step 1 had already direct-mapped, and the mapper's
overwrite refusal panicked the boot right after the banner. The first DECISIONS §43 casualty
proven on silicon rather than predicted.

## SBI extensions

OpenSBI is the vendor firmware's M-mode resident, so TIME, IPI, RFENCE and **HSM** (the bring-up
path `arch::psci_cpu_on` uses) are the standard set, and SRST (system reset) is how the board can
reboot or power off from S-mode. Which OpenSBI version is in the shipped flash, and whether its
**PMU** extension is present and how many of the U74's hpmcounters it exposes, is deliberately on
the bench list: the version banner prints on every boot and `sbi probe` answers the rest, and
guessing a counter count here would be exactly the manufactured fact this note exists to avoid.

## "The test suite where semihosting allows", concretely

There is no semihosting on this board. The riscv test exit (`arch/riscv64/semihosting.rs`) is not
semihosting at all but QEMU `virt`'s `sifive_test` finisher, an MMIO word at physical 0x10_0000
that tells **QEMU** to exit with a status. The JH7110 has no such device; a store to 0x10_0000
there is a bus error at best. So the kernel's test build, as it stands, cannot report pass/fail on
silicon.

The proposal, recorded now and deliberately not built until the bench says it is needed: a **UART
pass/fail marker** (a fixed final line, `CRICKER-TEST-EXIT: PASS` or `FAIL <code>`, that a harness
on the serial line greps for) followed by **SBI SRST shutdown** so the run terminates. Both halves
are a dozen lines against interfaces the kernel already has. The `sifive_test` path stays for QEMU,
selected the same way the finisher address already is.

## Boot-mode switches

Two DIP switches (RGPIO_1, RGPIO_0) select the boot media, read once at power-on [QSG]:

| RGPIO_1 | RGPIO_0 | Mode |
|---|---|---|
| 0 (L) | 0 (L) | 1-bit QSPI NOR flash (the vendor firmware; **use this**) |
| 0 (L) | 1 (H) | SDIO 3.0 (SD card holds the firmware too) |
| 1 (H) | 0 (L) | eMMC |
| 1 (H) | 1 (H) | UART recovery (XMODEM loader) |

QSPI is both the factory arrangement and StarFive's recommendation (the QSG notes SD/eMMC boot
fails on some cards) [QSG]. It is also what we want: the flash's SPL + OpenSBI + U-Boot chain
stays untouched, and our payload rides a microSD card that U-Boot merely reads files from. UART
recovery (1:1) is the unbrickable fallback if flash is ever corrupted [uboot-doc].

## Serial wiring

The debug console is UART0 on the 40-pin header, **3.3 V TTL** (the pins tolerate nothing higher)
[QSG]:

| Header pin | Signal | Connect to USB-serial |
|---|---|---|
| 6 | GND | GND |
| 8 | UART0 TX (GPIO 5 [dtsi]) | RX |
| 10 | UART0 RX (GPIO 6 [dtsi]) | TX |

115200 8N1, no flow control [QSG]. On macOS:
`screen /dev/cu.usbserial-* 115200`. Cross TX to RX; leave the adapter's VCC pin unconnected (the
board has its own power).

## The microSD payload

`script/board-image` (name provisional) builds it: the flat `Image`-format kernel
(`llvm-objcopy -O binary`, header at offset 0), an `extlinux/extlinux.conf`, and printed
instructions. It writes files and prints the copy commands; it runs nothing destructive itself.

The card layout U-Boot's distro boot wants [uboot-doc]: one FAT32 partition holding
`/extlinux/extlinux.conf` and the kernel image (MBR or GPT both work; the special GPT partition
GUIDs in [uboot-doc] matter only when the card holds the firmware itself, and ours stays in QSPI
flash). U-Boot scans each partition for
`/extlinux/extlinux.conf` and `/boot/extlinux/extlinux.conf`, loads the `kernel` file to
`kernel_addr_r` = 0x4020_0000 [uboot-cfg], and runs `booti`.

**How the DTB gets passed**: with no `fdt`/`fdtdir` line in the label, U-Boot falls back through
`fdt_addr_r`, then `fdt_addr`, then `$fdtcontroladdr` (its own control DTB) [uboot-pxe]. The
control DTB describes the board correctly, but both fallback addresses land outside the kernel's
boot page table (see DRAM above), so the extlinux path is the second step, after a manual first
boot proves the kernel. The manual path controls the DTB address exactly:

```
StarFive # load mmc 1:1 ${kernel_addr_r} /cricker-vf2.img
StarFive # fdt addr ${fdtcontroladdr}
StarFive # fdt move ${fdtcontroladdr} 0x86000000
StarFive # booti ${kernel_addr_r} - 0x86000000
```

0x8600_0000 is inside boot gigapage 2 and clear of the image (which ends well below 0x8100_0000)
and of `kernel_comp_addr_r` = 0x8800_0000 [uboot-cfg].

**TFTP alternative** (not built): cordoba is the designated PXE/TFTP host (milestone 87's scope
note). The board side is just `dhcp; tftpboot ${kernel_addr_r} cricker-vf2.img` followed by the
same `fdt move` + `booti`; the cordoba side (dnsmasq or tftpd serving the image) is its own small
piece of work and turns the flash-a-card loop into a rebuild-and-reset loop. Worth building the
moment the card loop gets annoying, which history says is the second bench session.

## The bench runbook

Setup, in order:

1. microSD: run `script/board-image`, follow its printed commands to partition and copy, insert
   the card.
2. DIP switches to QSPI: RGPIO_1 = 0 (L), RGPIO_0 = 0 (L) [QSG].
3. Serial: pins 6/8/10 as wired above, 115200 8N1, terminal attached **before** power so the SPL
   banner is not missed.
4. Power: USB-C. The board boots on power, there is no power button.

What appears, in order, on a good day: the SPL banner, OpenSBI's banner (version line included:
record it), U-Boot's banner and countdown, then either the extlinux menu or the `StarFive #`
prompt for the manual commands, then `## Flattened Device Tree`/`Starting kernel ...`, then ours:
a blank line and

```
cricker-os on RISC-V (rv64, S-mode, Sv39)
```

(`kernel/src/main.rs`; the console comes up before the DTB is touched, so this line precedes any
memory-map work). **Honest first-boot expectation: this line does not appear until the UART driver
learns the DW-8250 differences above.** The realistic first target is `booti` relocating and
jumping without complaint; the banner is the second target, after the driver work.

### The failure-triage ladder

| Symptom | Most likely cause, in order |
|---|---|
| Nothing on serial at all | TX/RX not crossed; wrong device (`cu.*` vs `tty.*`); DIP switches not on QSPI; a bad SPI flash (fall back to UART recovery mode [uboot-doc]) |
| Firmware banners but garbage | Baud mismatch in the terminal (must be 115200); a 5 V adapter on 3.3 V pins has by then possibly cost a board |
| U-Boot fine, `Bad Linux RISCV Image magic!` | The file is the ELF, not the objcopy output; `script/board-image` verifies the magic at offset 0x38 at build time, so a stale card is the other suspect |
| `Starting kernel ...` then silence | Expected until the UART driver handles reg-shift/io-width: the kernel may be running and polling LSR at the wrong offset. Also: DTB left at `$fdtcontroladdr`/`fdt_addr_r` (outside the boot map, faults with the trap path not yet printing); or the relocation did not happen (check U-Boot printed `Moving Image from ... to 0x80200000`) |
| `Starting kernel ...` then garbage | Kernel is alive and the divisor is wrong: driver reprogrammed the divisor against the wrong clock (needs 13 at 24 MHz, not 1) |
| Banner, then hang or trap dump | DTB parsing or the memory map: RAM at 0x4000_0000 exercises paths QEMU never did (bitmap placement, gigapage 1 unmapped, the S7's cpu node in `smp::init`, the PLIC context formula) |
| Banner, then an **OpenSBI** trap dump (`mepc` in firmware, hart 0) | The kernel started the S7: the vendor tree's `status`/`mmu-type` lies got past the roster. The supervisor rule ("Second bench stop" above) exists to refuse hart 0; if this dump is back, that gate has regressed or the tree found a third lie |

## To measure at the bench

Facts documentation could not settle, each an explicit measurement, none guessed above:

1. **OpenSBI version in the shipped flash** (banner), and which SBI extensions `sbi probe` reports;
   specifically whether PMU is present and how many hpmcounters it exposes on the U74s.
2. **What `sbi_hart_start` returns for hart 0** (the disabled S7): error, or a start that must
   never be requested. **Measured 2026-08-14: the worse answer.** Vendor OpenSBI does not refuse
   it; it starts the S-incapable core and dies in its own trap handler (see "Second bench stop"
   above). The refusal has to be ours, and now is.
3. **The vendor U-Boot's actual environment**: whether its distro boot scans our single-partition
   card (vendor firmware predates some mainline conventions), and the values of `kernel_addr_r`
   and `fdt_addr_r` in the flashed environment (`printenv`), documented above from mainline
   [uboot-cfg].
4. **Whether `booti` in the vendor build relocates as mainline does** (the `Moving Image` line);
   the source says yes [uboot-img], the flash is whatever was built from it.
5. **The boot hart id** OpenSBI hands us (`a0`), and whether `smp.rs`'s hwid-vs-index assumptions
   hold with hart ids 1..4.
6. **DRAM size of this specific board** (the memory node U-Boot patches in), and whether the
   `/memory` walk and bitmap placement behave with RAM at 0x4000_0000.
7. **UART reality check**: that byte-wide access at unshifted offsets truly fails (predicted, not
   yet observed), and the DW busy quirk's visibility.
8. **Boot-to-banner wall time**, once there is a banner, as the first real-hardware number.

## BUGS

The kernel first ran on this board on 2026-08-14 and got through its banner (DW-8250 console,
DTB parse, paging, traps, timer, frame allocator) before panicking on the QEMU PCIe constants;
the PCIe section above records that failure and its fix. The second boot that day carried the
PCIe fix, got through fine paging and ISA discovery, and died starting hart 0 (the "Second
bench stop" above): the vendor tree's `status` lie walked the S7 past the disabled-hart
handling, and vendor OpenSBI crashed rather than refuse the start. The boots after that carried
the supervisor rule and the PLIC context map through, and hit the third stop: the first
non-contiguous online set this kernel ever ran on exposed every count-as-index cpu loop at once,
starting with spawn placement putting `init` into parked slot 0's inbox (the "Third bench stop"
above; three boots to diagnose, fixed in `1329874` and swept after). The supervisor rule and the
context map are QEMU- and host-proven and board-proven as far as those boots reached; the
online-set sweep's {1,2,3} shape is host-proven in `crates/cpu_set`; whether the fixed placement
carries the board through the demo is the next bench boot's fact, like everything QEMU cannot
prove, which is what "To measure at the bench" is for.

Two limitations found while building those, honestly not fixed here:

- **The shell path's userspace input driver still speaks QEMU's UART layout.**
  `user/src/input.rs` reads the NS16550 at byte-stride offsets (LSR at 0x05), so on the board the
  kernel console will print but the interactive shell's input path reads garbage until that driver
  learns the same shape the kernel driver did. Not on the first-boot path (the tour and test
  builds take no input); bites at the shell milestone.
- **`cpu::MAX_CPUS` is 4 and this SoC describes five harts** (one unusable), so U74 hart 4 falls
  off the end of the roster and stays parked, silently. The fix is a `MAX_CPUS` bump or a roster
  that skips unstartable harts, and both are entangled with the logical-id-equals-hardware-id
  assumption recorded in `smp.rs`'s BUGS. On the vendor tree a bench boot will print "5 core(s)
  in the device tree, 3 startable" (the four roster slots hold the S7 and three U74s; the S7 is
  excluded by the supervisor rule) plus the cpu 0 exclusion line, and bring up two secondaries
  beside the boot hart, which is correct arithmetic and one hart short of the machine.

The `text_offset` in the Image header encodes one board's DRAM base; the header comment in
`boot.s` and the section above carry the caveat.

Everything cited from "mainline" (Linux dtsi, U-Boot doc and source) describes current upstream;
the flash on the board runs StarFive's vendor fork of unknown vintage. The relocation logic was
verified in the vendor branch too, the environment defaults were not, which is why they sit on the
bench list.
