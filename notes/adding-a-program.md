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

**Three hand-maintained lists, and skipping any of them breaks something different.** Two of the
three are now the same shape, which is a change from what this page said before 2026-08-18:

- `mkinitrd()` for aarch64: **one `("your_program", "your_program")` row in its `entries` table.**
  The pair is `(archive_name, bin_name)` and they differ exactly once in the whole table, for `init`.
  Nothing else on this side: the aarch64 archive builds the whole `user` package, so there is no
  per-program build list to extend.
- `initrd_riscv()` for riscv64: **two edits, not one.** A `"--bin", "your_program",` pair in the
  `cargo build` argument list at the top of the function, **and** the same `("your_program",
  "your_program")` row in the `entries` table below it. The table reads an ELF that only the `--bin`
  list causes cargo to build, so half the edit fails the build with `mkinitrd: cannot read
  .../your_program: No such file or directory`.

That last trap is not hypothetical, and the file carries its own scar about it: the credential pair
(milestone 56) sat in the riscv tables while nobody added them to the `--bin` list, so a clean tree
could not build them, and the lane's own riscv leg went green on a stale binary its target directory
still held.

**What changed, and why this page was wrong twice about the same function.** It used to send you to
a `for name in [ ... ]` list and warn you off an older tier of hand-written
`let name = match read_stripped(...)` blocks. **Milestone 130 deleted both on 2026-08-17**, replacing them
with the one table and one loop `initrd_riscv()` had always had, so the only asymmetry left is the
`--bin` list. Run 2 corrected this page on 2026-08-16 and 130 falsified it the next day; see this
page's `BUGS`, because the recurrence is the finding rather than the accident.

**You do not touch the measurement table.** init refuses to spawn a program its measurement manifest
does not vouch for, and a reader who meets that refusal reasonably wonders where to register a new
one. Nowhere: `xtask` hashes every entry of the archive it just packed and writes the manifest from
that (`write_measure_manifest`), so the table follows the archive by construction.

### 5. Keep the name under 32 bytes

`nifefs` caps `NAME_LEN` at 32, raised from 24 so `os_primitives_benchmarker` would fit. Raising
it again costs directory entries per block, so do not let a name spend it.

### 6. If the shell should be able to spawn it: a `Prog` variant

In `crates/grant_plan/src/lib.rs`, **seven edits**, not the six this page listed until 2026-08-18
and not the four before that:

**In this order**, which is not the order they appear in the file. The first four are the ones
nothing forces, and doing them first is what makes the last three fall out of a failing build:

1. the `Prog` variant itself;
2. **`PROG_COUNT`**, widened, and the table below says why this one goes second rather than last;
3. `from_id()`;
4. `from_name()`, which is how the shell resolves what you type. Without it the program is in the
   archive, loadable, and unreachable from the prompt, which looks like the program being broken
   rather than unlisted;
5. `name()`;
6. `id()`, the **stable wire id**;
7. **`manifest()`**, which carries all of the actual meaning: what the shell must grant your program
   and what it must refuse it. See "What you declare" below.

**The wire id is the expensive part.** It is a thing two programs agree on, which CLAUDE.md classes
as hard to reverse: the shell sends it and init decodes it, so changing one later is a flag day. The
code around it is cheap; the number is not.

**Then expect the build to fail in a crate you did not edit**, and expect that to be the design
working:

```
error[E0004]: non-exhaustive patterns: `Prog::Triple` not covered
   --> crates/swish/src/lib.rs:864:11
```

The shell must say how your program's answer renders, so the compiler asks. Add the arm.

#### Which of the seven the machine will remind you about, and which it will not

**Measured on 2026-08-18 by adding a variant and building after each edit**, because a list that
tells you what to do says nothing about what happens if you do not.

| edit | what happens if you skip it |
|---|---|
| `name()`, `id()`, `manifest()` | **compile error**, all three at once, `E0004` in `grant_plan` itself |
| the `swish` render arm | **compile error**, `E0004` in a crate you did not edit |
| `from_id()` | a host test fails, `init indexes slot N and no program claims it`, **but only if `PROG_COUNT` moved** |
| `from_name()` | a host test fails, `left: None, right: Some(YourProg)`, **same condition** |
| `PROG_COUNT` | **nothing at all** |

