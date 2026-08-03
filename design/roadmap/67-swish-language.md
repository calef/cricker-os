# 67. `swish` the language: quoting, sequencing, and exit status

**Status: NOT-STARTED.** Raised 2026-08-02, from measuring `swish` against a minimal POSIX shell.

## Where `swish` actually stands

It has `help`, `echo`, `caps` (the whole endowment, and a **preview** of what a command would grant),
`cd`, `pwd`, `ls`, `mkdir`, `rm`, program spawn with a file grant, `worker`, `budgeter --mem N`,
`date`, `wc`, **globbing**, **pipes**, and **redirection**. `ls | wc` and `ls > out.txt` run at a live
prompt on both ISAs.

**So it is an interactive shell without control flow.** The effort went into the
capability-interesting parts, composition and grants and navigation, which was the right order. What
is missing is the *scripting language*.

## What this milestone covers, and what already has one

| Gap | Where it lives |
|---|---|
| **Quoting**: `"..."`, `'...'`, backslash | **here** |
| **Sequencing**: `;`, `&&`, `\|\|` | **here** |
| **Exit status**: `$?`, which `&&` needs | **here** |
| `>>` and `2>` | **here** (named unbuilt in `notes/pipes.md`) |
| Variables, assignment, `export` | milestone 47 (studied: "the same question wearing a string costume") |
| Job control: `&`, `jobs`, `fg`, `bg`, `wait`, `kill` | milestone 48 |
| Subshells, command substitution `$(...)` | milestone 52 |
| Scripts, `if`/`while`/`for`/`case`, functions | **nowhere yet, and deliberately** |

## Quoting is the one that is not a convenience

**A filename with a space is currently unnameable.** That is a correctness gap in a shell whose whole
thesis is that *naming a resource is granting it*: a resource you cannot name is a resource you cannot
grant, so the gap lands squarely on the thing this shell exists to demonstrate.

It also interacts with globbing (§52's name sets) and with the grant planner: a quoted name must not
be glob-expanded, and an unquoted one must be. That is a parser change with a capability consequence,
which is why it belongs with the other two rather than being filed as polish.

## Exit status is a capability question in disguise

`&&` needs to know whether the previous command succeeded. Programs already report through a result
endpoint, so the mechanism exists; what does not exist is `$?` at the prompt, or a decision about
**what a status means when the thing that failed was a refusal rather than an error**. `swish` refuses
constantly and by design (`Refusal::TooManyNames`, "you hold no such capability"), and whether a
refusal is a non-zero status or something else is a design fork, not an implementation detail.

## BUGS

- **Scripting is not scoped here on purpose.** `if`/`while`/`for`/functions and reading a script file
  are a much larger thing, and this project has no story yet for what a script *is* when a program
  namespace is an endowment. Doing quoting and sequencing first is what makes that question
  answerable rather than theoretical.
- **The four gaps above are not independent.** `&&` needs exit status, and both want quoting to be
  settled first, or the parser gets rewritten twice.

**Effort: small to medium**, and mostly in `grant_plan`, which is host-testable, so most of it can be
proven in milliseconds without an emulator.
