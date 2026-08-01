# Live component replacement

*Milestone 23, DECISIONS §41. The flagship the roadmap points at: a running component is replaced
under a client that is talking to it, and the client's stream is unbroken.*

## The shape

```text
                   ┌──── the stable name: one endpoint object, forever ────┐
   chatty ──CALL──►│                       SVC                              │◄─RECV_CAP── rust_swappable  (v1, Rust)
  (a client)       └────────────────────────────────────────────────────────┘◄─RECV_CAP── c_swappable (v2, C)
                                            ▲
                                            │ swapper, an unprivileged operator, changes
                                            │ WHICH of the two is parked in RECV_CAP
```

Five programs, all in `user/src/`, sharing one module (`swap.rs`) the way the supervision tree
shares `suptree.rs`:

| program | what it is | what it holds |
|---|---|---|
| `swapper` | the operator: builder, supervisor and verifier | a budget, one device capability, four endpoints |
| `rust_swappable` | the component, version 1 (Rust) | the service endpoint (READ), a report endpoint, a coordination channel, the device, a shared page |
| `c_swappable` | the component, version 2, whose answers are computed in **C** | identical |
| `chatty` | the client, the producer, and the attacker (three roles, one binary) | the service endpoint (**WRITE**, not READ) |
| `broker` | the queue broker, the ladder's opt-in rung | a front endpoint (READ), a back endpoint (WRITE) |

## Why there is no broker in the fast path

This is the thing to understand about the milestone, and it is a property the kernel already had.

**A client names an endpoint, never a peer** (DECISIONS §12, notes/ipc-naming.md). The rendezvous is
anonymous in both directions: a server that `RECV`s does not learn who sent, and a client that
`CALL`s does not learn who answered. So a component's identity is not merely hidden from its
clients, it is *not represented anywhere a client can reach*. Any program that speaks the protocol
and holds the right capabilities **is** the component.

That makes the stable name the endpoint object itself, and a swap a change in who is parked in
`RECV_CAP` on it. Two consequences, both of which a forwarding broker would have had to reimplement
at a cost:

1. **The kernel's sender queue is the buffer for the down window.** A `CALL` that finds nobody
   receiving parks the caller as a blocked sender, with its message in its mailbox and the one-shot
   `Reply` capability the kernel minted for it riding in `outgoing_cap`. Whenever the *next* server
   calls `RECV_CAP`, it takes both, and answers a caller it was never wired to. So while the
   endpoint has no server at all, requests are not lost, not refused, and not reordered; the caller
   is simply blocked, which is what a synchronous IPC caller already is.
2. **The drain is a message travelling in band.** The operator's `OP_QUIESCE` goes to *the endpoint
   being drained*, and the sender queue is FIFO, so by the time it arrives the incumbent has answered
   everything queued ahead of it. No quiescence handshake, no timeout, no window to guess at.

The cost of all this is zero: the steady state is `call_reply`, the same path a client and server
already use (notes/benchmarks.md).

What endpoint-only naming does **not** mean is "whoever holds the endpoint is the server". `SEND` and
`RECV` are gated by *different rights* on the same object, so the same endpoint handed out two ways
is a one-way pipe in whichever direction each holder was trusted with. `chatty`'s usurper role holds
the honest client's exact capabilities and tries to receive on the service endpoint; it gets
`NotPermitted`.

## The steps

```text
  1 BUILT    lay the replacement out, endow it, retype its TCB -- but do not configure or start it.
             A thread that was never started is in nobody's queue, so it cannot take a request the
             incumbent is still there to serve.
  2 DRAINED  CALL OP_QUIESCE on the service endpoint. FIFO does the waiting. The incumbent replies
             and stops receiving; requests from here on park on the sender queue.
  3 REVOKED  Frame::REVOKE the device capability. Gone from every holder but the operator.
  4 STARTED  map the registers into the replacement, CONFIGURE, START. It drains the parked
             requests. The down window ends: four syscalls wide.
  5 REAPED   the incumbent is told to read one device register, faults, and its death arrives on the
             supervision endpoint; Endpoint::REAP collects the corpse and returns its region.
```

**The roadmap put the revoke second and building the replacement first, and both halves of that are
right for reasons it did not give.** Revocation is by *physical page* (DECISIONS §13), so a revoke
that ran after the replacement had been endowed with the device would take the replacement's copy
too, and since the kernel mints a `DeviceFrame` once at boot, nothing could hand one back. What moves
to the far side of the revoke is the **endowment**, not the build. And the build has to stay first
for a second reason found by running it: process construction is a few hundred syscalls, and when the
build was moved after the swap trigger the client finished its entire conversation on RISC-V before
the operator was ready.

## Taking a device back

`Frame::REVOKE` on a `Frame` un-shares a page from everyone **including the caller**, because §13
exists to make *reclamation* safe and a page about to be returned to the allocator must not stay
reachable. On a `DeviceFrame` the same method means **take-back**: every other holder loses the
capability and the mapping, the invoker keeps its own.

The asymmetry is forced, not convenient. A device page is never reclaimed, and the kernel mints its
capability once, at boot, so a symmetric revoke would strand the UART for the rest of the machine's
life. Two objects, two purposes, one verb: reclamation versus exclusive ownership transfer.

This is the "deferred capability-derivation tree finally earning its keep" the roadmap predicted, and
it is one level of that tree: the invoker is the root by construction (it holds `GRANT` and it is the
one asking) and everyone else is a derivative. Revoking one *named* holder while sparing another
still wants the real tree, and still is not built.

## How "the client did not notice" is proven

