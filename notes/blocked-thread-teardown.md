# Ending a permanently blocked thread

*Research, not a decision. This note starts where notes/hung-component.md stopped: its case (c), a
thread `Blocked` on an endpoint its supervisor cannot reach, which nothing in this kernel can end.
Read that note first for the taxonomy and for why the supervisor's whole vocabulary is refused. This
one surveys how seven other systems solve the same problem, maps each onto this kernel, and lays four
proposals side by side. **calef takes this fork; nothing here is a recommendation of a winner.***

*The code this note reads: `kernel/src/sched.rs` (`schedule`, `ipc_call`, `ipc_reply`,
`set_ipc_aborted`, `reap_region_objects`), `crates/ipc/src/lib.rs` (`drain_waiters`,
`remove_sender`), `crates/wake_handshake/src/lib.rs` (`park`, `abort`, `try_wake`), and
`kernel/src/thread.rs`'s `WaitRole`.*

## The problem in one paragraph

`Untyped::DESTROY` on a region holding a live thread does not fail passively: it marks the thread
`killed` and refuses, so the owner's retry reclaims a runaway (§16's amendment, §24's forcible `^C`).
The kill is spent at the top of `schedule()`, and only for a thread whose state is `Running`:

```rust
if let Some(t) = sched.threads.get_mut(current)
    && t.killed
    && t.handshake.state == State::Running
{
    t.handshake.state = State::Finished;
}
```

A permanently `Blocked` thread never becomes `current` again, so the kill is armed and never lands
and the refusal is permanent. The region is unreclaimable for the life of the machine, and so is the
region of every caller that thread stranded. **No privilege fixes this. It is a scheduler property,
not an authorization one**, which is why §32's "stronger right" is not merely large for the purpose
but insufficient.

For a backup target meant to run unattended for months, the shape of the risk is that the number of
hangs the system survives is a function of spare budget.

## What the machine already has, which reframes the question

The most useful finding in this lane is not from the prior art. It is that **the mechanism is nearly
free and the authority is the whole problem**, and that is the opposite of how the question reads
from outside.

### Every blocked thread is in exactly one of three places, and the kernel knows which

`thread::WaitRole` is a closed enumeration of three, written by the same `SCHED`-held statement that
writes `Blocked`:

| `WaitRole` | Where the TCB is linked | Reached by |
|---|---|---|
| `Sender` | the endpoint's sender `Fifo` | `Endpoint::recv`, `drain_waiters`, `remove_sender` |
| `Receiver` | the endpoint's receiver `Fifo` | `Endpoint::send`, `Endpoint::signal`, `drain_waiters` |
| `Reply` | **no queue at all** | `sched::ipc_reply`, addressed by tid |

There is no fourth. An `Irq::WAIT` is `sched::ipc_recv` on a routed endpoint, so it is a `Receiver`.
`handshake.wait_on` carries `(EpId, WaitRole)` and `endpoint_of` resolves the `EpId`, so from a bare
`Tid` the kernel can already say exactly which queue, if any, holds that thread.

### The abort-and-resume pair already exists, and is already used

```rust
set_ipc_aborted(sched, tid);   // handshake.abort(): opens the undelivered-wake gate
wake(sched, tid);              // Blocked -> Ready, onto a run queue
```

Those two lines are what `reap_region_objects` runs against every waiter it drains from a doomed
endpoint. The syscall layer then reads the flag through `take_ipc_aborted` and hands userspace
`abi::Error::Gone`. **The abort path is complete; it is only ever entered from an endpoint's wait
queue**, which is the whole of `Error::Gone`'s reach.

### The one primitive that is missing is twelve lines

`Endpoint::remove_sender` already exists, and it exists for exactly this shape of problem: a corpse
parked on its supervisor's sender queue must be unlinked before its TCB is freed. Its doc calls it
"the one operation an intrusive `Fifo` deliberately does not offer (arbitrary remove)". There is **no
`remove_receiver`**, and it is the same drain-and-repush over the other `Fifo`.

For `WaitRole::Reply` no unlink is needed at all, because the thread is on no queue. Mechanically the
case the hung-component note frames as hardest is the easiest one.

### And the precedent for reaching outside the region is already settled

The objection that suggests itself is that ending a thread blocked on somebody else's endpoint means
mutating an endpoint the destroyer does not own. The removal phase of `reap_region_objects` already
does that, deliberately and with its reasoning written down: a corpse parked on its *supervisor's*
endpoint, which "is not in this region and the endpoint sweep above did not touch it", is unlinked
with `remove_sender` before its TCB is freed. Endpoint wait queues are kernel state under `SCHED`,
not the endpoint owner's property, and the tree has already decided that once.

**So: the kernel could end any blocked thread today, in about thirty lines, with no new syscall.**
Every proposal below differs in who may ask and in what the victim and its peers observe, not in
whether it can be done.

## A hazard any proposal must solve: the stale reply capability

This is a live correctness bug that no current code path can reach, and **the first proposal that
wakes a reply-parked caller reaches it.**

`cap::reply_cap` mints `Object::Reply(tid)`. The payload is a generational `Tid` and nothing else:
there is no call sequence number, no endpoint, no nonce. `slots` bumps a slot's generation when the
*thread* is removed, not per call.

`ipc_reply`'s guard, the one boot 8 added, checks the **role and not the call**:

```rust
if !matches!(t.handshake.wait_on, Some((_, WaitRole::Reply))) {
    return;
}
```

The `_` is the endpoint, and it is discarded. Today this is sound, and the reason is worth writing out
because it is what an abort would invalidate. There are three ways out of a reply park, and **no
server holds an unconsumed `Reply(tid)` at the end of any of them**:

