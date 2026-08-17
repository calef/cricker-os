# 17. Multikernel-leaning scheduler (research, optional)

**Status: OPTIONAL.**

**Gate: MILESTONE 88.** Stated in the block's own sequencing: 88 provides the first machine with
enough harts for the `smp_throughput` curve to bend. The other half of this gate was `MILESTONE 80`,
for the method, because a design that replaces the lock with messages wants its protocol born
loom-checked; 80 landed 2026-08-13 and `script/interleaving-check` is the method it left behind, so
that half is satisfied rather than dropped. (Recorded here rather than deleted silently: the gate
check fails a `MILESTONE` token naming a BUILT milestone, which is what surfaced this.)

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

**Sequencing, recorded 2026-08-03.** The inventory of what the lock actually protects, function by
function with a temperature classification, is in notes/sched-lock-inventory.md; its three
structural findings (the hot set is IPC; CSpace operations partition for free if anything does;
the §13 revocation sweeps are what partitioning makes expensive) are the shape any design here
starts from. This milestone is gated on evidence two others produce: **milestone 88** provides the
first machine with enough harts for the `smp_throughput` curve to bend (no machine this kernel has
run on exceeds ten), and **milestone 80** provides the method, because a design that replaces the
lock with messages wants its protocol born loom-checked. Until 88's curve shows the lock in the
data, the answer to this milestone is the one already written above: the one lock, on purpose.
