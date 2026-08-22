# 101. Notification objects: async multiplexing without wait-any

**Status: DECIDED.** Raised 2026-08-20 by the architect's question ("let's decide on the seL4
model"), at the point where milestone 40's remaining fork exposed the single-wait-point limitation
as the structural constraint every component in this tree is working around. Decided the same day:
the seL4 notification object model is the right long-term solution for this kernel, and this file is
the specification a build lane works from. Nothing is built on it yet; the first build flips the
status to AMENDED with the implementation record.

**What is blocked: nothing directly.** The `terminal_sink_caretaker` narrowing (milestone 40's
remaining fork) unblocks the documentation viewer without this. What this unblocks is the class of
problem: every component that needs to wait on more than one thing at once gets the primitive it is
currently working around the absence of.

## What is being decided

Whether to add a **notification object** - a new kernel object type, created by `RETYPE_OBJ`, that
holds a data word and a wait queue, supports `SIGNAL` (async, non-rendezvous, never lost) and `WAIT`
(blocking receive of the accumulated word), and can be **bound to a TCB** so that a signal arriving
on the notification wakes a thread that is currently blocked in `RECV` on an *endpoint*.

This is seL4's notification object (seL4 manual, chapter "Notifications"), adapted to this kernel's
idioms: untyped-funded, generational, capability-governed, and riding on the existing `INVOKE`
dispatch rather than adding a new syscall.

## Why this is the right long-term solution

### The single wait point is the structural limit

A process in this kernel has exactly one blocking wait point. `SEND` blocks until a receiver takes
the message; `RECV` blocks until one arrives; there is no select, no poll, no receive-on-a-set, no
timed wait (DECISIONS §51's fork is still open, milestone 106 is NOT-STARTED). Every component that
must distinguish more than one class of sender hits this wall:

- **Shell** (notes/pipes.md): cannot feed a chain and read from it, so `doc page.md` is refused
  without a barrier downstream.
- **Compositor** (notes/compositor.md, §33): cannot hold per-client endpoints, so it routes
  everything through one endpoint and carries authority in memory. "The constraint is structural,
  not stylistic."
- **Supervision** (§26.5): chose a shared endpoint with kernel-stamped identity *specifically to
  avoid* needing wait-any or a thread per child.
- **FS server** (notes/fs-server.md): wants a set of endpoints, doesn't have it.
- **Credentialer** (notes/credentials.md): "this kernel has one wait point."
- **Network stack** (milestone 106): yields and re-polls across every retransmit window, burning a
  hart, because there is no timed wait.

Every one of these is the same missing primitive wearing a different hat. The `terminal_sink_caretaker`
narrowing solves one instance for one program; a notification object solves the class.

### The kernel already has half the mechanism

`ipc::Endpoint` already carries a **pending-signal count**: `signal()` wakes a waiting receiver or
counts the signal so it is not lost; `recv` drains a pending signal first. IRQ capabilities are
bound to endpoints via `bind_irq`, and `irq_notify` calls `endpoint.signal()`. The proved invariant
("at most one wait queue is ever non-empty") holds for signals because a signal never queues the
signaller - it is deliberately not a rendezvous.

What exists today:
- `signal()` on `ipc::Endpoint` - wakes a receiver or increments `pending`
- `bind_irq(intid, ep)` - routes a hardware interrupt to an endpoint
- `irq_notify(ep)` - called from IRQ context, calls `endpoint.signal()`

What is missing:
1. A **user-callable signal**: today only the kernel's IRQ handler can signal an endpoint. A
   userspace process needs to signal a notification object.
2. A **separate object type**: signals today are a side channel on an endpoint, which means the
   signal count and the IPC rendezvous share one wait queue. seL4 separates them: a notification is
   its own object with its own queue, and an endpoint is purely synchronous.
3. **Binding to a TCB**: today a signal wakes a thread blocked on the *same* endpoint. Binding lets
   a signal on a notification wake a thread blocked on a *different* endpoint (one is an IPC `RECV`,
   the other is the bound notification), so a thread can wait for IPC and be woken by an async
   signal at the same time.
4. **A badge**: the notification word carries information about which signaler fired, so the
   receiver can distinguish "the child exited" from "the timer fired" from "a key was pressed."

## What other operating systems do

### seL4: notification objects, bound to TCBs (the model we adopt)

seL4 has a separate `Notification` object type: a data word (an array of binary semaphores) plus a
queue of TCBs. `seL4_Signal` ORs the capability's badge into the word and wakes the head waiter (or
counts the signal if nobody is waiting). `seL4_Wait` blocks if the word is zero, returns the word
and clears it if non-zero. `seL4_Poll` is the non-blocking version.

