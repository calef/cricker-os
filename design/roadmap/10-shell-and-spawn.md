# 10. A shell at EL0, and processes spawned on command

**Status: BUILT.**

Backfilled 2026-08-03 from history (milestone 76). Built in `61ed8c2` (2026-07-14), the rung the
original table called "proof the whole stack works," and the commit claims exactly that: four
processes and the channels between them, an input driver owning UART RX, the shell, a console
server, and a worker spawned on demand through a process service. "Everything the user sees is a
conversation between processes, and the kernel is a message router that touches none of it."

The revised row (`491f23d`) reads "A process server, and a shell that spawns binaries," which
absorbed the original milestone 9 ("Processes: spawn, exit, wait") when the ladder was rewritten
around §10; the spawn half of that plan lives here, and teardown beyond `exit` was deliberately
left for later (revocation is milestone 13's subject and reaping a whole process is milestone
26's).

The shell itself was three commands (`help`, `echo`, `run <n>`). Interactive niceties began the
next day (`3f0de79` boots straight to a shell, `588c206` echoes keystrokes), and everything the
shell has since become is milestones 31, 47, and 67's story.
