# Scheduled execution: a cron whose every entry is a grant

Milestone 129. What `cron` becomes on a system with no ambient authority, why the interesting half
is what a schedule is *refused*, and the one kernel primitive whose absence shapes the whole
program.

The pieces: `crates/timetable` (the decision, host-tested and Kani-reached),
`user/src/timetable.rs` (the budget, the counter, the loader), `user/timetable.conf` (the
document), `kernel/src/user/timetable_tests.rs` (both ISAs). Every name in that list is
**provisional**; milestone 129's block declines to propose one and AGENTS.md says the eventual one
is calef's.

## What cron actually is, and what is wrong with it here

A crontab line is a command run as a user. That is the whole model, and it means the authority
behind a scheduled job is a property of *who owns the crontab* rather than of anything the line
says. Two consequences follow, and both are structural rather than accidents of implementation:

- **Nothing can be printed.** Ask "what will this entry be able to do?" and Unix has no answer
  narrower than "whatever that account can do". There is no tool that could print it, because there
  is no object that holds it.
- **The crontab is the attack surface.** Write access to one file is write access to everything the
  account reaches, at a time of your choosing, forever.

Neither is a criticism of cron, which is doing exactly what a Unix scheduler can do. It is the
observation that a capability system can do something else.

## The inversion

**An entry is a schedule plus a grant expression**, and the grant expression is the same text a
person would type at the prompt:

```text
every 150ms  worker 7
at-boot      worker 3
every 1s     date
```

At registration each command goes through `grant_plan::parse_run` and `grant_plan::plan`, which is
**the same function `swish` checks a prompt line with**. That is one call rather than a second
implementation, so "a scheduled entry is checked exactly as a typed one" is true in the code instead
of being a convention two places are trusted to keep. What comes out is a `grant_plan::Endowment`:
the complete authority the scheduled child will hold, computable before anything fires and therefore
printable.

So `timetable` prints its whole plan and *then* arms:

```text
timetable: the plan, before anything fires
  every 150ms   worker 7
    grants worker exactly:
      cap 0  endpoint  report its answer to this timetable
      arg      7
      and nothing else: no clock, no disk, no console, no network
  every 1s      date
    will not fire: this timetable holds no clock, so it cannot grant one
timetable: armed
```

The line `caps worker 7` prints at the prompt and the line this prints are the same decision, made
by the same code, one of them for a job that has not been scheduled yet.

## The four answers, and why three of them are the point

Milestone 126's `ps` made the case that the negative controls are the demonstration: three distinct
outcomes where `/proc` has one. Registration here has four where a crontab has one.

| answer | what it means | fixed by |
|---|---|---|
| `Admission::Fires` | planned, backed, armed | nothing to fix |
| `Admission::Refused` | the program's own manifest refuses the line, exactly as at the prompt | **editing the entry** |
| `Admission::Unbacked` | the line is legal and *this scheduler* holds nothing to back it | **granting the scheduler more** |
| `timetable::Error` | the document does not parse, with the line number | editing the document |

**The `Refused`/`Unbacked` split is the one worth keeping.** Collapsing them would tell a person to
edit a line that has nothing wrong with it. `budgeter` with no `--mem` is wrong wherever it is typed;
`date` is a perfectly good line that this particular scheduler cannot back, and the fix is a decision
somebody makes on purpose at the spawn site.

### The entry Unix cannot refuse

`every 1s date` is the example the shipped document carries deliberately. In a crontab it runs,
every time, because reading the clock on Unix is ambient. Here the wall clock is a capability
(DECISIONS §43, notes/clock.md): a read-only mapping of the clock page, endowed by whoever is doing
the spawning. `timetable` was granted none. So the entry is refused **in writing, at registration,
before the first tick**, rather than firing once a second and printing that it does not know what
time it is.

`every 1s ps` is the same shape one milestone later: a process view is a supervision endpoint with
`ENUMERATE` (milestone 126, notes/process-view.md), the timetable holds none to hand over, and a
scheduled `ps` is refused rather than shown an empty list.

Those two entries are the milestone in miniature. They are lines whose danger a Unix scheduler has no
vocabulary to discuss.

## `Held`: what the scheduler holds is a parameter, not an era

`timetable::Held` is `grant_plan::Holdings` one level out, and it exists for exactly the reason that
one does. "This scheduler holds nothing to back that" has to be a statement about a particular
process's cspace. The same document is four refusals in a scheduler granted nothing but a budget and
four running jobs in one granted a clock, a directory and a terminal, and neither the document nor
the program manifests can tell those two apart.

Every field on the shipped scheduler's `Held` is `false` or zero, and that is checkable against the
slot list in `user/src/timetable.rs`'s header:

