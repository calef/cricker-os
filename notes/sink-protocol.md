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
| `line_editor` | CALL, shared page, `OP_WRITE`, `r0` = bytes consumed |
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
`line_editor::proto::OP_BYTES` had already written down.

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
design/roadmap/50-pipes-and-redirection.md, and nothing in this framing is a step toward one.

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

`user/src/sink.rs` is one binary with roles, and it is the `fs_file_caretaker` shape: a caretaker
that speaks the sink contract to its client and the underlying protocol to whatever is behind it.

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

The same `std_exerciser` ELF is spawned twice with **identical grants except for what is behind slot 1**:
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
lane could have added. It added none, and the numbers say so rather than the argument doing it.
Deterministic icount ticks against `bench/baseline-aarch64.txt`, on the pinned QEMU:

| | aarch64 | riscv64 |
|---|---|---|
| `ipc_rtt_el0` (an EL0 send/receive round trip, the closest thing to a `println!`) | +0.13% | -0.63% |
| `relay_rtt` (a hop through a userspace relay) | +0.01% | +0.02% |
| `call_reply` | 0.00% | +0.00% |
| `null_syscall` | +0.00% | -0.04% |

Everything moved by less than 1% and in both directions, which is build noise on an instruction
count. **The baseline file was deliberately not updated**, because updating it is a statement that a
performance change was intended, and there was none.

Two honest caveats. There is **no benchmark that prices `println!` itself**; what these price is the
`SEND` that `println!` is, so a regression inside the PAL's chunking loop would not appear here.
And the reason the fastpath is untouched is structural rather than lucky: `OP_BYTES == 0` keeps the
wire identical, `SEND` keeps the message count identical, and the kernel's only change is on the
*failure* return of an aborted send, which no benchmark takes.

## What is still missing, named where a reader meets it

- **stdin.** `Stdin::read` still returns honest EOF. Both `< file` and a pipe's read end need an
  input-slot convention that does not exist, and this lane did not invent one: the sink contract is
  one-directional by construction and a source contract is its own design.
  *(Answered by the operators lane the same day: a source is the sink contract received rather than
  sent. See notes/pipes.md.)*
- **The terminal is a sink now** (`user/src/terminal_sink.rs`, 2026-08-03), and it took one new
  opcode and one process. The analysis this bullet used to carry was right about the shape and
  wrong about the cost; see "The terminal's sink adapter" below.
- **The console server's page-plus-ack channel is untouched**, and after building the adapter that
  looks like the right answer rather than a gap. `line_editor` is its only client and now speaks for
  two writers; a second client of the *console* would hit the same one-page wall one layer down,
  with nothing gained.

## The terminal's sink adapter, and the wall it hit

The last of milestone 50's remainders. A program's output slot can now hold **the terminal**, and it
still cannot tell that from a pipe or a file.

```text
  a declaring child ──sink_proto SEND──► terminal_sink ──OP_PRINT CALL──► line_editor ──► console
```

### It is a process for a capability reason, which was known

The cheap move is to have `line_editor` serve the sink contract on the endpoint it already has: a
`SEND` arrives there with no reply capability, so it is trivially distinguishable from the `CALL`s it
serves. **That is wrong.** That endpoint also carries `OP_READLINE`, and `WRITE` on an endpoint is
the right to `CALL`, so a child handed it as its output slot would hold the terminal's **keyboard**.
A sink capability that can read the keyboard is not a sink capability. This kernel offers no
receive-on-a-set, so the terminal cannot serve two endpoints itself, and the answer is a separate
process holding both contracts and handing out only one.

### And it needed a new opcode, which was not known

The plan said the adapter would be `ROLE_FILE`'s shape and that the work was rewiring init. Building
it found something else: **`OP_WRITE` reads from the client's output page, and there is exactly one
of those.** init maps a single frame into `line_editor` read-only and into the shell read/write. A
second page-based client needs a second frame and a page index in every request, which is `fs_proto`'s
one-page-two-clients problem (DECISIONS §55, the reason the file behind a `>` is the shell itself)
arriving in a second contract.

