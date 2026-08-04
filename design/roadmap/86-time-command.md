# 86. `time`: the shell times a command

**Status: BUILT.** Raised 2026-08-03, prompted by timing a forty-minute proof run on the
host and noticing this OS has no way to ask the same question. It shipped the same day it was
written up (`crates/swish`, `user/src/swish.rs`, `kernel/src/user/time_tests.rs`,
notes/time-command.md), reading the shell's clock as the block below leans, and proven at a real
prompt on both ISAs through `script/shell-check`. **The status row was left behind by that merge and
is corrected here**, which is the drift milestone 93 exists to catch: the work landed, the roadmap
kept saying it had not started. One question the lane raised against its own block, whether a
duration needs a clock capability at all, is open and does not unbuild anything.

`time wc log.txt` runs the command exactly as typed and reports how long it took. The name is the
standard term and stays: `time`, `nice` and `env` are the prefix-word idiom `caps <command>` already
cites as its precedent, and this is the second prefix word, so the grammar it needs is proven in the
shell rather than new. The tail is a real command through the same `RunSpec`/`plan_against` path,
which keeps "what you time is what you run" true the same way `caps` keeps it for inspection.

**The one design question is whose clock it is, and the leaning is the shell's.** `date` established
the clock story: a read-only clock-page mapping, endowed, with an honest refusal when the holder has
none. If `time` reads the shell's own clock capability, a child that holds no clock at all can still
be timed, which is the Unix behaviour (the timed program does not know it is being timed and needs
nothing to permit it); timing is then something an observer does with its own authority, which is
the capability-model answer too, since the child's wall-clock duration is observable to anyone who
can watch it start and stop. The alternative, delegating a clock to the child, would make `time` a
grant and change what the child can do, which is a different tool. Decide it on the record in
`DECISIONS.md` when built; this block records the leaning and the reason.

What the number means is bounded by what the shell can see: wall clock between spawn and the exit
arriving on the supervision endpoint, on the clock page's resolution. Not CPU time, which is the
scheduler's knowledge and unqueried today; if that arrives later it is an extension of the same
command, not a rival. A worked `EXAMPLES` entry and the resolution caveat go in the man-page-shaped
docs, per the FreeBSD standard.

## Scope note

Timing a command that holds no clock is the whole point, so the milestone includes the case where
the *shell* holds no clock either, and the refusal should be `date`'s, worded for the prefix
position. Host tests cover the plan and the arithmetic; the QEMU test is one timed spawn asserting
the duration is positive and sane, not a latency benchmark, which is `bench`'s job.
