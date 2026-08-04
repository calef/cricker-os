# 8. Process model / syscall ABI: DEFERRED to a hard decision point

**Status: SUPERSEDED BY 10.** (resolved at milestone 7; the deferral itself was the decision and it held.)

**RESOLVED at milestone 7. See §10.** Kept here as written, because the deferral was the
decision and it held.

Unix-like (fds, fork/exec) versus capability-based (seL4/Fuchsia-shaped) is genuinely
undecided, on purpose. Milestones 1-6 do not touch the syscall boundary, and every
kernel builds them roughly the same way, so the deferral is free until it isn't.

**Milestone 7 (user mode) is a hard decision point.** When we get there we stop, look at
what we've built, and choose deliberately. This deferral is a plan, not a drift. If we
find ourselves hacking in a syscall without having had that conversation, the plan has
failed.

It didn't. We stopped and had the conversation, over the course of a day, before a line of
milestone 7 existed.
