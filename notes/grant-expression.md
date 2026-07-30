# The command line as a grant expression

Milestone 31, phase 1. This is the note on the one idea the capability shell exists to make
visible: on a cricker-os command line, **naming a resource is how you grant it**. Mark Miller's
principle, "designation is authorization," applied at the one interface a human touches. The pure
logic lives in the `capsh` crate (host-tested); the wiring is `shell.rs` and the two init paths
(`hello.rs` init_boot on aarch64, `sysinit.rs` on riscv). The manifest half is written up
separately in [program-manifest.md](program-manifest.md).

## What Unix does, and why it is the opposite

A Unix child inherits every one of your file descriptors and runs under your uid, so it may
`open()` anything your uid allows. Authority comes from **who you are**, and it flows to a child
whether or not the command mentioned it. `grep secret public.txt` hands grep the authority to read
every file you own; that it only touches `public.txt` is grep's good manners, not a limit the
system imposed. This is ambient authority ([capabilities.md](capabilities.md)), and it is what
makes the confused deputy constructible.

The inversion: a cricker-os command grants **exactly what it names, and nothing else**. A program
that names no resource gets none. There is no ambient pool to draw from, so the question "may I?"
is never asked; there is simply nothing in the program's hands it was not given. `run worker 9`
grants a report channel and an argument. `run --mem 16 budgeter` grants a report channel and a
16-page memory budget. `run budgeter` alone grants a report channel and is refused, because
budgeter's manifest says it needs memory and the command named none.

## The grammar

```text
run [--mem N] <prog> [arg] [file:PATH ...]
caps [run ...]
help
echo <text>
```

`run` is the grant expression. Its arguments are designators:

- `--mem N` designates **N pages of untyped**, carved from the shell's own budget.
- `<prog>` names the program to spawn (a closed set in phase 1; `worker`, `budgeter`).
- a bare integer is the program's argument (worker's `n`).
- `file:NAME` designates a **file**: one name, at most 16 bytes, no path. See the per-file grant
  section below for what the shell narrows it from and what it can back today.

The `file:` prefix is explicit rather than inferring "a bare non-numeric token must be a file",
because an unplaceable token is refused (`Refusal::Unexpected`) and silently reclassifying it as a
grant would turn a typo into a capability transfer.

`caps` is introspection: with no argument it prints the shell's whole endowment; with a `run` tail
it previews exactly what that command would grant, so reading the command is reading the child's
authority. That is DECISIONS §14's claim made interactive.

## Where the authority actually comes from, and how it moves

The shell holds four capabilities (init grants them at boot, in this order): the terminal endpoint
(slot 0), a spawn endpoint to init (slot 1), a result endpoint (slot 2), and **its own untyped
budget** (slot 3). The budget is the piece milestone 31 added: init splits it off its own untyped
and `CAP_INSERT`s it into the shell, so the shell has memory that is genuinely its to give.