So the terminal contract grew **`OP_PRINT`**: up to eight bytes carried in the request's own words,
replied when they are on the wire. The adapter then needs **no page at all**, and `expand_output` is
shared with `OP_WRITE`, so a newline from an adapter gets the same manners as a newline from the
shell. Two writers, one terminal, one set of manners, no second frame.

Eight rather than sixteen is the contract's request shape and not a choice: a served request arrives
through `recv_cap`, which hands the server a reply capability and **two** data words. `OP_BYTES`
carries eight for the same reason, from the input direction, which is why the shape was already
there to copy.

### What it is for

DECISIONS §67's declared second stream. A program that declares diagnostics gets this endpoint in
its declared slot by default, endowed by init from the manifest exactly as the clock is, and `2>`
replaces it with a file the shell backs. So a `date` complaining about a missing clock reaches the
screen **without passing through the shell at all**, which is stronger than the shell printing it:
nothing the shell does to the output can touch those bytes, and `caps` says so in its `diags` row.

`kernel::user::sink_tests::the_terminal_is_a_sink_like_any_other_and_the_writer_cannot_tell` is the
proof, and it is the indifference claim made a third time: the same `sink` writer ELF, the same
transcript, a pipe and a file and now a terminal, and the program holds one capability in each case.

### BUGS

- **It is one process per system, not one per client.** Every declaring child writes to the same
  endpoint, so two of them talking at once interleave at message granularity. That is what a shared
  fd 2 does on Unix too, and there is nothing here that could do better without a second adapter and
  a way to decide which one a child gets.
- **The bytes bypass the shell entirely, which is the feature and also the limitation.** A shell
  cannot capture, indent, count or truncate them. `2>` is the only way to put them anywhere else, and
  it works by handing the child a *different* endpoint rather than by intercepting this one.
- **Building it found init's sixteen-slot cspace for the third time.** One more endpoint held across
  the shell's `build_child` made the boot print nothing at all, so the adapter is built **after the
  shell**; see notes/pipes.md. It was written down as "built last", and merging milestone 22 proved
  that half wrong: init now builds a `job_undertaker` after it and the cspace has room either way. The
  real constraint was never the ordinal, it was the shell's build. Where the adapter does have to sit
  is **before init gives the construction budget away**, because it is a system component and that
  budget is what the system is built from; building it afterwards would spend init's scratch pool on
  a whole program. See notes/trusted-init.md.
- **`date` was already speaking the contract before it existed**, which is the `OP_BYTES == 0`
  decision paying out immediately: its hand-rolled framing is bit for bit a `BYTES` message. It
  announces no end of stream, because nothing yet reads its output as a stream; when `|` lands, it
  will need to.
- **No buffering, and it is now measured rather than argued.** A pipe built from this contract is
  full lockstep, where Unix's 64 KB buffer lets a producer run ahead. `bench: sink_throughput`
  (`kernel/src/bench.rs`, two EL0 processes over one endpoint) and
  `bench/host/pipe_throughput.rs` are the two halves of the comparison; the numbers and what they
  decided are in notes/pipes.md.

- **A second stream is a declaration** (DECISIONS §67, 2026-08-03). A program that has diagnostics
  declares a second output in its manifest and gets a second endpoint speaking **this same
  contract**; `2>` names where those bytes go. Nothing about the framing changed, which is the
  measure of whether the contract was right: a diagnostic is bytes, and the only thing that makes
  one a diagnostic is which endpoint it is on. See notes/pipes.md.
- **`>>`.** Append is a property of `ROLE_FILE`'s wiring (it starts at the file's current size)
  rather than a mode a client can ask for, because a client of a sink cannot ask for anything.
  Whether append is a mode on open or a property of the sink is milestone 50's later question.
