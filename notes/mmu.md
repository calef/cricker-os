# The MMU (Memory Management Unit)

The piece of hardware that makes an operating system possible at all.

## The problem

Without an MMU, `load x0, [0x40001000]` reads physical byte `0x40001000` from the RAM
chip. Directly. Every program can read and write every byte of memory, including the
kernel's and every other program's. There is no such thing as a "process" in any
meaningful sense, because there is no wall between them.

## What it does

The MMU is hardware between the CPU and the memory bus. When it's on, every address the
CPU emits is a **virtual address**, and the MMU translates it to a **physical address**
before it reaches RAM.

Translation is driven by **page tables**: a tree the *kernel* builds in RAM and hands to
the hardware by writing its physical address into `TTBR0_EL1` / `TTBR1_EL1`. The kernel
writes the rules; the hardware then enforces them on every memory access, at full speed,
forever.

Three things fall out, and they are the foundation of modern computing:

**Isolation.** Each process gets its own page tables. Process A's virtual `0x1000` and
process B's virtual `0x1000` map to *different physical RAM*. Neither can name the
other's memory. Not "isn't allowed to." *Cannot express the address.*

**Protection.** Each mapping carries permission bits: readable, writable, executable.
Map code read-execute and data read-write, and a buffer overflow can no longer turn
injected data into running code.

**Illusion.** A process sees a clean, contiguous address space even though the physical
pages backing it are scattered wherever RAM was free. This later buys demand paging,
copy-on-write `fork`, memory-mapped files, and swap.

## Page faults

Touch an unmapped address, or violate the permissions, and the MMU **raises an exception
and hands control to the kernel**. That's a page fault.

Sometimes the kernel fixes it and resumes (the page was swapped out; copy-on-write needs
to duplicate it). Sometimes it kills the process. That's a segfault.

## Mechanics

Translation happens per **page** (4 KiB, typically), not per byte, because a per-byte
lookup table would be larger than the memory it describes.

Page tables are a radix tree. On aarch64 with 4 KiB pages and 48-bit addresses: **4
levels of 9 bits each** (512 entries per table), plus **12 bits of offset** within the
page. 9 x 4 + 12 = 48.

Walking a 4-level tree on every memory access would be brutally slow, so the CPU caches
recent translations in a **TLB** (translation lookaside buffer). Which means: **when the
kernel changes a mapping, it must invalidate the stale TLB entries.** Forgetting to is
one of the great classic kernel bugs, because everything works fine until it
catastrophically doesn't.

## The aarch64 two-register trick

aarch64 has **two** page table base registers, not one:

- `TTBR0_EL1` translates **low** addresses → userspace
- `TTBR1_EL1` translates **high** addresses → the kernel

The kernel lives permanently in the high half. On a context switch we swap only `TTBR0`;
the kernel's mappings never move. So a syscall requires no address space change at all.
x86_64 has one such register and achieves the same effect by convention. This is one of
the places aarch64's clean-sheet design visibly pays off.

## Turning it on is the sketchiest moment in the kernel

**The MMU is off at boot.** Addresses are physical.

The instant you flip the enable bit in `SCTLR_EL1`, the *very next instruction* is fetched
through the MMU. If you have not mapped the code that is currently executing, the CPU
tries to fetch from an address that no longer means anything, and the machine simply
stops existing.

You get one shot, and you cannot printf your way out of it. This is milestone 4, and it
is where the GDB stub earns its keep.

## Aside: why some chips can't run this OS

Microcontroller-class RISC-V and ARM parts (ESP32-C3, many Cortex-M) have **no MMU**.
No MMU means no virtual addresses, no isolation, no user mode as we mean it. They can run
an RTOS, but not the kind of OS we are building. This is why the RISC-V hardware we
considered had to be JH7110-class or better.

---

*Add to this file as new memory concepts come up.*
