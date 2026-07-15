# cricker-os

A small operating system for aarch64, written in Rust, from nothing.

This is a learning project. The goal is not to produce a useful OS, it's to understand how
operating systems actually work by building one, starting from the first instruction the
CPU ever executes. If it ends up useful, that's a bonus.

**Status: milestone 10 complete.** An interactive shell runs at EL0, spawning processes on command. It boots on QEMU, prints to a serial port, catches its
own faults and reports them legibly, reads its memory map out of the device tree, hands out
physical memory a page at a time, and **runs with the MMU on**: kernel `.text` is read-execute,
`.rodata` is read-only, nothing writable is executable, and there's a guard page under the
stack. `Vec` and `Box` work, because we built the heap they needed. The kernel runs in the
high half out of `TTBR1`, leaving `TTBR0` free for user processes.

```
cricker-os
  exception level : EL1
  stack top       : 0x0000000040097010
  device tree     : 0x0000000044000000

milestone 1: we are running our own code on a CPU with nothing underneath it.
milestone 2: and when it goes wrong, we get told.
           : and the machine now tells us what it is, instead of us guessing.
```

When something faults, you get this instead of a silent death:

```
[EXCEPTION]  Current EL, SP_ELx, Synchronous
             Data abort from the same EL (EC 0x25)

  ESR_EL1   0x0000000096000050   what happened
  FAR_EL1   0x00000000dead0000   the address that faulted
  ELR_EL1   0x0000000040081a40   the instruction that did it
  SPSR_EL1  0x00000000400003c5   the state it was in
```

## Quick start

You'll need [Rust](https://rustup.rs) (the toolchain file pins nightly and will install it
for you) and QEMU:

```bash
brew install qemu          # or your platform's equivalent
git clone https://github.com/calef/cricker-os
cd cricker-os

cargo xtask run            # boot it
cargo xtask test           # run the kernel's tests under QEMU
cargo xtask objdump        # disassemble it
cargo xtask image          # build the flat arm64 Image and dump its header
cargo xtask gdb            # boot paused, waiting for a debugger on :1234
```

`cargo xtask run` boots the kernel on QEMU's `virt` machine and wires the emulated UART to
your terminal. Ctrl-A then X quits QEMU.

## What's here

```
kernel/
  link.ld              where the image lives in memory, and what the linker exports to us
  src/arch/aarch64/
    image_header.s     64 bytes that make QEMU treat us as a kernel, not a blob
    boot.s             the first instructions the machine executes
    vectors.s          the exception vector table (shape dictated by silicon)
    exceptions.rs      the trap frame, ESR decoding, fault reports
    semihosting.rs     how we ask QEMU to exit with a status code
  src/drivers/pl011.rs the serial port
  src/console.rs       print! / println!
  src/testing.rs       the QEMU test harness
scripts/qemu-runner.sh how the kernel actually gets booted (ELF -> flat Image -> QEMU)
crates/dtb/            device tree parser        | pure logic, host-tested,
crates/frames/         physical frame allocator  | milliseconds, no emulator
xtask/                 build orchestration (build, run, test, gdb, objdump, image)
notes/                 a concept glossary, written as questions came up
design/                open proposals, not yet decided
DECISIONS.md           what we chose, what we rejected, and why
```

## The notes are the point

[`notes/`](notes/) is a running glossary written *while* building, not afterward. Every
file in it exists because a specific question came up and the answer turned out to be
load-bearing for code we actually wrote.

If any of the code looks like noise, start with
[**Reading aarch64 assembly**](notes/reading-assembly.md) and
[**Registers**](notes/registers.md). The second one is the most fundamental thing in the
repo: the register file *is* the CPU's state, in about 248 bytes, which is why context
switches and interrupts work the way they do.

Also in there: [what an MMU is](notes/mmu.md), [why the stack
exists](notes/stack.md), [what `no_std` actually removes](notes/no-std.md), [what a linker
script is for](notes/linker-scripts.md), [what QEMU is](notes/qemu.md), and [how portable
kernels are structured](notes/portability.md).

## The decisions

Written down in [`DECISIONS.md`](DECISIONS.md) before any code, so the reasons survive
contact with month four. The short version:

| | |
|---|---|
| **Architecture** | aarch64. Clean exception model, sane MMU, real hardware at the end of the road, and none of x86's forty years of archaeology. |
| **Target** | QEMU `virt` for daily work; a Raspberry Pi port as a deliberate later milestone. |
| **Kernel shape** | Monolithic, but drivers never reach into kernel globals and the syscall surface stays narrow. Two cheap rules instead of speculatively trait-ifying everything. |
| **Execution** | **Preemptive threads with real stacks.** Not async. See below. |
| **SMP** | One core for now. Refactor when it hurts. |
| **Testing** | QEMU harness plus host-testable pure-logic crates, from the first commit. |
| **Process model** | **Deliberately undecided.** Unix-like vs. capability-based gets settled at milestone 7, on purpose, as a recorded hard decision point. |

