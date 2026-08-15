# 32. A supervisor may collect a corpse without being able to build one

**Status: DECIDED.**

**Decided 2026-07-29 (calef).** Reaping a dead child stops requiring the authority to construct
one. The supervision relationship, not the memory, becomes the unit of authority.

## The problem, measured rather than anticipated

Reaping is §16's `Untyped::DESTROY`, which requires `WRITE` on the region capability, and `WRITE`
is also what builds a process out of that region. So a supervisor whose entire job is "notice
`net_stack` died, restart it" has to hold the authority to construct arbitrary threads and address
spaces. That is a large right granted for a small purpose, and it is backwards for a capability
system: a compromised supervisor should be able to restart what it supervises and nothing else.

This was a prediction until milestone 36 made it a measurement. Its `c_confiner` had to hold a
full-rights untyped budget for its whole life (`SPLIT`, `RETYPE`, `RETYPE_OBJ`, `DESTROY`) because
it needed the last one; everything else came attached. What it wanted was `DESTROY` on one region
it did not create: not `RETYPE`, not `SPLIT`, not a budget. Recorded in §31.

## The decision

**A new method on the supervision endpoint capability, authorized by the supervision relationship
the kernel already tracks.** A supervisor invokes it on the endpoint it already holds, naming the
tid the kernel stamped on the death message. The kernel authorizes it by checking that the named
thread's recorded `fault_ep` (§26 implementation note 1) *is* the endpoint being invoked, and that
the thread is already dead. Then it reaps: TCB, address space, and the region behind them, exactly
what `Untyped::DESTROY` would have reclaimed.

Four consequences, each deliberate:

1. **The supervisor holds no region capability and gains no memory authority.** The reclaimed
   region returns to its owner under §13 region ownership, which is the builder, not the reaper.
   A supervisor can free a child's memory; it cannot spend it. That separation is the whole point,
   and it means builder and supervisor can be different processes without the supervisor
   accumulating the builder's rights.
2. **It authorizes collecting a corpse, not killing.** The method refuses a thread that is still
   alive. Killing a live child is strictly more dangerous than collecting a dead one, and it
   already has a home: §24's forcible `^C` tier uses `Untyped::DESTROY`, which needs the
   construction authority precisely because it is the stronger act. **The honest limitation:** a
   supervisor that must restart a *hung* child (livelocked, not crashed, so no death message ever
   arrives) still needs the stronger right. That case is real, it is the watchdog case, and it is
   deliberately not solved here. When milestone 23's live replacement needs it, it is a new
   decision, and the SUSPEND tracker is where the resumable half of it already lives.
3. **It settles the queued tid-to-handle question for this case, and only this case.** The second
   fork raised alongside this one was how a supervisor names a child: a `Tcb::NAME` method,
   per-child fault endpoints, or a builder-reported tid. None is needed here, because the tid is
   authorized *relative to the endpoint it arrived on*. That is the endpoint-only naming discipline
   applied consistently: the name means something only to the holder of the capability it came
   through, and it is not a global handle. If some other operation later needs to name a child, it
   is a fresh decision and should reach for the same shape first.
4. **It is a new method, not a new syscall number, and not a new capability type.** Per the
   project rule that keeps the surface a boundary rather than a habit, it is recorded here with its
   semantics before it is built.

## The refinement I made to calef's ratification, stated so he can object

He approved putting the right on the child's fault endpoint rather than on a rights bit, on the
argument that the supervision relationship should be the unit of authority. I described it at the
time as an `Untyped::REAP` method gated on the fault endpoint. Designing it, hanging the method off
**the endpoint** rather than off `Untyped` is the better placement for the same reason: an
`Untyped` method has to name a region, and the entire premise is that the supervisor does not hold
one. So the invocation moves to the capability the supervisor actually has. Same principle, one
surface less. This is a placement change inside the ratified direction, not a second fork, and the
alternative is recorded here in case it reads otherwise.

## Alternatives rejected

- **A reap-only rights bit derived from the untyped at spawn time.** Cheap and fits the existing
  rights machinery, but it says "you may free memory" and then requires the kernel to work out
  *which* memory, which is the same coupling with an extra indirection. It also keeps the region
  capability in the supervisor's hands, which is the thing being removed.
- **Leaving it on construction authority until milestone 23 forces it.** Defensible, and rejected
  because 23 is the flagship and this is not work to be designing under that deadline. Milestone
  36 having already hit it in anger is the argument against waiting for a third instance.