The shell does not build children itself; init holds the initrd and stays the ELF loader (the
parser lives in one place, out of the shell). So the shell **directs** init and **delegates** the
capabilities it grants, over the spawn endpoint. The protocol (`capsh::spawnproto`, a userspace
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

Making `run --mem N` real, and not parsed-and-ignored, needed a kernel fix, recorded as an amendment
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
the authority the command handed it. `run --mem 16 budgeter` reports **15** pages mapped on both
ISAs: the sixteenth paid for the page table that reaches the others (the kernel allocates nothing on
a process's behalf, DECISIONS §10). Grant more and it maps more; grant nothing and it holds no
untyped at slot 1 at all, so its first `MAP` returns `NoSuchSlot` and it maps zero. There is no
ambient pool behind it. This is the demonstration that `--mem` moves real memory, not a parsed
number.

## The refusals read like the model, not like errno

A refusal is a fact about what the shell holds, phrased in the capability model's voice:

- `run frobnicate 1` → "frobnicate: no such program." There is nothing to name.
- `run budgeter` → "budgeter: needs a memory grant; add --mem <pages>." The manifest caught it.
- `run --mem 8 worker 3` → "worker: takes no memory grant; drop the --mem."
- `run worker 3 file:report.txt` → "worker: **you hold no such capability**: this shell was granted
  no directory to narrow."
- With a directory in hand, the same line becomes "worker: takes no file; drop the file: designator",
  because worker's manifest declares none, and a designator the program has no use for is authority
  the user thought they were moving. It is refused, not granted-and-dropped.
- `run wc file:sub/report.txt` → "wc: that is not a name this shell can grant: one component, at most
  16 bytes." There is no namespace here to walk, so a path is refused where it was typed rather than
  becoming an `ENOENT` from a server asked something meaningless.

The `no such capability` line is the headline refusal, and it is a statement about the shell's own
cspace: "there is nothing I hold that could grant this," never a Unix-flavored EPERM.

## Per-file grants: one file, one direction, and a caretaker in between

*(Phase 2. The `file:PATH` grammar was designed in phase 1 so this would slot in without a grammar
change, and it did.)*

The filesystem's unit of authority is a **directory**: the endpoint a client holds IS the directory
capability, and every name in an `OPEN` resolves under it (DECISIONS §27). `run wc report.txt` says
less than that. It names one file, so it must grant one file.

The narrowing is a **caretaker**, Mark Miller's pattern: a process that holds the wider capability,
exports a narrower one, and is the only path between them. `user/src/fwarden.rs` opens the granted
name once at startup and then serves the *same* `fs_proto::fs` contract on its own endpoint:

```text
  FS server ──file IPC──► fwarden ──narrowed file IPC──► the confined program
              (a directory)          (one file, one direction)
```

Three rules, and each is phrased as a fact about what the holder *has* rather than as a permission
refusal, because there is no policy here to consult:

| the holder asks | answer | why that answer |
|---|---|---|
| `OPEN` any other name | `ENOENT` | in this scope there is no such name. It cannot enumerate, and it cannot learn what else exists |
| `CREATE` | `ENOTDIR` | a file capability is not a directory; "make a name in it" is not a request that means anything |
| `WRITE` / `TRUNCATE` without the direction | `EROFS` | the capability carries one direction. `EACCES` was rejected: it implies a policy that could have said yes |

### Why the caretaker is a process and not a check inside the FS server

The FS server receives on **one** endpoint. Serving a second, narrower one would need a receive over
a *set* of endpoints, which this kernel does not offer; the way to add it is to give endpoint
capabilities a **badge** (seL4's answer), and that is a design fork, recorded rather than taken. The
caretaker needs nothing new: it is an ordinary FS client above and an ordinary FS server below.

It is also the stronger form of the claim. The confined program holds an endpoint to the warden and
**nothing that names the FS server**, so "it cannot reach a second file" is a property of its cspace,
not of a branch it is trusted to take. The boundary is an address space. That is the same reason
milestone 36's checker lives outside the component it checks.

The grant costs no memory. The name and the direction ride in the warden's three `START` argument
words (`fs_proto::grant`, 16 bytes of name), and one frame is shared by all three processes, which is
sound because every request on both hops is a blocking `CALL`: the client is parked inside its own
call for the whole time the warden touches the page.

### How it is proven, and why one test would not have been enough

An attacker (`fsclient`'s third role) reports a **bitmap of what got through**, not a pass. It is run
twice, on both ISAs:

- **Read-only grant of `motd`: every bit must be clear.** It tries to open `scratch`, which exists,
  sits one directory entry away, and the warden could open on any request it liked. It tries to write
  and truncate the file it *can* read (refusing a write to a file it cannot even name would prove
  nothing). It tries to create. It sprays handle numbers.
- **Read/write grant, same shape: the two write bits must be SET and everything else clear.**

The second run is what makes the first mean anything. A warden that refused every request would pass
the read-only test, and so would a grant that reached nothing at all; it fails the writable one. Each
accepted write is read straight back, because "the server accepted my write" and "my write landed"
are different claims. This is milestone 36's two-witness shape, and milestone 33's rule that an
attacker must be pointed at a real neighbour rather than a fictional one.

### The manifest declares the direction; the command line designates the file

`run wc report.txt` reads and `run tee report.txt` writes, with no flag either way. The split is
SHILL's and it is deliberate: whether a program writes is a property of what it does and belongs in
its published manifest, while *which* file is the human's business and belongs on the line. The
authority is still exactly what the line says, because the program's half is fixed and readable.
`caps run wc file:report.txt` prints it:

```text
  run wc would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    cap 2  endpoint  file     report.txt  (read-only, and nothing else on the disk)
```

### What the interactive shell cannot do yet, and why that refusal is true

At the prompt today, `run prog file:x` is still refused with "you hold no such capability: this shell
was granted no directory to narrow". That is a **fact about the shell's cspace**, not a placeholder:
the boot that starts it wires no FS service, so init grants it a terminal, a spawn channel, a result
channel and a budget, and nothing that names a filesystem. `caps` prints the absence in those words.

The decision is a function of `capsh::Holdings`, not of the calendar, and that distinction is the
lesson. Phase 1 hardcoded the refusal ("arrives with milestone 32"), which was true when written and
would have quietly become a lie the moment the mechanism landed. A refusal that describes what you
hold stays true as your holdings change; one that describes a release does not.

What remains is wiring an FS service into the interactive boot (kernel boot path, a RedoxFS disk on
the interactive runner, init building the warden per grant). It is deliberately not built here because
**nothing in the test suite boots the interactive shell**, so it would ship unexercised. The
mechanism it would use is proven on both ISAs by the tests above.

Phase 1 grants what exists today: program spawns, endpoints, frames, untyped budgets, device caps.
The shell demonstrates untyped-budget and endpoint grants concretely; the others share the same
`SEND_CAP`-to-init path.

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

- **A shared job frame** the shell mints per job (`capsh::jobframe`) and maps into the child. The
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
in `capsh::Escalation`. The two demonstrators are `heeder` (heeds the cooperative `^C`) and `spinner`
(a bare loop only the forcible tier ends). See DECISIONS §24's implementation amendment and
notes/terminal-contract.md's `OP_INTRCOUNT`.
