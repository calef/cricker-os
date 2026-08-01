# Pipes and redirection: `>`, `<` and `|` are one substitution

*Milestone 50, the operators lane. `crates/capsh/src/line.rs`, `user/src/wc.rs`, `user/src/shell.rs`,
`user/src/sysinit.rs`, `crates/capsh/src/spawnproto.rs`. The protocol half is
notes/sink-protocol.md and you should read that first.*

## What this lane had to add, which was less than it looks

The protocol lane established that a program's output destination is **a capability its spawner
chose**, and unified the four "write these bytes there" protocols into one. After that, `>` and `|`
are not two features. They are two spellings of *put a different capability in slot 0*, and the
whole of this lane is the grammar that lets a person choose it and the wiring that carries the
choice from the prompt to the child.

The kernel did not change. No new object, no new syscall, no `dup2`, no pipe buffer.

## The one-line summary of the mechanism

```text
  date | wc

  shell: SPLIT a region off its own budget
         RETYPE one page of it into an Endpoint          <- this is the pipe
         spawn date, delegating the endpoint  WRITE      <- into date's slot 0
         spawn wc,   delegating the endpoint  READ       <- into wc's slot 1
         read wc's answer off its own result endpoint
         DESTROY the region                              <- this is what ends a stalled writer
```

`date` is not recompiled, not told, and cannot ask. The endpoint in its slot 0 is the same kind of
object that was there before, held with the same right.

## The grammar, and the three rules that are not Unix's

`crates/capsh/src/line.rs` splits a line into stages and the names on the ends. It is host-tested and
runs in milliseconds; that is where nearly all of this lane's tests are.

```text
line  := stage ('|' stage)*
stage := <command words> ('<' name | '>' name)*
```

Three refusals that bash does not make, each because a line should mean what it looks like:

- **A redirection goes last in its stage.** `wc < report.txt`, never `< report.txt wc`. Bash accepts
  the second; refusing it is what lets a stage's command text be a *slice* of the line rather than
  something reassembled, which matters in a shell with no allocator.
- **Only the first stage takes input and only the last is redirected.** `date > f | wc` says where
  `date`'s bytes go and then pipes them somewhere else. That is two answers to one question, so it
  is refused rather than resolved by a precedence rule nobody should have to remember.
- **A redirection names one file.** `date > *.txt` is refused where the token is read, not expanded
  and then counted. Even a pattern that matched exactly one name would make what the line writes to
  depend on what is in the directory.

## The two manifest declarations, which were not in the plan

### `OutputSpec`: not every program has bytes to redirect

This system has **two** conventions for "a program said something", and only one of them can be
redirected:

| | what slot 0 carries | can it be `>` or `\|`? |
|---|---|---|
| `worker`, `budgeter` | a `u64` answer in a register | no |
| `heeder`, `spinner` | nothing; they report through a shared frame | no |
| `date`, `wc`, `rm` | the sink contract's byte messages | yes |

`worker 9 > out.txt` would otherwise put a raw word into a file sink, producing a file with nothing
legible in it and no error anywhere. Declaring the convention makes it `Refusal::NotAByteStream` at
the prompt. Unix has no equivalent because there every program's stdout is bytes by construction;
here the register fastpath is real and older than the sink contract, and the manifest is where the
two are told apart.

### `InputSpec`: the refusal Unix cannot produce

`wc` reads a stream. A `wc` with nothing feeding it blocks on a receive **forever**, and on Unix
that is a shell that appears to hang, because there fd 0 always exists and "nobody is ever going to
write to it" is not a property of the command line. Here it is: `wc` declares
`InputSpec::Required`, so a line that gives it neither a `<` nor a pipe is refused before anything
is spawned.

The mirror holds too. `date < report.txt` is `Refusal::InputForbidden`, on the same grounds an
unplaceable token is refused: authority that moved for no reason.

## The input slot's shape, which this lane had to decide

