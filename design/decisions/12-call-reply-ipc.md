# 12. Call/Reply IPC: a one-shot reply capability

**Status: DECIDED.**

Decided and built 2026-07-22 (milestone 12). The design was worked out ahead of time in
notes/ipc-naming.md and parked in "Open design ideas" against two triggers. This is where it lands,
because it widens the §4 syscall boundary and so is owed a numbered decision.

## The gap it closes

IPC names an endpoint and the sender is anonymous (notes/ipc-naming.md), so a server that `RECV`s a
request cannot reply to the *specific* caller. The workaround was a second reply endpoint wired per
client at spawn, correct only while a server's client set is static and it is single-threaded (the
console server). It does not serve anonymous clients, and nothing structural stops a misrouted reply,
a double reply, or a stale reply landing on a client that has moved on.

## The surface, and why it is this small

One new endpoint method and one new object. The syscall count stays at three (exit/yield/invoke).

- **`CALL`** (endpoint method): send two words and block until replied. At the rendezvous the kernel
  mints a one-shot `Reply` capability naming *this* caller and delivers it to the server through the
  existing `RECV_CAP` (x1 = the reply slot, x2 = the second word). Needs `WRITE`, like `SEND`.
- **`Reply`** (a capability object, `Object::Reply(Tid)`): kernel-minted, naming the blocked caller.
  Invoking it (`REPLY`) delivers the answer, wakes the caller, and **consumes the capability**. Minted
  `WRITE`-only and without `GRANT`, so it is non-transferable as well as single-use.

The server side reuses `RECV_CAP` rather than growing a new receive method: receiving a call looks
exactly like receiving a delegated capability, plus a second data word. The one asymmetry with `SEND`
is honest and worth stating: a `CALL` carries two words, not three, because the third register holds
the reply handle. That is fine under §10's rule that IPC carries control and bulk moves by frame.

## What it buys, as kernel guarantees rather than server discipline

1. **Reply to an anonymous caller, no pre-wiring**: the kernel mints the cap; the server never knew
   the caller.
2. **One-shot**: consumed on use, so a second reply is `NoSuchSlot`. No double reply, no hoarding.
3. **This caller, not another**: `Reply(Tid)` names the exact blocked caller; misrouting is
   unrepresentable.

Three tests hold the line: `a_call_gets_a_reply` (round trip, one endpoint), `a_reply_reaches_the_
caller_that_called` (two callers outstanding, each gets its own reply), and, through the real syscall
path at EL0, `a_process_calls_a_server_and_the_reply_is_one_shot` (which also checks that the second
reply is refused).

## Deferred, deliberately

- **The call chain and priority donation.** seL4's Reply cap also threads a kernel call chain so the
  server runs on the caller's priority. cricker-os is round-robin with no priorities, so donation is
  moot; building the chain now would be machinery with no consumer (§4). It is the natural extension
  when priorities arrive.
- **Timeouts.** A server that never replies (or whose cspace is full, so the reply cap is dropped)
  leaves the caller blocked until torn down, the same no-timeout limitation as any lost reply today.

## One rule the mechanism assumes

A `CALL` endpoint is served with `RECV_CAP`. A plain `RECV` cannot furnish the reply capability, so
the kernel delivers the words but leaves the caller blocked rather than wake it with its own request
masquerading as a reply. Servers use the right method by protocol; the guard is there so misuse hangs
(bounded by the no-timeout gap above) rather than mis-serves.
