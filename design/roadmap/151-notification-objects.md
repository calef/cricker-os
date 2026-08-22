# 151. Notification objects: async multiplexing without wait-any

**Status: NOT-STARTED.** Minted 2026-08-22, from DECISIONS §101's own sequencing (step 2), which
specified this milestone's shape and scope without a number: *"a kernel milestone, one lane,
estimated from the existing `Endpoint` and `signal()` work at the same scale as milestone 19a."*
This is that number.

**Gate: NONE.** §101 already decided the design (2026-08-20, `design/decisions/101-notification-objects.md`).
Nothing here is a fork; it is the kernel-side build the decision already specified.

## Why this exists

**A concrete, waiting bug forces it: milestone 40's `terminal_sink_caretaker` narrowing (§101 step 1,
DECISIONS §106) can outrun its own completion signal.** A child's exit is delivered cleanly today
via §26's fault/exit endpoint, but the caretaker's own trailing delivery to `line_editor` is a
second, independent `CALL` with no ordering primitive against the shell printing its next prompt.
`notes/tail-output-narrowing.md` names this precisely: a page's last line and the next `$ ` can
interleave under contention. A display glitch, not a confinement or correctness failure, and it is
being carried as a documented `BUGS` entry on milestone 40 until this milestone lands.

Three other consumers are already named and waiting on the same primitive (§101's own table):
the shell multiplexing child-output-or-exit into one wait, the compositor's per-client wakeup, the
FS server's async-event notification, and the network stack retiring its yield-and-re-poll spin
(milestone 106's current interim).

## What to build

Exactly §101's spec, no re-derivation needed:

- **A new `objtype::NOTIFICATION` (= 4)**, holding a data word and a wait queue, plus at most one
  bound TCB (`Option<Tid>`, set by `BIND`).
- **Four methods under the existing `SYS_INVOKE` surface** (no new syscall number): `SIGNAL`
  (async, non-rendezvous, never lost), `WAIT` (blocks until a signal arrives, returns the word),
  `POLL` (non-blocking `WAIT`), `BIND` (attach a notification to a TCB, at most one).
- **The binding mechanism**: when a TCB with a bound notification is blocked in `RECV` on an
  endpoint, `SIGNAL` on that notification wakes it the same way a message would, and the woken
  thread can tell which happened. This is the multiplexing primitive: `RECV`-on-endpoint-or-signal
  in one wait point, without a wait-any/port-set container (§101's "Why not the alternatives"
  section already rejects that shape and explains why the simpler design suffices).
- **Kani proof** of the object's state machine, matching the coverage `Endpoint` already has.

**Explicitly out of scope**, per §101: badged capabilities (a separate, later decision already
named in notes/supervision.md, notes/compositor.md, notes/dir-capability.md) and timed wait
(milestone 106, orthogonal: `WAIT` blocks indefinitely here exactly as `RECV` does today).

## What it unblocks

- **Closes milestone 40's caretaker-hop race** (its `BUGS` entry converts to: the shell binds a
  notification to its own TCB and `WAIT`s on "the caretaker's queue for this client has drained"
  instead of guessing, per §101's retrofit step.
- **The shell's exit-wait hack retires**, replaced by a proper multiplexed wait (§101 step 3).
- **The compositor, FS server, and network stack** can each take the retrofit named in §101's table,
  as their own follow-on work; this milestone builds the primitive, not their consumers.

## BUGS

- **Not yet measured against `Endpoint`'s own proof effort or milestone 19a's actual size.** §101's
  estimate ("same scale as 19a") is a comparison, not a number; the first lane to scope this should
  measure rather than trust the estimate.
