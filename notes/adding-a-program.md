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

**Write `provisional` when you expect the name to change**, which is AGENTS.md's word and, since
§89 (2026-08-16), the gate's too. Four states:

```
Name: ratified 2026-08-04 (calef, milestone 63). Refused `x` (why).
Name: recorded (milestone 46). <what the tree already argues, and where>
Name: provisional. <what you called it and why you expect it to change>
Name: unrecorded. <what the history does and does not say>
```

`provisional` is a claim about **intent** (you expect this to change); the other three are claims
about the **record**. A settled name can be `unrecorded` (nobody wrote down why `hello` is called
`hello`, and nobody needs to), so the two are not the same word for the same thing.

`script/names --provisional` lists them and they sort first in `--unratified`, because a name its
own author called wrong is the shortest conversation calef can have. This page told newcomers the
opposite until §89: run 2 of the stranger test wrote the word AGENTS.md asked for and got a red
gate, which is what raised the decision.

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

**Three hand-maintained lists, in three different shapes, and skipping any of them breaks something
different.** This page said "two" until run 2 walked it and found the third.

- `mkinitrd()` for aarch64: **a line in the `for name in [ ... ]` list.** There is also an older tier
  of hand-written `let name = match read_stripped(...)` blocks, one per program, which is what this
  page used to send you to write; a new program does not need one, and following the old advice
  costs you eight lines of boilerplate the tree stopped needing.
- `initrd_riscv()` for riscv64: **two edits, not one.** A `"--bin", "your_program",` pair in the
  `cargo build` argument list at the top of the function, **and** a `("your_program",
  "your_program")` row in the `entries` table below it. The table reads an ELF that only the `--bin`
  list causes cargo to build, so half the edit fails the build with `mkinitrd: cannot read
  .../your_program: No such file or directory`.

That last trap is not hypothetical, and the file carries its own scar about it: the credential pair
(milestone 56) sat in the riscv tables while nobody added them to the `--bin` list, so a clean tree
could not build them, and the lane's own riscv leg went green on a stale binary its target directory
still held.

There is no reason for the asymmetry beyond history. If you find yourself wishing it were one list,
you are right, and that is worth a milestone rather than a drive-by.

### 5. Keep the name under 32 bytes

`nifefs` caps `NAME_LEN` at 32, raised from 24 so `os_primitives_benchmarker` would fit. Raising
it again costs directory entries per block, so do not let a name spend it.

### 6. If the shell should be able to spawn it: a `Prog` variant

In `crates/grant_plan/src/lib.rs`, **six edits**, not the four this page used to list:

1. the `Prog` variant itself;
2. `from_name()`, which is how the shell resolves what you type. Without it the program is in the
   archive, loadable, and unreachable from the prompt, which looks like the program being broken
   rather than unlisted;
3. `name()`;
4. `id()`, the **stable wire id**;
5. `from_id()`;
6. **`PROG_COUNT`**, which this page never mentioned and whose own doc comment says forgetting it is
   "an out-of-bounds panic in init rather than a compile error".

**The wire id is the expensive part.** It is a thing two programs agree on, which CLAUDE.md classes
as hard to reverse: the shell sends it and init decodes it, so changing one later is a flag day. The
code around it is cheap; the number is not.

**Then expect the build to fail in a crate you did not edit**, and expect that to be the design
working:

```
error[E0004]: non-exhaustive patterns: `Prog::Doubler` not covered
   --> crates/swish/src/lib.rs:785:11
```

The shell must say how your program's answer renders, so the compiler asks. Add the arm.

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
cargo xtask build    # ~20s, and it is what packs both archives
script/lint          # the name block, the conventions, the host pass
script/test          # both ISAs, which is where a missed riscv initrd entry surfaces
script/shell-check   # if the shell spawns it
```

If the shell spawns it, add a line to `SHELL_CHECK_SCRIPT` in `xtask/src/main.rs` and bump the
array length the compiler asks for: `("doubler 21", Some("21*2 = 42")),`.

**Then run it once with a deliberately wrong expectation.** A green harness only proves the harness
did not complain; a red one proves your program was really loaded from the archive, measured,
granted its endpoint and run at EL0:

```
$ doubler 21
  a process at EL0 computed 21*2 = 42
--- shell-check (aarch64) FAILED ---
  `doubler 21` answered "a process at EL0 computed 21*2 = 42", wanted "21*2 = 43"
```

## BUGS

- **Nothing gates the two initrd lists against each other.** A program in `mkinitrd()` and not in
  `initrd_riscv()` builds, boots on aarch64, and is simply absent on riscv64. The parity gate catches
  it only if a test names the program.
- **This page is prose and the code can move without it.** The step that rots first is the manifest
  field list, which is why it is not repeated here: [program-manifest.md](program-manifest.md) has it,
  and the struct in `crates/grant_plan/src/lib.rs` is the authority over both.
- **Written from having done it, once, on 2026-08-16.** It began as a second-hand account of a
  first-hand guess: reconstructed after milestone 117's first stranger reconstructed it, and its own
  BUGS section asked the first person to add a program against it to correct whatever it got wrong.
  Run 2 did exactly that, adding a program called `doubler` and getting it answering at the prompt on
  both ISAs, and the page was wrong in four places: the aarch64 tier, the riscv `--bin` list, two of
  the six `grant_plan` edits, and the `provisional` spelling the gate rejects. Those are fixed above.
  **One walk-through is not a guarantee**, and the next person to add a program should treat a
  surprise here as this page's bug rather than their own.
- **One fact is written in five places** and nothing joins them: two initrd lists in different
  shapes, a `--bin` list, a six-part `Prog` table, and an exhaustive match in the shell. Steps 4 and
  6 are long because the tree is, not because adding a program is hard.
