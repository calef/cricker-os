# QEMU

## What it is

QEMU is a computer made of software. It simulates a whole machine: a CPU that fetches
and executes real instructions one at a time, some RAM, and a set of devices.

We need it because **the kernel is not a program that runs on macOS**. It has no OS
underneath it, because it *is* the OS. It expects to be the first thing running on a
machine, to own all of RAM, and to talk to hardware by writing values to specific
physical memory addresses. There is no `main()` for macOS to call, no `malloc`, no files.
You cannot type `./kernel` and have anything sensible happen.

So the kernel needs a computer. QEMU is that computer. It loads the kernel binary into
simulated memory, points the simulated CPU at the first instruction, and lets go. From
the kernel's point of view it has woken up on bare metal and is alone in the universe.
It cannot tell the difference.

## Why not develop on a real Raspberry Pi

Eventually we will (that's a planned milestone). But compare the loops:

| | Real hardware | QEMU |
|---|---|---|
| Iteration | Build, copy to SD card, move card, power-cycle, watch a serial cable | `cargo run`, ~1 second |
| Debugger | Needs special JTAG hardware | Built in (GDB stub) |
| Automated tests | Basically impossible | It's just a process with an exit code |
| Cost of a bug | Reflash the card | Press up-arrow |

## The `virt` machine

`-M virt` tells QEMU which computer to pretend to be. It can imitate many real boards,
including a Raspberry Pi. But `virt` is a machine that **does not physically exist**. The
QEMU developers invented it as a deliberately clean, well-documented, standards-following
ARM board.

Real hardware is full of quirks, undocumented registers, and errata. `virt` has a serial
port at exactly `0x0900_0000`, a standard ARM interrupt controller, and virtio devices,
all at fixed documented addresses. Nothing is weird. It is the machine you would design
if your goal was for a person to be able to learn on it.

That is exactly why we start here and treat the Pi as a later port. The Pi will teach us
what real hardware is like. `virt` lets us learn what a kernel is first, without the two
lessons tangled together.

## The serial port

The kernel's first output device is a serial port: ancient, and beautifully dumb. Write a
byte to a magic memory address and that byte goes out a wire, one bit at a time. No
graphics, no fonts, no buffering. It is the simplest way a computer can say anything at
all, which is why it is the first thing every kernel learns to do.

QEMU wires the simulated serial port straight to your terminal. When the kernel writes a
byte to `0x0900_0000`, a character appears in your shell.

## Flags we use, and why

| Flag | Meaning |
|---|---|
| `-M virt` | Be the fictional clean ARM board (above) |
| `-cpu cortex-a72` | Pretend to be this specific ARM core (the one in a Pi 4) |
| `-nographic` | No emulated display window. Wire the serial port to this terminal |
| `-semihosting` | Let the guest ask the host to do things, e.g. "exit with code 0". This is how our test harness reports pass/fail |
| `-kernel <file>` | Load this ELF and jump to its entry point |
| `-s -S` | Open a GDB stub on port 1234 and freeze the CPU until a debugger attaches |

## Emulation vs. virtualization

QEMU *emulates* by default: it reads each guest instruction and simulates its effect in
software. Slower, but it can pretend to be hardware you don't own, and it can stop the
world for a debugger.

A VM product (Parallels, VMware) *virtualizes*: guest instructions run natively on the
real CPU with hardware assist. Much faster, but the guest must match the host
architecture. QEMU can do this too (via KVM on Linux, HVF on macOS), but we don't need
the speed and we do want the debuggability.

---

*Add to this file as new QEMU concepts come up.*
