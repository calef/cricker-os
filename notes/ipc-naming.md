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

## Family resemblance

This is the seL4 model. Mach calls the same thing a **port**; QNX calls it a **channel**. All
three share the defining move: **you address the channel, never the peer.** It pairs with the
other half of the story, how bulk data crosses between processes, in [frames.md](frames.md):
IPC names the channel and carries control; shared memory carries the data.

---

*Add to this file as new IPC-naming questions come up.*
