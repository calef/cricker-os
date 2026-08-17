# What a timed wait costs

*(Written 2026-08-17, for milestone 106's fork. This note prices a design and does not build one:
nothing here adds a syscall, and the fork between milestone 51's three candidate shapes is calef's
and stays open.)*

Milestone 106's block says a deadline in the blocked state "means the scheduler carries a timer
wheel or an ordered deadline list, which is scheduler work the kernel does not do today". That
sentence is the only cost estimate five consumers have ever been weighed against, and every clause
of it turns out to be either wrong or beside the point. This note replaces it with numbers.

Name: **provisional.** `timed-wait` takes the phrase the tree already uses in five places ("there
is no timed wait anywhere in the kernel", roadmap 51, roadmap 106, notes/clock.md, notes/ntp.md,
notes/pipes.md) rather than inventing a second one; the roadmap file beside it is
`106-deadline-wait.md`, and the two words are the same idea from the caller's side and the
scheduler's. calef has not ruled on it.

## The price list

Every number below was taken from this tree between 13:00 and 13:20 on 2026-08-17. What each was
measured on, and its error bar, is in its own section.

| what | today | with a deadline | how it was measured |
|---|---|---|---|
| bytes per thread | `size_of::<Thread>()` = **744**, in a 4096-byte page | **752**, in the same page | compile-time probe, both ISAs |
| bytes of BSS | **0** (the pool is gone; TCBs are page-resident) | **0** | `sched.rs:81`, milestone 19c.2 |
| the idle tick, per core | ~491 instructions aarch64, ~400 riscv64 | **+30** (aarch64), **+31** (riscv64) | `llvm-objdump`, debug build, executed path |
| the idle tick, as a fraction of a core | n/a | **under 3 parts per million** | 30 instructions x 100 Hz against a 1 GHz core; less on a faster one |
| per-tick comparisons when nothing is due | n/a | **1**, identically for all three data structures | host model, 100,000 ticks |
| interrupt-stack chain | 4352 B aarch64, 4160 B riscv64, of 16384 | **3888 B / 4208 B** | `script/stack-depth-check`, prototype wired in |
| a context switch is reachable from the interrupt stack | no | **still no**, both ISAs | the same gate, gating mode |
| threads blocked at once, suite peak | **97** of 128 (mean 37, live peak 102) | unchanged | `#[cfg(test)]` census, 31,371 samples, full suite |
| one second of the current yield-spin | **a whole core-second, 10^5 to 10^6 syscalls** | ~12,400 instructions | derived from `bench/baseline-*.txt` and notes/benchmarks.md |

The two numbers that decide anything are the last two, and neither is a data structure.

## 1. A per-thread deadline is free, and the block's premise is stale

The brief for this lane said "`MAX_THREADS` is 128 and the thread pool is a static BSS array". That
was true at milestone 14 phase B.2 and **stopped being true at 19c.2**: the static pool is gone, and
every `Thread` now lives at the start of one page drawn from `kmem` or from a user process's own
untyped (`sched.rs:80-93`, notes/tcb.md). The table is `slots::Table<TcbPtr, 128>`, about 2 KiB of
pointers.

So the question "what does a per-thread deadline field cost in bytes" has an answer nobody has to
weigh: **`size_of::<Thread>()` is 744 bytes in a 4096-byte page.** Measured, not reasoned, on both
ISAs, with a compile-time probe (`let _: [u8; 1] = [0u8; size_of::<Thread>()];` and read the error).
Adding `deadline: u64` makes it **752**. There are 3352 bytes of slack in that page before and 3344
after.

- **It changes no size class**, because the size class is a page and the page is paid by whoever owns
  the thread.
- **It changes no cache behaviour worth naming.** A `u64` appended to a struct already spanning 12
  cache lines lands in the twelfth, which the expiry walk touches and nothing else does.
- **It costs no BSS at all**, which is the number the stale premise would have made 1 KiB.

This is the same for all three of milestone 51's shapes. A `SYS_SLEEP`, a timer object and a deadline
on `RECV` all have to record, somewhere reachable from the tick, when this thread's wait ends; the
TCB is the cheapest place in all three cases and it is free in all three cases.

## 2. The timer wheel is the wrong question

The block offers "a timer wheel or an ordered deadline list" as if those were the options. At
`MAX_THREADS = 128` and `TICK_HZ = 100` there is a third that beats both, and the reason is not
asymptotic.

Three candidates, modelled on the host over 200,000 ticks (2,000 seconds of kernel time) with a
deterministic operation count as the currency and wall clock beside it:

