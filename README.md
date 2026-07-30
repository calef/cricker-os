# cricker-os

[![CI](https://github.com/calef/cricker-os/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/calef/cricker-os/actions/workflows/ci.yml)
[![toolchain drift](https://github.com/calef/cricker-os/actions/workflows/toolchain-drift.yml/badge.svg)](https://github.com/calef/cricker-os/actions/workflows/toolchain-drift.yml)

A capability microkernel for aarch64 and riscv64, written in Rust, from the first instruction.

The goal (DECISIONS.md §14): a verified-Rust capability microkernel that runs real workloads,
built to stand next to Linux, macOS, and seL4 on the primitives that define an OS, and to win
where a minimal kernel should. The capability core carries machine-checked proofs. The kernel
allocates no memory of its own. Every driver and server is an EL0 process. The same portable
core boots on two ISAs.

This began as a learning project (build an OS to understand one) and pivoted to a demonstrator
deliberately, on the record. The habits survived the pivot: every decision written down, every
concept a note, every claim measured.

## Try it

```
script/setup               # one time: install the toolchain and QEMU, then build
script/console             # boot straight to an interactive shell at EL0
script/console --hvf       # ...on the real Apple Silicon core (instant boot)
script/server              # the full milestone tour, then the shell
script/test                # host tests, then the kernel under QEMU, both ISAs
script/verify              # the machine-checked proofs (Kani)
script/bench               # icount microbenchmarks against the committed baseline
```

`script/*` is the normalized "Scripts to Rule Them All" front door; each is a thin wrapper over
`cargo xtask`, which still does the work (`cargo xtask shell` and friends work too).

**The list above is a deliberate subset**, the seven commands worth knowing on day one. The complete
reference is [notes/scripts.md](notes/scripts.md), and `script/lint` checks that every script has an
entry there and that nothing named here has since been renamed away. Two documents, one comprehensive
and one curated, is fine; two documents both claiming to be complete is not, which is the mistake the
Status section above made twice.

At the `$` prompt: `help`, `echo hello`, `run 7` (spawns a process that computes 49). Quit with
Ctrl-C, or `pkill qemu-system-aarch64` from another terminal.

## What the badge means

The CI badge above is green only when **every** gate passes, and the gates are the argument rather
than a formality:

| Gate | What it proves |
|---|---|
| `script/test` | The host-logic crates, then the kernel under QEMU on **both ISAs**, aarch64 and riscv64. Architectural parity is a gate, not an aspiration (DECISIONS §19). |
| `script/verify` | 69 Kani harnesses across 13 crates: the capability model, IPC, MMU isolation, the DMA validator, the IOMMU domain. |
| `script/bench --check` | icount instruction counts against a committed baseline, on both ISAs, so a performance regression surfaces next to the change that caused it. |
| `script/lint` | clippy at `-D warnings`, plus broken intra-doc links, stray conflict markers, the roadmap's status vocabulary, DECISIONS numbering and citations, and that every script is documented. |
| `script/supply-chain` | cargo-deny (advisories, licences, bans, sources) over every workspace, and proof that each vendored tree is the published tarball plus exactly its recorded patches. |
| `script/fmt`, coverage | Formatting, and an 80%-per-file line-coverage floor on the host crates. |

CI runs on an **aarch64** runner deliberately: this kernel targets a weakly-ordered machine, and a
missing `Acquire`/`Release` passes on an x86_64 host and fails only on real ARM. Both the Rust
toolchain and QEMU are pinned to exact versions, so "the tests passed" means the same thing on a
laptop and on a runner. The second badge is a daily check of whether the *newest* nightly still
builds us; it is informational and never blocks a merge.

## What it does

This section is deliberately **not** status. Status lives in one place, with a gated status column and
a checker: **[design/roadmap.md](design/roadmap.md)**. What follows is what the system *is*, and each
claim points at the artifact that keeps it true rather than repeating a list that goes stale. The
previous version of this section did repeat them, and drifted twice inside three days.

- **The security-critical logic carries machine-checked proofs.** Kani, run by `script/verify` and
  gated in CI. Which crates and which properties, with the bounds and their justifications, is
  [notes/verification.md](notes/verification.md); the count is whatever the gate prints.
- **The kernel does not allocate.** There is no kernel heap. Page tables, TCBs, endpoints, and
  address spaces are all retyped out of untyped memory that userspace owns and pays for.
- **Processes come and go.** A userspace init builds the whole system through granular
  capability verbs (retype, configure, insert, start), and object revocation tears a process
  back down: its TCBs, address spaces, endpoints, and the memory behind them, reclaimed safely.
- **It runs real workloads.** A CoreMark-derived compute program against the written native ABI
  ([notes/abi.md](notes/abi.md)), and ordinary Rust `std` programs on a custom target.
- **Two ISAs at parity.** Everything architecture-specific lives under `kernel/src/arch/`, and
  riscv64 proves it: SMP, the whole test suite, the interactive shell, and the benchmarks all run on
  both. Parity is a gate rather than an aspiration (DECISIONS §19).
- **SMP.** Four cores via PSCI (aarch64) and SBI (riscv64), per-CPU run queues, cross-core
  placement by inbox plus a reschedule IPI. No shared run-queue lock.
- **Every driver and server is an EL0 process**, confined by the MMU and, for DMA, by a validator
  and an IOMMU. A driver that misbehaves faults; it does not take the kernel with it.
- **Benchmarked against Linux and macOS, honestly.** Same Apple Silicon core, same virtualization
  tier, release builds. Every number, and every caveat that makes a comparison not apples to apples,
  is in [notes/benchmarks.md](notes/benchmarks.md), which is the only place they are written down.

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

```bash
git clone https://github.com/calef/cricker-os
cd cricker-os
script/setup               # installs the pinned Rust toolchain and QEMU, then builds

script/server              # boot it
script/test                # run the tests
script/console             # boot straight to the interactive shell
```

`script/server` boots the kernel on QEMU's `virt` machine and wires the emulated UART to your
terminal. Ctrl-A then X quits QEMU.

The `script/*` commands are the normalized entry points (the [Scripts to Rule Them
All](https://github.com/github/scripts-to-rule-them-all) pattern, one interface across every
repo). They are thin wrappers over `cargo xtask`, which still does the work and exposes the rest:

```bash
cargo xtask objdump        # disassemble it
cargo xtask image          # build the flat arm64 Image and dump its header
cargo xtask gdb            # boot paused, waiting for a debugger on :1234
cargo xtask bench --riscv  # the benchmark suite on the second ISA
```

## What's here

```
kernel/
  src/arch/aarch64/    boot.s, vectors, MMU, GIC, timer, PSCI: everything ISA-specific
  src/arch/riscv64/    the same boundary, proved by a second ISA (SBI, Sv39, PLIC)
  src/drivers/         pl011, ns16550: a driver gets a base address and nothing else
  src/                 capabilities, scheduler, IPC, untyped, revocation, the syscall surface
user/                  EL0: init, the shell, the console/input/block drivers, servers
crates/                pure logic, host-tested in milliseconds: caps, ipc, paging, elf,
                       dtb, pci, frames, slots, crickerfs, intrusive, asid, ...
bench/                 the benchmark suite and committed baselines (both ISAs)
script/                normalized entry points (setup, test, console, verify, bench, ...)
xtask/                 build orchestration (build, run, test, bench, gdb, objdump, image)
notes/                 a concept glossary, written as questions came up
design/                the roadmap and worked designs
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

Written down in [`DECISIONS.md`](DECISIONS.md) as they were made, so the reasons survive
contact with month four. The short version:

| | |
|---|---|
| **Architecture** | Three declared targets: aarch64 (first: clean exception model, weak ordering as a discipline), riscv64 (at parity), x86_64 (declared, not started). **Parity is a gate, not an aspiration** (DECISIONS §19): a capability ships on every supported ISA under the same suite, or the gap is on the record. |
| **Target** | QEMU `virt` (TCG and HVF) for daily work; real hardware is milestone 16. |
| **Kernel shape** | **Capability microkernel** (seL4-shaped, decided at milestone 7): no `open()`, no ambient authority, drivers are EL0 processes, and since milestone 14 the kernel allocates nothing. See DECISIONS.md §10 and §14. |
| **Execution** | **Preemptive threads with real stacks.** Not async. See below. |
| **SMP** | Four cores, per-CPU run queues, cross-core placement by inbox plus IPI. (v1 said "one core, refactor when it hurts"; it hurt, we refactored.) |
| **Verification** | Machine-checked proofs (Kani) of the capability core: `caps`, IPC, the MMU isolation invariants. The frontier moves inward from the pure-logic crates. |
| **Testing** | QEMU harness plus host-testable pure-logic crates from the first commit, plus benchmarks with committed baselines that fail on regression. |

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
function calls, and Go owns its compiler and compiles *every line that runs*. It still didn't
work: a goroutine in a tight loop with no function calls never yields, and the garbage
collector could never stop it. **Go 1.14 added asynchronous preemption**, which is a timer
interrupt built in userspace out of signals. If a language that owns its entire toolchain
couldn't get away with cooperative scheduling, a kernel running arbitrary ELF binaries
certainly can't. See [DECISIONS.md](DECISIONS.md) §5.

## Milestones

The v1 plan, all built. The dividing line between "a Rust program that boots" and "an
operating system" is milestone 7.

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
| 11 | Untyped memory: the kernel stops allocating for userspace | ✅ |

The post-v1 roadmap ([design/roadmap.md](design/roadmap.md)), reordered by DECISIONS §14
around verification and real workloads:

| # | | |
|---|---|---|
| 12 | Call/Reply IPC: a one-shot reply capability | ✅ |
| 13 | Frame revocation: un-share a page from every holder | ✅ |
| 26 | Object revocation: tear a process back down | ✅ |
| 18 | Verify the capability core: `caps`, IPC, MMU invariants (Kani) | ✅ |
| 14 | Kernel objects from untyped: the kernel heap is deleted | ✅ |
| 15 | Tagged address spaces (ASIDs) | ✅ |
| 21 | Benchmarks with teeth: icount + committed baselines | ✅ |
| 19 | A real workload: userspace init, the native ABI, CoreMark | ✅ |
| 20 | The second architecture: RISC-V, brought to full parity | ✅ |
| 25 | Cross-OS numbers: vs Linux and macOS at a matched tier | ✅ (seL4 waits on real-hardware PMU) |
| 16 | Real hardware (16a: RISC-V silicon first) + IOMMU isolation in QEMU on both ISAs (16b: SMMUv3 + ratified RISC-V IOMMU behind one seam) | next |
| 22 | Trusted init: verify it, shrink what a broken one can do | ahead |
| 23 | A capability-routed component OS with live replacement | the destination |
| 17 | Multikernel-leaning scheduler | optional research |
| 24 | A second aarch64 board (Virtualization.framework) | optional |
| 27 | Rust `std` on the native ABI | ahead (feeds 23) |
| 28 | A solid terminal: the line discipline as a component | ahead (feeds 23, 27) |
| 29 | A display terminal (framebuffer, virtio-gpu, libghostty-vt) | optional |
| 30 | The network stack as a confined component (virtio-net, smoltcp) | ahead (feeds 23, 27) |
| 31 | A capability shell: designation is authorization (SHILL-inspired) | ahead (feeds 22, 23) |
| 32 | A real filesystem: RedoxFS behind a capability FS server | ahead (feeds 23, 27, 31) |

Also built along the way, outside the numbering: a four-part adversarial security audit,
per-process spawn quotas, kernel-mediated DMA confinement (QEMU `virt` has no IOMMU),
capability delegation between processes, and frame capabilities (shared memory a process owns
and delegates).

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

## Security

[SECURITY.md](SECURITY.md) says what is in scope (the confinement boundaries this kernel claims to
enforce), what is not (a demonstrator under QEMU is not a production system), and how to report
something privately. Two adversarial reviews are already on the record:
[notes/security.md](notes/security.md) and [notes/arch-audit.md](notes/arch-audit.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the
work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
