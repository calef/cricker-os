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

## The three roles, and the one rule that keeps work moving

Named 2026-08-04, after a night in which eleven agents shipped and the queue still went idle twice
because nobody's job was noticing. The roles were already real; only their names and the top-up rule
are new.

- **Maintainer.** One per session, the session itself. Briefs developers, gates and merges their
  work, mints anything global to the tree (`DECISIONS.md` sections, milestone numbers, names Chris
  has ratified), and keeps hygiene: prune the worktree, delete the branch, relink `cricker-dev`,
  leave no QEMU. Holds merge authority when Chris grants it. **Maintainer, not project manager**,
  because the name has to predict the authority: this role writes code, resolves conflicts and
  merges, and a coordinate-only reading of it would leave the tree unowned.
- **Developer.** A subagent executing exactly one milestone. Reports; never merges, never mints,
  never edits `DECISIONS.md`, `design/` or this file. Names anything new provisionally and says so.
- **A developer works in a lane**, and the lane is the isolation rather than the person: its own
  worktree, its own branch, one milestone, no visibility into the others. Two developers in one lane
  is the merge problem this vocabulary exists to prevent.
- **Steward.** Runs on an interval and holds a *lent* authority, which is what the name says: it
  merges what has earned it (green on every check, from a developer briefed this session, touching
  no syscall surface, no `DECISIONS.md` section and no dependency addition), cleans up behind
  finished work (delete the branch, prune the worktree, relink `cricker-dev`), reports queue depth
  against the target, and raises what has stalled or gone unanswered. It exists because the
  maintainer is structurally bad at noticing its own idleness: when it is busy, it is busy.

  **It does not brief developers**, because briefing is judgment and the good outcomes come from
  briefs that name the specific hazard (the sixteen-slot cspace, the claim to verify, the file
  another lane holds). A generic brief produces a worse lane than an idle slot costs. So the
  steward says "the queue is at one of three and these are ready" and the maintainer writes it.

  **It watches for work at risk**, not only for idleness: a lane worktree with modifications and no
  commit in half an hour is uncommitted work one prune away from gone, which is the only failure in
  this system that destroys rather than delays. That check earns its keep more than the idle one.

  **It must never hold the main checkout while a developer's gate is running**, which is the race
  that took the `cricker-dev` link out from under a lane on 2026-08-04. `caretaker` and
  `undertaker` were unavailable as names: this tree already spends both on capability-narrowing
  programs.

**The top-up rule, which is the whole point.** When a developer finishes, the maintainer **launches
the next work before writing the report**. Not after, and not when Chris next asks. A conversation
with Chris never blocks the queue; answering a question and keeping lanes full are concurrent, and
the failure mode is always the same, which is that the answer feels like progress and the idle
machine is invisible. Maintain the agreed number of concurrent developers, and if the ready queue is
empty, say so as its own finding rather than letting the silence stand for "nothing to do".

**A developer's final report ends by handing off**: what its work unblocked, and what it found that
wants a lane of its own. That is the same discipline as milestone 94's, applied to scheduling rather
than to findings.

**Open decisions live in a file, not in a conversation.** A decision waiting on Chris that exists
only in chat scrollback is in exactly the medium milestone 94 was written to abolish, and on
2026-08-04 five of them accumulated there in one day while that milestone was being built. They go
in `design/open-decisions.md` (name provisional), one entry each: what is being decided, the
options, the recommendation with its reason, and what is blocked until it is answered.

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

**And the instruction "do not run `xtask std-src`" is impossible for a lane that must gate**, which
milestone 57's lane found on 2026-08-01 by reading the code rather than by failing. `script/test`
calls `std_src()` transitively, and a fresh worktree always has a cold farm, so **any lane that runs
the gate takes the account-wide link.** Two instructions this file gave together could not both be
obeyed.

Until `xtask test` grows a flag that skips the farm, the honest rule for the integrator is: **expect
every lane to take `cricker-dev`, and relink from the main checkout at merge**, in the same breath as
pruning the worktree. Do not tell a lane not to do the thing gating requires; tell it what to say in
its report so the relink is not forgotten. That lane also demonstrated the workaround worth knowing:
symlink the worktree's `target/cricker-farm` at the main checkout's farm after checking the stamps
match (`cargo xtask std-stamp`), and `std_src()` early-returns instead of rebuilding.

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
   layout and constants **deliberately written twice**, once in Rust and once in `user/c/c_seam.c`,
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

**Shared modules are in scope for a reason.** `user/src/` used to hold 48 `[[bin]]` programs and a
handful of modules compiled into them with `#[path = "..."] mod ...`, with **nothing in the naming
distinguishing them**, so a reader who tried to run `cseam` was misled by the directory. Rule 7
retired that category on 2026-08-01: what two binaries share is a crate, and what remains in
`user/src/` beside the programs is single-consumer submodules (`vnet`, `netcli`), which are ordinary
Rust. `script/lint` now counts consumers per `#[path]` target and fails at two.

The count in an earlier draft of this paragraph said "three modules" and was wrong: the grep that
produced it matched only single-line includes, and several were two lines. Take a count from the
merged tree, with a pattern you have checked against the real shapes. A shared module's name has to answer a question a program's
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

**Name things with nouns** (Chris, 2026-08-01). A crate, a program or a module is a *thing*, so it
takes the name of a thing: `capability`, `grant_plan`, `user_heap`, `video_terminal`, `line_editor`,
`fs_subtree_caretaker`. A verb names an action and a namespace is not one, which is audible at the
call site: `line_edit::expand_output` reads as an instruction where `line_editor::expand_output`
reads as a location.