- slot 0: its output endpoint (WRITE)
- slot 1: an untyped budget (WRITE)
- slot 2: the child report endpoint (WRITE|GRANT)
- slot 3: the supervision endpoint (READ|GRANT)

No clock, no directory, no console, no network, no device. **Widening it is an edit to `Held` and a
visible change in the printed plan**, which is the property worth having: a scheduler gets wider
because somebody decided it should, not because an entry talked it into it.

## Registration is the security boundary

Milestone 129's block puts it in one sentence: *whoever can register an entry can make its grants
periodic, nothing more.* Concretely, compromising `timetable` yields the union of what it holds,
which is a memory budget and two endpoints into its own children. It cannot read the clock, cannot
touch a disk, cannot open a socket, and cannot give any of those to a child, because there is no
ambient authority anywhere for a child to fall back on.

**Who may register is answered by where the document lives**, and for the first deliverable that is
`include_str!`: the document is compiled into the binary, exactly as `user/mdns_responder.conf` is
compiled into the responder and for the same recorded reason (reading a file needs a file capability
wired through the spawn; see notes/mdns.md and milestone 131). So today the authority to register is
the authority to rebuild the image, which is the strongest possible answer and also the least useful
one. A runtime registration protocol is a real decision with a real fork in it (the boot endowment?
the shell? a per-registrar endpoint whose entries can only be as wide as the registrar?) and the
honest thing was to ship the document and leave the fork visible rather than settle it by accident.

## The arithmetic, and the decision inside it

`timetable::next_after(prev, period, now)` answers the next fire, and it has three properties:

- **strictly after `now`**, so a polling loop cannot fire one occurrence twice;
- **congruent to `prev` modulo `period`**, so an entry that ran late comes back onto its original
  beat instead of inheriting every delay it ever suffered;
- **it skips rather than catching up.** A scheduler away for an hour fires a 10ms entry **once**, not
  360,000 times.

The third is a decision, and Vixie cron makes the same one. Catching up turns a stall into a
stampede: a housekeeping job that runs two hundred times back to back on a machine that was already
struggling is how a slow morning becomes an outage. The cost is real and stated rather than smoothed
over: occurrences are genuinely lost, and work that must not be skipped wants a durable queue rather
than a scheduler.

Five Kani harnesses hold those properties (`crates/timetable/src/proofs.rs`), and the reason it is
Kani rather than more host tests is that **every wrong answer here is quiet**. An off-by-one at a
period boundary fires twice in one polling pass and nothing complains; a lost phase drifts a schedule
a few nanoseconds an hour and surfaces as a beat nobody can explain a month later; a catch-up
implementation satisfies every property except the one that bounds it. None of those is reachable by
sampling, because the interesting inputs are the ones nobody thinks to type.

**Two things about that file are worth knowing before anyone edits `next_after`.**

The first is a measurement. The function reaches its answer by snapping `now` back onto the beat
(`now - (now - prev) % period`) rather than by counting the periods that went by
(`first + (gap / period + 1) * period`). Both are correct and the host tests do not tell them apart;
the counting form contains a 64-by-64 **multiplication**, which is an enormous thing to hand a SAT
solver, and CBMC was still grinding on one harness after ten minutes. The snapping form proves in
about a second. That is a three-orders-of-magnitude difference bought by removing one operator, and
it is the kind of thing the verification path notices and a test suite never would.

The second is a gap, stated because a reader would otherwise assume it is covered: **phase
preservation is the one law that is host-tested rather than machine-checked.** Every way of writing
the congruence needs a modulo of a *computed* value on top of the one inside `next_after`, and a
second 64-bit modulo is where CBMC stops finishing (the direct spelling, the cheaper one, and the
cheaper one bounded to `1 << 32` all failed to return). Shipping a harness bounded far enough down to
finish would read as proved while covering a range no schedule lives in, which is worse than shipping
none. `next_after_is_strictly_in_the_future_and_keeps_its_phase` samples the law up to `u64::MAX / 4`
instead, and the implementation's shape is the real defence: the phase is not computed and then
preserved, it is the only thing that expression can produce.

## The primitive that is missing, and what it costs

**There is no timed wait anywhere in this kernel.** No sleep, no timeout, no deadline: the syscall
surface is `EXIT`, `YIELD`, `INVOKE` and `CAP_DELETE`, and a process has exactly one blocking wait
point. So a program whose entire purpose is to act at a time can only **yield and re-read the
counter**, and a running timetable costs a core's worth of yields.

