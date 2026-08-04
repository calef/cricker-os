# 38. Filesystem throughput, and the comparison (DECISIONS §34, condition 2; extends 21/25)

**Status: NOT-STARTED.**

**Gate: NONE.** The instrument and the method both exist: milestone 21's harness and milestone 25's
EL0-measured, matched-tier discipline. What is honestly comparable is a question the lane answers
with measurements rather than one that waits on anybody.

**In brief.** Sequential and random read/write throughput through the confined FS server, against ext4 on Linux and APFS on macOS at a matched virtualization tier, the way milestone 25 did the primitives. Requires deciding what is honestly comparable: our reads are device-latency-dominated (`fs_read` is ~204 us/read under HVF, and `relay_rtt` puts the isolation tax a thousand times below that), so the interesting question is whether the userspace-server architecture costs throughput once the device dominates, which is a claim a microkernel skeptic will press

**Why it matters.** **"primary filesystem" invites a comparison we cannot currently make.** We have the per-request numbers and the isolation tax, and no MB/s figure at all. Milestone 21's rule is measure rather than argue, and 25 already established that the honest way to do this is EL0-measured against real systems rather than self-reported. This is where the "userspace servers are too slow" objection gets an answer or a concession