The key mechanism is **binding** (`seL4_TCB_BindNotification`): a notification bound to a TCB
delivers signals even while the thread is blocked in `RECV` on an endpoint. The receiver
distinguishes "this was IPC" from "this was a notification" by checking the badge. This is how seL4
does `select` without a `select` syscall: one blocking wait point that can be woken by either a
synchronous IPC message or an async notification, and the badge tells you which.

### Unix/Linux: select/poll/epoll/kqueue

Unix's answer is fd multiplexing. `epoll_wait` blocks until one of N fds is ready. This works
because fds are a uniform namespace and the kernel maintains readiness state per fd. It is the wrong
model for a capability kernel: fds are ambient (a process can reach any fd its parent didn't
close), readiness is a polling abstraction rather than a delivery, and the mechanism is a
kernel-internal data structure rather than a delegatable capability.

### Fuchsia: zx_port_wait

Fuchsia has `zx_port` objects - async signal queues a thread can wait on. Multiple sources (timer,
channel, device interrupt) queue packets to the same port, and `zx_port_wait` returns one. Closer to
seL4's model (delivery, not readiness) but adds a layer: the port is a separate object that queues
packets, where seL4's notification is simpler (a word + a queue).

### Mach (macOS/XNU): ports and port sets

Mach has `mach_port` and `mach_port_set` - a thread can receive on a set of ports, and `mach_msg`
returns from whichever has a message. This is the direct ancestor of seL4's model (seL4 started as
L4, which started as a Mach replacement). seL4 replaced port sets with bound notifications because
sets added complexity (the set is another object with its own lifetime and revocation) for a
mechanism notifications handle more simply.

### Redox: event queues

Redox has `event:` schemes and an event queue. A process registers interest in events from multiple
fds and blocks on one queue. This is the Unix model with a Redox-specific API, and it assumes the
scheme namespace and fd model nife doesn't have.

## The design

### New object type

```
objtype::NOTIFICATION: u64 = 4
```

A notification is a page-resident kernel object, created by `RETYPE_OBJ`, owned by the caller's
untyped budget. It holds:

