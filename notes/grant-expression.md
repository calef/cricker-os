# The command line as a grant expression

Milestone 31, phase 1. This is the note on the one idea the capability shell exists to make
visible: on a cricker-os command line, **naming a resource is how you grant it**. Mark Miller's
principle, "designation is authorization," applied at the one interface a human touches. The pure
logic lives in the `grant_plan` crate (host-tested); the wiring is `swish.rs` and, on init's side of
the channel, `crates/system_initializer`'s spawn service. There are still two inits (`hello.rs` init_boot
on aarch64, `system_initializer.rs` on riscv), but since milestone 96 each is a table of the slot
numbers its kernel granted and nothing else, so the service that decodes a grant expression is
written once. The manifest half is written up separately in
[program-manifest.md](program-manifest.md).

## What Unix does, and why it is the opposite

A Unix child inherits every one of your file descriptors and runs under your uid, so it may
`open()` anything your uid allows. Authority comes from **who you are**, and it flows to a child
whether or not the command mentioned it. `grep secret public.txt` hands grep the authority to read
every file you own; that it only touches `public.txt` is grep's good manners, not a limit the
system imposed. This is ambient authority ([capabilities.md](capabilities.md)), and it is what
makes the confused deputy constructible.

The inversion: a cricker-os command grants **exactly what it names, and nothing else**. A program
that names no resource gets none. There is no ambient pool to draw from, so the question "may I?"
is never asked; there is simply nothing in the program's hands it was not given. `worker 9` grants a
report channel and an argument. `budgeter --mem 16` grants a report channel and a 16-page memory
budget. `budgeter` alone grants a report channel and is refused, because budgeter's manifest says it
needs memory and the command named none.

## The grammar

```text
<prog> [--mem N] [token ...]
caps [command]
help
echo <text>
```

**The command line itself is the grant expression.** Its parts are designators:

- `<prog>` names the program to spawn (a closed set today: `worker`, `budgeter`, `heeder`,
  `spinner`, `date`).
- `--mem N` designates **N pages of untyped**, carved from the shell's own budget.
- a bare token designates whatever the program's manifest declares in that position: the integer
  argument, then a **file** (one name, at most 16 bytes, no path). See the per-file grant section
  below for what the shell narrows it from and what it can back today.

`caps` is introspection: with no argument it prints the shell's whole endowment; with a tail it
previews exactly what that command would grant, so reading the command is reading the child's
authority. That is DECISIONS §14's claim made interactive.

## Two words came out of this grammar, and why (milestone 47)

Phase 1 spelled the same thing `run [--mem N] <prog> [arg] [file:PATH ...]`. Chris asked to be
convinced the two extra words earned their keep; they did not.

**`run` failed on consistency.** Milestone 47 adds `ls`, `cd`, `pwd`, `mkdir` and `rm` as shell
builtins, and nobody would type `run ls`. Keeping the verb would mean builtins are bare words while
programs need a prefix, so a user has to know *which class a command is in* before knowing how to
type it. That is the gratuitous divergence the milestone exists to refuse, and milestone 50 finishes
the argument: `run a | run b` is indefensible. What replaces it already existed, since resolving a
name to a program is `Prog::from_name` either way. The cost is three reserved words: `help`, `echo`
and `caps` win over a program of the same name, so the program namespace must not contain them.

**`file:` failed because it announced the wrong half of the grant.** `wc report.txt` reads and `tee
report.txt` writes: identical syntax, opposite authority, because the direction lives in the manifest
by design. The prefix decorated *which file*, which was already on the screen, and was silent about
read-versus-write, which is the part that decides what the child can do.

Its safety argument failed on inspection too. `worker 5 extra` is refused as unplaceable because
worker's manifest says `FileSpec::Forbidden`, not because of any prefix: **the manifest was doing all
the work and the prefix was taking credit.** The one thing the prefix genuinely bought is kept, in
the place where it applies: a token shaped like a flag (`--secret`) never falls into the file
position, because that is the one way a typo could become a capability transfer.

The deeper reason a prefix could never carry the thesis: **the capability claim is about absence.**
That a filename grants access to that file surprises nobody. What `wc report.txt` proves is that wc
got that file *and nothing else*, and that claim lives in the tokens which are **not** on the line. A
prefix decorating a token that is present cannot express it. `caps <command>` can, which is what
makes `caps` the visibility surface now that the designator is gone.

