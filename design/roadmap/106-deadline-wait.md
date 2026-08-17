# 106. A wait that ends on either the interrupt or the deadline

**Status: NOT-STARTED.** Raised 2026-08-04 from `notes/net.md:307`, where milestone 30's network
lane recorded the cost of not having one. It is a kernel-surface addition, so it is **a design fork
for calef before it is a task**, and it is the same fork **milestone 51** already records.

**Gate: DECISION.** A timed wait is a kernel-surface addition and **milestone 51's block**
(`design/roadmap/51-wall-clock-time.md`, "The fork this exposes, which is bigger than the milestone")
already records the fork with three candidate shapes. The block adds the fourth consumer and asks for
the decision to be made against all of them at once, and warns against settling it by accident.

This block said "DECISIONS §51" until 2026-08-17, and that citation was wrong: §51 is the sink
protocol. The fork is a *milestone* 51 block, not a decisions section, and `script/decisions --check`
cannot see the difference because a well-formed wrong citation resolves. Two other places still carry
it (`design/roadmap/103-interrupt-watch-stops-spinning.md:50`, `notes/pipes.md:656`) and belong to
whoever owns those files next.

**Priced 2026-08-17, and the pricing is the gate's input rather than its answer.** See
[notes/timed-wait.md](../../notes/timed-wait.md) for the numbers, what each was measured on and its
error bars. The fork is still calef's and this lane deliberately does not recommend a shape.

**The status deliberately does not move, and `NOT-STARTED` is the right word.** The pricing lane added
a syscall, an ABI constant and a line of kernel code: none. Nothing in the milestone is built, and
`PARTIAL` would claim otherwise. What changed is that the gate now has its input, so the block is
decidable without a conversation. The prototype that produced the numbers (a `deadline` word on
`Thread`, a cached earliest, an expiry walk in `on_tick`, and a blocked-thread census) was thrown away
on purpose: shipping it would have settled the fork by accident, which this block and milestone 51's
both warn against.

**The finding, and it was found the expensive way.** `std_net` hung on riscv64 under the four-hart
boot, watchdog-killed with every core idle and every thread blocked, while the identical test passed
on aarch64. The cause was not a dropped interrupt: instrumenting the PLIC at the hang showed no
source pending and the net source still enabled. The device was idle because both ends were waiting
on the same stalled timer. smoltcp drives retransmits, delayed ACKs and DNS timeouts from a clock
that only advances when `poll` is called; the old server loop blocked on the NIC interrupt between
polls, so a dropped segment left net_stack waiting for a peer that was waiting for a retransmit that
only a `poll` could fire.

**The fix, and the residual it leaves.** `wait_for_nic` (`user/src/net_stack.rs:331`) asks smoltcp
when it next needs to run. With no timer pending it blocks on the interrupt, 0% CPU until a frame
arrives. With a timer pending it does **not** block: it yields and re-polls, so the timer fires. The
note is plain about the price: "yielding across a retransmit window spins a hart until the timer is
due", bounded by the exchange and by a 15-second per-call backstop. Correct, and it burns a core
through every retransmit backoff, which is the interval a congested or lossy link spends most of its
time in.

**What the clean version needs.** A wait that returns on either the interrupt or a deadline, so the
server sleeps through the backoff instead of spinning. There is **no timed wait anywhere in the
kernel**: the syscall surface is `EXIT`, `YIELD`, `INVOKE` and `CAP_DELETE`, and `sched.rs` twice
calls out its own no-timeout limitation.

**This is milestone 51's fork, and it should be decided once.** Milestone 51 (wall-clock time) records three
candidate shapes and the argument between them:

| shape | the case for it | the case against |
|---|---|---|
| `SYS_SLEEP` | simplest | ambient, not capability-shaped |
| a timer object with `WAIT` | consistent with the model | the most machinery |
| a deadline on `Endpoint::RECV`/`CALL` | one addition fixes sleep, the `RECV` no-timeout limitation, and the shell's `^C` poll | it changes a primitive rather than adding one |

