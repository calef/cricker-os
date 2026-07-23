# Who does IPC name?

Short answer: **an endpoint, and nothing else.** Not a thread, not a process, not an address,
not a name in any global table. This is one of the defining choices of the whole model, so it
is worth being exact about.

## The sender names a channel, never the peer

A send is `ipc_send(ep, msg)` (`sched.rs`). `ep` identifies an **endpoint**: a rendezvous
channel. Whoever is blocked *receiving* on that endpoint gets the message. The sender has no way
to say "send to thread 5" or "send to the filesystem process." It can only say "send on this
endpoint," and **whoever answers, answers.**

```rust
struct Endpoint {
    senders: VecDeque<Tid>,    // blocked senders, if no receiver was waiting
    receivers: VecDeque<Tid>,  // blocked receivers, if no sender was waiting
    pending: u32,              // async signals (interrupts) delivered with nobody waiting
}
```

The receiver is **anonymous to the sender.** Any thread holding the receive side can service the
endpoint, which is exactly what lets a pool of workers sit behind one endpoint, or a driver be
replaced, with no client the wiser. See [capabilities.md](capabilities.md) for the rendezvous
mechanics and why which-end-you-are is a matter of rights (SEND needs WRITE, RECV needs READ).

## Two levels of naming, and the unforgeable part

Userspace never touches the raw endpoint index. A process names an endpoint through a
**capability** in its cspace, the same mechanism as a Unix file descriptor:

| Level | The "name" | Who can forge it |
|---|---|---|
| Userspace-visible | a cspace **slot** ("send on slot 7") | nobody: the cspace is in kernel memory |
| Kernel-internal | the endpoint's index, inside the `Object::Endpoint` capability the slot holds | only the kernel, which mints caps |

So the honest answer to "who does IPC name" is: **a capability the caller holds, which the
kernel resolves to an endpoint.** You can only name a channel you were *given*.

## Why this is the point (DECISIONS.md §10)

This is no-ambient-authority made concrete. There is **no global namespace**: no PIDs to signal,
no ports to connect to by number, no keys to look up. Compare Unix, which names IPC targets
through several global namespaces at once:

| Unix IPC | Names its target by | Ambient? |
|---|---|---|
| `kill(pid, sig)` | a global process id | yes |
| a socket | an address + port | yes |
| a SysV message queue | a global IPC key | yes |
| a pipe | an fd | **no** (an fd is already a capability) |

Every ambient one lets a process reach something by *knowing its name* rather than *having been
handed access*. Ours has exactly one mechanism (an endpoint capability) and no back door. §10's
whole argument is that the moment one syscall accepts a global name, you have rebuilt the thing
capabilities exist to avoid.

## Two corollaries the code already shows

- **A hardware interrupt also names an endpoint.** `bind_irq(intid, ep)` routes an IRQ to an
  endpoint; `irq_notify` delivers it, or bumps `pending` if nobody is waiting. So the naming of a
  device event is the *same* as the naming of a peer's message: an endpoint. "An interrupt becomes
  a message" is literal. See [interrupts.md](interrupts.md).
- **The name is transferable.** `SEND_CAP` / `RECV_CAP` let one process hand an endpoint
  capability to another over IPC, narrowed but never widened. Authority to name a channel is
  itself something you can pass along. See [delegation.md](delegation.md).

## Who is waiting on the other side, and in what order

An endpoint is a rendezvous, so the natural question is who receives. In our code (`Endpoint` in
`sched.rs`), **both sides are FIFO queues on the endpoint itself**:

- `receivers: VecDeque<Tid>` — threads blocked in `RECV` with no sender yet.
- `senders: VecDeque<Tid>` — threads blocked in `SEND` with no receiver yet.

At most one queue is ever non-empty (whoever arrived first and had to wait). A `SEND` that finds a
receiver does `receivers.pop_front()` and wakes exactly that one; a `RECV` that finds a sender does
`senders.pop_front()`. Two consequences:

- **A thread pool works out of the box.** N server threads can all `RECV` on one endpoint; they
  queue in `receivers`, and each incoming message wakes one, in arrival order. The kernel picks,
  and the client cannot tell which server answered (the anonymity again).
- **The endpoint is unbuffered.** There is no capacity and no "full" state: a `SEND` blocks *iff*
  no receiver is waiting, never because a buffer filled. This is seL4's rendezvous model, not a
  message queue, and it is fine because bulk data moves by a shared frame, not by copy
  ([frames.md](frames.md)) — there is nothing to buffer.