### The parser stopped classifying, and that is the load-bearing part

`parse` keeps the positional tokens in the order typed and refuses to say which is which;
`plan_against` places them into the slots the manifest declares. So `wc 2026` designates a file named
`2026`, which a shape-based rule ("a number is the argument") would have got wrong. It also means
"which token is the file" and "may this program have a file at all" are answered by the same
declaration, which is the honest version of what the prefix pretended to do.

**The window is why it happened now.** No program today takes both an argument and a file, so
positional resolution is at most one bare token. The first program that wants both (`grep pattern
file.txt`) forces `ArgSpec` to grow position and arity, and this change would have been a redesign
instead of an edit.

**Two limitations, named where a reader meets them.** A file whose name begins with `-` cannot be
designated (it reads as a flag), which is Unix's problem too and Unix's answer (`--`) is available
when something needs it. And with the prefix gone, the "you hold no such capability" refusal is only
reachable through a manifest that declares a file, so no *shipped* program can produce it at the
prompt today: `worker report.txt` now answers "worker: takes no file; drop the name", which is the
durable fact about worker rather than an accident of this shell's endowment. That reordering is
deliberate; see the refusal catalog below.

## Where the authority actually comes from, and how it moves

The shell holds four capabilities (init grants them at boot, in this order): the terminal endpoint
(slot 0), a spawn endpoint to init (slot 1), a result endpoint (slot 2), and **its own untyped
budget** (slot 3). The budget is the piece milestone 31 added: init splits it off its own untyped
and `CAP_INSERT`s it into the shell, so the shell has memory that is genuinely its to give.

The shell does not build children itself; init holds the initrd and stays the ELF loader (the
parser lives in one place, out of the shell). So the shell **directs** init and **delegates** the
capabilities it grants, over the spawn endpoint. The protocol (`grant_plan::spawnproto`, a userspace
protocol like the terminal contract, DECISIONS §21):

1. The shell resolves the command into an endowment (program id, argument, page count), checking it
   against the program's manifest first. A mismatch is refused at the prompt; nothing is sent.
2. The shell `SEND`s the request (program id, argument, page count).
3. If a memory grant was named, the shell `SPLIT`s N pages off its slot-3 budget and `SEND_CAP`s
   the resulting untyped to init, narrowed to `WRITE|GRANT`.
4. init loads the named ELF, endows the child with the result endpoint (slot 0) and, when one was
   delegated, the untyped (slot 1) narrowed to `WRITE`, and starts it with the argument.
5. The child runs and reports its answer on the result endpoint; the shell reads the one word.

Nothing the command did not name reaches the child. init inserts only the report channel every
spawn carries and whatever the shell delegated. The child's authority is the command line, read
literally.

## Untyped had to become delegable first

Making `--mem N` real, and not parsed-and-ignored, needed a kernel fix, recorded as an amendment
to DECISIONS §16. `Untyped::SPLIT` minted its child budget with `WRITE` alone, so it could be spent
but never delegated (`SEND_CAP` and `CAP_INSERT` both gate on `GRANT`). Untyped was the one object
type no process could hand on, which quietly foreclosed the whole feature.

The fix is rights **inheritance**, not a blanket upgrade, and the distinction matters: minting the
`SPLIT` child full rights unconditionally would be an escalation, since `SPLIT` gates only on
`WRITE`, so a process holding a spend-only untyped could split itself a `GRANT`-bearing child and
manufacture the right it was denied. Instead, a `SPLIT` child inherits the invoking capability's
rights and no more, and the **root** untyped init holds at boot is the delegable one
(`READ|WRITE|GRANT`). Rights narrow monotonically from that root down: root -> init split (inherits
`GRANT`) -> shell (narrowed to `WRITE|GRANT` at `CAP_INSERT`) -> shell split (inherits) -> spawned
child (narrowed to `WRITE`, spend-only). `GRANT` never appears where it was not present above.

## The budgeter proves the grant is real

