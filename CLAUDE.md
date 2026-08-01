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

**Some shared state is global to the *machine*, not the repo, and `rustup toolchain link` is the one
that has bitten.** The `cricker-dev` toolchain the `std` farm needs is a symlink in
`~/.rustup/toolchains`, so `xtask std-src` repoints a **user-account-wide** name at whichever
worktree ran it last. Two lanes building the farm race for it, and the loser silently compiles
against a farm inside someone else's worktree; deleting that worktree then breaks the toolchain for
everything, surfacing far from the cause as "override toolchain 'cricker-dev' is not installed"
during an unrelated build. Fix: `rustup toolchain link cricker-dev "$(pwd)/target/cricker-farm"` from
the main checkout. This is the same rule as the paragraph above, one level out: the integrator owns
what is shared, and "shared" is wider than this repository.

**Delete a lane's worktree too, and do it before the disk decides for you.** On 2026-07-31 the data
volume hit **zero bytes free** with 42 agent worktrees holding **78 GB**. Two lanes died mid-work and
could not even run `pgrep` to check whether they had leaked emulators, because every tool must create
an output file before it runs. Deleting the branch at merge does not remove the ~2 GB of `target/`
behind it, so **prune the worktree in the same breath**, and `git worktree prune` afterwards. If a
lane is blocked, commit and **push** its work before removing anything: a snapshot on the remote
cannot be lost by a cleanup. The warning signs were noted hours earlier and not acted on, and then
four more lanes were launched on top of them.

**Delete a lane's branch when you merge it, and never use a branch as a filing cabinet.** Forty-seven
branches accumulated in about two days of lane work and had to be pruned by hand on 2026-07-31; this
recurs by default, because merging is what finishes a lane and deleting is a separate act nobody is
prompted to take. So it belongs in the merge, not in a periodic cleanup.

The rule that matters more than the tidiness: **an unmerged branch is either abandoned or it is
holding knowledge that is not on `main`, and the second case is a bug in where the knowledge lives.**
`fix/redoxfs-write-loop` survived that prune because it carried an investigation's conclusion that
`notes/fs-server.md` does not. **Nobody reads branches.** If a branch holds a finding worth keeping,
land the finding in `notes/` and then delete the branch; do not keep the branch as the record.

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

Rules 2, 3 and 7 are what keep the microkernel option open (7 because a contract you cannot
test is a contract you cannot trust to replace a component behind). We are deliberately **not**
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

7. **Anything two binaries must agree on is a crate, never a `#[path]` module** (Chris, 2026-08-01).
   If a constant, an opcode, a layout, or an error code is shared by more than one program, it goes
   in `crates/` and is depended on. `#[path = "x.rs"] mod x;` is not an option.

   **Three reasons, and the second is the one that matters.**

   It removes a category that nothing enforces. A `#[path]` module is neither a program nor a crate,
   so a reader meeting `cseam::GRANT_VA` cannot tell what they are looking at, and `user/src/` held
   48 programs and 3 modules with nothing distinguishing them.

   **A `#[path]` module inside a `no_std` binary is unreachable by host tests and by Kani.** This
   project's entire method is pure logic in host-testable crates plus machine-checked proofs, and a
   shared module opts out of both. `cseam` is the case that proves it: it holds the address-space
   layout and constants **deliberately written twice**, once in Rust and once in `user/c/cseam.c`,
   with nothing checking that the two agree. A drift there shows up as a C component scribbling on
   the wrong page, arbitrarily far from the edit.

   And it makes location self-enforcing for free. Once shared definitions live in `crates/`,
   everything in `user/src/` is a program, with **no files moved** and no convention to remember.

   This was already the tree's practice for seven crates (`fs_proto`, `sink_proto`, `cred_proto`,
   `clock_proto`, `entropy_proto`, `ntp_proto`, `gfx_proto`) and the exceptions had no recorded
   reason; `cseam.rs`'s header describes the `#[path]` mechanism without ever justifying it.