The exception is a **term of art that happens to be a verb**, where the word is the one the field
already uses. `bind` (§50) is Plan 9's, and respelling it as a noun would assert novelty where there
is none. That is the paragraph above, not a hole in this one.

Three crates predated this rule and were settled by it on the day it was written: `compose` becomes
`compositor`, `measure` becomes `measured_boot`, and `dma_validate` becomes `dma_validator`. Each had
named itself a noun in its own first line while carrying a verb as its name.

**A crate and a program may share a name, and it says something when they do**: the crate is that
program's logic, lifted out so it can be host-tested and Kani-reachable while the program keeps the
IO. `coremark`, `line_editor` and `compositor` are all this pair, and splitting the names would hide
a relationship worth seeing.

### The convention: one rule per domain, and each domain's own

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

**But `snake_case` is the rule for Rust things, not for everything**, and an earlier draft of this
section said "everywhere" and was wrong. Three domains, each keeping its own convention:

| Domain | Form | Because |
|---|---|---|
| Crates, programs, modules | `snake_case` | Rust's own convention, and what the tree already does |
| `script/` and `scripts/` entry points | `hyphens` | shell commands are hyphenated everywhere (`apt-get`, `pkg-config`, `docker-compose`); an underscore in a command name reads as a mistake |
| Ordinary markdown (`notes/`, `design/`) | `hyphens` | filenames become URL slugs in every static site generator, and hyphens are word separators in a URL where underscores are joiners |
| Repo-root markdown | `SCREAMING_SNAKE_CASE` | **GitHub behaviour, not style.** It recognises `README.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, `CONTRIBUTING.md` and links them in its UI; get the name wrong and the Security tab does not find your policy |
| A directory holding a Rust package | named **exactly as the package**, so `snake_case` | the directory and the package are one thing with one name; thirteen under `crates/` already do this |
| Any other directory | `hyphens` if it needs two words | a directory is a path element, and paths are hyphenated outside this repository |

The directory rows are the same principle one level out, not a new tier: a package directory is a
Rust name, and everything else is a path. Three directories violated them when this was written
(`fs-server/`, `tools/redoxfs-host/`, and `user-std/`, whose package was called `hellostd` and
matched neither); **milestone 63 fixed all three on 2026-08-01**, along with about twenty other
names. The rule is now descriptive of the tree rather than aspirational.

**This is not the two-tier rule Chris rejected**, and the difference is the one he identified. That
split was *within* one domain, keyed on an **unstable** property: `wc` moved from internal plumbing
to prompt-typed pipeline stage inside a day. These splits are *across* domains on a **stable**
property. A file either is a Cargo target or is an executable in `script/`; `script/test` will never
become a `[[bin]]`.

It is also the same guard rail as "standard terms are already right", applied to **form** rather than
vocabulary. We do not rename `elf`, and we should not respell `supply-chain` either: a name whose
shape a reader already knows from outside costs them nothing.

**One constraint to know:** `crickerfs` caps archive names at `NAME_LEN = 32` bytes, raised from 24 on
2026-08-01 so `os_primitives_benchmarker` would fit. It can be raised again, and there is no data
migration because every image regenerates from that crate, but it costs directory entries per block.
(The old warning that it also costs kernel stack was stale: `Fs` stopped holding entries as a fixed
array when the FS-server stack bug was fixed. See notes/crickerfs.md.) Do not let it pick a name; do
not spend a format change on bytes nothing needs.

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

**Commit early and push, then curate before reporting.** These two rules read as opposites and are
not, and the resolution is a criterion rather than a compromise: **`git blame` is what a commit is
for.** A reader tracing why a line looks the way it does must land on a commit that explains it.

So while working, commit whenever a piece works and push whenever a commit exists, because a pushed
branch survives a dead session, a killed process and a laptop that will not wake, and nothing else
does. On 2026-08-04 a lane sat on seven modified files with **zero commits for hours**; had that
worktree been pruned the work was gone, and it was caught by inspection rather than by any
mechanism. Uncommitted work in a lane worktree is the one thing no part of this system protects.

Then, before reporting, **squash the checkpoints into the purposes** and force-push. A checkpoint is
for the lane's own safety and has no reader; a purpose commit has one.

**Never squash across purposes, and never squash-merge a branch.** Milestone 96's lane put the
loader unification in its own commit *ahead of* the migration precisely so that a boot failure could
not be ambiguous between two changes, which is the whole reason that structure exists. A
squash-merge would have destroyed it. The merge commit carries the pull request's title, so
`git log --first-parent` already reads as one entry per piece of work while the detail stays
reachable underneath.

The exceptions worth keeping unsquashed: a commit that records a correction or a surprise, and a
commit whose separateness is itself the argument (96's loader, above).

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

  **That check is not sufficient after you kill a harness, and on 2026-08-02 it took four attempts
  to notice.** Killing a loop script does not kill its descendants: `pkill -f hunt-...` left
  `cargo xtask test` running, which kept starting fresh QEMUs. So every check honestly reported "no
  qemu" and the next command found one holding `target/crickerfs.img`, which then failed unrelated
  test runs with `Failed to get "write" lock` and looked like a bug in the code under test.

  Two habits fix it. **Ask who holds the file, not whether a process matches a name**:
  `lsof target/crickerfs.img` names the holder even when your pattern does not. And **kill the tree
  at its root**: walk `ps -o pid,ppid,command` up to the harness and kill that, or the loop simply
  starts another child. `pgrep -l qemu` is also worth preferring to `pgrep -x qemu-system-aarch64`,
  because it matches both architectures and does not depend on getting the full name right.