The protocol lane left it open ("both `< file` and a pipe's read end need an input-slot convention
that does not exist"). The decision is the smallest one available:

> **A source is the sink contract received rather than sent.** An endpoint the program holds with
> `READ`, on which `OP_BYTES` messages arrive until `OP_EOF`.

No new protocol, no new opcodes, no reply. Three consequences fall out and all three are wanted:

1. **`<` and the right-hand side of `|` are the same convention**, exactly as `>` and the left-hand
   side of `|` are. A program that can be piped into can be redirected into, with no second code
   path.
2. **A source's producer is an ordinary writer.** A file behind a `<` is a process that opens the
   file and writes the sink contract at it, which is what `user/src/sink.rs`'s verify role already
   was. The shell itself is a producer when a builtin leads a pipeline.
3. **`OP_EOF` becomes load-bearing rather than tidy.** A reader has to be told the producer is
   finished; inferring it from a death notification would be a fact about process supervision
   standing in for a fact about a stream. This is why `date` gained an end-of-stream message.

The asymmetry with `OutputSpec` is real and worth naming: output has three shapes because the system
grew two of them before the sink contract existed, and input has one because **nothing read a
stream at all until this milestone**. There was never a chance for a second convention to establish
itself.

## The wire, and the one thing init had to learn

`capsh::spawnproto` grew two bits in the request word and two positions in the delegation order:

```text
  w2 = mem_pages | INTERRUPTIBLE_BIT | SINK_BIT | SOURCE_BIT

  then, over SEND_CAP, in this order:
    job untyped, job frame   (if INTERRUPTIBLE)
    the sink                 (if SINK)          -> the child's slot 0
    the source               (if SOURCE)        -> the child's slot 1
    the --mem untyped        (if mem_pages > 0)
```

Order rather than tags, because both sides read the same word: a `SEND_CAP` nobody expects and a
`RECV_CAP` nobody answers each deadlock both parties.

**The rights are narrowed per direction and that is load-bearing.** A pipe's write end travels as
`WRITE|GRANT` and its read end as `READ|GRANT`, and init inserts them as `WRITE` and `READ`. So the
program on the right of a `|` **cannot write back up its own input**. Nothing in either program
enforces that; the capability it holds simply cannot express it.

**One ack was added.** A child whose output was substituted owes the shell no answer, because its
answer is going somewhere else, so a failed spawn would be invisible and the pipeline would wait on
a producer that does not exist. `SPAWN_OK` on the result endpoint closes that. An unredirected spawn
is unchanged.

## A builtin can lead a pipeline, because the shell can be a writer

`echo hello world | wc` runs with **one** spawned process in it: the shell mints the endpoint, spawns
`wc` with the read end, and then writes `echo`'s bytes into the write end itself.

That costs no new mechanism, and it is the register-only sink contract paying out: being a writer
needs one capability and nothing else, so anybody can be one. The builtins already rendered their
output through a callback rather than through `print`, because the witness roles have no terminal;
that was done for testability and it means `echo`, `ls` and `pwd` feed a pipe with no branch in any
of them.

`ls | wc` therefore works, in a shell that holds a directory. The interactive boot does not; see
BUGS.

## SIGPIPE, and why the pipeline gets its own region

Deleting every capability that names an endpoint does **not** destroy the endpoint: the object lives
in a page of an untyped region, and only reclaiming the region frees it. So a pipeline whose reader
has finished while its writer is still blocked in a `SEND` would leave that writer blocked forever.

Each pipeline therefore takes its own region, split off the shell's budget, and the shell `DESTROY`s
it when the line is over. That is what turns a producer's next `SEND` into `abi::Error::Gone`, which
is `SIGPIPE` as a return value. The classification itself is asserted by value in
`kernel::user::sink_tests`.

## EXAMPLES

At the prompt, in the interactive boot:

```text
$ echo hello world | wc
  1 2 12

$ date
  date: the time is unknown: this process holds no clock capability
$ date | wc
  1 10 63

$ echo hello world | wc | wc
  1 3 7

$ wc
  wc: reads an input stream: give it one with '< name' or a pipe, or it waits forever

$ worker 9 | wc
  worker: does not write a byte stream, so there is nothing for > or | to redirect

$ date | date
  date: reads no input; there is no slot for those bytes to go in

$ caps date | wc
  date would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    output   an endpoint into the next stage. no file, no buffer, no object:
             the rendezvous IS the pipe
    ...
  wc would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    output   this shell's result endpoint (it reads the bytes and prints them)
    input    the previous stage's output
```

That last one is the demonstration the milestone owed: **`caps` can print where your output goes**,
because the destination is a capability rather than an integer with a convention attached. On Unix
the same question has no answer at that point, since fd 1 is whatever the shell's fd 1 happened to
be and nothing records what that was.

## What the guest test proves, on both ISAs

`kernel::user::pipeline_tests` wires the **real shell binary** in a role that reads a script instead
of a keyboard, with the interactive endowment slot for slot. The kernel plays the two parties on the
other ends: it serves the terminal contract and collects every byte the shell prints, and a second
thread serves `capsh::spawnproto` as init.

So the assertions are made against **what a person would see**, and the headline one is not a
constant:

- `date` alone: the shell prints N bytes.
- `date | wc`: `wc` reports N bytes.

Same ELF, same argument, two destinations, and the second number has to be the first one's length.
Comparing against the observed first arm rather than a literal is what makes it hold whether or not
the boot has a clock and whatever `date` decides to say.

### And the same claim on the input side

`kernel::user::sink_tests::one_reader_two_sources_and_the_same_answer` is the mirror, and it is what
says the source convention is real rather than merely chosen. One `wc` ELF, spawned twice with
identical grants except for what is behind slot 1:

- **a pipe**: the kernel sends the transcript on an endpoint itself, sixteen bytes at a time, then
  `OP_EOF`. That is exactly what a program on the left of a `|` does.
- **a file**: the same transcript is written into a real file on the real RedoxFS image by `sink`'s
  file role, then read back out by its source role, which streams it over the same contract. That is
  `wc < report.txt` minus the shell that would name the file.

The second arm crosses two userspace processes, an FS server, a block server and a virtio disk; the
first does not leave the kernel's address space. The answers must be equal, **and** must equal what
the transcript actually is, because two arms broken the same way would satisfy equality on their
own.

## BUGS, named where the reader meets them

- **`>` and `<` cannot be run from the interactive prompt.** They parse, plan, preview and refuse
  correctly, but the boot that starts the interactive shell wires no filesystem service, so the
  shell holds no directory to resolve a redirection against and the answer is the same "you hold no
  such capability" a named file gets. **Both mechanisms are proven** against a real RedoxFS image in
  `kernel::user::sink_tests` (a program writing into a file sink, and a program reading out of a
  file source), so what is missing is a **boot in which one shell holds both a filesystem and a
  spawn channel**. That is a wiring job in `sysinit`, not a design question. Until then the honest
  statement is: `|` runs at the prompt, `>` and `<` do not, and the parts they would be built from
  each work.
- **A `<` adapter cannot yet be told which file to open.** `user/src/sink.rs`'s source role opens
  the one name in `sink_proto::fixture`, because a name rides in two `START` argument words and the
  three-word ABI has no room left once the role selector and the spec are in. The shell would need
  to hand it the name the way `fwarden` is handed one, through a grant spec rather than through
  `START`. Nothing about that is hard; it was simply not needed while the prompt cannot reach it.
- **The guest test's init is the kernel, not `sysinit`.** It serves the same protocol, and the shell
  cannot tell the difference, but it is not the same code: a change to `user/src/sysinit.rs` that
  broke the spawn path would not fail `pipeline_tests`. The `--features shell` boot is what
  exercises `sysinit`, and nothing gates it.
- **Slot 1 is the input source or the `--mem` untyped, whichever the request carries.** That is
  unambiguous only because no manifest declares both, and `capsh` is where that stops being true. A
  program endowed a budget *and* an input needs a numbered slot convention rather than an ordered
  one.
- **A pipeline is full lockstep.** There is no buffer: every sixteen bytes is a rendezvous. Unix's
  64 KB pipe buffer lets a producer run ahead; this does not, and nothing here has been benchmarked
  against a Unix pipeline. If buffering earns its place it arrives as a component that speaks the
  sink contract on both sides and is inserted into the chain.
- **No `>>`, no `2>`.** Every sink this contract can build appends already (a sink has no seek), so
  `>` and `>>` would differ only in whether the file is emptied first, which is the sink's wiring
  rather than the writer's business. `2>` needs a second output slot, and inventing that convention
  before a program has two things to say would be inventing it rather than discovering it.
- **No quoting anywhere in this shell**, so a file whose name contains `>` cannot be named. That is
  a gap in the tokenizer, not in the operators.
- **`wc` has no `-l`, `-w` or `-c`.** It prints all three, because selecting among them is
  formatting and formatting belongs downstream.
- **A `date` whose reader stopped early stays parked.** `date`'s end-of-stream message is a
  rendezvous send like any other, so a reader that took its line and stopped leaves that process
  blocked until its region is reclaimed. Inside a pipeline the region is destroyed and it ends; in
  `kernel::user::date_tests`, which read a line and stop, it does not. Blocked, not spinning, and
  the suite's leaked-thread gate is about runnable threads.
