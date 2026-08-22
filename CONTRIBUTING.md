# Contributing to nife

This file is for a **person** deciding whether to work on this project: what it wants, what it will
cost you, and what you can decide for yourself. GitHub links it from the pull request UI, which is
why it is here rather than folded into something longer.

It deliberately does not restate the rules. **The rules are in [`AGENTS.md`](AGENTS.md)**, which is
the project's constitution and is written for whoever is doing the work, human or agent. This file
tells you which parts of it you need before your first change, and in what order. Two copies of a
standard is how one of them goes stale.

## The shortest honest description

A capability microkernel for aarch64 and riscv64, in Rust, from the first instruction, built as a
demonstrator (DECISIONS §14) rather than as a product. The kernel allocates no memory of its own.
Every driver and server is a userspace process. The security-critical logic carries machine-checked
proofs. Architectural parity across both ISAs is a gate rather than an aspiration (DECISIONS §19).

It is a research project with one architect, so the response you get to a pull request is a real
person reading it between other things. Both halves of that are honest.

## Before you write anything

**Read [`AGENTS.md`](AGENTS.md).** It is about 50 KB and you do not need all of it to start, but you
do need these, and each one will otherwise cost you a rewrite:

- **Where architecture-specific code lives.** All of it is under `kernel/src/arch/`. `asm!` outside
  that directory is the bug, not the exception.
- **What goes in `crates/` versus `user/src/`.** Anything two binaries must agree on is a crate, never
  a `#[path]` module, because a shared module inside a `no_std` binary is unreachable by host tests
  and by Kani, and this project's whole method is pure logic in host-testable crates plus proofs.
- **Names are the architect's call.** Ship a provisional one and say so in your pull request; do not
  wait, and do not rename anything on your own initiative.
- **`DECISIONS.md` section numbers are assigned at merge**, never claimed by a branch. Put your
  reasoning in `notes/` and in the pull request instead.
- **The ladder.** When something must not go wrong, prefer making the wrong state unrepresentable
  over a gate that fails loudly, over a written record at the thing itself, over a note. "Somebody
  will notice" is not a mechanism.

