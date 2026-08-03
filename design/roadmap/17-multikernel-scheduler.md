# 17. Multikernel-leaning scheduler (research, optional)

**Status: OPTIONAL.**

**In brief.** Partition the shared thread table and endpoints

**Why it matters.** optional; not on the thesis path

**Deliverable.** Partition or replicate the two structures still shared under one `SCHED` lock (the
thread table and the endpoint array), toward per-core state with message-passing where a lock now
sits.

**Why.** The SMP work (§11) already went most of the way: per-CPU run queues, per-CPU current and
held-rank, cross-core placement by inbox-plus-SGI with no shared run-queue lock. What remains shared
is the thread table and endpoints. Barrelfish's multikernel (treat the machine as a distributed
system, message-passing between cores) is the honest research answer for NUMA and P/E asymmetry.
This is a direction, not a commitment: keeping the one lock is a perfectly honest choice at the
current scale, and worth saying so rather than feeling the machine is owed a message-passing thread
table.
