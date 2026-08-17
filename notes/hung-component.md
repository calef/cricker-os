# The hung component

*Milestone 23's third residual, and the one §32 named and declined: a component that stops answering
**without dying**. The mechanism it interferes with is DECISIONS §41 and notes/live-replacement.md;
read those first if you want the swap itself. `crates/swap_proto`, `user/src/swapper.rs`'s
`ROLE_HUNG`, and `a_component_that_stops_answering_without_dying_is_invisible_to_its_supervisor` in
`kernel/src/user/live_swap_tests.rs`.*

## The problem, stated so the precision is usable

Every failure this system handles is a **death**. A component faults or exits; the kernel is the
witness and stamps a five-word message onto the supervision endpoint its spawner designated
(DECISIONS §26); the supervisor reads it, calls `Endpoint::REAP` with the tid the kernel put in the
message (§32), and the region comes home to the builder. That chain is complete, proven on both
architectures, and it is the whole of what a supervisor can do.

A component that **stops answering without dying** produces none of it. It holds its endpoint. It
holds its device. Its region is live and mapped. Its thread is `Blocked` or, if it is spinning,
`Running`. Nothing faults, nothing exits, no message arrives, there is no corpse, and every mechanism
in this tree reads that as a healthy server. Meanwhile a client is parked inside a `CALL` that will
not return, and its supervisor is blocked in `RECV` on an endpoint that will never deliver.

The tree already had the vocabulary for this and had never joined it up. DECISIONS §26.1 calls it
"alive but wedged" and says polling "remains the right tool" for it. §32's second consequence names
it exactly and declines it:

> **The honest limitation:** a supervisor that must restart a *hung* child (livelocked, not crashed,
> so no death message ever arrives) still needs the stronger right. That case is real, it is the
> watchdog case, and it is deliberately not solved here. When milestone 23's live replacement needs
> it, it is a new decision.

Milestone 23's live replacement needs it. This note is what a lane found out, which of the four
questions it could answer with the machine and which need calef, and one place where §32's sentence
above turns out to be **half wrong** in a way worth correcting on the record.

## Three hangs, not one, and they have different answers

This is the taxonomy the rest of the note runs on. It is not a classification exercise: the three
have different detectability and different terminal states, and treating them as one thing is what
makes "add a watchdog" sound like a single task.

| shape | what `SURVEY` sees | can the kernel tear it down today? |
|---|---|---|
| **(a) livelocked**, spinning in userspace | `READY` / `RUNNING`, always | **yes.** `Untyped::DESTROY` arms §16's kill and the scheduler converts the thread to a corpse at its next preemption |
| **(b) blocked on an endpoint whose region the supervisor can destroy** | `BLOCKED` | **yes, with collateral.** Destroying that region drains the endpoint's wait queues, aborts the blocked IPC with `abi::Error::Gone`, and the armed kill then lands |
| **(c) blocked on an endpoint the supervisor cannot reach** | `BLOCKED` | **no.** Nothing in the kernel can end it |

Case (c) is not speculation and it is not a gap somebody should close casually. `reap_region_objects`
says it in its own words, in the comment added on 2026-08-16 that fixed the (b) case:

> A server whose endpoints came out of the region being destroyed dies; **one blocked on somebody
> else's endpoint still does not**, and `reclaim_region`'s caller is told so by the refusal rather
> than by a hang.

The mechanism is worth spelling out because it is the load-bearing fact for question 4 below.
`Untyped::DESTROY` on a region holding a live thread does not fail passively: it marks the thread
`killed` and refuses, so the owner's retry reclaims a runaway (§16's amendment, §24's forcible `^C`).
The kill is **spent in `schedule()`**, at the top of the decision, and only for a thread whose state
is `Running`. A thread that is `Blocked` forever never reaches `schedule()` again. So the kill is
armed and never lands, the refusal is permanent, and the retry loop the shell's escalation performs
runs forever.

