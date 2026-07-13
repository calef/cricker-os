# Linker scripts

The file where you discover everything the operating system was quietly doing for you.

## What a linker does

The compiler turns each source file into an object file full of **sections**:

| Section | Contents |
|---|---|
| `.text` | machine code |
| `.rodata` | constants, string literals |
| `.data` | globals with a nonzero initial value |
| `.bss` | globals initialized to zero |

The linker merges all the object files, resolves symbol references (`bl kernel_main`
becomes a real offset), assigns every section a **final address**, and emits an
executable.

That last step is the whole story. *Someone has to decide what address the code lives at.*

## In a normal program you never think about this

The toolchain ships a default linker script, and the OS loader does the rest. Your ELF
says "load me at 0x400000", and the loader creates a virtual address space, maps your
pages, zeroes your `.bss`, allocates a stack, sets up `argv`, and calls `main`. All
invisible.

## For a kernel, none of that exists

QEMU is deliberately dumb. Given `-kernel foo.elf` it:

1. Reads the ELF program headers.
2. Copies bytes to **exactly the physical addresses those headers name**.
3. Jumps to the entry point.

That's all. Nobody relocates you, gives you an address space, zeroes anything, or gives
you a stack.

So **we** decide the physical memory layout, and the linker script is where we say it. On
the QEMU `virt` machine RAM begins at `0x4000_0000`, so we link the kernel at
`0x4008_0000` (128 KiB in, following the ARM Linux convention). Get this wrong and the
absolute addresses baked into the code point at nothing; the machine dies before printing
a character.

## The five jobs our script does

### 1. Force the boot code to be first

QEMU jumps to the ELF entry point and `_start` must actually be there. But LLVM reorders
functions freely; it has no idea one is special. So `_start` goes in a custom section
`.text.boot`, and the script places `.text.boot` first and wraps it in `KEEP`.

`KEEP` matters because **nothing calls `_start`**, so dead-code elimination would happily
delete it.

### 2. Reserve a stack

There is no OS to allocate one. At boot, `sp` points at garbage. The script carves out
64 KiB and defines `__stack_top`, and the first thing `_start` does is load it into `sp`.

**Before that instruction runs, you cannot call any Rust function.** On aarch64 the `bl`
itself is fine (the return address goes in register `x30`, not on the stack), but the
callee's prologue immediately stores registers and locals to `[sp, ...]`, which with a
garbage `sp` writes to a random address in memory. See [the stack note](stack.md).

### 3. Tell the code where `.bss` is, so we can zero it

`.bss` holds globals that start at zero. As an optimization it occupies **zero bytes in
the file**. The ELF just says "reserve N bytes here." Normally the OS loader zeroes that
region as it loads the program.

Nobody is doing that for us. The boot assembly must zero it by hand, so it needs to know
where `.bss` starts and ends. The **linker** is the only thing that knows, so the script
exports `__bss_start` and `__bss_end` as symbols the assembly can reference.

This is the inversion that trips people up: normally your code tells the linker what it
needs. Here **the linker tells your code where things ended up.**

### 4. Page-align the sections

Push `.text`, `.rodata`, and `.data` to 4 KiB boundaries. Looks like pointless padding
now. It pays off at milestone 4: MMU permissions are **per-page**, so if `.text` shares a
page with `.data` we cannot map code read-execute and data read-write. See [the MMU
note](mmu.md).

### 5. Discard the junk

`.eh_frame` (stack-unwinding tables), `.comment` (compiler version strings). Dead weight
in a kernel.

## Why it's worth the discomfort

A linker script feels like arcane build-system tax. It isn't. It is the first place the
project makes you confront that **a running program is not self-sufficient**. Something
set up its stack. Something zeroed its memory. Something decided where it lived.

That something was the operating system. Now it's us.

---

*Add to this file as new linker concepts come up.*
