# The backup-server ladder (53 to 55), and why it is the right deliverable

calef's goal, 2026-07-30: **the board should replace the drive hanging off his router as the Time
Machine target.** These three milestones are that goal decomposed honestly, and they are worth doing
for a reason beyond utility.

**It is a real workload with a real user.** Every other thing this project measures is a benchmark or
a test. This one gets used by people who did not write it, which changes what "works" means.

**And the stakes are exactly right, which matters more than they would be if they were higher**
(calef, 2026-07-30). This is **not** his durable backup: **Borg handles offsite**, and the board's job
is protecting against short-term mistakes. So losing the whole thing costs the ability to undo a bad
afternoon, not any data. That is the ideal shape for a demonstrator target: **genuine use, tolerable
failure.** Putting an experimental capability microkernel in front of someone's only copy would be
reckless; putting it in front of their convenience layer is a real test with a bounded downside, and
the entry should not pretend otherwise to sound weightier.

**It still exercises crash consistency for real.** §34's RedoxFS conditions get tested against actual
power loss on actual hardware rather than a QEMU crash image, and correctness is still the goal. The
honest correction is only to the consequence of failure, not to the standard.

**And it is the best security claim the thesis can make, because backup servers hold everything.** On
a Linux box, Samba runs with broad authority over the machine. Here the file-serving component would
hold **one directory capability and one network endpoint and nothing else**, so a compromise reaches
the backup share and stops: not by policy, not by a hardening guide, but because no capability naming
anything else was ever given to it. That is worth more on a backup server than on almost any other
workload.
