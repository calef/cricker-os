# Where cricker-os could actually run

## The ISA is almost never the constraint

"Does it run aarch64" is the wrong question. Three things decide whether you can boot your
own kernel on a device:

1. **Can you get code to execute at boot?** Unlocked bootloader, or no secure boot at all.
2. **Are the peripherals documented?** You need a UART, an interrupt controller, a timer,
   and eventually storage. The CPU is standardized. The stuff bolted around it is not, and
   that's where the work is.
3. **Can you physically reach a serial console?** Without one you are debugging a black box.

A device can be aarch64 and still be completely useless to us by failing any of those.

## Trap: "ARM" is not "aarch64"

**Cortex-M** microcontrollers (STM32, most Arduino-adjacent parts) are 32-bit and have **no
MMU**. They cannot run the OS we are building. Ever. No virtual addresses, no isolation, no
user mode as we mean it. They can run an RTOS; that is a different thing.

We need **Cortex-A53 or newer, in 64-bit mode**. Same reason the RISC-V hardware we
considered had to be JH7110-class or better (see [mmu.md](mmu.md)).

## The realistic targets

| Device | Boot access | Peripheral docs | Verdict |
|---|---|---|---|
| **Raspberry Pi 4** (~$60) | Wide open. GPU firmware loads `kernel8.img` off a FAT32 SD card. No signing, no lock. | Excellent, plus the largest bare-metal community anywhere | **The next port.** Serial is a $10 USB-TTL cable on GPIO 14/15. |
| **Raspberry Pi 5** | Same | Worse. I/O routes through the RP1 southbridge over PCIe, less documented, less trodden for bare metal | Doable, but Pi 4 is the safer first port |
| **Rockchip / Allwinner SBCs** (Orange Pi, Radxa Rock, Pine64) | U-Boot from SD | Decent TRMs, much thinner community | Fine. Similar difficulty, less help when stuck |
| **NVIDIA Jetson** | Possible | Good docs, but the TegraBoot chain is genuinely complicated | More work than it's worth as a first port |
| **AWS Graviton bare-metal EC2** (`c7g.metal`) | UEFI. Rent by the hour; no hardware to buy or brick | SBSA-standard server ARM | **Interesting for a specific reason.** See below. |
| **Ampere Altra** workstations / dev kits | UEFI | Standards-compliant | Same category, but you'd own it |
| **Android phones** with unlockable bootloaders (Pixel, Fairphone) | fastboot | Poor. Qualcomm peripherals are barely documented, and you fight TrustZone | Painful. Possible, rarely rewarding. |
| **iPhone, iPad, Apple TV** | Locked, signed | — | No |

## The wild one: an Apple Silicon Mac

This is real, and it is not a jailbreak.

**Apple deliberately permits booting non-Apple kernels on Apple Silicon.** There is a
documented "permissive security" mode, and Asahi Linux is built entirely on it. Their
bootloader, **m1n1**, runs as an Apple-signed payload, then loads an arbitrary kernel image.
It also gives you a **serial console over USB-C** and a hypervisor mode you can use to trace
what macOS itself does to the hardware.

So an M-series Mac is genuinely, legitimately bootable with our own OS.

The catch is brutal: **Apple documents none of the peripherals.** Asahi reverse-engineered
the interrupt controller, the UART, the display, and everything else over several years.
We'd be leaning entirely on their documentation and would be a long way off the beaten path.

Filed as: not the second port. Possibly the fifth. A genuinely impressive one.

## The reframe worth taking seriously

Go back to the Alpha lesson in [portability.md](portability.md): **the second port should be
as alien as possible, because that is what forces hidden assumptions into the open.** Porting
to something *similar* teaches you very little.

Now look at what the Pi actually is:

| | Hardware discovery |
|---|---|
| QEMU `virt` | **Device Tree** |
| Raspberry Pi | **Device Tree** |
| Graviton / Ampere (UEFI + ACPI) | **ACPI tables + PCIe enumeration** |

The Pi is different peripherals inside the *same worldview*. Valuable, and it will shake out
real bugs, but it is a port within one model.

A **UEFI + ACPI server ARM machine** is a genuinely different world: a different firmware
handoff, and hardware discovered by walking ACPI tables and enumerating PCIe rather than
reading a flattened tree. *That* is the port that finds our hidden assumptions, and it is
where the `arch/` boundary either holds up or gets exposed as fiction.

Graviton bare metal costs a few dollars an hour, with no hardware to buy or brick.

## The plan

1. **Raspberry Pi 4 is the next port.** Cheap, open, enormous community, and it delivers the
   "I ran my OS on a computer I can hold" moment, which is worth more motivationally than it
   sounds. It teaches us what real hardware quirks feel like.
2. **Then a UEFI/ACPI target**, precisely because it is alien. This is the one that tests
   whether the hardware abstraction boundary is real.
3. **Apple Silicon as the trophy.** Hardest, most impressive, and we already own the machine.

---

*Add to this file as new targets come up.*
