# 129. Scheduled execution: a cron whose every entry is a grant

**Status: PARTIAL.** Minted 2026-08-15 at calef's request, from the observation that the
customer path wants it: a backup server owes housekeeping on a schedule (snapshot thinning,
scrub passes, log rotation) even though the Mac initiates the backups themselves. Nothing else
on the roadmap runs anything on a schedule.

**Gate: NONE.** It stays `NONE` rather than moving to `DECISION`, which is worth a sentence because
the temptation was there. Every ingredient existed as predicted: §43's clock authority and
`clock_proto`, the spawn machinery and program manifests (milestone 31's grant expressions), and
supervision (§40) for what happens when a scheduled child dies. No new syscall surface was needed.
The remaining pieces that need nobody are the `--mem` grant and the per-entry image (the archive
endowment was narrowed to the plan on 2026-08-18); only a **runtime** registration protocol is
calef's, and a gate reading `DECISION` would say the milestone is blocked when most of it is not.

What the build did find is that the *absence* of a syscall shapes the whole program: see "The
finding" below.

## Built: the interval scheduler, 2026-08-18

This block's own "honest first deliverable" test, met: an interval scheduler running supervised
children on both ISAs, with the plan printed before anything fires.

- **`crates/timetable`** holds the decision and no IO, so the four answers registration can give are
  host-tested in milliseconds and the fire arithmetic is Kani-reached (five harnesses; the sixth law,
  phase preservation, is host-tested and notes/scheduled-execution.md says why).
- **`user/src/timetable.rs`** holds a budget, the monotonic counter and the loader. Its complete
  authority is four capabilities: an output endpoint, an untyped budget, a child report endpoint and
  a supervision endpoint. No clock, no directory, no console, no network.
- **`user/timetable.conf`** is the document, in `mdns_config`'s shape and carrying its recorded
  compiled-in limitation.
- **`kernel/src/user/timetable_tests.rs`**, one module for both ISAs.

**The demonstration is the refusals**, which is milestone 126's shape rather than a coincidence.
Registration has **four** answers where a crontab has one, and the split that matters is between "the
line is wrong" (fixed by editing the entry, and it is the prompt's own refusal arriving unchanged)
and "this scheduler holds nothing to back it" (fixed by granting the scheduler more). The sharpest
case is `every 1s date`: a legal line that runs in any crontab on earth, because there the clock is
ambient, refused here **in writing, at registration, before the first tick**, because the wall clock
is a capability (§43) and this scheduler was granted none. `every 1s ps` is the same shape over
milestone 126's process view. Neither ever runs, and the cross-ISA test asserts both the refusal and
the never-running.

**A correction to this block's own sketch.** It says "the service holds one clock capability and
subscribes to §43's monotonic time". It holds no clock, and it cannot: **monotonic time is ambient
here** (the kernel opened the counter to EL0, so `user_rt::monotonic_nanos` needs no capability), and
the wall clock is the thing §43 made a capability. So an interval scheduler needs no clock authority
at all, and the reason a *scheduled entry* can be refused a clock is that the scheduler has none to
give. That is a better story than the sketch's and it was found by building rather than by arguing.

**The registration story, decided for the compile-time case and deliberately not for the runtime
one.** The document is `include_str!`d, so today the authority to register an entry is the authority
to rebuild the image: the strongest possible answer and the least useful one. A runtime protocol is a
real fork (the boot endowment? the shell? a per-registrar endpoint whose entries can be no wider than
the registrar?) and settling it as a side effect of shipping a heartbeat would have been exactly the
accident AGENTS.md warns about.

## The finding: this is milestone 106's fifth consumer, and the first whose purpose is a deadline

**There is no timed wait anywhere in this kernel**, so a program whose entire reason for existing is
to act at a time can only yield and re-read the counter. A running timetable therefore costs a core's
worth of yields, and it reaps its dead children **lazily**, only when the budget cannot back another
instance, because reaping means blocking on the supervision endpoint and a process has exactly one
wait point.

Milestone 106's block counts four consumers of that fork (`net_stack`'s retransmit window,
`thread::sleep`, `RECV`'s no-timeout limitation, the shell's `^C` poll). This is the fifth, and the
first where a deadline is not a fallback path but the whole program. The shape of the fix is already
in the code: `Registry::next_deadline` computes exactly the instant a timed wait would block until,
computed today although nothing can use it, so the loop changes by one line rather than being
restructured.

## Built: the archive endowment narrowed to the plan, 2026-08-18

The scheduler was handed the whole initrd, which is every program in the tree behind a document that
names one. It is now handed an archive holding **exactly the programs its plan will build**, and the
argument is the one this milestone is made of: `Registry::programs` is complete and known before the
first tick, so the endowment can be narrowed to it. `kernel/src/user/timetable_tests.rs` builds the
sub-archive with `nifefs::write_image`.

**The set is computed on the host, not in the kernel, and that is a constraint rather than a
preference.** `Registry::register` carries a 21632-byte frame (a `[Row; MAX_ENTRIES]` where a `Row`
holds a kilobyte of `Endowment`) and `grant_plan::plan` a further 12048, against a 4096-byte guard
page; `script/stack-frame-check` refuses both, correctly. So the spawn site carries a written list
and a host test asserts it equals the plan, by name, from the same document and the same `Held`. A
document edited without the list fails on the host in milliseconds, and fails the cross-ISA test
again through the program's own audit line.

