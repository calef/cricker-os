# 50. Pipes and redirection: one sink protocol, and `|` turns out to be an endpoint

**Status: BUILT.** Closed 2026-08-14 by the integrator on the overnight verification lane's
evidence: every residual the paragraph below listed as open had closed on 2026-08-03 and the record
had not caught up. Buffering was measured and the verdict is build nothing (commit 8c27953;
notes/pipes.md carries the numbers and the honest caveats); the terminal sink adapter is
`terminal_sink_caretaker`, built, wired as the fifth boot component, name ratified (061066e); and
`2>` was decided by Chris and built as the declared second stream (design/decisions/67-second-stream.md).

(The Gate paragraph that stood here described the three residuals; all three closed 2026-08-03,
and a BUILT milestone gates nothing, so it is gone rather than stale.)

**The protocol lane built 2026-07-31** (`crates/sink_proto`, `user/src/sink.rs`, the std PAL's
`sys/stdio`, and `abi::Error::Gone`; concept note: notes/sink-protocol.md). One framing for "write
these bytes there", proven on both ISAs by running one `std_exerciser` ELF against two destinations that
share nothing but sixteen bytes of message and comparing the bytes.

Three things came out of it that were not in the plan below.

- **The kernel could not express the thing the plan required.** "Gone" and "never had one" both
  arrived as `NoSuchSlot`, so no amount of userspace protocol design could have recovered the
  distinction; the ABI grew a variant. That is the finding, and it is why doing this before `|`
  existed was right.
- **`SIGPIPE` needed no new mechanism above the ABI**, because std already splits fatal from
  swallowed print failures through `is_ebadf`, and the old PAL was defeating it by answering `true`
  unconditionally.
- **A sink capability must not double as a terminal-service capability**, which is what stops
  `line_editor` from simply serving the contract on the endpoint it already has: that endpoint also
  carries `OP_READLINE`, so handing it to a child as its output slot would grant the child the
  terminal's *input*. The terminal's sink is therefore a separate endpoint served by an adapter,
  which is the shape `user/src/sink.rs`'s file role proves against a real backend. Converting
  `line_editor` and the console server is left with the shell work, because their clients are the
  shell and `system_initializer`.

**The operators lane built 2026-07-31** (`crates/grant_plan/src/line.rs`, `user/src/wc.rs`, the shell,
`system_initializer`, and two bits on `grant_plan::spawnproto`; concept note: notes/pipes.md). `date | wc` runs at a
real prompt on both ISAs, with the shell minting the endpoint out of its own budget and init putting
it in the child's output slot. The kernel did not change.

Four things came out of it that were not in the plan below.

- **The input slot's shape had to be decided and the smallest answer was the right one**: a source
  is *the sink contract received rather than sent*. No new protocol, and `<` and the right end of a
  `|` become one convention, exactly as `>` and the left end already were.
- **The manifest had to learn that not every program's slot 0 carries bytes.** `worker` answers with
  a `u64` in a register and the interrupt demonstrators hold no output capability at all, so
  `worker 9 > out.txt` would have written an unreadable word into a file with no error anywhere.
  `OutputSpec` makes it a refusal at the prompt. This is a wart the sink protocol inherited rather
  than created: the register fastpath is older than the contract.
- **`InputSpec` produces a refusal Unix cannot.** `wc` with nothing feeding it blocks on a receive
  forever, and on Unix that is a shell that appears to hang, because fd 0 always exists there.
  Here the manifest knows, so the prompt knows.
- **A pipe needs its own untyped region, not just an endpoint.** Deleting every capability naming an
  endpoint does not destroy it (the object lives in a page of a region), so a producer blocked in a
  `SEND` after its reader finished would stay blocked forever. The shell splits a region per
  pipeline and `DESTROY`s it, and that is what turns a dead reader into `Gone`.

**The append lane finished it 2026-08-02** (`line::Mode`, `swish`'s `open_sink`,
`cargo xtask shell-check`). `>>` is the cheapest of the four operators and that is DECISIONS §55
paying out: the shell already backs the file, so append is one bit about how it opens one, and
`grant_plan` asserts that `date > f` and `date >> f` plan to endowments equal in every other field.
That lane also built the gate notes/pipes.md named as the milestone's most valuable missing test:
`script/shell-check` boots `--features shell` on both ISAs and types at the prompt, which is the
only thing in the tree that runs the real `system_initializer`.

**What the following paragraph listed as open is closed; kept for the reasoning, superseded on
the facts** (see the Status block above for where each landed): buffering (a pipeline is full
lockstep and has not been benchmarked against a Unix pipe), the terminal's own sink adapter, and
**`2>`, which is a design fork rather than a task**. This system has no ambient anything, so a
program holds one output endpoint and its diagnostics ride it in-band; a second stream is either a
second capability in a second slot (Unix's fd numbering with a capability underneath, and it forces
a numbered slot convention first) or a second opcode on the one endpoint (§51 intact, but a
diagnostic then flows down a `|` into a `wc` that would count it). notes/pipes.md weighs both. What
is already separated is the half that hurts most on Unix: the shell's own refusals never enter a
redirection, because the shell is a different process and its output was never in the substituted
slot.

The paragraphs below are the design as it stood before either lane; where they differ from the two
notes, the notes are what was built.