- **The reply arrives.** `abi::reply::REPLY` calls `ipc_reply` and then `delete_current_cap(slot)`,
  "so a second reply is `NoSuchSlot`". The cap is gone whether or not the delivery landed.
- **The caller is drained off a sender queue.** A `CALL` that met no receiver leaves the caller
  *both* Reply-parked and queued as a sender, with the reply cap riding in `outgoing_cap` rather than
  in anyone's cspace, so `drain_waiters` reaches it and `Gone` does resolve. **No server ever held
  that cap**, because no server ever collected the message.
- **The caller dies.** The `slots` generation bumps and every `Reply(tid)` naming it goes stale on its
  next use.

The dangerous shape is the one no path produces: a caller that resumes from a `CALL` whose request
*was* collected, so a server holds the cap, and then makes a second call.

**An abort creates that path.** Free a caller stranded by a hung server, hand it `Gone`, let it call
a healthy server instead, and the hung server still holds a live `Reply(tid)`. Its next invocation
passes the role check, clobbers the caller's mailbox, and wakes it with a forged reply belonging to a
different conversation. A merely hung server never does this; a compromised or confused one does, and
the confinement claims in this tree are written against the second.

L4Re documents this exact hazard as a consequence of its own finite receive timeouts, which is the
strongest corroboration available that it is real rather than theoretical:

