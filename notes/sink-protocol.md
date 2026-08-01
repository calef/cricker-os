# The sink protocol: one way to write bytes somewhere

*Milestone 50, the protocol lane. `crates/sink_proto`, `user/src/sink.rs`, the std PAL's
`sys/stdio/cricker.rs`, and `abi::Error::Gone`.*

## The problem, which was not the one anybody expected

The shell has no `|`, `>` or `<`, so the obvious reading is that a pipe is missing. It is not.
`patches/std-cricker/.../pal/cricker/rt.rs` fixes `STDOUT_SLOT = 1` and `sys/stdio/cricker.rs`
implements `println!` as a SEND on that slot, which means **a program's output destination is
already a capability its spawner chose**. Redirection is putting a different capability in that
slot. No kernel change, no pipe object, no `dup2`.

What blocked it was that we had four different protocols for "write these bytes there":

| Sink | Protocol before this lane |
|---|---|
| std `println!` | SEND, register-only, 16 bytes per message, `w0` = length, `w1`\|`w2` = bytes |
| `lineedit` | CALL, shared page, `OP_WRITE`, `r0` = bytes consumed |
| `fs_proto` | CALL, handle plus offset plus shared page, `WRITE` |
| console server | shared page, SEND the length, ACK on a second endpoint |

A child cannot be indifferent to what is in its output slot until those are one protocol, and that
unification is this lane. `>` and `|` are parser work afterwards.

## The shape, and the two decisions inside it

**A sink is an `Endpoint` capability with `WRITE`, and nothing else.**

### Register-only, not a shared page

This is forced, not chosen. Milestone 50's finding is that redirection *is* substituting one
capability in one slot. The moment a sink also requires a page mapped at an agreed virtual address,
substitution stops being one grant and becomes a spawn-time negotiation between the shell, the
writer and the sink, and the finding evaporates. It also decides the pipe: for `a | b` the shell
creates an endpoint, hands SEND to `a` and RECV to `b`, and that is the entire construction. A
page-based sink would make a pipe cost a frame, a mapping in each of two address spaces, and a
revocation record for each.

The cost is 16 bytes per message. That is the three-word fastpath (notes/abi.md §2) and it is what
std's stdout already used, so nothing on the default path got slower; see the benchmarks below.

### SEND, not CALL

The difference is whether the writer learns anything. A CALL would return "bytes consumed", which
for a self-framing message is always "all of them", and it would pay a second IPC hop on the hottest
path in the system to say so. Back-pressure does not need the reply: SEND blocks until a receiver
takes the message, so **the rendezvous is the flow control**, which is the property
`lineedit::proto::OP_BYTES` had already written down.

SEND also makes the reader of a pipe an ordinary program that does nothing but `recv`. With a CALL
protocol every pipe reader would owe a reply, which means every program on the right of a `|` would
have to know it was on the right of a `|`.

### What the protocol deliberately does not carry

**A per-write error code.** A sink that can no longer accept bytes says so by ceasing to exist: it
destroys its receiving end and the writer's next SEND fails. That collapses "the reader exited",
"the device is gone" and "the filesystem is full" into one fact, and the honest caveat is that the
writer learns that the sink is over and not why. It is the right trade for a byte stream, whose only
available response to any of the three is to stop, and it is the trade Unix makes as well: a writer
gets `EPIPE` and never the reader's reason. A destination whose failures a client must tell apart is
not a sink, it is a service, and it should be a CALL protocol like `fs_proto`.

**Types.** This carries bytes. Typed pipelines are a separate and larger fork, recorded as one in
design/roadmap.md's milestone 50 block, and nothing in this framing is a step toward one.

**Seek, truncate, re-read, stat.** A sink appends, and that is the payoff milestone 50 claims over
Unix. `> report.txt` hands a program strictly less than fd 1 with full file semantics does, and it
is the opcode list that makes that true rather than policy.

## The wire

```text
  w0 = (op << 56) | len          w1, w2 = up to 16 bytes, little-endian, low word first

  OP_BYTES = 0    len = 1..=16   the bytes are in w1|w2
  OP_EOF   = 1    len = 0        the writer is finished
```

`OP_BYTES` is **zero on purpose**. With the opcode at zero a bytes message's first word is exactly
its byte count, which is bit for bit the framing std's stdout already sent. So unifying the protocol
changed no instruction on the fastpath, cost no message, and the benchmark that prices `println!`
cannot tell that anything happened. An opcode is a claim about what a message means; the cheapest
claim to make is the one the wire was already making.

`OP_EOF` is new and it is not decoration. Without it a pipe's reader blocks forever after the writer
exits, and "the producer is done" would have to be inferred from a death notification the reader may
not even be the supervisor for, which is a fact about process supervision standing in for a fact
about a stream. std sends it from the PAL's `cleanup`, which std's runtime calls after `main`
returns and after it has flushed stdout.

## "Gone" versus "never had one", which is the point of doing this now

The old code swallowed a failed SEND, with a comment saying a program without a console "just prints
into the void, which is what every OS does to a process whose stdout is closed". That is right for a
program with no console and **wrong for a pipeline**, where `yes | head` must end when `head` exits.
So the protocol needs to tell two failures apart:

- **never had one**: the slot is empty. Keep running; the bytes go nowhere.
- **gone**: there was a sink and it has been destroyed. **End the program.**

### The kernel could not tell them apart, and that was the actual finding

