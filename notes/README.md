# Concept notes

Running glossary for cricker-os. Written as concepts come up, not up front. If something
in the code or the conversation doesn't make sense, it belongs here.

## Tooling

- [QEMU](qemu.md) — the software computer we develop on. Why we need it, what the `virt`
  machine is, what each flag does.

- [Semihosting](semihosting.md) — how the kernel asks QEMU to exit with a status code, so
  that `cargo test` can read it. Also: it's a syscall ABI where the OS on the other side is
  the emulator, which makes it a preview of milestone 7 running backwards.

## Devices

- [The UART](uart.md) — the serial port, and why every kernel learns to drive one first.
  What "asynchronous" actually means (there is no clock wire), and a line-by-line read of
  our own PL011 driver.

## Architecture

- [Registers](registers.md) — 248 bytes of storage inside the CPU, and why that's the
  whole ballgame. **The most fundamental note here.** The register file *is* the CPU's
  state, which is why context switches and interrupts work the way they do.
- [aarch64](aarch64.md) — the instruction set. Registers, exception levels (EL0-EL3),
  system registers, and why the target triple is spelled the way it is.
- [The stack, `sp`, and `x30`](stack.md) — the stack is just RAM plus an agreement. Why
  `bl` doesn't push, why `sp` must be 16-byte aligned, and why there's one `sp` per
  exception level.
- [Reading aarch64 assembly](reading-assembly.md) — five rules that decode almost
  everything, the addressing-mode table, and a line-by-line walkthrough of `boot.s`.
  **Start here if a code block looks like noise.**

## Memory

- [The MMU](mmu.md) — virtual vs. physical addresses, page tables, the TLB, page faults,
  and why turning it on is the scariest moment in the kernel.

## Rust

- [`no_std`](no-std.md) — why the kernel can't use the standard library, what `core` still
  gives us, and how we earn each missing piece back by building the thing `std` assumed.

- [Exceptions](exceptions.md) — faults, interrupts, and syscalls are **the same mechanism**
  on aarch64, which is why we build the plumbing once. The vector table's shape is dictated
  by silicon. Also: why `brk` needs `elr += 4` and `svc` doesn't.

## The point of all this

- [Userspace](userspace.md) — what milestone 7 actually builds, and why it's the line
  between "a Rust program that boots" and "an operating system." Three walls, all of them
  hardware. **Read this to understand why the milestone order is what it is.**

## Design

- [How portable kernels are written](portability.md) — what actually goes in `arch/` (a
  surprisingly short list), what can't be abstracted (the memory model), and why the second
  port should come early and be as alien as possible.
- [Where cricker-os could actually run](target-hardware.md) — the ISA is almost never the
  constraint. What decides bootability, why a Pi 4 is the next port, and why the port
  *after* it should probably be a UEFI/ACPI machine rather than another Device Tree board.

## Build

- [Linker scripts](linker-scripts.md) — who decides what address your code lives at, why
  nobody zeroes our `.bss`, and where the stack comes from when there's no OS.
- [ELF](elf.md) — the container the kernel ships in. Sections vs. segments, where the
  entry point lives, and what QEMU actually does with `-kernel` (almost nothing).
- [The boot protocol](boot-protocol.md) — how QEMU decides whether you're a kernel or an
  anonymous blob, and the 64-byte arm64 Image header that is the entire difference. Why
  `text_offset` and the linker script must agree, and why the failure mode is silent.

---

## Still to write

Topics we've touched but not yet documented. Add as they come up:

- The GIC (interrupt controller)
- Context switching, and what a "register file" is
- virtio
