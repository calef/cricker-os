# 67. `swish` the language: quoting, sequencing, and exit status

**Status: BUILT.** Raised 2026-08-02 from measuring `swish` against a minimal POSIX shell; built
2026-08-04 in three commits (`eca4da2f` quoting, `b20bfc3b` sequencing and exit status, `9155d647`
the boot gate and the note), and the record did not catch up for twelve days. Closed 2026-08-16 on
the tree's own evidence rather than on a report: `crates/grant_plan/src/word.rs`,
`crates/swish/src/sequence.rs`, `swish::Status`, `kernel::user::language_tests` and
notes/swish-language.md all exist, and the four rows below are each answered by a named artifact.

(The Gate paragraph that stood here said `NONE` and explained that quoting needed no decision. It
was right, the work is done, and a BUILT milestone gates nothing, so it is gone rather than stale.)

## What the milestone said was missing, and what answers each row now

| Gap | Where it lives now |
|---|---|
| **Quoting**: `"..."`, `'...'` | `grant_plan::word`: `Cursor` (is this byte bare) and `read` (take the quotes off one token) |
| **Sequencing**: `;`, `&&`, `\|\|` | `swish::sequence::{split, Joint, Sequence}`, split **outermost**, above `line::split` |
| **Exit status**: `$?`, which `&&` needs | `swish::Status`, three values, and two cells in `user/src/swish.rs` |
| `>>` and `2>` | already built with milestone 50's later work, 2026-08-03. See notes/pipes.md |

The backslash escape named in the original block is **not** built and is not planned: every token in
this shell is a slice of the line, and an escape has to remove a byte from the middle of a word,
which a slice cannot do. The quoted spelling is the one that works. That is a recorded limitation
rather than a residual, and it is in notes/swish-language.md's BUGS beside the feature.

## The two things worth reading the note for

**Quoting was an authority gap, not a convenience.** A file called `my notes.txt` could not be
named, and in a shell whose thesis is that naming a resource is granting it, a resource you cannot
name is a resource you cannot grant. The built rule is that **quoting delimits a word and never
rewrites one**, which keeps every token a slice, and its capability consequence is a *narrowing*:
`rm "*.txt"` designates one name where `rm *.txt` designates a set, and `rm "-r"` names a file
rather than widening a directory grant.

**The fork this milestone was raised to settle is settled**: a refusal is not an error, and `$?`
says which. `0` ran, `1` failed (something was attempted and did not work), `2` was refused (the
shell declined at the prompt with nothing spawned, nothing opened and no authority moved). `&&` and
`||` read one bit out of it, because they ask one question and both non-zero answers are "no"; the
distinction stays where a person can see it. Unix cannot draw that line at all, because there `127`
and a program's own `exit(1)` are the same kind of integer.

## What proves it, on both ISAs

`kernel::user::language_tests` reads the tail of the same scripted run `redirection_tests` reads:
the real shell binary through the real init, with a directory narrowed by an `fs_subtree_caretaker`
to one subtree of the real RedoxFS image. It shares that witness rather than wiring a seventh
scripted shell, which is a memory finding the note keeps: the first version's extra role put
`time_tests` over the frame pool intermittently.

The assertions are **pairs**, so a shell that ignored quoting entirely fails both halves of each:
`echo "*.txt"` against `echo *.txt`, `wc "my notes.txt"` against `wc < "my notes.txt"`, and
`worker 3 && echo yes` against `worker && echo yes`. Parity is met by `script/shell-check` running
the same script on aarch64 and riscv64 rather than by a second implementation.

## BUGS

- **Scripting is still nowhere, and that was never in scope.** `if`/`while`/`for`/functions and
  reading a script file are a much larger thing, and this project has no story yet for what a script
  *is* when a program namespace is an endowment. Doing quoting and sequencing first is what makes
  that question answerable rather than theoretical.
- **The status is the shell's own reading of the line, not a program's.** No program in this system
  reports an exit status: a spawned program answers with a value, with bytes, or through a job frame.
  A per-program status would be a `spawnproto` bit, a delegation position and an edit to every
  program, which is a milestone and not a field.
- **`$?` is readable only in `echo`.** Substituting a word anywhere else needs the machinery
  milestone 47's variables need anyway.
- **There is no grouping**, so `a && b || c` is left to right with no precedence. `{ }` and `( )`
  should arrive with milestone 52's subshells.
- **This block stood at NOT-STARTED for twelve days after the work merged**, which is the §76 class
  the roadmap split and its gate exist to catch, and neither caught it: the gate compares the index
  row against the file's status line, and here **both** said NOT-STARTED, so they agreed and the
  build stayed green. Two records that agree with each other and not with the tree are invisible to a
  consistency check. The remaining full list of limitations is in notes/swish-language.md, next to
  the feature a reader meets.

**Effort: small to medium**, and it was: mostly `grant_plan` and `swish`, both host-testable, so
nearly all of it is proven in milliseconds without an emulator.