## Chris names the crates, the programs, and the shared modules

**The name of a crate, a program, or a shared module is Chris's call, not a lane's and not yours**
(2026-08-01). This is the same rule as `DECISIONS.md` section numbers, one level up: it is global to
the tree, so it is decided by the person who can see the whole tree.

**Shared modules are in scope for a reason.** `user/src/` holds 51 files: 48 are `[[bin]]` programs
and three are modules compiled into other programs with `#[path = "..."] mod ...` (`cseam.rs`,
`suptree.rs`, `swap.rs`). **Nothing in the naming distinguishes them**, so a reader who tries to run
`cseam` has been misled by the directory. A shared module's name has to answer a question a program's
name never raises, which is *"where does this get compiled into?"*, and that makes it a naming problem
of its own rather than a smaller version of the program one.

The reason is his: **names are what make this OS accessible to humans and to LLMs.** A reader meets a
name before they meet anything else, and in a capability system the name is often the only thing that
says what a program can *do*. `DECISIONS.md` §39 already says a name is a claim; this says who gets to
make the claim.

The evidence that it needed a rule is the tree itself. `dwarden` is named for what it **holds** while
its two siblings are named for what they **serve**, so a reader who correctly infers the scheme gets
it wrong. `conx` has no recorded expansion anywhere: not in §41, not in `notes/live-replacement.md`,
not in the commit that introduced it. `cseam.rs` sits among 48 programs and is not one; it is a shared
module. Every one of those was a locally reasonable choice by whoever was mid-task.

**How to work it.** Propose names with what each thing actually does, and wait. A lane that needs a
new program or module ships a **provisional** name, says so in its report, and expects it to change;
the integrator surfaces it. Never rename on your own initiative either, because a rename is a naming
decision with extra steps.

**Crates are in scope too** (extended 2026-08-01). They are the most reader-facing names in the tree:
a newcomer greps `crates/` before they ever open `user/src/`, and a crate name appears in every
`Cargo.toml` that depends on it, in every `use` statement, and in the dependency graph an outsider
reads to understand the shape of the system.

The crate names have the same three failure modes the programs did. **Abbreviations** that need a
decoder (`capsh`, `uheap`, `vt`). **Generic words** that could name almost anything in an operating
system (`compose`, `measure`, `regions`, `slots`, `caps`, `frames`). And **standard terms that are
genuinely right** and should not be touched (`elf`, `pci`, `dtb`, `gpt`, `ipc`, `paging`, `glob`,
`asid`), because renaming those would cost a reader the recognition the whole tenet exists to buy.
That last group matters: this rule is not a licence to rename everything, and a name a reader already
knows from outside this project is the best name available.

### The convention: `snake_case`, one rule, everywhere

Crates already do this (`fs_proto`, `cred_proto`, `user_rt`). Programs did not: **0 of 57** used an
underscore, so multiword names were squished (`fsclient`, `sysinit`, `credcli`).

An earlier draft of this rule had two tiers, short names for programs a user types and underscores for
programs only the system spawns. **Chris rejected it, correctly**: the category is not a stable
property of a program. `wc` was internal plumbing and became a prompt-typed pipeline stage in one day,
and a convention keyed to something that changes produces renames. It is also not how Unix got its
names; the terseness of `ls` is an emergent pressure on words people type constantly, not a rule
anyone wrote down. Codifying an emergent property turns it into a classification chore that every
contributor has to get right.

So: one rule, no branch to get wrong. A short name for a typed command is then a *choice its author
makes*, not a convention to apply, and nobody needs a rule to know `wc` beats `word_count`.

**One constraint to know:** `crickerfs` caps archive names at `NAME_LEN = 24` bytes. It can be raised,
and there is no data migration because every image regenerates from that crate, but it costs
directory entries per block and kernel stack (`Fs` holds entries as a fixed array on the boot and
spawn paths). Do not let it pick a name; do not spend a format change on bytes nothing needs.

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
