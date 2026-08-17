# 23. A capability-routed component OS with live replacement

**Status: PARTIAL.**

**Gate: NONE.** The mechanism is built and the block names nothing that blocks a start on the
**three** remaining residuals: state handoff, dependency-aware orchestration, and the hung-component
case. Each is a lane or more. The fourth, **the component manifest, is built** (2026-08-17,
`crates/component_plan`, notes/component-manifest.md), which is the piece milestone 39's packaging
analysis leans on.

**In brief.** Every userspace component (driver, server, app) is a swappable, vendor-shippable unit behind a stable contract; operators replace them live, no reboot. The console hot-swap is instance one; a durable queue-broker decouples component lifecycles (opt-in per channel, for latency)

**Why it matters.** **the flagship payoff and a product ambition:** competing vendor components, confined by the kernel and swapped live; the verified core is the one fixed thing

**Status (2026-07-30): the mechanism is built and proven on both ISAs; the generalisations below are
not.** DECISIONS §41, notes/live-replacement.md. What landed: the four steps, an unprivileged
operator (`swapper`) that runs them, a client (`chatty`) that talks across the swap and is its own
witness, an attacker holding the client's exact capabilities that cannot become the server, a control
that must fail (the outgoing instance reads a UART register after the revoke and faults, at the
device's own page, with the kernel as the witness), and a replacement written in **C** over §31's
seam, so what held across the swap is the contract rather than a recompile. Both rungs of the ladder
that this milestone specified are built (`broker` is the opt-in one), and priced: `broker_rtt`.

**Three things the build settled, all in §41.** The block imagined a forwarding *process* as the
broker; it does not need one, because §12's endpoint-only naming already makes the endpoint object
the stable name, so the swap costs **zero** in steady state and the kernel's sender queue buffers the
down window. The block's step order (start the new server, then revoke) does not survive contact:
revocation is by physical page, so the endowment has to move to the far side of the revoke, though
the *build* does stay first. And revoking a **device** had to mean take-back rather than destroy,
which is the "deferred CDT finally earns its keep" this block predicted, at one level of the tree.

**The manifest landed 2026-08-17, and the defect it named is gone.** `swapper` no longer contains
an endowment: grep it for `abi::rights` or `abi::aspace::MAP_R` and there is nothing left. The
capability half of the contract lives in `swap_proto` beside the wire half, which is where §41's own
sentence puts it (a program that speaks the protocol **and holds the right capabilities** is the
component), and the operator declares only which of *its own* objects answers to which role name, per
child. Three things fell out that were not obvious from this block. **A component manifest is a
sibling of `grant_plan::Manifest` and not a subtype**, because that one declares what a human at a
prompt may designate and this one declares what a supervisor must route, and the argument is in
notes/component-manifest.md. **A manifest is a request and the provisions are the authority** (the
Fuchsia `use`/`offer` split), which is what stops a vendor's declaration being a privilege-escalation
surface. And the role name is the *component's* while the object is the *supervisor's*, so one
declaration wired two ways makes a component's **peer** substitutable too: `chatty` asks to use
`service` and gets the shared endpoint on one channel and the queue broker's front endpoint on the
other, without noticing. What is **not** done is shipping a manifest with a binary rather than
compiling it in, which is a wire format and so calef's call; the options and their costs are in that
note's `BUGS`.

**What remains, and it is the part the block itself calls the real engineering:** state handoff
(the component here is near-stateless, which is what makes kill-and-replace sufficient),
dependency-aware orchestration (which will need the manifest to carry a dependency graph, and it
does not yet: a component declares what it needs, not that another component supplies it), and the
hung-component case (§32's watchdog). Also the console proper: the component swapped owns the real
UART and is shaped like a console server, but `line_editor`/`display_terminal`/`compositor` are not themselves swapped,
because the interactive stack is not running under the test harness.

**The destination the design points at, and a product ambition.** A client names an *endpoint*,
never a peer (the milestone 7-8 decision), so a component's identity is invisible to the code that
uses it: any program that speaks the protocol and holds the right capabilities *is* the component.
That decoupling is what makes running components replaceable at all, and it generalizes: the aim is
a system where **every userspace component (driver, server, app) is a swappable, vendor-shippable
unit behind a stable contract, and operators replace them live, no reboot** -- with the verified
kernel as the one fixed thing underneath an entirely swappable userland. This is Fuchsia's shape
(capability-routed components, stable protocol interfaces) on a verified core.

**Instance one: hot-swap the console server (the mechanism).** Replace a running server with a new
version, no reboot, with a client that never notices. Four steps, each on earlier machinery:

1. **Start the new server** (a supervisor builds it via the granular verbs, endows it fresh).
2. **Revoke the old server's device capability** so there are never two owners of one device's
   registers (the interleaving hazard): milestone 13's revocation extended from frames to *device*
   capabilities, where the deferred CDT (capability-derivation tree) finally earns its keep.
3. **Redirect clients through a broker.** Clients hold a cap to a stable *broker* endpoint, not to
   the server; the broker re-points on a swap, so substitution is invisible. A userspace naming
   service.
4. **Drain in-flight requests and tear the old server down** (the reaper plus revocation).

**The broker as a queue, and its latency (the concern that governs where this is used).** The
instance-one broker just re-points; the general form *buffers* -- a **durable queue server** that
holds messages in its own budget while a backend is down (crashed, restarting under supervision, or
being swapped), so a producer never blocks on an absent consumer and the new consumer drains the
backlog. This is the OS analogue of a distributed message queue (Kafka/RabbitMQ): a stable, always-up
broker decouples the *lifecycles* of the two ends, which is what makes crash-restart and live swap
seamless rather than merely possible. The kernel does not change -- it keeps synchronous rendezvous
(tiny, verified, no allocation); the queue is userspace policy, its buffer bounded by the server's
own untyped, so a runaway producer hits backpressure or a drop policy, never unbounded kernel memory.

Latency is the price, and it dictates where the queue is wired. Interposing a queue server turns one
rendezvous (one IPC, one switch, register transfer) into **two IPCs, two switches, and a copy**
through the server's buffer -- roughly a 2x IPC tax plus a scheduling hop. On a microkernel where
IPC is the hot path, that is not paid everywhere:

- **Opt-in per channel, never the default.** Direct synchronous rendezvous stays the fast path;
  queuing is chosen only for channels that cross a lifecycle boundary (components that restart or
  swap), where the decoupling is worth the tax.
- **Pass-through when both ends are up.** The broker buffers only during the down window; in steady
  state, with a live consumer waiting, it forwards directly, keeping the common case near direct IPC.
- **A latency ladder, not one point.** Fastest: a shared-memory ring buffer + async notification
  (the io_uring / virtio shape nife *already runs* for device I/O; the notification primitive
  is a generalisation of the endpoint's async-signal count) -- no middleman process, decouples in
  rate. Middle: a queue-server process -- decouples lifecycle, one extra hop. Slowest: a durable
  queue server that writes to storage -- survives its own crash. The rung is a per-channel choice.
- **Measure it, do not argue it.** Milestone 21's benchmark harness is the instrument: add a
  queued-IPC round trip beside the direct one, so the tax is a committed baseline number and a
  regression in it surfaces proximate to its cause.

Prior art for the queue itself: Mach ports (kernel message queues, macOS's foundation), Unix pipes,
POSIX/SysV message queues, and every distributed broker (Kafka, RabbitMQ, SQS); the shared-memory
ring variant is io_uring, DPDK, and virtio.

**Generalising to all components: what the console case does not yet need.**

- **A uniform component contract + manifest.** **Built 2026-08-17**, `crates/component_plan` and the
  four declarations in `crates/swap_proto`. Each component declares the capabilities it needs (this
  device, these endpoints) and the supervisor wires it from the declaration, with a typed refusal
  before anything is built when it cannot. seL4 CapDL / Fuchsia territory as this block predicted, and
  Fuchsia's `use`/`offer` split turned out to be the load-bearing half. Still compiled in rather than
  shipped beside a binary: notes/component-manifest.md.
- **State handoff (the crux).** The console is easy because it is near-stateless. A filesystem
  server (open handles, caches, in-flight writes) or a network stack (live connections) cannot be
  kill-and-restarted without losing state; live-swapping them needs a serialise-old / absorb-new
  protocol over a supervisor-brokered channel. Prior art: Erlang/OTP `code_change`, VM live
  migration, CRIU checkpoint/restore. This is where the real engineering is.
- **Dependency-aware orchestration.** If B is a client of A, swapping A means quiesce B, swap,
  resume; the supervisor (22) needs the dependency graph and a quiescence protocol.

**The fixed core, stated honestly.** Two things are deliberately *not* hot-swapped this way, and
that boundary is a feature. The **kernel** is the verified TCB enforcing everything; you do not
live-swap it (changing it is a reboot; seamless kernel update is a separate, heavier problem). A
**minimal init / root supervisor / broker** is the fixed point that makes swapping everything else
possible -- pushed as tiny and stable as it can be, but you cannot swap the swapper infinitely.

**Why this is the selling point, and safe.** Because the kernel confines every component to exactly
the capabilities it was granted, **untrusted, competing vendor components run safely**: a Linux
vendor kernel module is ring-0 and can do anything; a nife vendor component is a confined
process that can touch only what the operator handed it. A malicious console driver scribbles on the
UART it was given and nothing else -- it cannot read another component's memory, forge authority, or
reach the kernel. That is what makes "different vendors ship competing components, operators swap
them live" not merely possible but *safe*, and it is the payoff of the capability model plus
milestone 22's authority-minimisation. It also connects directly to the parked competitor ambition
([competitor-question.md](../competitor-question.md)): this component model *is* a general-purpose product story, on the verified
core the demonstrator earns first.

**Prior art.** Fuchsia (the closest match: capability-routed, manifest-declared, swappable
components); MINIX 3's reincarnation server (live driver replacement in userspace); QNX
(hot-swappable drivers); Erlang/OTP hot code loading and supervision. The common thread is ours:
components are isolated processes, named through indirection and confined by capability, so one can
be swapped under the others.