- **A. No structure at all.** A per-thread deadline word, plus one cached `earliest`. The tick
  compares `now` against `earliest`; only when that fires does anything walk the table.
- **B. A sorted deadline list**, intrusive and doubly linked, ordered by deadline. The head is the
  minimum, so the tick is one compare; insert walks to its position.
- **C. A hashed timer wheel**, 256 slots at one tick each (2.56 s of range) plus an overflow list
  rehomed at each wrap.

Comparisons per tick, averaged over the pass. `occ` is how many threads hold a deadline; `churn` is
the chance per tick that an armed wait is cancelled before it fires, which is the case that actually
dominates (an ACK arrives and the retransmit timer is thrown away):

| occ | churn | A/no-structure | B/sorted list | C/wheel |
|---|---|---|---|---|
| 0 | - | **1.00** | **1.00** | **1.00** |
| 1 | 90% | **1.95** | 2.90 | 5.50 |
| 4 | 90% | **4.99** | 16.39 | 19.01 |
| 16 | 90% | **18.06** | 162.21 | 73.02 |
| 64 | 90% | **75.33** | 2179.20 | 289.04 |
| 128 | 90% | 156.48 | 8435.52 | **577.04** |
| 16 | 10% | **6.76** | 31.41 | 9.34 |
| 128 | 10% | 149.69 | 1701.46 | **67.65** |

Three things fall out, and the first is the one that collapses part of the fork:

**The idle tick is one comparison, and it is the same one comparison for all three.** Every shape can
cache the earliest deadline in a word; the tick loads it, compares, and returns. That is the number
paid on every tick on every core whether or not anyone is waiting, and it does not distinguish
between a wheel, a list, and no structure at all. Measured over 100,000 ticks at occupancy zero: 1.000
comparisons and **0.000 writes** for A and B, 1.004 for C (the wrap check). So "the scheduler carries
a timer wheel" is not a cost the fork has to weigh: **whatever is chosen, the always-paid cost is
identical.**

**The sorted list is the worst option at every occupancy above one**, which is the opposite of what
the block's "or an ordered deadline list" implies. Its tick is cheap and its *insert* is O(k), and
inserts outnumber expiries by the churn rate. At occ=128 it costs 8,435 comparisons per tick where
scanning costs 156.

**Scanning wins until about 64 threads hold deadlines simultaneously**, and the wheel wins past that.
The crossover is where it is because a scan is O(live threads) *per expiry event* while a wheel is
O(1) per insert and per expiry; at a handful of deadline holders the scan's constant factor is
smaller than the wheel's bookkeeping. The five known consumers (net_stack's retransmit window,
`thread::sleep`, `Endpoint::RECV`'s no-timeout limitation, milestone 103's `^C` watch, milestone
106's `Irq::WAIT`) are on the order of one deadline each.

**The honest error bars.** These are *modelled* operation counts, not machine instructions: the host
model counts comparisons and pointer writes at the points a kernel implementation would perform them,
which is load-independent and reproducible but is a model. Wall-clock ns/tick was recorded beside them
and is not reported as a number, because the machine was not quiet: another lane's `script/test` and
`script/lint` were running throughout and load average sat between 9 and 25. Two runs at different
loads produced the same **ordering** at every row, which is what the table is being read for.

## 3. What the timer interrupt path costs, and what a deadline scan adds to it

This is the number that matters most, because it is paid on every tick on every core whether or not
anyone is waiting.

