# Pipes and redirection: `>`, `<` and `|` are one substitution

*Milestone 50, the operators lane and its closure. `crates/grant_plan/src/line.rs`,
`user/src/wc.rs`, `user/src/swish.rs`, `user/src/date.rs`, `user/src/terminal_sink.rs`,
`user/src/system_initializer.rs`, `user/src/hello.rs`, `crates/grant_plan/src/spawnproto.rs`,
`script/shell-check`. The protocol half is notes/sink-protocol.md and you should read that first.*

**All five operators run at a real prompt on both ISAs.** `|` landed first; `>` and `<` needed a
boot in which one shell holds both a filesystem and a spawn channel, and building that turned up a
reason the file end of a redirection cannot be a separate process. That finding is the section
["The file behind a `>` is this shell"](#the-file-behind-a--is-this-shell-and-that-was-not-the-plan),
and it is the most reusable thing in this note. `>>` is the cheapest of the four,
which is that finding paying out: the shell already holds the file, so append is one bit about how
it opens one.

`2>` came last of all, on 2026-08-03, and it is the one operator that is not a spelling for
something the system already had: it needed a second stream to exist first. It exists **per program,
by declaration** (DECISIONS §67), which is what makes the digit a familiar spelling rather than a
number everybody has to agree on. See ["`2>`: built as a
declaration"](#2-built-as-a-declaration-decisions-67).

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
stage := <command words> ('<' name | '>' name | '>>' name | '2>' name | '2>>' name)*
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

`date` went one further on 2026-08-03 and declares a **second** byte stream as well (DECISIONS §67),
which is what `2>` binds to. `OutputSpec` is where that is written down too.

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
  w2 = mem_pages | INTERRUPTIBLE_BIT | SINK_BIT | SOURCE_BIT | DIAG_BIT

  then, over SEND_CAP, in this order:
    job untyped, job frame   (if INTERRUPTIBLE)
    the sink                 (if SINK)          -> the child's slot 0
    the source               (if SOURCE)        -> the child's slot 1
    the diagnostic endpoint  (if DIAG)          -> the slot the MANIFEST names (§67)
    the --mem untyped        (if mem_pages > 0)
```

`DIAG_BIT` is the odd one and the difference is the decision: the other three say *which slot*, and
this one does not, because the slot is the program's declaration rather than the shell's choice. What
the wire says is only "expect one more capability". It is also the only one the shell sets from an
**operator** rather than from the wiring: with no `2>` on the line a declaring child still gets a
second stream, endowed by init from the manifest the way the clock is.

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

## `2>`: built as a declaration (DECISIONS §67)

**Decided and built 2026-08-03.** A program that has diagnostics **declares a second output in its
manifest**; the shell plans a second endpoint only for the programs that declare one; `2>` names
where those bytes go; and aimed at a program that declares none it is a refusal at the prompt, in
the same voice as every other refusal here. `date` is the first and so far only declarer, because
`date` is the program the loss was measured on.

The analysis of the fork as it stood open is kept below, because the reasoning is the reusable part
and because reading it explains why the built thing has the shape it does. What follows first is
what exists.

### The five moving parts, and none of them is a number everybody agrees on

```text
  crates/grant_plan            OutputSpec::BytesAndDiagnostics { slot }   the declaration
                               line::Diagnostics { None, Printed, File }  where they go
                               Refusal::NoDiagnosticStream                aimed at a non-declarer

  crates/grant_plan/line.rs    `2>` and `2>>`, at the tail, like `>` and `>>`

  user/src/swish.rs            an endpoint the shell mints ONLY for a `2>`, because that is the
                               case where it has to back a file

  user/src/terminal_sink.rs    where the bytes go with no operator on the line: the terminal,
                               served by an adapter, so nothing goes through the shell at all

  user/src/hello.rs            receives whichever it is and inserts it at the slot the MANIFEST
  user/src/system_initializer  names, not at the next free one. Yes, both: see below
```

The two init lines are the ones to read twice. **The slot is high (eight) and placed explicitly**, and that
is not a style choice: how many low slots a child gets depends on what the command line granted it
(`date` gets a clock from init and none from the guest-test harness), so a diagnostic stream that
landed "next" would sit at slot 2 in one wiring and slot 1 in another, and a program that probes one
number would read the wrong slot. `abi::tcb::CAP_INSERT` already had an explicit target, added for
`abi::fault::FAULT_EP_SLOT` for exactly this reason; §67 is its second user.

So the number eight is not a convention anybody has to know. It is in the manifest, `caps` prints
the stream that uses it, and a program that declares nothing has no slot at all.

### What it looks like, and the case it was built for

At a real prompt, on a machine whose clock works, `date`'s second stream exists and is **empty**:

```text
$ date 2> err.txt
  Sun 2026-08-03 22:41:07 UTC
$ wc < err.txt
  0 0 0
```

That is the whole claim in four lines. The answer went to the terminal, the second stream went to
`err.txt`, and it carried nothing because there was nothing to complain about. Before §67 there was
one stream, so `err.txt` could not have existed and the timestamp would have been the only thing
either destination could have held.

And the case it was built for, which needs a `date` that was granted no clock (the interactive
prompt has one, so this is `kernel::user::date_tests`, asserted by value rather than read off a
transcript):

```text
  date, no clock, with a declared second stream:
     the diagnostic endpoint     "date: the time is unknown: this process holds no clock capability"
     the output endpoint         OP_EOF, and not one byte before it
```

The second line is the fix. `date > when.txt` drains the output into the file, and the output is
empty, so **the file is empty and the complaint is on the screen**. Nothing in `date` decides that;
it wrote to a different endpoint, and which endpoint it had was its spawner's choice.

`caps` prints both destinations, because a preview that named only one of them would be a
half-truth about a line that has two:

```text
$ caps date 2> err.txt
  date would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    cap 1  frame     clock    read-only. it can read the time and not set it,
                              and no token on the line could have asked for more
    output   this shell's result endpoint (it reads the bytes and prints them)
    diags    err.txt  (this shell writes them there; the program holds a
             second endpoint and still cannot seek, truncate or stat)
             this shell empties it first, before the command runs
    arg    (none)
  reading the command is reading its whole authority.
```

And with no operator, where the `diags` row names a capability **this shell does not hold**:

```text
$ caps date
  date would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    cap 1  frame     clock    read-only. it can read the time and not set it,
                              and no token on the line could have asked for more
    output   this shell's result endpoint (it reads the bytes and prints them)
    diags    the terminal's own sink, a component this shell does not hold.
             declared by the program, so a > cannot swallow them, 2> can
             name them, and they reach the screen without passing here
    arg    (none)
  reading the command is reading its whole authority.
```

The refusal, which is the other half of "a declaration, not a number":

```text
$ wc gate.txt 2> err.txt
  wc: declares no second output, so there is nothing for 2> to name (its diagnostics ride its output)
```

Nothing was denied. `wc` writes one stream, its diagnostics ride it, and the operator has nothing to
bind to. That is `Refusal::NoSuchProgram`'s shape applied to a stream, and it is the sentence §67
asked for.

### A third bug of the same shape, and the pattern is now three deep

Building the terminal's sink adapter made the boot print **nothing at all** on aarch64, and it was
init's sixteen-slot cspace for the third time. The adapter was built between the input driver and the
shell, which looked like the natural place; at that moment init still holds the terminal endpoint,
two shared frames, the file service and its page, so one more endpoint put `build_child` one slot
short while it was retyping the *shell's* address space. The shell was never built, so nothing ever
printed.

The fix is where rather than what: build it **last**, after the shell and after every boot capability
but `term_ep` has gone back, which is the narrowest the cspace ever is. `term_ep` goes back
immediately after.

The pattern worth keeping is that all three instances of this presented identically, as a boot that
reaches userspace and then says nothing, and all three were a capability slot rather than memory.
**A cspace that is sized in a constant and consumed in an order is a resource with no error message.**
`build_child` returns `Err(())` and init halts, which is correct and silent.

### A correction: there are two inits, and this note said there was one

The section below on `script/shell-check` says it "is the only thing in the tree that runs the real
`system_initializer`". **That is wrong about which program it runs on aarch64**, and building `2>`
found it the hard way: the shell delegated a diagnostic endpoint, nobody received it, and the prompt
hung on the first `date` with no fault and no message.

`kernel/src/main.rs` hands off to `user::initrd()`, which loads the program named **`init`**, and on
aarch64 that is `user/src/hello.rs`'s `init_boot` role. `user/src/system_initializer.rs` is riscv64's.
Both serve `grant_plan::spawnproto`, and **the serving loop is written twice**, once in each file,
about a hundred and forty near-identical lines: the same delegation order, the same slot ordering,
the same clock rule, now the same `build_child_at`.

That duplication is a rule-7 problem wearing a different hat. What two binaries must agree on is a
crate, and these two agree on far more than a constant; a change to the protocol that lands in one
of them is a boot that hangs on the ISA nobody tested. It is not fixed here, because merging two
inits is its own lane, but it is now written down where the next person will meet it.

What the gate proves is unchanged and is the reason it caught this: `script/shell-check` boots
**both** ISAs, so it runs both inits, which is exactly what a single-ISA gate would have missed.

### Where the bytes go by default, and why it is not this shell

**With no `2>` on the line, a declared second stream goes to the terminal's own sink**, which is a
component (`user/src/terminal_sink.rs`, notes/sink-protocol.md) and not the shell. init endows it
from the manifest, exactly as it endows the clock and for the same reason: the shell holds no
terminal capability it could delegate, and a person does not designate a screen.

That is stronger than the shell printing those bytes, and the difference is worth stating. **They
reach the screen without passing through the shell at all**, so nothing the shell does to a program's
output can reach them: `date > when.txt` drains the output into the file and never sees the
complaint. `caps` prints it as a row that names a capability this shell does not hold.

It is also what keeps the shell single-threaded and honest. The shell reads one endpoint at a time
(there is no receive-on-a-set in this kernel, the gap `fs_file_caretaker` records too), so a default
that came back here would put it in the business of multiplexing two streams, and the ordering
constraint below would apply to every declaring program rather than to the ones a `2>` names.

### The one rule a `2>` costs, and why only a `2>` costs it

**A program whose second stream this shell is backing must say everything it has to say, and close
that stream, before it writes a byte of output.**

`2> err.txt` is the case where the shell holds both ends: it mints an endpoint, hands it to every
declaring stage, and drains it into the file. Two streams, one single-threaded reader, and it must
read the **diagnostics** first. Consider `date | wc 2> err.txt`: the shell is not the reader of
`date`'s output, `wc` is. A shell that drained the output first would be waiting on `wc`, which is
waiting on `date`, which is blocked in a rendezvous `SEND` on a diagnostic endpoint nobody is
listening to. Diagnostics first is the only order in which nobody waits on somebody who is waiting on
them.

`date` keeps the rule for free, because every complaint it has is a reason it has no answer, and it
keeps it unconditionally: it closes its second stream before its first either way. A program with
running commentary (a `rm -rv` that hit one unremovable name halfway through a thousand) could not,
and would be fine at the default destination and wrong under a `2>`. **That asymmetry is the honest
cost and it is in this note's BUGS.**

### What it deliberately does not do

- **There is no `Diagnostics::Pipe`.** A diagnostic that flowed down a `|` into a `wc` would be
  counted as output, which is the conflation the second stream exists to end. `2>` names a file or
  nothing.
- **One diagnostic stream per line, not per stage.** The shell mints one endpoint and hands it to
  every declaring stage, so `2>` at the tail names where *the line's* diagnostics go. Two declaring
  stages would interleave on it, which is exactly what Unix's shared fd 2 does; the difference is
  that here they interleave because the shell chose one destination and not because a number was
  ambient.
- **No `2>&1`.** Merging the streams back is not expressible and should not be: the point of the
  declaration is that the two are different capabilities. A program that wants one stream declares
  one.

### The fork as it stood open, kept for the reasoning

*Everything from here to the end of this section is the analysis that was written while `2>` was
still undecided, kept because the reasoning is the reusable part and because it explains what the
built thing is not. "Today" means before 2026-08-03; the section that closes it is "Why this is
Chris's call", and he called it: option (c), the manifest declaration.*

#### There is no second stream today, and that is a fact rather than an omission

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

#### But the half that hurts most on Unix is already separated here

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

#### Why fd 2 exists on Unix, and why that reason does not transfer

Unix needs a *numbered convention* because a process cannot ask its parent for a channel. Every
process gets three descriptors by inheritance, so what fd 2 is has to be agreed in advance by
everybody, forever. **Nothing here is ambient.** A program holds an endpoint because init put one in
a slot, and init put it there because the shell's plan said to, and the plan came from a manifest
that already declares what kind of output the program has (`OutputSpec`). The mechanism for "this
program has a second thing to say" is therefore a **declaration**, not a number.

#### The two shapes it could take, and what each costs

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

#### Why this is Chris's call

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

## Buffering: measured, and the answer is to build nothing

The roadmap block said it plainly: **measure `a | b` throughput against a Unix pipe before deciding
anything**, and if buffering earns its place it arrives as a component speaking the sink contract on
both sides. It was measured on 2026-08-03. It has not earned its place, and the number says
something more useful than "no".

### The measurement

`bench: sink_throughput` (`kernel/src/bench.rs`) is a pipeline with the shell taken out: two EL0
processes, one endpoint, the left one packing sixteen bytes into a sink message and `SEND`ing, the
right one `RECV`ing and self-timing. `bench/host/pipe_throughput.rs` is the same shape over a real
`pipe(2)`, twice, because only one of the two arms is apples to apples.

Apple Silicon, one machine, one sitting. cricker-os under HVF (`cargo xtask bench --real`), so the
nanoseconds are real; the host arms are medians of three.

| | per 16 bytes | throughput |
|---|---|---|
| cricker-os `a \| b` (one endpoint, no buffer) | **1146 ns** | **13.3 MiB/s** |
| macOS pipe, 16-byte writes | 348 ns | 44 MiB/s |
| macOS pipe, 64 KiB writes | (5.4 µs per 64 KiB) | 11,600 MiB/s |

Two reference points from the same run make the first row legible. `ipc_rtt_el0` is 2785 ns for a
round trip, so a one-way rendezvous is about 1.4 µs and **our pipe is one rendezvous per message and
nothing else**: there is no overhead to find. And `relay_rtt` (1187 ns) against `ipc_rtt` (2313 ns)
prices what a hop through a userspace process costs, which is roughly double.

### What the numbers actually say, which is not what the block expected

**The lockstep is not the bottleneck. The sixteen-byte message is.**

Read the two host rows together. At the same granularity Unix is 3.3x faster than us, which is a
real gap and is the cost of a capability rendezvous against a tuned kernel pipe. The 870x is
somewhere else entirely: Unix's win is that a program writes 64 KiB per syscall, and ours is capped
at sixteen bytes per message because the sink contract is register-only.

That cap is not an oversight and it is not a thing buffering fixes. `notes/sink-protocol.md` records
why register-only was **forced**: the moment a sink also needs a page mapped at an agreed address,
redirection stops being "put a different capability in one slot" and becomes a three-way spawn-time
negotiation, and milestone 50's whole finding evaporates.

### So a buffering component would make it worse, and that is the decision

Insert a process between the two ends and every message pays a second rendezvous. `relay_rtt` prices
that at roughly double, so an 80 KB pipeline would go from 5.7 ms to something near 11 ms. **A
buffer cannot batch its way out of that**, because what it forwards is still sixteen bytes per
message: the contract it speaks on both sides is the one that sets the cap.

Buffering buys decoupling, not bandwidth. It wins when a producer has *work to do between writes* and
a consumer has work to do between reads, so the two can overlap instead of alternating. It does not
win when both are only moving bytes, which is what this benchmark and every pipeline in this system
today are.

**So nothing is built, and that is the successful outcome the block described.** What would move the
number is a larger message, and that is a different decision with §51's indifference on the other
side of it. It is not taken here and it is not needed: `date | wc` and `ls | wc` are hundreds of
bytes, and at 1.15 µs per sixteen that is tens of microseconds for a whole line.

### The honest caveats

- **The benchmark does not measure the case buffering is for.** Both ends do nothing but move bytes,
  so there is no producer-side work for a buffer to overlap with the consumer's. A pipeline of two
  programs that each compute would show a different shape, and if one is ever built, this is the
  benchmark to extend rather than the conclusion to keep.
- **The host arm is macOS, not Linux.** `bench/host/run_linux.sh` exists for the cross-OS suite and
  this program belongs in it; the Linux number is not taken here, and Linux's pipe is a different
  implementation with a different fast path.
- **The 64 KiB row is nearly memcpy-bound** and the producer runs ahead through the whole transfer,
  which is the buffer effect at its most flattering. It is in the table because it is what a Unix
  program actually gets, not because it is a fair comparison.
- **`sink_throughput` is not in `bench/baseline-*.txt`.** It is a new row, and a baseline row is a
  claim about a number somebody has looked at; the integrator adds it from the merged tree, on both
  ISAs, in the same breath.

## SIGPIPE, and why the pipeline gets its own region

Deleting every capability that names an endpoint does **not** destroy the endpoint: the object lives
in a page of an untyped region, and only reclaiming the region frees it. So a pipeline whose reader
has finished while its writer is still blocked in a `SEND` would leave that writer blocked forever.

Each pipeline therefore takes its own region, split off the shell's budget, and the shell `DESTROY`s
it when the line is over. That is what turns a producer's next `SEND` into `abi::Error::Gone`, which
is `SIGPIPE` as a return value. The classification itself is asserted by value in
`kernel::user::sink_tests`.

## The five processes an interactive boot now runs

For a reader arriving at this file cold, the shape of the system `2>` completed:

```text
  init ──builds──► console server            reads a page, writes the UART
              ├──► line_editor               the terminal contract: OP_WRITE, OP_READLINE,
              │                              OP_BYTES, OP_INTRCOUNT, OP_PRINT
              ├──► input driver              the UART receive interrupt, into the terminal
              ├──► swish                     the prompt
              └──► terminal_sink             the sink contract, into OP_PRINT
```

The fifth is new (DECISIONS §67, notes/sink-protocol.md), and it is the only one a person never
interacts with directly. It exists so "the terminal" can be a **destination a capability designates**
rather than a thing only the shell can reach, and its whole job is turning sink messages into terminal
prints. init keeps the endpoint it serves and hands it to any child whose manifest declares a second
stream, the way it hands out the clock.

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
  wc: reads an input stream: name a file, redirect with '<', or pipe into it

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
wc gate                    -> 2 4 24   milestone 31: the name IS the grant, same bytes
wc                         -> refused  ... and with no name there is nothing to read
caps wc gate               -> input    ... and the preview says which file, and how
date                       -> ...UTC   milestone 51's wiring: a clock init endowed
caps date                  -> cap 1    ... and the visibility surface names it
```

One line would have caught all three bugs. Five is still seconds, and it walks the whole endowment:
a spawn through the real init, the FS service the real init narrowed into the shell, and both
redirection operators.

The `wc gate` trio is milestone 31's headline checked at the one interface a human touches. Its
answer has to equal the `<` line's, because it is the same designation with the operator left out, and
the pair is what makes that a claim about the machine rather than an assertion: one line reaches the
file through an operator and one through a name, so if they disagree, one of them opened something
else. `wc` alone is the negative control the pair would be weaker without, refused at the prompt
before anything is spawned.

The last two arrived with milestone 51's wiring lane and check a different half of the same boot.
`date`'s answer cannot be a constant, so the assertion is `UTC`: `Format::Human` ends in the offset's
name and **neither** unknown-clock sentence contains those three letters, so one word fails the gate
if the clock service did not run, if the kernel granted init no page, if init did not endow `date`,
or if `date` was handed a page nobody published to. `caps date` then requires that the shell's own
visibility surface names the capability, because `caps` claims to print a process's whole authority
and a clock endowed but not printed would make that claim false.

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

## The shell's stack, a fourth time, and what the pattern is now

Milestone 50 hit "a boot that printed nothing" three times and one of them was four stack pages
being one deep call short of the redirection path. Milestone 31's input operand hit the **same
symptom in the same file** and it is worth naming the shape rather than the instance.

`wc out.txt` reaches `run_pipeline` through `dispatch` -> `dispatch_one` -> `run`, where a line with
an operator on it reaches it through `dispatch` -> `pipeline`. One frame deeper, on a program whose
frames carry parsed lines and planned endowments **by value**, and the scripted wiring's six extra
pages were not enough: a data abort one word below the lowest mapped page, mid-script, with `far`
equal to `sp`.

Two things came out of it, and the second is the one to keep:

- **An `Endowment` is not small.** The first version of that arm declared
  `[Option<Endowment>; MAX_STAGES]` for a line that is one stage by construction, and an `Endowment`
  carries a whole `NameSet`. `run_pipeline` takes a slice, so `&[Some(endow)]` is the same call with
  the array cost deleted. That alone was not enough, but it is the fix that should have been written
  first: the reflex of reaching for the full-width array is what made a one-stage line pay for four.
- **The two wirings gave the shell different stacks, and that was the real oddity.**
  `system_initializer` maps eight pages and `pipeline_service` mapped seven, so the wiring that is
  *not* the one a person types was the smaller one, and it is where the overflow landed. They are
  both eight now. A test wiring with less headroom than the boot wiring will keep finding faults the
  boot does not have, which is a bug in the harness rather than a signal.

**And a fourth time, for `2>`.** A second `FileOut` on `run_pipeline`'s frame (each carries a
256-byte staging buffer by value, because the filesystem's write unit is a page and the sink
contract's is sixteen bytes) overflowed eight pages by twenty-four bytes, in the same place, with the
same signature. All three wirings are **twelve** now, and the number went up by four rather than one
on purpose: every previous instance bought exactly enough headroom and the next change found the
wall again. 48 KiB of address space per child is not worth a fifth bisect.

The pattern the four instances share is worth more than any of them: **this shell's frames carry
whole values that grew as the milestone grew** (a parsed `Line`, an array of `Endowment`s each
holding a `NameSet`, a listing buffer, now two file buffers), and none of that shows up anywhere a
reader would look. The symptom is always a data abort one word below the lowest mapped page.

## BUGS, named where the reader meets them

- **The terminal's sink adapter has one consumer, and it is `2>`'s default destination.** Nothing
  else puts it in a child's output slot yet, though nothing stops it: a shell that wanted a program
  to print straight to the screen rather than through its own result endpoint could hand it over,
  and would lose the ability to redirect that program at all. That trade is the interesting question
  it raises and it is not answered here.
- **`OP_PRINT` carries eight bytes, so a sixteen-byte sink message is two calls to the terminal.**
  That is the terminal contract's request shape rather than a choice (see notes/sink-protocol.md),
  and it doubles the round trips on a path that is a person reading text.
- **`script/shell-check` is not in `script/test` or in CI.** It is the only gate on the real init
  (both of them) and nothing runs it automatically, which is a weaker version of the gap it closed.
  It has now caught two boots that printed nothing, which is two more than any automatic gate did.
  Wiring it into the CI test job is a one-line change and is deliberately still not taken here.
- **`user/src/sink.rs`'s file and source roles are no longer on the shell's path.** They are still
  the right shape for an adapter whose client is not the shell, and `sink_tests` still proves them
  against a real image, but nothing at the prompt builds one. (`user/src/terminal_sink.rs` is that
  shape with a client the prompt does build, which is the closest this has come to being used.) The source role also still opens the
  one name in `sink_proto::fixture` and cannot be told another; the shell would have had to hand it
  a name the way `fs_file_caretaker` is handed one, and it turned out not to need to.
- **The interactive prompt holds the image root, unnarrowed.** A `fs_subtree_caretaker` between it
  and the FS server would cost one process and would make the prompt's own authority as legible as
  the authority it hands out. It is the machine's own shell, so this is a defensible default rather
  than an oversight, but it is a default and not a decision anybody made on the record.
- **`rm` is still not reachable from the prompt, and neither is a per-file capability.** The shell
  holds a directory, so the refusal is no longer "you hold no such capability"; what is missing is
  the caretaker init would have to build per invocation, and `spawn` says so rather than spawning
  `rm` with nothing. init deletes its copy of the FS endpoint after building the shell, so that is
  the line that changes first. The same gap is why `FileSpec::Required` has no consumer: `wc
  gate.txt` grants a *stream* of one file (the shell opens it), which is narrower than the per-file
  capability `fs_file_caretaker` serves and is not the same claim. See notes/grant-expression.md.
- **Slot 1 is the clock, the input source, or the `--mem` untyped, whichever applies.** Three things
  in one ordered position now (milestone 51's wiring added the clock, which init endows from the
  program's manifest rather than from the request). It is unambiguous only because no manifest
  declares two of them, and `grant_plan` is where that stops being true. A program endowed a budget
  *and* an input, or a clock *and* an input, needs a numbered slot convention rather than an ordered
  one.
- **A pipeline is full lockstep.** There is no buffer: every sixteen bytes is a rendezvous. Unix's
  64 KB pipe buffer lets a producer run ahead; this does not, and nothing here has been benchmarked
  against a Unix pipeline. If buffering earns its place it arrives as a component that speaks the
  sink contract on both sides and is inserted into the chain.
- **`2>` works only on a program that declares a second stream, and one does** (`date`). That is
  DECISIONS §67's cost, stated where a person meets it: `wc gate.txt 2> err.txt` is refused, and the
  fix is `wc` declaring a second output rather than anything about the operator.
- **Under a `2>`, a declaring program must say everything before it produces anything.** The shell
  drains the diagnostic stream to end-of-stream *before* the output, because there is no
  receive-on-a-set and any other order deadlocks a pipeline (the section above has the chain). At
  the default destination the shell is not in the path at all and the rule does not apply, so the
  same program is correct without the operator and would deadlock with it. That asymmetry is real
  and is the price of the shell backing files itself (DECISIONS §55). `date` is unaffected: it
  closes its second stream before its first, always.
- **A `2>` on a builtin is refused as "declares no second output".** True (a builtin has no manifest
  and no second stream) and a slightly odd sentence about `ls`, which is not a program at all. The
  wording gap is the same shape as `<<`'s below.
- **The shell's diagnostic endpoint is never destroyed**, so a writer parked on it after the shell
  stopped reading would stay parked rather than getting `Gone`. It cannot happen today (the shell
  always drains to end-of-stream before it reads anything else) and it is a real asymmetry with the
  pipeline region, which is split and destroyed per line precisely to produce that `Gone`.
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
