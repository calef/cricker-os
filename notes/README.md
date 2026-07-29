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

- [The `script/` entry points](scripts.md) — the "Scripts to Rule Them All" front door:
  `setup`, `test`, `server`, `console`, and friends, thin wrappers over `cargo xtask` so every
  repo has the same first command. Also: why `script/` and `scripts/` both exist.

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
- [Harts and PEs](harts-and-pes.md) — the precise words for "one thing that runs an instruction
  stream" (RISC-V's hart, ARM's PE), why "core" is too ambiguous to build specs on, and the day
  the distinction earned its keep here (the icount clock counts harts, not cores).
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
  really a heap-correctness checker. **Retired from the kernel at milestone 14** (the kernel
  cannot allocate now; design/kernel-objects-from-untyped.md is the story of how), and the
  `heap`/`slab` crates were deleted outright on 2026-07-27 once nothing referenced them: the
  git history preserves the work, and a demonstrator's tree should hold what it ships. The
  note stays; building the allocator and then earning its deletion were both the point.
  **Milestone 27 brought the heap back in userspace**: `crates/uheap` (the algorithm,
  host-tested) plus `user_rt::heap` (a `GlobalAlloc` that grows out of the process's own
  untyped via `untyped::MAP`); the note's last section is that story.
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
- [The native ABI](abi.md) — the contract a program runs against (milestone 19e, "Decision 2"):
  one `svc` and four syscall numbers, the whole object world behind `SYS_INVOKE`, `_start(x0,x1,x2)`,
  and how a program meets its capabilities by convention rather than discovery. Why we wrote the
  convention down instead of building a BootInfo, and what a POSIX shim would cost (nothing, later).
- [Rust `std` on the native ABI](std.md) — milestone 27: std's platform layer implemented directly
  on the capability ABI (Hermit's shape, not a POSIX shim). Heap from an untyped budget, stdout to an
  endpoint, time from the virtual counter, `panic!` faults, `thread::spawn`/`fs` honestly
  `Unsupported`, and (phase two) `std::net`'s `TcpStream`/outbound `UdpSocket` bound to netd's socket
  contract. How build-std runs against a hardlink-cloned, patched rust-src, why the symlink farm
  was measured to fail, and the honest caveats (monotonic-only clock, non-crypto random, std-internals
  coupling).
- [How authority moves, narrows, and ends](capability-lifecycle.md) — capabilities spread by
  copy-with-narrowing (never widening), `SEND_CAP` is share not move, the two independent
  narrowings (rights vs. GRANT), and why there's no revocation yet (a control gap, not a
  safety hole: spend-only untyped keeps shared frames valid).
- [Object revocation: tearing a process back down](object-revocation.md) — reclaiming the TCBs,
  address spaces, and endpoints a process built (extends §13 from frames to objects). Region
  ownership plus generational staleness instead of a capability derivation tree, why destroy is
  the owner's explicit act and must stay off the scheduler lock, `Untyped::SPLIT`/`DESTROY`, and
  the generational region slots that make a repeatable spawn loop finally possible.
