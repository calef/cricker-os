# A shell at EL0

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
- **The input driver** (new) owns the UART receive side and its interrupt (INTID 33). It
  assembles a line character by character and hands each completed line to the shell.
- **The shell** (new) reads a line, echoes it, and runs a command: `help`, `echo`, `run`.
- **A worker** is spawned for each `run`. It computes, reports its answer to the shell, and
  **exits** — a whole process lifecycle driven by a line the user typed.

Every one of those is a program at EL0. None can reach the hardware except through a capability it
was handed. The kernel routes messages and creates processes; it prints nothing on anyone's
behalf and reads no device on anyone's behalf.

## What a session looks like

```
cricker-os shell. every command below runs at EL0.
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
   crosses the endpoint — control by message, data by shared memory, §10 again).
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
memory — §10's deferred third axis, milestone 11. The shell does not care where the service lives,
only that it can name it, which is the point: the interface is a capability either way, and moving
the service to userspace later changes nothing the shell can observe.

**And the worker is a role of one binary, not a separate file on disk.** A richer shell would read
a named ELF from the crickerfs filesystem (milestone 9) and exec it. The pieces are all present —
the disk driver reads files, the ELF loader runs arbitrary binaries — and wiring `run <file>` to
them is the natural next step. What milestone 10 proves is the harder half: a process, spawned on a
typed command, running at EL0, reporting back, and exiting.
