# 133. Ending a permanently blocked thread, and deciding who may

**Status: NOT-STARTED.** Proposed 2026-08-17 by the research lane
`roadmap/blocked-thread-teardown`, from the residual milestone 23's hung-component lane recorded and
declined to open. **The number 131 is PROVISIONAL**, and the hazard is not hypothetical: this lane first wrote 130 and
found `feature/130-code-smells` already holding it, by listing remote branches before reporting. The
integrator mints the real one at merge, per the rule that anything global to the tree is assigned
there and never claimed by a lane.

**Gate: DECISION.** Four proposals are laid out in notes/blocked-thread-teardown.md with their costs,
their authorities and their failure modes, and no lane may pick one. The fork is calef's because it
is a capability question before it is a scheduler question: **which held capability expresses the
right to end a thread**, and the candidates (a region capability, a `Tcb` capability, a supervision
endpoint, a new right) each concede something different. Proposal B would also add a method to the
syscall surface, which §10 makes a design fork on its own.

**In brief.** `Untyped::DESTROY` on a region holding a live thread marks the thread `killed` and
refuses, so the owner's retry reclaims a runaway (§16's amendment, §24's forcible `^C`). The kill is
spent at the top of `schedule()` and only for a thread whose state is `Running`. A permanently
`Blocked` thread never becomes `current` again, so the kill is armed and never lands, the refusal is
permanent, and the region is unreclaimable for the life of the machine. **No privilege fixes it; it
is a scheduler property**, which is why §32's "stronger right" is not merely large for this purpose
but insufficient. `reap_region_objects` records the fact in its own comment, and notes/hung-component.md
is where it was first stated as a taxonomy: case (c), a thread blocked on an endpoint its supervisor
cannot reach.

The cost is a capacity cost rather than a tidiness one, and it compounds. A hung server strands its
callers, and a caller stranded mid-`CALL` is itself permanently `Blocked` in its own region, so **one
hang can cost two unreclaimable regions and a service with many callers in flight costs one per
caller.** The number of hangs the system survives is therefore a function of spare budget, which is
the wrong shape for milestone 55's unattended backup target.

**A second, separable defect the research turned up.** `abi::Error::Gone` does not reach a
reply-parked caller. The abort machinery walks an endpoint's wait queues, and a caller whose request
was taken was popped off at the rendezvous; it is woken by `sched::ipc_reply` and by nothing else. So
a caller stranded by a server that merely *died* is stranded too, which QNX Neutrino has not permitted
since the 1990s ("If the server thread fails, exits, or disappears, the client thread becomes READY,
with MsgSend() indicating an error"). Nothing in the tree records this as intended. It looks less like
a fork than the rest of this milestone and may want to be split off.

**And a hazard whichever way the fork goes.** `cap::reply_cap` mints `Object::Reply(tid)`, whose
payload is a generational thread name with no call identity, and `ipc_reply`'s guard checks the
`WaitRole` and discards the endpoint. That is sound only because nothing can currently leave a reply
park and enter a second `CALL` while an unconsumed `Reply` names it. **Any proposal that wakes a
reply-parked caller creates exactly that path**, and a hung server's stale reply capability would then
forge an answer to a later, unrelated call. L4Re documents the identical hazard as a consequence of
its own finite receive timeouts. The two known fixes are seL4's: delete the outstanding reply
capability at abort (a cspace sweep, the pattern `sched::delete_frame_caps` already establishes), or
do not wake the victim at all.

**What the research established, and it reframes the work.** The mechanism is close to free and the
authority is the whole problem. `thread::WaitRole` is a closed enumeration of three places a blocked
thread can be (an endpoint's sender queue, its receiver queue, or no queue at all for a `CALL`
caller); `handshake.wait_on` names the endpoint; `set_ipc_aborted` plus `wake` is the existing
abort-and-resume pair; `Endpoint::remove_sender` already exists for surgical unlinking and its
receiver twin is the same twelve lines; and `reap_region_objects` already reaches into an endpoint
*outside* the region being destroyed, with its reasoning written down. So the kernel could end any
blocked thread today in about thirty lines with no new syscall. **What has never been decided is who
may ask, and that is the milestone.**

**The prior art says nobody else blocks forever.** seL4 folds cancellation into `seL4_TCB_Suspend`
(`suspend()` is `cancelIPC` + dequeue + `ThreadState_Inactive`, authorized by a TCB capability, and
the victim is never handed an error at all). L4 makes it a flag on `ex_regs`. Mach splits
`thread_abort` from `thread_abort_safely` on exactly the hazard of interrupting a non-restartable
operation. Zircon **removed** thread killing in RFC-0007 and answers a blocked `zx_channel_call` by
closing the channel, with `ZX_RIGHT_DESTROY` on a process or job handle as the authority. Linux had to
invent `TASK_KILLABLE` because two states were a false choice. nife is currently alone in having no
way out, and it is alone by accident rather than by decision.

**Deliberately in scope: refusing.** Proposal D in the note is "accept the leak, bound it with the
`QuotaToken` machinery that already holds a spawner's slot for precisely this case, make it visible,
and recover the service rather than the memory". It is argued rather than listed. §40 is "no reaper of
last resort", Fuchsia deleted this feature after shipping it, and a capacity failure is a visible
refusal where a forcible teardown's failure is a broken invariant inside a component that was still
holding something. **If this milestone ends as `RECORDED`, that is a result.**

**What is not in scope.** Detection. Deciding that a component *is* hung needs milestone 106's timed
wait and is milestone 23's residual, not this one's; this milestone is about what can be done to a
thread already known to be stuck, whether it is stuck because a peer hung, because a peer died, or
because it was written wrong.
