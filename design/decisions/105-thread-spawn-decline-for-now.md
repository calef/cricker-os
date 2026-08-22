# 105. `std::thread::spawn` stays declined, until a customer needs it

**Status: DECIDED.** calef, 2026-08-22, on a milestone 64 lane's write-up (`notes/thread-spawn-fork.md`,
pull request #394): option C, decline. *"I don't know of a customer for A yet. We will likely do A
when there is such a customer."*

## The question

Milestone 64's rank-3 gap: `std::thread::spawn` returns `Unsupported` on nife unconditionally
(`patches/std-nife/overlay/std/src/sys/thread/nife.rs`), not because anything is missing but because
`Tcb::CONFIGURE` **consumes** the address-space capability it binds and `kernel/src/thread.rs` owns
that `AddressSpace` outright for automatic teardown. No two TCBs can share one address space today;
that is the existing contract, not an oversight. `rayon`, `crossbeam-channel`, `tokio` and `ignore`
all compile and link against the PAL already: nothing is blocked on code, everything was blocked on
whether nife should build real shared-memory threads at all, and if so, how.

The lane costed three options rather than building any of them, per this project's rule that the
syscall surface is calef's call:

- **Option A, shared VSpace (seL4-style).** Change `CONFIGURE`'s contract so an aspace capability
  can be bound without being consumed, and extend `AddressSpace` with the same multi-holder liveness
  tracking `Endpoint` already has (§16). The correct design, and the expensive one: it changes what
  an *existing* syscall method promises, which every future program written against `CONFIGURE`
  would inherit.
- **Option B, sibling processes with replicated frames.** Touches no syscall at all. Looked cheap
  going in; measured out not cheap. A fixed shared arena is free (the same pattern `net_stack` and
  the compositor already use), but a *growing* heap kept consistent across independent address
  spaces needs a from-scratch, race-free synchronization protocol that no surveyed prior art (seL4,
  Zircon, Linux `pthread_create`) implements for the general case, because all three instead make
  "one address space, many schedulable things" the definition rather than something userspace
  reconstructs by hand.
- **Option C, decline.** `thread::spawn` stays `Unsupported`, permanently rather than "phase one,
  pending." Zero kernel cost, fully reversible: nothing is built, nothing is promised, and neither A
  nor B is foreclosed later.

## The decision

**Option C.** No customer for real shared-memory threads exists today. The only cited want is
milestone 149's Rayon-parallel NPB-Rust variants, and the lane's own note is honest that this is
"useful evidence, not a paying workload." Nothing on the customer path (milestone 54/55) needs
shared-memory threading; that work is IPC-and-process-shaped already, which nife fully supports.

This is the *move fast on what can be undone; be methodical on what cannot* tenet applied directly:
A is the irreversible, hard-to-undo option, being weighed against zero current demand. Declining now
costs nothing and forecloses nothing. When a real consumer needs shared-memory `std::thread::spawn`,
build A: it is seL4-precedented, and `notes/thread-spawn-fork.md` is the costing to start from.

**What this does not decide.** It does not rule out Option A forever, and it does not pick between A
and B as a matter of principle if the calculus changes; it says today's answer is "no customer, so no
cost paid yet." Milestone 149's Rayon-parallel variants stay permanently out of scope rather than
pending, recorded where a reader meets that milestone.

## What it unblocks

Milestone 64 can now record `thread::spawn` as a closed scope decision (a `BUGS`/scope-note entry,
not an open gap) rather than a still-open rank in its worklist. Milestone 149's roadmap row already
names the dependency correctly ("the Rayon-parallel variants gate on milestone 64's still-open
`thread::spawn` fork"); that gate is now answered as **out of scope for now**, not resolved into a
build.
