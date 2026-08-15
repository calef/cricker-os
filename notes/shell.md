# A shell at EL0

**The shell's name is `swish`** (milestone 63): `user/src/swish.rs`, packed into the archive as
`swish`, loaded by that name. This note calls it "the shell" throughout because that is what it is;
where a path or an archive entry is meant, the spelling is `swish`. The argument for the name is in
milestone 63's roadmap block, and the short version is that `bash`, `zsh` and `fish` are names while
`shell` is a category.

Milestone 10. The rung DECISIONS.md calls "proof the whole stack works," and it is exactly that:
everything the user sees is a conversation between processes, and the kernel is a message router
that touches none of it.

## What runs

Four processes, and the channels between them:

```
  input driver ──line──► shell ──print──► console server ──► UART
   (owns UART RX)         │  ▲
                          │  └──result── worker (spawned on demand)
                          └──spawn──► process service (kernel)
```

- **The console server** (milestone 8) owns the UART transmit side and prints what it is sent.
- **The input driver** (new) owns the UART receive side and its interrupt (INTID 33 on QEMU
  `virt`; since 2026-08-15 the number comes from the device tree, with the constant as the
  documented fallback; see notes/device-tree.md). It
  assembles a line character by character and hands each completed line to the shell.
- **The shell** (new) reads a line and runs a command: `help`, `echo`, `run`.
- **A worker** is spawned for each `run`. It computes, reports its answer to the shell, and
  **exits**: a whole process lifecycle driven by a line the user typed.

Every one of those is a program at EL0. None can reach the hardware except through a capability it
was handed. The kernel routes messages and creates processes; it prints nothing on anyone's
behalf and reads no device on anyone's behalf.

## What a session looks like

```
nife shell. every command below runs at EL0.
commands: help, echo <text>, run <n>
$ help
  help        this text
  echo <text> print <text>
  run <n>     spawn a worker process that returns n*n
$ echo hello from a userspace shell
hello from a userspace shell
$ run 9
  a spawned process at EL0 computed 9*9 = 81
```

## Console input, the receive half of a terminal

Milestone 8 put console *output* in userspace. A shell needs *input*, which is a second
userspace driver, and it is where the milestone-9a interrupt-as-message machinery earns its keep
again. The input driver:

