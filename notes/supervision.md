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

**And at the interactive prompt** (the milestone 22 increment that migrated the boot path): every job
the shell spawns is born supervised, and `job_undertaker` collects the corpse. It is the smallest
supervisor in the tree and the one that says most plainly what supervision costs a system: one
endpoint capability, no memory, no policy, and the job's pages come home to init's pool rather than to
the collector. The restart half is deliberately absent, because a command a person typed has no
business being restarted when it ends. `kernel/src/user/job_undertaker_tests.rs` proves it with a control
(three jobs exhaust a three-job pool when nothing collects) and a claim (twelve go through the same
pool when `job_undertaker` runs); `script/shell-check` runs eleven through the real boot's six-job pool.
See notes/trusted-init.md for what init gave up and what it still holds.

Cross-ISA kernel tests (`kernel/src/user/supervision_tests.rs`): a child built holding a fault
endpoint crashes on a null load, the supervisor receives `(FAULT, tid, pc, addr)` with the right tid
and address, the corpse still holds its fault message after delivery, revocation reaps it, and a
fresh child runs in its place; a second test drives the clean-exit path and asserts the `EXIT` event.
See DECISIONS §26, notes/abi.md (the message format and spawn-slot convention), and
notes/object-revocation.md (the reap the supervisor uses).

## The reap authority a supervisor actually needs, stated concretely

Milestone 36 (notes/c-seam.md) is the first thing outside milestone 22 to build a supervisor that
restarts a real component, and it pinned down the requirement §26's phase-B block left open.

Reaping is `Untyped::DESTROY`, which needs `WRITE` on the region. **`WRITE` on a region is also what
builds a process from it** (`RETYPE`, `RETYPE_OBJ`, `SPLIT`). So a supervisor that can restart its
child is a supervisor that can build processes, unless it proxies the reap through something else.

- The **proxy** exists today: phase B.2's `sub_server_supervisor` holds no memory and asks `spawner` to reap. Right
  for a system's init, where the point is that init can no longer build.
- Milestone 36's `c_confiner` takes the **direct route** and therefore holds a full untyped budget
  for its whole life, which is exactly the bundling the open fork is about.
- **The requirement, in one sentence: a supervisor needs `DESTROY` on one region it did not create.**
  Not `RETYPE`, not `SPLIT`, not a whole budget. Whether that becomes a rights bit split out of `WRITE`
  or a distinct `Untyped::REAP` method changes the rights model and the syscall surface, so it stays a
  decision rather than an implementation detail.

## Resolved: `Endpoint::REAP` (DECISIONS §32)

calef ratified it the same day the requirement above was measured, and it is built on both ISAs. The
method hangs off **the supervision endpoint**, not off `Untyped`: an `Untyped` method has to name a
region, and the whole premise is that the supervisor holds none. Authorization needs no new
bookkeeping, because §26 already stores `Thread::fault_ep` and the kernel already stamps the tid on the
death message, so the check is "the named thread's recorded fault endpoint *is* the endpoint being
invoked". No registry, no new capability type, one new method.

Two boundaries, both deliberate:

- **It collects corpses; it does not kill.** A live thread is refused. Killing is strictly more
  dangerous and keeps its existing home in §24's forcible tier, which needs construction authority
  precisely because it is the stronger act. The cost is real and stated rather than hidden: **a
  supervisor cannot restart a hung child**, because a livelocked process never sends a death message.
  That is the watchdog case, and it waits for milestone 23.
- **The reclaimed region goes to its owner under §13, which is the builder, not the reaper.** A
  supervisor can free a child's memory and cannot spend it, which is what lets builder and supervisor
  be separate processes without the supervisor slowly accumulating the builder's rights.

It also settles the tid-to-handle half of this gap **for this case only**: the tid is authorized
*relative to the endpoint it arrived on*, so no `Tcb::NAME`, no per-child endpoints, no
builder-reported tid. That is the endpoint-only naming discipline applied consistently, a name that
means something only to the holder of the capability it came through rather than a global handle. Any
future operation that needs to name a child is a fresh decision and should try this shape first.

### What narrowing the existing supervisors measured

This is the part worth reading, because it says what §32 did and did not buy.

- **`sub_server_supervisor` (milestone 22 phase B.2) now holds nothing but endpoints.** It had been the proxy: no
  memory of its own, asking `spawner` to reap on its behalf. The proxy is no longer needed, so the
  supervisor role reduces to exactly the authority its job describes. That is the payoff.
- **`c_confiner` (milestone 36) still holds a full construction budget, and that is a finding rather
  than a failure.** It is *also* the builder: it splits a region per instance and lays `c_shim` out
  in it, and §32 does not touch construction. So the bundling §31 recorded was **two** things and
  only one of them was the reap. What changed is that the per-instance region capability is now
  deleted as soon as the child starts, instead of being held for the instance's whole life, so the
  caretaker holds nothing that reaches a live instance's memory. Split roles 1 and 2 into separate
  processes and the supervisor half would hold only endpoints, which is what `sub_server_supervisor` now
  demonstrates. Keeping them fused in the spike stays deliberate: the requirement is visible in one
  program rather than hidden behind an IPC hop.

The authorization invariant is machine-checked, not only tested: two Kani harnesses in `crates/capability`
cover it, which is the right instrument for "a capability that cannot build cannot be made to build via
reap" because it quantifies over rights combinations rather than sampling them.
