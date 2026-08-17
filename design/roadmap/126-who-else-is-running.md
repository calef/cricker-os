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

**The finding this turned up, and it is now fixed rather than recorded.** A view riding on `READ`
was wider than looking needs, because `READ` on a supervision endpoint is also what `RECV` and
`endpoint::REAP` take: a `ps` could have collected the children it listed. The lane left it on the
reasoning that the signalling stratum needed the same decision, and that reasoning did not survive
the ruling below. `capability::Rights::ENUMERATE` is the answer, the kernel-level twin of
`fs_proto`'s directory `ENUMERATE` and the same argument one layer down; `SURVEY` takes it, `RECV`
and `REAP` still take `READ`, and a viewer is granted `ENUMERATE` alone. notes/process-view.md
carries the whole argument.

**Only `Endpoint` consults the new right today, deliberately.** `Aspace` wants it when `pmap` is
built (observe a mapping without being able to map) and `Untyped` wants it when `free` is built
(ask what is committed without being able to `SPLIT` or `DESTROY`). Both are named here rather than
pre-wired, because a right defined for hypothetical callers is the speculative abstraction this
tree declines; the consumer that can say what it needs is the one that should extend it.

## Built: `pgrep`, 2026-08-17

**`pgrep` works on both ISAs, and it is granted exactly what `ps` is: one supervision endpoint with
`ENUMERATE`, and not one right more.** The two manifests in `grant_plan` are identical field for
field, which is the readable form of the ruling below rather than a redundancy to factor out. It
filters the survey `ps` already walks (`crates/pgrep` takes a finished `ps::Survey` and never touches
a cursor), and the selector is the run state, matched by `crates/glob` against the four state names
`ps` prints.

**What the demonstration shows, and it is one answer more than `ps` had.** A selector that **matched
nothing** in a domain that really has members is a distinct outcome from an empty domain and from a
refusal, and upstream `pgrep` collapses all three into printing nothing and exiting 1, so a monitor
there cannot tell an idle machine from a closed door. Four answers, asserted in twelve host tests and
again on a real kernel domain on both ISAs
(`kernel::user::survey_tests::a_filter_names_members_and_tells_its_four_answers_apart`), which also
asserts the claim the whole pair exists to make: **the capability that printed a corpse's tid is
refused the reap.**

**The limitation that shaped it, and it is a property of the boundary rather than of the program.**
Nothing in this system delivers *bytes* from a command line to a program: `Endowment::arg` is one
`u64`, `spawnproto` carries three words, and every string-shaped designation a person types arrives at
the child as a **capability**, which is what `rm logs/old` and `doc notes/glob.md` both are. So a
pattern cannot be typed at this prompt, and the shipped `pgrep` names every member, which is
`Prog::Date`'s deliberate under-declaration verbatim. It lifts when `ArgSpec` grows milestone 47's
deferred positional arity. Written up in notes/process-view.md, with the reason recorded where a
reader meets the feature.

**Still to build:** the rest of the view stratum (`top`, `pmap`, `pwdx`, `w`), the machine-wide
statistics, `watch`, and the `sysctl` fork below. The signalling stratum is no longer on this list;
see the ruling below. Each of the four remaining view programs is blocked on something real rather
than on effort, and notes/process-view.md names what: `pmap` on extending `ENUMERATE` to `Aspace`,
which is calef's open decision; `top` on per-thread CPU accounting that does not exist; `pwdx` and
`w` on a process display name this system does not have. The package
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
| **read the process namespace** | `ps`, `top`, `pgrep`, `pmap`, `pwdx`, `w` | `ps` and `pgrep` **built**; the other four each blocked on something named above |
| **signal a process** (control, not view) | `kill`, `pkill`, `skill`, `snice` | **mostly abolished 2026-08-17**: a domain names, never acts, and a tid is not a capability. Killing stays with whoever holds the child's region |
| **machine-wide statistics**, no process namespace | `free`, `uptime`, `vmstat`, `slabtop`, `tload` | **a different capability entirely**, and none exists |
| **write kernel tunables** | `sysctl` | no design, and see the fork below |
| **none of the above** | `watch` | nearly free: `line_editor` and the compositor already exist |

Build in that order. `ps` first, because it is a snapshot of the domain and needs no clock and no
accounting, so it is the whole capability argument with none of the scheduler work. Then the rest of
the view stratum, then signalling, then statistics, then `sysctl`, then `watch` whenever.

**`pgrep` came second rather than `top`, and the order turned out to matter more than expected.** It
needs nothing that does not exist, so it was the cheapest way to find out whether the capability
argument survives being *filtered* rather than merely listed. It did, and it also surfaced the
boundary limitation above, which is a fact about every future program that wants an operand and not a
fact about `pgrep`.

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

## The demonstration was `pgrep` beside `pkill`, and the ruling took `pkill` away

**A domain names its members and does not act on them** (calef, 2026-08-17). That answers the
signalling stratum before it was built, and it mostly abolishes it.

The reason is one the ABI already made and nobody had read back: **a survey returns a tid, which is
a name and not a capability**, and there is no path from a tid to authority over that thread. `Tcb`
has no `DESTROY`; killing a live child is `Untyped::DESTROY` on the region it was built from (§24's
forcible `^C`), held by whoever spawned it. So `pkill` cannot be assembled out of a view, and making
a domain confer control would be the one place this system copied the thing it exists to refuse,
which is Unix turning `kill(pid)` from a name into an action.

What replaces the demonstration: **`caps pgrep` prints a scope, and there is no `caps pkill` to print
beside it**, because the program does not exist here. That is a weaker side-by-side and a stronger
claim, and the write-up has to make the trade explicit rather than quietly dropping a promised
comparison. Killing stays with the shell that spawned the thing, which already holds the region.

**Done, 2026-08-17.** `pgrep` is built and the trade is stated in three places a reader might arrive
at: `crates/pgrep`'s module docs, `user/src/pgrep.rs`'s, and notes/process-view.md's own section. The
claim is also asserted rather than argued: the kernel test filters a domain down to its corpse and
then shows that the same capability which named the tid is refused the reap.

The cost, stated plainly: `procps` gets ported without its signalling stratum, and a reader who
expects `kill` to be a program will not find one.

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