Both arrived as `abi::Error::NoSuchSlot`. A destroyed endpoint leaves the holder's capability in
place (endpoints are named generationally, `crates/slots`), and the failure surfaces when
`sched::take_ipc_aborted` is set; `syscall.rs` mapped that to `NoSuchSlot`, the same value an empty
slot returns. **The only available behaviour was therefore the wrong one for a pipeline**, and no
amount of userspace protocol design could have recovered the distinction, because the fact lives in
the kernel.

So the ABI grew one variant, `abi::Error::Gone` (-11): *the capability names an object that no
longer exists*. It applies to all five endpoint IPC paths (SEND, RECV, SEND_CAP, RECV_CAP, CALL),
because the fact is about the endpoint and not about the direction.

This is the second time a distinction like this came up and it was resolved the other way the first
time. DECISIONS §32 deliberately makes `Endpoint::REAP` return one error for "already collected" and
"not your child", because telling them apart would let a supervisor probe the tid space of children
it has no relationship with. `Gone` carries no such risk: the capability is one the caller already
holds, in its own cspace, so learning that its object died reveals nothing it was not entitled to
know.

### SIGPIPE, arriving through std's own seam

std already has exactly this two-way split and we had been defeating it. `io::stdio`'s `handle_ebadf`
swallows an error for which `is_ebadf` returns true and propagates everything else, and a propagated
error makes `println!` panic. The old PAL returned `true` unconditionally, so every failure was
swallowed. Now:

- **never had one** maps to `ErrorKind::Unsupported`, which `is_ebadf` accepts. That is the same
  answer every other ungranted capability gives in this PAL (`std::fs` without a directory,
  `std::net` without a stack), and it is the honest one: this program was not given an output
  stream.
- **gone** maps to `ErrorKind::BrokenPipe`, which propagates, so `println!` panics with "failed
  printing to stdout: broken pipe". The target is panic=abort, the panic reaches `rt::abort`, and
  the kernel kills the process and attributes the fault.

That is byte for byte what a Rust program on Linux does when its reader exits, because Rust sets
`SIGPIPE` to `SIG_IGN` and lets the `EPIPE` reach the same panic. **A third signal disappears**, on
the same grounds milestone 48 retired `SIGTTIN` and `SIGTSTP`: the question the signal answered is
already answered by who holds what.

## The sinks

`user/src/sink.rs` is one binary with roles, and it is the `fwarden` shape: a caretaker that speaks
the sink contract to its client and the underlying protocol to whatever is behind it.

- **`ROLE_FILE`**: holds an `fs_proto` endpoint and a shared page, creates or opens one name, and
  appends every message's bytes at a running offset. `OP_EOF` closes the handle and reports the
  total. Its client holds an endpoint to this process and nothing that names the FS server, so it
  cannot seek, truncate, re-read or stat, which is milestone 50's "grants strictly less than Unix"
  made structural rather than promised.
- **`ROLE_WRITER`**: the indifferent writer used by the tests. It writes a fixed transcript to
  whatever is in slot 0 and reports the classification it got back, which is how the "gone" path is
  asserted by value.

## What the indifference test proves

`kernel::user::sink_tests`, both ISAs.

The same `hellostd` ELF is spawned twice with **identical grants except for what is behind slot 1**:
once with an endpoint the kernel test receives on directly (the pipe shape: the reader is an
ordinary receiver), and once with an endpoint served by `sink` in `ROLE_FILE`, which writes the bytes
into a file on the real RedoxFS image through the real FS server. The test then reads that file back
and compares it, byte for byte, with the transcript the first arm received.

Same binary, same transcript, two destinations that share nothing but the sixteen bytes of a
message. The program is not told which one it has and has no way to find out.

The `Gone` half is asserted separately and by value, because it is a claim about a number: the
kernel creates an endpoint out of a region it owns, spawns `sink` in `ROLE_WRITER` with WRITE on it,
takes some messages, destroys the region, and the writer reports that its next SEND classified as
`Sent::Gone` rather than `Sent::NoSink`. Without the ABI variant that assertion is not expressible.

## Benchmarks

`println!` is on the ABI fastpath and `relay_rtt` / `call_reply` price exactly the kind of hop this
lane could have added. It added none: `OP_BYTES == 0` keeps the wire identical, and SEND keeps the
message count identical. The measured movement is recorded in notes/benchmarks.md; the summary is
that the default rung costs nothing, which is what milestone 50's own argument requires.

## What is still missing, named where a reader meets it

- **stdin.** `Stdin::read` still returns honest EOF. Both `< file` and a pipe's read end need an
  input-slot convention that does not exist, and this lane did not invent one: the sink contract is
  one-directional by construction and a source contract is its own design.
- **The other two sinks are not converted.** `lineedit`'s `OP_WRITE` and the console server's
  page-plus-ack channel still speak their own protocols to their existing clients. Converting them
  means editing the shell and `sysinit`, which a sibling lane owns; the sink contract is what they
  would be converted *to*, and `ROLE_FILE` proves the shape works against a real backend.
- **No buffering.** A pipe built from this contract is full lockstep, where Unix's 64 KB buffer lets
  a producer run ahead. If buffering earns its place it arrives as a component that speaks the sink
  contract on both sides and is inserted into the chain, not as a redesign.
- **`>>`.** Append is a property of `ROLE_FILE`'s wiring (it starts at the file's current size)
  rather than a mode a client can ask for, because a client of a sink cannot ask for anything.
  Whether append is a mode on open or a property of the sink is milestone 50's later question.
