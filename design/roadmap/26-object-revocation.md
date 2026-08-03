# 26. Object revocation: tear a process back down

**Status: BUILT.**

**In brief.** Reclaim the TCBs, address spaces, and endpoints a process built, and the regions behind them, so a workload that comes and goes can leave. **Built:** region-ownership + generational staleness (no CDT), `Untyped::SPLIT`/`DESTROY`, generational region slots (retires the 256-lifetime cap), endpoints (safe subset). Extends §13 from frames to objects; DECISIONS §16, notes/object-revocation.md

**Why it matters.** **the teardown half of "run real workloads":** a process can be reaped, not just built
