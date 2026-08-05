# 106. A wait that ends on either the interrupt or the deadline

**Status: NOT-STARTED.** Raised 2026-08-04 from `notes/net.md:307`, where milestone 30's network
lane recorded the cost of not having one. It is a kernel-surface addition, so it is **a design fork
for Chris before it is a task**, and it is the same fork DECISIONS §51 already records.

**Gate: DECISION.** A timed wait is a kernel-surface addition and DECISIONS §51 already records the
fork with three candidate shapes. The block adds the fourth consumer and asks for the decision to
be made against all of them at once, and warns against settling it by accident.

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

**What it costs.** A deadline in the blocked state means the scheduler carries a timer wheel or an
ordered deadline list, which is scheduler work the kernel does not do today. That is real, and it is
the honest counterweight to four consumers wanting it.

## Scope note

**Milestone 51 is BUILT and this fork is explicitly tracked outside it.** 51's block says the
timed-wait fork "is separable and should be decided on its own, since it serves more than this
milestone", and 51's `date` client is a one-shot synchroniser rather than a polling service for
exactly this reason: "adding a sleep syscall to get a real one would settle that fork by accident."
Do not settle it by accident here either.

**The consumers, so the decision is made against all of them at once**: net_stack's retransmit
window (this block), `thread::sleep` in the std PAL (a yield-spin today), `Endpoint::RECV`'s
no-timeout limitation (the kernel complains about it twice), and the shell's `^C` watch (milestone
103). A shape that serves one and not the others is the wrong shape.
