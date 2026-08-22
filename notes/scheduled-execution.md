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

## The archive it holds is narrowed to the plan

The endowment above is what a scheduled *child* holds. What the **scheduler** holds is a different
question, and until 2026-08-18 the answer had one embarrassing entry in it: the whole initrd,
mapped read-only, which is every program in the tree.

That is wider than anything the document can ask for, and the plan is what makes the gap
measurable. `Registry::programs` is the complete set of programs a document will ever start, as a
bitmask, computable at registration and therefore **before the spawn site has to decide what to hand
over**. So the spawn site builds an archive of exactly that set with `nifefs::write_image`, and the
timetable is handed that instead of the initrd.

### Where the set is computed, and why not at the spawn site

The obvious shape is for the spawn site to register the document itself and read `programs()` off the
result. **The kernel cannot do that, and the reason is worth knowing before anyone tries again.**

`timetable::Registry` is a `[Row; MAX_ENTRIES]` and a `Row` carries an `Admission`, which carries a
kilobyte of `grant_plan::Endowment`. So `Registry::register` compiles to a **21632-byte stack
frame**, and the `grant_plan::plan` underneath it to a further 12048. In `user/src/timetable.rs`
those numbers are fine and stated: it is a process with a 32-page stack, and
`kernel/src/user/timetable_tests.rs` says why it maps 32 pages. In the **kernel** they are exactly
what `script/stack-frame-check` refuses, because a frame larger than the 4096-byte guard page can
move `sp` from inside a thread's stack to below the guard in one step, touching nothing in between,
so the guard never faults and the write lands in the neighbouring thread's stack. This tree measured
that on riscv64 on 2026-08-14, at 4088 bytes below the bottom of a 4096-byte guard; eight more bytes
would have produced no fault at all. See notes/stack-high-water.md.

Neither number is a bug in `timetable`. A plan you can print is a plan you have to hold, and holding
one entry's worth of authority costs a kilobyte because that is what the authority *is*. What it
means is that **the plan is computed where there is stack for it**, which is the host and the
program, and never in the kernel.

So the spawn site carries the program set as a written list, `PLANNED_PROGRAMS`, and the thing that
keeps it equal to the plan is a **host test** rather than a comment:
`the_archive_a_timetable_holds_is_measured_against_what_it_will_build` registers the same
`user/timetable.conf` against the same `timetable::SHIPPED_HELD` and asserts the plan is exactly that
set, by name. Editing the document without editing the list fails in milliseconds with no emulator,
and the program's own audit line then fails the cross-ISA test as well, so a wrong list goes red
twice.

That is rung two of AGENTS.md's ladder where the first draft of this reached for rung one, and the
demotion is forced rather than chosen: the rung-one version does not fit on a kernel stack.

**A process cannot narrow its own endowment**, so what `timetable` does instead is measure it.
`timetable::Audit` compares the names the archive carries against the plan's program set and prints
one of two sentences after the plan:

```text
timetable: the archive it holds carries exactly the 2 programs its plan names
timetable: the archive it holds carries 57 programs, 55 of them beyond its plan
```

The second one is what the shipped program printed before this landed, and keeping both is the
point rather than politeness: the width of an endowment should be a line on the console rather than
a fact only the spawn site knows. `kernel/src/user/timetable_tests.rs` asserts the first sentence
**and** asserts the second never appears, so a spawn site that quietly went back to handing over the
initrd fails a test rather than passing one it no longer earns.

What this does not do is narrow per *entry*: the residual is the union of the plan's programs, so a
document admitting three programs leaves each instance's loader able to name the other two's images.
`user/src/spawner.rs` has the narrower shape (one image, and "build me program X" cannot be asked),
and reaching it here needs a capability per entry rather than one per timetable. Recorded in `BUGS`.

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

- the plan names what an admitted `worker` and an admitted `budgeter --mem 4` will each hold, and
  says "and nothing else";
- `date` and `ps` are refused for want of a clock and a process view, in the plan, before anything
  runs;