**A process cannot narrow its own endowment, so it measures it.** `timetable::Audit` compares the
names the archive carries against the plan's program set and prints the answer next to the plan, in
one of two sentences. The test asserts the narrow one and asserts the wide one never appears, so a
spawn site that went back to handing over the initrd fails rather than passes.

```text
timetable: the archive it holds carries exactly the 1 program its plan names   <- now
timetable: the archive it holds carries 57 programs, 56 of them beyond its plan <- before
```

## Still to build

- **A `--mem` grant a scheduled entry can actually be backed with.** `Held::mem_pages` is zero today,
  so an entry naming one is refused although the process holds a budget.

  **This block's own sketch for backing it is wrong and is corrected here** (2026-08-18, found by
  reading the kernel rather than by failing). It said to split the grant out of the *instance's own
  region* so a single `DESTROY` reclaims both. `regions::destroy_outcome` answers `Refused` for any
  region with a live child, and `sched::reap_supervised` passes that back, so a corpse whose region
  carries a grant cannot be collected at all until the grant is destroyed first.

  The nesting survives the correction for a better reason: **a refused reap is the only thing that
  pairs a death with a grant.** A supervisor learns a tid and nothing else, because nothing hands a
  builder its child's tid (`build_child` returns a TCB capability and `abi::tcb` reads none out). So
  what is left is a decision rather than wiring: how many `--mem` instances may be outstanding at
  once, since the refusal identifies one. Serialising them is the small answer.
  `crates/timetable` supports the planning and refusal already, and its host tests cover both. Needs
  nobody.
- **One image per entry rather than one archive per timetable.** The narrowing above is to the plan's
  *union*, so a document admitting three programs leaves each instance's loader able to name the
  other two's images. `user/src/spawner.rs` has the narrow shape (one image, and "build me program X"
  cannot be asked of it) and reaching it here needs a capability per entry. Needs nobody, and it is
  smaller than it looks now that the sub-archive exists.
- **A runtime registration protocol, and who holds the right to use it.** calef's, per above.
- **Calendar syntax, wall-clock entries, persistence.** Each its own later decision, per the scope
  note below, and none of them started.

**In brief.** Unix cron is a daemon that reads a text file and runs arbitrary commands as
ambient authority made periodic: whatever root's crontab says, happens, and the crontab is the
attack surface. The capability shape inverts it: a scheduler service holds a clock capability
and, per entry, exactly the grant expression that entry's program is endowed with, checked at
registration the way the shell checks a command line at the prompt (milestone 31). An entry
cannot name what its manifest does not; compromising the scheduler yields the entries' summed
endowments, not the system.

## The shape, sketched for whoever scopes it

- **An entry is a manifest plus a schedule.** The schedule vocabulary starts embarrassingly
  small: every N seconds, and at-boot. Calendar cron syntax (minute/hour/day fields, its DST
  ambiguities) is a later decision, not a default; the housekeeping the backup server needs is
  interval-shaped.
- **The service holds one clock capability** and subscribes to §43's monotonic time; wall-clock
  scheduling waits for a decision about what a wall-clock entry should do across an NTP step,
  which the era-pivot work (`ntp_proto`) already gives vocabulary for.
- **A fired entry is an ordinary spawn** through the existing verbs, supervised per §40: a
  scheduled child that dies is reaped like any other, and the entry's failure count is state the
  service reports rather than hides.
- **Registration is the security boundary.** Whoever can register an entry can make its grants
  periodic, nothing more. Who may register is itself a capability question the scoping should
  answer deliberately (the boot endowment? the shell? both?).

## Scope note

Sequenced by need, not dependency: nothing blocks starting it today, but its first real customer
is milestone 55's housekeeping, so scoping it before 54/55 take shape risks building the wrong
verbs. The honest first deliverable is the interval scheduler running a no-op heartbeat program
under supervision on both ISAs, with the registration story decided; calendar syntax, wall-clock
entries, and persistence of the entry table across reboot are each their own later decision.

## BUGS

- The name is a placeholder in the oldest tradition ("cron"), and the eventual program name is
  calef's like every other (AGENTS.md). This file deliberately does not propose one. **The 2026-08-18
  lane shipped `timetable` as a provisional name** for the crate, the program and the document, said
  so in every module header, and recorded what it refused (`cron`, `almanac`, `metronome`, and
  `scheduler`, which this tree already spends on `kernel/src/sched.rs`).
- Persistence is unaddressed: entries registered at runtime die with the boot. Fine for a
  heartbeat, wrong for a backup server's housekeeping; the persistence story probably belongs to
  whatever milestone gives services durable configuration at all, which does not exist yet.

- **The document is compiled in, not read from disk**, which is the limitation
  `user/src/mdns_responder.rs` records and has the same fix (a `FileSpec` grant plus an `fs_proto`
  open-and-read at startup, milestone 131). It is load-bearing here in a way it is not there, because
  where the document lives is also what answers "who may register".