`budgeter` is a program whose whole job is to spend the memory it was granted: it maps pages out of
its slot-1 untyped until the budget is exhausted, then reports the count. The number it prints is
the authority the command handed it. `budgeter --mem 16` reports **15** pages mapped on both
ISAs: the sixteenth paid for the page table that reaches the others (the kernel allocates nothing on
a process's behalf, DECISIONS §10). Grant more and it maps more; grant nothing and it holds no
untyped at slot 1 at all, so its first `MAP` returns `NoSuchSlot` and it maps zero. There is no
ambient pool behind it. This is the demonstration that `--mem` moves real memory, not a parsed
number.

## The refusals read like the model, not like errno

A refusal is a fact about what the shell holds, phrased in the capability model's voice:

- `frobnicate 1` → "frobnicate: no such program (try 'help' for the builtins)." There is nothing to
  name. A mistyped builtin lands here too, now that the first word is either a builtin or a program,
  which is why the line points at both halves of what the prompt understands.
- `budgeter` → "budgeter: needs a memory grant; add --mem <pages>." The manifest caught it.
- `worker 3 --mem 8` → "worker: takes no memory grant; drop the --mem."
- `worker 5 extra` → "worker: takes no file; drop the name." The token could only have been a file,
  and worker declares none, so it is refused rather than granted-and-dropped. **The answer is the
  same in a shell that holds a directory**, which is the point: the manifest decides, not the
  endowment.
- `worker eight` → "worker: needs an integer argument." Not a file, because worker has no file slot
  for the word to fall into.
- `wc report.txt`, at a program that *does* declare a file, in this shell → "wc: **you hold no such
  capability**: this shell was granted no directory to narrow."
- `wc sub/report.txt` → "wc: that is not a name this shell can grant: one component, at most 16
  bytes." There is no namespace here to walk, so a path is refused where it was typed rather than
  becoming an `ENOENT` from a server asked something meaningless.
- `wc --secret report.txt` → "wc: unexpected argument (this shell will not grant what it cannot
  place)." An unknown flag never reaches the file position.

The `no such capability` line is the headline refusal, and it is a statement about the shell's own
cspace: "there is nothing I hold that could grant this," never a Unix-flavored EPERM.

**One ordering changed with the designator, deliberately.** Phase 1 reported "you hold no such
capability" before any manifest quibble, so `worker file:x` produced it even though worker takes no
file. That was the prefix taking credit again: with the designator gone, a program's declaration is
checked first, and the holdings decide only whether a file the program *does* declare can be backed.
The reason is that "worker takes no file" stays true whatever this shell holds, while "no directory
to narrow" is an accident of this boot; the durable fact is the more useful one to print. The visible
consequence is that no shipped program can reach the headline refusal from the prompt today, because
none declares `FileSpec::Required`; it is exercised by the host tests through `plan_against`, the
same door `FileSpec::Required` has always come through.

## Per-file grants: one file, one direction, and a caretaker in between

*(Phase 2. The designator grammar was designed in phase 1 so this would slot in without a grammar
change, and it did; milestone 47 then removed the `file:` prefix from it, which changed the spelling
and not one line of the mechanism below.)*

The filesystem's unit of authority is a **directory**: the endpoint a client holds IS the directory
capability, and every name in an `OPEN` resolves under it (DECISIONS §27). `wc report.txt` says
less than that. It names one file, so it must grant one file.

The narrowing is a **caretaker**, Mark Miller's pattern: a process that holds the wider capability,
exports a narrower one, and is the only path between them. `user/src/fs_file_caretaker.rs` opens the
granted name once at startup and then serves the *same* `fs_proto::fs` contract on its own endpoint:

```text
  FS server ──file IPC──► fs_file_caretaker ──narrowed file IPC──► the confined program
              (a directory)          (one file, one direction)
```

Three rules, and each is phrased as a fact about what the holder *has* rather than as a permission
refusal, because there is no policy here to consult:

| the holder asks | answer | why that answer |
|---|---|---|
| `OPEN` any other name | `ENOENT` | in this scope there is no such name. It cannot enumerate, and it cannot learn what else exists |
| `CREATE` | `ENOTDIR` | a file capability is not a directory; "make a name in it" is not a request that means anything |
| `WRITE` / `TRUNCATE` without the direction | `EROFS` | the capability carries one direction. `EACCES` was rejected: it implies a policy that could have said yes |
| `SETXATTR` / `REMOVEXATTR` without the direction | `EROFS` | the same rule, and derived rather than listed: milestone 61's verb table answers "does this verb mutate", so the third way to change a file is refused by the branch that refuses the other two |

### The verb surface is part of what the capability is (milestone 61)