### Why not async/await

Because it's a ceiling, not a tradeoff.

A userspace process is an arbitrary ELF binary. It has its own stack, it never yields, and
it will loop forever, because you will write a bug. Under cooperative scheduling one bad
user program hangs the machine permanently, with no recovery.

Real user mode *requires* per-thread stacks, a context switch that saves and restores the
register file, and timer-driven preemption. Async doesn't defer that work. It forecloses
it. So we build real threads first, and async can come back later in userspace, on top of
them, exactly the way a real OS lets a program run Tokio.

**Async's core assumption is "I compiled everything that runs." An operating system's entire
purpose is to run code it did not compile.** That's why Embassy is excellent on a
microcontroller and impossible here.

And Go corroborates it the hard way. Goroutines were originally cooperative, yielding at
function calls — and Go owns its compiler and compiles *every line that runs*. It still didn't
work: a goroutine in a tight loop with no function calls never yields, and the garbage
collector could never stop it. **Go 1.14 added asynchronous preemption**, which is a timer
interrupt built in userspace out of signals. If a language that owns its entire toolchain
couldn't get away with cooperative scheduling, a kernel running arbitrary ELF binaries
certainly can't. See [DECISIONS.md](DECISIONS.md) §5.

## Milestones

The dividing line between "a Rust program that boots" and "an operating system" is
milestone 7.

| # | | |
|---|---|---|
| 1 | Boot to Rust, print to UART | ✅ |
| 2 | Exception vectors, handlers, legible fault reports | ✅ |
| 3 | Physical frame allocator, device tree parsing | ✅ |
| 4 | MMU on, W^X, guard page, kernel heap, higher-half | ✅ |
| 5 | GIC + timer interrupts | ✅ |
| 6 | Kernel threads, context switch, scheduler | ✅ |
| 7 | **EL0, address spaces, capabilities, ELF loader, IPC** | ✅ |
| 8 | **The console driver leaves the kernel** | ✅ |
| 9 | virtio-blk in userspace + a filesystem server | ✅ |
| 10 | A process server, and a shell that spawns binaries | ✅ |
| 11 | Untyped memory: the kernel stops allocating | ~ |

Deliberately out of scope for v1: SMP, a writable filesystem, networking, a GUI, dynamic
linking, real hardware. Each multiplies debugging difficulty, and none teaches something
the first ten don't already set up.

## Things this project has already gotten wrong

Kept here on purpose, because the corrections were the most instructive part.

**QEMU does not hand an ELF a device tree pointer in `x0`.** It only does that under the
Linux boot protocol, which it selects for flat arm64 `Image` files. We shipped an ELF, so it
took the bare-metal path and populated no registers. We found out by printing `x0` and
getting zero. *Since fixed*: we now emit a flat binary with a 64-byte Image header, and two
tests hold the line. See [notes/boot-protocol.md](notes/boot-protocol.md).

**`bl` does not push a return address onto the stack.** That's x86. On aarch64 the return
address goes into register `x30`, and the stack is where it gets *parked* when a function
needs `x30` for a call of its own. See [notes/stack.md](notes/stack.md).

**`into_iter()` on a big array is a kernel footgun.** Milestone 3 hung the machine for
150 seconds with no output. `[Option<Frame>; 1024].into_iter().flatten()` moves 16 KiB by
value, twice, onto a 64 KiB stack; `sp` walked through `.bss` and `.data` into `.text` and
the kernel executed its own overwritten code. Two of the three diagnoses along the way were
wrong. The write-up of *how it was actually found* (semihosting exit codes as bisection
markers, because `println!` runs through the `.text` you just corrupted) is the most useful
thing in [notes/stack.md](notes/stack.md).

## Reading

- The **xv6 book** (MIT, ~100pp) for how a real Unix-shaped kernel is put together
- [`rust-raspberrypi-OS-tutorials`](https://github.com/rust-embedded/rust-raspberrypi-OS-tutorials)
  for aarch64 mechanics
- The [OSDev wiki](https://wiki.osdev.org), as a reference rather than a tutorial
- [Compiler Explorer](https://godbolt.org), set to Rust + aarch64. The fastest way to build
  assembly intuition that exists.

## License

MIT