FIFO on both sides, on the endpoint, chosen deliberately and matching seL4.

## The reply problem, and the Reply capability (milestone 12)

**Built.** As of milestone 12 there is a one-shot Reply capability and a `CALL` method; see
DECISIONS §12. What follows is the design that led there, kept because the reasoning is the point,
and written in the present tense of *before* it existed.

Before milestone 12 there was no Reply capability and no `Call` (atomic send-and-wait) primitive: the
endpoint methods were `SEND`, `RECV`, `SEND_CAP`, `RECV_CAP`, and that was all (`crates/abi`).

The gap is the direct consequence of the anonymity above. A server that `RECV`s a request has **no
idea who sent it**, so it cannot reply to that specific caller. seL4 solves this with a
**Reply capability**: on a `Call`, the kernel mints a *one-shot* cap naming "whoever just called"
and hands it to the server, which `ReplyRecv`s to exactly that caller and then waits for the next.
It buys three things:

1. **Reply to an anonymous caller without pre-wiring** — a server can serve clients it was never
   individually introduced to.
2. **One-shot safety** — the cap is consumed on use, so the server cannot hoard it, reply twice,
   or hold the caller hostage.
3. **A kernel-tracked call chain** — which enables priority donation (the server runs on the
   caller's time) and makes `Call`+`ReplyRecv` the optimized RPC fast path.

### What we do instead

A second, explicit **reply endpoint**, wired at spawn. The console server (`user.rs`) is the
pattern: a `request` endpoint (server `RECV`, client `SEND`) and a `reply` endpoint (server
`SEND`, client `RECV`), both created up front and granted to each party with the right rights. It
works because the client topology is **static** — one known client per reply endpoint.

Its limits are exactly the three points above, inverted: it does not scale to a server with many
*anonymous* clients (which client's `RECV` grabs a shared reply is ambiguous), there is no call
chain so no priority donation, and the server's reply `SEND` **blocks** until the client reaches
its `RECV` (a real Reply cap delivers to a caller already parked, without blocking the server).

We could go part of the way with the machinery we already have: a client could create a reply
endpoint and pass a `SEND` cap to it *in the request* via `SEND_CAP`, so the server receives "reply
here" alongside the message. That fixes anonymity without pre-wiring. It still would not be
one-shot, and still would carry no call chain: those need real kernel support (a `Reply` object and
a `Call` syscall).

### The decision: design now, build at the trigger

Following the advice that the reply path is *endpoint semantics, not a separate feature*, it is
designed here rather than left implicit. But building a kernel `Call`/`Reply` primitive now would
violate DECISIONS.md §4 (the syscall surface stays narrow; no abstraction before the requirement):
every server we have has a static client topology and the two-endpoint pattern serves it.

**Two triggers to build it.** *Functional:* the first server that must answer clients it was not
individually wired to (a general RPC service). *Safety:* the first reply whose correctness depends
on going to **this** caller, or on being consumed **exactly once** — because a pre-wired reply
endpoint is reusable and nameable, so nothing *structural* prevents a misrouted reply, a double
reply, or a stale reply landing on a client that has moved on. A one-shot kernel-minted reply cap
makes those kernel guarantees instead of server discipline. At that point the right shape is a
`Reply` object capability and a `Call` method, one-shot, with the call chain — its own DECISIONS
entry when it lands, because it widens the boundary §4 guards.

**Safe today, by convention not by guarantee (checked 2026-07-22).** The console server shares one
`reply` endpoint across clients yet is correct, because it is **single-threaded** and IPC is
synchronous rendezvous: it runs one request-reply cycle at a time, so the only client parked in
`RECV(reply)` when it replies is the one it just served. Workers and drivers avoid the question
entirely with a **per-request** reply endpoint. Nothing in the kernel enforces either property; the
safety trigger above fires the moment a server fields concurrent clients on a shared reply path (a
thread pool, or pipelined requests).

## Family resemblance

This is the seL4 model. Mach calls the same thing a **port**; QNX calls it a **channel**. All
three share the defining move: **you address the channel, never the peer.** It pairs with the
other half of the story, how bulk data crosses between processes, in [frames.md](frames.md):
IPC names the channel and carries control; shared memory carries the data.

---

*Add to this file as new IPC-naming questions come up.*
