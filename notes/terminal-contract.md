# The terminal contract

Milestone 28. This is the interface a terminal presents, written down so that the programs on
either side of it can be built independently and swapped without either one knowing. Milestones
29 (the display terminal) and 31 (the capability shell) implement *against* this contract rather
than against `termd`, the particular component that satisfies it today.

A terminal sits between two driver endpoints and an application:

```text
  input driver ──OP_BYTES──►┌──────────┐──text──► console server ──► UART
                            │ terminal │
       application ◄─lines──└──────────┘◄─OP_WRITE / OP_READLINE── application
```

Nobody in that picture can name anyone else. The input driver holds "an endpoint I send wire
bytes to." The application holds "an endpoint that prints text and reads lines." The console
server holds "an endpoint requests arrive on." Endpoint-only naming
([ipc-naming.md](ipc-naming.md)) is the whole point: rewire the endpoints and no client can tell
the terminal changed, which is milestone 23's hot-swap claim in component form. See
[line-discipline.md](line-discipline.md) for the component that implements this today and why it
was built rather than ported.

## The two halves of the contract

A contract has a wire half and an IPC half, and they are independent.

- **The wire half** is what the terminal echoes to the screen and what escape sequences it
  understands from the keyboard. A client never sees this. It is the agreement between the
  terminal and the *human* at the far end of the serial line, and it is documented in
  [line-discipline.md](line-discipline.md) with the engine that produces it.
- **The IPC half** is the protocol on the endpoints: the opcodes, the flags, the shared pages.
  This is what a client and the drivers must speak, and it is the substance of this note. The
  framing constants live in `linedisc::proto` so the server, its clients, and the kernel-side
  tests share one definition.

The protocol is a **userspace** protocol, not kernel ABI. The kernel routes these words the way
it routes any IPC (§10, §12); it never reads an opcode. Adding an opcode is a change to this note
and to `linedisc::proto`, not a change to the syscall surface.

## The IPC protocol

Every request is an endpoint `CALL` (DECISIONS §12): the client sends two words and blocks until
the terminal replies through the one-shot Reply capability the kernel mints. The first word packs
an opcode in bits 63:56 and a length or count in the low 32; bits 55:32 are reserved and zero.
`proto::req(op, len)` builds it; `proto::op` and `proto::len` take it apart.

Bulk data never rides in the words. It travels in pages the client shares with the terminal, one
outbound and one inbound, exactly the §10 split: control by message, data by shared memory. A
client maps an **output page** (it writes, the terminal reads) and an **input page** (the
terminal writes, it reads).

| Opcode | Direction | First word | Second word | Reply `r0` | Reply `r1` |
|---|---|---|---|---|---|
| `OP_WRITE` | app → terminal | `req(OP_WRITE, len)` | 0 | bytes consumed | 0 |
| `OP_READLINE` | app → terminal | `req(OP_READLINE, plen)` | 0 | line length | flags |
| `OP_BYTES` | driver → terminal | `req(OP_BYTES, n)` | n bytes, packed LE | 0 | 0 |

- **`OP_WRITE`**: print `len` bytes from the client's output page. The terminal performs
  output-side newline translation (`\n` becomes `\r\n`) and passes everything else, ANSI
  included, untouched: the wire belongs to the application while it is printing. The reply comes
  when the bytes are on the console's side; the output page is the client's to reuse again.

- **`OP_READLINE`**: read one line. The low bits carry a prompt length; the prompt bytes sit at
  the start of the output page and the terminal paints them, followed by any type-ahead the user
  already entered. The reply comes when a completed line is ready: `r0` is its length (the bytes
  are in the client's input page) and `r1` carries the flags below. **At most one read may be
  outstanding per terminal.** A second `OP_READLINE` while one is parked is a protocol violation
  and is refused with `BAD_REQUEST`; the contract is one line reader per terminal, which is what
  a session is.

- **`OP_BYTES`**: the driver half. One to eight raw wire bytes, packed little-endian in the
  second word, replied immediately. A keystroke is one byte and control flow, not bulk, so the
  words-in-registers path fits; a paste drains eight bytes per message, and the `CALL`
  rendezvous is the flow control that keeps a fast sender from outrunning the discipline. The
  driver does no editing, echo, or line assembly; it forwards bytes and nothing else, the way a
  UART driver feeds the Unix tty layer without being the tty layer.

### Read flags (`r1` of an `OP_READLINE` reply)

- `FLAG_EOF` (`1<<0`): end of input (`^D` on an empty line). The line length is 0.
- `FLAG_INTERRUPTED` (`1<<1`): the read was interrupted (`^C`). The line length is 0. This is the
  contract's hook for interrupt routing, whose design is open; see
  [../design/interrupt-routing.md](../design/interrupt-routing.md).

A client that speaks the contract handles both flags. The shell's response is the model: on
`FLAG_INTERRUPTED` it discards and reprompts, on `FLAG_EOF` it notes there is nowhere to exit to
and reprompts. Neither flag carries a signal or a process identity; the terminal reports a fact
about the read, and what to do with it is the client's business.

### `BAD_REQUEST`

`proto::BAD_REQUEST` (`u64::MAX`) is the reply `r0` to a request whose opcode the terminal does
not implement, and to a second concurrent read. A sentinel rather than silence, so a confused
client fails fast instead of hanging on a reply that will never come.

## What a terminal owes a program, and what it does not

Owes:

- **Line discipline on input.** The program calls `OP_READLINE` and receives a finished line. All
  editing (cursor motion, backspace, kill and yank, history) happened on the far side of the
  endpoint; the program never sees a keystroke, an escape sequence, or an echo.
- **Newline translation on output.** A program writes Unix `\n` and the terminal puts a carriage
  return on the serial wire. A program that wants raw control of the wire gets it: everything
  that is not a bare `\n` passes through, so ANSI from the application reaches the screen intact.
- **Type-ahead.** Bytes typed before (or during) a read are buffered and delivered in order, up to
  a bounded queue; past the bound the newest line is dropped with a bell, as a real tty's flooded
  input queue does.

Does not owe:

- **Terminal size tracking.** The redraw math assumes a line fits one row. A line longer than the
  terminal is wide will redraw incorrectly past the margin. `linedisc::LINE_MAX` keeps this rare;
  a full fix needs size negotiation the serial contract does not carry. Honest limit, recorded.
- **Tab completion.** Completion needs the command namespace, which is the application's
  knowledge, not the terminal's. Tab is ignored here and belongs to the shell (milestone 31).
- **More than one concurrent reader.** One session, one line reader (above).

## A known race, carried forward from milestone 10

The first byte of input piped in a single burst at boot can be lost once: the input driver arms
its receive interrupt a few instructions after the FIFO already holds the piped text, and the
leading byte can fall in that window. An interactive user typing after the prompt never hits it,
and every line after the first is intact. Fully closing it needs the driver armed before any
input arrives. Noted, not papered over; see [shell.md](shell.md).

## For milestones 29 and 31

- **29 (display terminal)** implements the *same IPC half* against a framebuffer and a VT engine
  instead of a serial line and this line discipline. The wire half differs (a grid, not a row),
  but a program that speaks `OP_WRITE` / `OP_READLINE` does not change, which is the point of
  writing the contract before the second terminal exists.
- **31 (capability shell)** is a client of this contract. It reads lines through `OP_READLINE`
  and prints through `OP_WRITE`, and it adds the command semantics (completion, grant
  expressions) that the terminal deliberately does not carry.
