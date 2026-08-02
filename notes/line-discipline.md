# The line discipline as a userspace component

Milestone 28. The layer Unix builds into the kernel as the tty line discipline is here a process,
`line_editor`, sitting on plain endpoints between the raw input driver and the application, and between
the application and the console server. This note covers the build-vs-reuse call, the component's
shape, and the one argument that makes the design correct: it cannot deadlock. The interface it
presents is written up separately in [terminal-contract.md](terminal-contract.md); the interrupt
question it raises is [../design/interrupt-routing.md](../design/interrupt-routing.md).

## Two pieces: a sans-IO engine and a wiring process

The editing lives in the `line_editor` crate, which does no IO and knows nothing about IPC, UARTs,
or endpoints. Byte in, echo bytes out, completed lines out. That split is DECISIONS §7 applied:
the editing rules are host-tested in milliseconds against a small terminal model that interprets
the echo the way a VT does, so the tests assert **what the user sees**, not which escape
sequences were emitted. Twenty tests cover typing, backspace, mid-line insert and delete, cursor
keys in three encodings (CSI, SS3, vt220 `~`), kill and yank (`^K` `^U` `^W` `^Y`), history with
non-destructive browsing and dedup, CR/LF/CRLF endings, `^C`, `^D`'s double duty, `^L` repaint,
overflow, and output newline expansion. The redraw strategy can change without touching a test,
which is the payoff of modelling the screen instead of the byte stream.

The `line_editor` binary (in `user/`) is the other piece: words in, pages copied, words out. It owns no hardware.
Its whole authority is the terminal endpoint (serve), the console server's request and reply
endpoints (print), and three shared pages (the console's, the client's output, the client's
input). It touches no UART and no interrupt, which is exactly why the component could not exist
until the drivers did: it is the layer *between* drivers, not a driver.

## The build-vs-reuse call: built, against the rule's default

The prior-art rule (DECISIONS §14) says for userspace, outside the trusted computing base,
actively prefer porting. The editing engine was **built** anyway, and here is the accounting for
why the two live candidates did not fit.

- **`noline`** (a `no_std` readline) has a sans-IO core, which is the right shape. Two things
  disqualify it. Its initialization **blocks on a cursor-position report** (`ESC[6n`) that the
  terminal may never send: a serial line driven by a piped boot script never answers, so a line
  discipline that must be always-on would hang at startup. And it is a **per-read readline**, a
  function you call when you want a line, not an always-on discipline that echoes type-ahead
  while the application is busy elsewhere. Our component must accept and echo bytes that arrive
  before any read is outstanding, which is a different lifecycle than `noline` offers.
- **`embedded-cli`** is a command framework: it parses commands and dispatches handlers. That is
  the *application's* altitude (the shell, milestone 31), not the terminal's. It does not give us
  the line-editing-and-echo layer we needed and would have pulled command parsing into the wrong
  process.

What we would reuse if the requirement grew: a mature VT engine on the *output* side is a real
port candidate for milestone 29 (libghostty-vt or `vte`), because a full grid-and-scrollback
state machine is genuinely worth not writing. The input-side line editor, by contrast, is a few
hundred lines of well-understood logic with an unusual lifecycle requirement, and owning it kept
the sans-IO testability and the always-on behavior that the candidates fought.

## Why it cannot deadlock

This is the part worth getting right, because a terminal that can wedge is worse than none. The
risk: a client blocked *printing* while the server is blocked *delivering a line* to it, each
waiting for the other.

The Reply capability (DECISIONS §12) removes it. Every request is a `CALL`, served through
`RECV_CAP`. When an `OP_READLINE` arrives and no line is ready, the server does not block waiting
for input. It **parks the caller's one-shot Reply capability in a slot** and loops back to serve
everyone else; the caller stays blocked (that is `CALL`'s contract) without holding the server
hostage. Bytes keep flowing from the input driver, the discipline assembles a line, and only then
does the server invoke the parked Reply to wake the reader. A client blocked in `OP_WRITE` and a
reader parked in `OP_READLINE` are both just parked callers; the server is never blocked on either
while serving the other.

The kernel makes the slot management safe: `cspace.insert` hands each incoming Reply capability a
*fresh* free slot, so a parked read's Reply at one slot is never clobbered by the next `CALL`'s
Reply at another. One outstanding read per terminal (the contract) bounds the parked set to one.

The one place `line_editor` does block is its own call to the console server, and that is a separate,
single-client rendezvous (`line_editor` is the console's only client) that cannot interleave with the
terminal endpoint it serves.

## Type-ahead, bounded

Completed lines that arrive with no reader waiting go into a fixed FIFO (four lines). A user
typing ahead of a busy application loses nothing until the queue fills; then the newest line is
dropped with a bell, which is what a real tty's flooded input queue does. When a read finally
arrives and a line is queued, the two are married at once: the line is copied into the client's
input page and the parked Reply is invoked. `^C` clears the queue and fails any parked read with
`FLAG_INTERRUPTED`.

## The component is swappable, and that is the demonstrator point

Because every party names only an endpoint ([ipc-naming.md](ipc-naming.md)), the discipline can
be replaced by rewiring endpoints, with no client able to tell. The input driver holds "an
endpoint I send bytes to"; the shell holds "an endpoint that prints and reads lines." Neither
holds a name for `line_editor`. This is milestone 23's "the console hot-swap is instance one" in
concrete form: a terminal with real behavior is a far better first swap target than a raw echo
loop. `line_editor` faults into a clean kill on any bug (it `brk`s / `ebreak`s in its panic handler),
which is honest rather than reckless precisely because the hot-swap story is what recovers it.

## Where it runs

The interactive system is built by userspace init out of its own budget, on both architectures:
aarch64 through `hello`'s init role (the `initboot` path, and now the `shell` feature and the
default tour hand off to it too, since the kernel-wired `shell_service` cannot host a
contract-speaking shell), and RISC-V through the portable `system_initializer` builder (`riscv_shell_boot`).
Init creates the terminal endpoint and the shared frames, grants `line_editor` the serve side and the
drivers and shell the `WRITE` side, and none of the wiring is in the kernel. Proven under QEMU on
both ISAs: `help`, `echo`, and `run` drive a worker to completion through the full path
(input driver → line discipline → console server, and shell → line discipline → console server).