**In brief.** The shell has no `|`, `>`, or `<`, and a shell without them is not a shell. The
surprise on investigating is that **the mechanism is already built** and the missing piece is
somewhere else entirely: the work is unifying the four byte-sink protocols we already have, after
which the pipeline operators are parser changes and wiring.

**Why it matters.** Pipelines are Unix's composition primitive and the reason the shell is worth
having. They are also the place where the capability model gets to show a result that is *better*
rather than merely equivalent, which is what a demonstrator exists to produce.

## The finding: stdout is already a capability in a slot

`patches/std-nife/.../pal/nife/rt.rs` fixes `STDOUT_SLOT = 1`, and `sys/stdio/nife.rs`
implements `println!` as a SEND on that slot. So **a program's output destination is a capability the
spawner chose**, and redirection is putting a different capability in that slot. No kernel change, no
new object, no `dup2`. The existing doc comment even anticipates the case: a failed SEND is swallowed
so "a program without a console still runs, it just prints into the void".

The same is true at the other end of the design. `line_editor::proto::OP_BYTES` already documents
`the rendezvous is the flow control`, which is exactly a pipe's back-pressure story.

## A pipe is an endpoint, not an object

For `a | b`, the shell creates an endpoint, grants SEND to `a` as its output slot and RECV to `b` as
its input slot, and spawns both. **Nothing is added to the kernel.** Unix needs a pipe object with a
64 KB buffer because fds are anonymous and the kernel has to decouple two parties who cannot name
each other; here the shell names both, so the rendezvous is the pipe.

The cost is honest and should be measured rather than argued: this is **full lockstep**, where Unix's
buffer lets a producer run ahead. The reply is that IPC speed is the thing this project has spent
itself on, so measure `a | b` throughput and only then decide. If buffering earns its place it
arrives as **a component that speaks the sink protocol on both sides** and is inserted into the
chain, not as a redesign. An optimization that is a process is the shape a microkernel wants.

## SIGPIPE becomes a return code, the same way `SIGTTIN` disappeared in 48

`yes | head`: `head` exits, its endpoint dies, and the producer's next SEND fails. Unix needs a signal
because there is no other way to tell a writer that an anonymous fd is gone; §16 revocation already
destroys the capability and the failure arrives as an error return. **A third signal disappears**, on
the same grounds milestone 48 retired `SIGTTIN` and `SIGTSTP`: the question the signal answered is
already answered by who holds what.

This forces one concrete change. Today's swallow ("print into the void") is right for a program with
no console and **wrong for a pipeline**, where a dead reader must end the writer. So the sink protocol
needs a distinguishable "gone" versus "never had one", and std's `Stdout::write` must stop discarding
the result.

## The actual work: four sink protocols, one needed

| Sink | Protocol today |
|---|---|
| std `println!` | SEND, register-only, 16 bytes/msg, w0 = len, w1\|w2 = bytes |
| `line_editor` (crate and component) | CALL, shared page, `OP_WRITE`, r0 = bytes consumed |
| `fs_proto` | CALL, handle + offset + shared page, `WRITE` |
| console server | shared page, SEND length, ACK on a separate reply endpoint |

Four shapes for "write these bytes there". A child cannot be indifferent to what is in its output
slot until they are one, and **that unification is the milestone**. The precedent is
`fs_file_caretaker`, which is a caretaker precisely because it "serves the same `fs_proto` protocol
its own client speaks": narrowing preserves the protocol, so a pipe, a file, and a terminal become
substitutable.

## The result that is better than Unix, and worth stating plainly

`>` and `file:PATH` must stay **different mechanisms**, and the difference is the payoff. `file:report.txt`
grants the *program* a file its manifest declared it wanted (milestone 31, a capability shell; the
filesystem contract it grants against is §27). `> report.txt` substitutes the
*stream the shell owns*, and the program never holds a file capability at all: it cannot seek, cannot
truncate, cannot re-read, cannot stat. It can append bytes to a sink. **Unix hands the same program fd
1 with full file semantics**, so our redirection grants strictly less than Unix's while doing the same
job.

Keeping them distinct is also what keeps the manifest meaningful: `run worker 5 > out.txt` must not
become a way to route around `FileSpec::Forbidden`, and it does not, because the sink is not a file
grant.

The demo that shows it: **`caps` can print where your output goes** ("output: terminal" versus
"output: file report.txt, append-only"), because the destination is a capability rather than an
integer with a convention attached.

## What is genuinely missing

- **stdin.** `sys/stdio/nife.rs` returns honest EOF because "nothing grants a std program input
  yet". Both `< file` and a pipe's read end need an input-slot convention that does not exist.
- **`>>`.** Append is expressible with `FSTAT`/`SIZE` then write-at-offset, but that is racy if the
  file is shared. Decide whether append is a mode on open or a sink property; do not over-solve it.
- **Multi-stage pipelines.** `a | b | c` is two endpoints, not one, and the shell's spawn path builds
  one child at a time today.

**Sequencing.** The protocol unification is independent of 47 and 48 and could start now; the parser
work wants 47's tokenizer changes, and the "who holds the terminal" story is shared with 48, so the
natural order is unify the protocol, then `>` and `<`, then `|`, then revisit 48's `fg` with pipelines
in hand. **Effort: 3 lanes estimated** (protocol, redirection, pipelines), noting estimates for
unbuilt work are guesses on a history-calibrated scale, and that the unification is the one most
likely to surprise.
