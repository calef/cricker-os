# `date`: printing the time, and refusing to make one up

The command that makes the wall clock visible to a person. Milestone 51's last small piece, and the
first thing in the tree that puts [the clock service](clock.md) and [the calendar
crate](calendar.md) in the same process. The program is `user/src/date.rs`; the guest tests are
`kernel::user::date_tests`.

It is a hundred lines and most of them are comments, which is the point: the design decided
everything interesting before the program existed, so what is left is a page read, an add, and a
call into a crate that was proved on the host.

## What it prints

`a0` picks one of `calendar`'s five formats. Zero is `Human`, so a spawn that passes no arguments
at all gets what a person typing `date` wants.

| `a0` | `Format` | output |
|---|---|---|
| 0 | `Human` | `Thu 2026-07-30 12:34:56 UTC` |
| 1 | `Rfc3339` | `2026-07-30T12:34:56Z` |
| 2 | `Date` | `2026-07-30` |
| 3 | `Time` | `12:34:56` |
| 4 | `Unix` | `1785414896` |

`a1` is a UTC offset in **minutes**, signed. `a1 = 330` prints `Thu 2026-07-30 18:04:56 +05:30`.
That is a fixed offset and not a time zone: `-480` is Pacific Standard Time for part of the year and
wrong for the rest, and notes/calendar.md records why the IANA database is a data-distribution
problem rather than a calendar one.

`a2`, when non-zero, adds a second line naming where the time came from:

```text
date: clock source: rtc, generation 1
```

There is no argv on this ABI; a program gets three words at `_start` (notes/abi.md), so the
"arguments" are registers. The shell spawns `date` with all three zero, which is the default a
person typing `date` wants; a manifest that could express the selectors needs positional arity
(notes/program-manifest.md), and that is deferred.

## The provenance line, and why it is worth a line

`clock_proto::state` has four values and they are not a boolean. `rtc` means the machine read its
hardware clock at startup; `set` means somebody holding the page read/write wrote the offset;
`synced` means the service accepted a proposal it bounded. "A human told me" and "an external source
I checked" are different claims, and the difference is what a caller weighing a certificate expiry
actually wants (DECISIONS §43).

The generation counts publishes, so a reader can see whether the clock has been stepped under it
without comparing timestamps and guessing. `rtc, generation 1` becoming `synced, generation 2` is a
clock that was corrected once since boot.

No `date` on a Unix can print this, because Unix has one clock and no notion of who last touched it.

## It reads. It does not set. And that is a property of its wiring.

DECISIONS §43 splits the clock into three authorities that are three *different objects*: read is a
**read-only mapping** of the clock page, set is the **same page mapped read/write**, propose is an
**endpoint**. `date` holds the first.

So there is no `date -s`, and its absence is not a `TODO`. There is no flag `date` could be passed
and no method it could call, because the authority it lacks is a page permission rather than a
check somewhere in the file. A program that set the clock would be a different binary holding a
different capability; it is not built, it would be about thirty lines, and it would be recognisable
by its wiring rather than by its argument parsing.

Unix's `date` is one binary that reads for everyone and sets for root, which is the conflation the
capability model gets to decline. Worth saying plainly because it is the milestone's whole claim
reduced to a command anyone can type.

## The unknown clock, which is the part that is easy to get wrong

`clock_proto::state::UNKNOWN` is a real state and it is the **default**: a frame nobody has
published to is zero, and zero reads as unknown. A machine with no RTC, or one whose RTC read a time
the clock service did not believe, leaves its readers holding exactly that.

`std`'s `SystemTime::now()` has no error channel, so its only loud refusal is a **panic**. `date`
has an error channel, namely its output, so it **reads the state word before it reads the offset**
and prints a sentence:

```text
date: the time is unknown: the machine has no clock it believes
date: the time is unknown: this process holds no clock capability
```

Two causes, two sentences, because they call for different fixes: the machine never learned the
time, or nobody granted this process a clock. This is DECISIONS §42's no-silent-degradation rule
applied where a person can see it. The alternative, and the thing the milestone exists to remove, is
printing `Thu 1970-01-01 00:00:04 UTC` as though it were a fact.