- A **notification word** (`u64`), acting as a bitfield of binary semaphores.
- A **wait queue** (intrusive `Fifo<Thread>`, same as an endpoint's receiver queue).
- A **bound TCB** (`Option<Tid>`, set by `BIND`, at most one).

One page per object, same as every other object type (§14's one-object-per-page rule).

### New methods

```rust
pub mod notification {
    /// invoke(cap, SIGNAL, word, _, _) -> 0.
    /// OR `word` into the notification word. If a thread is waiting, wake it and deliver the
    /// accumulated word. If a notification is bound to a TCB and that TCB is blocked in RECV on
    /// an endpoint, wake it there instead (the signal arrives as a badge on the RECV return).
    /// Never blocks, never lost. Needs WRITE.
    pub const SIGNAL: u64 = 0;

    /// invoke(cap, WAIT, _, _, _) -> word.
    /// If the notification word is non-zero, return it and clear it. If zero, block until a
    /// signal arrives. Needs READ.
    pub const WAIT: u64 = 1;

    /// invoke(cap, POLL, _, _, _) -> word.
    /// Non-blocking WAIT: return the word (possibly zero) without blocking. Needs READ.
    pub const POLL: u64 = 2;

    /// invoke(cap, BIND, tcb_slot, _, _) -> 0.
    /// Bind this notification to the TCB in `tcb_slot`. From now on, signals to this notification
    /// are delivered to that TCB even if it is blocked in RECV on an endpoint. At most one
    /// notification may be bound to a TCB. Needs WRITE on the notification and WRITE on the TCB.
    pub const BIND: u64 = 3;
}
```

### Rights

Notifications use the existing rights model:

| Right | What it permits |
|-------|----------------|
| `READ` | `WAIT`, `POLL` |
| `WRITE` | `SIGNAL`, `BIND` |
| `GRANT` | Delegate the notification capability to another process |

No new rights bits. `ENUMERATE` is not needed (a notification has no children to survey).

### The binding mechanism

This is the part that makes it more than a counting semaphore. When a notification is bound to a
TCB:

1. If the TCB is **runnable or blocked on the notification itself**, `SIGNAL` behaves as on an
   unbound notification: wake the waiter or count the signal.

2. If the TCB is **blocked in `RECV` on an endpoint**, `SIGNAL` wakes the TCB from that endpoint's
   wait queue instead. The `RECV` returns with a distinguished return value indicating "this was a
   notification, not an IPC message," and the notification word is available in the return
   registers (the badge).

3. If the TCB is **blocked anywhere else** (in `SEND`, in `CALL`, in `REAP`), the signal is counted
   in the notification word and delivered when the TCB next enters `RECV` on any endpoint or calls
   `WAIT` on the notification. This is the same "remember, don't lose" semantics the existing signal
   count has.

This means a thread that does `RECV` on its IPC endpoint can be woken by either:
- A synchronous IPC message (a sender rendezvoused), or
- An async notification signal (the bound notification was signaled)

and it can tell which by the return value.

### The return-value distinction

`RECV` today returns `w0` (the message's first word, or `1` for a signal). We extend the convention:

- `w0 = 0`: this was an IPC `SEND` rendezvous. `w1`, `w2` carry the sender's data words.
- `w0 = 1`: this was an IRQ signal (existing, unchanged).
- `w0 = 2`: this was a bound notification. `w1` carries the notification word (the accumulated
  badge). No `w2`.

This is additive - the existing `0` and `1` cases don't change - and it costs one new constant in
the ABI.

### What this does NOT add

- **No new syscall.** `SIGNAL`, `WAIT`, `POLL`, and `BIND` are methods under the existing
  `SYS_INVOKE` dispatch, the same way `SEND`, `RECV`, `REAP`, and `SURVEY` are. §4's "the syscall
  surface stays narrow" holds: `SYS_INVOKE` already exists, and this adds one new `objtype` and four
  new method constants under it.

- **No badged capabilities (yet).** seL4 badges the *capability* to the notification, so each
  signaler's copy ORs a different bit. This kernel has no badge field on capabilities (notes/abi.md,
  notes/supervision.md, notes/compositor.md all record this as a design fork). The notification word
  is passed as an argument to `SIGNAL`, so a signaler *can* identify itself by choosing a bit, but
  the kernel does not stamp it. This is the honest interim: a signaler can lie about who it is, same
  as a `SEND` can. Badged capabilities are the fork that retires the lie, and this decision does not
  take it; it records that the notification object is ready for the day it arrives.

- **No timed wait.** `WAIT` blocks indefinitely, same as `RECV`. A timed wait is milestone 106's
  fork, and a notification bound to a TCB does not give the TCB a deadline. What it does give is the
  ability to be woken by a *timer notification* - a user process that holds a clock capability and a
  thread can signal a notification when the timer fires, which is how seL4 userspace implements
  sleeps without a kernel timed-wait.

## What this replaces and what it doesn't

### The `terminal_sink_caretaker` narrowing (milestone 40)

The narrowing is still the right short-term move. It's a wiring change, not a kernel change, and it
unblocks the documentation viewer immediately. With notification objects, the shell could instead
bind a notification to its TCB, `RECV` on the child's output endpoint, and be woken by either the
child's `OP_EOF` (IPC) or a bound notification (exit, interrupt). That is the cleaner long-term
shape, but it requires the child's exit to signal the notification, which requires the kernel's
death-delivery path (§26) to signal notifications in addition to endpoints. That is a follow-on, not
part of this decision.

The narrowing remains valid as a permanent shape for "a tail stage whose output goes to the screen"
- seL4 systems use direct device access too. Notifications are the general primitive; the narrowing
is a specific wiring that doesn't need them.

### The existing signal mechanism on endpoints

The `signal()` method on `ipc::Endpoint` and the `bind_irq` / `irq_notify` path remain unchanged.
IRQ delivery to endpoints is built, proved, and working. The notification object is a *new* object
type for *user-initiated* async signals, not a replacement for the IRQ path.

Over time, IRQs may be rerouted to notifications (a driver would bind a notification to its TCB and
receive both IPC and IRQ signals on one wait point), but that is a migration, not a requirement, and
this decision does not mandate it.

### The shared supervision endpoint (§26.5)

§26.5 chose a shared endpoint with kernel-stamped identity to avoid needing wait-any. That decision
holds: the kernel is the only sender on the supervision path, so the tid is trustworthy without
badges, and a notification object does not change that. What it does change is the *alternative* a
supervisor could take: per-child fault endpoints, with a notification bound to the supervisor's TCB,
waking it from whichever child's endpoint delivers first. That is now possible but not required;
§26.5's choice remains valid.

## Surface cost

| Area | Cost |
|------|------|
| New `objtype` | `NOTIFICATION = 4` in `abi::objtype` |
| New methods | `SIGNAL`, `WAIT`, `POLL`, `BIND` in `abi::notification` |
| New ABI constant | One return-value tag (`w0 = 2` for notification delivery on a bound RECV) |
| New kernel object | A `Notification` struct (word + queue + bound TCB), page-resident, in the object registry |
| `RECV` dispatch | One new case: check whether the blocked TCB has a bound notification with a non-zero word, and if so, return it instead of blocking |
| Kani proofs | The notification's invariant: "the word is zero iff no signal is pending and no waiter is queued" (same shape as the endpoint's "at most one queue non-empty") |
| Syscall surface | **No new syscall.** All methods dispatch under the existing `SYS_INVOKE`. |

## What this unlocks (by consumer)

| Component | What it gets |
|-----------|-------------|
| **Shell** | Wait on child output (IPC) and child exit (notification) at the same time; no barrier required for `doc page.md` |
| **Compositor** | Per-client endpoints, woken by a bound notification for input/screenshot; unforgeable sender identity (when badges arrive) |
| **FS server** | One `RECV` woken by any client's message, with a notification for async events (disk full, queue drain) |
| **Network stack** | A timer process signals a notification; the stack's `RECV` on the socket endpoint is woken by either a packet or the timer, retiring the yield-and-re-poll |
| **Supervisor** | Per-child fault endpoints with a bound notification, if desired (§26.5's shared endpoint remains valid) |
| **Pager** | `doc <page>` rendering with pager: a keypress signals a notification, the viewer's `RECV` on the terminal endpoint is woken by either the next input byte or the interrupt |
| **Milestone 106** | A timed wait becomes "a timer process signals my bound notification," which is how seL4 userspace does it without a kernel `sleep` |

## Sequencing

1. **Now**: take the `terminal_sink_caretaker` narrowing (milestone 40, no kernel change).
2. **Notification object**: new `objtype`, four methods, the binding mechanism, Kani proof. This is
   a kernel milestone - one lane, estimated from the existing `Endpoint` and `signal()` work at the
   same scale as 19a (which added `RETYPE_OBJ` and the endpoint object). **Minted as milestone 151**
   (2026-08-22), forced by a concrete bug rather than scheduled speculatively: §106's
   `terminal_sink_caretaker` narrowing can race its own completion signal, and this is that fix's
   tracked home.
3. **Retrofit**: the shell binds a notification to its TCB and replaces the "wait for exit" hack
   with a proper multiplexed wait. The compositor can take per-client endpoints. The network stack
   can stop yield-spinning.
4. **Badge fork** (separate decision): badged capabilities, so notification identity is
   kernel-stamped rather than signaler-supplied. This is the fork notes/supervision.md,
   notes/compositor.md, and notes/dir-capability.md already record.

## Why not the alternatives

### Wait-any on endpoints (the port-set model)

A `RECV` that waits on a *set* of endpoints. Mach's port sets, and the form notes/compositor.md
considered.

Rejected for the reason seL4 rejected it: the set is a new container object with its own lifetime
and revocation, and every endpoint in the set gains a second wait queue (the set's). The
notification object is simpler: one queue, one word, no container, and the binding mechanism gives
the multiplexing without the set's complexity.

### Non-blocking RECV (the poll model)

A `RECV` that returns immediately if no message is pending. seL4 has this (`seL4_NBRecv`,
`seL4_Poll`), and it is part of this design (`POLL` on a notification). But non-blocking RECV alone
does not solve the multiplexing problem - it turns "block" into "spin," and a spin loop burns a hart
(milestone 106's current answer, which the notes call "the honest interim a riscv-SMP lost wakeup
forced"). The notification object is what makes non-blocking RECV useful: poll the endpoint, poll
the notification, and if nothing is ready, `WAIT` on the notification (which blocks until something
is).

### Pull-based source (the single-endpoint model)

Restructure the sink contract so a stage `CALL`s for input and `SEND`s output on one endpoint, and
the shell's loop is one `RECV` that either replies with bytes or writes bytes out. notes/pipes.md
records this as a design fork and calef's call.

Rejected for the reason the notes give: it destroys the property that the read end and write end are
separate capabilities, so the program on the right of a `|` could write back up its own input. That
property is called "load-bearing" and the notification object preserves it: read and write stay
separate endpoints, and the multiplexing happens at the wait level, not the protocol level.

### Buffering stage

Measured and rejected (notes/pipes.md, 2026-08-03). A buffer doubles the per-message cost (second
rendezvous) and cannot batch its way out of the 16-byte sink contract cap. Buffering buys
decoupling, not bandwidth, and no pipeline in the tree has producer-side work to overlap. The
notification object does not make buffering more attractive - it makes it less, because the
deadlock buffering was proposed to fix is solved structurally.

## The honest caveats

- **Binding is the hard part.** The `RECV` dispatch change - "check whether the blocked TCB has a
  bound notification with a non-zero word" - touches the IPC fastpath, which is the most
  performance-sensitive code in the kernel (milestone 132, notes/l4-lessons.md rows 11/13/14). The
  check is one load and one compare on a field the TCB already carries, but it is on the path that
  `ipc_rtt_el0` measures, and the cost must be measured rather than assumed. If the check is
  non-zero (the common case is no notification bound), it is one branch not taken.

- **The badge is not kernel-stamped.** A signaler chooses the word it ORs in, and nothing prevents
  a signaler from lying. seL4 stamps the badge at the capability level, so a signaler cannot
  impersonate another. This kernel has no badge field on capabilities, and adding one is a separate
  fork (badged endpoints/notifications) that touches every capability in the tree. This decision
  takes the honest interim: the notification word is signaler-supplied, same as a `SEND`'s data
  words are signaler-supplied. Badges are the fork that retires the lie, and this design is ready
  for them.

- **One notification per TCB.** seL4 allows one bound notification per TCB, and so does this design.
  A thread that needs to wait on multiple async sources binds one notification and has each source
  signal a different bit. This is sufficient for every consumer identified above, and it keeps the
  TCB's binding field to one `Option<Tid>`.

- **The notification object is not a substitute for milestone 106 (timed wait).** A timed wait is a
  kernel primitive that blocks for a deadline. A notification object lets a *userspace timer
  process* wake a thread at a deadline, which is how seL4 does it, but it requires that timer
  process to exist and to hold a clock capability. If the kernel itself needs a timed wait (for a
  watchdog, a scheduling deadline, or a retransmit timer in-kernel), milestone 106 is still owed.
  This decision does not retire it; it gives userspace a way to build the same thing without waiting
  for the kernel to.

## What this does not decide

- **Badged capabilities.** Recorded as a separate fork in notes/supervision.md,
  notes/compositor.md, notes/dir-capability.md, and notes/grant-expression.md. The notification
  object is compatible with badges and ready for them; this decision does not add them.
- **The `terminal_sink_caretaker` narrowing.** That is milestone 40's fork, and this decision does
  not take it. The two are independent: the narrowing is a wiring change that works today; the
  notification object is a kernel change that replaces the class of workaround.
- **Direct process switch (§96).** The L4 lessons audit found this kernel has no direct process
  switch (rows 11/13/14). That is a separate decision about the scheduling model, and the
  notification object does not depend on it or change it.
- **IRQ migration.** Whether IRQs are rerouted from endpoints to notifications is a migration
  decision for the driver model, not this decision. The existing `bind_irq` / `irq_notify` path
  stays.
