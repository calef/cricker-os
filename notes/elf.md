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

---

# Milestone 7c: the kernel actually loads one

The above was written at milestone 1, when ELF was a thing we *read about*. This is the part
where a file we did not compile becomes a running process.

## How the binary gets in: the initrd, which is how Linux does it

There is no filesystem yet (that is milestone 9). So the program arrives the way Linux's
**initramfs** does:

1. QEMU is given `-initrd target/.../hello`.
2. QEMU loads the file into RAM, somewhere, and **writes the address into the device tree** it
   generates, at `/chosen/linux,initrd-start` and `linux,initrd-end`.
3. The kernel reads it there ([device-tree.md](device-tree.md)) and tells the frame allocator
   that region is **forbidden**, or it would hand the program's own bytes out as scratch memory.

That reservation was written at **milestone 3**, with a comment saying milestones 8 and 10 would
want it. It turned out to be 7c.

**Nothing about the binary is known to the kernel at build time.** No `include_bytes!`, no build
script reaching into another crate's `target/`. The kernel is handed an address by the firmware
and finds a program there, which is exactly the relationship a real kernel has with its
bootloader.

## The parser is a HOST crate, and that is the whole trick

`crates/elf` is pure logic and compiles for the laptop, so its tests run in **milliseconds with
no emulator** ([DECISIONS §7](../DECISIONS.md)).

Which means **forging a malicious binary is eleven lines**:

```rust
let bytes = Builder::new()
    .seg(PF_R | PF_W | PF_X, 0x40_0000, &[0xaa; 16], 16)   // W AND X
    .build();

assert_eq!(Elf::parse(&bytes).err(), Some(Error::WritableAndExecutable));
```

Getting a real toolchain to *emit* that file, packing it into an initrd, and booting QEMU to
watch it be rejected would be a day of work and a twenty-second test. Here it is a microsecond,
and there are fourteen of them.

## What the loader refuses, and why each one is a real file and not a hypothetical

| Refused | Because an ELF can simply **ask** |
|---|---|
| `WritableAndExecutable` | ...for a page that is both. That is the thing every exploit wants, and the file is allowed to request it. `paging::Flags` has no constructor that returns one; this is what stops a *file* talking us into building one. |
| `SegmentOutOfBounds` | `p_offset` and `p_filesz` are **attacker-controlled**. A loader that trusts them reads past the buffer and then **maps what it finds into a process**. |
| `NotAarch64` | ...to be an x86 binary. Caught here, rather than as a mystery illegal-instruction fault at EL0. |
| `NeedsRelocation` | ...to be a PIE. It expects a dynamic linker. We are not one, and running it anyway means jumping to an address that means nothing. |
| `EntryNotExecutable` | ...to start in its `.data` segment. |
| `SegmentTruncated` | `p_memsz < p_filesz`. |

And one the parser **cannot** catch, because it is a kernel policy and not an ELF fact:

## The attack: a binary that asks to be loaded over the kernel

**An ELF names its own load address.** So a hostile one names `0xffff_0000_4008_0000` and waits
to see whether the loader is credulous.

It is refused **by construction, not by a check we remembered to write.** The user `Mapper` is
built with `Half::Low`, and a high address is **not a thing it can express**:

```rust
if !self.half.contains(va) {
    return Err(MapError::WrongHalf);
}
```

That guard has been in `paging` since **milestone 4**, and it was put there because a *host test*
discovered that bits 63:48 are not translated ([higher-half.md](higher-half.md)) and we needed a
way to say which table a mapping belongs to. It has been sitting there for three milestones,
waiting for this file.

This is the same move as `TlbFlush`'s `Drop` and the lock ranking: **make the bad state
unrepresentable rather than checking for it.**

## `memsz > filesz` is `.bss`, and forgetting it is the classic loader bug

```
Type: PT_LOAD    VirtualAddress: 0x402000    FileSize: 8    MemSize: 16
```

The file carries **eight** bytes. The program expects **sixteen**, and the other eight must be
**zero**. Copy `filesz` and stop, and the program's `.bss` holds whatever the previous owner of
that frame left behind. Every uninitialized-memory bug in that program becomes an information
leak from a dead process.

Our loader zeroes every page before copying, so the tail is free. **But only because we thought
about it**, and the test binary deliberately has a `.bss` variable it checks is zero, and a
`crates/elf` test asserts the test binary *has* a `.bss` at all, so the check cannot go vacuous.

## The loader honours permissions and does not widen them

An ELF's `.rodata` segment is `PF_R` **alone**. The tempting shortcut is to map every
non-executable segment as `user_data()` — which is **writable**, quietly granting the program
authority its own file never asked for.

`paging::Flags` grew a `user_rodata()` for exactly this. Three segment shapes, three
constructors, no widening:

| ELF says | We map |
|---|---|
| `PF_R \| PF_X` | `user_code()` — readable and executable at EL0, **PXN** so the kernel can never execute it |
| `PF_R` | `user_rodata()` — readable at EL0 and *nothing else* |
| `PF_R \| PF_W` | `user_data()` — readable and writable, **UXN and PXN** |
| `PF_W \| PF_X` | *refused* |

## The program has no syscalls, and says so in the only two words it has

There is no ABI yet ([DECISIONS §10](../DECISIONS.md): the syscall surface gets designed at 7d,
against a capability table). So the test binary cannot **tell** the kernel anything.

Instead it **checks its own image** and speaks with:

- **`svc`** — everything I expected about my own memory is true.
- **`brk`** — it is not. (Which the kernel treats as a fault, and kills it.)

**No data crosses the boundary.** The kernel counts `svc`s and faults and learns whether its
loader is correct, without either side agreeing on the meaning of a single register. `svc` and no
fault means: `.text` executed, `.rodata` was readable, `.data` was copied from the file, `.bss`
was zeroed, and the stack worked well enough to recurse eight frames.

### And a `brk` from EL0 had to stop being a breakpoint

Writing that program exposed a bug. `exception_dispatch` matched `ec::BRK64` **before** it checked
which exception level the trap came from, so a `brk` from a *user* program would have been
**stepped over** as if it were one of ours. A user program could park a `brk` in a loop and be
**immortal**.

A breakpoint is a debugging affordance for code we trust. From EL0 it is a fault.

## What it prints

```
    initrd : 813656 bytes at 0x44000000, from the device tree
    hello  : a real ELF, loaded from the initrd, ran and verified its own .text/.rodata/.data/.bss

  the machine has run code it does not trust, and taken the CPU back.
  and it did not compile it, or link it, or ever see it before this boot.
```
