# The L4 lessons, audited against this kernel

Elphinstone and Heiser, **"From L3 to seL4: What Have We Learnt in 20 Years of L4 Microkernels?"**
(SOSP 2013), is unusual among retrospectives: it does not merely narrate, it renders a **verdict on
each of the original design decisions**, tagged `Retained`, `Replaced` or `Abandoned`. That makes it
auditable rather than inspirational, which is why this note exists.

Done 2026-08-18, at calef's question ("does our OS apply the lessons of Elphinstone and Heiser '13").
The paper's own tags are quoted; the nife column is checked against the tree, file by file, and the
evidence column names where.

**The headline: 15 of 17 applied, one partially, two not, and the misses are not independent.** Both
failures and the partial are the same cluster, and it is the same cluster milestone 132 measured and
design/decisions/95-a-proven-ipc-fastpath.md is deciding about.

## The audit

| # | The paper's verdict | nife | Evidence |
|---|---|---|---|
| 1 | Retained: minimality | yes | services are userspace processes; `kernel/src/user/*_service.rs` *spawns* them with grants rather than implementing them |
| 2 | Replaced: sync IPC augmented with async notification | yes | `ipc::Endpoint::signal`, with `recv_drains_a_pending_signal_first` proving a signal is never lost |
| 3 | Replaced: physical by virtual message registers | yes | three words in registers, a five-word mailbox in the TCB, no user-visible buffer address |
| 4 | Abandoned: long IPC | yes | no multi-buffer transfer exists; bulk goes through shared frames |
| 5 | Replaced: thread IDs by endpoints as destinations | yes | endpoint-only naming, and no syscall names a receiver |
| 6 | Abandoned: IPC timeouts | yes | none, and the resulting hang is recorded as a limitation where the reader meets it |
| 7 | Abandoned: clans and chiefs | yes | never had it; capabilities do the job it was invented for |
| 8 | Retained: user-level drivers | yes | `user/src/{blk,driver,net_stack}.rs`; the kernel validates DMA and never drives |
| 9 | Abandoned: hierarchical process management | yes | capabilities. §40 supervision is a lifetime relation, not an authority hierarchy |
| 10 | Abandoned: recursive page mappings | yes | capabilities with §16 revocation, rather than authority riding on a mapping |
| 11 | **Replaced: process kernel by event kernel** | **no** | every thread owns `STACK_PAGES` (6) of kernel stack plus a guard page. The word "continuation" appears nowhere in `kernel/src` |
| 12 | Abandoned: virtual TCB addressing | yes | a generational slot table (`crates/slots`), proved in `a_removed_name_never_resolves_again` |
| 13 | Replaced: lazy scheduling by Benno scheduling | **partly** | the ready queue holds every runnable thread *except* the running one, requeue happens at switch, and blocking does no queue surgery. But a rendezvous **queues** the receiver instead of switching to it |
| 14 | **Replaced: direct process switch subject to priorities** | **no** | `sched::ipc_send` fills the mailbox, wakes the receiver onto the run queue, and returns. The sender keeps running |
| 15 | Retained: mostly non-preemptible, strategic preemption points | yes | `preempt_if_needed` takes a `need_resched` flag at a defined point one frame outside the interrupt trampoline |
| 16 | Replaced: non-portable implementation by agnostic code | yes | 82% of `kernel/src` outside `arch/`, 75% excluding `cfg(test)` code. portability.md has the full comparison |
| 17 | Abandoned: non-standard calling conventions | yes | the standard Rust ABI throughout; the `svc` plus `x8` convention exists only at the user boundary |
| 18 | Abandoned: assembler code for performance | yes | 1,152 lines of assembly, all boot, context switch and vectors. None of it is there for speed |
| 19 | Abandoned: C++ (for seL4 and OKL4) | in spirit | Rust, chosen for what the verifier can reach, which is the same reasoning one language over |

Nineteen rows against "17 verdicts" because the paper tags two decisions twice in one passage and one
row here (19) is an analogy rather than a match. The count that matters is the three that are not
plain yes.

## The cluster, which is one design choice wearing three hats

Rows 11, 13 and 14 are not three findings. **They are one: this kernel has no direct process switch,
and every consequence follows from that.**

- Because a rendezvous does not switch to the receiver, it must **queue** it, which is the half of
  Benno scheduling row 13 does not get. seL4 gets it for free precisely because the common case
  switches instead of queueing.
- Because every thread must be able to block anywhere in the kernel and resume there, every thread
  needs **its own kernel stack**, which is row 11. An event kernel does not, because a blocking
  operation stores a continuation instead of a stack.

The paper's numbers for what row 11 costs, measured by Warton on Pistachio and quoted there: the
event kernel's **per-thread memory use was a quarter** of the process kernel's, and it held a **20%
performance advantage on a multi-tasking workload** (AIM7), despite needing more than twice the TCB
size to store continuations. Micro-benchmarks were generally within 1%, which is the part worth
noticing: **the cost of a process kernel does not show up where we currently measure.**

Our own figure for the same thing: 6 pages of kernel stack and a guard page per thread, so a thread
costs 28 KiB of kernel memory before it does anything.

### A connection milestone 132 found without knowing it

`script/fastpath-footprint` closed naively from `finish_switch` and got **11.2 KiB**, because the
reap branch drags in `KernelStack::drop`, `untyped::destroy`, `revoke_region` and the unmap path.
That was recorded as a measurement artifact and excluded as cold, correctly.

But the reason a *stack teardown* is reachable from the *switch* path at all is row 11. **In an event
kernel there is no per-thread kernel stack to free there.** The artifact the gate had to work around
is a fingerprint of the design choice this audit finds missing, which is the kind of thing only a
structured audit surfaces.

## What this does and does not imply

**It is not a defect list.** Rows 11 and 14 are choices the paper itself presents with trade-offs:
direct process switch "generally ignor[es] priorities", which is why seL4 made it *subject* to
priorities rather than restoring it wholesale, and Fiasco.OC and NOVA made it optional. A capability
kernel that wants predictable scheduling has a real reason to hesitate before adopting either.

**What is a defect is that neither was ever decided.** There is no block, no `DECISIONS` section and
no scope note anywhere in this tree recording that the process-kernel model was considered against an
event kernel, or that direct process switch was weighed and declined. They are the state the code
happened to grow into, and this note is the first place either is named as a choice.

**And the sequencing is already right, by accident.** §95 recommends landing the eligibility
predicate before any fastpath, and row 14 is exactly the change a fastpath would make. So the open
decision already sits on top of the largest gap this audit finds, which means the audit reorders
nothing. It raises the stakes of one decision that was already open.

## BUGS

- **The nife column is a reading of the tree, not a proof.** Each row was checked against named files
  on 2026-08-18, and a row could go stale the day the code moves. Rows 11, 13 and 14 are the ones
  worth re-checking first, because they are the ones any fastpath work would change.
- **Row 19 is an analogy and is marked as such.** The paper's verdict is about C++ against C for
  verifiability; ours is Rust against C for the same reason. It is not the same decision.
- **The paper's measurements are its own and were not reproduced here.** The quarter-of-the-memory
  and 20% figures for event kernels come from Warton's Pistachio work as quoted in the paper. Nothing
  in this tree has measured what an event kernel would save us, and until something does, the case
  for row 11 rests on somebody else's numbers on somebody else's kernel.