- [Supervision: a thread's death becomes a message](supervision.md) — milestone 22's fault endpoint
  (DECISIONS §26). The kernel is the only witness to a fault, so it delivers a five-word message
  (event, tid, pc, addr, reserved) to the supervision endpoint a thread was spawned holding; the
  corpse is dead-until-reaped so the supervisor can inspect it and reap it with §16 revocation. No
  new syscall or method: a spawn-slot convention and a message-format convention. Restart policy
  stays in userspace; the kernel never relaunches anything.
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

- [PCIe, and driving a disk over it](pcie.md) — the PCIe transport (DECISIONS §18): ECAM, BARs,
  the capability list, why the kernel is the firmware here (OpenSBI does no PCI), the transport
  seam that runs one driver over two buses, and INTx through the PLIC. The hardcodes are held by
  witnesses against the machine's own device tree.
- [A shell at EL0](shell.md) — milestone 10: an interactive shell, a userspace input driver
  (console receive), and worker processes spawned on command. Proof the whole stack works, as a
  conversation between processes the kernel only routes.
- [The line discipline as a userspace component](line-discipline.md) — milestone 28: the tty
  layer as a process (`termd`) on plain endpoints, a sans-IO editing engine host-tested against a
  screen model, why it was built rather than porting `noline`/`embedded-cli`, and the Reply-cap
  argument that makes it deadlock-free.
- [The terminal contract](terminal-contract.md) — milestone 28: the interface a terminal
  presents (the `OP_WRITE`/`OP_READLINE`/`OP_BYTES` IPC protocol, the read flags, the shared
  pages, and the honest limits), written down so milestones 29 and 31 implement against a
  contract, not against today's component.
- [The command line as a grant expression](grant-expression.md) — milestone 31: naming a resource
  in a command is how you grant it (Miller's "designation is authorization"), the inversion of
  Unix's ambient authority at the one interface a human touches. The shell's own budget, the
  `SEND_CAP`-to-init spawn protocol, `run --mem N` made real by the `budgeter` program, the "you
  hold no such capability" refusal, and the `SPLIT`-grants-`GRANT` fix that let untyped be delegated.
- [The program manifest](program-manifest.md) — milestone 31: a program's declared endowment,
  checked against the command at the prompt so a mismatch is a legible refusal, not a mystery hang.
  SHILL's contract shrunk to phase 1, and milestone 23's component contract in embryo.

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
  Now also the write direction (milestone 32: same check, both hazards) and the kill-mid-write
  record, including the DMA-frame-reclaim caveat.
- [Confining DMA with an IOMMU](iommu.md) — the hardware version (milestone 16b, DECISIONS §20), on
  both ISAs behind one seam: the format-generic `paging` crate builds a device's DMA domain (an
  identity map over the frames it may reach) the same way it builds a process address space, and two
  arch drivers (SMMUv3, RISC-V IOMMU v1.0.1) attach it. The disk and attacker suites run behind it;
  a confinement test makes the IOMMU fault an escaping DMA, so a silent bypass fails loudly. The
  shadow ring stays as defence in depth.
- [The network stack as a confined component](net.md) — milestone 30 (DECISIONS §21). Multi-queue
  DMA confinement (built, both ISAs): the validator grows a second queue and the receive direction,
  where the device writes into driver memory, proved by the same address-bounding check. Then the
  prior art (seL4 dataports, Fuchsia Netstack3, Plan 9 /net as the counter-design), the socket
  contract proposal and its open fork, the smoltcp 0.13.1 pin, and the driver/server work that
  follows.
- [A security audit](security.md) — an adversarial four-part review of the whole kernel. The
  MMU and capability confinement held up; two panics on untrusted input were fixed; the DMA/no-IOMMU
  limitation and the missing resource quotas are named rather than hidden.
- [Machine-checked proofs (Kani)](verification.md) — the verification thesis (DECISIONS §14) in
  practice: the capability model is proved for *every* input, not just tested on the cases we wrote.
  Run by `script/verify`. Milestone 18 completed the spread inward: `caps`, then IPC (rendezvous and
  the one-shot Reply), then the MMU isolation invariants, each proof landing on code the kernel runs.
- [Generational names](generational-names.md) — milestone 14 phase A: the thread table becomes a
  fixed generational slot table (`crates/slots`). A Tid is `(generation, slot)`; a dead thread's
  name can never resolve again, even after slot reuse. Bounded like an array, safe like a
  never-reused counter, and the first step toward capability-only thread naming.
- [Intrusive queues](intrusive-queues.md) — milestone 14 phase A.2: the run queues and migration
  inboxes become intrusive (`crates/intrusive`); the link lives inside the TCB, a push is two
  pointer writes that cannot allocate or fail, and a pop hands back the thread itself. One link
  means one queue, which is the scheduler's state machine made physical.
- [Benchmarks with teeth](benchmarks.md) — milestone 21: two instruments, because gating and
  truth exclude each other. Deterministic icount counts gate commits against a committed
  baseline (`script/bench --check`); HVF runs the kernel natively on the M-series core for real
  magnitudes. The first real numbers: IPC round trip ~705 ns, call/reply ~886 ns.
- [The PMU, and the two clocks in a core](pmu.md) — the cycle counter (`PMCCNTR`) versus the
  generic timer (`CNTVCT`), and why the coarse, boring timer is the one that survives
  virtualization. The reason our bench runs on a laptop and `sel4bench` does not.
- [ASIDs: tagged address spaces](asids.md) — milestone 15: every user mapping is `nG`, each
  address space owns one ASID for life, the tag rides in TTBR0 with the root, and the context
  switch flushes nothing. Why a bitmap suffices where Linux needs generations (milestone 14
  bounded the spaces), and the witness test that would catch a broken tag.
- [init, and loading a program from userspace](init-and-loading.md) — milestone 19d: the ELF
  parser leaves the kernel for init, an ordinary confined program. How init loads a child through
  the granular verbs (retype, copy-and-map each segment, endow, configure, start), why
  SYS_CAP_DELETE exists (a loader recycles a 16-slot cspace over hundreds of frames), and the two
  hardware details a userspace loader must respect (I-cache coherency, cross-space W^X).
- [The kernel's own budget](kernel-budget.md) — milestone 19c.1: kernel stacks stop drawing
  open-endedly from the frame allocator and draw from one boot-carved region (`kmem`) with
  page recycling, so the kernel cannot spend beyond its carve. The three-round decision behind
  it, and the fact that collapsed it: a thread cannot swap the stack it runs on, so every
  kernel stack is kernel-created and one budget covers all of them.
- [The TCB](tcb.md) — what a Thread Control Block is (our `Thread` struct, field by field), the
  acronym collision with Trusted Computing Base, and why TCBs live in a static pool rather than
  being retyped from kernel untyped (the phase B.2 decision: same machine behavior, and seL4's
  retype only earns its ledger once userspace is the one paying).

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
- [RedoxFS std-footprint audit](redoxfs-audit.md) — milestone 32's engine, costed by building
  it: the no_std core compiles for both bare-metal targets three imports away from clean, the
  Disk trait is a blk-IPC client's exact shape, and the one real cost (a userspace GlobalAlloc)
  was already on milestone 27's books.
- [Prior art and reuse](prior-art.md) — where to look before building (Redox, rCore, Tock,
  Hubris, seL4, Fuchsia) and the rule that decides build-vs-reuse: the reuse boundary is the
  TCB boundary. Inside it, always build; userspace components, actively prefer porting,
  because a confined foreign component is evidence for the milestone-23 thesis.
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
- [Porting to RISC-V](riscv-port.md) — the second-architecture port (milestone 20), the real
  test of rule #1. The exact `arch/` boundary RISC-V must satisfy, the two HAL leaks it exposes
  (`Context` is aarch64-shaped in portable code; the `paging` crate encodes the aarch64 descriptor
  format), the RISC-V specifics (SBI, S-mode boot, Sv39, NS16550, PLIC/CLINT), and the incremental
  plan from "compiles for riscv64" to "the capability core runs on a second ISA".
- [Scoping RISC-V / aarch64 parity](riscv-parity-scope.md) — aarch64 is a strict superset once the
  port proved the capability core; this scopes the remaining gap (SMP, an in-kernel test run,
  virtio+DMA, the full boot/shell, benchmarks), what each proves, and the order to close them.
- [Scoping a PCIe transport](pcie-transport-scope.md) — a PCI root complex (ECAM enumeration, BARs,
  virtio-pci capability parsing, INTx via the PLIC) so a virtio disk can be driven over PCIe, the
  transport QEMU's riscv `virt` and real hardware use. Portable (both boards are ECAM-generic); the
  virtqueue/DMA-confinement machinery is reused. Unblocks parity C.

## Build

- [LLVM](llvm.md) — the thing that actually turns our Rust into aarch64. rustc is a
  *frontend*; it emits LLVM IR and hands off. Explains why we get an ARM backend, a
  cross-platform linker, and `llvm-objcopy` for free.
- [Linker scripts](linker-scripts.md) — who decides what address your code lives at, why
  nobody zeroes our `.bss`, and where the stack comes from when there's no OS.
- [ELF](elf.md) — the container the kernel ships in. Sections vs. segments, where the
  entry point lives, what QEMU actually does with `-kernel` (almost nothing), and what a
  magic number is (the `BadMagic` that caught the 19f archive fed to the ELF loader).
- [The boot protocol](boot-protocol.md) — how QEMU decides whether you're a kernel or an
  anonymous blob, and the 64-byte arm64 Image header that is the entire difference. Why
  `text_offset` and the linker script must agree, and why the failure mode is silent.

---

## Still to write

Topics we've touched but not yet documented. Add as they come up:

- The GIC (interrupt controller)
- virtio
