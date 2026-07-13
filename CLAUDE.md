# Working on cricker-os

## What this project is

A hobby operating system for aarch64, in Rust, built from the first instruction. **It is a
learning project.** Chris is an experienced software engineer and engineering leader, but
new to OS internals and early in Rust (through ~chapter 5 of the book). The point is for
him to understand how operating systems work, not to ship a product.

That single fact should drive most of your judgment calls. **Velocity is not the goal.
Understanding is.** A fast solution that leaves him unable to explain his own kernel is a
failed solution.

## How to work

**Write code together, explaining as you go.** Chris chose this mode explicitly. Write
working code, then explain the reasoning, the alternatives you rejected, and what the
hardware is actually doing. He reads, questions, and redirects.

**Stop and explain whenever he asks, however basic the question.** He has interrupted
mid-build to ask "what is a register?" and "what is the stack?" That is the project working
as intended, not a detour from it. Answer properly, from the ground up, without
condescension.

**Then write it down.** Every concept that comes up gets a note in `notes/`, indexed in
`notes/README.md`. The glossary is written *while* building, not afterward. If you explain
something substantial in chat and don't capture it, you've lost it.

**Push back when he's wrong, with a technical reason.** He picked async/await for the
execution model and said it "sounded more tractable." The correct response was not to
comply. It was to point out that cooperative scheduling *cannot run an arbitrary ELF
binary* (it has its own stack, never yields, and will loop forever), so async doesn't defer
the hard work, it forecloses it. He changed his mind and thanked us for it. Do that again
when it's warranted. Do not cave to be agreeable, and do not manufacture disagreement to
seem rigorous.

**Correct yourself loudly.** We told him QEMU passes a device tree pointer in `x0`. It
doesn't. We found out by printing it and getting zero, and we fixed the note rather than
quietly patching over it. Both the README and `notes/portability.md` record the error on
purpose. The machine overrules the documentation, and it overrules us.

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

Rules 2 and 3 are what keep the microkernel option open. We are deliberately **not**
speculatively trait-ifying every subsystem, because that builds the wrong abstraction before
the requirements are known.

4. **Assume weak memory ordering.** We're on ARM, which is the weak one, and that's a gift:
   we cannot develop hidden strong-ordering assumptions the way an x86-first project would.
   Don't squander it.

## Milestone 7 is a hard decision point

The process model (Unix-like with fds and fork/exec, vs. capability-based like seL4) is
**deliberately undecided**. Milestones 1-6 don't touch the syscall boundary, so the deferral
is free until it isn't.

**If you find yourself hacking in a syscall without having had that conversation, the plan
has failed.** Stop and raise it.

## Testing

`cargo xtask test` boots the kernel under QEMU and reports pass/fail via semihosting.

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
