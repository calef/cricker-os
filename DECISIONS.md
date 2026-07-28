# cricker-os: Architecture Decisions

Decisions made 2026-07-12, before any code was written. Each entry records what we
chose, what we rejected, and why. Revisit these deliberately, not accidentally.

## 1. Target architecture: aarch64

Chosen over x86_64 and RISC-V.

x86_64 has the deepest pool of tutorials, but a large fraction of what it teaches is
Intel history (real mode, the A20 line, segmentation ghosts, PIC-vs-APIC) rather than
operating system concepts. RISC-V is the cleanest architecture to learn on, but it is
the *hardest* of the three to actually get onto silicon: peripheral documentation for
the JH7110-class SoCs is thin.

aarch64 gives a clean exception model (EL0/EL1/EL2/EL3), a sane MMU, an excellent
bare-metal community, and real hardware at the end of the road (Raspberry Pi). The dev
machine is also ARM, so kernel assembly and host assembly are the same instruction set.

## 2. Primary target: QEMU `virt`, Raspberry Pi as a later port

The QEMU `virt` machine has a PL011 UART, a GIC interrupt controller, and virtio
devices, all well-specified. Boots in a second, debuggable with GDB, scriptable in
tests.

The Raspberry Pi port is a deliberate later milestone, not an afterthought. It is the
moment the hardware abstraction layer gets tested for real, and it will reveal exactly
which assumptions were secretly QEMU-shaped.

## 3. Use the crate ecosystem

`aarch64-cpu` for system-register access, `tock-registers` for typed MMIO. Not
hand-rolled `asm!` and raw volatile pointer writes.

Time goes to kernel concepts (memory, scheduling, syscalls, filesystems), not to
debugging typos in ARM system-register encodings that a crate would have caught at
compile time.

## 4. Kernel shape: monolithic, deferred, with two cheap rules

We are NOT speculatively trait-ifying every subsystem to "keep the microkernel door
open." That builds the wrong abstraction before the requirements are known, and taxes
every file for a door we may never walk through.

Instead, two rules that cost almost nothing and preserve the real option:

1. **A driver never reaches into a kernel global.** It gets what it needs passed in.
2. **The syscall surface stays narrow and explicit.** It is a boundary, not a habit.

## 5. Execution model: preemptive threads with real stacks

Rejected: async/await cooperative multitasking (where the Philipp Oppermann blog series
ends).

The reason is a hard ceiling, not a matter of taste. A userspace process is an arbitrary
ELF binary. It has its own stack, it never yields, and it will loop forever because we
will write a bug. Under cooperative scheduling, one bad user program hangs the machine
permanently. Real user mode *requires* per-thread stacks, a context switch that saves and
restores the register file, and timer-driven preemption. Async doesn't defer that work,
it forecloses it.

Async can come back later, in userspace, on top of real threads, exactly the way a real
OS lets a program run Tokio. Nothing is given up.

### The claim, sharpened (revisited after milestone 5)

Async is not *wrong*. It is wrong at **this layer**, and the reason is exact:

> **Async's core assumption is "I compiled everything that runs."**
>
> **An operating system's entire purpose is to run code it did not compile.**

Which is why Embassy (async, no threads, no preemption) is excellent on a microcontroller:
you compile every task, there is no untrusted code, and 64 KB of RAM genuinely cannot afford
twenty stacks. Every assumption async needs holds there. **None of them hold in a kernel with
userspace.**

And one word above is too strong. Strictly, a kernel *could* use async internally for its own
I/O while running user processes as real preemptive threads. That is a legitimate design. The
precise claim is narrower and stronger:

> **Async cannot be the execution model for userspace.** It can be an execution model *inside*
> the kernel, on top of real threads.

### The corroboration: Go had to build preemption

Go's goroutines were originally **cooperative**. They yielded at function calls, via the
stack-growth check in every function prologue. And Go owns its compiler, owns its runtime, and
compiles **every line that executes** — every assumption async needs, satisfied.

It still didn't work. A goroutine in a **tight loop with no function calls** never yields. The
garbage collector's stop-the-world could never stop it. The program hangs.

**Go 1.14 (2020) added asynchronous preemption**: the runtime sends a signal to the OS thread,
and the signal handler forces the goroutine to yield.

Which is to say: **Go built a timer interrupt in userspace, because cooperative scheduling
could not take the CPU back from a loop.**

If a language that owns its entire toolchain could not get away with cooperative scheduling, a
kernel running arbitrary ELF binaries certainly cannot.

### The asymmetry, which is the whole decision

| Direction | Cost |
|---|---|
| threads → async | **additive.** Run an executor on top. Nothing is thrown away. |
| async → threads | **a rewrite.** You need per-task stacks and a context switch — exactly what the executor existed to avoid. The executor goes in the bin. |

When one direction is cheap and the other is a rewrite, take the one that keeps the option
open. That generalizes well beyond this decision.

### And the hard part turned out to be already written

The instinct that async was "more tractable" was measuring the wrong thing.

`SAVE_CONTEXT` and `RESTORE_CONTEXT` in `vectors.s` were written at **milestone 2**, for
exception handling, with no thought of threads. They save `x0`–`x30`, `ELR_EL1`, and
`SPSR_EL1` into a `TrapFrame`.

**That is the register file.** A context switch is: save into thread A's frame, restore from
thread B's frame, swap `sp`. About thirty instructions, and most of them already exist,
because a kernel needs them anyway.

Writing a scheduler is not hard. Saving a register file is not hard. What is hard is the part
async cannot do at all.

## 6. SMP: single-core, refactor when it hurts

Boot CPU 0 only. Globals and a big lock are fine for now.

We explicitly considered shaping per-CPU data structures up front as cheap insurance,
and declined. Feeling the pain that created per-CPU structures is itself a legitimate
way to learn why they exist. Cost: a scheduler rewrite later. Accepted knowingly.

## 7. Testing: QEMU harness + host-testable crates, from commit one

A custom test harness boots the kernel in QEMU, runs tests, and exits with a status code
`cargo test` understands. Separately, pure logic (allocator algorithms, page-table math,
scheduling policy, filesystem parsing) lives in crates that compile for the *host*, so
most tests run in milliseconds with no emulator.

Front-loads about a day. Prevents a year of debugging by `println!`.

## 8. Process model / syscall ABI: DEFERRED to a hard decision point

**RESOLVED at milestone 7. See §10.** Kept here as written, because the deferral was the
decision and it held.

Unix-like (fds, fork/exec) versus capability-based (seL4/Fuchsia-shaped) is genuinely
undecided, on purpose. Milestones 1-6 do not touch the syscall boundary, and every
kernel builds them roughly the same way, so the deferral is free until it isn't.

**Milestone 7 (user mode) is a hard decision point.** When we get there we stop, look at
what we've built, and choose deliberately. This deferral is a plan, not a drift. If we
find ourselves hacking in a syscall without having had that conversation, the plan has
failed.

It didn't. We stopped and had the conversation, over the course of a day, before a line of
milestone 7 existed.

## 9. Locking: IrqSafeMutex, plus a discipline

Decided 2026-07-13, before milestone 5 brings interrupts.

**The problem.** A plain spinlock in a kernel that takes interrupts is a guaranteed hang.
On **one core**: kernel code takes the lock, a timer interrupt fires, the handler tries to
take the same lock, and spins forever waiting for code that cannot run until the handler
returns. Not a race. Not "under load." A deterministic deadlock the moment the timing
lines up. SMP makes it worse; single-core does not save us.

**The decision: A + B.**

**A. Every kernel lock is an `IrqSafeMutex`** (`kernel/src/sync.rs`). It masks IRQs on
acquire and **restores the previous state** on release. This is Linux's
`spin_lock_irqsave`.

**B. Interrupt handlers do not allocate.** They acknowledge, record what happened, and
defer the real work to normal context. This keeps the interrupts-off window short, which is
what makes A's cost acceptable.

### Rejected: per-CPU reserve pools