The shape milestones 29, 33 and 36 used: two witnesses in two address spaces, an attacker with real
authority, and a control that must fail.

**Witness one, the client, in its own address space.** `chatty` calls sixty-four times in a plain
loop, holding one capability for its whole life. It never reconnects, never retries, and has no code
path for "the server went away" because there is no such event to have one for. It checks, from what
it saw: every call returned; every reply echoed the sequence number that asked for it (so the
kernel's one-shot `Reply` never misrouted); every digest matched **its own independent computation**
of the same definition; and the version word went up exactly once, somewhere strictly inside the
conversation.

That last one is worth stating precisely. The client *can* tell a swap happened, because the reply
carries a version word put there for exactly that purpose. The claim is that its **stream was
unbroken**, not that a swap is undetectable by a client that goes looking.

**Witness two, the operator, in a different address space.** A page `swapper` owns and maps read/write
into each instance; each stamps its own version at the index of every request it serves. Read after
every writer is dead, it says two things the client cannot: that no sequence number went unserved
(nothing was lost in the down window) and that the version **never goes backwards**, which is the
"there were never two owners" assertion, because two instances serving concurrently would interleave.
The two witnesses are cross-checked against each other on *where* the swap happened; neither is taken
on the other's word.

**The control that must fail.** After the revoke, the outgoing instance is told to read one UART
register. It faults, and the kernel's fault message carries the device's own page. Before the revoke
the identical read succeeded (each instance probes at startup and reports), which is what makes this
a receipt rather than a coincidence. A run in which that read *succeeds* is failed loudly rather than
silently: the instance reports `RPT_PROBE_SURVIVED` and the test refuses the run.

**The attacker.** Endowed with exactly the honest client's capabilities, including a real working
capability to the stable endpoint, it tries to park itself in `RECV_CAP` and take the client's
requests. `NotPermitted`.

**And the replacement is written in C** (`user/c/c_swappable.c`, over the seam DECISIONS §31 built).
That is the strongest form of the claim available: what held across the swap is the *contract*, not
a recompile of the same source. The C holds no capability and makes no syscall, because the Rust
shell around it holds every capability and makes every syscall; its entire interface to the system
is `(uint64_t) -> uint64_t`.

## The latency ladder

The roadmap's rule is **opt-in per channel, never the default**, and it is a rule because of a
number.

| rung | what it is | steady-state cost | what it decouples |
|---|---|---|---|
| 0 (default) | the shared endpoint; no process in the path | **zero** (`call_reply`) | lifecycle, at the price of blocking the caller during the window |
| 1 (opt-in) | `broker`, a queue-server process | **1.99x** a direct call, ~1.2 us under HVF | lifecycle, with the producer never blocking |
| 2 | a durable broker that writes the backlog to storage |: | its own crash. **Not built.** |

`broker` is pass-through when both ends are up: it forwards the two words and hands the backend's
answer straight back, holding the client's `Reply` capability across the hop. When the operator tells
it the backend is away it answers `ACCEPTED` immediately and holds the item in its own `.bss` (which
lives in the region it was built in, so the bound is a bound on *its* memory and the kernel's
footprint is unchanged; a runaway producer gets `QUEUE_FULL`, which is backpressure as a value rather
than a policy hidden inside a server). On the way back up it drains in arrival order before it
answers, so "the broker is up" and "the backlog is delivered" are one event to anyone watching.

Its control messages travel **in band on its own front endpoint**, for the same reason `OP_QUIESCE`
does: synchronous rendezvous means a server blocks on one endpoint, and a second one would need the
wait-any primitive DECISIONS §26.5 deliberately does not have.

## The system reclaims itself, and the test asserts it

Every child the operator starts is supervised, and every corpse is collected through the supervision
endpoint (DECISIONS §32), which returns each instance's region to the operator's budget. So
reclaiming the budget at the end can only *succeed* if all five splits are gone: §16 refuses a region
whose children are still carved out of it.

That is an assertion rather than housekeeping, for a reason with nothing to do with tidiness.
`untyped::create` takes a **contiguous** run of frames; the first version of these tests leaked all
three systems, which fragmented the frame allocator badly enough that a *later, unrelated* test could
not get init's own eight-megabyte region. The failure surfaced nowhere near its cause, which is the
usual signature of a leak.

## What this does not yet demonstrate

- **State handoff**, which is where the real engineering is. The component here is near-stateless by
  construction, and that is what makes kill-and-replace sufficient. A filesystem server's open
  handles or a network stack's live connections need a serialise-old / absorb-new protocol.
- **A component manifest.** The operator's endowments are literals in its own source, not a declared
  list of capabilities a vendor's build could be wired from (seL4 CapDL / Fuchsia territory).
- **Dependency-aware orchestration.** One channel at a time, no dependency graph, no cross-component
  quiescence.
- **A hung component.** The whole sequence rests on the outgoing instance answering `OP_QUIESCE`. A
  livelocked one needs the stronger right, which is §32's recorded watchdog case.
- **The console proper.** The component swapped here owns the real UART and is shaped like a console
  server, but `lineedit`/`vterm`/`compositor` are not themselves swapped: the interactive stack is not
  running under the test harness, and building it there would have measured the harness.

## See also

- DECISIONS §41 (this milestone's decisions), §12 (endpoint-only naming), §13 and §16 (revocation),
  §26 (the fault endpoint), §31 (the C seam), §32 (`Endpoint::REAP`)
- notes/ipc-naming.md, notes/supervision.md, notes/object-revocation.md, notes/c-seam.md
- notes/benchmarks.md for `broker_rtt` and what the default rung costs
