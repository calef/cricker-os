# 150. Adding a program should not need eight hand-maintained lists

**Status: NOT-STARTED.**

**Gate: NONE.** Minted provisionally on 2026-08-22 by milestone 117's handoffs lane; the integrator
should confirm the number at merge. 147, 148 and 149 were already claimed by other lanes' pull
requests (147/148 open, 149 merged into this lane's own base commit) as this was written.

## What this is

**Nominated by three successive strangers.** Milestone 117's stranger-test runs 3, 4 and 5 each
independently walked `notes/adding-a-program.md` by adding and removing a scratch program, and each
one named the same defect as the highest-value thing a newcomer could fix: adding a program to this
tree means editing the same fact into eight hand-maintained places, by hand, with no gate that
catches every way to get one of them wrong.

The eight, as `notes/adding-a-program.md` currently enumerates them:

1. the `[[bin]]` block in `user/Cargo.toml`;
2. `mkinitrd()`'s `entries` table (aarch64 archive, `(archive_name, bin_name)` pairs);
3. `initrd_riscv()`'s `--bin` argument list (what cargo actually builds for riscv64);
4. `initrd_riscv()`'s own `entries` table (what the archive actually packs);
5. the `Prog` variant in `crates/grant_plan/src/lib.rs`, itself seven edits per program
   (`PROG_COUNT`, `from_id()`, `from_name()`, `name()`, `id()`, `manifest()`, and the variant);
6. the exhaustive `match` arm in `crates/swish/src/lib.rs` that renders the shell's answer;
7. `SHELL_CHECK_SCRIPT` in `xtask/src/main.rs`, if the shell spawns the program;
8. `notes/adding-a-program.md` itself, which describes the other seven and has gone stale twice in
   the four days between runs 2 and 3 alone.

Run 5 added an eighth *edit site within site 5*: a program that both requires an argument and takes
an input fails `the_arg_line_follows_the_manifest_for_every_program` in `crates/swish`, a test in a
crate the person adding the program has no reason to open, because that host test hard-codes
`Holdings::default()` and a fixed argument line rather than reading the rest of the manifest it
claims to check generally.

## Why this is a design question, not a documentation fix

CLAUDE.md's ladder ranks "make the wrong state unrepresentable" above "a gate that fails loudly"
above "a written record at the thing itself" above "a note." Right now the eight-list problem is
answered entirely at rung four (the note you are reading a summary of), and the note itself is the
evidence that rung four cannot hold this fact: it has been rewritten by four successive strangers,
correctly, and gone stale again within days each time, because the fact it describes lives in seven
other files it does not control.

**What the compiler already catches, and what it does not**, measured rather than assumed (run 5,
2026-08-18, by widening `PROG_COUNT` last and watching what broke):

| edit skipped | consequence |
|---|---|
| `name()`, `id()`, `manifest()` on the `Prog` variant | compile error, `E0004`, in `grant_plan` itself |
| the `swish` render arm | compile error, `E0004`, in a crate the author did not otherwise touch |
| `from_id()` or `from_name()` | a host test fails, **but only if `PROG_COUNT` was also widened** |
| `PROG_COUNT` | **nothing.** The tree compiles, every host test passes, and the program cannot be spawned from the prompt until somebody notices by hand |
| the riscv `--bin` list (site 3) without the riscv `entries` table (site 4), or vice versa | either a build failure naming a missing ELF, or a program present in `mkinitrd()` and silently absent from the riscv archive, uncaught by `cargo xtask build` or `script/lint`, first caught (if at all) by `script/shell-check` or `script/test`, both of which cost an emulated boot |

`PROG_COUNT` is the keystone and forgetting it hides the other two: the sweep in
`prog_id_round_trips` only walks up to that constant, so a variant added past it is invisible to the
one test that would have caught the missing `from_id`/`from_name` arms. **And nothing counts
programs at all**: the kernel test suite reports the identical total, 1312, both before and after a
program is added or removed. A program's presence in this tree is proven only by a transcript line
somebody remembered to type into `SHELL_CHECK_SCRIPT`.

## What this milestone would decide

**The shape run 3 nominated and runs 4 and 5 did not dispute**: a `Prog` variant should carry its
archive name and its manifest as data, so that both initrd tables (sites 2 and 4), the `--bin` list
(site 3), and as much of the `grant_plan` bookkeeping (site 5) as possible are *generated* from the
one place a program is declared, rather than hand-copied into four more. Concretely, this milestone
would need to answer:

1. **Where the single declaration lives.** A `const` table in `grant_plan` that both `xtask` (for
   the initrd lists) and the shell (for dispatch) can read is the shape the current seven-edit list
   already gestures at with `Prog`; whether `xtask` can depend on `grant_plan` without pulling
   `no_std` machinery into a host binary is the first thing to check, not assume.
2. **Whether `PROG_COUNT` can be replaced by something the compiler derives**, rather than a
   number a human widens. Rust has no built-in way to count an enum's variants without a derive
   macro, and this tree has deliberately not taken one (DECISIONS §46: write it if it's on the
   verification path). Whether a hand-rolled `const fn` count, a macro this tree writes itself, or
   accepting the manual count with a stronger gate is the right trade is exactly the kind of
   question that belongs in `design/decisions/`, not assumed by a lane.
3. **Whether the argument-plus-input manifest gap (run 5's eighth edit site) is closed by the same
   mechanism or needs its own decision.** `notes/adding-a-program.md`'s `BUGS` section already
   records this as open: whether that combination is wanted at all, or should be refused the way
   file-plus-input already is refused with a comment at `plan_against_with`. This milestone's
   generalization would likely force that question rather than let it stay implicit.
4. **What a gate on program *count* would look like**, since nothing today notices a program was
   added or removed. A `PROG_COUNT`-vs-archive-entry-count assertion in a host test is the cheapest
   candidate and should be weighed against the cost of computing "how many programs are actually in
   this archive" outside a QEMU boot.
5. **Removal**, which run 4 pointed out is the same eight places in reverse and has no page at all
   today. Whatever generation mechanism this milestone builds should make removal the deletion of
   one declaration rather than a checklist nobody has written down.

## What this does NOT include

- **Rewriting `notes/adding-a-program.md` a fifth time as prose.** That is the move this milestone
  exists to stop making. If any hand-maintained step survives this milestone, it gets documented,
  but the target is fewer steps, not a better description of the current eight.
- **Deciding the generation mechanism up front.** The three candidate shapes above are starting
  points for the design fork, not a recommendation; this block scopes the question.
- **Touching the riscv/aarch64 archive-content asymmetry** (that `mkinitrd()` packs `hello` as
  `init` and `initrd_riscv()` packs `builder`). That is a separate, already-recorded finding
  (`notes/adding-a-program.md`'s `BUGS`) about which binary boots as `init` on each ISA, not about
  how a program's declaration is spread across files.

## Prior art in this tree

- `xtask`'s measurement manifest is already generated rather than hand-maintained: it hashes every
  entry of the archive it just packed and writes the manifest from that (`write_measure_manifest`),
  so a program's presence there follows the archive by construction with no eighth place to edit.
  That is the shape sites 2 through 4 (and as much of 5 as practical) should end up in.
- The boot tour's `println!("... skipped (no 'X' program in the initrd)")` pattern already treats
  "this program may or may not be present" as ordinary data to check against, rather than an
  assumption baked into a hand-written list; the same posture, one level up, is what a generated
  `Prog` table would give the initrd builders.

## Where the finding is recorded today

`notes/adding-a-program.md`'s `BUGS` section carries the full history (runs 3, 4 and 5, each citing
the file and line the previous stranger read) and should link to this milestone once the integrator
confirms its number, replacing the current circular pointer at "the tracked home for the mechanism is
milestone 117's handoff."
