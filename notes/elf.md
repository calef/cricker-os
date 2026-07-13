# ELF

**E**xecutable and **L**inkable **F**ormat. The standard container for compiled code on
Unix-like systems. (macOS uses Mach-O, Windows uses PE, but every bare-metal ARM and
RISC-V toolchain emits ELF, including ours.)

An ELF file is **a bag of bytes plus metadata describing what those bytes are and where
they belong.**

## One file, two views

The whole design, and what "Executable *and* Linkable" is pointing at. The same bytes are
indexed twice, for two audiences.

| | **Sections** (linking view) | **Segments** (execution view) |
|---|---|---|
| Granularity | fine: dozens | coarse: usually 2-4 |
| Examples | `.text`, `.rodata`, `.data`, `.bss`, `.symtab`, `.debug_info` | "load 8 KB at `0x40080000`, read+execute" |
| Who reads it | the **linker**, `objdump`, GDB | whoever **loads** the file |

The linker thinks in **sections** because it merges, sorts, and places them (exactly what
our [linker script](linker-scripts.md) does). A loader thinks in **segments** because it
doesn't care about `.rodata` vs `.text`, only "which contiguous chunks go where, with what
permissions." Sections are grouped into segments by permission before shipping.

Same bytes, two indexes.

## The ELF header

First 64 bytes of the file:

- Magic number `\x7f E L F`, so anything can identify the format instantly
- 64-bit? Little-endian? Machine type (`EM_AARCH64`)?
- **The entry point address** (`e_entry`)

That last one is the payoff. `e_entry` is a single 64-bit number saying "start executing
here," and it is *exactly* what `ENTRY(_start)` in our linker script sets. The linker script
writes an address into the ELF header; QEMU reads it back out and puts it in the program
counter. That's the handoff.

## What QEMU does with `-kernel kernel.elf`

Deliberately, almost nothing:

1. Read the ELF header. Confirm it's aarch64.
2. Walk the **program headers** (segments). For each loadable one, copy its bytes to the
   physical address it names.
3. Set the program counter to `e_entry`.
4. Release the CPU.

That is the entire "boot process." No relocation, no address space, no stack, no `argv`.

### Nobody zeroes `.bss`

`.bss` occupies **zero bytes in the file** (it's just "reserve N bytes here"), so there is
nothing to copy. In a normal program the C runtime zeroes it before calling `main`.

We have no C runtime. Real hardware certainly won't do it, and we don't rely on QEMU to.
**So `boot.s` zeroes it by hand.** That loop is not paranoia; it is the missing piece of a
runtime we don't have.

## Why ELF and not a flat binary

You could strip all metadata and get a **flat binary** (`objcopy -O binary`): raw bytes,
loaded at a fixed address, no header, no structure. That's what a real Raspberry Pi wants
(`kernel8.img`). Most primitive thing possible.

We use ELF for two practical reasons:

**The entry point travels with the file.** A flat binary loader has to *assume* execution
starts at byte zero. ELF says so explicitly.

**Symbols.** ELF carries `.symtab` (names → addresses) and `.debug_*` (DWARF). This is how
GDB knows `0x400800f0` is `kernel_main`, and how it shows the **Rust source line** you're
stopped on instead of a raw address. Debugging a kernel without symbols is miserable, and
it's a big reason we set up the GDB path early.

Symbols also flow the other way, which we already rely on: `__bss_start` and `__stack_top`
are symbols the **linker invents** and writes into the ELF, so our assembly can reference
addresses it has no way of knowing at compile time.

## Why don't macOS and Windows use it?

History, not merit. The three formats are far more alike than different: header, symbol
table, relocations, "load these bytes here with these permissions." Nobody looked at ELF
and found it wanting.

**The timing is the whole story.** ELF was published ~1988 with System V Release 4 and took
years to become *the* Unix standard (Linux didn't switch from `a.out` to ELF until ~1995).
Both Apple's and Microsoft's formats were locked in before that happened.

**Mach-O** comes from the Mach kernel (CMU, mid-1980s). NeXT built NeXTSTEP on Mach + BSD
and used Mach-O. Apple bought NeXT in 1997, NeXTSTEP became Mac OS X, and Mach-O came
along. Apple never *chose* Mach-O over ELF; it was already in the building.

One real technical reason it stuck: Mach-O supports **fat binaries** (one file containing
code for multiple architectures). Apple changes CPU architecture roughly every decade
(68k → PowerPC → Intel → Apple Silicon) and each transition was survivable partly because
one `.app` could run natively on both old and new machines. ELF has no equivalent.

> The interesting version of the fat-binary argument is not about ISAs at all, it's about
> **microarchitecture variants within one ISA** (AVX-512, LSE atomics, SVE). That case is
> live for cricker-os and is written up in
> [design/fat-binaries.md](../design/fat-binaries.md).

**PE** (Portable Executable, Windows NT 1993) extends **COFF**, AT&T's *previous* Unix
object format from the early 1980s — the one ELF was designed to replace. NT development
started ~1988; the team took the well-understood format they had and extended it for DLLs
and Windows' resource system. They had zero incentive to adopt a brand-new, unproven,
competitor's-Unix standard.

**The principle:** an executable format is a compatibility boundary with enormous switching
costs and near-zero switching benefits. Compiler, assembler, linker, loader, dynamic
linker, debugger, profiler, `nm`, `strip`, and the kernel's `exec` path all have to agree,
and changing it breaks every binary ever compiled. So it gets decided very early, usually
by "what did the builders already have lying around," and then frozen forever.

### The punchline

**UEFI firmware uses PE.** Microsoft's format is what boots essentially every modern x86
and ARM PC, including Linux machines. A UEFI bootloader is formally a Windows executable.

So had we gone x86_64 + UEFI, we'd have been asking Rust to emit **a Windows PE binary** to
boot a Unix-flavored kernel from a Mac. Not a joke: that is literally what the
`x86_64-unknown-uefi` target does.

We dodged it by picking aarch64 and QEMU's `-kernel`, which takes ELF directly.

## Poking at it

Once we build, all of these work on our kernel:

```bash
readelf -h kernel.elf     # the header: entry point, machine type
readelf -l kernel.elf     # program headers (segments) - what QEMU reads
readelf -S kernel.elf     # section headers - what the linker made
nm kernel.elf             # symbols: where did __bss_start land?
```

Running `readelf -l` and seeing `LOAD 0x40080000` is a nice moment. It's the linker
script's decisions, made visible.

---

*Add to this file as new ELF details come up.*
