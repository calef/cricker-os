# Concept notes

Running glossary for cricker-os. Written as concepts come up, not up front. If something
in the code or the conversation doesn't make sense, it belongs here.

## Start here

- [**Acronyms**](acronyms.md) — every one this project has thrown at you, expanded, with a
  link to the note that explains it properly. IRQ, GIC, PMR, ESR, TTBR, PXN, DAIF, BBM, and
  the forty others. Look here first.

## Tooling

- [QEMU](qemu.md) — the software computer we develop on. Why we need it, what the `virt`
  machine is, what each flag does.

- [Semihosting](semihosting.md) — how the kernel asks QEMU to exit with a status code, so
  that `cargo test` can read it. Also: it's a syscall ABI where the OS on the other side is
  the emulator, which makes it a preview of milestone 7 running backwards.

## Devices

- [The device tree](device-tree.md) — the machine describing itself. Everything in it is
  big-endian, and the width of an address is declared by the *parent* node. Those are the
  two things most likely to be silently wrong.
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

- [Tearing down an address space](teardown.md) — two ways to reclaim page-table frames
  (walk-and-reclaim vs record-all-frames), why a space that dies all at once wants the
  second, why kernel stacks want neither, and how a stale TODO nearly grew an unused method.
- [The heap and the slab](heap.md) — why the stack isn't enough (its lifetimes must nest, and a returned
  Vec's don't), why fragmentation is the permanent enemy, and why Rust's ownership system is
  really a heap-correctness checker.
- [Physical memory](physical-memory.md) — the frame allocator. Why a bitmap and not a free
  list, the bootstrap problem (the allocator's first act is to allocate itself), and why
  `mark_used` rounds *outward*.
- [The higher-half kernel](higher-half.md) — why the kernel MUST be in TTBR1 (or the first
  context switch would delete it), and the two facts that let a kernel linked at a high
  address boot from a low one: `adrp` is PC-relative, and bits 63:48 aren't translated.
- [aarch64 page tables](page-tables.md) — the structure the MMU walks. The trap bits (AF,
  PXN, AttrIndx), why W^X is enforced by construction, and the thing a failing host test
  taught us: bits 63:48 aren't translated, they choose which TABLE to use.
- [The MMU](mmu.md) — virtual vs. physical addresses, page tables, the TLB, page faults,
  and why turning it on is the scariest moment in the kernel.

## Rust

- [Vec, Box, String, BTreeMap](collections.md) — the four types the heap gave back. Why
  `Box` is what makes a recursive type finite, why `Vec` doubles, why `&str` works in
  `no_std` and `String` doesn't, and why a kernel uses `BTreeMap` and not `HashMap`.
- [`no_std`](no-std.md) — why the kernel can't use the standard library, what `core` still
  gives us, and how we earn each missing piece back by building the thing `std` assumed.

- [Interrupts: the GIC and the timer](interrupts.md) — the preemption source. Why the timer **9a**: and a hardware interrupt can become a message to a userspace driver.
  is a per-core PPI, why GIC priorities run backwards, and the bug we shipped: re-arming with
  a *relative* countdown silently lost 30% of our ticks.
- [Exceptions](exceptions.md) — faults, interrupts, and syscalls are **the same mechanism**
  on aarch64, which is why we build the plumbing once. The vector table's shape is dictated
  by silicon. Also: why `brk` needs `elr += 4` and `svc` doesn't.

- [Threads, the context switch, and preemption](threads.md) — a thread is a stack plus a set
  of register values, and here that's literal: 8 bytes. The context switch is fifteen
  instructions and **the last one returns into a different thread.**

- [Capabilities, and why the kernel has no `open()`](capabilities.md) — a capability is a file
  descriptor that can point at *anything*. Unix already had them; it just also built a back door.
  The milestone 7 decision, and the confused deputy. **7d**: three syscalls, a capability is the
  only way to print, and `AT S1E0R` is how the kernel refuses to read its own memory on a user's
  behalf. **7e**: endpoints and synchronous IPC, and the scheduler learns a thread can be
  `Blocked` waiting for a message it can only reach by a capability.
- [Who does IPC name?](ipc-naming.md) — an endpoint, never the peer. The sender names a
  channel it holds a capability to; the receiver is anonymous. No global namespace, which is
  no-ambient-authority made concrete. Even a hardware interrupt names an endpoint.
- [How authority moves, narrows, and ends](capability-lifecycle.md) — capabilities spread by
  copy-with-narrowing (never widening), `SEND_CAP` is share not move, the two independent
  narrowings (rights vs. GRANT), and why there's no revocation yet (a control gap, not a
  safety hole: spend-only untyped keeps shared frames valid).