> Special care is required if a finite timeout for the receive phase of an IPC call is specified: The
> IPC receive operation could abort before the partner was able to send the reply message. Under
> certain circumstances the partner may still have the temporary reply capability to the calling
> thread and may use this capability to reply to the caller at a later, unexpected time specifying an
> arbitrary IPC label. This case is relevant for servers which call another, possibly untrusted,
> server while serving a client request.
>
> Source: [L4Re, *L4 Inter-Process Communication (IPC)*](https://l4re.org/doc/l4re_concepts_ipc.html), IPC
> Timeouts (fetched 2026-08-17)

Two fixes exist, and both have prior art below:

- **Delete the outstanding reply capability at abort**, sweeping every cspace for `Object::Reply(tid)`
  the way `sched::delete_frame_caps` sweeps for `Object::Frame(phys)`. This is seL4 non-MCS's
  `cteDeleteOne(callerCap)`, reached from `cancelIPC`. Cost is O(threads x 16 slots) on a teardown
  path, which is 2048 comparisons at `MAX_THREADS = 128` and `CSPACE_SLOTS = 16`.
- **Do not wake the caller at all**, so no second call can exist. This is seL4's `ThreadState_Inactive`
  and is discussed under proposal A.

## Prior art

Seven systems, in the order that makes the argument. Every quote below was fetched and read in this
lane on 2026-08-17; where a claim is from memory rather than a source, it says so.

### seL4: suspension is cancellation, and the authority is a TCB capability

The closest relative, and the most instructive, because seL4 does not have a separate "abort an IPC"
operation at all. It has `seL4_TCB_Suspend`, and cancellation falls out of it. From the kernel
source, verbatim:

```c
void suspend(tcb_t *target)
{
    cancelIPC(target);
    if (thread_state_get_tsType(target->tcbState) == ThreadState_Running) {
        updateRestartPC(target);
    }
    tcbSchedDequeue(target);
#ifdef CONFIG_KERNEL_MCS
    tcbReleaseRemove(target);
    schedContext_cancelYieldTo(target);
#endif
    setThreadState(target, ThreadState_Inactive);
}
```

Source: [`seL4/src/kernel/thread.c`](https://raw.githubusercontent.com/seL4/seL4/master/src/kernel/thread.c)

`cancelIPC` is the closed enumeration, and it is the same enumeration this kernel has:

```c
switch (thread_state_ptr_get_tsType(state)) {
case ThreadState_BlockedOnSend:
case ThreadState_BlockedOnReceive: {
    endpoint_t *epptr = EP_PTR(thread_state_ptr_get_blockingObject(state));
    assert(endpoint_ptr_get_state(epptr) != EPState_Idle);
    tcbEPDequeue(tptr, epptr);
    ...
    setThreadState(tptr, ThreadState_Inactive);
    break;
}
case ThreadState_BlockedOnNotification:
    cancelSignal(tptr, NTFN_PTR(thread_state_ptr_get_blockingObject(state)));
    break;
case ThreadState_BlockedOnReply: {
#ifdef CONFIG_KERNEL_MCS
    reply_remove_tcb(tptr);
#else
    cte_t *slot = TCB_PTR_CTE_PTR(tptr, tcbReply);
    cte_t *callerCap = CTE_PTR(mdb_node_get_mdbNext(slot->cteMDBNode));
    if (callerCap) {
        cteDeleteOne(callerCap);
    }
#endif
    break;
}
}
```

Source: [`seL4/src/object/endpoint.c`](https://raw.githubusercontent.com/seL4/seL4/master/src/object/endpoint.c)

Four things to take from this, and the last is the one that changes the shape of the question.

**The authority is a TCB capability and nothing narrower.** `seL4_TCB_Suspend` "Suspend a thread",
capability required: "seL4_TCB capability to the target thread"
([API reference](https://docs.sel4.systems/projects/sel4/api-doc.html)). There is no suspend right, no
kill right, and no way to reach a thread you were not handed a TCB cap for. Holding a TCB cap is
holding the thread.

**Cancellation is not a message; it is a state.** The suspended thread is `ThreadState_Inactive`. It
does not resume, does not return an error to userspace, and does not run another user instruction. A
later `seL4_TCB_Resume` calls `restart()`, which sets `ThreadState_Restart` and re-executes the
interrupted syscall from the top. **seL4 never hands a cancelled thread an error code**, so it never
has to define one, and userspace never has to handle one.

**The reply relationship is a first-class object with a back-pointer.** Under MCS a `reply_t` holds
`replyTCB`, so finalising a reply capability finds the blocked caller:

```c
case cap_reply_cap:
    if (final) {
        reply_t *reply = REPLY_PTR(cap_reply_cap_get_capReplyPtr(cap));
        if (reply && reply->replyTCB) {
            tcb_t *tcb = reply->replyTCB;
            switch (thread_state_get_tsType(tcb->tcbState)) {
            case ThreadState_BlockedOnReply:
                reply_remove(reply, tcb);
                break;
            case ThreadState_BlockedOnReceive:
                cancelIPC(tcb);
                break;
            default:
                fail("Invalid tcb state");
            }
        }
    }
```

Source: [`seL4/src/object/objecttype.c`](https://raw.githubusercontent.com/seL4/seL4/master/src/object/objecttype.c)

and `reply_unlink` ends it the same way suspension does:

```c
/* Unlink a reply from its tcb */
static inline void reply_unlink(reply_t *reply, tcb_t *tcb)
{
    ...
    reply->replyTCB = NULL;
    /* This means the value of the thread state reply reference no longer matters. */
    setThreadState(tcb, ThreadState_Inactive);
}
```

Source: [`seL4/include/object/reply.h`](https://raw.githubusercontent.com/seL4/seL4/master/include/object/reply.h)

**So seL4 solves nife's stranded-caller problem structurally rather than by adding an operation.**
Destroying the reply object *is* freeing the caller, in both configurations: MCS through
`replyTCB`, non-MCS through the mapping database that links the caller's `tcbReply` slot to the
server's `callerCap`. In nife the `Reply` capability is a bare `(tid)` with no back-link and no
bookkeeping in the caller, so there is nothing to destroy and nothing to find.

### Mach: the distinction between abort and abort-safely is exactly this hazard

Mach has two operations and the difference between them is the whole design question.

> The function `thread_abort` aborts page faults and any message primitive calls in use by
> target_thread [...] `thread_abort` will abort any non-atomic operation [...] at an arbitrary point
> in a non-restartable way.
>
> Source: [`thread_abort`](https://web.mit.edu/darwin/src/modules/xnu/osfmk/man/thread_abort.html), XNU
> `osfmk` man pages

with an explicit caution not to use it on a non-suspended thread, because it is very difficult to
know what system trap the thread might be executing. Against that:

> The basic purpose of `thread_abort_safely` is to let one thread cleanly stop another thread [...]
> when `thread_abort_safely` returns (if successful), the target thread will appear to have just
> returned from the kernel [...] `thread_abort_safely` will not abort any non-atomic operation [...]
> but will return an error instead.
>
> Source: [`thread_abort_safely`](https://web.mit.edu/darwin/src/modules/xnu/osfmk/man/thread_abort_safely.html)

`KERN_FAILURE` means "The thread is in the middle of a non-restartable operation."

The lesson for nife is sharper than it looks. **Mach's split is between an abort that can leave the
victim's own invariants broken and one that refuses rather than do so**, and it needed both because
Mach threads block inside arbitrary kernel work. This kernel's blocked threads are all parked at one
of three well-defined points, with no kernel state in flight, so **every abort here is
`thread_abort_safely`-shaped and the dangerous variant has no reason to exist.** That is a real
advantage of a microkernel with four syscalls, and it is worth stating out loud rather than
assuming.

### L4 (Fiasco.OC / L4Re): cancellation as a side effect of touching registers

L4 has no abort verb either. It has `ex_regs`, whose stated job is setting a thread's instruction and
stack pointer, and cancellation is a *flag* on it:

> **L4_THREAD_EX_REGS_CANCEL** = 0x10000UL, "Cancel ongoing IPC in the thread"
> **L4_THREAD_EX_REGS_TRIGGER_EXCEPTION** = 0x20000UL, "Trigger artificial exception in thread"
>
> "This method allows to manipulate and start a thread. The basic functionality is to set the
> instruction pointer and the stack pointer of a thread. [...] Additionally, this method allows also
> to cancel ongoing IPC operations and to force the thread to raise an artificial exception (see
> flags). If the thread is in an IPC operation or if L4_THREAD_EX_REGS_TRIGGER_EXCEPTION forces an
> IPC then changes in IP and SP take effect directly after returning from this IPC."
>
> Source: [L4Re, `l4_thread_ex_regs`](https://l4re.org/doc/group__l4__thread__api.html)

Unlike seL4, L4 *does* hand the victim an error, and it distinguishes the two sides:

> "If, for instance, the IPC was aborted using L4::Thread::ex_regs(), the sender gets an
> L4_IPC_SECANCELED error while the receiver gets an L4_IPC_RECANCELED error."
>
> Source: [L4Re, *L4 Inter-Process Communication (IPC)*](https://l4re.org/doc/l4re_concepts_ipc.html)

with a separate `SEABORTED`/`REABORTED` pair in the error table
([error codes](https://l4re.org/doc/group__l4__ipc__err__api.html)). **The documentation does not say
what distinguishes cancel from abort**, and it uses "aborted" in the sentence explaining the
*canceled* codes, which is a caution about inheriting a vocabulary: two words that a reader cannot
tell apart are worse than one.

The shape worth taking seriously is the third one on the list: L4 also has **IPC timeouts**, so an L4
thread need never block forever in the first place. That is a different answer to the same problem
and it is not one the milestone 23 lane sketched. It is milestone 106's fork.

### Fuchsia / Zircon: they had this, and they removed it

The most valuable entry, because Fuchsia shipped thread killing, lived with it, and deleted it.

> "In the past, `zx_task_kill` allowed usermode to kill individual threads. However, killing
> individual threads encourages bad practices and has a high chance of leaving the process in a bad
> state." [...] "There is no reasonable use for usermode to kill individual threads. Exposing such
> facility encourages bad practices."
>
> The dangers listed: "Locks can be left acquired, including global locks like ones controlling the
> heap." "Memory can be leaked. At the very least the thread stack, but often many other pieces."
> "Runtime left in an inconsistent state." "Killing a thread in its way to a syscall leaves the
> process in an unknown state." "Defeats RAII wrappers and automatic cleanup."
>
> Source: [RFC-0007: Zircon removal of thread killing](https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0007_remove_thread_killing)

The current syscall documentation is the outcome:

> "This asynchronously kills the given process or job and its children recursively, until the entire
> task tree rooted at handle is dead. **Killing a thread is not supported.**" Rights: "_handle_ must
> have `ZX_RIGHT_DESTROY`." Errors: "`ZX_ERR_NOT_SUPPORTED` handle is a thread handle."
>
> Source: [`zx_task_kill`](https://fuchsia.dev/fuchsia-src/reference/syscalls/task_kill)

Zircon's answer to a thread blocked in `zx_channel_call` is therefore **not to touch the thread**. It
closes the channel and lets the call fail:

> `ZX_ERR_PEER_CLOSED`, "The other side of the channel was closed or became closed while waiting for
> the reply."
> `ZX_ERR_CANCELED`, "_handle_ was invalidated (e.g., closed) while waiting for a reply."
>
> Source: [`zx_channel_call`](https://fuchsia.dev/reference/syscalls/channel_call)

That is precisely case (b) in the hung-component taxonomy: destroy the object the thread is blocked
on and the block resolves itself. Zircon can always do this because a channel handle is an ordinary
object with an owner, and killing the *process* closes all of its handles. **The granularity at which
Zircon expresses "may end this" is the task, and the authority is a right on the object's own
handle**, which is the closest match in the survey to how this tree already thinks.

Zircon also documents the stale-transaction problem in its own vocabulary:

> "If a `zx_channel_call()` returns due to `ZX_ERR_TIMED_OUT`, if the server eventually replies, at
> some point in the future, the reply _could_ match another outbound request [...] This syscall is
> designed around the expectation that timeouts are generally fatal and clients do not expect to
> continue communications on a channel that is timing out."

Two systems, independently, documenting the hazard this note found in `ipc_reply`.

### QNX Neutrino: the client is unblocked by the server's death, and told so

QNX is the industrial synchronous-IPC system and its state machine is nife's, renamed:

> "If the client thread calls MsgSend(), and the server thread hasn't yet called MsgReceive(), then
> the client thread becomes SEND blocked." [...] "Once the server thread calls MsgReceive(), the
> kernel changes the client thread's state to be REPLY blocked, which means that server thread has
> received the message and now must reply." [...] **"If the server thread fails, exits, or
> disappears, the client thread becomes READY, with MsgSend() indicating an error."**
>
> Source: [QNX Neutrino, *Synchronous message passing*](http://www.qnx.com/developers/docs/7.0.0/com.qnx.doc.neutrino.sys_arch/topic/ipc_Sync_messaging.html)

The error is `ESRCH`, documented on `MsgSend` as "the server died while the calling thread was
SEND-blocked or REPLY-blocked"
([`MsgSend()`](https://www.qnx.com/developers/docs/6.4.1/neutrino/lib_ref/m/msgsend.html)).

This is the entry that most directly indicts the current state of this kernel. **QNX unblocks a
REPLY-blocked client when its server dies. nife does not**, because `Error::Gone` reaches only
endpoint wait queues and a reply-parked caller left the queue at the rendezvous. A hung *or dead*
server strands its callers here in a way QNX has not since the 1990s.

### Linux: the third state had to be invented, and the reason was social

Included for contrast, and the contrast is that Linux could not fix this by fiat.

> "If the expected wakeup event does not materialize, the process will wait forever and there is
> usually nothing that anybody can do about it short of rebooting the system. This is the source of
> the dreaded, unkillable process which is shown to be in the 'D' state by ps." [...] "Matthew
> created a new sleeping state, called TASK_KILLABLE; it behaves like TASK_UNINTERRUPTIBLE with the
> exception that fatal signals will interrupt the sleep."
>
> On why the existing interruptible sleep could not simply be used: "Unix tradition (and thus almost
> all applications) believe file store writes to be non signal interruptible. It would not be safe or
> practical to change that guarantee."
>
> Source: [LWN, *TASK_KILLABLE*](https://lwn.net/Articles/288056/) (merged for 2.6.25)

Two lessons, and neither is "copy this".

**A third state was needed because two were a false choice.** `TASK_INTERRUPTIBLE` means every wait
site must handle `-EINTR` and every application must expect it; `TASK_UNINTERRUPTIBLE` means nobody
handles anything and the process is unkillable. `TASK_KILLABLE` splits the difference by making the
wait breakable *only* by something that ends the process anyway, so **no correct program ever
observes the interruption**, and therefore no wait site has to be rewritten to handle it.

That is directly transferable. An abort that only ever precedes teardown costs userspace nothing; an
abort that returns an error a program is expected to survive costs every `CALL` site in the tree.

**And the constraint that forced it was compatibility, which this project does not have.** Linux
could not change what a blocking write means because applications depend on it. nife has 54 user
programs, all in this tree. The window in which this decision is cheap is now.

### Windows NT: the wait opts in

A third shape again, and the only one where the *victim* has a say. An NT wait is alertable or it is
not; a user APC is delivered only to a thread waiting alertably, and such a wait completes with
`STATUS_USER_APC`
([`KeWaitForSingleObject`](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-kewaitforsingleobject),
[Waits and APCs](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/waits-and-apcs)).
A non-alertable wait is not interruptible by an APC at all.

Read as a design, this is `TASK_KILLABLE` with the choice moved from the kernel's wait site to the
caller's argument, and it is the shape that fits worst here: an opt-in that a hung component chooses
for itself is exactly the cooperation a hang is defined by not giving. It is included because the
*inversion* is worth seeing, and because it is where an "interruptible `CALL`" design would end up if
the flag were the caller's rather than the destroyer's.

### The survey in one table

| System | Verb | Authority | What the victim sees | Does it reach a reply-parked caller? |
|---|---|---|---|---|
| seL4 | `seL4_TCB_Suspend` (cancel is a side effect) | a TCB capability | **nothing**: `ThreadState_Inactive` | yes, via the reply object's `replyTCB` back-link |
| Mach | `thread_abort` / `thread_abort_safely` | task/thread port | an interrupted-syscall return code | yes (message primitives) |
| L4Re | `ex_regs` with `..._CANCEL` (cancel is a flag) | a thread capability | `L4_IPC_SECANCELED` / `RECANCELED` | yes; also avoided by IPC timeouts |
| Zircon | `zx_task_kill`, **refused for threads** | `ZX_RIGHT_DESTROY` on a process/job handle | `ZX_ERR_PEER_CLOSED` / `ZX_ERR_CANCELED` from the channel | yes, by closing the channel, not by touching the thread |
| QNX | server death, implicitly | none: it is automatic | `ESRCH` from `MsgSend` | **yes, and this is its headline behaviour** |
| Linux | fatal signal + `TASK_KILLABLE` | ambient, by pid | nothing (the process dies) | n/a |
| NT | user APC to an alertable wait | thread handle | `STATUS_USER_APC` | only if the wait opted in |

**Nobody in this survey blocks forever with no way out.** nife is currently alone in that, and it is
alone in it by accident rather than by decision, which is what makes this worth a fork rather than a
`BUGS` entry.

## Mapping onto this kernel: what each shape would break

### Milestone 124's static proof stays intact, for all four proposals

`script/stack-depth-check` proves in CI, on both ISAs, that no context switch is reachable from the
interrupt-stack entry point, and `schedule()` carries the runtime half as a `debug_assert!`. **No
proposal here delivers an abort from an interrupt context.** Every one of them is entered from a
syscall on the *destroyer's* own thread stack, takes `SCHED`, mutates the victim's `Handshake` and
possibly one endpoint queue, and returns. The victim is touched as data, never switched to, and the
destroyer never calls `schedule()` from anywhere new.

The property to preserve if a proposal ever grows a timer (a deadline on `CALL`, milestone 106's
fork) is that **firing a deadline from the timer IRQ must not switch there either**. The existing
answer is already written: the interrupt path defers its switch to `preempt_if_needed`, one frame
outside the trampoline, back on the interrupted thread's own stack. A deadline would have to use the
same deferral, and that is a constraint to write into 106 rather than a reason to refuse it.

### The reply capability's semantics are the sharp edge

One-shot, `WRITE` without `GRANT`, minted at the rendezvous, consumed on use, living in the callee's
cspace. Three consequences:

- **The operator cannot answer on the component's behalf**, and this is by construction rather than
  by omission: the cap is minted without `GRANT` "so it could not have been delegated here in the
  first place". Nothing about freeing a stranded caller can route through delegating the reply.
- **The cap carries a thread name, not a call name.** See the hazard section above. Any proposal that
  wakes a reply-parked caller and lets it call again must either delete the outstanding `Reply(tid)`
  caps or refuse to let the caller run.
- **Deleting them is a whole-cspace sweep**, and `sched::delete_frame_caps` is the existing pattern
  for exactly that. It is not free but it is a teardown path.

### §16's revocation model and the `killed` flag

`killed` today means "convert to a corpse at the next preemption", and its one enforcement point
requires `state == Running`. Two of the proposals below change what `killed` means for a `Blocked`
thread, and that is a semantic change to a flag that `Untyped::DESTROY`, §24's `^C` escalation and
`sched::kill_thread` all set. **Whatever is decided, `killed` should end up with one meaning**, not a
meaning that depends on the victim's state, because the current pair (armed for a runaway, inert for a
sleeper) is exactly the kind of state-dependent rule this tree's ladder says to design out.

### Endpoint-only naming survives, but only if the authority is an existing capability

The rule is that nothing may acquire the ability to name a thread it was not handed. A `Tcb`
capability *is* a thread name that was handed over, so it does not breach the rule. But note what it
would change: **`Object::Tcb` is a construction-time authority today.** Every method on it
(`CONFIGURE`, `CAP_INSERT`, `START`) refuses a thread that is not an `Embryo`, so a `Tcb` cap is inert
the moment the thread runs. Giving it a method that works on a *running* thread converts it from a
builder's tool into a lifetime handle, which is a real widening and has to be decided as one.

### Weak memory ordering

Everything in this area is written under `SCHED`, which is the reason `wake_handshake`'s module doc
opens by saying there are no hand-rolled atomics in the protocol. The concurrency lives in the gaps
between critical sections, and the three rules that guard those gaps (the deferred wake for an
`on_cpu` thread, the undelivered-wake gate, the single-owner run queue) all apply unchanged to an
abort, because an abort *is* a wake with `ipc_aborted` set. The one new question a proposal raises is
whether a victim can be `Blocked` with `on_cpu` still set: it can, briefly, and `try_wake` already
defers that case into `wake_pending`. A proposal that instead writes `Finished` directly must check
`on_cpu` itself, which is precisely the `RegionReap::RefuseStanding` rule that cost four CI panics to
learn.

## The authorization question

The mechanism is small. This is the part that is not, and it is the part this system exists to get
right. In Unix you kill by pid with ambient authority; here the question is **which held capability
expresses the right to end a blocked thread**, and there are four candidate answers with different
consequences.

**Milestone 126 set the frame.** A domain names its members and does not act on them, with
`Rights::ENUMERATE` separating looking from acting, and `SURVEY` taking `ENUMERATE` while `RECV` and
`REAP` take `READ`. Any answer here has to say which side of that line it falls on.

| Candidate | Who holds it today | What it already authorizes | What ending a blocked thread would add |
|---|---|---|---|
| **The region capability** (`Untyped`) | the builder | `DESTROY`: reclaim the region and everything retyped from it | nothing new. `DESTROY` already commits to ending every resident; it just cannot finish |
| **The `Tcb` capability** | the builder, during construction | configure, endow, start | a lifetime handle where there is now a construction tool. seL4's answer |
| **The supervision endpoint** (`READ`) | the supervisor | `RECV` a death message, `REAP` a corpse | the "stronger right" §32 declined, and the hung-component note showed it is insufficient for case (c) anyway |
| **A new right** (`Rights::TERMINATE`) | nobody | nothing | a fifth bit; `Rights::ALL` is `0b1111` today, so this is an ABI change to `abi::rights` |

Three observations, offered as analysis rather than as a verdict.

**The region capability is the answer that adds no authority at all**, and that is a strong argument
in a system whose first principle about rights is that they should not widen. A holder of an
`Untyped` cap can already end every `Ready` and `Running` thread in the region, and can already leave
every `Blocked` one permanently killed-and-refused. It cannot only *finish*. Framed that way, ending
a blocked resident is not a new power; it is the completion of one that is already granted and
already destructive, and `reclaim_region`'s own `BUGS` already says a refused reclaim is destructive.

**The `Tcb` capability is the answer that composes**, and it is what seL4 chose. It names one thread
rather than a region, so it can end a hung component without touching its neighbours, and it is
already delegable and already narrowable. What it costs is the widening above: a builder that hands
out a `Tcb` cap today is handing out "you may assemble this thread", and afterwards would be handing
out "you may end this thread whenever you like, forever". Those are different offers and existing
call sites made the first one.

**The supervision endpoint is the answer that is most tempting and least available.** It is where a
reader expects the authority to live, because the supervisor is the party that notices. But §32
declined it, milestone 126 ruled that a domain names its members and does not act on them, and the
hung-component note proved the supervisor's vocabulary is insufficient for case (c) regardless. Adding
a terminate verb to `Endpoint` would overturn all three at once. It should be refused unless calef
means to overturn them.

**And the watchdog does not want any of these** (hung-component's answer 1, which stands). A watchdog
holds `ENUMERATE` and produces a verdict; the party that acts is the one that already holds the
authority to act. Nothing in this note changes that separation, and every proposal below keeps it.

## Four proposals

Each states the mechanism, the authority, the cost, what it breaks, and how it fails. **They are not
ranked, and none is recommended.** They are ordered from smallest surface to largest.

---

### Proposal A: `DESTROY` finishes what it starts

**Provisional name for the concept: *the completed reclaim*. Not minted; calef's call.**

**Mechanism.** In `reap_region_objects`'s refuse phase, a resident thread that is `Blocked` is not
merely marked `killed`. It is unlinked from whatever holds it (`remove_sender`, a new
`remove_receiver`, or nothing at all for `WaitRole::Reply`), every `Object::Reply(tid)` naming it is
swept out of every cspace, and its state is written straight to `Finished` without ever waking it.
The next pass, which the owner's existing retry loop already performs, finds a reapable corpse and
reclaims the region. `RegionReap` grows a fourth verdict; `region_reap_verdict` already exists as a
lifted, testable function precisely so a rule like this can be stated and proved without staging a
race.

**Authority: the region capability. No new right, no new method, no new syscall number, no ABI
change.** The holder of the `Untyped` has already said this region is going away.

**What it costs.** Roughly thirty lines plus `remove_receiver`. One reply-cap sweep per victim, O(128
x 16). No user-visible surface change whatsoever, so no program in the tree needs editing and no
documentation outside the kernel changes.

**What it breaks.**

- **A `Blocked` thread dies without running another instruction.** It never returns from its syscall,
  never sees `Gone`, never runs a destructor. This is seL4's `ThreadState_Inactive` and Zircon's
  entire objection to thread killing, and the objection lands: locks it holds stay held, and whatever
  it was in the middle of stays in the middle. The mitigating fact particular to this kernel is that
  a `Blocked` thread holds no *kernel* state in flight, and its region is being destroyed anyway, so
  the "process left in a bad state" hazard applies to the region's other threads and to nothing else.
- **Destroying region A silently removes a waiter from an endpoint in region B.** The endpoint's owner
  observes a sender that never arrived, which is indistinguishable from a client that never called.
  The precedent is settled (`remove_sender` on a supervisor's endpoint already does this) but the
  scope widens from corpses to live threads.
- **`killed` acquires a second meaning**, or, better, loses its old one for blocked threads. Worth
  deciding explicitly rather than letting the two coexist.

**How it fails.** It does not solve the *stranded caller in another region*. A client of the hung
server, blocked in `CALL`, sits in its own region, and freeing that region is its own owner's act.
Proposal A makes each region individually reclaimable by its own owner, which is enough for the
capacity problem and is not the same as an abort. If `wait_on` is ever stale (a future block site that
writes `state = Blocked` by hand instead of calling `park`, which `wake_handshake`'s `BUGS` says
nothing prevents), the unlink targets the wrong queue or none, and the failure is a dangling pointer
in an endpoint queue: the worst failure mode in the set. That argues for a `debug_assert!` pairing
`Blocked` with `Some(wait_on)`, and possibly for the lint-shaped fix that crate declined.

---

### Proposal B: a terminate verb on the `Tcb` capability

**Provisional names, all unratified: `Tcb::TERMINATE`, `Tcb::STOP`, `Tcb::CANCEL`. calef's call, and
the vocabulary matters more than usual because L4 shipped two words for this and cannot tell them
apart in its own documentation.**

**Mechanism.** seL4's `suspend()`, transliterated: cancel the IPC (unlink, sweep the reply caps),
dequeue, set a terminal state. A `Tcb` capability's methods stop refusing non-`Embryo` threads for
this one verb.

**Authority: a `Tcb` capability with `WRITE`.** Delegable, narrowable, and it names exactly one thread
that the holder was handed. Endpoint-only naming is not breached.

**What it costs.** A new method number in `abi::tcb`, a `DECISIONS` section for its semantics (§10's
rule), and the widening of what a `Tcb` cap means. Existing holders gain a power they were not offered;
`crates/system_initializer` and every spawn path would need auditing for who ends up holding one after
`START`.

**What it breaks.**

- **The construction-time invariant.** Today a `Tcb` cap is inert once the thread runs, and that is a
  clean, checkable property. Afterwards it is a lifetime handle, and "who holds a `Tcb` cap on this
  thread" becomes a question with security weight that it does not have now.
- **It gives a *narrower* authority than proposal A gives**, which sounds like an advantage and is
  also the risk: a holder can end one thread without owning its region, so the thread's memory is not
  thereby reclaimed. Ending a thread and reclaiming its memory become two acts with two authorities,
  and the hung-component note already found that conflating them is what made §32's sentence half
  wrong.

**How it fails.** If the answer is "suspend, then the region owner destroys", it needs both
authorities to be held by cooperating parties, and a supervisor holding a `Tcb` cap but no region
capability can stop a hang without reclaiming anything. That may be exactly right (it separates
policy from memory) or exactly wrong (it produces a stopped thread nobody can free), and which one it
is depends on a spawn-time endowment convention that does not exist yet.

---

### Proposal C: the caller's side, and only the caller's side

**Provisional name: *the abortable call*. Unratified.**

**Mechanism.** Leave the hung *server* alone entirely and fix the stranded *client*, which is QNX's
behaviour and Zircon's. Two independent pieces, and they can be taken separately:

1. **A reply-parked caller is woken when the thing it awaits becomes unreachable**, closing the gap
   the hung-component note found: `Error::Gone` reaches endpoint wait queues and not reply parks. The
   trigger would be the destruction of the server's region or of the endpoint the call went to, both
   of which the kernel already witnesses. This makes case (b) work for callers the way it already
   works for waiters, and it needs a way to find reply-parked callers of a given endpoint: their
   `wait_on` records `(ep, WaitRole::Reply)`, so a scan over `MAX_THREADS` answers it.
2. **A deadline on `CALL`**, which is L4's answer and is milestone 106's fork rather than this one's.

**Authority: none new for piece 1.** It rides on authorities already exercised (destroying a region,
destroying an endpoint) and changes only their reach. Piece 2's authority is the caller's own.

**What it costs.** Piece 1 is small and is arguably a bug fix rather than a feature: a caller stranded
by a *dead* server is stranded today, which QNX has not permitted since the 1990s and which nothing in
this tree records as intended. Piece 2 is a syscall-surface change and a timer, and is 106's.

**What it breaks.** Piece 1 makes `Gone` reachable at reply parks, which is exactly the path that
opens the stale-reply-capability hazard, so it cannot ship without the reply-cap sweep. Piece 2
inherits the L4 warning quoted above, word for word.

**How it fails.** It does not reclaim the hung component's own region, which is the capacity problem
that motivated the whole question. It halves the leak (one hang costs one region instead of one per
caller in flight) and leaves the other half. **It is the only proposal here that is plainly
complementary to the others rather than an alternative to them**, and that is worth noticing when
ranking: it can be taken with A, with B, or alone.

---

### Proposal D: accept the leak, bound it, and say so

Argued properly, because it may be right.

**Mechanism.** No kernel change. Instead:

1. **Record the limitation where a reader meets it**, which is `reclaim_region`'s `BUGS`, `Holding`'s
   `BUGS`, `notes/frames.md` and `notes/quotas.md`. Today `reap_region_objects`'s comment is the only
   place this fact lives, and a comment inside a kernel function is rung four.
2. **Bound it with the quota machinery that already exists.** `QuotaToken` is documented as holding a
   spawner's slot precisely for this case: "a child that blocks forever keeps holding it, which is
   correct: it is still consuming a thread, a stack, and an address space." A supervisor that spawns
   from a bounded budget cannot leak unboundedly; it runs out of children and refuses, loudly, at a
   place a human can see.
3. **Make the leak visible.** `SURVEY` already reports `BLOCKED`; a count of unreclaimable regions is
   a `free`-shaped program (milestone 126 already names `Untyped` + `ENUMERATE` as wanting exactly
   this) rather than a kernel change.
4. **Recover the service, not the memory**, which is what the hung-component lane demonstrated works
   today with no new authority: build, revoke the device, start the replacement.

**Authority: unchanged.** Nothing new anywhere.

**The argument for it, which is not a straw man.**

- **§40 is "no reaper of last resort", and Zircon deleted this feature after shipping it.** RFC-0007
  is a system with far more users than this one concluding that thread killing "encourages bad
  practices" and removing it. A project that adds it should be able to say why Fuchsia was wrong, or
  why its case differs. The honest difference is that Fuchsia kills the *process* instead, which nife
  can do for cases (a) and (b) and cannot for (c); the honest similarity is that both are arguing
  about the same hazards.
- **The failure this prevents is a capacity failure, and a capacity failure is survivable and
  visible.** Running out of untyped budget is a refusal at a call site, not corruption. A forcible
  teardown's failure mode is an invariant broken inside a component that was still holding something.
- **The customer path may not need it.** A backup target's hang budget is set by its supervisor's
  quota; if the supervisor can restart the service (which it can) and the operator can restart the
  machine on a schedule it already has, an unreclaimable region is a monthly reboot rather than an
  outage. That is a real answer, and it should be tested against the actual budget before a kernel
  change is spent on it.
- **This tree has refused stronger machinery on principle before**, and the refusals are on the record
  rather than being drift.

**How it fails.** The bound is per-supervisor, not global: N supervisors each with a quota of M leak
N x M regions before anyone refuses, and nothing composes those bounds. It also makes the *first*
hang free and the *last* one fatal with no gradient in between, which is the shape of failure that
looks fine in testing and arrives at 3 a.m. And it leaves this kernel alone in the survey table above
in having no way out at all, which is a claim a stranger reading the demonstrator will notice.

---

### Side by side

| | A: completed reclaim | B: `Tcb` terminate | C: caller's side | D: accept and bound |
|---|---|---|---|---|
| **New syscall / method** | none | one method on `Tcb` | none (piece 1) | none |
| **New right** | none | none | none | none |
| **New error visible to userspace** | none | none | `Gone` at a new place | none |
| **Authority** | the region cap, unchanged | a `Tcb` cap, widened | unchanged | unchanged |
| **Reclaims the hung component's region** | yes | only with a region cap too | no | no |
| **Frees its stranded callers** | each by its own owner | no | **yes** | no |
| **Needs the reply-cap sweep** | yes | yes | yes | n/a |
| **Victim runs again** | no | no | yes, with `Gone` | n/a |
| **Overturns a recorded decision** | no | widens `Tcb` semantics | no | no |
| **Rough size** | ~30 lines + `remove_receiver` | that plus ABI and a §-section | ~40 lines + sweep | docs and a program |

**What I would want to know more about, offered as questions rather than as a ranking.**

1. **Is the stale-reply hazard reachable today by any path I did not find?** I convinced myself it is
   not, by enumerating the exits from a reply park, but it is a proof by exhaustion over code I read
   rather than a checked property. It is Kani-shaped if `ipc_reply`'s guard were lifted into a
   testable function the way `region_reap_verdict` was.
2. **What does the customer path actually need?** Proposal D's case stands or falls on whether
   milestone 55's supervisor quota bounds the leak below the reboot cadence, and that is a number
   nobody has measured. It is measurable now.
3. **Is C separable enough to take on its own merits?** It looks less like a fork and more like a
   defect: a caller stranded by a *dead* server is a QNX behaviour from the 1990s that this kernel
   lacks, and nothing in the tree records that as intended.

## EXAMPLES

**See the kill that cannot land.** The condition is one line, and the state it requires is the one a
blocked thread cannot reach:

```sh
grep -n -A6 'Forcible teardown, before anything else' kernel/src/sched.rs
```

**See the abort machinery that already exists**, and that it is only ever entered from a wait queue:

```sh
grep -n 'set_ipc_aborted' kernel/src/sched.rs
```

Every hit is either a stale-endpoint check at the top of an IPC primitive or the `drain_waiters`
sweep in `reap_region_objects`. Nothing addresses a thread by name.

**See that a reply-parked caller is on no queue**, which is why the sweep misses it:

```sh
grep -n -B4 -A10 'pub enum WaitRole' kernel/src/thread.rs
```

**See the guard that checks the role and not the call**, which is the hazard:

```sh
grep -n -A12 'pub fn ipc_reply' kernel/src/sched.rs
```

**See the precedent for unlinking a thread from an endpoint outside the region being destroyed:**

```sh
grep -n -B10 'remove_sender' kernel/src/sched.rs
```

**Read the taxonomy this note starts from**, on the branch that holds it until it merges:

```sh
git show origin/milestone/23-hung-component:notes/hung-component.md
```

## BUGS

**Nothing here is built, and nothing here should be built before calef rules.** This note is analysis.
The proposals are sketches at the level of "which lines change", not designs; each would owe a
`DECISIONS` section for its semantics before a lane took it, and B would owe one for a new method
under §10's rule.

**The stale-reply-capability hazard is unreachable today and is therefore untested.** I established it
by reading `cap::reply_cap`, `slots`'s generational naming, and `ipc_reply`'s guard, and by
enumerating the two exits from a reply park. It is an argument, not a proof, and the tree's own
standard for arguments about the scheduler is that four CI panics have taught it to distrust them.
**No test in this tree would catch a regression here**, because no path can currently produce a second
`CALL` from a thread with an outstanding `Reply`.

**The claim that `WaitRole` enumerates every blocked thread depends on every block site calling
`park`.** `wake_handshake`'s own `BUGS` says nothing enforces that: the fields are public because the
kernel has legitimate out-of-protocol writers, so a future block site writing `state = Blocked`
directly opts out silently and would leave `wait_on` stale. Proposals A, B and C all unlink using
`wait_on`, so all three inherit that as their sharpest failure mode. I did not audit every write of
`State::Blocked` in the tree to confirm the enumeration is currently complete; I read the three IPC
primitives and the `WaitRole` doc comment, which assert it.

**The prior-art survey is thin on two systems.** Mach is read from the XNU `osfmk` man pages, which
document the interface and not the implementation, so what `thread_abort_safely` does to a Mach port's
in-flight message is not established here. QNX is read from the architecture guide and the library
reference, both vendor documentation with no source available, so the claim that the kernel actively
unblocks a REPLY-blocked client (rather than the client discovering the death later) is the
documentation's word and not verified.

**No number in this note was measured.** The line counts ("about thirty lines") are estimates from
reading, the sweep cost is arithmetic from `MAX_THREADS` and `CSPACE_SLOTS`, and no benchmark was run.

**The milestone number in the roadmap block this lane proposes is provisional** and belongs to the
integrator to mint, per the rule that anything global to the tree is assigned at merge.