The reading order for everything else is at the top of [`README.md`](README.md#start-here).

## Getting it building

```sh
git clone https://github.com/crickertech/nife
cd nife
script/setup     # installs the pinned Rust toolchain and QEMU, then builds
script/test      # host crates, then the kernel under QEMU on both ISAs
```

**On Linux, `script/setup` stops at the QEMU check and tells you to run `script/ci-qemu` first.**
That is the design working: no distribution ships a QEMU with `riscv-iommu-pci`, and the project
refuses to drop the device because a confinement test that quietly stops testing is worse than a red
build. Building the pinned QEMU takes about twelve minutes, once.

The full command reference is [`notes/scripts.md`](notes/scripts.md). The seven worth knowing on day
one are in the README.

## What kind of change is wanted

**Whole pieces.** A complete, correct, tested change that lands with its documentation is worth more
here than a large one that lands without. That is not politeness; the documentation is part of the
deliverable, because a demonstrator that nobody can read demonstrates nothing.

Concretely, a change is finished when:

- It has a test that proves something **specific that nothing else would have caught**. `.bss` was
  zeroed, `sp` is aligned, the era pivot happens at the right second. Filler tests are worse than no
  test, because they cost a reader's attention and buy nothing.
- Pure logic lives in a crate that compiles for the **host**, so it runs in milliseconds without an
  emulator, and so Kani can reach it.
- It works on **both ISAs**, or a scope note records the gap and the plan. A feature that works on
  aarch64 and silently not on riscv64 is the bug.
- Any limitation it has is written in a **`BUGS` section next to the feature**, not in a tracker and
  not left for the reader to discover. This is the convention the project reaches for hardest, and it
  is not modesty: a newcomer who hits a limitation the docs named will trust the docs, and one who
  hits a limitation the docs hid will not trust anything again.
- Anything measurable is **measured**. `script/bench` runs icount microbenchmarks against a committed
  baseline. An honest tie recorded plainly is worth more than an overclaimed win.

## What is not yours to decide, and why that saves you time

Bring these up rather than building them, because each one is expensive to unship and cheap to ask
about:

| | |
|---|---|
| **The syscall surface** | A boundary, not a habit. A new method inside the existing capability model is fine and gets its semantics recorded in `DECISIONS.md`; a new syscall number is a design fork. |
| **A new dependency** | Taking one is a decision (DECISIONS §46). The tree is thin architectural primitives or whole subsystems nobody would write, with nothing in between. |
| **Names** | Crates, programs, and shared modules are named by the architect. Ship provisional, say so. |
| **Anything two programs agree on** | A wire format, an opcode number, a packed word. The code is a morning's work; the un-shipping is not. |
| **`DECISIONS.md` section numbers** | Assigned at merge. |

Everything else you should simply decide and do. Most decisions here are reversible and deliberating
them costs more than getting them wrong.

## How to propose a change

```sh
git checkout -b fix/short-description        # or milestone/, feature/, roadmap/, decisions/,
                                             # toolchain/, ci/, bench/, integration/, audit/
# ...work, committing as pieces prove out...
script/gates                                 # the five checks a PR must pass
git push -u origin HEAD
gh pr create --draft                         # then mark it ready when the gates are green
```

`script/lint` refuses a branch prefix outside that set, so the first line saves you a red check.
**`script/gates` is the one command to remember**: it runs `script/fmt --check`, `script/lint`,
`script/icount`, `script/test` and `script/test --hvf` in that order, cheapest first, so a
formatting slip costs twenty seconds rather than the whole run. `--hvf` (the aarch64 suite on the
physical core) is the slowest stage and the one most likely to flake on a contended host; see
notes/load-sensitive-assertions.md if it does.

Pull requests land through GitHub's merge queue, which batches and rebases them, so you do not need
to keep your branch current by hand.

**One purpose per commit, and the message explains why rather than what** (the diff already shows
what). If a commit records a correction or a surprise, say so in the message: those are the most
useful commits in this history. `git blame` is the test. A reader tracing why a line looks the way it
does has to land on a commit that explains it.

**If your change is a design argument rather than code**, it goes in `design/decisions/` as a file
with `**Status: PROPOSED.**`, saying what is being decided, the options, the recommendation with its
reason, and what is blocked until it is answered. A decision that lives only in a pull request thread
is in the medium this project exists to get things out of.

## Where the arguments are

- [`design/decisions/`](design/decisions/README.md) is what was chosen, what was rejected, and why,
  **including the decisions that were refused**. That is on purpose: you can disagree with an
  argument, but not with an authority.
- [`design/roadmap/`](design/roadmap/README.md) is the only status in the tree. It has a fixed
  status vocabulary and a checker; anywhere else that claims status is stale by construction.
- [`notes/`](notes/README.md) is a glossary written while building, one file per question that
  turned out to be load-bearing.
- [`design/audit-reports/`](design/audit-reports/) is every audit, its lens, and when the next is due.

## Reporting a bug, and reporting a security bug

Ordinary bugs: open a GitHub issue, and say which **commit sha** you were on, because `main` moves.

Anything you believe lets a confined process escape goes through [`SECURITY.md`](SECURITY.md)
instead, privately. Escaping is the exact thing this project claims cannot happen, so that class of
report is the most valuable one it can receive.

## Licensing

Dual MIT / Apache-2.0, at your option. Unless you state otherwise, anything you submit for inclusion
is dual licensed the same way, with no additional terms. There is no CLA.

## BUGS

- **This file is not a substitute for [`AGENTS.md`](AGENTS.md), and following only this file will get
  your first pull request sent back.** It names the five rules that most often cost a rewrite; there
  are more, and some of them (the ladder, the record-versus-code distinction, the worktree hazards)
  are judgement rather than checklist. The split is deliberate: this file is for deciding whether to
  contribute, that one is for doing the work.
- **`AGENTS.md` is 50 KB and reads as a constitution rather than a manual**, which is a real cost to
  a first-time reader and is known. `CLAUDE.md` at the root is a symlink to it, kept so agent tooling
  keeps finding it; a human who opens `CLAUDE.md` and a human who opens `AGENTS.md` get the same
  file.
- **There is no issue template and no pull request template**, so nothing prompts you for the commit
  sha or for which ISAs you tested. Until there is, this file is the prompt.
- **The contribution path assumes you can run QEMU on both ISAs.** A change that only touches the
  host-testable crates does not, but nothing here tells you which crates those are without reading
  `script/test`.
- **Written 2026-08-18 by the third run of milestone 117 (the stranger test), which is an agent and not a person.**
  Every friction estimate in this file is therefore a lower bound: an agent does not get bored and
  reads further before asking than a human would.
