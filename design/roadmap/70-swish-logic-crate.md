# 70. `swish`'s remaining logic in a crate, host-testable like its siblings

**Status: BUILT.** Raised 2026-08-02, and **the finding that prompted it was wrong**, which is
worth recording because the corrected version is a smaller and more honest milestone.

`crates/swish` holds the shell's logic and `user/src/swish.rs` keeps the IO, which took 354 lines out
of the program and bought 36 host tests (33 unit, 3 doctests) where there had been none. What lifted:
the routing of a typed line, the pattern-versus-text question, the expansion order, `echo`, and every
sentence the prompt prints (the refusals, the outcome, the endowment preview, the shell's own `caps`
table, the help). What did not, and why, is in notes/shell.md and in the crate's own `BUGS` section:
`builtin`, `dispatch_one`, `run`, `spawn` and `pipeline` are capability movement, and lifting them
would have needed the shell's IO restructured, which this milestone was scoped not to do.

## The correction

`user/src/swish.rs` is 2,625 lines with **zero `#[cfg(test)]` blocks**, and that was first reported
as "the shell is untested". It is not. The shell is covered twice over:

- **~28 QEMU integration `test_case`s** across five kernel test modules (`shell_navigation_tests`,
  `pipeline_tests`, `redirection_tests`, `glob_grant_tests`, `rm_program_tests`), which spawn the
  real binary and drive it.
- **93 host unit tests in `crates/grant_plan`**, which already holds swish's parsing, navigation and
  grant-planning logic. `swish.rs` imports `grant_plan::{expand, line, nav}` and `line_editor::proto`
  rather than reimplementing any of it.

So 0% was a fact about one **file**, not about a component, and a file-level metric said something
false about the system. That is the general lesson: coverage measured per file counts where tests are
*written*, not what they *reach*.

## The real gap, which is narrower

What is left in `swish.rs` is mostly IO glue, and some of it is logic that a host test could reach if
it were lifted: `builtin` and `dispatch`, `outcome`'s interpretation of a spawn result, `preview`'s
rendering of an endowment, `refuse`'s mapping from a `Refusal` to what the user reads, `print_num`.
Today every one of those is exercised only by booting QEMU, which is slow, coarse, and cannot easily
provoke the error paths.

CLAUDE.md already names the pattern this should follow: **a crate and a program may share a name, and
it says something when they do** (the crate is the logic, lifted so it is host-testable and
Kani-reachable; the program keeps the IO). `coremark`, `line_editor` and `compositor` are each that
pair. `swish` is the largest program that is not one.

## Scope note

This is an incremental tidy of a working, tested component, not a fix for a defect. It should be
scheduled accordingly, and it should not grow into a rewrite of the shell. If lifting a function
needs the shell's IO restructured to accommodate it, that function stays where it is.

## A gate blind spot found while raising this

Milestone 69's **table row said `NOT-STARTED` while its own detail block said `BUILT`** on `main`,
because the lane that built it updated one and not the other. `script/roadmap --check` did not catch
it: the check validates the status VOCABULARY and that every detail block has a table row, but never
that the two statuses AGREE. Corrected by hand on 2026-08-03.

This is the third such blind spot found in two days, and they are all the same shape: the gate
verifies that a thing is well-formed and never that it is right. `script/decisions --check` reports
"numbering clean" for a section filed in the wrong place, and nothing checks that a source path cited
in prose resolves to a file that exists (milestone 69 fixed 49 stale ones by hand). Each is a few
lines to add. None is written yet, and they are listed here rather than in a tracker so the next
person to touch these scripts finds them.
