# 74. Audits run on change, not on the calendar: events first, then a count

**Status: DECIDED.** calef, 2026-08-16: **event triggers first, a count-based trigger second, and
the calendar only as a backstop.** This adopts the recommendation below over the block's original
quarterly proposal, and it unblocks milestones 92 and 93.

## The numbers

**The count trigger fires at 15 milestones built since the last audit, or 8 new crates and
programs, whichever comes first.** Both are read from the same place the tripwire already reads.

**Corrected 2026-08-16, the same day, by milestone 92's lane**: this entry said three security
audits existed and there are **four**. `notes/security.md` (2026-07-15) predates the three counted
below, and `SECURITY.md` undercounts by one for the same reason. The interval arithmetic is
unaffected, because the two audits with countable milestone history are still the pair used, but a
decision that miscounts its own evidence should say so where the evidence is cited. The index in
`design/audit-reports/README.md` is now the authority, and it derives its counts from the tree
rather than from prose, which is what makes a third miscount catchable.

Neither number was picked in the abstract; they are the interval this project chose when nobody
was counting. Four security audits exist. The arch-and-assembly pass (notes/arch-audit.md) came
first. The shared-page time-of-check-to-time-of-use pass landed 2026-08-04 with **54 milestones
built**. The untrusted-counterparty-input pass landed 2026-08-15 with **71 built**, an interval of
**17 milestones**, and it was productive rather than early or late: it found the NVMe driver's
panic on two device-written completion fields (the reciprocal, one layer down, of the shared-page
pass's own finding 6) and cleared three crates that did not exist when the previous audit ran. 15
is that interval rounded to a number a person can hold, and it is deliberately slightly tighter
than the one instance we have.

**Counting components as well as milestones, because a milestone can be a note.** What changes an
attack surface is a new crate that reads bytes somebody else wrote. The 2026-08-15 audit chose its
own lens by asking what had landed since the last pass, which is a component-shaped question:
`nvme` (08-14), `mdns_proto` and `smb_proto` (08-15), `cred`/`ntlm` (08-04). Eight is the size of
that kind of batch.

## Why the event triggers stay primary

The count is a backstop for the events, not the other way around, and 2026-08-15 is the worked
example. `smb_proto` and `mdns_proto` are the first parsers in this tree whose input arrives from
the **network at runtime** rather than from firmware or a disk, which is 92's "a new component
holding device or network authority" trigger exactly. It fired within a day of the components
existing. A count would have reached the same place eventually, and eventually is the wrong word
for an attack surface.

## Why the calendar survives at all, and what its job actually is

Six weeks rather than a quarter, as the recommendation below says, but its purpose is the
opposite of what a reader assumes. It does not catch a busy period; the count does that, sooner.
It catches a **quiet** one: a tree that sits untouched for two months while the field's threat
model moves anyway, which no measure of this project's own change can see.

## The argument against, and why it did not win

Stated fairly below and still true: a count is a number somebody must pick and defend, and picking
it too tight produces audit fatigue, which is how a practice gets skipped. What answers it is that
the number is no longer a guess. It is one measured interval, rounded down, with a component
clause covering the case where milestones and surface disagree. If audits start feeling
mechanical, that is evidence to raise 15, and raising it is a one-line edit to a decision that
records why it was 15 in the first place.

**What.** Milestone 92 proposes quarterly security audits plus event triggers, and milestone 93
shares its index and tripwire for documentation sweeps. The tripwire compares the last audit's date
in `design/audit-reports/README.md` against the cadence and goes red when one is overdue. Red means
"run the audit", not "an automation ran it for you".

**The problem with a calendar on this project.** Milestone 1 was built 2026-07-12 and 54 milestones
were built by 2026-08-04, which is roughly three weeks. A quarterly interval on that velocity means
one audit per sixty-odd milestones, and the lens the last audit lacked (milestone 43's insight) will
be a lens on a system that no longer exists. The unit that matters here is **change, not time.**

**The recommendation, and it is a change to the block's proposal.** Make the **event triggers
primary** and the calendar a backstop:

- Trigger on what 92 already lists: a new syscall method, a new component holding device or network
  authority, a new dependency class (§46), or first boot on a new machine class.
- Add a **count-based** trigger, because it is as cheap to compute as a date and much better matched:
  N milestones built since the last audit, or N new components. The tripwire already reads a file; it
  can read a count from the same place.
- Keep a calendar backstop so a quiet period cannot go unaudited, but at **six weeks rather than a
  quarter** while velocity is this high, with the interval explicitly indexed to velocity and
  expected to lengthen when the tree settles.

**The argument against, stated fairly.** A count-based trigger is a number somebody has to pick and
then defend, and picking it wrong in the tight direction produces audit fatigue, which is how a
practice gets skipped, which is the exact failure 92 exists to prevent. Quarterly is boring,
defensible, and nobody argues with it. If the recommendation above looks like over-engineering, the
honest fallback is **quarterly plus the event triggers**, which is the block's own proposal and is
better than what exists today (nothing).

**Blocked.** Milestones 92 and 93 do not start until this is answered.