**Today's path**, static instruction counts of the Rust half in the debug build the icount tripwire
measures (`llvm-objdump -d`, whole functions, excluding the assembly vector's frame save):

| aarch64 | | riscv64 | |
|---|---|---|---|
| `exception_dispatch` | 76 | `riscv_trap_dispatch` | 69 |
| `exception_body` | 127 | `riscv_trap_body` | 238 |
| `handle_irq` | 79 | | |
| `gic::acknowledge` | 31 | | |
| `gic::end_of_interrupt` | 29 | | |
| `timer::tick` | 31 | `timer::tick` | 30 |
| `timer::rearm` | 72 | | |
| `sched::on_tick` | 9 | `sched::on_tick` | 14 |
| `canary::check` | 17 | `canary::check` | 19 |
| `preempt_if_needed` | 12 | `preempt_if_needed` | 18 |
| `take_need_resched` | 8 | `take_need_resched` | 12 |
| **total** | **491** | **total** | **400** |

**What the deadline check adds.** A prototype was wired into `on_tick` (read the counter, compare
against a cached `EARLIEST_DEADLINE`, walk only if it fired) and the two builds' disassembly of
`on_tick` compared instruction by instruction on the **executed** path, which is exact rather than
static:

- aarch64: `on_tick` goes 9 -> 25 executed instructions, plus two non-inlined callees the debug build
  keeps as real calls (`timer::now` 6, `Atomic<u64>::load` 8). **+30 instructions per tick per core.**
- riscv64: the same shape, `on_tick` 14 -> 30, callees 6 and 9. **+31.**

Parity is not a coincidence: the added work is one counter read, one relaxed load and one compare on
both, and the ISAs differ only in how the debug build spills.

**So "nothing when the list is empty" is nearly true and is now proved rather than asserted.** It is
not literally nothing: it is 30 instructions, at 100 Hz, per core. At 400 ticks per second across four
cores that is 12,000 instructions per second, which against a 1 GHz core is **under three parts per
million** and less on a faster one. In a release build the same code collapses to roughly four instructions (an `mrs`, a load, a
compare, a not-taken branch), and the debug number is the one reported because the debug build is what
this tree gates.

**The error bar, stated plainly.** The 491 and 400 totals are *static whole-function* counts and
therefore over-count: they include arms of `exception_body` and `handle_irq` that a timer tick does
not take. The **+30 / +31** figures do not have that problem, because they were read off the executed
path of one function. So the ratio "+30 on ~491" is a lower bound on the percentage; the percentage is
somewhere above 6% of the Rust half of the tick handler and the absolute number is exact. Nothing was
measured under icount, because the icount tripwire's own note records ±5% codegen drift between
binaries (notes/benchmarks.md), which swamps a 30-instruction change: the disassembly is the finer
instrument here, not the coarser one.

## 4. The interrupt-stack proof holds, and the reason it holds was already in the tree

Milestone 124 gave each core an interrupt stack and gates one rule in CI: **nothing that runs on an
interrupt stack may context-switch away from it.** `script/stack-depth-check` proves no context switch
is *reachable* from the interrupt-stack entry point on either ISA. A deadline expiring in the timer
handler wants to wake a thread, and this was the question most likely to be the real cost.

**It is not the real cost, and the reason is that the kernel already does exactly this.**
`handle_irq` calls `sched::irq_notify` for every routed device interrupt
(`arch/aarch64/exceptions.rs`, in `exception_body`, on the interrupt stack), and `irq_notify` takes
`SCHED`, signals the endpoint, and calls `wake_load_aware`, which moves a `Blocked` thread to `Ready`
and pushes it onto a run queue. **A wake is an enqueue, not a switch.** The switch is already deferred:
`exception_body` returns `true`, and `exception_dispatch` runs `sched::preempt_if_needed` one frame
out, back on the interrupted thread's own stack. A deadline expiry is the same operation on the same
stack through the same function.

Measured rather than argued. A prototype `expire_deadlines` was wired into `on_tick`, walking the
table under `SCHED` and calling the existing `wake`, and the gate was run:

```
    aarch64: deepest thread-stack chain 13568 of 24576 bytes, 11008 spare; interrupt stack 3888 of
             16384, no context switch reachable from it; 1802 functions reachable, none over the
             guard page
    riscv64: deepest thread-stack chain 13232 of 24576 bytes, 11344 spare; interrupt stack 4208 of
             16384, no context switch reachable from it; 1796 functions reachable, none over the
             guard page
```

Against the same gate on the unmodified tree (4352 aarch64, 4160 riscv64 on the interrupt stack): the
budget moves by **-464 bytes on aarch64 and +48 on riscv64**, and the aarch64 number going *down* is
the tell that this is codegen churn rather than the scan. The deepest interrupt-stack chain on either
ISA runs through `watchdog_tick` -> `check_test_ceiling` -> `dump_threads` and a panic, which is a
test-build failure path and has nothing to do with deadlines. The suite's own runtime measurement
agrees: **the interrupt stacks' high-water is 1088 of 16384 bytes, 6%**, on all four cores.

**What it does cost, and this is the finding worth carrying to the fork.** A deadline expiry has to
take `SCHED`, and `SCHED` is the whole-machine lock. `irq_notify` establishes that this is *allowed*
from interrupt context (`IrqSafeMutex` masks interrupts for as long as it is held, so the interrupted
code on this core cannot have been holding it), but a device interrupt is rare and a timer tick is
not: four cores ticking at 100 Hz all reaching for `SCHED` is contention that nothing pays today.
**The cached `earliest` is what keeps that off the common path**, and it is therefore load-bearing
rather than an optimization: without it, every tick on every core takes the whole-machine lock. That
is the one place where "add a deadline" genuinely does touch scheduler structure, and it is a word,
not a wheel.

## 5. What the current spinning costs, so the comparison is not against zero

Five consumers spin today. The cleanest to price is the `std` PAL's `thread::sleep`
(`patches/std-nife/overlay/std/src/sys/thread/nife.rs`), which is exactly:

```rust
let deadline = rt::now().saturating_add(ticks);
while rt::now() < deadline {
    rt::yield_now();
}
```

One `SYS_YIELD` per iteration, as fast as the core will go, for the whole sleep. `net_stack`'s
`wait_for_nic` has the same shape over a retransmit window, and its own note is honest about it:
"yielding across a retransmit window spins a hart until the timer is due".

From the tree's own gated icount baselines (`bench/baseline-aarch64.txt`, `bench/baseline-riscv64.txt`;
under `-icount shift=0` one guest instruction is 1 ns of virtual time, `CNTFRQ_EL0` is 62.5 MHz so one
counter tick is 16 instructions, and riscv's `timebase-frequency` is 10 MHz so one `rdtime` tick is
100):