That is milestone 51's fork and milestone 106's gate, and this program is its **fifth consumer**. The
block counts four (`net_stack`'s retransmit window, `thread::sleep`, `RECV`'s no-timeout limitation,
the shell's `^C` poll); this is the first whose whole reason for existing is a deadline.

The shape of the fix is already in the code. `Registry::next_deadline` computes exactly the instant a
timed wait would block until, and it is computed today even though nothing can use it, so that when
the fork is decided the loop changes by one line rather than being restructured.

**The second thing the missing primitive costs is lazy reaping**, and it is worth naming because it
looks like a design choice and is not. Reaping a scheduled child means blocking on the supervision
endpoint; blocking means not watching the clock; one wait point means you cannot do both. So
`timetable` reaps only when the budget cannot back another instance, and the failure counts it
reports lag reality until then. A wait that returns on either a message or a deadline fixes this too,
and it is the same fork.

## What is built, and on what

`kernel/src/user/timetable_tests.rs`, one module for both ISAs (nothing in it is
architecture-specific, so the parity gate is met by literally the same test running twice). It spawns
the real program on the real `user/timetable.conf`, reads the plan it prints, then watches what
fires:

- the plan names what an admitted `worker` will hold, and says "and nothing else";
- `date` and `ps` are refused for want of a clock and a process view, in the plan, before anything
  runs;
- `budgeter` and `wc` carry the **prompt's own refusal sentences**, unchanged, which is the check
  being the same check;
- the admitted entries fire, under supervision, and their answers arrive on the endpoint the plan
  said they would hold;
- and the summary accounts for every child: `4 fires, 4 clean exits, 0 faults`, which is the reap
  working. A scheduler that leaked a region per fire would print the same fire count and then run out
  of budget instead of finishing.

**Nothing in that test asserts on time.** The fires are counted, never timed, and no wall clock is
compared to anything: a loaded host makes the test slower and cannot make it red
(notes/load-sensitive-assertions.md; milestone 62 spent a week putting that property back into this
tree and this is not the lane to take it out again).

## BUGS

- **The scheduler holds the whole initrd archive, which is wider than its plan.** The set of programs
  it will ever build is fixed at registration and printable (`Registry::programs`), but the
  *capability* it holds reaches any program in the archive. The narrower shape already exists in this
  tree: `user/src/spawner.rs` is handed one image and "build me program X" is not a thing that can be
  asked of it. Doing that here needs the spawn site to hand over one image per admitted entry, which
  needs a sub-archive built where the timetable is spawned; that is a milestone rather than a
  drive-by and it is proposed as one.

- **`--mem` entries are refused.** `Held::mem_pages` is zero on the shipped scheduler, so an entry
  naming a memory grant is `Unbacked::Memory` even though the process holds a budget. Backing one
  means splitting the grant out of the *instance's own region*, so that a single `Untyped::DESTROY`
  still reclaims both and a restart loop is not a leak. `crates/timetable` supports it and its host
  tests cover it; only the wiring in the program is missing.

- **Nothing is persistent.** Entries die with the boot, which is fine for a heartbeat and wrong for a
  backup server's housekeeping. Milestone 129's block records this and points at whatever milestone
  gives services durable configuration at all, which does not exist yet.

- **The document is compiled in, not read from disk**, which is also what decides who may register
  (see above). `mdns_responder` carries the same limitation for the same reason; milestone 131 is
  where the runtime-read shape lands, and nothing about the format, the parser, the line-numbered
  errors or the tests changes when it does.

- **The schedule vocabulary is two words.** `every <interval>` and `at-boot`, with `ms`, `s` and `m`.
  No calendar syntax, deliberately: what a `0 2 * * *` entry should do when the wall clock steps an
  hour is a question this system has vocabulary for (`ntp_proto`'s era pivot, notes/ntp.md) and no
  answer to yet, and a default drifted into is worse than a decision deferred. Milestone 129's block
  scopes it the same way.

- **`Unbacked::File` and `Unbacked::Directory` are unreached by any shipped entry**, because the
  shipped scheduler holds no directory and `grant_plan` refuses a file designation before this crate
  sees it. They are live, host-tested logic (`what_the_scheduler_holds_decides_what_it_can_schedule`
  plans a scheduler that holds a directory), and they will be reached the first time a scheduler is
  granted one. Recorded rather than removed, because deleting them would mean the check is silently
  absent the day somebody widens `Held`.

## See also

- [program-manifest.md](program-manifest.md) and [grant-expression.md](grant-expression.md): what
  the check at registration actually is, one and two levels down.
- [supervision.md](supervision.md): why a scheduled child that dies becomes a message, and what
  `Endpoint::REAP` needs.
- [clock.md](clock.md): why a scheduled `date` is refused, and why reading a *duration* needs no
  capability while reading the *time* does.
- [process-view.md](process-view.md): why a scheduled `ps` is refused.
- [load-sensitive-assertions.md](load-sensitive-assertions.md): why the test counts fires instead of
  timing them.
- [mdns.md](mdns.md): the configuration-document shape this copied, including its compiled-in
  limitation.