**Which means the "stronger right" §32 points at is not merely large for the purpose. For case (c) it
is insufficient.** A supervisor holding full construction authority over a hung child's region cannot
tear it down. There is no privilege that fixes this; it is a scheduler property.

## The four questions

### 1. Who notices, and with what authority?

**A watchdog holds `ENUMERATE` and nothing else, and there is no hole in the names-not-acts rule
because noticing and acting are separated into two processes rather than two rights in one.**

`abi::endpoint::SURVEY` (milestone 126) is the whole view: a cursor walk over the supervision
subtree, returning `(tid, state)` per member, gated on `Rights::ENUMERATE` and pointedly not `READ`,
because `READ` is what `RECV` and `REAP` take. calef's ruling on 2026-08-17 is that **a domain names
its members and does not act on them**.

A watchdog looks like the first thing that legitimately needs both halves, and it is not, for a
reason that survives being stated plainly: **a watchdog does not need to act. It needs to be
believed.** What it produces is a verdict about liveness. What acts on that verdict is the party that
already holds the authority to act, which is the *supervisor* (`READ` on the supervision endpoint, so
it may reap) or the *builder* (a region capability, so it may destroy). Both of those relationships
already exist and neither wants widening.

So the shape is three parties and no new authority anywhere:

```text
    watchdog  ──ENUMERATE──►  the domain          (may look; cannot reap, cannot kill, cannot serve)
        │
        │ a verdict, over an ordinary endpoint it holds WRITE on
        ▼
    supervisor ──READ──────►  the domain          (may reap a corpse; §32)
        │
        │ (for the terminal case only)
        ▼
    builder   ──region cap─►  the hung component  (may destroy; §16, §24)
```

Three properties of that arrangement are worth having on the record, because they are the reason it
is better than a watchdog that can kill:

- **The most dangerous program in the system does not exist.** A watchdog is the classic candidate
  for one, and this one holds `ENUMERATE` on one endpoint plus `WRITE` on one endpoint. A compromised
  one lies about liveness. It cannot end a process, cannot free memory, cannot answer a request, and
  cannot see a domain it was not handed.
- **A lie is refusable.** The supervisor is the one that acts, so it can apply policy the watchdog
  cannot express: restart at most N times, never restart the thing holding the only copy, escalate to
  a human. That is §40's "no reaper of last resort" holding rather than being worked around.
- **It is the same shape as `caretaker` and `dwarden`**, which is what makes it recognisable rather
  than novel: a program whose whole purpose is to hold less than the thing it stands in front of.

**Provisional name, and calef's call: `liveness_watch`.** `caretaker` and `undertaker` are spent on
capability-narrowing filesystem programs; `watchdog` alone is a hardware term and would claim a timer
this system does not have; the noun is `watch` and the thing watched is liveness. Nothing was minted
in this lane and no such program was built (see below).

### 2. What counts as hung?

**Not elapsed time. Unserved work.** The bound belongs to the supervisor, not to the manifest and not
to the client, and the reason is that a bound denominated in time can convict a healthy component of
being slow while a bound denominated in progress cannot.

Take the three candidate owners in turn, because the manifest answer is genuinely attractive and it
is wrong.