| primitive | aarch64 | riscv64 |
|---|---|---|
| `null_syscall`, one `svc`/`ecall` in and out | 324 instructions | 361 |
| `ctx_switch`, one EL0 yield round trip | 9,368 instructions | 9,745 |

A spinner alone on its hart pays near the `null_syscall` end (the run queue is empty, `schedule` takes
its no-switch exit); one sharing a hart pays the `ctx_switch` end. So one second of spinning is
**between 10^5 and a few times 10^6 syscalls, and 100% of a hart for the whole second**, and the tree
has the real-hardware figures too: ~27 ns per `null_syscall` and ~112 ns per EL0 yield round trip,
release build on HVF (notes/benchmarks.md).

The same second, waited on a deadline: one blocking syscall, one switch out, 100 ticks at 30
instructions, one wake, one switch back. About **12,400 instructions**, and the hart is in `wfi` for
the rest.

**The ratio is at least 10^5 to 1**, and it grows with core clock rather than shrinking: a core-second
is 10^9 instructions at 1 GHz and 3 x 10^9 at 3 GHz, while the 12,400 does not move. That is the
number the fork is actually about, and it is five orders of magnitude larger than any difference
between the three shapes or the three data structures.

**Error bars.** This is *derived* from measured primitives rather than measured end to end: the icount
figures are the tree's own gated baselines and the ns figures are its own HVF medians, but no run was
made that counted `SYS_YIELD` calls during a real retransmit backoff. The direction is not in doubt
(the spin is a hot loop at 100% of a hart, which `notes/net.md` and `testing.rs` both already record),
and the magnitude is uncertain by about one order of magnitude, which is where the "10^5 to 10^7"
range comes from rather than a single number.

## Where the three shapes actually differ

Sections 1 through 4 are common to all three of milestone 51's candidates: the same TCB word, the same
cached minimum, the same one-comparison tick, the same wake from the interrupt stack. Where they part
company is not the scheduler at all.

**A `SYS_SLEEP` and a timer object need nothing else.** The thread blocks with a deadline and no
counterparty; the expiry wakes it; it returns.

**A deadline on `Endpoint::RECV`/`CALL` needs two things the other two do not**, and both are concrete:

1. **A targeted unlink from an endpoint's wait queue.** `crates/intrusive`'s `Fifo` is **singly
   linked**: one `next` per node, `head`/`tail`/`len`, and its whole API is `push_back`, `pop_front`,
   `is_empty`, `len`. `ipc::Endpoint` adds only `drain_waiters`, which drains *all* of them. Removing
   one specific waiter is a new method, O(queue length) from the head, on a crate that carries
   machine-checked proofs of its one-queue invariant, so the proofs move with it. Bounded by
   `MAX_THREADS`, paid only on an actual expiry, and the census in section 6 says a queue can really
   hold most of the table.
2. **Nothing else.** The half that looks harder is already built. `wake_handshake`'s undelivered-wake
   gate refuses a wake with `wait_on.is_some()` and nothing delivered (boot 8), so a timeout looks
   like exactly the wake the gate exists to stop; but `Handshake::abort()` already passes the gate for
   this reason, and `sched::set_ipc_aborted` + `wake` is already the pair that revocation uses on a
   drained waiter (`sched.rs`, the `doomed_eps` walk). A timeout is that pair with a different reason.

