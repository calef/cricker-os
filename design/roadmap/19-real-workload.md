# 19. Run a real workload

**Status: BUILT.**

**In brief.** A native-ABI workload first; Linux-compat or VM hosting later

**Why it matters.** **the "runs real workloads" half** of the thesis. **Built:** granular verbs and userspace init (19d), init as the real boot path (19d.2c), dedicated binaries delivered as a crickerfs archive with a shared `user_rt` runtime (19f.1-6), the native ABI written down (19e/Decision 2, notes/abi.md, DECISIONS §15), and the first real workload, a CoreMark-derived compute program spawned against that ABI (19e). design/init-and-granular-spawn.md

**Deliverable.** The "runs real workloads" half of §14: a real, unverified program running in
confined userspace on the verified core. A **native-ABI** workload first (the leanest thing that
proves the point), with a Linux-compat personality or VM hosting as later, larger options.

**Why.** The thesis is not "a verified kernel" but "a verified kernel *that runs real workloads*."
This is the milestone that makes the second half true, and it is what a demonstrator ultimately shows.

**The sub-decision it carries.** What counts as the first "real workload," and by which ABI. Native
first keeps the kernel pure and the surface small. A Linux-compat personality (Starnix / gVisor /
WSL1 shape, a userspace server translating syscalls) is how a demonstrator eventually reaches
existing software, and it is where the parked competitor ambition would begin. VM hosting (seL4's
route) needs the EL2 work in design/driver-domains.md. Decide the first target before writing
compat code, so it stays scoped.