1. Enables the PL011's receive interrupt.
2. Blocks on it (its `Irq` capability's `WAIT`).
3. When a character arrives, reads the receive FIFO, buffers it, and on a newline hands the line
   to the shell over an endpoint (the bytes travel in a page shared with the shell; the length
   crosses the endpoint: control by message, data by shared memory, §10 again).
4. Acknowledges the device and re-arms the interrupt.

**Driving it from a pipe.** QEMU connects the guest UART to stdio, so a script of commands piped
into QEMU arrives at the receive FIFO and the shell runs it. Getting there flushed out two real
things in the harness, both recorded because they cost real time:

- `scripts/qemu-bounded.sh` backgrounds QEMU (`"$@" &`) so it can enforce a timeout. A
  backgrounded command's stdin is redirected to `/dev/null` by the shell (POSIX), which silently
  swallowed all piped input. Fixed with an explicit `<&0`.
- `-nographic` **multiplexes** the serial port with the QEMU monitor on stdio, and piped input was
  going to the monitor. Switched to `-display none -serial stdio`, which dedicates stdio to the
  serial port.

## Echoing what you type

The terminal runs in raw mode (QEMU hands the whole serial line to the guest), so nothing echoes
locally: if the guest does not show a character back, you cannot see what you are typing. The
**input driver** echoes each character as it reads it, and handles backspace visually (back,
space, back). The shell does **not** echo the command afterward, or you would see it twice.

This is safe against interleaving with the shell's output because of the synchronous handoff: while
you type, the shell is blocked in `RECV` waiting for the line, so it is not writing the UART. The
input driver echoes, sends the completed line, and only then does the shell wake, print its output,
prompt, and block again. Prompt, your keystrokes, output, prompt: one writer at a time, in order.
(An earlier version had the *shell* echo the whole line after Enter, to keep piped bulk input
tidy. That left interactive typing invisible until you pressed Return, which is the wrong trade: a
person needs to see each character as they type it.)

## Two things left honest rather than hidden

**The first character of piped bulk input is lost once.** A script piped all at once overruns the
16-byte receive FIFO's timing at boot, and the very first character of the stream goes missing.
The demo absorbs it with a leading newline, and an interactive user loses at most the first
character of their first command, once. The root cause is a boot-time race between QEMU filling
the FIFO and the driver draining it; a fuller driver would enable hardware flow control. Noted,
not papered over.

**The process service is a kernel thread.** The shell's `run` sends a spawn request to a service
that starts the worker. That service lives in the kernel today, because true userspace process
creation needs the kernel to hand out address-space and thread capabilities built from **Untyped**
memory: §10's deferred third axis, milestone 11. The shell does not care where the service lives,
only that it can name it, which is the point: the interface is a capability either way, and moving
the service to userspace later changes nothing the shell can observe.

**And the worker is a role of one binary, not a separate file on disk.** A richer shell would read
a named ELF from the nifefs filesystem (milestone 9) and exec it. The pieces are all present:
the disk driver reads files, the ELF loader runs arbitrary binaries, and wiring `run <file>` to
them is the natural next step. What milestone 10 proves is the harder half: a process, spawned on a
typed command, running at EL0, reporting back, and exiting.

## The program and the crate (milestone 70)

`swish` is now two things with one name: the program at `user/src/swish.rs` and the crate at
`crates/swish`. The crate holds what the shell **decides or renders**; the program holds everything
that needs a capability. That is the same pair `coremark`, `line_editor` and `compositor` already
are, and the reason for the shared name is in CLAUDE.md: splitting them would hide the relationship.

The line between them is one question. **If a function needs a capability, it stays in the
program.** Routing a typed line, deciding whether a word is a pattern or text, and every sentence
the prompt prints need none, so they moved; the terminal page, the spawn channel, the filesystem
requests and the pipe endpoints did not.

Two functions in the crate take the shell's directory read as a callback, `expand`, rather than
losing it. Matching a pattern is pure and lives in `grant_plan::expand`; only *reading* the
directory needs a capability. So `swish::echo` and `swish::expansion` are host-testable end to end
against a fixture directory, which is what makes `echo *.txt` and `caps rm *.txt` provably render
the same set.

### What did not move, and why

- `builtin` and `dispatch_one`. Every arm is a request to the filesystem server (`cd`, `ls`,
  `mkdir`) or a print. `swish::route` lifts the decision they sit under, which is the half with a
  bug behind it; the arms belong with the wiring.
- `run`. Its body is two calls into `grant_plan`, both already host-tested there, wrapped around a
  choice between two spawn paths. Lifting it would move the spawn decision away from the only code
  that can act on it.
- `spawn`, `pipeline`, the file sinks and sources, the interruptible job path. Capability movement,
  all of it.

### The finding that prompted this was wrong, and the correction is the useful part

The milestone was raised as "the shell is untested": `user/src/swish.rs` had 2,625 lines and zero
`#[cfg(test)]` blocks. It was covered twice over the whole time, by about 28 QEMU integration
`test_case`s (`shell_navigation_tests`, `pipeline_tests`, `redirection_tests`, `glob_grant_tests`,
`rm_program_tests`) and by 93 host tests in `crates/grant_plan`, which already held its parsing,
navigation and grant planning.

So 0% was a fact about a **file**, not about a component. Coverage measured per file counts where
tests are *written*, never what they *reach*, and in a tree whose whole method is "pure logic in
crates, IO in programs" that metric will read zero for every program by construction. The real gap
was narrower and worth closing anyway: the error paths QEMU cannot easily provoke. `caps` refusing a
pipeline whose first stage has no bytes to pipe, a pattern the shell holds no directory to expand, a
spawn that failed under a program whose report would otherwise print a number.
