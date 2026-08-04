# 51. The sink protocol: a writer must not be able to tell what it is writing to

Milestone 50. `crates/sink_proto`, `user/src/sink.rs`, `kernel::user::start_file_sink`, and
`abi::Error::Gone`. See `notes/sink-protocol.md`.

**A program's output is an endpoint, and the program cannot learn what is behind it.** One framing,
"write these bytes there", serves a file, a pipe, and a terminal alike.

## Why indifference is the decision and not a side effect

`>` and `|` look like two features and are one. Unix gets that from the file descriptor: a process
writes to fd 1 and the shell decides what fd 1 is. We get it from a capability, which is the same
trick with the ambient part removed. The program does not name a destination, cannot enumerate
destinations, and holds exactly one endpoint that it may write to.

That is worth stating as a property rather than an implementation note, because it is what the
milestone is *for*: the acceptance test is one unmodified ELF, run against two destinations,
producing identical bytes. A design where the program could ask "am I a pipe?" would pass every
functional test and fail the claim.

## The surface

`OP_BYTES = 0` carries bytes, `OP_EOF = 1` ends the stream, and the operation rides in the top byte
of the request word (`OP_SHIFT = 56`). Up to `INLINE_MAX = 16` bytes travel in registers, which is
the common case for a shell pipeline's small writes; more goes through the shared page.

## `Gone` (-11) is SIGPIPE as a return value

If a writer cannot tell what it is writing to, it also cannot tell when the reader stopped caring.
Unix answers with `SIGPIPE`, an asynchronous signal that kills the process by default. **We have no
signals and are not adding any** (§24 decided Ctrl-C without them), so the same fact has to be
expressible as a return.

`abi::Error::Gone` says the sink existed and has been destroyed. It is a *return value*, so a writer
that ignores it keeps running rather than dying at an arbitrary instruction, and one that checks it
gets to exit cleanly. This is the better shape and the credit is not ours: SIGPIPE's default-kill is
widely regarded as a wart, and it exists because Unix had no other way to interrupt a blocked write.

`sink_proto` restates the value as `GONE: i64 = -11` rather than depending on `abi`, keeping the
protocol crate dependency-free per §46.

## BUGS

- **A sink is not a file and does not seek.** The protocol expresses append and end-of-stream, which
  is what a pipeline needs and less than a file offers. A program that needs to seek needs the file
  contract, and it will then know it has a file, which costs it the indifference above. That trade
  is real and unresolved.
- **Nothing yet proves the claim end to end.** The protocol, both ends, and a read-back landed
  together; the operators that would let a *user* compose two programs did not. Until `ls | wc` runs
  at the prompt, indifference is demonstrated by a test rather than exercised by a shell.