This is the caretaker that needs a **per-verb** answer, and the other two do not. They serve the
directory protocol their client speaks, so every verb means what it always meant and the attenuation
lives elsewhere. A file capability is a *different protocol*: nothing to enumerate, nothing to
create a name in, one handle. So "which verbs exist" is not a filter over a wider surface, it is
what the capability **is**, and it is written down in `fs_proto::verb::file_grant::POLICY`, one row
per opcode, in a crate host tests and Kani can reach.

Three policies, and the third is the one worth keeping distinct:

- `Local`: answered here, from what this process holds. `OPEN` resolves the one granted name; `CLOSE`
  is a truthful no-op, because the caretaker owns the underlying handle for its whole life.
- `Forward`: sent on the caretaker's own handle, with the caller's substituted, gated on the
  direction when the verb mutates.
- `Refused(errno)`: not offered, **and the errno says which kind of "no" this is.** `ENOTDIR` means
  the request does not *mean* anything here, which is a stronger statement than a refusal. Flattening
  that into "allowed / not allowed" is exactly what a table is at risk of doing, so the rows carry
  the errno rather than a boolean.

Milestone 61 also closed the attribute gap here: `GETXATTR`, `SETXATTR`, `LISTXATTR` and
`REMOVEXATTR` used to answer `EOPNOTSUPP`, so a program handed one file could read the file and not
what was attached to it. See [xattr.md](xattr.md).

#### BUGS

- **The directory verbs other than `CREATE` answer `EBADF`, not `ENOTDIR`, and that is inherited
  rather than argued.** Before the table, `OPENDIR`, `READDIR`, `MKDIR`, `RENAME`, `UNLINK` and
  `RMDIR` fell through one `_ =>` arm shared with "you named a handle I never minted", so two
  different statements came out as one word. Writing the rows down is what made the conflation
  visible. `ENOTDIR` is very likely right for all seven, by exactly the argument `CREATE` already
  makes, but changing it changes what a client observes on the wire, so it is a contract decision
  rather than a table's to take. Named here, and in `POLICY`'s own doc comment, so the reader meets
  it where they meet the feature.

### Why the caretaker is a process and not a check inside the FS server

