# Pipes and redirection: `>`, `<` and `|` are one substitution

*Milestone 50, the operators lane. `crates/grant_plan/src/line.rs`, `user/src/wc.rs`, `user/src/swish.rs`,
`user/src/system_initializer.rs`, `user/src/hello.rs`, `crates/grant_plan/src/spawnproto.rs`,
`script/shell-check`. The protocol half is notes/sink-protocol.md and you should read that first.*

**All four operators run at a real prompt on both ISAs.** `|` landed first; `>` and `<` needed a
boot in which one shell holds both a filesystem and a spawn channel, and building that turned up a
reason the file end of a redirection cannot be a separate process. That finding is the section
["The file behind a `>` is this shell"](#the-file-behind-a--is-this-shell-and-that-was-not-the-plan),
and it is the most reusable thing in this note. `>>` came last and is the cheapest of the four,
which is that finding paying out: the shell already holds the file, so append is one bit about how
it opens one.

`2>` is **not** built, and the reason is the interesting half of this note's second half: this
system has no ambient anything, so there is no second stream for a `2` to name. See
["`2>`: an open fork"](#2-an-open-fork-and-the-question-is-not-the-operator).

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

`crates/grant_plan/src/line.rs` splits a line into stages and the names on the ends. It is host-tested and
runs in milliseconds; that is where nearly all of this lane's tests are.

```text
line  := stage ('|' stage)*
stage := <command words> ('<' name | '>' name | '>>' name)*
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

`grant_plan::spawnproto` grew two bits in the request word and two positions in the delegation order:

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

`ls | wc` therefore works, in a shell that holds a directory, and since milestone 50's second half
the interactive boot grants one.

## The file behind a `>` is this shell, and that was not the plan

The plan was the obvious one: `sink.rs`'s file role is an adapter that holds an FS session and
serves the sink contract, `sink_tests` proves it against a real RedoxFS image, so the shell asks
init to build one per redirection and hands the child the endpoint. **That does not work, and the
reason is worth more than the feature.**

`fs_proto` shares **one page** between the FS server and its clients (`fs_service`'s
`FILE_VA_CLIENT`; the server maps the same frame at `FILE_PAGE`). A client stages bytes or a name in
that page and *then* calls, so its use of the page straddles the call boundary. Two client
**processes** doing that at once race, and nothing in the contract orders them: there is no lock, and
a rendezvous on the FS endpoint cannot span a put-then-call pair.

That is survivable for `date > out.txt`, where the shell touches no file while the adapter writes.
It is not survivable for `ls > out.txt`, which is exactly a line where the shell must read the
filesystem **while** the redirection is being written: `ls` reads a page of directory entries, hands
a name to the writer, and comes back for the next round. Interleave an adapter's `put` with that and
the listing and the file are both corrupt, silently.

The note that recorded this hazard first is `fs_service::wait_for_caretaker`, which found the
**startup** half (a client that already exists writes over the name a caretaker staged). This is the
steady-state half, and it has no ordering fix, because there is no moment when both parties are
done.

So the shell backs both ends itself. It already holds the directory capability; it is the one
process that can write the file without opening a second session.

```text
  date > out.txt      date's slot 0 holds the shell's result endpoint, exactly as an
                      unredirected `date`'s does. The shell drains it into a file
                      instead of onto the terminal.

  wc < out.txt        the shell mints an endpoint, gives `wc` READ on it, opens the
                      file and streams it over the sink contract. Same code path a
                      builtin producer already took.

  ls > out.txt        no process is spawned at all. The shell is the producer and
                      the shell is the sink.
```

**This costs the milestone nothing, and that is the test of whether it is the right shape.** What a
redirected program holds is unchanged: one endpoint, `WRITE`, no way to ask what is behind it. There
is no new message, no change to `grant_plan::spawnproto`, and no change in init. `Sink::File` and
`Source::File` still exist in the plan, because the manifest check needs them (`worker 9 > out.txt`
is still `NotAByteStream`), and the wiring simply does not need a capability for them.

It is also the smaller claim, honestly stated. `> report.txt` still grants strictly less than Unix's
fd 1, but the sentence is now "the program holds an endpoint and the shell holds the file", not "an
adapter process holds the file". The adapter shape is still real and still proven; it is the right
answer when the client is **not** the shell, which is what `sink_tests` measures.

### What it costs, named where you meet it

- The shell is single-threaded, so it is inside the drain for the whole of a redirected command.
  That is what an unredirected command already did.
- Every byte crosses the shell's address space twice. There is no benchmark for this.
- A `>` cannot outlive the line, because the thing writing the file is the prompt.

### And what it pays for: `>>` is one bit, in one process

`>>` is the first thing built on that shape, and it is the test of it. Because the shell backs the
file, append is **a decision the shell makes when it opens one**, and everything else is untouched:

```text
  >   CREATE the name; if it exists, OPEN and TRUNCATE it        offset starts at 0
  >>  CREATE the name; if it exists, OPEN and FSTAT it           offset starts at the size
```

That is the whole diff on the wiring side. `FileOut` already carried an absolute running offset,
because the FS contract's `WRITE` names a position rather than advancing a cursor, so "append" is an
**initial value** rather than a mode the filesystem has to hold. There is no `O_APPEND`, and there
is nothing for one to mean: a sink has no seek, so every writer appends already and the only
question a `>` ever answered was what happens to the bytes that were there first.

What the child holds does not move, and `grant_plan` asserts that rather than asserting it in prose:
`append_and_truncate_plan_the_same_endowment` plans `date > f` and `date >> f` and compares the two
endowments whole, with the one differing field made equal. They designate the same `FileGrant`, and
the mode rides **beside** `Sink::File` rather than inside the grant, because the grant is the
authority and the open mode is not part of it.

`>>` is also the reason the truncate is worth stating out loud. `>` empties the file **before the
command runs**, which is what makes `ls > out.txt` report one more name than the `ls` before it did,
and a `>` that had quietly appended to whatever the last run left behind would have been `>>`
wearing `>`'s spelling.

Two rules fall out and neither needed code. **`>>` inherits every rule `>` has**, because it is the
same operator: the tail of the pipeline only, one per stage, one name, and a name that is not a
pattern. And **there is no here-document**: `<<` is refused, because the second `<` is read as the
operator it is and there is then no name after it. The message is about the missing name where the
mistake was a missing feature, which is a wording gap and is in this note's BUGS.

## `2>`: a closed fork, decided as a declaration (DECISIONS §67)

**Decided 2026-08-03**: option (c) below, the manifest declaration; the analysis that follows is
the record of the fork as it stood open, kept because the reasoning is the reusable part. The
build lands with milestone 50's closure.

**Not built at this note's last update, deliberately.** `>>` was an implementation task and `2>` is not, and the difference is
worth stating because it is the same difference this project keeps meeting: one is a spelling for
something the system already has, and the other would create the thing it is a spelling for.

### There is no second stream today, and that is a fact rather than an omission

A cricker-os program holds **one** output endpoint, in slot 0, placed there by its spawner. Its
diagnostics travel on it, in-band with everything else:

```text
  date: the time is unknown: this process holds no clock capability
```

is `date` writing sink messages on the same endpoint it would have written a timestamp to
(`user/src/date.rs`'s `line`). `rm`'s header says the same thing in its own words: "slot 1: a report
endpoint, `WRITE`. Diagnostics and `-v` lines as framed text". One channel, two kinds of thing on it,
and no way to tell them apart at the far end.

So `date > when.txt` on a machine with no clock writes the complaint **into the file**, which is
exactly the loss `2>` exists to prevent on Unix.

### But the half that hurts most on Unix is already separated here

The thing a person usually reaches for `2>` to save is **the shell's own refusals**, and those never
enter a redirection here. `wc < nosuch.txt` is `Say::Failed` printed by the prompt; `worker 9 >
out.txt` is `Refusal::NotAByteStream` printed by the prompt; a spawn that fails is
`spawnproto::SPAWN_FAILED` printed by the prompt. All of it goes to the terminal, always, because
the shell is a different process from the thing being redirected and its output was never in the
substituted slot.

That is not a small residue. It is most of what fd 2 carries in a Unix shell session, and it is
separated here **by process boundary rather than by convention**, which is the stronger separation:
there is no `2>&1` that could merge them back by accident.

What remains is a program's own diagnostics, which today are indistinguishable from its output.

### Why fd 2 exists on Unix, and why that reason does not transfer

Unix needs a *numbered convention* because a process cannot ask its parent for a channel. Every
process gets three descriptors by inheritance, so what fd 2 is has to be agreed in advance by
everybody, forever. **Nothing here is ambient.** A program holds an endpoint because init put one in
a slot, and init put it there because the shell's plan said to, and the plan came from a manifest
that already declares what kind of output the program has (`OutputSpec`). The mechanism for "this
program has a second thing to say" is therefore a **declaration**, not a number.

### The two shapes it could take, and what each costs

**A second endpoint in a second slot.** The direct translation. It costs a `spawnproto` bit (there
are 29 free in that word), a delegation position, an init branch, a slot in every child, a manifest
declaration, and an edit to every program that has anything to say. It also doubles §51's claim: a
writer holding two endpoints must be able to tell them apart, which it does by slot number rather
than by asking, so indifference survives *technically*. What does not survive is the sentence "a
program's output is an endpoint", which becomes "a program's outputs are endpoints, and which is
which is a convention" and is Unix's fd numbering with a capability underneath.

The concrete blocker is smaller and more annoying: **slot 1 already means two things.** It is the
input source or the `--mem` untyped, whichever the request carried, and that is unambiguous only
because no manifest declares both (this note's BUGS has carried the entry since the milestone
landed). A third stream makes an ordered slot convention untenable and forces a numbered one first.

**An opcode on the one endpoint.** `sink_proto` puts the operation in the top byte of the request
word, so `OP_BYTES = 0` and `OP_EOF = 1` leave 254 spellings free. A third, "these bytes are a
diagnostic", would carry the distinction on the wire the writer already holds: no second capability,
no second slot, no spawnproto change, no init change, and §51 intact word for word. The **reader**
then decides, so `2> name` would name where the shell sends the diag messages it is already
receiving, and `date > out.txt` would print its complaint to the terminal and write nothing to the
file.

Its cost is real and it is in the middle of a pipeline. `a | b`: `a`'s diagnostics arrive at `b`,
which is a `wc` that would count them, and the answer has to be a rule (`wc` drops what it cannot
read? every reader forwards diags upstream?). Unix's answer is that fd 2 bypasses the pipe entirely,
and that is exactly the property one endpoint cannot express. Attaching a rule to it is a protocol
design task, not a wiring one.

### Why this is Chris's call

Both shapes are defensible and they commit to different things. The first says a program can have
several output capabilities and the model should name them; the second says a program has one
output capability and the *contract* on it should be richer. That choice constrains everything
downstream: a logging service, a supervisor collecting a child's complaints, and whatever milestone
40's documentation service does with a component's diagnostics.

And there is a real third answer: **do nothing**, on the grounds that in-band diagnostics on one
stream is what a program with one thing to say should do, and that the separation the shell already
gives (its refusals never enter a redirection) is the part that was worth having. That is the
current state and it is not obviously wrong.

Inventing the convention before a program has two things to say would be inventing it rather than
discovering it, which is the same argument milestone 50 made about `InputSpec` and got right by
waiting.

## SIGPIPE, and why the pipeline gets its own region

Deleting every capability that names an endpoint does **not** destroy the endpoint: the object lives
in a page of an untyped region, and only reclaiming the region frees it. So a pipeline whose reader
has finished while its writer is still blocked in a `SEND` would leave that writer blocked forever.

Each pipeline therefore takes its own region, split off the shell's budget, and the shell `DESTROY`s
it when the line is over. That is what turns a producer's next `SEND` into `abi::Error::Gone`, which
is `SIGPIPE` as a return value. The classification itself is asserted by value in
`kernel::user::sink_tests`.

## The boot that has a filesystem, which is what `>` was actually waiting for

The kernel brings the block server and the FS server up **before init exists** and hands init the
file-service endpoint plus the frame its clients map. init narrows both into the shell: slot 4, and
the page at `FS_VA`. Nothing else in the system changed shape; the shell simply holds one more
capability.

```text
  kernel  ── wires blk + fs_server, drains both readiness sentinels ──┐
                                                                     v
          ── spawns init, granting the FS endpoint and the page (GRANT on both)
                                                                     |
  init    ── builds console, line_editor, input ──────────────────────── |
          ── builds the shell: slot 4 = the FS endpoint (WRITE, no GRANT)
                                          + the page at 0x60_0000
          ── starts it with arg1 = the dir rights that endpoint carries
```

`arg1` is `0` on a machine with no RedoxFS disk attached, and then the shell is exactly the shell it
was: `Nav::empty()`, and every verb that would need a directory says so. **The same ELF is in both
positions**, which is why `kernel::user::pipeline_tests` (no slot 4) and
`kernel::user::redirection_tests` (slot 4) are each other's control.

Three things had to move to make room, and each is a fact worth keeping:

- **The shell's terminal page moved from `0x60_0000` to `0xc0_0000`.** `0x60_0000` is
  `FILE_VA_CLIENT`, which six programs map; the terminal page is the one address only the shell and
  its init know, so it is the one that moved.
- **init's cspace is sixteen slots and two more kernel grants overflowed it.** The console's
  `build_child` had no slot left to retype an address space into and returned an error, which
  presented as a boot that brought the console up and then printed nothing at all. init now retypes
  the spawn and result endpoints **after** the drivers are built, and gives the console's three
  capabilities back before the shell, which is the same discipline the file already had one step
  later.
- **Every child init builds gets eight stack pages, not four.** The redirection path carries a
  parsed line, an array of planned endowments, a listing buffer and a file buffer by value, and four
  pages overflowed at the first `ls > out.txt` (a data abort one word below the lowest stack page).
  The kernel's own scripted wiring had already found the same floor and maps seven.

## EXAMPLES

At a real prompt, on the RedoxFS fixture. `script/console` is the aarch64 spelling and builds
everything it needs (the FS server into the initrd, and the image, because the runner attaches the
disk only when the file is there). RISC-V has no `xtask` verb for the interactive boot, so it is two
commands:

```sh
script/console                                   # aarch64

cargo xtask initrd-riscv                         # riscv64
CRICKER_INITRD=target/initrd-riscv.img CRICKER_DISK=target/crickerfs.img \
  cargo run -p kernel --features shell --target riscv64imac-unknown-none-elf
```

```text
$ ls
  globset/
  motd
  other/
  redir/
  rmtree/
  scratch
  sub/
$ ls > out.txt
$ wc < out.txt
  8 8 57
$ date > when.txt
$ wc < when.txt
  1 11 66
$ ls
  globset/
  motd
  other/
  out.txt
  redir/
  rmtree/
  scratch
  sub/
  when.txt
```

Read the numbers rather than the fact that it ran. `wc < out.txt` says **eight** lines where the
listing above it had seven, because `>` creates and truncates its file **before** the command runs,
so `ls` sees `out.txt` in the directory it is listing. That is Unix's order and it is worth seeing
rather than being told. The 57 bytes are those eight names plus a newline each; the terminal's
two-space indent is the terminal's manners and is not in the file.

The riscv64 prompt is the same session with different numbers, because that leg's image had two more
names on it from an earlier test run (`10 10 77` rather than `8 8 57`). The numbers being *different*
and still internally consistent is the better demonstration: nothing here is a constant anybody
pinned.

And `>>`, at the same prompt. `date` writes one line of 66 bytes, so two of them is 132:

```text
$ caps date >> when.txt
  date would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    (clock: this shell holds none to delegate, so it will report the time
     as unknown. the clock is init's to endow; no token on the line can.)
    output   when.txt  (this shell writes the bytes there; the program holds
             an endpoint and cannot seek, truncate, re-read or stat)
             this shell keeps what is already in it and writes after it
    arg    (none)
  reading the command is reading its whole authority.
$ date > when.txt
$ date >> when.txt
$ wc < when.txt
  2 22 132
```

Read the `caps` output rather than the counts. **The last line of the `output` row is the only thing
`>>` changes**, and it is a sentence about *this shell*, not about `date`: the `cap 0` row above it
is identical to the one `date > when.txt` prints, and so is everything else, because the two
spellings hand the child the same endpoint with the same right. That is the property `>>` was built
to be a test of, printed where a person meets it.

And the same at a prompt with no filesystem, which is the same binary:

```text
$ ls > out.txt
  you hold no such capability: this shell was granted no directory to narrow
```

The rest of the operators:

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
thread serves `grant_plan::spawnproto` as init.

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

### And the redirections, at a prompt that holds a filesystem

`kernel::user::redirection_tests` is `pipeline_tests` with one more capability: the same shell ELF,
the same four slots, plus a directory at slot 4 narrowed by a `fs_subtree_caretaker` to one subtree
of the real RedoxFS image. Three claims, and none of them is "it printed something":

- **One builtin, two destinations.** `ls > out.txt` writes a listing into a file and prints nothing;
  the `ls` after it prints the same listing; `wc < out.txt` has to agree with what was printed, once
  the prompt's two-space indent comes off. The expected counts are *derived from the transcript*, so
  a `>` that dropped every second byte fails even though it would still produce a file.
- **One program, two destinations.** `date` printed and `date > date.txt` counted, and the file's
  byte count has to be the length of what was printed.
- **The refusals a directory does not rescue.** `wc < nosuch.txt` is the filesystem's own sentence
  (a `<` does not create, because a `wc` that truthfully reported zero for a file that is not there
  is a number a person would believe), and `worker 9 > out.txt` is still `NotAByteStream`.

The pair of witnesses is the capability argument made twice with one binary:
`pipeline_tests::a_redirection_a_shell_cannot_back_is_refused_rather_than_dropped` refuses because
slot 4 is empty, and this writes the file because slot 4 holds a directory. Neither is a branch in
the shell.

## The gate for the boot itself, which is what runs the real init

The guest tests above wire the shell **from the kernel**: it serves the terminal contract and, on a
second thread, `grant_plan::spawnproto` in place of init. The shell cannot tell the difference, and
that is the problem. `user/src/system_initializer.rs` is not the same code, so a change that broke
the real spawn path failed nothing, and the `--features shell` boot is the only thing that runs it.

That cost this milestone three manual bisects, and **all three presented as a boot that printed
nothing at all**: the shell's terminal page colliding with `FILE_VA_CLIENT`, init's sixteen-slot
cspace overflowing when the kernel handed it two more grants, and four stack pages being one deep
call short of the redirection path.

`script/shell-check` closes it. It boots that system on both ISAs, types five lines at the prompt,
and reads the answers back:

```text
echo hello world | wc      -> 1 2 12   the bytes went through a real spawned process
echo hello world > gate    -> nothing  the same bytes into a file the shell backs
wc < gate                  -> 1 2 12   ... and they are the same bytes
echo hello world >> gate   -> nothing
wc < gate                  -> 2 4 24   ... exactly twice, so `>>` kept the first line
```

One line would have caught all three bugs. Five is still seconds, and it walks the whole endowment:
a spawn through the real init, the FS service the real init narrowed into the shell, and both
redirection operators.

**Two things the machine corrected while it was being written**, and both are the kind of thing a
harness gets wrong quietly:

- **The line editor echoes a character the moment it arrives**, whether or not the shell has asked
  for a line yet. So a harness that types ahead produces a transcript in which a command's echo
  appears *before* the `$ ` that should introduce it, and then fails to find its own echo. The gate
  waits for the transcript to **end** in a bare prompt, which is the unambiguous "ready".
- **The script types `wc < gate.txt` twice on purpose**, so every search is anchored at a cursor
  rather than run over the whole transcript. An unanchored search found the first answer for both
  lines, and would have passed a `>>` that truncated.

It drives `scripts/qemu-runner-aarch64.sh` directly rather than `cargo run`, so the process it owns **is**
QEMU (the runner `exec`s it) and the kill lands on the emulator instead of on cargo. It is not part
of `script/test`, because it builds a second kernel and boots it twice.

## BUGS, named where the reader meets them

- **`script/shell-check` is not in `script/test` or in CI.** It is the only gate on the real init and
  nothing runs it automatically, which is a weaker version of the gap it closed. Wiring it into the
  CI test job is a one-line change and is deliberately not taken here.
- **`user/src/sink.rs`'s file and source roles are no longer on the shell's path.** They are still
  the right shape for an adapter whose client is not the shell, and `sink_tests` still proves them
  against a real image, but nothing at the prompt builds one. The source role also still opens the
  one name in `sink_proto::fixture` and cannot be told another; the shell would have had to hand it
  a name the way `fs_file_caretaker` is handed one, and it turned out not to need to.
- **The interactive prompt holds the image root, unnarrowed.** A `fs_subtree_caretaker` between it
  and the FS server would cost one process and would make the prompt's own authority as legible as
  the authority it hands out. It is the machine's own shell, so this is a defensible default rather
  than an oversight, but it is a default and not a decision anybody made on the record.
- **`rm` is still not reachable from the prompt.** The shell now holds a directory, so the refusal
  is no longer "you hold no such capability"; what is missing is the `fs_subtree_caretaker` init
  would have to build per invocation, and `spawn` says so rather than spawning `rm` with nothing.
  init deletes its copy of the FS endpoint after building the shell, so that is the line that
  changes first.
- **Slot 1 is the input source or the `--mem` untyped, whichever the request carries.** That is
  unambiguous only because no manifest declares both, and `grant_plan` is where that stops being true. A
  program endowed a budget *and* an input needs a numbered slot convention rather than an ordered
  one.
- **A pipeline is full lockstep.** There is no buffer: every sixteen bytes is a rendezvous. Unix's
  64 KB pipe buffer lets a producer run ahead; this does not, and nothing here has been benchmarked
  against a Unix pipeline. If buffering earns its place it arrives as a component that speaks the
  sink contract on both sides and is inserted into the chain.
- **No `2>`, and it is an open fork rather than a missing feature.** A program holds one output
  endpoint and its diagnostics ride it in-band, so `date > when.txt` on a clockless machine writes
  the complaint into the file. The section above weighs the two shapes an answer could take and says
  why neither is a lane's to pick.
- **No here-document, and `<<` says the wrong thing about why.** It is refused as "a redirection
  needs a name", because the second `<` is read as the operator it is. The refusal is right and the
  sentence is about the wrong thing.
- **No quoting anywhere in this shell**, so a file whose name contains `>` cannot be named. That is
  a gap in the tokenizer, not in the operators.
- **`wc` has no `-l`, `-w` or `-c`.** It prints all three, because selecting among them is
  formatting and formatting belongs downstream.
- **A `date` whose reader stopped early stays parked.** `date`'s end-of-stream message is a
  rendezvous send like any other, so a reader that took its line and stopped leaves that process
  blocked until its region is reclaimed. Inside a pipeline the region is destroyed and it ends; in
  `kernel::user::date_tests`, which read a line and stop, it does not. Blocked, not spinning, and
  the suite's leaked-thread gate is about runnable threads.
