# Working on cricker-os

## What this project is

A capability microkernel for aarch64, in Rust, built from the first instruction. **It is a
demonstration OS** (DECISIONS.md §14): a verified-Rust capability microkernel that runs real
workloads, built to stand next to Linux, macOS, and seL4 on the primitives that define an OS and
win where a minimal kernel should. Chris is an experienced software engineer and engineering
leader; on this project he is the **architect and reviewer**, not the line-by-line builder.

That should drive your judgment calls. **A complete, correct, well-documented, benchmarked
milestone is the goal.** Proceed autonomously, produce whole pieces, and let Chris steer at the
design forks.

This began as a learning project and pivoted to a demonstrator, deliberately and on the record
(2026-07-26). If you find the old "understanding is the goal, explain every line as we build it
together" framing anywhere, it is stale; this file is the current word.

## How to work

**Default to autonomous execution.** Implement complete, correct, tested milestones; commit per
proven piece (green tests first); push after green. You are building the demonstrator. Chris
reviews architecture and outcomes, not every line.

**Stop and bring it to Chris only when it is genuinely his call:** a design fork not already
decided, a test that will not pass after real effort, a hardware or external dependency, or the
machine contradicting the plan. Otherwise proceed and report what you did.

**Keep the documentation current, because a demonstrator's docs are part of the deliverable.**
Every design decision goes in `DECISIONS.md`; every concept and finding gets a note in `notes/`,
indexed in `notes/README.md`. Record the *why* and the honest caveats.

**The standard to aim at is FreeBSD's** (Chris, 2026-07-30): the Handbook and the man pages, which
are the best documentation in the field and are the reason a FreeBSD admin can answer a question
without leaving the system. Four things make them that, and all four are things we can do:

- **Task-oriented.** "How do I do X", in order, with the actual commands, rather than a reference
  dump the reader has to reassemble.
- **In-tree and versioned with the code**, so the docs cannot describe a system that no longer
  exists. Already true here; keep it true.
- **Real `EXAMPLES`.** A page without a worked example has not finished explaining itself.
- **An honest `BUGS` section.** FreeBSD man pages document known limitations *in the manual*, next to
  the feature, rather than only in a tracker. This is the one worth copying hardest, because it is
  the convention this project already reaches for by instinct: the map "tie", the spawn caveat, the
  scope notes on parity gaps. **Name the limitation where the reader meets the feature.**

The point is not the format, which is theirs. It is the posture: documentation written for someone
who has to *use* the thing, and honest enough that they trust it when it says something works.

**Anything global to the tree is assigned by the integrator at merge, never claimed by a lane.**
Concurrent lanes cannot see each other, so a lane that reaches for a shared resource is guessing.
Two kinds bit us on 2026-07-30:

- **`DECISIONS.md` section numbers**, three collisions in one day. Preferred: a lane **does not touch
  `DECISIONS.md` at all**, puts the reasoning in `notes/` and in its report, and the integrator mints
  the section at merge. (Milestone 51's calendar lane did exactly this, unprompted, and it was the
  only one of four that caused no conflict.) If a lane must write the section to make its own gates
  pass, the number is **provisional**: say so in the report, and expect renumbering.
- **Counts that span the tree.** The Kani harness count was written as 76 on one branch and 80 on
  another; the merged tree had 95. Both were counted honestly. Take such a number at merge, from the
  merged tree.

**After any renumber, check citations by content, not by running the gate.** `script/decisions
--check` verifies that a cited `§N` resolves to *some* section, never that it resolves to the right
one, so a well-formed wrong citation is invisible to it. This has already produced two of them.

**Benchmarks and cross-OS comparisons are first-class.** Measure, do not argue. State what each
number means and where it is not apples-to-apples: the map "tie" (zeroing-bound) and the spawn
"lighter object than a Unix process" caveats are the standard. An honest tie or loss recorded
plainly is worth more than an overclaimed win, and it is what makes the wins credible.

**Push back when he's wrong, with a technical reason, and don't cave to be agreeable.** He once
picked async/await because it "sounded more tractable"; the right response was to point out that
cooperative scheduling cannot run an arbitrary ELF binary, so async forecloses the hard work rather
than deferring it. He changed his mind. Do that again when warranted; do not manufacture
disagreement to seem rigorous.

**Correct yourself loudly.** We told him QEMU passes a device tree pointer in `x0`. It doesn't. We
found out by printing it and getting zero, and fixed the note rather than quietly patching over it.
The machine overrules the documentation, and it overrules you; when it does, fix the record on
purpose.

**Explain on request, however basic.** Autonomous by default does not mean opaque: if Chris asks
"what is a register?" or "why does `destroy` avoid `SCHED`?", answer properly, from the ground up,
and write it down.

## The rules that hold the codebase together

These come from `DECISIONS.md`. They are cheap to follow and expensive to retrofit.

1. **All architecture-specific code lives under `kernel/src/arch/`.** Assembly, `asm!`,
   system registers, CPU-specific behaviour. If you're writing `asm!` outside `arch/`, that
   is the bug. This is what makes the Raspberry Pi port a new directory instead of a diff
   across every file.

2. **A driver never reaches into a kernel global.** It gets what it needs passed in (a base
   address, later a DMA allocator, later an interrupt registration). See
   `drivers/pl011.rs`: it takes a base address and knows nothing else.