**`PROG_COUNT` is the keystone, and forgetting it hides the other two.** The sweep in
`prog_id_round_trips` counts up to that constant, so a variant whose id is past it is a variant the
sweep never reaches, and the guard beside it (`from_id(PROG_COUNT)` answers `None`) passes *because*
you forgot. A tree with the variant, the three forced arms, the `swish` arm and nothing else
**compiles and passes every host test**, and the failure arrives later as a program that cannot be
spawned from the prompt. Widen `PROG_COUNT` and the same test immediately names both missing arms,
one after the other.

That is why the list above is ordered the way it is: widen `PROG_COUNT` early and
`cargo test -p grant_plan` tells you what is still missing. Widen it last and there is nothing left
to tell you.

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
cargo xtask build    # the aarch64 archive ONLY, which is the trap below
script/lint          # the name block, the conventions, the host pass
script/test          # both ISAs, and it builds the riscv archive
script/shell-check   # if the shell spawns it: also both ISAs, and much faster than the suite
```

**`cargo xtask build` does not pack both archives**, whatever the name suggests, and this page
claimed it did until 2026-08-18. It runs `mkinitrd()` and stops; `initrd_riscv()` is called by
`test()` and by `shell-check` and by nothing else, so after a green `cargo xtask build` the file
`target/initrd-riscv.img` may not exist at all. **A step-4 mistake on the riscv side is invisible to
it.** If the shell spawns your program, `script/shell-check` is the cheapest thing that catches one:
it builds both archives and boots both prompts, and it does not run the kernel suite.

If the shell spawns it, add a line to `SHELL_CHECK_SCRIPT` in `xtask/src/main.rs` and bump the array
length the compiler asks for. The element is a `(&str, &[&str])` pair, one line typed and the
substrings its answer must contain: `("triple 21", &["21*3 = 63"]),`.

**Then run it once with a deliberately wrong expectation.** A green harness only proves the harness
did not complain; a red one proves your program was really loaded from the archive, measured,
granted its endpoint and run at EL0. Verbatim from a run of this page on 2026-08-18:

```
$ triple 21
  a process at EL0 computed 21*3 = 63
--- shell-check (aarch64) FAILED ---
  `triple 21` answered "a process at EL0 computed 21*3 = 63", wanted "21*3 = 64"
