# Adding a user program

Task-oriented, because milestone 117's first stranger run found that **no file described this**. It
reconstructed the steps from `xtask`, `user/Cargo.toml` and `grant_plan`, said it expected to have
got one wrong, and was right to expect that: the two initrd lists are easy to half-do.

A program is a `[[bin]]` in `user/`, running at EL0, linked against `user_rt`.

## The steps

### 1. The source

`user/src/<name>.rs`, `snake_case` (DECISIONS §39, and the convention table in
[naming.md](naming.md)). `no_std`, against `user_rt`.

### 2. A provenance block in its module doc

```rust
//! Name: unrecorded. Introduced 2026-08-14 for <what it does>.
```

**`script/lint` fails without one**, via `script/names --check`. Three states: `ratified` (calef
ruled, with the date and what was refused), `recorded` (the tree argues the name somewhere, with a
citation), `unrecorded` (nothing outside this block says why). **The gate checks presence, never
`ratified`**, so an unratified name never blocks a build and `unrecorded` is a truthful answer.

**The name is calef's** (AGENTS.md, "calef names the crates, the programs, and the shared modules").
Ship a provisional one, say so in your report, expect it to change.

### 3. A `[[bin]]` block in `user/Cargo.toml`

```toml
[[bin]]
name = "your_program"
path = "src/your_program.rs"
test = false
bench = false
```

`test` and `bench` off are **mandatory**, not tidiness: the default libtest harness needs
`extern crate test`, which does not exist for a bare-metal target.

### 4. Pack it into both initrds, in `xtask/src/main.rs`

**Two hand-maintained lists, in two different shapes, and skipping the second silently breaks a
parity gate (§19).**

- `mkinitrd()` for aarch64: one `let` block per program.
- `initrd_riscv()` for riscv64: an entry in the `entries` table.

There is no reason for the asymmetry beyond history. If you find yourself wishing it were one list,
you are right, and that is worth a milestone rather than a drive-by.

### 5. Keep the name under 32 bytes

`crickerfs` caps `NAME_LEN` at 32, raised from 24 so `os_primitives_benchmarker` would fit. Raising
it again costs directory entries per block, so do not let a name spend it.

### 6. If the shell should be able to spawn it: a `Prog` variant

In `crates/grant_plan/src/lib.rs`: a `name()`, a `from_id()` arm, a **stable wire id**, and a
`manifest()`.

**The wire id is the expensive part.** It is a thing two programs agree on, which CLAUDE.md classes
as hard to reverse: the shell sends it and init decodes it, so changing one later is a flag day. The
code around it is cheap; the number is not.

## What you declare: the manifest

The manifest is the program's endowment, and the shell checks it **at the prompt, before a child
exists**. A mismatch is a legible refusal on the line you typed rather than a hang deep inside a
program that assumed a slot was full. See [grant-expression.md](grant-expression.md) and
[program-manifest.md](program-manifest.md).

**The manifest declares the direction; the command line designates the file.** Whether a program
writes is a fixed, publishable property of it. Which file it touches is the caller's business. So
`wc report.txt` reads and `tee report.txt` writes, and nobody types a mode.

**A manifest is as much about refusal as need.** `date`'s row is `Forbidden` throughout, so a memory
grant aimed at a clock reader stops at the prompt.

## Check your work

```sh
script/lint          # the name block, the conventions, the host pass
script/test          # both ISAs, which is where a missed riscv initrd entry surfaces
script/shell-check   # if the shell spawns it
```

## BUGS

- **Nothing gates the two initrd lists against each other.** A program in `mkinitrd()` and not in
  `initrd_riscv()` builds, boots on aarch64, and is simply absent on riscv64. The parity gate catches
  it only if a test names the program.
- **This page is prose and the code can move without it.** The step that rots first is the manifest
  field list, which is why it is not repeated here: [program-manifest.md](program-manifest.md) has it,
  and the struct in `crates/grant_plan/src/lib.rs` is the authority over both.
- **Not written from having done it.** This was reconstructed after a stranger reconstructed it,
  which means it is a second-hand account of a first-hand guess. The first person to add a program
  against this page should correct whatever it gets wrong.