The FS server receives on **one** endpoint. Serving a second, narrower one would need a receive over
a *set* of endpoints, which this kernel does not offer; the way to add it is to give endpoint
capabilities a **badge** (seL4's answer), and that is a design fork, recorded rather than taken. The
caretaker needs nothing new: it is an ordinary FS client above and an ordinary FS server below.

It is also the stronger form of the claim. The confined program holds an endpoint to the caretaker
and **nothing that names the FS server**, so "it cannot reach a second file" is a property of its
cspace, not of a branch it is trusted to take. The boundary is an address space. That is the same
reason milestone 36's checker lives outside the component it checks.

The grant costs no memory. The name and the direction ride in the caretaker's three `START` argument
words (`fs_proto::grant`, 16 bytes of name), and one frame is shared by all three processes, which is
sound because every request on both hops is a blocking `CALL`: the client is parked inside its own
call for the whole time the caretaker touches the page.

### How it is proven, and why one test would not have been enough

An attacker (`fs_test_client`'s third role) reports a **bitmap of what got through**, not a pass. It is run
twice, on both ISAs:

- **Read-only grant of `motd`: every bit must be clear.** It tries to open `scratch`, which exists,
  sits one directory entry away, and the caretaker could open on any request it liked. It tries to
  write and truncate the file it *can* read (refusing a write to a file it cannot even name would
  prove nothing). It tries to create. It sprays handle numbers.
- **Read/write grant, same shape: the write bits must be SET and everything else clear.** Three of
  them since milestone 61: `WROTE`, `TRUNCATED`, and `WROTE_ATTR`, which is the third way to change
  a file and would have been missed by a direction check that only covered the first two.

The read-only run also carries the attribute half of milestone 61, in the clear bits: the listing
and the get reached the store (so the caretaker really does forward them) and the set did not (so a
read grant really does not). Before that milestone all four answered `EOPNOTSUPP` and the first half
would have failed.

The second run is what makes the first mean anything. A caretaker that refused every request would
pass the read-only test, and so would a grant that reached nothing at all; it fails the writable
one. Each accepted write is read straight back, because "the server accepted my write" and "my write
landed" are different claims. This is milestone 36's two-witness shape, and milestone 33's rule that
an attacker must be pointed at a real neighbour rather than a fictional one.

### The manifest declares the direction; the command line designates the file

`wc report.txt` reads and `tee report.txt` writes, with no flag either way. The split is
SHILL's and it is deliberate: whether a program writes is a property of what it does and belongs in
its published manifest, while *which* file is the human's business and belongs on the line. The
authority is still exactly what the line says, because the program's half is fixed and readable.
`caps wc report.txt` prints it:

```text
  wc would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    cap 2  endpoint  file     report.txt  (read-only, and nothing else on the disk)
```

### At the interactive prompt (milestone 50 wired it, and one thing is still missing)

The refusal this section used to describe is gone, and the reason it went is the point of the way it
was written. It said "you hold no such capability: this shell was granted no directory to narrow",
which was a **fact about the shell's cspace** rather than a release date; milestone 50 gave the
interactive boot a RedoxFS disk and had init narrow the file service into the shell, and the same
sentence stopped being printed with no edit to the condition that prints it. Phase 1's first draft
hardcoded "arrives with milestone 32", which was true when written and would have become a lie the
moment the mechanism landed.

So at the prompt today the shell holds the image root, `holdings().dir` is true, and a name on the
line resolves. What has **not** been built is the last item of phase 3's list: **init does not build
a caretaker per grant.** Two consequences, both visible from the prompt:

- **`rm` is still unreachable.** It declares `DirSpec::Required`, so its grant is an endpoint served
  by a `fs_subtree_caretaker` that init would have to build per invocation, out of the FS endpoint it
  currently deletes after building the shell. `spawn` says so rather than starting `rm` with nothing,
  which is the one failure this model must never have: a program told to destroy something, holding
  nothing, saying nothing.
- **`FileSpec::Required` still has no consumer.** The per-file caretaker is built and proven on both
  ISAs (below), and no shipped program declares a file. The one the milestone block names is `wc`,
  and milestone 50 deliberately made it something else: a stream consumer that cannot speak the
  filesystem contract at all, on the argument that "a `wc` that could open the file it counts would
  be a `wc` that could open any file". That argument is right and it is *later*, so the block's
  `wc report.txt` is a proof of designation rather than of the file-capability path.

### `wc report.txt`: the input operand, and what it does and does not prove

Naming a file to a program that declares an input grants it that file's bytes. `wc report.txt` is
`wc < report.txt` with the operator left out, and it runs down the same path: the planner puts the
designated name in the stage's **source**, the shell opens it and streams it, and the child holds an
endpoint.

This is milestone 47's move applied to the other direction. The `file:` prefix came out on the
finding that the manifest was doing all the work, and it is doing all of it here too: a program that
declares `InputSpec::Required` reads a stream and declares no file and no directory, so a bare name
after its declared grants can only be the thing feeding it. There is nothing for the parser to
classify. A manifest that declared both an input and a file would need positional arity, which is the
same widening `ArgSpec` is waiting on, and no manifest declares both today.

**Designation is authorization here, and the negative control is what says so.** `wc` with no name is
refused at the prompt, before anything is spawned, because its manifest says it reads a stream; on
Unix the same command is a shell that appears to hang. So the name is what moved something.

**What it is not** is the per-file capability `FileSpec::Required` describes, and the difference is
worth stating rather than glossing. The child gets bytes: it cannot seek, re-read, stat, or name a
second thing, and it cannot tell a file from a pipe from a builtin typing at it. That is *narrower*
than a file capability, not wider, so nothing is over-granted; what is missing is the claim that the
**child** holds the file. `caps` prints which it is, in the child's own terms:

```text
$ caps wc gate.txt
  wc would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    output   this shell's result endpoint (it reads the bytes and prints them)
    input    gate.txt  (this shell reads it and streams it in; the program
             holds an endpoint, not a file)
```

`script/shell-check` types `wc gate.txt` and `wc < gate.txt` at the real prompt on both ISAs and
requires the same three numbers from both, which is what makes "the same designation" a claim about
the machine: one line reaches the file through an operator and one through a name, so if they
disagree one of them opened something else.

## `date` at the prompt, and the authority the command line cannot name

`date` became reachable from the shell with the grammar change, because with `run` gone `date` is
exactly what a person types. Its manifest is all `Forbidden`: no argument, no memory, no file. It is
the first program in the table whose **whole authority is something the command line cannot
designate**, and that is worth being explicit about rather than letting it read as an oversight.

What `date` holds is a read-only mapping of the clock page (DECISIONS §43, notes/clock.md). Read,
set and propose are three different objects there, and the reason `date -s` cannot exist is that the
read authority is a page permission rather than a check the program could skip. None of that is
expressible on a command line, so there is nothing to type and nothing to get wrong.

**The interactive boot now starts a clock service and hands the page to init**, on both ISAs, so
`date` at the prompt prints a time. The shell is not on that path and holds no clock: `Manifest`
grew a `clock: bool` the way it has `reports: bool` (a fixed fact about the program, not a
designation), and *init* reads it and endows the child. `caps date` prints the row anyway, because a
preview that showed only what the line designates would be off by exactly one capability:

```text
$ caps date
  date would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    cap 1  frame     clock    read-only. it can read the time and not set it,
                              and no token on the line could have asked for more
    arg    (none)
  reading the command is reading its whole authority.
$ date
  Tue 2026-08-04 00:35:59 UTC
```

### The four links this note said were missing, and the one it got wrong

The assessment above listed four, in different subsystems, and three of them were right:

1. **The interactive boot started no clock service.** Correct, and it was the bulk of the work: the
   kernel starts it before init exists on both boot paths.
2. **Init had no way to receive the page.** Correct: it is a read-only frame capability now, granted
   ahead of the filesystem pair so its slot number does not depend on whether a disk was attached.
3. **"The spawn protocol carries no clock. A clock is a third position and a new flag word, in both
   inits."** **Wrong, and the reason is the interesting part.** A clock is not designated on the
   command line, so there is nothing for the shell to *send*: init already decodes the program id, so
   it can read that program's manifest itself and decide. The wire did not change at all. The general
   rule that falls out: a flag word carries what the **sender chose**, and an authority the sender
   could not choose does not belong on it.
4. **The child needs the page mapped and the cap inserted.** Correct, at `CLOCK_VA` and slot 1,
   because `date` probes the slot before touching the address.

The old paragraph also said all of it "would ship unexercised, because nothing in the test suite
boots the interactive shell". That stopped being true when milestone 50 wrote `script/shell-check`,
which is the gate this landed against.

### What a delegable clock would still need

Nothing here lets the *shell* hand a clock to anything. That is a real difference and not a
formality: a shell holding the page could grant it to any child, and init granting it per manifest
means the set of processes that can read the time is decided by declarations rather than by a prompt.
Making it delegable would mean init keeping a copy for the shell, a `Holdings` field, and a clock
position on the wire after all. There is no program asking for it, so it is recorded and not built.

## What phase 1 deliberately does not do

- **No live cspace introspection.** `caps` prints the shell's own endowment (which it knows by the
  boot convention) and previews a command's grant (from the manifest). Reading *another running
  process's* cspace would need a new kernel method (a debug/reflection capability), which is a
  design fork, not built here. The manifest is the userspace stand-in, and for the shell's purpose
  (what would this command grant?) it is the right answer anyway: the authority is the command, and
  the command is on the screen.
## The interrupt grant (milestone 24)

A foreground job the user can `^C` is another grant the command line expresses, and it flows the
same way: the manifest marks a program `interruptible`, and the shell endows a supervised job with
what the two-tier interrupt (DECISIONS §24) needs, and nothing more.

- **A shared job frame** the shell mints per job (`grant_plan::jobframe`) and maps into the child. The
  cooperative signal is a word in it: on the first `^C` the shell writes the interrupt flag, and a
  cooperative program reads it between work units and exits cleanly. Shared memory, not an endpoint,
  because a running computation cannot poll an endpoint (no non-blocking receive); this is the one
  place control rides in memory rather than a message, and the note says why.
- **A job untyped** the shell splits from its own budget and delegates for init to build the *whole*
  child from, so the child's region is one the shell holds. That is what makes the forcible tier a
  capability the shell already has: a second `^C` tears the job down with `Untyped::DESTROY` on that
  region (which force-kills the resident thread, §16 amendment), and even a runaway that ignores the
  cooperative flag ends and the prompt returns.

A program the command did not run as a supervised job holds no job frame and no reclaimable region,
so it cannot be signaled or torn down through this path; the authority is exactly the endowment, as
everywhere else. The escalation policy (how many `^C`, the grace timeout) is the shell's, host-tested
in `grant_plan::Escalation`. The two demonstrators are `heeder` (heeds the cooperative `^C`) and `spinner`
(a bare loop only the forcible tier ends). See DECISIONS §24's implementation amendment and
notes/terminal-contract.md's `OP_INTRCOUNT`.
