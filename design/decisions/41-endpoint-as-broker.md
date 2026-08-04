# 41. The endpoint is the broker, and a device is revoked by taking it back (milestone 23)

**Built 2026-07-30.** Milestone 23 is the flagship the roadmap points at: every userspace component
is a swappable unit behind a stable contract, and an operator replaces one live, with a client that
does not notice. Concept note: notes/live-replacement.md.

The roadmap specified four steps and a latency ladder. Building it settled three things the block
left open, and contradicted the block on one. All four are here.

## 1. The stable name is the endpoint object, not a process in front of it

The block's step 3 says "clients hold a cap to a stable *broker* endpoint, not to the server; the
broker re-points on a swap", and its prose imagines a **forwarding process**. It does not need one,
and it should not have one.

§12's endpoint-only naming already gives the indirection: a client names an endpoint and never a
peer, and the kernel's rendezvous is anonymous in both directions. So the stable name a client holds
*is* the endpoint object, and a swap is a change in **who is parked in `RECV_CAP` on it**. Nothing
stands in the data path, so the steady-state cost of the mechanism is **zero**: the same
`call_reply` path, instruction for instruction, and the benchmark for the swap's steady state is
`call_reply` itself (bench/baseline-aarch64.txt; notes/benchmarks.md).

Two properties fall out that a forwarding broker would have had to reimplement:

- **The down window needs no buffer, because the kernel already is one.** A `CALL` that finds nobody
  receiving parks the caller on the endpoint's sender queue with its message and its one-shot Reply
  capability in `outgoing_cap`; the next server's `RECV_CAP` picks both up and answers a caller it
  was never wired to. Nothing is lost while the endpoint has no server, and nothing was added to the
  kernel to make that true.
- **The drain is the quiesce message travelling in band.** The operator's `OP_QUIESCE` goes to the
  *same endpoint being drained*, and the sender queue is FIFO, so by the time it arrives the
  incumbent has answered every request queued ahead of it. There is no quiescence protocol, no
  timeout, and no window the operator has to guess at.

What endpoint-only naming does **not** mean is "whoever holds the endpoint is the server". `SEND`
and `RECV` are gated by different rights on the same object, so a client holding `WRITE` cannot
receive. That refusal is a test (`a_client_of_the_stable_endpoint_cannot_become_its_server`), with
an attacker endowed with the honest client's exact capabilities.

## 2. Revoking a device means taking it back, not destroying it

Step 2 revokes the outgoing server's device capability so a device never has two owners. §13's
`Frame::REVOKE` was the obvious mechanism and it does the wrong thing here: it revokes **all**
derivatives of a page *including the invoker's own*, because it exists to make reclamation safe and
a page about to be freed must not stay reachable. A device page is never reclaimed, and the kernel
mints a `DeviceFrame` capability **once, at boot**, so a symmetric revoke would strand the UART for
the rest of the machine's life.

So `Frame::REVOKE` now dispatches on `Object::DeviceFrame` with **take-back** semantics: delete every
`DeviceFrame` capability naming the page from every cspace *except the invoker's*, and unmap it from
every address space *except the invoker's*. `GRANT`-gated, exactly as the frame case is, on the same
reasoning (you were trusted to lend it on, so you may take it back). Afterwards exactly one process
can reach the registers: the one that asked, which is then free to endow the replacement.

**This is the roadmap's own prediction cashed in** ("where the deferred CDT finally earns its
keep"), and it is one level of that tree and only one: the invoker is the root by construction, and
every other holder is a derivative. Revoking one *named* holder while sparing another still wants
the real derivation tree, and still is not built.

**Surface cost.** No new syscall, no new method number, no new object type, no new error: it is an
existing method answering on a second object type, where that object previously returned
`BadMethod`. Recorded here rather than only in code, per the project's rule.

**The asymmetry is deliberate and is the one thing to remember.** `Frame::REVOKE` on a frame takes
the page from everyone including you; on a device it takes it from everyone but you. Same verb, two
objects, two purposes: reclamation versus exclusive ownership transfer.

## 3. The block's step order does not survive contact, and the fix is smaller than it looks

The block says start the new server first, then revoke. Built that way it cannot work: revocation is
by *physical page*, so a revoke that ran after the replacement had been endowed with the device
would take the replacement's copy too, and nothing could hand one back.

What moves is the **endowment**, not the build. The replacement is laid out, endowed with everything
except the device, and left unconfigured (a thread that was never started is in nobody's queue, so
it cannot race the incumbent for requests); the registers are mapped into it on the far side of the
revoke. The down window is then four syscalls wide: revoke, map, configure, start.

Building it *is* what showed that the build has to stay first, for a second reason the design did
not anticipate: process construction is a few hundred syscalls, and when the build was moved after
the swap trigger the client got through its entire conversation on RISC-V before the operator was
ready. So the correct order is the roadmap's order for a reason the roadmap did not give.

## 4. The latency ladder: two rungs built, and the rule that governs them

The block's rule is *opt-in per channel, never the default*, and the reason is a number.

- **Rung 0, the default: the shared endpoint.** No process in the path, cost zero, buffering by the
  kernel's sender queue, decoupling limited to "the caller blocks until the replacement arrives".
- **Rung 1, opt-in: `broker`, a queue-server process.** A producer that cannot afford to block gets
  an immediate `ACCEPTED` while the backend is away; the broker holds the backlog in its own `.bss`
  (bounded, inside its own region, so a runaway producer hits `QUEUE_FULL` and never kernel memory)
  and drains it in order when a backend returns. Its control messages travel in band on its own
  front endpoint, for the same reason `OP_QUIESCE` does: a server blocks on one endpoint, and a
  second one would need the wait-any primitive §26.5 deliberately does not have.
- **Rung 2, durable (writes the backlog to storage, survives its own crash): not built**, and named
  so the ladder is not implied to be complete.

`broker_rtt` prices rung 1 against `call_reply`, the same client and backend with nothing in
between: **1.99x on aarch64, 2.00x on RISC-V, about 1.2 microseconds per request under HVF.** That
is what the rule protects. Paying it on every IPC would trade the project's measured round-trip
advantage for a feature used during swaps.

## What this does not yet demonstrate

Named rather than implied, because the block itself lists them as the general case and they are the
real engineering:

- **State handoff.** The component here is near-stateless by construction, which is what makes
  kill-and-replace sufficient. A filesystem server's open handles or a network stack's live
  connections need a serialise-old / absorb-new protocol, and there is none.
- **A manifest.** The operator's endowments are literals in its own source, not a declared list of
  capabilities a vendor's build could be wired from.
- **Dependency-aware orchestration.** One channel is swapped at a time, with no dependency graph and
  no quiescence protocol across components.
- **A hung component.** The whole sequence rests on the outgoing instance cooperating with
  `OP_QUIESCE`. A livelocked one would need the stronger right, which is §32's recorded watchdog
  case, still open.
