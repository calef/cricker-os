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
- `file:PATH` designates a **file**, which the shell cannot grant yet (see below).

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
- `run worker 3 file:report.txt` → "worker: **you hold no such capability**: this shell cannot
  grant files (arrives with milestone 32)."

That last one is the headline refusal and the forward-compatibility hook at once. A file designator
is parsed today, but the shell holds no directory capability, so it cannot back the grant, and the
honest answer is "there is nothing I hold that could grant this," never a Unix-flavored EPERM.

## What waits for the filesystem (milestone 32)

Per-file grants need something to point at: a directory capability the FS server hands out, resolved
by path only *inside* the server. The `file:PATH` grammar is designed and parsed now precisely so it
slots in without a grammar change when milestone 32 lands. Phase 1 grants what exists today: program
spawns, endpoints, frames, untyped budgets, device caps. The shell demonstrates untyped-budget and
endpoint grants concretely; the others share the same `SEND_CAP`-to-init path.

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
