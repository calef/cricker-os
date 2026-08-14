# 126. `ps` and `top`: who else is running, and who is allowed to ask

**Status: NOT-STARTED.** Minted 2026-08-14 by Chris, from a design conversation about what ambient
authority utilities become on this system.

**Gate: NONE.** `ps` needs nothing that does not exist. `top` needs per-thread CPU accounting, which
this milestone builds rather than waits for.

## Why this is the sharpest case in the utility set

**What these programs want is enumeration of the process namespace, and enumeration is the authority
this system is built to refuse.** Milestone 121 makes the same argument for directories: a program
that can list learns what exists, which is a larger power than reading something it was handed.

On Linux the answer comes from `/proc`, which is **ambient**. Any process reads it with no grant from
anyone, so `ps aux` prints every command line on the machine, including the ones with secrets in
`argv`. Nobody defends that design; they live with it, and `hidepid` exists because enough people
stopped wanting to.

That makes this a better first demonstration than `ripgrep` on one axis: **the reader already knows
the Unix behaviour is wrong.** The confinement claim needs no setup. "On Linux any user can see every
process's command line. Here a monitor sees exactly the processes it was given, and `caps` prints
which."

## What exists today, measured rather than assumed

| piece | state |
|---|---|
| a `ps` or `top` program | **none**, nothing in `user/src/` does this |
| the supervision tree (who reports to whom) | **built**: every thread carries `fault_ep`, the endpoint its corpse reports to (§26) |
| the `caps` builtin, which prints a command's authority | **built**: `grant_plan::Command::Caps`, routed by `swish` |
| the wall clock, as a read-only page | **built**: milestone 51, DECISIONS §43 |
| per-thread CPU accounting | **absent**. `QuotaToken` exists and its own comment says `spawn_with_quota` "has no caller of its own today" |

## The design: a view over a supervision domain

**The scope is the supervision subtree, because the kernel already maintains it.** A shell holds a
domain; the programs it spawns are in that domain; a `ps` launched from that shell sees exactly those
and nothing else. Same move `rm -r` makes with a directory subtree and `ripgrep` will make with
`ENUMERATE`: **authority is a subtree, not a global.**

A scope the system already keeps cannot drift out of agreement with reality, which is the argument
for deriving it rather than inventing a second namespace (see the fork below).

**Seeing is not controlling, and the two must be separate rights.** A `Tcb` capability carries
`DESTROY`. A monitor needs neither that nor the ability to send to the thread. Unix conflates them
behind uid checks, which is why `top` can kill what it can see. Here a viewer should be grantable the
read right alone, and that is a claim a test can make: **run the monitor, try to kill something,
and get `NotPermitted` from the kernel rather than from a policy check inside the program.**

**A wide grant is fine and must be nameable.** An operator's `top` genuinely wants the whole machine.
The point is not to forbid it but to make it visible: `caps top` should print the difference between
a `top` that sees one shell's children and a `top` that sees everything. On Linux there is no such
distinction to print, which is the whole difference.

## Build `ps` first, and it is buildable today

The two programs split cleanly, and the split is the schedule:

**`ps` is a snapshot** of the domain: what exists, its state (`Ready`, `Blocked`, `Dead`), its
supervisor. **It needs no clock and no accounting**, so it needs nothing this tree does not have. It
is the whole capability argument with none of the scheduler work.

**`top` is `ps` in a loop with time**, and it needs two things `ps` does not: **per-thread CPU
accounting in the scheduler**, which does not exist, and the clock page to sample against. The
accounting is the unglamorous majority of this milestone and it is worth saying so up front rather
than discovering it.

## The demonstration

`ps` from a shell that spawned three programs lists exactly three, with a fourth program running
outside that domain **absent rather than hidden**. Then the negative control, in milestone 108's
shape: the same binary, run with a view it was not granted, refused loudly. A monitor that silently
shows an empty list because it could not look is the worst failure available to this tool, and
`fs_proto` already chose `EPERM` over an empty listing for exactly that reason.

The pairing that makes it land: **the same command line, on Linux and here, with `caps` printed
beside it.** One of them can read every command line on the machine.

## The fork, which is Chris's

**Derive the process view from the supervision tree** (recommended), or **give processes a separate
namespace with its own capability**. The first is elegant, already exists, and cannot disagree with
reality. The second is more flexible: it would let a monitor watch a set that is not a subtree, which
a supervision-derived view can never express. Taking the first forecloses that, and the honest cost
is worth stating rather than discovering later.

## Prior art

The three questions, against `notes/prior-art.md`'s ecosystems.

**A design to copy: Fuchsia's job handles**, which is this milestone's design already shipped. A
Fuchsia process lives in a *job*, jobs nest, and listing processes requires a handle to the job whose
children you want. Their `ps` needs a handle to the root job to see everything, and that is exactly
the "wide grant, explicitly held" shape proposed here. Worth reading for how they handle a process
that dies mid-enumeration.

**A mistake to avoid: `/proc` as an ambient filesystem.** Plan 9 made `/proc` cleaner than Linux did
and still put process state in a namespace a program reaches by *naming* rather than by *holding*.
Getting this wrong looks like `ps` working beautifully while the confinement is decorative.

**Code to use:** none for the capability half. The rendering half (columns, sorting, a terminal that
redraws) is ordinary and `line_editor` and the compositor already exist beneath it.

## BUGS

- **Aggregate statistics are a side channel, and capabilities do not close it.** CPU time per process
  leaks information about work the viewer was never shown, even with names withheld. A capability
  bounds *who* may ask; it says nothing about what the numbers reveal to whoever may. This is a real
  limit of the model and belongs next to the feature rather than in a threat model nobody reads.
- **A process has no name here.** `ps` shows command lines; this system has `arg0` in `Spawn` and no
  display name. A name is information rather than authority, but a confined viewer may still not be
  entitled to it, and there is no design for that today.
- **`QuotaToken` is dead code**, so the accounting starts from nothing. Anyone estimating this
  milestone from the `ps` half will estimate it wrong.
- **A supervision-derived view cannot express a non-subtree set.** If the fork lands on the
  supervision tree, a monitor that should watch two unrelated services has no way to say so, and the
  workaround is a supervisor that exists only to be their common parent. That is a real shape and it
  should be recorded when the fork is decided rather than found by someone who needs it.
- **The demonstration compares against a Linux `ps` that is doing something genuinely different.**
  Ours lists a domain, theirs lists a machine. That is the point, and a benchmark table that puts
  them side by side without saying so would be dishonest in the way §14's map "tie" caveat exists to
  prevent.