51's block calls the third strongest, and the reason is the count of consumers rather than
elegance: **three problems, one addition**. This milestone adds a fourth, an `Irq::WAIT` with a
deadline, and milestone 103 (the shell's interrupt watch) is the consumer that turns the third
column's "the shell's `^C` poll" from a footnote into an owner.

## What it costs, measured

**The sentence this block used to carry was hand-waving, and it was wrong in three places.** It said:
"A deadline in the blocked state means the scheduler carries a timer wheel or an ordered deadline
list, which is scheduler work the kernel does not do today. That is real, and it is the honest
counterweight to four consumers wanting it." Quoted rather than deleted, because it was the only cost
estimate the consumers had ever been weighed against and a reader should be able to see what the
numbers replaced. Priced 2026-08-17; the numbers and their error bars are in
[notes/timed-wait.md](../../notes/timed-wait.md), and the short form is:

- **The per-thread word is free.** `size_of::<Thread>()` is **744 bytes in a 4096-byte page**
  (measured, both ISAs); a `deadline: u64` makes it 752. The static `MAX_THREADS`-sized BSS pool the
  cost argument assumed **has not existed since milestone 19c.2**: TCBs are page-resident, so this
  costs zero bytes of BSS and no size class moves.
- **The always-paid cost is one comparison, and it is the same for every candidate structure.** Any
  shape can cache the earliest deadline in a word, so the tick loads, compares and returns. Measured
  over 100,000 idle ticks: **1.000 comparisons and 0.000 writes** for a scan and for a sorted list,
  1.004 for a wheel. "The scheduler carries a timer wheel" is therefore **not a cost the fork has to
  weigh**: whichever is chosen, the tick pays the same.
- **The ordered deadline list is the worst of the three above one waiter**, which is the opposite of
  what this block implied. Its tick is cheap and its *insert* is O(k), and inserts outnumber expiries.
  At 128 holders it costs 8,435 comparisons per tick where a plain scan costs 156. **Scanning wins
  outright until about 64 threads hold deadlines at once**, and the five known consumers hold about
  one each.
- **The tick handler grows by 30 instructions on aarch64 and 31 on riscv64**, per core, debug build,
  read off the executed path of `on_tick` in two disassemblies. Against ~491 (aarch64) and ~400
  (riscv64) instructions on the Rust half of the timer-IRQ path, and **under three parts per million
  of a core** at 100 Hz.
- **Milestone 124's proof holds, measured, on both ISAs.** A prototype expiry was wired into `on_tick`
  and `script/stack-depth-check` still reports "no context switch reachable from it"; the
  interrupt-stack budget moves by -464 bytes on aarch64 and +48 on riscv64 against 16384, and the
  measured runtime high-water is 1088. The reason is structural and already in the tree:
  `handle_irq` already calls `sched::irq_notify` from the interrupt stack, and that already wakes a
  blocked thread onto a run queue. **A wake is an enqueue, not a switch**, and the switch is already
  deferred to `preempt_if_needed` one frame out. This was the question most likely to be the real
  cost and it is not.
- **The one place a deadline genuinely touches scheduler structure is `SCHED`.** An expiry has to take
  the whole-machine lock, and four cores ticking at 100 Hz all reaching for it is contention nothing
  pays today. The cached `earliest` is what keeps that off the common path, so it is load-bearing
  rather than an optimization. That is a word, not a wheel.
- **And the counterweight runs the other way.** One second of today's yield-spin is **100% of a hart
  and 10^5 to a few times 10^6 syscalls**; the same second on a deadline is about **12,400
  instructions** with the hart in `wfi`. The ratio is **at least 10^5 to 1** and grows with core clock.
  That is five orders of magnitude larger than any difference between the three shapes.

**What the pricing found that this block did not ask about.** Sections 1 to 4 of the note are common
to all three shapes; where they differ is not the scheduler:

- A `SYS_SLEEP` and a timer object need nothing beyond the above.
- **A deadline on `Endpoint::RECV`/`CALL` needs exactly one thing more**: a targeted unlink from an
  endpoint's wait queue. `crates/intrusive`'s `Fifo` is **singly linked** and its API is `push_back`,
  `pop_front`, `is_empty`, `len`; `ipc::Endpoint` adds only `drain_waiters`, which drains all of them.
  Removing one waiter is a new O(queue length) method on a crate carrying machine-checked proofs, so
  the proofs move with it. The census says that queue can hold **97 of 128 threads** at the suite's
  peak, so the walk is not always short.
- **The half that looked harder is already built.** `wake_handshake`'s undelivered-wake gate (boot 8)
  would refuse a timeout wake, and `Handshake::abort()` already passes that gate for precisely this
  reason: `set_ipc_aborted` + `wake` is the pair revocation already uses on a drained waiter. A
  timeout is that pair with a different reason.
- **One hazard to decide rather than discover.** A `CALL` that times out leaves a live `Reply`
  capability naming a thread that is no longer waiting. `ipc_reply` already refuses to deliver to a
  thread not parked as `WaitRole::Reply`, and its comment names this exact case, so the dangerous
  version is covered. The residual: `Object::Reply(tid)` carries a Tid and **no per-call nonce**, so a
  caller that times out and then issues a second `CALL` could be satisfied by the first call's stale
  reply. Latent today (revocation produces the same shape) and reachable on purpose with a deadline on
  `CALL`. It wants a lane of its own whichever shape wins.

## Scope note

**Milestone 51 is BUILT and this fork is explicitly tracked outside it.** 51's block says the
timed-wait fork "is separable and should be decided on its own, since it serves more than this
milestone", and 51's `date` client is a one-shot synchroniser rather than a polling service for
exactly this reason: "adding a sleep syscall to get a real one would settle that fork by accident."
Do not settle it by accident here either.

**The consumers, so the decision is made against all of them at once**: net_stack's retransmit
window (this block), `thread::sleep` in the std PAL (a yield-spin today), `Endpoint::RECV`'s
no-timeout limitation (the kernel complains about it twice), the shell's `^C` watch (milestone
103), and a **liveness watch over a supervision domain** (milestone 23's hung-component residual,
added 2026-08-17, notes/hung-component.md). A shape that serves one and not the others is the wrong
shape.

**The fifth consumer discriminates between the candidates rather than only adding a vote**, which is
why it is worth more than a line. A liveness watch does not want to *sleep*: it wants to be told
"nothing arrived by time T" while staying able to receive a report, so a bare `SYS_SLEEP` leaves it
spinning on the very question it exists to answer, and it wants the answer on a `RECV`, because the
domain it watches reports progress to it. Its own note also recommends bounding a hang in **progress
rather than duration** (milestone 62's argument, one level out), which softens what an inaccurate
deadline costs it: a bad number delays a diagnosis rather than convicting a healthy component of being
slow. So it is the cheapest of the five to serve and has the most specific requirement.

**Two citations corrected 2026-08-17**, by milestone 23's hung-component lane, which needed this fork
and so read it closely. This block twice attributed the three candidate shapes to "DECISIONS §51", and
§51 is *the sink protocol*; they live in **milestone 51's** roadmap block, in its rejected-alternatives
list. `script/decisions --check` proves that a cited §N resolves to *some* section and never that it
resolves to the right one, so a well-formed wrong citation is invisible to it. §N and milestone N are
colliding schemes, and this is what the collision costs.