Considered, and it turned out to be **an answer to a different question**. Per-CPU page
caches (Linux's PCP lists) exist for *scalability* and *cache locality*, not interrupt
safety: Linux still wraps them in `local_irq_save`. They do not solve this deadlock. They
belong to the SMP conversation (§6), where the problem is lock *contention*, not deadlock.

We also confirmed A+B is genuinely sufficient rather than a compromise. The only handler
that ever needs to allocate is the page fault handler, and:

> **Kernel memory is never demand-paged.** Kernel pages are mapped eagerly. A page fault
> taken from EL1 is a bug and is fatal (which is already true).

So every allocating fault comes from EL0, whose context held no kernel locks, because it
cannot. Nothing is left that needs a reserve pool.

### The rules

| Rule | Why |
|---|---|
| All kernel locks are `IrqSafeMutex` | A bare spinlock is a deadlock waiting for a schedule |
| **Acquire: mask IRQs, *then* take the lock** | The other order leaves a window holding the lock with IRQs live |
| **Release: drop the lock, *then* restore IRQs** | The other order leaves the same window, from the other side |
| **Restore, never blindly enable** | A lock taken inside a handler must not unmask IRQs on release |
| Keep critical sections short | Interrupts are off for the whole of it |
| Never allocate while holding a lock | Nested acquisition, and it makes the window long |
| Never `wfi`/`wfe` or block while holding a lock | Interrupts are off. You will not wake up. |
| Two locks? Define a global order, always take them in it | Otherwise AB-BA deadlock, which is a *real* race and far nastier |
| Interrupt handlers record and defer; they do not do work | Keeps the IRQ-off window short |
| **The panic/fault path breaks the console lock** | Faulting mid-`println!` would otherwise deadlock in the handler and lose the one message that mattered |

The last one is `console::force_unlock()`, called at the top of the panic handler and the
fatal exception path. Linux does the same and calls it `bust_spinlocks`.

### The ordering rule is now enforced, not merely written down

We wrote "define a global order and always take them in it" and then relied on remembering.
Now every lock carries a **rank**, and `IrqSafeMutex::lock` asserts:

> **You may only acquire a lock strictly LOWER than everything you currently hold.**

If every acquisition strictly decreases, a **cycle is unrepresentable**. Not unlikely.
Impossible. It destroys the circular-wait Coffman condition outright (notes/deadlock.md),
which is *prevention*, not detection: Linux's `lockdep` builds a dependency graph at runtime
and hunts for cycles; this costs three instructions and cannot be wrong. FreeBSD (WITNESS) and
Solaris use the same mechanism.

```
  50  HEAP, SLAB      the allocators
       |
  30  FRAMES, RAM     the physical memory map
       |
  10  CONSOLE         the leaf: everyone may take it, it takes nothing
```

Two locks at the **same** rank may never nest (`R < R` is false), which is exactly right:
equal rank means we declared no order between them, so nesting would be picking one at random.

The nestings this permits are the ones that actually happen:

- **SLAB (50) → FRAMES (30)** — a size class runs dry and takes a page while holding its lock.
- **anything → CONSOLE (10)** — a panic prints while holding a lock. Which is *why* the console
  must be the leaf.

The panic path calls `sync::force_reset_ranks()` alongside `console::force_unlock()`. Panicking
while holding the console lock would otherwise trip the ranking assertion *inside the panic
handler* and lose the original message to a recursive panic. **The bookkeeping is a debugging
aid; it must never be the thing that stops us saying what went wrong.**

## 10. Process model: capability-based, microkernel. Untyped memory deferred.

Decided 2026-07-14, at the §8 decision point, before any of milestone 7 was written.

**A process names a resource by holding an unforgeable token it was handed. There is no
ambient authority, and there is no global namespace.** Drivers and services are userspace
processes. IPC is the primary syscall.

### What a capability is, so the word means something

A capability is **a file descriptor that can point at anything, not just files**. Same
mechanism, generalized: a per-process table living in *kernel* memory, indexed by a small
integer. The unforgeability is not cryptographic and there is no magic. You cannot
fabricate slot 7 for the same reason you cannot fabricate `fd 7`: the table is not yours to
write.

The difference from Unix is not the fd. **Unix already has capabilities.** The difference is
that Unix *also* has a back door, `open(path)` checked against your uid, which lets a process
**mint** authority out of who it is. We are not building the back door.

### Rejected: Unix-like (fork/exec, paths, uids)

Not rejected because it is bad. Rejected on an **asymmetry**, and it is the same asymmetry
that decided §5.

| Direction | Cost |
|---|---|
| capabilities to a Unix-shaped API | **Additive.** A POSIX shim in userspace. Fuchsia's `fdio` is exactly this: `open`/`read`/`write` on top of capability handles. Nothing is thrown away. |
| Unix to capabilities | **A rewrite, and historically it fails.** |

The second row is not speculation. **FreeBSD's Capsicum** (2010) added `cap_enter()`, which
drops a process into capability mode with no ambient authority. It works. It is in the base
system. It has been there for fifteen years, and **almost nothing uses it**, because every
program assumes it may call `open("/etc/resolv.conf")`, and once that assumption is baked
into a million lines of userspace you cannot take it back. OpenBSD's `pledge`/`unveil` and
Linux's `seccomp` and Landlock are the same story: revoke-after-the-fact, all partial, none
achieving no-ambient-authority.

> **Ambient authority, once granted, cannot be withdrawn.**

§5 said the asymmetry argument "generalizes well beyond this decision." It does. It
generalizes to this one.

### And what the Unix path actually costs us

We lose `fork`, copy-on-write, a VFS, and pipes as things we build with our own hands. Those
are each instructive, and they are the mechanisms in the system Chris uses every day. That is
a real loss, taken knowingly.

Against it: **on the Unix path you transcribe; on the capability path you derive.** xv6 exists,
is 10,000 lines, has a book, and holds a canonical answer to every question the Unix path
raises. That is a feature if the goal is to ship and a **hazard** if the goal is to understand,
because the path of least resistance becomes "look at how xv6 did it," and the result is a
working kernel you did not think through. There is no xv6 for this path. Every design question
is ours.

For a project whose stated purpose is understanding, that is not a cost. It is the product.

### Not a reason: differentiation

It was floated, and it is **factually wrong**, and it is worth writing down so it does not come
back.

aarch64 is not virgin ground for capability microkernels. **It is their home turf.** seL4 is
primarily an ARM story. L4 runs on every Qualcomm baseband. An L4 derivative runs the Secure
Enclave. QNX runs most cars. Trusty runs on essentially every Android phone. Zircon runs on ARM.
And in the hobby-Rust space, **Redox is already a Rust microkernel that runs on aarch64.**

Building a capability microkernel on ARM is not unusual. It is the single most ARM-shaped thing
one could build.

More importantly: **differentiation is a product goal, and this is not a product.** Choose
capabilities to be novel and you will make decisions that *look* novel. Choose them to
understand and you will make decisions that teach. Those diverge. See the top of CLAUDE.md.

### The performance question, answered so it stops being asked

**It does not matter to us**, and we should not let it drive the decision in either direction.
We run on QEMU with no workload and no users. We will never measure it. But the honest numbers,
since they were asked for:

| Axis | Runtime cost |
|---|---|
| **Capabilities as the naming model** | **~Zero.** A capability lookup and an fd-table lookup are the same operation. Anyone who says "capabilities are slow" means IPC. |
| **Untyped memory** | **~Zero**, possibly negative: the allocator moves to userspace, where it has no kernel lock and no boundary to cross. |
| **Microkernel (servers in userspace)** | **The entire cost. All of it.** |

And even there, the shape surprises. **One IPC is not slow**: seL4's fastpath is a few hundred
cycles, comparable to a Linux syscall (and *better* than one post-Spectre). Liedtke fixed that
in 1995, and it stayed fixed. The cost is that **you need more crossings**: a `read()` that was
one syscall becomes six. And the real bite is not cycles but **cache and TLB pollution**, which
UNSW have measured at several times the direct cost.

The discipline that recovers most of it, which every serious microkernel converges on:

> **IPC carries control. Shared memory carries data.**

Put the bytes *in* the message and you copy twice and you are Mach, and slow. Put a *frame
capability* in the message and the receiver maps it: zero copies.

Ballpark: **none** on compute-bound work, **low single-digit percent** for general-purpose work
(L4Linux is the cleanest apples-to-apples number that exists), a **bad tail** on I/O-heavy and
per-packet workloads.

And the gap has closed **from both directions**. Spectre and Meltdown mitigations made Linux's
syscall boundary genuinely expensive. `io_uring` exists precisely because of it, and its answer
(a shared-memory ring, batch the operations, stop crossing the boundary per call) **is the
microkernel discipline under another name**. DPDK and SPDK moved networking and storage drivers
into userspace for the same reason. Those are microkernels. They just had to bolt the isolation
on afterward, with an IOMMU, instead of getting it free from an address space they already had.

### The three things this actually buys, none of which is speed

1. **A driver bug is a crashed process, not a dead machine.** Drivers are the majority of a
   monolithic kernel's code and carry far higher bug density than its core. In Linux every one of
   them runs at EL1 in the kernel's address space. Here a driver holds a capability to some MMIO
   and an endpoint, and when it faults, it faults **alone**.

2. **Least privilege by construction, not by policy.** A compromised network driver in Linux owns
   the machine. Here it holds a capability to the NIC's frames and an endpoint to the network
   stack, and **it cannot express reading your disk**. Not "the attempt is denied." The attempt is
   not constructible. That is the confused-deputy problem made unrepresentable, which is the same
   move as `TlbFlush`'s `Drop` and the lock-rank assertion in §9: prevention, not detection.

3. **A kernel small enough to hold in your head.** seL4 is ~10,000 lines and has a machine-checked
   proof. Linux is over 30 million. For a project whose purpose is understanding, that is not
   incidental.

And one that is pure Rust luck: **a capability is an owned, unforgeable, non-copyable token.** It
is a `Box` with teeth. Learning Rust and learning OS design turn out, here, to be the same
education.

### An interrupt becomes a message

Worth stating early, because it is where §5's exception model meets this one. A driver holds an
**IRQ capability** bound to a notification, and blocks. The kernel's handler does one thing:
signal it. The driver has no interrupt handler. It has a loop:

```rust
loop {
    wait(irq_notification);          // sleeps until the device interrupts
    let packet = read_device_fifo();
    send(netstack_endpoint, packet);
    ack(irq_cap);
}
```

Ordinary code, in a process, at EL0. If it deadlocks, it deadlocks by itself.

### What we are NOT doing yet: untyped memory

seL4's most astonishing property is that **after boot the kernel never allocates.** It has no
heap. Memory is a capability type (`Untyped`), and userspace hands the kernel a chunk and says
"retype this into a page table." Three things fall out: the kernel *cannot* run out of memory,
kernel-memory exhaustion disappears as an attack class, and formal verification becomes tractable
because there are no allocation-failure paths to reason about.

**Deferred, deliberately, and it is not a dodge.** Of the three axes, it is the only one that
**retracts working code**: `crates/frames`, `crates/heap`, and `crates/slab` would leave the
kernel entirely. Those are four milestones that work and are well tested.

Capabilities plus a microkernel, with a kernel that still allocates its own page tables, TCBs, and
endpoints out of the heap we already have, is **exactly Zircon's model** and entirely coherent.

And untyped memory stays genuinely available, because it is **additive**: add `Untyped` as a
capability type, move the allocator to a userspace library. It is a fantastic milestone to reach
once IPC and servers already run, and a punishing one to attempt before. It is milestone 11.

### The rules this adds

| Rule | Why |
|---|---|
| **No ambient authority.** A process can only use what it was handed. | The whole decision. The moment one syscall takes a global name, Capsicum's fate is ours. |
| **No `fork`.** Spawn takes an explicit list of capabilities. | "Inherit everything" is the confused deputy with a default. And it is *less* code: no copy-on-write. |
| **No global namespace in the kernel.** No paths, no uids. | A name you can *say* is authority you did not have to be *given*. Paths can come back as a **userspace** convenience over a directory capability, which is what `fdio` is. |
| **IPC carries control; bulk data moves by mapping a frame capability.** | Copy twice and we are Mach. |
| **A capability's rights may only be narrowed on delegation, never widened.** | Otherwise delegation launders authority and the whole model is theatre. |

Rule 4 of §4 ("a driver never reaches into a kernel global") was an option bought on day one,
before there was code, for exactly this moment. `drivers/pl011.rs` takes a base address and knows
nothing else. **That driver is already shaped like a process.** Milestone 8 makes it one.

## 11. SMP: per-CPU run queues, message-based migration. §6, reopened.

Decided 2026-07-22. This reopens §6, which chose single-core and named the cost: "a scheduler
rewrite later, accepted knowingly." This is that rewrite.

§6's caution was against building per-CPU structures *while still single-core*, when the need was
speculative. Going multi-core makes the need real, so per-CPU run queues are the design now, not
premature insurance. We build the per-CPU design directly rather than staging through an
intermediate global-lock scheduler.

The one real fork, how cores share scheduling work, was decided against work-stealing and for
**message-based migration**: no core ever touches another core's run queue; work moves by a message
to the target's inbox and an SGI. This keeps scheduling coherent with the rest of the kernel, where
coordination is already IPC (§10) and an interrupt is already a message (9a), and it makes the
cross-core race class unrepresentable instead of merely guarded. The trade accepted: no pull-based
load-balancing, and migration costs an IPI, neither of which matters on a 4-core QEMU box.

### What is already SMP-safe, and what is not

Two earlier decisions paid forward, and the starting point is cleaner for it:

- **`IrqSafeMutex` is already a real cross-core spinlock.** Its inner primitive is `spin::Mutex`
  (sync.rs), which provides mutual exclusion and the acquire/release fences on lock and unlock.
  Anything touched under a lock is already correct across cores. §9's "every kernel lock is an
  `IrqSafeMutex`" was SMP groundwork we didn't label as such.
- **TLB invalidation is already broadcast.** Every `tlbi` we emit is the inner-shareable form
  (`vmalle1is`, `vaae1is`); `flush_tlb`'s own comment says "wait for every core." aarch64's DVM
  broadcasts invalidation in hardware, so cross-core TLB shootdown needs no IPI for the cases DVM
  covers. This is a place aarch64 is simply better than x86, where shootdown is an IPI storm. §4
  rule 4 ("assume weak ordering") banked exactly this.

What has no SMP story, the four gaps:

1. **Secondary bring-up: none.** Cores 1..n park in `wfi` at `boot.s` with no wake path. No PSCI.
2. **Per-CPU storage: none.** `TPIDR_EL1` is unused; there is one boot stack in `link.ld`.
3. **`HELD_RANK` is a single global** (sync.rs). A second core clobbers it and the lock-rank
   assertion starts firing on phantom violations.
4. **`SGIR` / `ITARGETSR` are hardcoded to core 0** (gic.rs).

### The design

**Per-CPU identity via `TPIDR_EL1`.** Each core holds a pointer to its own per-CPU block in
`TPIDR_EL1`, set once during that core's init; `cpu::current()` reads it back. `MPIDR_EL1`'s
affinity gives the physical id at bring-up, mapped to a dense logical `0..N`. This is the standard
aarch64 per-CPU base; Linux uses `TPIDR_EL1` identically.

**The per-CPU block.** One `PerCpu` per core, in a fixed `[PerCpu; MAX_CPUS]`: its run queue, `current`,
`idle`, `need_resched`, held-rank, timer counters, and a cross-core **inbox**. Everything except the
inbox is touched by that core alone.

**The core rule: no core ever touches another core's run queue.** A run queue is single-owner. The
only way work reaches core B is a **message** to B's inbox followed by an SGI; B drains its own inbox
into its own queue. This is the same "coordination is a message" paradigm as IPC (§10) and
interrupt-as-message (9a), now applied to scheduling. It makes the entire class of cross-core
run-queue races **unrepresentable** rather than defended-against, the same move as §9's rank
assertion and `TlbFlush`'s `Drop`. We chose this over work-stealing deliberately (see the design
alternatives discussion): stealing means shared mutable queues and cross-core locking, and
message-based migration is the coherent fit for a kernel whose whole thesis is that coordination is
IPC.