```

## BUGS

- **Nothing gates the two initrd lists against each other.** A program in `mkinitrd()` and not in
  `initrd_riscv()` builds, boots on aarch64, and is simply absent on riscv64. The parity gate catches
  it only if a test names the program.
  **The two commands you will run first are both blind to it**, which run 4 measured: such a program
  passes `cargo xtask build` *and* `script/lint`, and is caught first by `script/shell-check` or
  `script/test`, both of which cost an emulated boot. **And nothing counts programs**, so the
  suite total is identical with and without one: 1312 tests before `tally` and 1312 after. A
  program's presence is proven only by a transcript line somebody remembered to write into
  `SHELL_CHECK_SCRIPT`.
- **The archives do not boot the same binary, and the sentence saying so is 200 lines from where
  you need it.** `mkinitrd()` packs `hello` under the archive name `init`; `initrd_riscv()` packs
  `builder`. `xtask/src/main.rs` does state this, in a comment on the aarch64 table's `hello`
  row rather than on either `("init", ...)` row, and run 4's stranger read both tables in the
  same minute and still reported the asymmetry as undocumented. For a project whose loudest
  claim is architectural parity that is worth meeting at the table you are editing.
- **Removal is the same eight places and has no page.** Taking a program out is clean only while
  you can still name every file you touched; a half-removed program is a `PROG_COUNT` too large
  and an init table slot no variant claims, which is the same silent failure as a forgotten
  `PROG_COUNT`, reached from the other side. There is no `removing-a-program.md` and this page is
  about adding. Run 4 reverted `tally` to a byte-identical tree and noted that it worked first
  time only because the eight edits were still in its head.
- **This page is prose and the code can move without it.** The step that rots first is the manifest
  field list, which is why it is not repeated here: [program-manifest.md](program-manifest.md) has it,
  and the struct in `crates/grant_plan/src/lib.rs` is the authority over both.
- **Written from having done it, three times, most recently on 2026-08-18.** It began as a
  second-hand account of a first-hand guess: reconstructed after milestone 117's first stranger
  reconstructed it, and its own BUGS section asked the first person to add a program against it to
  correct whatever it got wrong. Every walk since has been that, and each one found the page wrong:

  | walk | program | wrong in |
  |---|---|---|
  | 2026-08-16 (run 2) | `doubler` | the aarch64 tier, the riscv `--bin` list, two of the six `grant_plan` edits, the `provisional` spelling the gate rejects |
  | 2026-08-18 (run 3) | `triangle` | the aarch64 tier again (milestone 130 had deleted both shapes it described), and `manifest()` missing from the `grant_plan` list |
  | 2026-08-18 (this lane) | a scratch binary, added and removed | `cargo xtask build` claimed to pack both archives and packs one, the `SHELL_CHECK_SCRIPT` example did not compile, and nothing said which of the seven `grant_plan` edits the machine catches |
  | 2026-08-18 (run 4) | `tally`, added and removed | **nothing.** The first walk of four to find no defect, including `crates/swish/src/lib.rs:864`, which the page quotes by line number and which still is that line |
  | 2026-08-18 (run 5) | `nth`, kept | an **eighth** edit site the list of seven does not have: a manifest that requires an argument *and* an input fails `the_arg_line_follows_the_manifest_for_every_program` in `crates/swish/src/lib.rs`, in a crate the walker did not edit |

  Run 3 recorded its two rather than fixing them, deliberately and per its own convention: a run
  that stops to fix things stops measuring, and its findings stop being traceable to it (see
  notes/stranger-test.md). The lane below it did the fixing.

  **One walk-through is not a guarantee and four are not either**, and the next person to add a
  program should treat a surprise here as this page's bug rather than their own.

  **This table is also a leak, and it is worth knowing about before adding to it.** Milestone
  117's fourth stranger read these rows within half an hour and knew from them that it was at
  least the fourth person walking this page under measurement, which changed how it wrote. The
  rows stay, because deleting them would fabricate a tree and because the page's value is that
  it says how often it has been wrong. See notes/stranger-test.md's `BUGS`.
- **This page went stale inside two days, twice, which is the finding rather than the accident.** Run
  2 corrected it on 2026-08-16; milestone 130 falsified step 4 on 2026-08-17; run 3 found it wrong on
  2026-08-18. The fact it describes lives in seven hand-maintained places and this page is an eighth.
  By the ladder in AGENTS.md that is a rung-four answer to a rung-one problem, and rewriting the
  prose a fourth time will not change it. The tracked home for the mechanism is milestone 150
  ("Adding a program should not need eight hand-maintained lists," minted provisionally 2026-08-22 by
  milestone 117's handoffs lane, nominated by three successive strangers): a `Prog` variant could
  carry its archive name and its manifest as data, and both initrd tables could be generated from it.
- **There is an eighth edit site and it depends on your manifest, so the count above is a lower
  bound.** Found 2026-08-18 by milestone 117's fifth stranger, which deliberately picked the one
  manifest combination nothing in the tree had used: a **required argument together with a required
  input**. `the_arg_line_follows_the_manifest_for_every_program` in `crates/swish/src/lib.rs` sweeps
  `Prog` and asks each program's manifest whether it takes an argument, which is the generalisation
  its own doc comment argues for at length. Then it builds the line `"<name> 21"` against
  `Holdings::default()` and hard-codes everything else, so the planner refuses it for any program
  that also requires an input, and the sweep goes red on a program whose only sin is a manifest
  shape nothing had used yet. **A test written to survive the next program added does not survive
  this one**, and it is in a crate the person adding the program has no reason to open. The
  stranger repaired it in its own disposable clone and the tree is unchanged: on `main` an
  argument-plus-input program cannot be added without a host test going red. Whether that
  combination is wanted at all is an open question this page cannot answer: `plan_against_with`
  carries a comment ruling out file-plus-input on `ArgSpec` grounds and says nothing about
  argument-plus-input, so a newcomer cannot tell headroom from a gap left on purpose.
- **The program's name is written in seven places** and nothing joins them: the `[[bin]]` block in
  `user/Cargo.toml`, `mkinitrd()`'s table, `initrd_riscv()`'s `--bin` list, `initrd_riscv()`'s table,
  the seven-part `Prog` table in `grant_plan`, the exhaustive match in `swish`, and
  `SHELL_CHECK_SCRIPT`. This page is an eighth. Steps 4 and 6 are long because the tree is, not
  because adding a program is hard. **Three of the seven can be skipped in silence**: a missing
  `initrd_riscv()` row, and `from_id()` and `from_name()` when `PROG_COUNT` was forgotten alongside
  them. Step 6's table is measured rather than reasoned, and a claim about which of these the
  compiler catches is worth re-measuring rather than quoting: the last such claim written down in
  this tree was wrong, and it was written in the test that makes it.
