# 126. The `procps` package: who else is running, and who is allowed to ask

**Status: PARTIAL.** Minted 2026-08-14 by calef, from a design conversation about what ambient
authority utilities become on this system. **Scoped to the whole package by calef the same day**, for
consistency with milestone 123's approach to popular packages: the corpus is chosen by an external
ordering and taken in the units that ordering uses, which is packages rather than programs we like.

**Gate: NONE.** `ps` needs nothing that does not exist. The rest need work this milestone builds.

## Built: the first stratum, 2026-08-16

`ps` works on both ISAs, and the view it reads is `abi::endpoint::SURVEY`: **a new method on the
supervision endpoint, no new syscall number**. Membership is `capability::survey_includes`, the same
relationship §32 authorizes a reap with, so the domain a monitor sees and the domain a supervisor may
collect from cannot diverge; three Kani harnesses hold that. Written up in notes/process-view.md,
with the semantics of the new method stated there for the integrator.

What the demonstration actually shows, and it is the negative control rather than the listing: a
viewer holding the endpoint **send-only** is refused (`NotPermitted`), a viewer holding nothing is
refused (`NoSuchSlot`), and a domain that is genuinely empty **answers**. Three distinct outcomes
where `/proc` has one, proved in one cross-ISA kernel test whose walk is `ps::collect`, the real
program's real loop.

`Manifest::domain` is the declaration that gets the grant to a `ps` at the prompt, `clock`'s twin,
and `caps ps` prints the scope before anything is spawned.

**The honest finding this turned up, recorded rather than fixed:** a view riding on `READ` is wider
than looking needs, because `READ` on a supervision endpoint is also what `RECV` and
`endpoint::REAP` take. Splitting view from control changes the rights model, and it is the same
decision the signalling stratum has to make to give `pgrep` and `pkill` genuinely different rights.
Deciding it once, there, beats deciding it twice; notes/process-view.md's `BUGS` carries the whole
argument.

**Still to build:** the rest of the view stratum (`top`, `pgrep`, `pmap`, `pwdx`, `w`), the
signalling stratum, the machine-wide statistics, `watch`, and the `sysctl` fork below. The package
file list still wants a real `dpkg -L procps` before anyone counts programs; nothing built so far
depended on it, and the next lane does.

## Why this package, and why the package rather than the program

**What these programs want is enumeration of the process namespace, and enumeration is the authority
this system is built to refuse.** Milestone 121 makes the same argument for directories: a program
that can list learns what exists, which is a larger power than reading something it was handed.

On Linux the answer comes from `/proc`, which is **ambient**. Any process reads it with no grant from
anyone, so `ps aux` prints every command line on the machine, including the ones with secrets in
`argv`. Nobody defends that design; they live with it, and `hidepid` exists because enough people
stopped wanting to.

That makes this a better first demonstration than `ripgrep` on one axis: **the reader already knows
the Unix behaviour is wrong.** The claim needs no setup.

`procps` (upstream `procps-ng`) is Priority: important, so it is on essentially every Ubuntu install
and high on any popcon ordering. **Taking the package whole is the point rather than a burden**: it
is the unit the distribution ships, so it is the unit that tests whether the approach generalises. A
port that cherry-picked the two programs with the tidiest capability story would prove nothing about
typical software.

**Confirm the file list before building.** The table below is from memory and wants a real
`dpkg -L procps` against a current Ubuntu; getting the membership wrong would silently change the
scope.

## The strata, which are the build order

**The package is the unit of ambient authority, not the program.** All of these exist because
`/proc` is readable by anyone. Once that is replaced by a held capability, they stop being one thing:

| what it actually needs | programs | state here |
|---|---|---|
| **read the process namespace** | `ps`, `top`, `pgrep`, `pmap`, `pwdx`, `w` | the supervision tree exists (§26's `fault_ep`); the view does not |
| **signal a process** (control, not view) | `kill`, `pkill`, `skill`, `snice` | `Tcb` carries `DESTROY`; nothing splits view from control yet |
| **machine-wide statistics**, no process namespace | `free`, `uptime`, `vmstat`, `slabtop`, `tload` | **a different capability entirely**, and none exists |
| **write kernel tunables** | `sysctl` | no design, and see the fork below |
| **none of the above** | `watch` | nearly free: `line_editor` and the compositor already exist |

Build in that order. `ps` first, because it is a snapshot of the domain and needs no clock and no
accounting, so it is the whole capability argument with none of the scheduler work. Then the rest of
the view stratum, then signalling, then statistics, then `sysctl`, then `watch` whenever.

## The design: a view over a supervision domain

**The scope is the supervision subtree, because the kernel already maintains it.** A shell holds a
domain; the programs it spawns are in that domain; a `ps` launched from that shell sees exactly those
and nothing else. Same move `rm -r` makes with a directory subtree and `ripgrep` will make with
`ENUMERATE`: **authority is a subtree, not a global.** A scope the system already keeps cannot drift
out of agreement with reality.

**A wide grant is fine and must be nameable.** An operator's `top` genuinely wants the whole machine.
The point is not to forbid it but to make it visible: `caps top` should print the difference between
a `top` that sees one shell's children and a `top` that sees everything. On Linux there is no such
distinction to print, which is the whole difference.

## The demonstration: `pgrep` beside `pkill`

**These two do the same lookup and differ only in what they do with the answer**, which makes them a
better demonstration than any test this milestone could invent. On Unix they are siblings in one
package with identical `/proc` access and the split between them is convention. Here they hold
genuinely different rights, and `caps pgrep` beside `caps pkill` prints that as two lines.

It beats "run a monitor and try to kill something" because there is no argument about whether the
program was artificially hobbled to make a point. **Both programs already exist upstream and already
differ**; all this system does is make the difference structural instead of conventional.

The negative control keeps milestone 108's shape: a viewer run against a domain it was not granted is
**refused loudly** rather than shown an empty list. A monitor that silently reports nothing because
it could not look is the worst failure available to this tool, and `fs_proto` already chose `EPERM`
over an empty listing for exactly that reason.

## `sysctl` is a design fork, not a program to port

It writes machine-global kernel tunables, and **it ships in the same package as `ps`**, which is a
striking illustration of what Unix packaging bundles: `apt install procps` gets you process listing
and the ability to retune the kernel.

There is no ambient tunables namespace here to write to, and inventing one would import exactly the
thing this system exists to avoid. The plausible shapes:

- **A capability per subsystem**, so `sysctl` becomes a program that holds a bag of them and can
  change only what it was handed. Honest, and it means `sysctl` on this system is a different program
  wearing the same name.
- **No `sysctl` at all**, with each subsystem's tuning reached through that subsystem's own service.
  Cleaner, and it breaks the package's coverage claim, which is worth saying out loud rather than
  quietly dropping one binary from a list of seventeen.

**This is calef's**, and it should be decided before the statistics stratum rather than after, because
it decides whether "we implemented `procps`" is a true sentence.

## The other fork: where the process view comes from

**Derive it from the supervision tree** (recommended), or **give processes a separate namespace with
its own capability**. The first is elegant, already exists, and cannot disagree with reality. The
second is more flexible: it can express a monitor watching two unrelated services, which a subtree
view can never say. Taking the first forecloses that, and the honest workaround is a supervisor whose
only purpose is to be their common parent.

## Prior art

**A design to copy: Fuchsia's job handles**, which is this milestone's design already shipped. A
Fuchsia process lives in a *job*, jobs nest, and listing processes requires a handle to the job whose
children you want; their `ps` needs a handle to the root job to see everything. That is exactly the
"wide grant, explicitly held" shape proposed here. Worth reading for how they handle a process that
dies mid-enumeration, which this block has no answer for.

**A mistake to avoid: `/proc` as an ambient filesystem.** Plan 9 made `/proc` cleaner than Linux did
and still put process state in a namespace a program reaches by *naming* rather than by *holding*.
Getting this wrong looks like `ps` working beautifully while the confinement is decorative.

**Code to use:** none for the capability half. The rendering (columns, sorting, a redrawing terminal)
is ordinary, and `line_editor` and the compositor already exist beneath it.

## BUGS

- **Estimating the package from the `ps` half gets it wrong twice.** `top` needs per-thread CPU
  accounting that does not exist at all: `QuotaToken` is dead code whose own comment says
  `spawn_with_quota` "has no caller of its own today". And `free`, `uptime` and `vmstat` want machine
  statistics rather than process enumeration, so **building `top` does not give you `free`**. Three
  separate bodies of work wear one package name.
- **Aggregate statistics are a side channel, and capabilities do not close it.** CPU time per process
  leaks information about work the viewer was never shown, even with names withheld. A capability
  bounds *who* may ask; it says nothing about what the numbers reveal to whoever may. A real limit of
  the model, recorded next to the feature rather than in a threat model nobody reads.
- **A process has no name here.** `ps` shows command lines; this system has `arg0` in `Spawn` and no
  display name. A name is information rather than authority, but a confined viewer may still not be
  entitled to it, and there is no design for that today.
- **A supervision-derived view cannot express a non-subtree set**, if that fork lands that way. The
  workaround is a supervisor existing only to be a common parent, and it should be recorded when the
  fork is decided rather than found by whoever first needs it.
- **The comparison against Linux is not apples to apples and the write-up must say so.** Ours lists a
  domain; theirs lists a machine. That is the entire point, and a table putting them side by side
  without stating it would be dishonest in the way §14's map "tie" caveat exists to prevent.
- **The package membership above is from memory.** It needs `dpkg -L procps` on a current Ubuntu
  before anyone counts programs or estimates from it.