- [Delegating a capability](delegation.md) — a capability system where processes can't pass
  capabilities isn't one. A process now delegates a capability to another over an IPC endpoint
  (`SEND_CAP`/`RECV_CAP`), narrowing the rights, and only if it holds `GRANT`. Authority composes
  between processes at runtime instead of being wired by the kernel at spawn.
- [Frame capabilities](frames.md) — shared memory a process owns rather than one the kernel wires
  in. Retype a page out of untyped into a `Frame`, map it, and delegate a read-only view to a peer
  that maps the same physical page. §10's "shared memory carries data," composed by the processes;
  the IPC rendezvous that carries the frame is also the edge that orders the memory.

- **7c update in [elf.md](elf.md)** — the kernel now *loads* one. An ELF names its own load
  address, so a hostile one names the kernel's; it is refused by a `Half::Low` guard that has
  been sitting in `paging` since milestone 4, waiting for exactly this file.

- [virtio-blk, driven from userspace](virtio.md) — milestone 9: a real block device driven by a
  process at EL0, with DMA, a virtqueue, and the completion arriving as an interrupt-message. Plus
  the two scheduler bugs it flushed out: no idle thread, and interrupts restored under the lock.

- [A shell at EL0](shell.md) — milestone 10: an interactive shell, a userspace input driver
  (console receive), and worker processes spawned on command. Proof the whole stack works, as a
  conversation between processes the kernel only routes.

- [Running under virtualization on Apple Silicon](virtualization.md) — `cargo xtask run --hvf`
  puts the kernel on the real M3 core via Apple's Hypervisor.framework. It found two QEMU-shaped
  assumptions on the first boot: the physical timer (fixed, we use the virtual timer now) and
  semihosting (emulation-only, so tests stay on TCG).

- [Untyped memory: the kernel stops allocating](untyped.md) — milestone 11: a process spends
  pages out of a capability to raw memory it was handed, and the kernel's free-frame count does not
  move while it allocates. A process cannot make the kernel allocate, so it cannot exhaust it.

- [Per-process resource quotas](quotas.md) — a spawner may have at most N children alive; the slot
  returns when a child is reaped, riding the thread's lifetime, so a spawn flood is bounded with no
  bookkeeping. Closes the audit's exhaustion vector.
- [Confining DMA without an IOMMU](dma.md) — the device bypasses the MMU, so a hostile driver
  could DMA over the kernel. Closed by kernel-mediated descriptor validation: the kernel owns the
  ring addresses and the notify, and refuses any descriptor outside the driver's own DMA region.
- [A security audit](security.md) — an adversarial four-part review of the whole kernel. The
  MMU and capability confinement held up; two panics on untrusted input were fixed; the DMA/no-IOMMU
  limitation and the missing resource quotas are named rather than hidden.

## The point of all this

- [The console driver leaves the kernel](userspace-drivers.md) — milestone 8: the console is now a
  userspace process that owns the UART, reached by IPC, and the kernel is no longer on the data
  path. The 7d confused-deputy bug is *dissolved*, not defended against.
- [Userspace](userspace.md) — the line. And as of 7a it is **real**: entering EL0 turns out to be
  *returning from an exception that never happened*, and the two bugs on the way there were worth
  more than the code
  between "a Rust program that boots" and "an operating system." Three walls, all of them
  hardware. **Read this to understand why the milestone order is what it is.**

## Design

- [Why this isn't a general-purpose OS](why-not-general-purpose.md) — what an application
  would actually hit (no POSIX/libc, no writable FS, no network, no GUI), why that's a
  deliberate teaching-subset choice rather than a limit of the model (Fuchsia is a
  general-purpose capability microkernel), and what it would take to grow toward one.
- [Deadlock](deadlock.md) — the four Coffman conditions, and why breaking *any one* makes
  deadlock impossible. Every rule in our locking discipline is "pick a condition and destroy
  it." Also: Rust does not save you from this, and the reason why is worth knowing.
- [Locking](locking.md) — why a plain spinlock in a kernel with interrupts is a
  *guaranteed* deadlock on a single core, the two orderings that are the whole point, and
  why "restore" is not the same as "enable".
- [How portable kernels are written](portability.md) — what actually goes in `arch/` (a
  surprisingly short list), what can't be abstracted (the memory model), and why the second
  port should come early and be as alien as possible.
- [Where cricker-os could actually run](target-hardware.md) — the ISA is almost never the
  constraint. What decides bootability, why a Pi 4 is the next port, and why the port
  *after* it should probably be a UEFI/ACPI machine rather than another Device Tree board.

## Build

- [LLVM](llvm.md) — the thing that actually turns our Rust into aarch64. rustc is a
  *frontend*; it emits LLVM IR and hands off. Explains why we get an ARM backend, a
  cross-platform linker, and `llvm-objcopy` for free.
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
- virtio