**The manifest's, and no.** `component_plan::Requirements` is where a deadline looks like it belongs:
one declaration, shipped with the contract, read by every supervisor. It fails on the tree's own
evidence. A contract cannot know the machine: the same `OP_PUT` under QEMU TCG, under HVF, and on a
VisionFive 2 differ by orders of magnitude (notes/cpu-models.md, milestone 59's matrix), and a
wall-clock number compiled into a `*_proto` crate would be a **shipped** version of exactly the
load-sensitive assertion milestones 62 and 78 exist to remove. It also fails the manifest's own test:
notes/component-manifest.md's line is that a manifest declares **authority**, and a deadline is not
authority, it is a service-level claim about a machine the declaration has never met. Fuchsia does not
put timeouts in a `.cm` either.

**The client's, and not alone.** The client is the party harmed, which is a real argument for its
deadline being the one that matters. But a client cannot act on it, so a client deadline is only ever
a report, and worse, a client on the default rung of the latency ladder is *already blocked* by the
time it would like to give up: there is no timed `CALL`, so it has no moment at which to notice.

**The supervisor's, denominated in progress.** This is milestone 62's argument, one level out, and
the fact that this tree has already made it about its own test harness is why it is a recommendation
rather than a preference. 62's block:

> **The heartbeat is bumped once per test, at the test's start**, and never while a test runs. So "no
> progress for 60 s" cannot distinguish a genuine deadlock from a test that is simply *slower* than
> 60 s. [...] **The fix**: a per-test *progress* heartbeat [...] so a slow test keeps the watchdog fed
> and a wedged one does not.

And its closing line, which is the general form: *"a bound expressed in something other than the
property under test."* A duration is that. A monotone count of work completed is not.

Concretely, and it needs nothing that does not exist: **a component publishes a monotone progress
counter in a page its supervisor owns.** §41 already has that page. `swap_proto::LOG_VA` is a frame
the operator retyped from its own budget and mapped read/write into every instance, and every instance
stamps it as it serves. Reading it costs the watcher **zero syscalls** and the component **one store**.
A verdict is then: *work is owed (a request went in and no counter moved) and the counter has not moved
across k observations.* No clock is consulted, and the number k is the supervisor's, set by somebody
who knows the machine it is running on.

Two honest costs, and the first is the one to design against:

- **The counter's granularity is the false-positive rate.** A component that bumps once per request
  looks hung throughout a single legitimately long request. One that bumps inside its work loop does
  not. This is 62's own finding and it does not go away by moving up a level; it becomes a property of
  the contract, which is at least a property somebody writes down.
- **Separating k observations still needs the watcher to wait**, and there is no timed wait. See the
  next paragraph, because this is the fork.

**What is genuinely missing, precisely.** Measuring a duration needs nothing: `user_rt::now()` is a
plain register read (`CNTVCT_EL0` / `rdtime`), ambient by design, so any process can time anything.
**Waiting** on one is what does not exist. There is no timed wait anywhere in the kernel; the syscall
surface is `EXIT`, `YIELD`, `INVOKE`, `CAP_DELETE`. So a watchdog today must yield-spin between
observations, which burns a core and makes its own timing load-sensitive, exactly as `net_stack`'s
retransmit window does (`wait_for_nic`, and milestone 106 records the price). **That is milestone
106's fork, and 106 says in its own words not to settle it by accident.** This lane did not.

**A citation to fix while passing through**, because it is the exact failure `script/decisions
--check` cannot catch: milestone 106's block attributes the fork's three candidate shapes to
"DECISIONS §51" twice, and §51 is *the sink protocol*. The three shapes are in **milestone 51's**
roadmap block (`design/roadmap/51-wall-clock-time.md`, its rejected-alternatives list). §N and
milestone N are colliding schemes, the gate proves only that a cited §N resolves to *some* section,
and a well-formed wrong citation is invisible to it. Corrected in 106's block by this lane.

### 3. What happens to the client that is already blocked?

**`abi::Error::Gone` does not cover it, and this is the answer with the sharpest edge.** A caller
killed mid-`CALL` is not merely inconvenienced; it is unwakeable and its memory is unreclaimable for
the life of the machine.

The path, from the code:

- `sched::ipc_call` parks the caller with `WaitRole::Reply` and says so at the site: *"We are NOT
  queued as a receiver; the Reply capability, which carries our tid, is the only thing that can wake
  us."*
- `sched::ipc_reply` is the only site that wakes such a thread, and it is addressed **by tid** rather
  than through an endpoint's wait queue.
- The abort machinery that produces `Gone` (`set_ipc_aborted`, and `reap_region_objects`'s endpoint
  sweep calling `drain_waiters`) reaches threads **on an endpoint's wait queues**. A caller whose
  request has been taken was popped off that queue at the rendezvous. It is not there.

So there are three ways a blocked caller ends, and a hung server is none of them: its endpoint is
destroyed (`Gone`), its server replies, or its server replies. `Gone`'s own doc comment is accurate
and the accuracy is the problem: *"the capability names an object that no longer exists."* A hung
server's endpoint exists. A hung server exists.

**And the operator cannot answer on the component's behalf.** The one-shot `Reply` capability naming
that caller is minted `WRITE` without `GRANT` (§12, and the syscall layer says why: "minted without
`GRANT`, so it could not have been delegated here in the first place"), it lives in the hung
component's cspace, and it is consumed on use. It cannot be delegated to the supervisor, forged, or
reached by revoking anything. **Freeing a stranded caller requires the cooperation of the component
whose lack of cooperation is the definition of the hang.**

The test demonstrates that inversion rather than asserting it. `swap_proto::NOTE_RELEASE` is the
operator's reply to the wedged instance, and the instance then uses the reply capability it took to
answer `WEDGE_RELEASED` to the caller it stranded. In the test the wedge is deliberate and
cooperates; a real one does not, and the `CL_WAS_RELEASED` bit exists so a reader can tell which of
the two happened.

**Consequences a design has to carry, not a footnote:**

- A caller stranded by a hung server holds a region that `Untyped::DESTROY` will refuse forever, for
  case (c)'s reason applied to the *caller*: it is `Blocked` and never reaches `schedule()`.
- So **one hang can cost two unreclaimable regions**, its own and its caller's, and a service with
  many concurrent callers costs one per caller in flight.
- This is not new with hangs. It is true of any server killed mid-`CALL`, including §24's forcible
  `^C` tier applied to a server. Nothing in the tree records that today. It is the most transferable
  finding in this note and it wants its own lane (see below).

### 4. Is killing even the right response?

**No, and the more useful statement is that killing is neither necessary nor sufficient.**

**Not necessary, for restoring the service.** This is where §32's sentence is half wrong, and the
test is the argument. Milestone 23's four steps against a hung incumbent:

```text
  1 BUILT    unchanged. Lay the replacement out, endow it with everything but the device, do not
             configure or start it.
  2 DRAINED  UNAVAILABLE, and unnecessary. OP_QUIESCE needs the incumbent to answer, which is the
             one thing it does not do. But quiescing exists to make the incumbent stop receiving,
             and a hung component has already stopped receiving. **The step that needs its
             cooperation is the step the hang makes redundant.**
  3 REVOKED  unchanged. Frame::REVOKE take-back (§41) is GRANT-gated on the operator's own device
             capability and asks the holder for nothing. It works on a live, wedged, wholly
             uncooperative holder exactly as on a quiesced one.
  4 STARTED  unchanged. The replacement parks in RECV_CAP on the stable endpoint and picks up
             whatever queued behind the silence, because the stable name is the endpoint object and
             the kernel's sender queue is the buffer (§41).
  5 REAPED   UNAVAILABLE. There is no corpse. Endpoint::REAP answers StillAlive.
```

So a supervisor recovers the **service** with no authority it did not already hold, and §32's
`Endpoint::REAP` was never in the path. What §32 is right about is step 5: **reclaiming a hung
component's memory** needs more than a supervisor holds. Restarting a service and reclaiming a
region are different acts, and the sentence conflates them.

The honest limit on that good news, and it is a real one: **recovery costs a fresh region while the
old one stays spoken for**, so a supervisor survives as many hangs as it has spare budget and no
more. That makes the reclamation question a capacity question rather than a tidiness one, which is
the strongest argument for deciding it.

**Not sufficient, for the terminal case.** Case (c) again: `Untyped::DESTROY` arms a kill that a
permanently `Blocked` thread never spends. Handing a watchdog the construction authority would buy
nothing for the shape that most needs it.

**And restarting is what the machinery already does.** §40's rule is that there is no reaper of last
resort, and the live-replacement mechanism is a restart with a stable name in front of it. A hung
component's replacement is the same four steps minus the one that needed cooperation. That is the
right response, and it is built.

## What this lane built, and what it deliberately did not

**Built: the case, demonstrated on both ISAs.** `swapper`'s `ROLE_HUNG` runs the direct channel's
system against an incumbent that swallows one request and stops answering. Four results, every one an
assertion about program order and **not one of them about elapsed time**:

1. **The domain does not report a hang.** `SURVEY` reports every member `BLOCKED` and none `DEAD`,
   which is byte for byte what a healthy idle system reads as: `abi::survey::BLOCKED` is the state of
   a server parked in `RECV_CAP`, and that is every healthy server between requests. The widest view
   a supervisor has cannot tell the difference, and no death message has arrived by then either.
2. **The supervisor's whole vocabulary is refused.** `Endpoint::REAP` is asked about *every* member of
   the domain and answers `StillAlive` every time. It is side-effect free by construction:
   `reap_supervised` decides `StillAlive` before it looks up a region.
3. **The service is restored with no new authority**, per the table above, and the drain step is
   asserted **absent** so a run that quietly quiesced cannot pass.
4. **The stranded caller is not restored by that**, and is freed only by the component that stranded
   it, after the service is already back. The two recoveries are separable and the report ordering
   proves it.

**Not built: a watchdog program.** Deliberately, and the reason is the ladder rather than time. Both
halves of one are behind decisions calef has not made: the detection threshold needs milestone 106's
timed wait (or a yield-spin that is the flaky assertion this project is currently removing), and the
terminal action needs §32's new decision. A program shipped with a made-up threshold would be policy
nobody ruled on, wearing a name nobody ratified, and the tree's rule is that a right defined for
hypothetical callers is speculative abstraction. What is shipped instead is the case, the taxonomy,
and the two asks.

**Not built: any kernel-surface change.** No new syscall, no new method, no new rights bit, no change
to `SURVEY` or `REAP`. That is worth saying because the obvious extension is tempting and is a
decision: see `BUGS`.

## Why the wedge is a `CALL`

Worth its own heading because a reader will otherwise reach for the simpler shape and reintroduce a
race the test was written to avoid.

The wedged instance announces itself with `call(NOTE, NOTE_WEDGED, served)` rather than `send`, and
the operator serves that one message with `RECV_CAP` and **keeps the reply capability, never using
it**. That is not a message-passing style choice; it is the hang. Three properties fall out:

- **The blocked state is provable rather than raced.** `sched::ipc_call` marks the caller `Blocked`
  inside the same critical section that wakes the receiver, so at the instant the operator's
  `RECV_CAP` returns, the instance is already parked. A `send` followed by a `recv` would leave a
  window in which the instance was still `Ready`, and a survey inside that window would read a state
  the assertion forbids. The test would then be measuring a scheduler race.
- **It is the commonest real hang.** Blocked awaiting a reply from a peer that will not answer is
  what a deadlock between two servers looks like from either side.
- **It gives the operator the only handle anything has on a wedged process**, which is what makes the
  cooperative release in result 4 expressible at all.

The wedge fires on the **identity of one request** (`WEDGE_SEQ`), not after a count of them. A count
would depend on how far the conversation had got when the operator was ready, which is a race; a
request identity happens in the same place on both architectures under any scheduling.

## EXAMPLES

**Read what a supervisor can see about a hung child.** The whole view, and its limit, in one
declaration:

```sh
grep -B4 -A30 'pub const SURVEY' crates/abi/src/lib.rs
```

**Run the case, on both architectures.**

```sh
script/test 2>&1 | grep -i 'stops answering'
script/cpu-matrix                     # the riscv64 leg
```

**Watch the domain of a running supervisor from a program that cannot touch it.** The pattern a
`liveness_watch` would use, and it is `ps`'s loop with the acting half absent:

```rust
let mut cursor = abi::survey::DONE;          // 0, which is also "start here"
loop {
    let (next, tid, state) = user_rt::survey(domain, cursor);
    if next < 0 { break }                    // refused: NOT an empty domain. Say so.
    let next = next as u64;
    if next == abi::survey::DONE || next <= cursor { break }
    // `state` is READY | RUNNING | BLOCKED | DEAD, and BLOCKED is where a hang hides.
    cursor = next;
}
```

**Endow it so it cannot act.** One right, and the emptiness of the rest is the claim:

```rust
sched::tcb_insert_cap(tid, cap::endpoint_cap(deaths, Rights::ENUMERATE), None)
```

`crates/system_initializer` already does exactly this for `ps`, with the reasoning beside it:
"`ENUMERATE` alone, deliberately. Granting `READ` here would hand a viewer [the authority] to
collect a child rather than to name one."

**Restore a service under a hung component**, which is the four steps minus one:

```rust
// 1 BUILT: the replacement, endowed without the device, unconfigured.
// 2 DRAINED: skipped. A component that is not receiving needs no quiesce.
// 3 REVOKED: take the device back from a holder that consents to nothing.
invoke(DEVICE, abi::frame::REVOKE, 0, 0, 0);
// 4 STARTED: map, configure, start. It drains what queued behind the silence.
```

## BUGS

**Detection is not demonstrated, only its impossibility with what exists.** The wedge announces
itself, and the note says so at every site: `swap_proto::NOTE_WEDGED`'s doc comment, the test's
header, and this file. What is under test is what a supervisor can do about a hang **it already knows
about**. A run in which something noticed a hang on its own is not in this tree and cannot be until
milestone 106's fork is decided, because every available notifier is a deadline and there is nothing
to wait on.

**`SURVEY` does not say what a thread is blocked *on*, so no wait-for graph is possible.** This is
the single extension that would turn detection from a heuristic into a decision procedure: a cycle in
the "waiting for" graph is a deadlock, provably and with no threshold at all (notes/deadlock.md,
Coffman condition 4, and Postgres's wait-for graph as the prior art). It is also a **kernel-surface
change to a method milestone 126's lane landed hours ago**, and it is arguably `ENUMERATE`-shaped
rather than `READ`-shaped, so it is a decision and not a task. Recorded here rather than attempted.
The cost of not having it: every liveness verdict in this system is a heuristic with a knob.

**A hang can cost two unreclaimable regions, and nothing else in the tree records this.** The hung
component's, and the caller's if a request was in flight. Both are `Blocked` and neither ever reaches
`schedule()` to spend the kill `Untyped::DESTROY` arms, so the refusal is permanent, and the retry
loop in the shell's `^C` escalation would run forever. This is **not specific to hangs**: it is true
of any server torn down mid-`CALL`. It wants its own lane.

**The shared witness page cannot be taken back from a hung holder.** `Frame::REVOKE` on a
`DeviceFrame` is take-back and spares the invoker; on an ordinary `Frame` it is symmetric and takes
the page from the invoker too (§13, §41, and the asymmetry is deliberate). So an operator cannot
un-share a writable page from a component it no longer trusts without losing its own view of it. A
hung component that later wakes can therefore still scribble on the supervisor's witness page. The
test does not hit this because its wedge writes nothing after waking, which is a property of the
fixture and not of the mechanism. See notes/shared-page-audit.md.

**The queue rung does not protect against a hang, and it looks as though it should.** `broker`'s own
comment is honest about the reason: it "does not probe the backend and cannot: liveness is exactly the
thing a synchronous rendezvous cannot report on". In pass-through it *calls* the backend, so a hung
backend hangs the broker, which then stops receiving on its front endpoint, and producers park on the
kernel's sender queue where nothing can count them. Rung 1 decouples a **scheduled** absence (the
operator said `BOP_DOWN`) and not an unscheduled one. `QUEUE_FULL` looked like a clock-free "this has
gone on too long" signal and is not reachable in the hung case.

**The three hang shapes are not all tested.** The test exercises case (b)/(c)'s observable half: a
component blocked on an endpoint, indistinguishable from healthy. Case (a), the spinner, is
**distinguishable** by `SURVEY` alone in principle (a healthy request/reply server is `BLOCKED`
between requests and a spinner is never `BLOCKED`), and is not tested here, because establishing
"never" needs repeated sampling and that is the load-sensitive assertion this lane refused to write.
It is the easiest of the three to detect and the easiest to kill; a lane that gets a bound it may use
should take it first.

**Every name in this residual is provisional.** `wedge`, `WEDGE_SEQ`, `WEDGE_RELEASED`, `NOTE_WEDGED`,
`NOTE_RELEASE`, `ROLE_HUNG`, `RPT_SURVEY`, `RPT_UNCOLLECTABLE`, `RPT_WEDGED`, `survey_counts`, and the
proposed `liveness_watch`. `wedge` is at least the word this tree already uses for the condition
(§26's "alive but wedged", milestone 62's "a wedged one"). `caretaker` and `undertaker` were
unavailable: both are spent on capability-narrowing programs.

**§32 and §41 both carry a sentence this lane's evidence contradicts**, and a lane may not edit
`design/decisions/`. §32's consequence 2 says a supervisor restarting a hung child "still needs the
stronger right"; §41's last bullet says a livelocked instance "would need the stronger right, which is
§32's recorded watchdog case". Both are right about reclaiming the component's memory and wrong about
restarting its service, and the four-step table above is the correction. The integrator owns the edit.

## What is left of milestone 23

Two residuals, and this lane changed the shape of neither, but it did narrow what one of them costs.

**State handoff** is unchanged and is still the crux: a serialise-old / absorb-new protocol over a
supervisor-brokered channel, which is a wire format and so calef's. What this lane adds is a reason it
is *harder* than the block says. A hung component cannot be asked to serialise anything, so a state
handoff protocol that only works when the outgoing instance cooperates recovers a planned swap and not
a failure, which is the case it is most wanted for. Erlang/OTP's `code_change` has the same shape and
the same hole.

**Dependency-aware orchestration** is unchanged and still needs a manifest extension first: a
component declares what it needs, not that another component supplies it, so there is no dependency
graph. This lane touches it at one point worth recording: the quiescence protocol that orchestration
needs is exactly the step a hang makes unavailable, so a dependency-aware supervisor needs a
non-cooperative fallback for every edge in the graph, not just for the component it is replacing.

## See also

- DECISIONS §32 (a supervisor may collect a corpse without being able to build one), whose consequence
  2 is where this case was named and declined; §41 (the endpoint is the broker), §26 (the fault
  endpoint), whose sub-decision 1 calls this "alive but wedged"; §12 (a one-shot reply capability),
  §13 and §16 (revocation), where the armed kill lives; §24 (interrupting the foreground process),
  the forcible `^C` tier; §40 (there is no reaper of last resort)
- notes/live-replacement.md for the swap itself and its two witnesses; notes/component-manifest.md for
  why a deadline is not a manifest field; notes/process-view.md for `SURVEY` and `ENUMERATE`
- notes/deadlock.md for the four Coffman conditions and why detection means killing somebody;
  notes/supervision.md, notes/teardown.md, notes/sink-protocol.md (`abi::Error::Gone`)
- design/roadmap/106-deadline-wait.md and design/roadmap/51-wall-clock-time.md for the timed-wait fork
  and its three candidate shapes, which five consumers now want;
  design/roadmap/62-time-sensitive-tests.md for the progress-versus-duration argument this note
  borrows, and 78 for the assertions it is currently removing
- Prior art: MINIX 3's reincarnation server (which does poll, with a per-service timeout an
  administrator sets), QNX's high-availability manager, Erlang/OTP supervisors, and the Linux kernel's
  `hung_task` detector, which reports and never acts, for the same reason recommended here
