# 129. Scheduled execution: a cron whose every entry is a grant

**Status: NOT-STARTED.** Minted 2026-08-15 at calef's request, from the observation that the
customer path wants it: a backup server owes housekeeping on a schedule (snapshot thinning,
scrub passes, log rotation) even though the Mac initiates the backups themselves. Nothing else
on the roadmap runs anything on a schedule.

**Gate: NONE.** Every ingredient exists: §43's clock authority and `clock_proto`, the spawn
machinery and program manifests (milestone 31's grant expressions), and supervision (§40) for
what happens when a scheduled child dies. No new syscall surface is expected; if the design
finds it needs one, that is a fork to raise, not to build.

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
  calef's like every other (AGENTS.md). This file deliberately does not propose one.
- Persistence is unaddressed: entries registered at runtime die with the boot. Fine for a
  heartbeat, wrong for a backup server's housekeeping; the persistence story probably belongs to
  whatever milestone gives services durable configuration at all, which does not exist yet.