**Two consequences fall out:**

- **The run queue needs no cross-core lock at all.** Only its owning core reads or writes it, and
  reentrancy from that core's own timer/IRQ is handled by masking IRQs around the access. §9's
  `IrqSafeMutex` masks IRQs *and* spinlocks; here the spinlock half is simply unnecessary. A per-CPU
  `VecDeque` behind IRQ-masking, not a `spin::Mutex`.
- **The hot path holds no lock.** `schedule()` pops from its own queue (IRQs masked) and switches; it
  touches no global structure. To make that true, a run-queue entry is a small
  `RunNode { tid, ctx: *mut Context, kstack_top, ttbr0 }` carrying everything a switch needs, cached
  at enqueue. The `Thread` box stays owned in the global `threads` map (for lookup and reaping), and
  the raw `ctx` pointer is valid because a thread leaves every queue before the reaper frees it. This
  is the "decouple" answer to the run-queue↔global-map ordering question: the map is off the hot path.

**What stays global, behind a lock:** the `threads` map (Tid → Thread; owner and directory, touched
on spawn/reap, not on the switch) and `endpoints` (IPC rendezvous; shared, because a send on one core
wakes a receiver bound to an endpoint). Neither is on the scheduling hot path.

**The inbox is the one cross-core structure.** A per-core `IrqSafeMutex<VecDeque<Tid>>`. A producer
(another core) locks it, pushes a Tid, unlocks, and SGIs the target. The owner locks it, drains into
a local, unlocks, then enqueues into its own run queue (no lock). Touched only on migration, which is
rare; the hot path never sees it. (Lock-free MPSC inbox is a later exercise; a tiny spinlock is the
correct first cut.)

**Lock ordering.** With no run-queue locks, the surface is small:

- **`THREADS` and `ENDPOINTS` rank above `INBOX`.** Spawn or IPC-wake finds/creates a thread (holding
  THREADS or ENDPOINTS), then pushes to a target inbox. Always that order.
