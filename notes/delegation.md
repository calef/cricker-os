# Delegating a capability

For most of this project cricker-os *called* itself a capability system while quietly not being one
in the way that matters. Capabilities existed, rights existed, `GRANT` existed, and `derive` (copy a
capability with narrowed rights) was written and unit-tested. But a running process could not use any
of it. Every capability was minted by the kernel and placed in a process's table at spawn, and there
was no operation for a process to hand a capability to another process. The kernel was a central
authority-granting oracle. That is ambient authority wearing a capability costume: the thing §10
argued against, moved one layer down.

This note is the fix. A process can now **delegate a capability to another process over an IPC
endpoint**, and that is the operation that makes the model composable by the processes themselves.

## The shape: capabilities ride the same channels as messages

The delegation path reuses the endpoints we already had. Two new methods:

- `SEND_CAP(channel, cap_slot, rights, data)` — pass the capability in `cap_slot`, narrowed to
  `rights`, plus one data word, over `channel`. Blocks until a receiver takes it, like `SEND`.
- `RECV_CAP(channel)` — receive a data word and, if one was delegated, a capability. The capability
  lands in a free slot of the *receiver's own* cspace, chosen by the kernel, and `RECV_CAP` returns
  that slot number (or `NO_CAP` if the message carried none).

This is the seL4 model: capabilities move as part of IPC, not through a side channel. It fits what we
already built, and it means the receiver never names a slot it doesn't own. The kernel picks the
slot and tells the receiver where the capability went.

## Two rights, two different questions

`SEND_CAP` checks two rights, and conflating them would be a bug:

- **`WRITE` on the channel.** May I send on this endpoint at all? Same check `SEND` makes.
- **`GRANT` on the capability being passed.** Was I trusted to hand *this* on? This is the right
  Unix has no word for. Without it you may use a thing and not lend it. Our console capability is
  `WRITE` without `GRANT` for exactly this reason: a program may print and may not pass printing to
  anyone else.

And the rights the receiver ends up with must be a **subset** of what the sender holds. Delegation
narrows, never widens. If it could widen, the model is theatre: you would delegate yourself a better
capability than the one you were given. The kernel rebuilds the requested rights from their bits,
masks them to the defined set, and rejects anything that is not a subset. So a sender can drop
`GRANT` on the way (hand over the use of a thing without the right to pass it on further), which is
exactly what the demo does.

## The mechanism, and where the capability actually moves

The transfer happens at the rendezvous, under the scheduler lock, because that is the one moment both
processes' cspaces are reachable at once:

- **Receiver waiting when the sender arrives.** The sender inserts the capability into the receiver's
  cspace right then, records the slot in the receiver's mailbox, and wakes it.
- **Sender waiting when the receiver arrives.** The sender parked the capability in a new
  `Thread.outgoing_cap` field (the capability analogue of the mailbox that parks the data words). The
  receiver `take()`s it and files it in its own cspace.

If the receiver's cspace is full the capability is dropped and the receiver sees `NO_CAP`; the data
word still arrives. One honest wart: `SEND_CAP` and plain `RECV` (or `SEND` and `RECV_CAP`) share the
same endpoint queues and do not check that both sides agree to carry a capability. A correct protocol
uses the matching pair. Mixing them does not corrupt anything, it just delivers a capability nobody
reads, or reports `NO_CAP` to someone who expected one.

## What the test proves, and how it can fail

`a_capability_can_be_delegated_over_ipc_and_grant_gates_re_delegation` wires two user processes and
checks the three things that have to hold:

1. **The receiver gets the capability.** `RECV_CAP` returns a real slot, not `NO_CAP`.
2. **The capability works when the receiver invokes it.** The receiver `SEND`s a distinctive word on
   the delegated capability, and a `RECV` on the other end collects it. A capability minted by one
   process carries real authority when invoked by another.
3. **`GRANT` gates re-delegation.** The receiver was handed the capability narrowed to `WRITE`
   (no `GRANT`), so when it tries to `SEND_CAP` it onward, the kernel refuses before any rendezvous.

Verified it can fail: stub the `GRANT` check to accept, and claim 3 collapses. The receiver's
re-delegation is now permitted, blocks forever on the empty endpoint it tried to send over, and the
whole test suite hangs. The gate is load-bearing, not decoration.

## What this unlocks

Delegation is the prerequisite for the next simplification worth making: a `Frame` capability. Today
every shared page (the console buffer, the DMA region) is mapped into both parties by the kernel at
spawn. With delegation in place, a process can hold a `Frame` capability and *pass it* to whoever it
wants to share memory with, narrowing the rights, instead of the kernel pre-wiring the sharing. That
is "IPC carries control, shared memory carries data" (§10) done by the processes rather than arranged
for them. See DECISIONS' open ideas and cap.rs's note on the `Frame` object.
