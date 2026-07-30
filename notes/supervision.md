# Supervision: a thread's death becomes a message

The kernel is the only witness to a thread's fault. It is the one that saw the bad load, the illegal
instruction, the exit. So it is the one that must pass the news along. Milestone 22 builds the one
kernel mechanism a userspace supervision tree needs (DECISIONS §26): **when a thread faults or exits,
the kernel delivers a message to the supervision endpoint its spawner designated.** Restart policy
stays in userspace, and the kernel never relaunches anything.

This is seL4's fault endpoint, and it is the mechanism half of the mechanism/policy split the whole
project runs on. The kernel turns a death into a message; what to *do* about the death (retry,
back off, give up, escalate) is a userspace supervisor's business, layered with ordinary IPC.

## What the kernel does, exactly

Three pieces, and the surface cost is zero new syscalls and zero new methods.

1. **Designation, at spawn only.** A supervised thread is spawned with its supervision endpoint in a
   reserved cspace slot (`abi::fault::FAULT_EP_SLOT`, the last one). At `START` the kernel reads that
   slot; an `Endpoint` capability there means "supervised," and the kernel records the endpoint as
   the thread's fault target (`Thread::fault_ep`) and **clears the slot**, so the child holds no
   authority to send on it. A thread spawned with an empty fault slot is unsupervised and gets the
   pre-22 behaviour: it dies and is reaped immediately, reporting to no one. Supervision is fixed at
   spawn and cannot change afterward; runtime reattach is deferred (§26.2) until milestone 23's
   hot-swap work needs it.

2. **Delivery, without blocking the faulting path.** When the thread dies (`sched::depart`, reached
   from both the arch fault handlers and `SYS_EXIT`), the kernel builds the five-word message and
   delivers it to the fault endpoint. Delivery is the ordinary synchronous-send rendezvous, reused:
   if a supervisor is already blocked in `RECV`, hand it the message and wake it; if none is, **the
   corpse itself parks on the endpoint's sender queue** with the message in its mailbox, so the
   notification waits there rather than being lost. This is the same guarantee an ordinary blocked
   sender gets, and it is why a data-carrying death rides the sender queue rather than the data-less
   IRQ signal count (`irq_notify`): a signal count could say "something died" but not carry the tid,
   pc, and address. The corpse is never woken: `ipc_recv` recognises a `Dead` sender, takes its
   message, and leaves it dead, exactly the way it already leaves a `CALL` caller blocked.

3. **Dead until reaped.** After the message, the thread is `State::Dead`: it never runs again, but
   its corpse (TCB, address space, memory, and the fault-time registers in its mailbox) persists for
   postmortem. The scheduler never runs a `Dead` thread, and the reaper (`finish_switch`) never
   collects one; only the supervisor's explicit §16 revocation (`Untyped::DESTROY` on the child's
   region) frees it. That is what makes a future resume protocol possible additively: the reserved
   fifth message word can carry it, because the corpse it would resume is still there.

## The message

Five words, delivered to the supervision endpoint's holder through a plain `RECV`:

```text
  w0  event    fault::EVENT_FAULT or fault::EVENT_EXIT   (crashed vs finished)
  w1  tid      the dead thread's id, kernel-stamped
  w2  pc       the faulting instruction (0 for a clean exit)
  w3  addr     the faulting address (0 for a clean exit)
  w4  reserved 0 today; a fault-reply / resume protocol arrives here additively
```

`RECV` returns `w0` in the result register and `w1..w4` in the next four argument registers. The IPC
mailbox widened from three words to five to carry this; ordinary three-word IPC leaves the top two
zero, so only a supervisor reads them and no other program's `RECV` changes.

Both events flow because restart policy needs to tell "crashed" from "finished": a crash is a reason
to restart, a clean exit is a reason to stop. The tid is trustworthy without a badge because the
**kernel is the only sender on this path**. seL4 solves the general untrusted-sender case with badged
capabilities; that machinery returns as its own decision if a supervision endpoint ever needs
trustworthy identity from userspace senders.

## Why the corpse is a new state, not a reused one

`Finished` is the state of a thread the reaper should collect right now (a normal exit, or an
unsupervised fault). `Dead` is a thread that has reported and must *not* be collected until its
supervisor says so. Reusing `Finished` would race the reaper against the supervisor; a distinct
`Dead` state makes "dead until reaped" a property of the type, not of timing. Revocation treats
`Dead` as reapable (unlike a live `Ready`/`Running`/`Blocked`), and the scheduler treats it as
never-runnable, so it is dead in every sense but "its memory is gone."

## What this is not

It is not restart policy, and it is not automatic. The kernel delivers one message and stops; a
userspace supervisor decides everything else. It is not a heartbeat or a liveness check either: this
detects death at the exact instant with the exact cause, which polling cannot, but a supervisor that
wants to catch "alive but wedged" layers its own timeout with ordinary IPC and no kernel help (§26.1).

## Proven

**The policy half, built on top of this** (phase B.2, notes/trusted-init.md): a real userspace
supervision tree where a supervisor holding *no memory at all* applies bounded-retry policy to a
sub-server a construction server builds, and init has deleted the authority it would need to interfere.
That is what this mechanism exists for, and it is proven on both ISAs in `authority_tests`.

Cross-ISA kernel tests (`kernel/src/user.rs`, `supervision_tests`): a child built holding a fault
endpoint crashes on a null load, the supervisor receives `(FAULT, tid, pc, addr)` with the right tid
and address, the corpse still holds its fault message after delivery, revocation reaps it, and a
fresh child runs in its place; a second test drives the clean-exit path and asserts the `EXIT` event.
See DECISIONS §26, notes/abi.md (the message format and spawn-slot convention), and
notes/object-revocation.md (the reap the supervisor uses).