- **Inboxes are equal rank and never nested.** A core locks at most one inbox at a time (the
  target's), so §9's rule that `R < R` is false forbids the only possible cycle.
- **`HELD_RANK` becomes a `PerCpu` field.** Each core tracks its own; `force_reset_ranks` resets only
  the caller's.

**Placement and waking.** Prefer the **current core**: a `spawn` or an IPC-wake whose thread can run
here just enqueues locally, no lock, no IPI. Only when a thread must run elsewhere (spreading across
idle cores at spawn, or waking a thread whose target core is idle in `wfi`) do we message the target
inbox and SGI it; the SGI handler drains the inbox and re-runs `schedule()`. That is also the
reschedule-a-remote-core primitive. Spreading policy stays trivial (round-robin idle cores);
balancing cleverness is unmeasurable on QEMU.

**Bring-up via PSCI.** QEMU `virt` implements PSCI. Core 0, after its own init and once the heap
exists, calls `PSCI CPU_ON` (via `SMC`) for each secondary, passing an entry point and a per-core
stack **allocated from the frame allocator** (the heap is up by then, so no static stack array).
Each secondary sets `sp`, sets `TPIDR_EL1`, enables its own GICC (PMR + CTLR) and its timer PPI,
then enters the scheduler and runs its idle thread.

**Memory ordering, as one invariant.** The rule that keeps this tractable:

> **Per-CPU state is touched only by its own core. All cross-core work movement is exactly: lock
> the target's inbox, push a Tid, unlock, SGI.**

The inbox's `spin::Mutex` supplies the acquire/release fences for the Tid handoff, and the SGI is an
event the receiver observes only after the push is visible. So the per-CPU lock-free atomics
(`need_resched`, `idle`) stay single-core-accessed and need nothing above `Relaxed`. The audit is
mechanical: any lock-free atomic read or written by more than one core either becomes per-CPU or gets
Acquire/Release. The known suspects, all `Relaxed` today (`NEED_RESCHED`, `IDLE_TID`, the timer
counters), all become per-CPU, which resolves them.

**GIC.** The SGI is now the migration primitive, so it matters more than I first framed. Parameterize
`send_sgi(intid, target)` off the core-0 hardcode; each core runs its own GICC enable + PMR. SPI
routing (`ITARGETSR`) stays on core 0: the only sources are the per-core timer PPI (needs no routing)
and virtio SPIs (one core fields them). The timer being a PPI means preemption is already per-core for
free.

### Build order (done, 2026-07-22)

The migration path came online *with* the queues, not after: there was no separate race-prone
stealing phase to bolt on, because we are not stealing. All of this landed and passes under
`-smp 4` (91 kernel tests):

1. **Per-CPU infrastructure** ✅ (3a, 3b-i). `TPIDR_EL1`, the `PerCpu` block, `cpu::current()`, and
   `HELD_RANK` / run queue / `current` / `idle` / `need_resched` → per-CPU. Behavior-neutral on one
   core, verified in isolation.
2. **Secondary bring-up** ✅ (step 2). PSCI `CPU_ON`, per-core stacks. Cores come up and idle.
3. **Secondaries schedule** ✅ (3b-ii). Per-core idle thread, GIC CPU interface, timer, and the fine
   map; the reaper fixed to run after the switch, not during. Each core schedules from its own queue.
4. **Cross-core migration** ✅ (3c). The inbox + reschedule SGI; `spawn_on(core, f)` places work on
   any core. The memory-ordering invariant held: the inbox lock's release/acquire orders the handoff,
   no extra barriers needed.

**Remaining, and deliberately deferred:** wiring `spawn` itself to round-robin over `spawn_on` (auto
load-balancing). The mechanism is done; making it the default placement policy would scatter the
existing tests' threads across cores and make their yield-based synchronization timing-dependent, so
it wants those tests audited first. Also still deferred (unchanged): per-CPU allocator caches for
*scalability* (§9), which are a different problem from correctness.

Three SMP-latent bugs, each invisible on one core, surfaced during 3b-ii and are recorded in that
commit: `VBAR_EL1` is per-core (a secondary that never set it died silently on its first trap); the
per-core boot stacks were an immutable `static` that landed in read-only `.rodata`, which only bit
once the fine map enforced W^X; and a global tick counter, advanced by every core, broke "holding a
lock masks *my* timer." The clean starting point (`IrqSafeMutex` already a real spinlock, every
`tlbi` already inner-shareable) is why there were only three.

### Testing

`-smp 4` in `qemu-runner.sh`. New invariants, each proving something one core could not: a shared
counter incremented by threads on multiple cores under a lock sums **exactly** (cross-core mutual
exclusion); a spawned thread runs on a core other than the spawner (the inbox/SGI path actually
delivers work); an IPC send on one core wakes a receiver that runs on another; the per-CPU rank
tracking does not false-positive under concurrent locking. The semihosting exit stays single-caller:
core 0 drives the runner, the others idle at suite end.

### Risks, named

The race that eats SMP schedulers, two cores mutating one run queue, is **gone by construction**: no
core touches another's queue. What is left is smaller and more legible: the inbox handoff (a Tid
published under a lock, consumed after an SGI), the memory ordering of that handoff, and PSCI
bring-up. First-encounter weak-memory bugs are still heisenbugs, so the ordering invariant above is
kept deliberately narrow. This is still the hardest debugging in the project, but the single-owner
choice removed its worst part.

### Out of scope

**Work-stealing** (pull-based migration, an idle core reaching into a busy core's queue) is
deliberately not built: it is the shared-mutable-queue design we chose against. It stays available as
a contained later exercise ("replace the inbox push with a stolen queue") once the foundation is
solid. Also out: CPU affinity/pinning, NUMA, CPU hotplug, per-CPU reserve pools for allocation
scalability (§9 parked those separately), and any balancing cleverer than round-robin spread.

---

## 12. Call/Reply IPC: a one-shot reply capability

Decided and built 2026-07-22 (milestone 12). The design was worked out ahead of time in
notes/ipc-naming.md and parked in "Open design ideas" against two triggers. This is where it lands,
because it widens the §4 syscall boundary and so is owed a numbered decision.

### The gap it closes

IPC names an endpoint and the sender is anonymous (notes/ipc-naming.md), so a server that `RECV`s a
request cannot reply to the *specific* caller. The workaround was a second reply endpoint wired per
client at spawn, correct only while a server's client set is static and it is single-threaded (the
console server). It does not serve anonymous clients, and nothing structural stops a misrouted reply,
a double reply, or a stale reply landing on a client that has moved on.

### The surface, and why it is this small

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

### What it buys, as kernel guarantees rather than server discipline

1. **Reply to an anonymous caller, no pre-wiring** — the kernel mints the cap; the server never knew
   the caller.
2. **One-shot** — consumed on use, so a second reply is `NoSuchSlot`. No double reply, no hoarding.
3. **This caller, not another** — `Reply(Tid)` names the exact blocked caller; misrouting is
   unrepresentable.

Three tests hold the line: `a_call_gets_a_reply` (round trip, one endpoint), `a_reply_reaches_the_
caller_that_called` (two callers outstanding, each gets its own reply), and, through the real syscall
path at EL0, `a_process_calls_a_server_and_the_reply_is_one_shot` (which also checks that the second
reply is refused).

### Deferred, deliberately

- **The call chain and priority donation.** seL4's Reply cap also threads a kernel call chain so the
  server runs on the caller's priority. cricker-os is round-robin with no priorities, so donation is
  moot; building the chain now would be machinery with no consumer (§4). It is the natural extension
  when priorities arrive.
- **Timeouts.** A server that never replies (or whose cspace is full, so the reply cap is dropped)
  leaves the caller blocked until torn down, the same no-timeout limitation as any lost reply today.

### One rule the mechanism assumes

A `CALL` endpoint is served with `RECV_CAP`. A plain `RECV` cannot furnish the reply capability, so
the kernel delivers the words but leaves the caller blocked rather than wake it with its own request
masquerading as a reply. Servers use the right method by protocol; the guard is there so misuse hangs
(bounded by the no-timeout gap above) rather than mis-serves.

---

## 13. Capability revocation and untyped reclamation (frames)

Decided and built 2026-07-22 (milestone 13). The direction was parked in "Open design ideas" and
notes/capability-lifecycle.md; the concrete mechanism is designed here, because it is a
capability-model change gated on a memory-safety precondition.

### What it closes

A granted capability could not be retracted and a spent page could not be reclaimed. That was safe
only by a structural accident: retyped frames are spend-only and never reused, so a peer that still
mapped a shared frame after the granter left was mapping valid, non-reused memory. `untyped::destroy`
carried a tripwire spelling this out: wiring up any reclamation before revocation exists turns those
dangling mappings into a use-after-free.

### The scope: frame revocation, not the full tree

