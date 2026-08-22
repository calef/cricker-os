# nife

*Formerly cricker-os; renamed 2026-08-15, milestone 120. Old links redirect. The name is
lowercase everywhere, sentence starts included, and is said like* knife: *Ni + Fe, the Earth's
nickel-iron core. The full story, refused spellings included, is
[notes/naming.md](notes/naming.md).*

<img src="art/cobble-first-draft.jpg" alt="Cobble, the nife mascot: a stone golem with red eyes and mossy shoulders, holding a gear" width="300">

*Cobble, guardian of the machinery. First draft, by Clay. The full naming record is
[notes/mascot.md](notes/mascot.md).*

[![CI](https://github.com/crickertech/nife/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/crickertech/nife/actions/workflows/ci.yml)

A capability microkernel for aarch64 and riscv64, written in Rust, from the first instruction.

The goal (DECISIONS §14): a verified-Rust capability microkernel that runs real workloads,
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

At the `$` prompt: `help`, `echo hello`, `worker 7` (spawns a process that returns 49). Quit with
Ctrl-C, or `pkill qemu-system-aarch64` from another terminal.

## Start here

**A reading order, which is what this page used to leave you to guess at.** There are 427 markdown
files here and 145 of them are notes; the problem a newcomer has is not missing documents, it is
that nothing says which one is first. Read these in this order and stop when you have what you came
for.

1. **Run it.** The block above, in about the time it takes to read the next item. A system you have
   watched boot is a different thing to read about than one you have not.
2. **[`CONTRIBUTING.md`](CONTRIBUTING.md)** if you might change something. What this project wants, what
   it will cost you, what is yours to decide, and what to bring up instead of building.
3. **[`AGENTS.md`](AGENTS.md)**, which is the project's constitution and is the file most of the
   rules live in and nowhere else. It addresses whoever is doing the work, human or agent.
   `CLAUDE.md` at the root is a symlink to it; the two are one file, and the section below says why
   the name misleads.
4. **[`notes/capabilities.md`](notes/capabilities.md)**, the one idea everything else is downstream
   of. If a design choice here looks strange, this is usually the reason.
5. **[`design/roadmap/`](design/roadmap/README.md)**, the only status in the tree, with a fixed
   vocabulary and a checker. Anywhere else that claims status is stale by construction.
6. **[`DECISIONS.md`](DECISIONS.md)**, which is two pages and teaches you to resolve the `§N`
   citations the rest of the tree is full of. Then
   [`design/decisions/`](design/decisions/README.md) when you want the argument behind a specific
   choice, including the ones that were refused.
7. **[`notes/adding-a-program.md`](notes/adding-a-program.md)**, which is the first thing to do with
   your hands rather than your eyes. Doing it is how you find out whether you understood 4.
8. **[`notes/`](notes/README.md)** from here on **by question, not in order**. It is a glossary
   written while building, one file per question that turned out to be load-bearing, and reading it
   front to back is a mistake it will happily let you make.

**If you read only two**, make them 3 and 4: the rules, and the idea. Everything else you can look up
when it bites.

**Provisional, and this list is a claim about what matters**, so expect it to be reordered by
someone with the standing to make that claim. It came out of milestone 117 (the stranger test),
whose first two runs both established that the entry point was missing without either one being
able to say what it should be.

## What the badge means

The CI badge above is green only when **every** gate passes, and the gates are the argument rather
than a formality:

| Gate | What it proves |
|---|---|
| `script/test` | The host-logic crates, then the kernel under QEMU on **both ISAs**, aarch64 and riscv64. Architectural parity is a gate, not an aspiration (DECISIONS §19). |
| `script/verify` | over 100 Kani harnesses <!--count-at-least:kani-harnesses--> across more than 20 crates <!--count-at-least:harness-crates-->: the capability model, IPC, MMU isolation, the DMA validator, the IOMMU domain, the NTP era pivot. |
| `script/bench --check` | icount instruction counts against a committed baseline, on both ISAs, so a performance regression surfaces next to the change that caused it. |
| `script/lint` | clippy at `-D warnings`, plus broken intra-doc links, stray conflict markers, the roadmap's status vocabulary, DECISIONS numbering and citations, and that every script is documented. |
| `script/supply-chain` | cargo-deny (advisories, licences, bans, sources) over every workspace, and proof that each vendored tree is the published tarball plus exactly its recorded patches. |
| `script/fuzz` | Coverage-guided fuzzing of the four parsers that read bytes we did not write (a device tree from firmware, an ELF the loader will map, a partition table off somebody else's disk, and the boot archive's round trip). The complement to the proofs, not a second opinion on them: they are exhaustive inside a bound, this is unbounded and random. It found two bugs on its first day. |
| `script/fmt --check`, coverage | Formatting, and an 80%-per-file line-coverage floor on the host crates. |

CI runs on an **aarch64** runner deliberately: this kernel targets a weakly-ordered machine, and a
missing `Acquire`/`Release` passes on an x86_64 host and fails only on real ARM. Both the Rust
toolchain and QEMU are pinned to exact versions, so "the tests passed" means the same thing on a
laptop and on a runner.

## What it does

This section is deliberately **not** status. Status lives in one place, with a gated status column and
a checker: **[design/roadmap/](design/roadmap/README.md)**. What follows is what the system *is*, and each
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
git clone https://github.com/crickertech/nife
cd nife
script/setup               # installs the pinned Rust toolchain and QEMU, then builds

script/server              # boot it
script/test                # run the tests
script/console             # boot straight to the interactive shell
```

`script/server` boots the kernel on QEMU's `virt` machine and wires the emulated UART to your
terminal. Ctrl-A then X quits QEMU.

**On Linux, `script/setup` will stop at the QEMU check and tell you to run `script/ci-qemu` first.**
That is expected rather than broken: no Ubuntu release ships a QEMU with `riscv-iommu-pci`, and the
project refuses to drop the device, because a confinement test that quietly stops testing is worse
than a red build. Build the pinned QEMU (about twelve minutes), then run `script/setup` again.

```bash
script/catch-up            # what changed since you last looked
```

**`script/catch-up` is the one to run second**, and it is worth knowing about before you need it: it
recomputes what moved (milestone status, decisions landed, what is waiting on calef, what is ready to
start) from the roadmap, the decision files and git, rather than from a hand-written status page that
would rot. Milestone 117's second stranger run called it the best onboarding command here and noted
that nothing pointed at it, which this paragraph is fixing.

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
                       dtb, pci, frames, slots, nifefs, intrusive, asid, ...
bench/                 the benchmark suite and committed baselines (both ISAs)
script/                normalized entry points (setup, test, console, verify, bench, ...)
xtask/                 build orchestration (build, run, test, bench, gdb, objdump, image)
notes/                 a concept glossary, written as questions came up
design/                the roadmap and worked designs
design/decisions/      what we chose, what we rejected, and why
design/audit-reports/  every audit, its lens, and when the next one is due
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

## The rules a contributor is held to

**How to propose a change is [`CONTRIBUTING.md`](CONTRIBUTING.md)**, which is short and links to the
rules rather than repeating them. The rules themselves are below.

**They are in [`CLAUDE.md`](CLAUDE.md), and its name is misleading**, which milestone 117's first
stranger run established by skipping it: a 50 KB file called `CLAUDE.md` at the root reads as tooling
config for an agent, so a human contributor walks past it. It is not config. It is where the
project's rules live, and several of them exist nowhere else.

`script/lint` cites it by name in its own failure messages ("CLAUDE.md rule 7"), so a build can fail
against a file the person who triggered it was never told to read. Until that is fixed properly, this
paragraph is the pointer.

What is in there and nowhere else: that all architecture-specific code lives under
`kernel/src/arch/`; that a driver never reaches into a kernel global; that anything two binaries must
agree on is a crate rather than a `#[path]` module, and why (a shared module in a `no_std` binary is
unreachable by host tests and by Kani); that names are calef's call; and the ladder that ranks
"make the wrong state unrepresentable" above "a gate that fails loudly" above "a note nobody reads".

## The decisions

Written down in [`design/decisions/`](design/decisions/README.md) as they were made, so the reasons survive
contact with month four. The short version:

| | |
|---|---|
| **Architecture** | Three declared targets: aarch64 (first: clean exception model, weak ordering as a discipline), riscv64 (at parity), x86_64 (declared, not started). **Parity is a gate, not an aspiration** (DECISIONS §19): a capability ships on every supported ISA under the same suite, or the gap is on the record. |
| **Target** | QEMU `virt` (TCG and HVF) for daily work; real hardware is milestone 16. |
| **Kernel shape** | **Capability microkernel** (seL4-shaped, decided at milestone 7): no `open()`, no ambient authority, drivers are EL0 processes, and since milestone 14 the kernel allocates nothing. See DECISIONS §10 and §14. |
| **Execution** | **Preemptive threads with real stacks.** Not async: async assumes "I compiled everything that runs", and an operating system's whole purpose is to run code it did not compile ([§5](design/decisions/05-preemptive-threads.md)). |
| **SMP** | Four cores, per-CPU run queues, cross-core placement by inbox plus IPI. (the original plan said "one core, refactor when it hurts"; it hurt.) |
| **Verification** | Machine-checked proofs (Kani) of the capability core: `capability`, IPC, the MMU isolation invariants. The frontier moves inward from the pure-logic crates. |
| **Testing** | QEMU harness plus host-testable pure-logic crates from the first commit, plus benchmarks with committed baselines that fail on regression. |

## Milestones

**Not repeated here.** They live in **[design/roadmap/](design/roadmap/README.md)**, which has a status
column with a fixed vocabulary and a checker (`script/roadmap`) that fails the build if a milestone is
cited in prose without a row, or carries a status outside the vocabulary. This file used to hold a
second copy: fifty-two lines of tick-marks, a partial and out-of-order subset, and nothing checking
it. A duplicate of a gated artifact is the copy that goes stale, because only one of them has the
gate.

If you want the shape rather than the list: milestone 7 is the dividing line between "a Rust program
that boots" and "an operating system": it is where EL0, address spaces, capabilities, the ELF loader
and IPC arrive together.

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