**Finding out whether a clock was granted cannot involve reading the page.** A process granted no
clock has nothing mapped at `CLOCK_VA`, so a probe that read it would fault instead of answering. So
the probe invokes the capability slot with a method number no object type defines: an empty slot
answers `NoSuchSlot`, a real `Frame` answers `BadMethod`, and a refusal from an object is proof one
is there. The std PAL's `granted()` solves the same problem the same way; they are the same problem.

## What the tests prove, and one gap they close

Three, all arch-neutral, so both ISAs run literally the same ones (DECISIONS §19).

**`date_prints_the_wall_clock_it_was_granted`** is not "it printed something shaped like a date".
The kernel computes the wall clock itself, straight from the page's offset plus the ambient counter,
then requires `date`'s output to name the same instant: the `Unix` rendering compared as a number,
the `Rfc3339` one parsed back, and the `Human` one reduced to RFC 3339 (drop the weekday, close the
space before the offset) and parsed too. A wrong epoch, a nanoseconds-for-seconds confusion, an
offset applied twice, or a calendar that is simply wrong all fail that, and none of them fails a
regex.

**`an_unknown_clock_is_said_plainly_rather_than_printed_as_1970`** closes a gap DECISIONS §43 named
in its own "what this lane did not do": *"the unknown-clock path is not proven in the guest"*, on the
grounds that both QEMU boards always have a working RTC. That reasoning was about the *machine*, and
the thing under test is the *page*. A frame nobody has published to **is** the machine that does not
know what time it is, as far as every reader is concerned, so the test allocates one, grants it, and
requires the sentence. Both causes are covered: the blank page, and no capability at all.

Falsified before it was believed. Removing the state check makes `date` print
`Thu 1970-01-01 00:00:04 UTC`, and the test fails with exactly that string in the diff, which is the
old lie caught in the act.

**`date_reports_where_the_time_came_from`** pins the provenance line across a step: `rtc,
generation 1` before a proposal, `synced, generation 2` after one the service accepted. The propose
endpoint is an authority `date` does not hold, which is the point: the provenance follows the page
rather than the process.

## BUGS

Named here rather than in a tracker, next to the feature.

- **One-second resolution, and the page read is not atomic with the counter sample.** The seqlock
  makes the *page* read consistent; the counter is read just after it. A clock stepped between the
  two is printed as the new time, which is right, but the two lines of an `a2` run can straddle a
  step and disagree. Nothing here needs better.
- **The prompt spawns it with the defaults only.** Milestone 47 gave `date` a `Prog` entry and a
  manifest, so typing `date` at the shell runs it (`Human`, UTC, no provenance line). The three
  register selectors are not reachable from there: `ArgSpec` is `Required`/`Forbidden` with no
  position or arity, and growing it is deferred until a program wants both an argument and a file.
  See notes/program-manifest.md.
- **The clock is init's to endow, and the shell still holds none.** The interactive boot starts the
  clock service and hands *init* the page read-only, so `date` at the prompt prints a real time; but
  the grant comes from `Prog::Date`'s manifest rather than from the command line, because there is no
  token a person could type that designates a clock. `caps date` prints the row anyway (a preview
  showing only what the line designates would be off by one capability), and that is the honest shape
  rather than a gap: a shell that held the page could hand it to anything it spawns.
- **The clock service is parked in its startup announcement for the whole interactive boot.** It
  publishes the RTC reading and *then* announces, with a blocking send, so the page is right before
  anybody could read it; the boot spawns one thread whose only job is to take that message, which
  leaves the propose endpoint live. Nothing at the prompt proposes a time, so nothing exercises it,
  and the first `date` after a boot with a broken RTC would read `UNKNOWN` rather than block.
- **Nothing at the prompt can set or propose a time**, and `date -s` is still not a missing flag. The
  boot grants init `READ` on the frame, so there is no writable mapping anywhere on the path from the
  kernel to a spawned child. A setter would be a different program holding a different capability,
  and init would have to be handed one to hand on.
- **A fixed UTC offset is not a time zone.** See above, and notes/calendar.md.
- **No `strftime`.** Five named formats. A format-string interpreter is a second parser with runtime
  errors in a program that has no allocator, for combinations nothing here asks for.
