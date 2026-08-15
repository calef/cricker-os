# 74. What cadence do the audits run on?

**Status: PROPOSED.** (raised 2026-08-04; waiting on calef. It is the last thing gating milestones 92
and 93, whose index name was settled the same day.)

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