And one hazard shape 3 carries that is worth naming before it is discovered, because it leaves the
machine the moment a program is written against it. **A `CALL` that times out leaves a live `Reply`
capability naming a thread that is no longer waiting.** `ipc_reply` already refuses to deliver to a
thread not parked as `WaitRole::Reply`, and its comment says why in exactly these terms ("a stale
reply whose CALL was aborted long ago, its caller re-parked elsewhere"), so the dangerous case is
covered. The residual is narrower: `Object::Reply(tid)` carries a Tid and **no per-call nonce**, so a
caller that times out and then issues a *second* `CALL` is Reply-parked again, and the first call's
stale reply would satisfy the second. This is latent today (revocation can produce the same
situation) and a deadline on `CALL` would make it reachable on purpose. It wants a lane of its own,
whichever shape wins.

## 6. How many threads are actually blocked at once

`MAX_THREADS` is 128 and the asymptotics in section 2 turn over between 64 and 128, so the occupancy
is worth knowing rather than assuming. A `#[cfg(test)]` census was sampled from inside `schedule()`'s
decision (which already holds `SCHED` and already has the table), every 64th call, over the whole
aarch64 suite:

```
106-pricing: blocked-census samples=31371 mean_x100=3695 max=97 live_max=102
```

**Mean 36.95 threads blocked, peak 97, against a live peak of 102 of 128.** So this suite genuinely
runs near the ceiling, and "128 is theoretical" would have been wrong.

Two caveats that matter for how the number is used:

- **Blocked is not deadline-holding.** These are threads parked on endpoints, overwhelmingly with no
  timed wait in sight. The deadline-list occupancy is bounded above by this and in practice would be
  the number of consumers actually in a timed wait, which is single digits. Section 2's table should
  be read at `occ` = 1 to 16, not at 97.
- **97 is what a targeted unlink walks past.** The one place the blocked count is the *right* number
  is shape 3's `Fifo::remove`, which walks an endpoint's wait queue from the head. This says that
  queue can be long, so the walk is O(97) rather than O(3) in the worst case the suite reaches.

## BUGS

- **No end-to-end measurement of the spin exists.** Section 5 is derived from the tree's gated icount
  baselines and its HVF medians rather than from a run that counted `SYS_YIELD` calls during a real
  retransmit backoff. The order of magnitude is uncertain by about ten; the conclusion is not.
- **Section 2 is a host model, not a kernel measurement.** It counts the comparisons and pointer
  writes a kernel implementation would perform, at the points it would perform them. It does not
  model cache behaviour, and its wall-clock column was taken on a machine running two other lanes'
  gates (load average 9 to 25) and is therefore not reported. Two runs at different loads agreed on
  the ordering of every row.
- **The 491 and 400 instruction totals in section 3 are static whole-function counts** and include
  arms a timer tick does not take, so they over-count. The +30 / +31 deltas do not: those are executed
  paths of one function. Treat the ratio as a lower bound and the delta as exact.
- **No release-build number is reported.** Everything here is the debug build, because that is what
  this tree gates and what its icount baselines measure. The release figures would be smaller and
  nothing here turns on them.
- **The prototype was thrown away.** The `deadline` field, the cached `EARLIEST_DEADLINE`, the
  `expire_deadlines` walk and the census were built to obtain the numbers above and are not in the
  tree. Reproducing them means writing them again; the shapes are described precisely enough here
  that this is a morning's work, and shipping them would have settled the fork by accident, which
  milestone 51's block and milestone 106's both warn against.
- **Nothing here says which shape to choose.** That is the point: this note exists to make the fork
  decidable, and the decision is calef's.

## A miscitation found on the way

Milestone 106's block, milestone 103's block and `notes/pipes.md` all cite the timed-wait fork as
**"DECISIONS §51"**. `design/decisions/51-sink-protocol.md` is the sink protocol; the fork is in
**`design/roadmap/51-wall-clock-time.md`**, a milestone block rather than a decisions section. This is
exactly the failure `CLAUDE.md` warns about ("`script/decisions --check` verifies that a cited `§N`
resolves to *some* section, never that it resolves to the right one, so a well-formed wrong citation
is invisible to it") and the collision `MEMORY.md` records between `§N` and milestone N as two
numbering schemes. Only 106's block is corrected here, because that is this lane's file;
`design/roadmap/103-interrupt-watch-stops-spinning.md:50` and `notes/pipes.md:656` still carry it and
belong to whoever owns them next. `design/decisions/43-clock-authority.md:107` gets it right, saying
"the milestone block's fork".