- `budgeter` with no `--mem` and `wc` carry the **prompt's own refusal sentences**, unchanged, which
  is the check being the same check;
- the admitted entries fire, under supervision, and their answers arrive on the endpoint the plan
  said they would hold, `budgeter`'s included: its grant is nested inside its own instance's region
  (below) and reclaimed before the loop fires anything else;
- and the summary accounts for every child: `4 fires, 4 clean exits, 0 faults`, which is the reap
  working. A scheduler that leaked a region per fire would print the same fire count and then run out
  of budget instead of finishing.

**Nothing in that test asserts on time.** The fires are counted, never timed, and no wall clock is
compared to anything: a loaded host makes the test slower and cannot make it red
(notes/load-sensitive-assertions.md; milestone 62 spent a week putting that property back into this
tree and this is not the lane to take it out again).

## A backable `--mem` grant, and why it runs alone

Built 2026-08-22. `Held::mem_pages` was zero on the shipped scheduler, so an entry naming a memory
grant was `Unbacked::Memory` even though the process held a budget; `timetable::SHIPPED_HELD.mem_pages`
is now 4 and `timetable.conf`'s `at-boot budgeter --mem 4` is planned, backed, and fires.

Milestone 129's block said to split the grant out of the *instance's own region*, "so that a
single `Untyped::DESTROY` still reclaims both and a restart loop is not a leak". **The kernel
refuses precisely that.** `regions::destroy_outcome` answers `Refused` for any region with a live
child, `split_stays_within_budget_and_progresses`' sibling proof pins it, and
`sched::reap_supervised` passes the refusal straight back as `NotPermitted`. So a corpse whose
region carries a split grant is uncollectable through `reap` until the grant is destroyed first, by
its own separate capability.

The nesting is still the right shape, for a reason the block did not state. **A refused reap would
be the only thing in this system that pairs a death with a grant**, because a supervisor learns a
tid and nothing else: `supervision_proto::build_child` hands back a TCB capability, `abi::tcb` has
no method that reads a tid out of one, and `abi::fault`'s five-word message carries no
builder-chosen tag. `user/src/timetable.rs` does not lean on that ambiguous signal, though: it
sidesteps the need to interpret a refusal at all by making the pairing structural. `fire_with_grant`
and `collect_grant` are called back to back, with nothing else fired in between and everything
already outstanding drained first, so the very next death on the supervision endpoint cannot be
anyone else's and the grant is destroyed unconditionally rather than guessed at.

That is the decision the block left open, made: **at most one `--mem` instance may be outstanding
at a time.** A generation counter or a slot table could track more, but nothing here needs its
child's tid for any other reason, so that would be speculative machinery for a document that
schedules exactly one such entry. The cost lands on every *other* entry, not on `--mem` ones:
`_start` is fully blocked in one syscall for as long as the grant-bearing instance takes to die, so
an interval entry due during that window runs late rather than on schedule when the loop resumes
(never dropped: `next_after`'s ordinary skip-not-catch-up rule covers a wait outlasting more than
one period, same as any other stall). `timetable.conf`'s `at-boot budgeter --mem 4` fires before the
first `every 150ms` tick can even become due, so the cross-ISA test does not exercise that cost; a
document whose `--mem` entry shared the clock with a fast interval would.

## BUGS

- **The narrowing is to the plan, not to one image per entry.** The archive the scheduler holds now
  carries exactly the programs its document will build, and no more; what it does not do is give each
  entry its own image. So a compromise of the timetable reaches the *union* of the plan's programs
  rather than one of them. `user/src/spawner.rs` is the narrower shape and needs a capability per
  entry to reach here, which this tree does not have.

- **A `--mem` entry blocks everything else in the document while it runs.** See "A backable `--mem`
  grant, and why it runs alone" above: the exclusivity that makes the pairing sound also means the
  scheduler is fully unresponsive to its clock for as long as one grant-bearing instance takes to
  die. Fine for a document with one such entry that fires at boot; a document leaning on `--mem`
  entries competing with fast intervals would feel it.

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