3. **The syscall surface stays narrow and explicit.** It is a boundary, not a habit.

5. **Architectural parity is a gate, not an aspiration** (DECISIONS §19). The targets are
   aarch64, riscv64, and x86_64 (declared, not yet started). A kernel capability ships on every
   supported architecture, proven by the same suite, or a scope note records the gap and the
   plan. If a feature works on one ISA and silently not another, that is the bug.

Rules 2 and 3 are what keep the microkernel option open. We are deliberately **not**
speculatively trait-ifying every subsystem, because that builds the wrong abstraction before
the requirements are known.

4. **Assume weak memory ordering.** We're on ARM, which is the weak one, and that's a gift:
   we cannot develop hidden strong-ordering assumptions the way an x86-first project would.
   Don't squander it.

6. **Taking a dependency is a decision, not a convenience** (DECISIONS §46). The tree's shape is
   thin architectural primitives (`aarch64-cpu`, `spin`, `tock-registers`) or whole subsystems we
   would never write (`smoltcp`, vendored RedoxFS), with **nothing in between**: thirty crates have
   no external dependencies at all. Write it if it is on the verification path, because you cannot
   restructure someone else's crate to make a model checker tractable. Vendor it if correctness is
   won by *exposure* rather than by reading the spec, which is why §46 says write the calendar and
   vendor the crypto.

## The syscall surface is a boundary, not a habit

Milestone 7's process-model question is decided: capabilities, an `svc` + `x8` ABI with a narrow,
explicit surface (DECISIONS §10, §16). The discipline that remains: the surface stays small and
every method is deliberate. New methods are fine within the established capability model (object
revocation added `Untyped::SPLIT` and `DESTROY` this way); **record each new method's semantics in
`DECISIONS.md`, not just in code.** A method that does not fit the model, or a brand-new syscall
number, is a design fork, raise it before building it.

## Testing

`script/test` (a thin wrapper over `cargo xtask test`) boots the kernel under QEMU and reports
pass/fail via semihosting. The `script/*` commands are the normalized "Scripts to Rule Them All"
front door (`setup`, `test`, `server`, `console`, ...); they delegate to `cargo xtask`, which is
still the engine and exposes more (`gdb`, `objdump`, `image`). See notes/scripts.md.

Tests should prove something specific that nothing else would have done for us. The four in
`main.rs` are the model: `.bss` was zeroed (nobody else would have), `sp` is 16-byte aligned
(a bug here is a mystery crash), we're at EL1 (we are where we think we are). Don't add
filler tests.

Pure logic (allocator algorithms, page-table math, scheduling policy, filesystem parsing)
belongs in crates that compile for the **host**, so most tests run in milliseconds without
an emulator.

## Commits

One purpose per commit. The message explains **why**, not what (the diff shows what). If a
commit records a correction or a surprise, say so in the message. See the milestone 1
history for the shape.

## Comments

The kernel is commented far more heavily than production code would be, deliberately. A
comment should explain a constraint the code can't show: *why* `sp` must be set before the
first `bl`, *why* `.bss` needs zeroing by hand, *why* the baud divisors are ignored by QEMU
but needed by a real Pi. Cross-reference the notes (`See notes/stack.md`) so the code and
the glossary stay stitched together.

Do not write comments that restate the next line.

## Style

Chris's global preferences apply, and they matter here because the notes are prose he'll
reread for months:

- No em-dashes. Use commas, periods, semicolons, or parentheses.
- No "delve", "comprehensive", "landscape", "moreover", "furthermore", "notably", "it's
  worth noting", "straightforward".
- No sycophantic openers, no filler conclusions that restate what was just said.
- Plain, direct language. Vary sentence length. Write like a person.

## Never leave QEMU running

A cricker-os kernel that has finished its work calls `arch::halt()`, which is `loop { wfi }`.
It never exits. So QEMU never exits either, unless something kills it or the kernel asks the
host to terminate via semihosting (which only the test build does).

Two consequences:

1. **Every interactive/demo QEMU run must be bounded** (see the note in Environment below).
2. `halt()` must use **`wfi`, not `wfe`.** QEMU implements `wfi` as a real vCPU halt and the
   host thread sleeps; it merely spins on `wfe`. A halted kernel using `wfe` burns **99.7% of
   a host core**. With `wfi` it is 0.0%.

## Environment

- macOS on Apple Silicon (itself aarch64, which is a nice coincidence: kernel assembly is
  the same ISA the laptop runs)
- QEMU via Homebrew, `qemu-system-aarch64`
- Rust nightly, pinned in `rust-toolchain.toml` (needed for `custom_test_frameworks`)
- Target: `aarch64-unknown-none-softfloat`
- `timeout(1)` does not exist on macOS, and **`perl -e 'alarm N; exec @ARGV'` DOES NOT WORK
  ON QEMU.** QEMU installs its own `SIGALRM` handler and swallows the alarm, so the process
  runs forever. This is not theoretical: it leaked eleven QEMU processes over one day of
  development, burning a combined 729% CPU, the oldest with eight hours of CPU time on it.

  Use `scripts/qemu-bounded.sh <seconds> <cmd...>` instead. It uses SIGTERM, which QEMU does
  honour, and it detaches the killer so it survives a pipeline whose reader (`head`) exits
  early.

  **After any session that ran QEMU, check `pgrep -x qemu-system-aarch64` and clean up.**