seL4 keeps a capability-derivation tree and revokes a *subtree* (revoke Bob's copy while keeping
Alice's and my own). We build the **unmap side only** and revoke *all* derivatives of a page, which
is exactly what reclamation wants and is the memory-safety-critical half. The full tree buys subtree
granularity, which nothing on the roadmap needs, so by §4 it waits for a driver. This is a considered
terminal design, not a way-station: if subtree revoke is ever required, the machinery here
(unmap-from-any-address-space, the revoke-before-reclaim discipline, `untyped::destroy`) is reused
unchanged, and only the index (an object-to-holders list) is rebuilt as a tree. design/roadmap.md
has the argument.

### The mechanism

A **mapping database, lite** (revoke.rs): every mapping of an untyped-derived page, `(phys, root,
va)`, recorded at `Untyped::MAP` and `Frame::MAP`, and forgotten when an address space is torn down
(so a stale root is never walked after its tables are freed and reused). To revoke a page:

1. Delete every `Frame` capability to it from every cspace, so no `Frame::MAP` started afterward can
   re-establish a mapping.
2. Unmap it from every address space that held it, with the broadcast TLB flush we already use, so
   SMP and the no-ASID case are covered.

Two entry points: **`Frame::REVOKE`** (a method needing `GRANT`, the un-share trigger; it does not
reclaim, since the untyped is spend-only) and **`untyped::destroy`** (revoke every mapped page in the
region, then return the pages to the allocator, the reclaim trigger). Reclamation is now safe because
"no live mapping survives" replaces "spend-only, never reused".

### Tests

`revoke_unmaps_a_shared_page_from_every_address_space` (a page mapped in two address spaces is
unmapped from both), `destroy_unmaps_a_region_before_reclaiming_it` (the tripwire's use-after-free
made impossible: the mapping is gone before the frame returns to the allocator), and, at EL0,
`a_process_revokes_a_frame_and_loses_the_capability`.

### Deferred, deliberately

- The full capability-derivation tree and subtree-granularity revoke (above).
- Revocation of non-memory objects (endpoints, IRQs): no unmapping, just cspace removal, and less
  urgent since they are not the memory-safety seam.
- Reclaim-on-process-death: an additive step now that explicit `revoke` and `destroy` are safe.
- Returning a single revoked frame to a reusable pool: the untyped is still a spend-only bump
  allocator, so `Frame::REVOKE` un-shares but does not reclaim; `untyped::destroy` reclaims a region.

### The one honest race

A `Frame::MAP` in flight on another core, between a revoke's cap-delete and its unmap, can slip a
mapping past the revoke. seL4 closes this with a mapping-database lock held across the whole
operation; that lock is the deferred full-database machinery. Named, not hidden.

---

## 14. The project's direction: a verified-Rust capability microkernel that runs real workloads

Committed 2026-07-23. This is the North Star, recorded because everything downstream (which
milestones are on the critical path, what "done" means) now answers to it. It does not replace the
learning ethos (CLAUDE.md: understanding over velocity); it gives that learning a destination and
settles the forks that were parked *because* the project was "just learning."

### The differentiator, stated precisely

The goal is **a verified-Rust capability microkernel: a small, machine-checked trusted core that
hosts real, unverified workloads with strong isolation guarantees.**

The precision matters, because the obvious phrasing ("a verified capability OS that runs real
workloads") is *already seL4*: verified C, capabilities, running Linux VMs and safety-critical
components in real deployments. The edge that is not already taken is **verified in Rust**. seL4's
proof carries the entire safety burden because C gives it nothing; here the language already removes
the ~70% memory-safety class and machine-checked proofs cover the security-critical logic on top. A
verified-Rust capability kernel that runs real workloads is a live research frontier no shipping OS
occupies. The Rust angle is the whole novelty, which is why the existing choices (capabilities,
share-not-move, no fork, memory safety as a language property) were the right seed.

### The shape that makes "verified" and "real workloads" compatible

They pull opposite ways: "verified" wants a tiny kernel, "real workloads" wants a large system.
seL4's resolution, adopted here: **verify the small microkernel TCB; run real, unverified workloads
in confined userspace on top.** What is promised is not "the whole system is proven" but "the trusted
core is proven, and it confines everything above it, so a compromised workload cannot escape." The
microkernel structure was built for exactly this.

### How we verify, and the evidence it is tractable

**Kani** (bounded model checking for Rust), chosen over an Isabelle/HOL refinement proof (seL4's way,
person-decades, not a solo endeavor). The experiment that earned the choice is in the tree: five
harnesses in `crates/caps` prove the capability model's core theorems for *every* input rather than
sampled cases, including "`derive` never widens rights" and "userspace cannot forge a right"
(`script/verify`, notes/verification.md). It installed and ran in minutes, and the proofs read like
the properties they state. That is the green light.

Verification spreads **inward from the capability core**: the `caps` logic now, then IPC (the
rendezvous and the one-shot reply), then the MMU isolation invariants. Pure-logic crates (§7) are the
natural frontier because they already compile for the host; the proofs live behind `#[cfg(kani)]` and
never touch an ordinary build. **(Milestone 18 delivered all three steps**: the rendezvous state
machine is extracted and proved and the scheduler runs it; the one-shot Reply's mechanism is proved
in `caps` and `ipc`; the MMU isolation invariants, including the user-VA gate the syscalls now call,
are proved in `paging`. See notes/verification.md for what each proof says and what stays on tests.)

### What this resolves and what it changes

- **The verification-endgame fork (design/roadmap.md) is resolved: verification is the goal.** So
  milestone 14 (remove the kernel heap) moves from optional purity to **prerequisite** on the
  critical path: a verifiable kernel cannot allocate dynamically.
- **A verification track becomes first-class**, spreading proofs inward as above.
- **"Real workloads" becomes a named track** with its own sub-decision (a native-ABI target first, a
  Linux-compat personality or VM hosting later), replacing the old "POSIX posture" fork, which was an
  optional study back when reach did not bind. It binds now.

### Calibration, the honest limits

- **Not** a from-scratch seL4-scale functional-correctness proof of the whole kernel. That is
  person-decades. The target is machine-checked proofs of the security-critical core, which is both
  novel (in Rust) and reachable.
- **Staged ambition.** The near-term deliverable is the *demonstrator*: a verified core running real
  confined workloads. A general-purpose competitor is an explicit *later optionality*, not the current
  goal; the competitor questions stay parked until the demonstrator earns them.
- **Still a learning project.** The destination is committed; the method (write it together, explain
  the hardware, write the notes) is unchanged. A demonstrator he cannot explain is a failed
  demonstrator.
- **init is the privileged unverified component, and that is a known soft spot.** "Verified core,
  confined unverified workloads" is honest about the *kernel*, but init (which builds every other
  process) is unverified and privileged. The kernel confines it and a compromised init cannot break
  the kernel or escape confinement; but init's bytes are loaded unsigned today and its authority is
  broad. Milestone 22 (design/roadmap.md) is where this is closed: verify init before it runs, and
  shrink what a broken one can do. Recorded here so the thesis is not read as claiming more than it
  proves.

---

## Open design ideas

Not decisions yet. Proposals with real open questions, parked deliberately.

The [post-v1 milestone roadmap](design/roadmap.md) sequences the buildable ones below into
proposed numbered milestones (12+) and names the two decisions they force (the verification
endgame, and POSIX posture). The entries here remain the detailed source for each.

- [Microarchitecture-variant binaries](design/fat-binaries.md) — our targets straddle the
  ARMv8.0 / ARMv8.2 line (no LSE atomics on Cortex-A72, LSE on everything newer), and with
  no libc we can't lean on LLVM's `outline-atomics` to paper over it. Milestone 6 forces
  the kernel-atomics question; milestone 7 is where a fat userspace format would be
  decided. Feature detection via the `ID_AA64ISAR*_EL1` registers is worth building at
  milestone 2 regardless.

- [Driver domains, and the DMA-confinement design space](design/driver-domains.md) — the
  principled version of the DMA hole we closed in software (notes/dma.md): run each driver in its
  own VM with cricker-os as the hypervisor at EL2, and confine its DMA with the SMMU's stage-2. The
  strongest driver isolation there is, and the opposite of a shortcut: it needs EL2, an SMMU
  driver, and is impossible under HVF. Parked as the most interesting unbuilt direction.

- **Call/Reply IPC: a kernel-minted, one-shot reply capability** (notes/ipc-naming.md). IPC names
  an endpoint and the sender is anonymous, so a server cannot reply to a *specific* caller. Today
  we wire an explicit reply endpoint per client at spawn. seL4 mints a one-shot `Reply` cap on
  `Call` so a server can answer whoever called, with a kernel-tracked call chain that also enables
  priority donation. We can emulate reply-to-caller with `SEND_CAP` (the client passes a
  reply-endpoint cap in the request), but *not* the one-shot safety or the call chain: those need a
  `Reply` object and a `Call` method, which widen the §4 syscall surface and so should not be added
  speculatively.

  **Two triggers to build.** *Functional:* the first server that must serve clients it was not
  individually wired to (a general RPC service). *Safety:* the first reply whose correctness depends
  on going to **this** caller (caller-identity) or on being consumed **exactly once**. The
  distinction matters because a pre-wired reply endpoint is reusable and nameable, so nothing
  *structural* stops a reply reaching the wrong caller, a double reply, or a stale reply landing on
  a client that moved on. A one-shot kernel-minted reply cap makes "exactly one reply, to exactly
  this caller, consumed on use" a kernel guarantee instead of a server discipline.

  **Where we stand today (checked, 2026-07-22):** safe, but by *convention*, not guarantee. The
  console server shares one `reply` endpoint across clients yet is correct because it is
  **single-threaded** and IPC is synchronous rendezvous: it handles one request-reply cycle at a
  time, so the only client in `RECV(reply)` when it replies is the one it just served. Workers and
  drivers use a **per-request** result endpoint (no sharing). The safety trigger fires the moment
  either of those stops holding: a server **thread pool** on a shared reply path, or pipelined /
  asynchronous requests.

  **Built at milestone 12 (§12).** The shape sketched here is exactly what landed: a `CALL` method and
  a one-shot `Object::Reply(Tid)`, kernel-minted at the rendezvous, delivered through `RECV_CAP`, and
  consumed on use. The call chain and priority donation are deferred (moot without priorities); the
  detail above stays as the design record.

- **Capability revocation, and untyped reclamation** (notes/capability-lifecycle.md).
  **Built at milestone 13 (§13), scoped to frame revocation.** A `Frame::REVOKE` method and
  `untyped::destroy` now unmap a page from every holder and delete every capability to it, which is
  what met the precondition below and let reclamation land. The full capability-derivation tree (for
  subtree-granularity revoke) is deferred, not on the path to an inevitable rewrite; see §13 and
  design/roadmap.md. The rest of this entry is the pre-§13 design record.

  A granted
  capability cannot be retracted: no capability-derivation tree, no refcount, no `revoke`
  (untyped.rs). This is **not a memory-safety hole** — frames come from spend-only untyped and
  teardown never frees a shared leaf, so a surviving peer maps valid, non-reused memory — but it
  means you cannot *un-share* a frame from a live peer (only destroy the peer) and never *reclaim*
  the page. seL4's mechanism is a capability-derivation tree plus a recursive `revoke` that unmaps
  the object from every holder; expensive and kernel-tracked, which is why it is a first-class
  object there and "the harder story parked for later" here. **Trigger to build:** needing to
  retract authority from a live, untrusted peer, or to reclaim untyped on process death.

  **BLOCKING PRECONDITION on any reclamation work.** The "not a memory-safety hole" conclusion
  rests entirely on one invariant: **retyped frames are spend-only and never returned to a reusable
  pool.** So *any* future reclamation — wiring up `untyped::destroy`, a frame free-list, an
  allocator that recycles, or the reclaim-on-process-death above — is **blocked on revocation
  landing first.** The instant a shared frame can be reused while a peer still maps it, every
  dangling mapping this entry calls "harmless" becomes a use-after-free. This is the classic seam:
  two individually-correct changes, months apart, whose *interaction* is the hole. `untyped::destroy`
  already exists, unused, as exactly that trap; it carries the same warning at the code, so the
  person who eventually wires it (thinking about untyped accounting, not shared-frame lifetimes)
  meets the precondition there too.

---

## Milestones

Each rung is independently demoable. The dividing line between "a Rust program that
boots" and "an operating system" is milestone 7.

| #  | Milestone                                      | What it teaches                          |    |
|----|------------------------------------------------|------------------------------------------|----|
| 1  | Boot to Rust on QEMU `virt`, print to UART      | Freestanding binaries, linker scripts    | ✅ |
| 2  | Exception vectors, handlers, fault reports      | ARM privilege model, exception dispatch  | ✅ |
| 3  | Physical frame allocator from the memory map    | Where RAM actually comes from            | ✅ |
| 4  | MMU on: page tables, address spaces, kernel heap| Virtual memory, `alloc` in `no_std`      | ✅ |
| 5  | GIC + timer interrupts                          | The preemption source                    | ✅ |
| 6  | Kernel threads, context switch, scheduler       | Stacks, register files, run queues       | ✅ |
| 7  | **EL0, address spaces, CSpaces, ELF loader, IPC** | **The actual OS boundary.** Decided in §10  | ✅ |
| 8  | **The console driver LEAVES the kernel**        | The microkernel thesis, executable        | ✅ |
| 9  | virtio-blk in userspace + a filesystem server   | Userspace drivers, MMIO caps, IRQ-as-message, DMA | ✅ |
| 10 | A process server, and a shell that spawns binaries | Proof the whole stack works            | ✅ |
| 11 | Untyped memory: a process allocates, the kernel does not | §10's deferred axis, to the extent §10 intended. | ✅ |

Milestone 8 is the one that proves §10 was real. When it lands, **the kernel no longer knows
what a UART is.** If we cannot take the console out, we did not build a microkernel; we built a
monolithic kernel with an unusual syscall table.

Milestone 11 is complete *to its intent*, not to seL4's. The kernel still allocates its own
page tables, TCBs, and endpoints from the heap; §10 chose that deliberately (Zircon's model).
What 11 demonstrates is the half that was the point: a userspace process spends pages out of an
`Untyped` capability and **the kernel's free-frame count does not move**, so a process cannot
force the kernel to allocate, and kernel-memory exhaustion stops being an attack class. Taking
the allocators out of the kernel entirely stays additive and unbuilt.

### Beyond the plan (post-v1)

The eleven milestones are the plan. Work since, in git order: a security audit
(notes/security.md); per-process spawn quotas (notes/quotas.md); kernel-mediated DMA
confinement, since QEMU `virt` has no IOMMU (notes/dma.md); capability delegation between
processes via `SEND_CAP`/`RECV_CAP` (notes/delegation.md); frame capabilities, shared memory a
process owns and delegates (notes/frames.md); SMP (§11); Call/Reply IPC, a one-shot reply capability
(§12, milestone 12); and capability revocation with safe untyped reclamation, scoped to frames (§13,
milestone 13).

**The road past v1** is sketched in [design/roadmap.md](design/roadmap.md): proposed milestones
12-17 and the two decisions they force. Milestone 12 (Call/Reply, §11's sibling in getting its own
decision entry before code) is the first of them built; the rest stay proposals until started.

Deliberately out of scope for v1: a writable filesystem, networking, a GUI, dynamic linking.
Each multiplies debugging difficulty and none teaches something the first ten don't already set
up. SMP and real hardware, listed here originally, are now on the table.

## 15. The native ABI: formalize the convention, defer the BootInfo (milestone 19e)

Decided 2026-07-25, at milestone 19e ("Decision 2" in design/init-and-granular-spawn.md), against a
system that could finally run and deliver distinct programs (19f). The full contract is written up in
notes/abi.md; this records the decision and why.

§10 already settled the model (capability-based, not Unix). What 19f forced open was the smaller
question: what is the contract between a program and the system? The syscall convention (`svc`, four
numbers, everything through `SYS_INVOKE`) and the object surface were already built and stable. The
one genuinely open piece was **how a program meets its initial capabilities and arguments**.

**The decision: write down the convention we already run, and do not build a self-describing
environment yet.** A program is entered at `_start(x0, x1, x2)` with its cspace pre-populated by its
loader at slots the program hardcodes, per a contract published in that program's own source.

Rejected for now: a **BootInfo** page (seL4's model), a structured block the loader hands the program
describing its capabilities and arguments, so the program *discovers* its world instead of assuming a
layout. Not rejected because it is bad; it is the right tool. Rejected because it is a mechanism
without a requirement here: init builds every program and knows every layout, so out-of-band
agreement between one parent and its own children is sufficient, and it is exactly what seL4 does for
every task below its root. BootInfo earns its place when a loader must start programs it did not build
and whose layout it cannot know, which is milestone 23 (live component replacement, competing
vendors). Building it now would be an abstraction ahead of its requirement, which rule 3 and the §5
asymmetry both warn against.

The coupled half, **what runs first**: a native compute workload (CoreMark), because the disk is
still blocked (milestone 16) and compute is the honest "real workload" a program can do now. Native,
not Linux-compat: §10 records that a POSIX shim is *additive* and can come later without a rewrite, so
there is no reason to pay for it before running something native.

## 16. Object revocation: reclaim the objects a process built (extends §13)

§13 revoked **frames**. This extends the same idea to **kernel objects** (TCBs, address spaces,
endpoints), so a process can be torn back down and its memory returned, the reclamation a
run-workloads-that-come-and-go system needs. Full reasoning in notes/object-revocation.md.

### The model

**Region ownership plus generational staleness, not a capability derivation tree.** An object's
lifetime is its backing region's lifetime; reclaiming a region frees each object's registry slot, and
because objects carry generational names (§14, `crates/slots`), every outstanding capability to them
goes stale on next use with no capability to hunt. This is coarse (reclaim a region, kill every object
in it at once; no per-delegation revocation) and that is the right authority semantic here. The CDT is
a later, purely-additive layer if fine-grained revocation is ever wanted; the rework to add it is
near-zero, because the per-object teardown and region reclamation it would call are what we build now.

### The trigger, and the lock constraint that shaped it

Reclamation is explicit: `Untyped::DESTROY`, invoked by the region **owner** (who holds the untyped
cap), never automatic on thread exit, because a region belongs to its owner, not to the thread that
occupies it, and reclaiming memory is an authority a capability system grants only through a capability
held. Thread exit does the live-state teardown; the owner's destroy does the memory. `destroy` must
never take `SCHED` (it is reachable from `AddressSpace::Drop` under the reaper's `SCHED`), so the
`SCHED`-taking object reap is a separate caller-driven step and `destroy` stays `SCHED`-free.

### Two new methods on the Untyped object (the surface stays three syscalls)

- **`SPLIT`** carves a child untyped off a parent's budget (seL4's untyped-retype-into-untyped), so a
  spawner gives each child its own reclaimable region. A parent with live children cannot be destroyed
  (freeing its run would double-free a child's pages), tracked by a child count so the parent becomes
  destroyable again once its children are gone. **Return-of-pages is LIFO:** a child destroyed at the
  top of the parent's watermark gives its pages back to the parent's budget (un-bump), which is exactly
  what a spawn-then-reap loop does, so a split parent is *not* committed for its lifetime; a child freed
  out of order leaves a hole until the parent itself is destroyed. This is the LIFO half of seL4's
  return-to-parent without the derivation tree that would handle the general case.
- **`DESTROY`** reclaims a region and every object retyped from it. Refuses (NotPermitted) while a live
  thread occupies it, an endpoint in it has a blocked waiter, or it has been split.

### Also

Region indices became **generational** (`destroy` reuses the slot), retiring the old cap where the
kernel could create only 256 regions in its whole lifetime. **Endpoint revocation wakes a blocked
waiter with an error:** revoking an endpoint drains its wait queues, marks each waiter aborted, and
wakes it, so its blocking `ipc_recv`/`ipc_send` returns an error (the endpoint is gone) rather than
stranding the reclaim or dangling on a freed page. `endpoint_of` became fallible so a stale endpoint
capability fails cleanly instead of panicking; the check folds into the existing IPC locks, so the
hot path does not regress. The EL0 `lat_proc` spawn benchmark also landed (notes/benchmarks.md):
cricker-os builds a process faster than Linux or macOS, with the honest caveat that a
capability-microkernel process is a lighter object than a Unix one.

## 17. The second architecture: RISC-V, and the page-table format trait

The port to RISC-V (rv64, QEMU `virt`) is the first real test of rule #1 ("all architecture-specific
code lives under `arch/`"), an assumption held on faith since milestone 1. RISC-V over x86_64 because
it is clean-different rather than legacy-different: it exercises the HAL abstraction (a different
trap, paging, interrupt, and firmware model) without the real-mode / GDT / IDT / APIC / UEFI tax.
x86_64 is the third port. Full plan and findings: notes/riscv-port.md.

The port found the `arch/` boundary was **almost** complete, and each leak it exposed was pushed under
`arch/`, not bodged around:

- **`thread::Context`** was aarch64-register-shaped in portable code. Now arch-owned, behind
  intent-named constructors (`Context::for_kernel_thread`, `for_user_thread`); `thread.rs` names no
  register.
- **The userspace-entry path** (the EL0/U-mode trap frame, the enter-userspace glue, cache
  coherence) lived in `user.rs`. Now `TrapFrame::for_user_entry`, `arch::enter_user`,
  `arch::sync_icache`, `arch::current_sp` seams; the hand-written aarch64 demo programs are gated to
  aarch64 (RISC-V reaches U-mode through the ELF loader).
- **The `paging` crate** encoded the aarch64 descriptor format outright.

**Decision on `paging` (the significant one, Chris steered it):** generalize behind a `PageFormat`
trait rather than duplicate the walk or write a separate RISC-V pager. `Flags` became a
format-neutral capability set (write / user / user-exec / kernel-exec / global / device) with the
same constructor and predicate API; the `Mapper` walk (`map`/`unmap`/`translate`) is written once and
generic over `F: PageFormat`; each format (`Aarch64`, `Sv39`) supplies only `LEVELS`, the half split,
and the handful of encode/decode operations. **The walk is proved once and the encoding proved per
format:** the aarch64 and Sv39 modules each carry Kani proofs of index-in-bounds, address/permission
separation, and the half split, and the shared walk inherits both. The alternatives (a duplicated
sibling module, or a fresh un-verified RISC-V pager) were rejected because only the trait gives RISC-V
paging the same formal verification aarch64 has, which is the demonstrator's whole point. The cost was
real (it touched a verified crate and every `Flags` consumer) but landed with aarch64 fully green.
Base Sv39 has no device-memory PTE bit, so `CAP_DEVICE` rides in an RSW (software) bit to keep the
`Flags` round-trip exact; real hardware would use Svpbmt. Portable code that must name the format (the
user-VA gate, the user `Mapper`) refers to `arch::mmu::Format`, so the choice lives in `arch/`.

**The whole capability core runs on RISC-V today.** Boot (higher-half Sv39, `.bss`, the `tp`
per-CPU register, an NS16550 console, the OpenSBI device-tree handoff), the MMU (fine-grained W^X
Sv39 tables + `satp`, the single-`satp` process model where every root carries the kernel high half),
traps (`stvec`, the `sscratch` dance, the syscall-ABI reconciliation), the SBI timer, the scheduler
and context switch, U-mode user programs making syscalls, a capability invocation (`SYS_INVOKE` →
lookup → rights check → endpoint SEND) from U-mode, **preemption** (the timer preempts a thread that
never yields, DECISIONS §5), and **a real compiled ELF at U-mode** (the `worker` Rust binary,
delivered as the initrd and run through the kernel's arch-neutral ELF loader, squares an input and
SENDs it home). The ELF step needed three small aarch64 assumptions closed: `user_rt` grew a RISC-V
syscall ABI (`ecall`+`a7`), the `elf` crate accepts the running kernel's machine (a symmetric,
cfg-selected `EXPECTED_MACHINE`, so each kernel refuses the other's binaries), and one trap
instruction in the program was arch-gated; the loader itself was already portable. The port surfaced two RISC-V-specific bugs worth recording: `tp` is
a general register, not a system register, so a U-mode trap must restore the kernel `tp` from a known
source rather than trust it (notes/riscv-port.md); and the shallow TCB entry path let a trap frame
placed at the top of the kernel stack overlap `enter_frame`'s own frame, fixed by placing the frame
below the live `sp`. **Userspace init builds the system too:** from a crickerfs
archive holding `init` (a portable minimal builder) and `worker`, the kernel loads only `init`, maps
the archive into it, grants it a budget and a report endpoint, and `init` loads `worker` by name and
builds it as a child through the capability verbs, wiring it to the report. The kernel never touches
the child's bytes. That exercised the full userspace capability syscall surface on RISC-V.

**Device interrupts flow through the PLIC too:** a
keystroke raises the NS16550's line into the PLIC, which delivers a supervisor external interrupt;
the handler claims it, masks the source, and notifies the endpoint it is routed to, and a driver
blocked there wakes, reads the byte, and re-arms. This is the real second consumer the
interrupt-controller abstraction was waiting for.

**The last leak is closed.** The device driver moved to userspace (an unprivileged process holding
an `Irq` capability and a device mapping, running the seL4 `WAIT`/read/`SEND`/`ACK` loop), and its
ACK forced the interrupt-controller seam. Then `arch::irq` was extracted with two working consumers
behind it, the GIC and the PLIC, and `drivers::gic` was gated to aarch64. Every portable caller names
`arch::irq`; the only code naming `drivers::gic` is aarch64 arch code and `cfg(test)` aarch64 tests.
The order was deliberate: prove a real interrupt through the PLIC, then a userspace driver exercising
the ACK on both arches, then extract, so the abstraction was factored out of two exercised
controllers rather than guessed ahead of one.

**The RISC-V port is complete.** Every primitive that defines the kernel runs on both aarch64 and
RISC-V from one portable core: boot, the MMU, traps, the timer, the scheduler, preemption, U-mode
user programs, capability invocation, userspace-built processes, and device interrupts serviced by an
unprivileged userspace driver. Rule #1 held: the second ISA was a new `arch/` directory, not a diff
across the kernel, with no exceptions left in portable non-test code.

## 18. The PCIe transport: one driver, two buses, the seam in the kernel

**Decided 2026-07-27, built the same night** (notes/pcie.md, notes/pcie-transport-scope.md). A PCI
root complex (ECAM enumeration, BAR placement, virtio-pci capability parsing, INTx through the
PLIC) and a virtio transport seam, so the same userspace block driver runs over virtio-mmio and
virtio-pci unchanged. The three decisions the scope note flagged, resolved as it recommended:

1. **INTx before MSI-X.** Legacy INTx is a wire into the PLIC, which is exactly the interrupt
   model the kernel already has (`bind_irq`, the `Irq` capability, WAIT/ACK). MSI-X is a later
   enhancement for a device that needs many vectors; nothing tonight does.

2. **One driver, two transports, and the seam lives in the kernel.** `virtio::Transport` answers
   the virtio-mmio register vocabulary against whichever bus the device sits on; the pci variant
   translates each name to the virtio-pci common-config layout, the read-to-ack ISR, and the
   resolved notify doorbell. The mmio vocabulary is canonical because it is what `abi::virtio`
   already exposes; nothing else about the choice is load-bearing. Everything above the seam (the
   shadow ring, the validator, the queue-layout contract, the userspace driver) is one copy and
   did not change. The security consequence is the point: the DMA confinement is written once and
   polices both buses, and PCI Bus-Master Enable (DMA permission at the bus level) is granted
   last, after the confined transport is fully described.

3. **virtio-mmio stays.** Neither board's working mmio path is migrated; PCIe runs **alongside**
   it, and the portability claim is proven rather than promised: the same crate and seam drive
   the disk on riscv (INTx via the PLIC, irqs 32..35) and on aarch64 (INTx via the GIC, INTIDs
   35..38, the highmem ECAM at 0x40_1000_0000). The per-arch cost was exactly the predicted
   constants-plus-map change, which is rule #1 doing its job on a whole subsystem.

**Build-vs-reuse, recorded late.** The `pci` crate was built rather than adopting `pci_types`
(the kernel-agnostic config-space/BAR/capability crate several Rust OS projects use), and the
call was made without the survey pass the reuse convention (notes/prior-art.md) requires; this
paragraph is the record arriving a day after the code, noted so the omission is visible rather
than smoothed over. The defense, worth about sixty percent: the closure-injection shape (every
function takes read/write closures, so the logic host-tests against a fake config space) and the
witness tests wanted an API of our own, and the whole crate is ~400 lines covering exactly what
we drive (type-0 headers, memory BARs, virtio vendor caps, the INTx swizzle). The counterweight:
`pci_types` covers most of that decode, and under the rule as written, a maintained no_std crate
should have been the default for peripheral plumbing outside the TCB. Verdict: keep ours (the
swap would trade witness-tested, zero-churn code for a dependency, backwards at this point), and
let the rule bind prospectively, milestone 16's parsing needs being the next real test.

**What the kernel is on this bus:** with `-bios default`, OpenSBI does no PCI setup, so the kernel
is the firmware: it sizes and places BARs itself. The hardcoded window/irq constants are held by
host-run witnesses against the machine's own device tree (the ECAM `reg`, all sixteen
`interrupt-map` entries), the UART's hardcode-with-a-witness pattern.

**Correction, on the record.** Parity C was recorded as blocked ("QEMU's riscv virt has no mmio
disk; it prefers PCIe"). It was not: the runners silently dropped `CRICKER_DISK` when the image
file did not exist, the machine was asked nothing, and the honest-looking "device-id 0" readings
were a diskless boot. Both runners now fail loudly on a missing disk file, parity C completed over
mmio in an evening, and the PCIe transport kept its own justification (the door to NVMe and real
NICs, the transport real hardware uses) rather than a manufactured one. The false record and its
correction are kept in notes/riscv-parity-scope.md because the mechanism, a silent no-op
manufacturing a plausible machine fact, is the instructive part.

## 19. Architectural parity is a tenet; the targets are aarch64, riscv64, and x86_64

**Decided 2026-07-27** (Chris), promoting what practice had already become. The RISC-V work
began as a portability proof and ended at full parity (notes/riscv-parity-scope.md: SMP, the
suite, the shell, the benchmarks, the disk, the DMA confinement, and now the §18 transport and
the coming §16b IOMMU work, all on both ISAs). The tenet makes that the standing rule rather
than a happy outcome:

**Parity is a gate, not an aspiration.** A kernel capability ships on every supported
architecture, proven by the same test suite, or the gap is recorded in a scope note with what
is missing, what it proves, and the plan, the way riscv-parity-scope.md did it. An
architecture is a new `arch/` directory (rule #1), never a fork of the feature matrix. Where a
capability is genuinely asymmetric (a board has no device to prove it on), the record says so
loudly; the false parity-C blocker showed what a quiet gap costs.

**The target set is explicit: aarch64, riscv64, x86_64.** Status, honestly: aarch64 is where
the kernel grew up; riscv64 is at parity; **x86_64 is a declared target that does not exist
yet** (milestone 20 always named it as the reach past RISC-V; this section makes it a
commitment rather than a mention). What x86_64 will stress, known now: a different boot world
(UEFI/ACPI, not device tree; no OpenSBI/PSCI analog), the APIC instead of GIC/PLIC, a third
page-table format behind the `paging` seam (which two IOMMUs are about to prove out anyway),
and TSO memory ordering, where rule #4's weak-first discipline finally pays out in the other
direction: code proven on weak machines is correct on TSO, and nothing about x86 development
could have said the reverse. The PCIe transport (§18) is already x86's native bus, and the
ECAM bridge on both `virt` boards is the same `pci-host-ecam-generic` shape x86 machines
present through ACPI.

## 20. IOMMU-backed DMA isolation: one seam, two arch drivers (milestone 16b)

**Built 2026-07-28**, on both ISAs in QEMU emulation. Milestone 9's shadow ring (notes/dma.md)
confined DMA in software: the kernel validates every descriptor and the device reads a copy the
driver cannot touch. An IOMMU does it in hardware, generically, with no transport knowledge: it
sits between a device and memory and translates every address the device emits through page tables
the kernel programs. §16b makes that real on both boards, with the shadow ring demoted to defence in
depth.

**The seam is the payoff.** Each architecture's IOMMU translates with its own CPU's page-table
format: the SMMUv3 walks VMSAv8-64, the ratified RISC-V IOMMU (v1.0.1) walks Sv39. Those are the two
formats the `paging` crate already builds for process address spaces (§17), so a device's DMA domain
is not a new kind of table. `paging::domain::build_identity_domain` fills a `Mapper` with an
identity map (IOVA == PA) over exactly the frames a device may reach, and nothing else;
`crate::iommu::confine` calls it through `DmaFormat`, an arch alias, so one call site builds a
VMSAv8-64 domain on aarch64 and an Sv39 domain on riscv. This is the page-table format seam (§17)
paying off a second time: proven once, it now backs both process isolation and device isolation.

**Two arch drivers, structural twins**, under `arch/` per rule #1
(kernel/src/arch/{aarch64,riscv64}/iommu.rs). Each owns its register file and the in-memory
structures the hardware is driven by: a per-device table (the SMMU's stream table, the RISC-V
IOMMU's device directory) keyed by the PCIe requester id, a per-device context (the SMMU's STE plus
context descriptor, the RISC-V device context, each the IOMMU's copy of the CPU's TTBR/`satp`), a
command queue for invalidations, and a fault/event queue where a blocked transaction is recorded.
`init` installs an all-invalid table and enables translation, so every device is denied by default
until `confine` writes its entry; `attach` points a device at a domain and invalidates the caches;
`take_fault` drains the fault queue.

**The requester id is the key.** A PCIe function stamps `bus:8 | dev:5 | fn:3` on every transaction
(`Bdf::requester_id`), and both boards publish an identity `iommu-map` in the device tree, so that
id is exactly what the IOMMU looks a device up by. It is threaded from `pci::find_block_device`
through `virtio::register` (a new `Option<u32>` argument: `Some` for a PCI device, `None` for
virtio-mmio, which no IOMMU fronts on either board). `confine` runs at register time, before the
device is entered in the transport table and before it is ever rung, so the domain is installed the
moment the device could DMA. New lock rank `IOMMU` (54), a leaf below `VIRTIO`: the domain's
page-table frames are allocated before the lock is taken, so it is never held across an allocation.

**Discovery differs by arch; the rest is portable.** The SMMUv3 is a device-tree platform node
(`smmu_region`, mapped by `mmu::init`); the RISC-V IOMMU is itself a PCI function (`riscv-iommu-pci`,
1b36:0014), so `pci::init_iommu` enumerates it and places its BAR from a now-shared cursor before
handing the base to the driver. `init` is therefore called per-arch in boot; `active` / `confine` /
`take_fault` are the portable surface.

**Loud on bypass.** Every virtio-pci device needs `iommu_platform=on`, which puts it behind the
IOMMU and makes it offer VIRTIO_F_ACCESS_PLATFORM (bit 33); the driver negotiates that bit only when
offered, so the same binary drives the bare mmio disk and the IOMMU-fronted PCIe disk. A device
without the flag silently bypasses translation, the same manufactured-fact hazard the runners
already fail loudly on. The guard is the confinement test,
`the_iommu_faults_a_dma_that_escapes_the_domain`: it points a confined device's available ring at a
frame the domain does not map, kicks it, and asserts the IOMMU recorded a fault at that frame. If
translation were absent (a missing `iommu=smmuv3` / `riscv-iommu-pci`, or a dropped
`iommu_platform=on`), the escaping read would succeed and no fault would appear, so the test fails
rather than passing on a fiction. It runs on both ISAs.

**QEMU vs ours.** The RISC-V IOMMU emulation is newer than the SMMUv3's, so the record says which is
which: both behaved exactly as their specs describe, and no bug (QEMU's or ours) surfaced during the
build. The existing disk and both attacker suites pass behind the IOMMU on both ISAs (aarch64 118
kernel tests, riscv 60), and the shadow ring stays as defence in depth.

**Honest limits.** QEMU tier only; silicon carries the riscv driver over when a board ships the
ratified spec (the emulate-then-carry pattern the kernel was built on). The domain is an identity map
over frame-granular regions, so it cannot confine below a page. Fault reporting is drained by the
confinement test; routing faults to a handler in a production boot is future work. The IOMMU buys
generality (no transport knowledge in the kernel), not the absence of a trusted DMA policy: the
kernel still programs the domain. See notes/iommu.md.

## 21. Multi-queue DMA confinement: the validator's second direction (milestone 30)

**Decided and built 2026-07-28.** A virtio-net device needs two virtqueues (receive on queue 0,
transmit on queue 1), and receive is the direction where the *device writes into* the driver's
memory rather than reading from it. The §18 seam and the shadow-ring validator were queue-0-only,
so the net driver's prerequisite is a proved second queue and a proved second direction, built
under the same confinement discipline as the disk rather than bolted on when a NIC needs it.

**What changed, and what deliberately did not.** The validator (`validate_and_shadow`) did not
change at all: it bounds the address of every descriptor, `addr..addr+len` inside the driver's DMA
region, whichever way the device moves the bytes. That is the same property milestone 32's write
path relied on (§ notes/dma.md, "The write direction"), now asserted for the direction where the
*device* is the writer: a receive descriptor aimed at kernel memory would let the device overwrite
the kernel with an inbound packet, and it is refused before the device is rung, for the same reason
and by the same check as a read descriptor aimed there. The new work is per-queue **state and
plumbing**: each device carries a per-queue last-validated index and a per-queue ring block, and
`setup_queue`/`notify` take a queue number.

**The queue-layout contract.** Queue `q`'s descriptor table, available ring, and used ring sit at
`q * RING_BLOCK` (0x200) in both the driver's DMA region and the kernel-private shadow frame. One
shadow frame still holds every queue (MAX_QUEUES = 2, so 0x400 of a 4 KiB frame), asserted at
compile time. Queue 0's layout is byte-identical to the old single-queue layout, so the disk driver
needs no change: its data buffers already begin at 0x200 (= queue 1's block), free because a disk
has no queue 1.

**The surface stays narrow (§4 rule 3, the syscall-boundary discipline).** No new syscall and no
new object: the `Virtio` capability's existing `SETUP_QUEUE` and `NOTIFY` methods each grew a queue
argument (`SETUP_QUEUE(num, queue)`, `NOTIFY(queue)`), which is the established way this project
adds capability semantics (object revocation grew `Untyped` the same way). The disk passes queue 0
for both, so its ABI is unchanged. An out-of-range queue is `BadQueue`.

**Proof.** Two new unit tests beside the existing confinement suite, on both ISAs:
`the_validator_refuses_an_rx_descriptor_that_escapes_the_region` (an in-region device-writable
receive buffer validates; the same buffer aimed at kernel memory is refused) and
`a_second_queue_validates_on_its_own_block` (a good chain on queue 1 lands in queue 1's shadow
block while a sentinel in queue 0's block is untouched, and a queue-1 escape is refused the same as
queue 0). See notes/net.md and notes/dma.md.

## Reading

- **The seL4 manual**, and Klein et al., *seL4: Formal Verification of an OS Kernel* (SOSP'09)
- **Liedtke**, *On µ-Kernel Construction* (SOSP'95) — why Mach was slow and why that was not a law
- **xv6 book** (MIT, ~100pp) for how a real Unix-shaped kernel is structured. Read it as the
  road not taken (§10), not as a template.
- `rust-raspberrypi-OS-tutorials` for the aarch64-specific mechanics
- OSDev wiki as a reference, not a tutorial
- *Operating Systems: Three Easy Pieces* for the theory
