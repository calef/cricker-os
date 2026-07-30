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

- **SMP thread placement** (§11's deferred step 3c). **SUPERSEDED by §28 (built 2026-07-28/29).** The
  standing gap this described (every spawn and wake on the current core, so a workload fanning out
  from one core stayed there; the milestone 32 FS mount starved beside three idle cores) is closed.
  §28 shipped the power-of-two-choices spawn placement and message-shaped work stealing this entry
  weighed, and its implementation amendment chose the third option here, **wake-time balancing**, for
  device interrupts specifically (least-loaded, ties to the current core) while keeping IPC rendezvous
  wakes local. See §28 and notes/scheduler.md. Kept for the record of the reasoning that led there.

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

- **`Tcb::SUSPEND`/`RESUME`: pause a thread without killing it** (deferred from the `^C` decision,
  §24, 2026-07-28). The two-tier interrupt design covers notify and kill; suspend is the third
  verb that would make "interrupt" mean pause-and-inspect. Deferred because it widens the §4
  syscall surface with no consumer yet, and because it should be designed next to milestone 22's
  fault endpoints (both are "the kernel turns a thread's state into a message a supervisor
  holds"). **Triggers to build:** (1) a userspace pager (demand paging is fault-message,
  fix, resume: the fault endpoint of §26 is its front half); (2) real job control (`fg`/`bg`, a
  stopped-process state) in the shell; (3) a debugger. (Milestone 22's supervision tree chose
  dead-until-reaped over suspend-on-fault, §26, so it is no longer a trigger.) Whichever fires first, design SUSPEND and the fault endpoint as one
  surface, and give the method its own DECISIONS entry.

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

### Amendment (milestone 31): untyped becomes delegable by rights *inheritance*, with a delegable root

`SPLIT`'s child untyped was minted with `WRITE` alone (`cap::untyped_cap`), where every other
creation path gives its creator full rights: `RETYPE` mints a frame `READ|WRITE|GRANT`, `RETYPE_OBJ`
mints an endpoint, aspace, or TCB the same. Because `SEND_CAP` and `CAP_INSERT` both gate on `GRANT`,
that under-grant silently made untyped the one object type **no process could delegate**: a split
budget could be spent by its holder and handed to no one. That foreclosed "untyped budgets as
first-class grants," milestone 31's headline: a shell that endows a child N pages from its own budget
must delegate an untyped, and could not.

**The first fix was wrong, and the way it was wrong is the interesting part.** Minting the `SPLIT`
child `READ|WRITE|GRANT` unconditionally is a *rights escalation*. `SPLIT` gates only on `WRITE`, so a
process holding a deliberately `GRANT`-less untyped (one delegated to it spend-only) could `SPLIT` it
and receive a `GRANT`-bearing child over the same memory, manufacturing the very right its capability
withheld. That violates the model's derive-never-widens invariant, and it does so at a *fresh mint*
site the Kani proofs (which cover `derive`) do not reach.

**The right fix is rights inheritance, not a rights default.** Two coordinated changes:

1. A `SPLIT` child inherits **the invoking capability's rights, never more**
   (`untyped_cap_rights(child, cap.rights)` at the mint site). A spend-only untyped splits into
   spend-only children; `GRANT` is passed down only if the parent held it. This makes `SPLIT` honor
   derive-never-widens by hand, the same discipline `derive` enforces.
2. The **root** untyped the kernel hands init at boot becomes `READ|WRITE|GRANT`
   (`cap::untyped_root_cap`, at the three init-boot grant sites). Delegating budgets to the children
   it builds is init's job, so the root of the budget tree carries `GRANT`. This was the actual bug:
   the `WRITE`-only root, not the `SPLIT` default, is what left no delegable untyped anywhere and
   forced the escalating workaround.

Rights then narrow monotonically from the root down: root (`GRANT`) -> init's `SPLIT` (inherits
`GRANT`) -> `CAP_INSERT` into the shell (narrowed to `WRITE|GRANT`) -> shell's `SPLIT` (inherits) ->
`CAP_INSERT` into the spawned child (narrowed to `WRITE`, spend-only). `untyped_cap` (`WRITE` only)
stays the constructor for a spend-only leaf budget; nothing manufactures authority at any step.

A kernel test pins the invariant at the mint site (`syscall.rs`,
`split_inherits_the_parent_capabilitys_rights_never_widening`): a `GRANT`-less untyped splits into a
`GRANT`-less child that cannot be delegated, while the delegable root splits into delegable children.
This is a bug fix to this section's intent (untyped is delegable in seL4, the model we borrow
guarantees from), recorded here rather than as a new section. See `kernel/src/syscall.rs`'s `SPLIT`
handler, `cap::untyped_root_cap`, and notes/grant-expression.md.

### Amendment (milestone 22): DESTROY force-kills a live resident thread, it no longer only refuses

`DESTROY` refused (NotPermitted) while a live thread occupied the region, on the reasoning that "its
owner must let it finish first." That is right for a cooperative child, and wrong for the exact case
§24 built the forcible tier of `^C` for: **a runaway that never finishes.** A thread spinning at EL0,
never yielding and never checking its interrupt endpoint, would refuse `DESTROY` forever, so the
shell's escalation had nothing to escalate *to*. §24 named the forcible tier "§16's revocation" and
said "no new kernel primitive"; this is the small change to `DESTROY` that makes that true.

**The refusal now arms a kill.** When `DESTROY` finds a live (`Ready`/`Running`/`Blocked`) resident
thread, it marks it `killed` and still refuses this pass. A killed thread never runs again: the
scheduler converts it to a `Finished` corpse at its **next preemption** instead of requeueing it, and
the ordinary reaper tears down its stack and address space exactly as a clean exit would. So the
owner that retries `DESTROY` (the shell's escalation loop already retries, for the exit sliver)
reclaims the region once the runaway has been torn down.

**Why a flag and a retry, not a synchronous kill.** Yanking a thread out of a run queue needs an
arbitrary-remove the intrusive `Fifo` deliberately does not have, and stopping a thread `Running` on
another core needs a cross-core IPI and a rendezvous. The killed flag needs neither: a runaway is
preemptible by construction (DECISIONS §5), so **each core converts its own killed thread on the
timer**, and the whole mechanism is one branch in `schedule()` plus one flag in `DESTROY`. The cost
is that reclamation is not instantaneous (the runaway runs to the end of its timeslice, then dies),
which is exactly the semantics the shell wants: a bounded escalation, not a stop-the-world.

**Scope, honestly.** This tears down the runaway (`Running`/`Ready`), which is §24's stated target. A
thread that only ever *blocks* is never scheduled to hit that preemption, so the flag alone will not
reap it; that case is the cooperative tier's job (send the program its interrupt endpoint, which by
definition it is listening on), not the forcible tier's. A single kernel test builds a one-instruction
EL0 runaway and reclaims its region out from under it, on both ISAs (`user.rs`,
`destroy_force_kills_a_runaway_and_reclaims_its_region`). See `kernel/src/sched.rs` (`schedule`,
`reap_region_objects`) and `Thread::killed`.

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

## 21. The terminal is a userspace component, and the kernel is out of the shell business (milestone 28)

**Decided and built 2026-07-28.** Milestone 28 put the tty line discipline in userspace as a
swappable component (`termd`), sitting on plain endpoints between the input/console drivers and
applications. Three things here are decisions, and the reason each gets recorded rather than left
in code:

- **The terminal protocol is a userspace protocol, not kernel ABI.** The opcodes
  (`OP_WRITE`/`OP_READLINE`/`OP_BYTES`), the read flags, and the shared-page convention live in
  `linedisc::proto` and are written up in [notes/terminal-contract.md](notes/terminal-contract.md).
  Every request is an endpoint `CALL` served through `RECV_CAP` and answered through the one-shot
  Reply capability (§12); the kernel routes the words without reading them. **No new syscall and no
  new kernel method were added.** This is the §4 boundary held on purpose: a whole tty layer landed
  as userspace composition, not as syscall surface.

- **The kernel is retired as the interactive system's builder.** The aarch64 kernel-wired
  `shell_service` (the pre-19d.2c path) cannot host a shell that speaks the terminal contract, so
  every aarch64 interactive build (the milestone tour, `--features shell`, `--features initboot`)
  now hands off to userspace init through `boot_via_init`, the way RISC-V's `--features shell`
  already hands off to the portable `sysinit`. `shell_service` stays as dead code for reference.
  This completes the §15 / 19d.2c direction ("userspace init is the boot path") for the
  interactive system on both architectures; the reasoning and the deadlock-freedom argument are in
  [notes/line-discipline.md](notes/line-discipline.md).

- **`^C` (interrupting the foreground process) is deferred as a design fork.** The terminal
  detects the interrupt and the contract carries `FLAG_INTERRUPTED`, but *routing* the interrupt to
  a running foreground process is a capability-routing question whose answer will not be Unix
  signals, and it is not built. The problem, candidate mechanisms, prior art (seL4, Fuchsia, Plan
  9), and a recommendation are in [design/interrupt-routing.md](design/interrupt-routing.md), for
  the architect to settle before code.

The engine was **built, not ported**, against the §14 default for userspace, because `noline`
blocks on a cursor-position report a piped line never answers and is a per-read readline rather
than an always-on discipline, and `embedded-cli` is the application's altitude. The full accounting
is in [notes/line-discipline.md](notes/line-discipline.md).
## 22. Rust `std` on the native ABI, the Hermit way (milestone 27)

Decided and built 2026-07-28. Full write-up in notes/std.md; this records the decision and the
forks inside it.

**The decision: implement std's platform layer (`sys`) directly on the capability ABI, not a POSIX
shim under the Unix one.** This is Hermit's shape (std on a non-POSIX unikernel ABI), which
DECISIONS §15 already priced the alternative (Redox's relibc-first road) at "later, if ever, and at
no cost to defer". A std program draws its heap from an untyped budget at slot 0, SENDs stdout to an
endpoint at slot 1, reads `Instant`/`SystemTime` from the virtual counter, and gets honest
`Unsupported` from `thread::spawn`, `fs`, and `net` until the servers that back them exist
(milestones 30 and 32). No new syscall and no new capability method: the PAL is a client of the ABI
as it already stands, the same surface `allocdemo` proved. `panic!` prints and faults (panic=abort;
unwinding is never linked), which is this ABI's honest `abort()`.

**Why now:** the first wall an application hits on cricker-os is "no std", and milestone 23's
vendor-component ambition needs components writable by people who are not kernel people. std on the
native ABI widens "runs real workloads" to most of crates.io that stays off files and sockets,
without smuggling in the POSIX assumptions (no fork, no open-by-path, no ambient anything) the ABI
deliberately excludes.

**The one genuinely new thing is build machinery, and its forks were settled by measurement, not
taste.** `-Zbuild-std` reads std's source from the sysroot of the rustc it invokes, so a patched std
means a toolchain whose sysroot is patched. Three approaches were on the table; the empirical result
chose:

- *Symlink farm* (link a fake toolchain, symlink lib, real patched src): **rejected, measured to not
  work.** rustc derives its sysroot from the resolved location of `librustc_driver`, and a symlinked
  dylib resolves back to the real toolchain, so build-std read the unpatched src.
- *In-place patch of the shared rustup toolchain*: **rejected.** It mutates a shared, rustup-managed
  directory (a surprise `rustup update` would clobber, and it clobbers what other projects build
  against), which the "never clobber" discipline warns against.
- *Hardlink-clone the toolchain* (`cp -al` bin+lib, real copy of just `src`, patch that): **chosen.**
  The clone's `librustc_driver` lives inside the clone, so rustc resolves the clone as its sysroot;
  blocks are shared so the disk cost is near zero; and the real toolchain is never touched.
  `cargo xtask std-src` builds and links it as `cricker-dev`.

**Target specs, not real targets** (roadmap's "a spec first, a real target later if ever"): custom
JSON with `os = "cricker"`, `panic-strategy = "abort"`, softfloat, and `singlethread = true`. That
last one is honest for phase one, one thread of execution per process, so std uses its `no_threads`
sync and single-`static` TLS; it flips off when `thread::spawn` becomes real. The ABI numbers and
the heap algorithm are generated verbatim into the patched std from `crates/abi` and `crates/uheap`,
so they have exactly one definition and cannot drift.

**Accepted costs, recorded:** `SystemTime` is monotonic-since-boot rather than wall-clock (no RTC);
`std::random` is a non-cryptographic splitmix64 (no entropy source); stdout and stderr interleave on
one endpoint; and the `std-src` patches are string-anchored to the pinned nightly's std internals, a
coupling that fails loudly on a rustc bump (the intended tripwire) rather than silently. Proven by a
real std program (`Vec`, `String`, `HashMap`, `println!`, `Instant`) spawned as a workload and
checked byte for byte on both ISAs (the §19 parity gate).

**Amendment (phase two, 2026-07-28): `std::net` binds to the socket contract.** `std::net::TcpStream`
and outbound `std::net::UdpSocket` now work, backed by netd over the §25 socket contract; the
`net honestly unsupported` line of phase one is retired. The PAL (`sys/net/connection/cricker.rs`) is
a **pure client** of the frozen contract, no new syscall and no new capability method: it holds a
`Stack` endpoint (slot 2) and a frame untyped (slot 3), mints a shared frame per socket, and drives
netd with `netproto` `CALL`s. The wire constants are generated verbatim from `user/src/netproto.rs`
into the patched std, the same anti-drift discipline as the ABI and heap crates. A std program does
networking only if it holds those two slots; without them `std::net` returns `Unsupported`, which is
"no ambient network" (§10) made visible from inside a process. The same `hellostd` binary proves
both: spawned without the net slots it runs the offline transcript, spawned with them (and a running
netd) it does a real UDP DNS query and a TCP echo round trip, each asserted byte for byte on both
ISAs. Honest gaps carried as `Unsupported`: `TcpListener` (no LISTEN verb), non-blocking mode and
timeouts (blocking-only contract), DNS resolution (`lookup_host`; numeric addresses only), and IPv6.
One finding reported up: netd ties a socket's local port to its socket id, so reopening a closed id
reuses its port and can stall against slirp; the fix is ephemeral local ports in netd, a contract-side
change (notes/std.md).

**Amendment (phase two, 2026-07-29): `std::fs` binds to the FS-service contract, and a path means
"under the directory I hold".** `std::fs::File` now works, backed by the §27 FS service; the
`fs honestly unsupported` line of phase one is retired for a program that was granted a directory.
The PAL (`sys/fs/cricker.rs`) is again a pure client of a frozen contract, no new syscall and no new
capability method, with `crates/fs_proto` generated verbatim into the patched std.

**The design question, and the answer.** `File::open` takes a path and this system has no global
namespace, so the binding had to decide what a path *means*. Per §27, open-by-path exists only inside
the server, resolved against the one directory node the client's endpoint is bound to. So the mapping
is: **a std program holds a directory capability at slot 4, and `File::open("foo")` means "foo, under
the directory I was granted."** Everything else follows, and is enforced client-side before a byte
reaches the wire so a would-be escape becomes a legible error rather than an `ENOENT`:

- An absolute path, any `..`, and any nested path are **refused as `ErrorKind::InvalidFilename`**,
  with a message naming which case it was. Deliberately **not** `PermissionDenied`: nothing consulted
  a permission, and there is no name here for what was asked, because no capability designates it.
  Mapping a capability refusal onto EPERM would smuggle in exactly the POSIX fiction this milestone
  exists to avoid. A name that is expressible but absent stays an ordinary `NotFound`, which is what
  makes the difference legible.
- A program with **no directory capability gets `Unsupported` from all of `std::fs`**, the same shape
  the net half uses without a `Stack` capability. Detecting that cannot touch the shared page (an
  ungranted program has none mapped), so the probe is a payload-free `FSTAT` on an impossible handle:
  the kernel refuses the invoke, or the server answers `-EBADF`, and only the latter means a
  filesystem is reachable.
- **The slot convention now has a gap, and the gap is load-bearing.** A program granted a directory
  but no network holds slots 0, 1, and 4, because empty 2 and 3 are how `std::net` knows it has no
  network. `Spawn.grants` fills from zero and cannot express that, so the kernel gained
  `sched::grant_at`, the same explicit-slot move `Tcb::CAP_INSERT` already offers a userspace loader
  (§26's fault slot uses it). notes/abi.md §4 records the convention.

Bound: open, read, write, seek, `metadata`/`len`, close on `Drop`, plus `metadata`/`read`/`exists` by
name. **Honestly `Unsupported`, because the contract has no verb for them:** creating a file and
truncating one (so `std::fs::write` and `File::create` are Unsupported by construction, and writing
means opening a file the image already carries), directory iteration, `mkdir`/`unlink`/`rename`,
symlinks and hard links, `canonicalize`, permissions, file times, locks, and `duplicate`. Proven on
both ISAs (§19) by the same `hellostd` binary, now with three behaviours chosen by its grants alone:
its stdout is compared byte for byte with the file's own bytes spliced in from the shared fixture, so
one assertion covers disk, block server, FS server, contract, PAL, and endpoint.

**Two things reported up rather than built** (see §27's amendment and notes/std.md): adding `CREATE`
and `TRUNCATE` verbs to the contract, which is what `std::fs::write` needs; and the overlap between
the wire's negated-errno space and the kernel's invoke-error space (-1..-8), where `-2` is both
`ENOENT` and `WrongObject`.

## 23. Multi-queue DMA confinement: the validator's second direction (milestone 30)

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

## 24. Interrupting the foreground process: two-tier, shell-held, no new kernel surface

**Decided 2026-07-28 (Chris), from the proposal in design/interrupt-routing.md.** `^C` routes in
two tiers. The first `^C` is cooperative: the shell sends an interrupt message on an endpoint the
foreground child was spawned holding, and a program that listens can cancel cleanly. The second
`^C` (or a shell-side timeout) escalates to the forcible tier: the shell tears the child down with
object revocation (§16), which handles the runaway that never checks its endpoint. The interrupt
capability is held by the **shell**, because job control is the shell's knowledge; a process that
was not granted a child's interrupt endpoint cannot interrupt it, and there is no ambient deliverer
of signals. Unix signals (ambient authority, delivered by PID) are exactly what this design
refuses to reintroduce.

**No new kernel primitive.** The cooperative tier is the existing endpoint machinery; the forcible
tier is §16's revocation. The escalation policy (how many `^C`s, what timeout) lives in the shell,
userspace, where policy belongs.

**Deferred, deliberately: `Tcb::SUSPEND`/`RESUME`.** A suspend method would make "interrupt" mean
pause-and-inspect (real job control, an eventual debugger) instead of notify-or-kill. It is
deferred, not rejected: it widens the syscall surface for a consumer that does not exist yet, and
milestone 22's supervision work (fault endpoints) is the adjacent primitive it should be designed
beside. Tracked in Open design ideas above; the trigger to revisit is written there.

### Implementation amendment (built): two primitives forced the shape

Building it (both ISAs) hit two facts about the primitives that refine, without changing, the
two-tier decision. Both were confirmed with the architect before building; recorded here because
the reasoning is the deliverable.

**The cooperative signal is a shared-memory flag, not an endpoint delivery.** The design imagined an
async notification on an endpoint the foreground job watches. But the job the user most wants to
interrupt is *running a computation*, and a running program cannot watch an endpoint: there is no
non-blocking receive, and a blocking one would stall the very work being interrupted. So the shell
mints a per-job shared frame (`capsh::jobframe`), maps it into the child, and writes an interrupt
word the child reads with a plain load *between work units*. This is "control by shared memory"
where the model usually says "control by message", and it is honest about why: the message form
needs a notification primitive that does not exist yet. It is granted like any capability, through
the manifest's `interruptible` endowment, so the authority story is unchanged: a program the shell
did not endow a job frame cannot be signaled, and cannot signal back.

**The forcible tier is a plain `Untyped::DESTROY` on the child's region, and that required the §16
amendment.** The shell builds a supervised child *entirely from an untyped it split from its own
budget and delegated to init*, so the whole child, aspace and TCB and code and stack, lives in a
region the shell holds. Tearing it down is `DESTROY` on that region. The first instinct, faulting
the child by revoking a frame it touches, was rejected: a genuine runaway (a bare `loop {}`) touches
nothing revocable, so frame-revocation cannot reach it. Instead `DESTROY` learned to force-kill a
live resident thread (the §16 amendment): a refused reclaim arms the kill, each core converts its
own killed thread to a corpse at the next preemption, and the owner retries `DESTROY` until it
succeeds. The shell's watch loop retries exactly so. A pure `loop {}` spinner is now torn down on
the second `^C` on both ISAs.

**The shell learns of `^C` by polling, deliberately (wait A).** The shell must watch the job and the
`^C` at once, and with only blocking primitives it cannot block on both. It busy-polls `termd`'s new
`OP_INTRCOUNT` (an immediate reply with the running `^C` count) with `yield` between, driving the
escalation from the count's advance. The escalation policy (first `^C` cooperative, a second `^C` or
a grace-window timeout forcible) is host-tested in `capsh::Escalation`. Holding `^C` routing in the
shell, not `termd`, is the §24 premise: job control is the shell's knowledge, and `termd` stays a
terminal. The clean blocking form waits for the notification primitive milestone 23's latency
ladder forecasts; the shared flag and the poll are the honest interim, not the destination. See
notes/grant-expression.md (the interrupt grant) and notes/terminal-contract.md (the flow).

## 25. Socket identity: a socket id in phase one, minted endpoints as the tracked later step

**Decided 2026-07-28 (Chris), resolving the milestone 30 piece-3 fork (notes/net.md).** A process
holds one `Stack` endpoint capability; opening a connection yields a **socket id**, a small
integer carried in the message words, with the per-connection **shared frame** as the real granted
resource. Chosen because milestone 27's `std::net` PAL wants a file-descriptor-like handle, and
because a minted kernel endpoint per TCP connection spends a bounded kernel object per socket
(the endpoint budget is finite, as the 27+28 merge demonstrated).

**The purer capability story is deferred, not rejected, and Chris's direction is explicit: come
back for it.** Minted-endpoint-per-socket makes a socket an unforgeable, individually delegatable,
individually revocable object. **Triggers to build:** (1) a socket needs to be delegated to a
third process (the id is meaningless outside the holder of the stack cap, which is a feature until
it is a limit); (2) milestone 23's hot-swap work wants per-connection revocation during a net
server swap. The contract keeps the shared frame as the per-connection resource precisely so this
migration changes the handle, not the data plane.

## 26. The fault endpoint: thread death becomes a message a supervisor holds

**Decided 2026-07-28 (Chris), the five sub-decisions settled one at a time; not yet built.** The
kernel is the only witness to a thread's fault, so it is the one that must pass the news along.
When a thread faults or exits, the kernel delivers a message to the supervision endpoint its
spawner designated. This is the one kernel mechanism milestone 22's supervision tree needs;
restart policy stays in userspace, and the kernel never relaunches anything.

The five parts, each decided explicitly:

1. **Build it.** The alternative (userspace heartbeat polling) is a poor death detector: timeouts
   are guesswork where the kernel has the exact instant and cause. Polling remains the right tool
   for a different problem, liveness ("alive but wedged"), and any supervisor can layer it on with
   ordinary IPC and no kernel help.
2. **Supervision is granted at spawn, only.** The fault endpoint is one more capability in the
   spawn endowment, so the supervision relationship is visible in the spawn literal and cannot
   change afterward. Runtime reattach (`Tcb::SET_FAULT_EP`) is deferred until milestone 23's
   hot-swap work demands supervision handoff, and it is a new decision when it does.
3. **Both faults and exits flow**, distinguished by an event code. Restart policy needs to tell
   "crashed" from "finished".
4. **Dead-until-reaped.** After the message, the thread never runs again, but its corpse (TCB,
   address space, memory) persists for postmortem until the supervisor reaps it with §16 object
   revocation. Suspend-for-inspection (resumable faults) is deferred to the SUSPEND tracker in
   Open design ideas, which now carries the userspace pager as a third trigger; the message format
   reserves a word so a fault-reply/resume protocol can arrive additively.
5. **One shared supervision endpoint per supervisor**, kernel-stamped identity per message:
   `(event code, tid, fault pc, fault address, reserved)`. Synchronous rendezvous means `RECV`
   blocks on one endpoint, so per-child endpoints would force a supervisor thread per child or a
   new wait-any primitive; the shared endpoint needs neither. The id word is trustworthy because
   the kernel is the only sender on this path (seL4 solves the general untrusted-sender case with
   badged capabilities; that mechanism returns as its own decision if shared endpoints ever need
   trustworthy identity from userspace senders).

Surface cost: no new syscall and no new method. Spawn already carries grants; delivery is a
kernel-internal send. The additions are a message-format convention and a spawn-slot convention,
recorded in notes/abi.md when built.

### Implementation (milestone 22, phase A), the decisions the build settled

The five sub-decisions above are the design; building it settled the details they left open. These
are amendments to §26, not a new section, per its own "no new section" intent.

1. **The spawn-slot convention is the last cspace slot, consumed at `START`.** The designated
   endowment (§26.2) is a real capability in a reserved slot, `abi::fault::FAULT_EP_SLOT` (=
   `CSPACE_SLOTS - 1 = 15`). A supervisor places its endpoint there with `Tcb::CAP_INSERT`, which
   grew an explicit target-slot argument (`0` keeps first-free, `n` targets slot `n - 1`) so the
   fault endpoint lands in the reserved slot instead of wherever first-free fell. That is the one
   surface change, and it is an *argument* to an existing method, not a new method. At `START` the
   kernel reads the slot: an `Endpoint` there makes the thread supervised, and the kernel records the
   endpoint (`Thread::fault_ep`) **and clears the slot**, so the child cannot forge messages on its
   own supervision endpoint. The *last* slot is deliberate: ordinary children fill low slots from
   zero, so none accidentally lands a working endpoint there and gets read as supervised.

2. **Delivery reuses the synchronous-send rendezvous; the corpse is the parked sender.** The
   non-blocking requirement (do not lose the event if the supervisor is not in `RECV`) is met by the
   *existing* sender-queue mechanism, not a new one. If a supervisor waits, rendezvous; if not, the
   dead thread parks on its supervision endpoint's sender queue with the message in its mailbox, and
   `RECV` collects it later. A death carries data (tid, pc, addr), so the data-less IRQ signal count
   does not fit; the sender queue does, and it is already proven. The corpse is never woken:
   `ipc_recv` leaves a `Dead` sender dead after taking its message, the same way it leaves a `CALL`
   caller blocked. So no new kernel mechanism was needed, which is what §26 predicted.

3. **Dead-until-reaped is a distinct thread state.** `State::Dead` is a corpse the reaper must *not*
   collect (unlike `Finished`); only the supervisor's §16 `DESTROY` frees it. Reusing `Finished`
   would race the reaper against the supervisor, so the distinction is a property of the type. The
   corpse's TCB retains the fault-time registers (its mailbox holds the five words), which is what
   the reserved fifth word needs to exist for.

4. **The IPC mailbox widened from three words to five.** The message is five words and `RECV` must
   deliver all five, so the kernel mailbox and the `RECV` result grew to five registers. Ordinary
   three-word IPC pads the top two with zero, so `user_rt::recv` and every existing program are
   unchanged; only a supervisor reads `w3`/`w4`. This is the message-format convention made real.

Proven on both ISAs (`kernel/src/user.rs`, `supervision_tests`): a child crashes and its supervisor
receives `(FAULT, tid, pc, addr)`, the corpse survives with its state until revocation reaps it, a
respawned child runs, and a clean exit reports `EXIT`. See notes/supervision.md and notes/abi.md §5.

### Milestone 22 phase B.1: measured boot, and the signature variant we did not build

**Built 2026-07-29.** Recorded here rather than as a new numbered section, because this is milestone
22's record and §26 is where milestone 22's decisions already live; the section numbering is
contended and grabbing a number would collide. Concept note: notes/trusted-init.md.

The gap: §14 promises "a verified core that confines unverified workloads," and at runtime the kernel
confines init as well as anything (MMU isolation proved, W^X, capabilities unforgeable, a compromised
init cannot break the kernel or escape). But init's **bytes** were loaded unchecked, and it is the
program that builds every other process. Anything that could substitute bytes at
`/chosen/linux,initrd-start` got to be init. Milestone 16b (§20, the IOMMU) had already closed the DMA
window a device could have used to rewrite the initrd *behind* the check, which is why the check is
now airtight rather than theatre; that ordering was deliberate.

Five decisions, each with its alternative on the record:

1. **Measured, not signed.** The kernel carries a digest of the boot program compiled into its own
   image (`trust::TRUST_ROOT`, generated by `kernel/build.rs`) and refuses to enter a program that
   does not match. The meaning is exactly "this kernel image runs exactly this init," which needs no
   keys, no certificate chain, and no signature-verification code inside the trusted computing base.
   It is the minimal honest thing that closes the gap.

2. **SHA-256, hand-written, one implementation for both sides.** The threat is byte substitution, so
   the hash must be collision- and preimage-resistant; a non-crypto hash (the FNV xtask uses for
   stale-input detection) would let someone craft a colliding init. Among collision-resistant options
   SHA-256 costs the TCB least: ~100 lines of shifts and adds in `crates/measure`, no dependency, no
   allocation, no `unsafe`, and independently checkable with `shasum -a 256` anywhere. BLAKE3 (faster)
   and SHA-3 (a second permutation to audit) both buy speed we do not need for one 1.2 MB hash per
   boot. Hand-written rather than vendored, because a vendored crate is a supply-chain edge inside the
   TCB to save arithmetic whose reference text and test vectors are published. The build and the
   kernel hash through the *same* crate, so the measurement has one definition; the risk that trades
   for (an implementation agreeing only with itself) is answered by testing against the published
   FIPS 180-4 vectors and by the cross-check against the host's `shasum`.

3. **Fail closed in both directions, and an unmeasured program is a refusal.** Wrong bytes halt with a
   diagnostic naming the expected and measured digests. A *missing* measurement halts too: a kernel
   built without the manifest gets an empty trust root, and an empty trust root vouches for nothing.
   That second half is the one that matters, because the natural bug in a measured boot is for the
   check to evaporate silently when the build step does not run.

4. **The build composes one way: userspace -> archive -> manifest -> kernel image.** The kernel image
   holds the hash of a separately built initrd, so the initrd must exist first. No chicken-and-egg:
   the hash never feeds back into the initrd, and every xtask path already built `user()` before the
   kernel (it boots with the archive as `-initrd`), so nothing was resequenced. Cost accepted: a
   userspace change now relinks the kernel, which is what "runs exactly this init" means. A bare
   `cargo build`/`clippy` with no manifest yields an empty trust root rather than a build error, so
   the lint gate still works and the failure lands at boot where it belongs; a *malformed* manifest is
   a hard build error, because measuring nothing silently is worse than stopping.

5. **The kernel measures only the program the kernel loads.** `init` on aarch64, `init` and `sysinit`
   on riscv64. Every other program in the archive is loaded by init in userspace and is not measured
   today, so the chain of trust stops at init's entry. The capability-correct extension is **init
   measuring what init loads** (its own table, in userspace, trustworthy because init's own bytes are
   now measured), which keeps policy out of the kernel the same way supervision does. Recorded as the
   follow-up, not built. Hashing the whole 14 MB archive in the kernel would cover everything with one
   value but puts both the cost and the policy in the wrong place.

**The signature variant, recorded as a follow-up rather than built.** A signature over init against a
public key compiled into the kernel buys one thing a hash cannot: **updating init without rebuilding
the kernel.** Its costs are real and both land in places this project protects. First, signature
verification enters the TCB: Ed25519 means field arithmetic, point decompression, and SHA-512, which
is an order of magnitude more code inside the boundary than SHA-256, and it is code where a subtle bug
is an accepted forgery rather than a crash. Second, key custody becomes a question a hash never asks:
where the private key lives, who can sign, how it rotates, and what revokes a compromised one (a
kernel with a baked-in public key and no revocation list is one leaked key away from accepting
anything forever). The peer project Atom ships Ed25519-signed executables, so this is real and
reachable, just a bigger TCB. It becomes worth paying for when init is delivered independently of the
kernel; today they are built by one command in one tree in one sequence, so the hash is strictly
better. The natural sequence if it is ever wanted: signature verification *in addition to* the
measured root (so the hash stays the floor if key handling fails), and the verification code proved
under §18's toolchain before it is trusted.

Proven on both ISAs (`kernel/src/user.rs`, `measured_boot_tests`): the boot program in RAM measures to
the digest in the running kernel's own `.rodata` (the end-to-end build-composition proof, nothing
hard-coded), and one flipped bit or an unmeasured name is refused. The refusal *path* is not booted in
a test because a real refusal halts the machine; the decision function is tested instead, and the boot
path's only response to `Err` is `arch::halt()`. Recorded plainly. Host tests in `crates/measure`
carry the FIPS vectors. No bench movement: the bench boot enters no boot program.

### Milestone 22 phase B.2: init gives its authority away, and the supervision tree keeps running

**Built 2026-07-29.** Phase A gave the kernel the mechanism (a death becomes a message); B.1 settled
what bytes init is. This settles **what a compromised init can still reach**, which was the second half
of the §14 soft spot. Recorded here with the rest of milestone 22 for the same reason B.1 is.

1. **Init's authority becomes short-lived, not merely careful.** The pre-B.2 init holds a large untyped
   budget for its whole life because it stays the system's process builder, so every process is one bug
   in init away from being built wrong. The new root (`user/src/rootsup.rs`) holds full construction
   authority only long enough to build two servers, then **deletes** it (the wiring capabilities, the
   spawner's budget copy, and the root untyped). After that it cannot make a page, an address space, a
   thread, or an endpoint. The alternative (keep the budget, be careful with it) was rejected on the
   §14 thesis: a confinement you can only honour by being correct is not confinement.

2. **Process construction moves to a sub-server that holds one program image, not the archive.** The
   spawner gets `flaky`'s bytes copied into read-only pages of its own address space, never the 14 MB
   initrd, so "build program X" is unanswerable for any other X. Its budget is `WRITE` without `GRANT`:
   it may spend memory, never lend it. Each instance is built in its own region split off that budget,
   which makes a reap one `Untyped::DESTROY` (§16) and, LIFO, returns the pages to the budget.

3. **The supervisor holds no memory at all.** `subsup` has a request channel, a fault endpoint, and a
   report endpoint. It cannot build, allocate, or reap; it can only *ask*. So the split is: the
   supervisor decides **whether** to reap and rebuild, the spawner is what **can**. Policy and
   authority separated by an IPC boundary, which is the same shape as every other decision here.

4. **Restart policy is userspace code and stays there.** Bounded retries, a clean exit read as
   "finished" rather than "crashed" (which is why §26 delivers both events), a give-up. The kernel's
   whole contribution is one five-word message, unchanged from phase A. No new syscall, no new method.

5. **Proven by authority, not by timing.** Two cross-ISA tests (`authority_tests`): after the handoff
   both construction primitives fail from inside init with `NoSuchSlot` (nothing there) rather than
   `NotPermitted` (something there, restricted); and a faulting sub-server is reaped and restarted by
   its supervisor, with the clean exit of the replacement *not* triggering another restart. "init was
   not involved" is proven by the empty capability slot, not by scheduling order: a process that cannot
   retype a page cannot have built the replacement.

**Two design forks found, reported rather than built through.**

- **Reaping needs the same right as building.** `DESTROY` and `RETYPE` both need `WRITE` on the region,
  so a root supervisor that can restart a dead tier-one server is a root supervisor that can build
  processes, which is the authority the milestone exists to give away. rootsup therefore chooses to be
  unable to build, and its policy for a tier-one death is "report and stop", the fail-closed floor.
  Splitting a **reap-only right** out of `WRITE` (a rights bit, or a distinct `Untyped::REAP` method)
  would let a root recover without regaining construction authority. That changes the rights model and
  the syscall surface, so it is a decision, not an implementation detail.
- **A supervisor cannot turn a tid into a handle.** The fault message names the dead thread by tid
  (§26.5), but nothing maps a tid to something a builder holds, so `subsup` names instances by a handle
  the spawner issues. That is sufficient for one child at a time and insufficient in general. Options:
  a `Tcb::NAME` method (small, and discloses nothing the fault message does not already), per-child
  fault endpoints (which §26.5 rejected for needing a thread per child or a wait-any primitive), or the
  builder reporting the tid it created.

**What is deliberately still open.** The tree proves the pattern with real programs on both ISAs, but
it is **not yet the interactive boot's init**: `sysinit` and `hello`'s init role still hold their
budgets for life, because they remain the shell's spawn service. That migration is the next increment
and was not done blind in the same pass, because that boot path is hand-validated (the harness cannot
inject keystrokes) and moving the spawn service wants an interactive confirmation, not a green unit
test. See notes/trusted-init.md for the shape it takes.

**A pre-existing bug this work found, on both architectures.** The supervision tree enters more
processes per run than anything before it, and that surfaced a race in the **exception-return path**:
staging `SPSR_EL1`/`ELR_EL1` (aarch64) or `sepc`/`sstatus` (riscv) for the return is not atomic with
respect to a nested exception, so an interrupt in a two-instruction window could return a brand-new
process to its entry point **at EL1** (aarch64, observed, about one suite run in four) or to a kernel
address in U-mode (riscv, found by inspection). Only the first-entry path was exposed, because a normal
trap return already has interrupts masked. Fixed by masking at the top of the restore, one instruction,
free at the far end because the return restores the mask from the saved state anyway. Written up in
notes/exceptions.md. The icount baselines moved by well under 1% (one extra instruction per exception
return) and were re-saved in the same commit.

## 27. The filesystem service: a capability-shaped contract over a component we did not write (milestone 32 phase 2)

RedoxFS runs confined as a userspace FS-server component, and its interface is **capability-shaped
from birth**. Three processes, wired by the kernel and named by nobody else: a **block server** (a
role of the virtio driver) that serves blocks over blk IPC with the DMA confinement unchanged; an
**FS server** (`fs-server/`, its own workspace because it links the vendored engine) that runs the
no_std RedoxFS core behind a `Disk` trait over blk IPC and allocates from its own untyped budget
through §22's `GlobalAlloc`; and a **client** that holds only a directory capability. The contract
and both wire protocols live in `crates/fs_proto`, host-tested, the way the terminal contract lives
in `linedisc::proto`. Full design in notes/fs-server.md.

**The contract's rules, which milestone 31 will grant against.** The endpoint a client holds IS the
directory capability: it is bound, in the server, to one directory node, and every name in an `OPEN`
is resolved under that directory. There is no absolute path, no `..`, no global namespace; a client
without the endpoint can open nothing, and the refusal is "no such capability", not a permission
check. A handle is a server-minted token, validated against the session's table in one place, so
forging one is meaningless. Open-by-path exists only inside the server. None of this adds a syscall:
the kernel routes these words the way it routes any IPC (§10, §12) and never reads an opcode, so
adding a method is a change to `fs_proto` and the note, not to the surface (the §16 discipline).

**The error boundary is mapped exactly once.** RedoxFS's error type (`syscall::error::Error`) rides
unmapped through the sans-IO core and the `Disk` impl; the serve loop is the single site that turns
it into the wire's negated errno (`fs_proto::reply_err`). There is no ABI type below the boundary to
leak, which is what makes the rule enforceable rather than aspirational.

**The block server moves a whole block per request, and waits on the interrupt.** RedoxFS scans a
256-entry header ring at mount, so an open is hundreds of reads. The block server moves a whole
4096-byte block per virtio request (its DMA region's second page IS the FS server's block page, so
the device DMAs straight in, no copy), which is what keeps the mount's request count in the low
hundreds and affordable. It then **WAITs on the device's completion interrupt**, the milestone-9
driver discipline, and lets `used.idx` decide when a wakeup is really its own.

*This paragraph is a correction* (fix/irq-delivery, 2026-07-29). It used to say the server polled the
used ring deliberately, because "a reschedule per read overran the watchdog". It does not: with the
WAIT path the fs-server test passes on both ISAs at the 4-core SMP boot, all of the mount's
interrupt-driven completions landing well inside the 60 s watchdog. QEMU still completes virtio-blk
synchronously inside `NOTIFY` (notes/dma.md), so the interrupt is already pending when the server
WAITs, and the pending-signal count (§9a) returns that WAIT at once instead of blocking on an event
already over. The machine overruled the note.

The runners also order the two mmio disks with care, because QEMU assigns virtio-mmio slots in
reverse command-line order and the kernel enumerates by ascending slot.

**Creation stays host-side, always.** The std-gated core APIs are exactly creation (uuid, getrandom);
the server only ever opens an image, so entropy never becomes a userspace dependency. Test images are
made by `tools/redoxfs-host` with the same pinned engine (roadmap §32 item 4).

**Proven.** The read path is proven end to end on both ISAs (the §19 gate): a host-made image,
mounted by the confined FS server over blk IPC, its `motd` opened through a granted directory
capability and read back byte for byte, plus a host-tool consistency check after the run. The sans-IO
core is host-tested for read AND write (`fs-server` lib), so the filesystem logic is proven both ways
independently of any device.

**Amendment (2026-07-29, then narrowed the same day): a FIRST on-device write works; a repeat write
to the same block still loops.** Read the correction below together with this qualification, which a
second agent established by reproducing the loop on main: the gate only ever performs first writes,
because `mkredoxfs` rewrites the target block to a placeholder before every run, so the loop hides
behind the harness rather than being absent. The original blocker was not stale, it was narrower than
recorded, and the optimistic correction below overstated it. See notes/fs-server.md. This
section used to carry an open item, that an end-to-end write "loops inside RedoxFS's allocator commit
on bare metal even on a pristine image" (the `prev`-chain walk in `Transaction::sync_allocator`). It
does not. Driven through `std::fs` (§22's phase-two amendment), the write completes on both ISAs and
reads back byte for byte when the **host tool reopens the image afterwards** with the pinned engine,
which is the half a cache cannot fake; that reopen is now part of the gate rather than a comment. The
likely cause of the old symptom is the interrupt-delivery fix of the same day (the block server WAITs
on the completion IRQ instead of polling the used ring, the same correction the read path needed);
stated as likely, not proven, because what was measured is that the write completes, not why the poll
path did not. The milestone-32 client stays read-only by choice now rather than by blocker.

**The remaining gap is in the contract: there is no `CREATE` and no `TRUNCATE` verb**, so
`std::fs::write` and `File::create` are honestly `Unsupported` and a write means opening a file the
image already carries. Both verbs are addable (`Transaction::create_node` is not std-gated; "creation
stays host-side" above is about creating a *filesystem*, which needs uuid and getrandom, not a file),
and adding them is a change to `fs_proto`, the FS server, and this section, so it is a decision to
take deliberately rather than a hole to plug. Reported up, with the reply-space overlap noted in
notes/std.md (the wire's negated errnos collide with the kernel's invoke errors, -1..-8). See
notes/fs-server.md.

## 28. SMP placement: two random choices at spawn, message-shaped stealing, local wakes

**Decided 2026-07-28 (Chris), after §11's deferred "step 3c" was demonstrated by the machine** (a
starved core 0 beside three idle cores, the FS-server watchdog incident). Three parts, each chosen
against the alternatives on the record:

1. **Spawn placement: the power of two choices.** At thread creation, sample two random cores'
   runnable counters (relaxed atomics; stale reads are fine, the gossip lesson) and place on the
   lighter. Near-optimal balancing with O(1) state touched (Mitzenmacher; Sparrow's proof at
   datacenter scale), and the placement path never reads more than two remote cache lines no
   matter how many cores real silicon brings. Chosen over a full least-loaded scan (contends on
   every counter, ages badly with core count) and over Windows-style round-robin (blind to load).
2. **Wake stays local, deliberately.** A rendezvous partner wakes on the current core: message in
   registers, cache warm, direct-handoff locality (seL4's precedent, Linux wake_affine's lesson).
   The hot path affords no policy; the imbalance it can cause is the next part's job.
3. **Correction: idle cores steal by message.** An idle core sends a steal request over the §11
   inbox/SGI machinery to a loaded core, which hands one runnable thread back at its next
   scheduler entry. Pull beats push under uncertainty (every distributed work queue), no shared
   run-queue locks appear (the per-core queues stay single-owner), and it leans toward milestone
   17's message-passing direction rather than away. Cost accepted: a steal lands at the victim's
   next scheduler pass, bounded by the tick.

**Deferred, with triggers:** an explicit placement grant in the spawn manifest (milestone 23's
contract; overrides the default, recovering seL4's userspace-owns-placement story for pinned
components); priorities and CPU budgets (no mechanism today, round-robin is the whole story; the
trigger is a real workload where fairness visibly fails, and the design starts from budgets as
narrowing grants, not from nice); §12's dormant priority-donation item wakes only with priorities.

**Changeability, stated at ratification:** this is scheduler-internal policy. No ABI, no
capability semantics, no baseline movement (the icount benches are hart-pinned). The one-time cost
of enabling any migration at all: latent same-core assumptions (per-CPU state, weak-memory
orderings) lose their accidental cover, so the implementation lands with cross-core stress tests,
rule 4's discipline applied on purpose. Supersedes the Open design ideas placement entry when the
in-flight FS integration lands it. Implementation slots after milestone 22 phase B, before
milestone 23's swap-under-load demo.

### Implementation amendment (2026-07-29, as built)

The three parts shipped as ratified, with one addition the machine forced and two corrections worth
recording. Code in `kernel/src/sched.rs` and `kernel/src/cpu.rs`; scheduler note in
notes/scheduler.md; cross-core stress tests in `sched.rs`, `smp.rs`, and `user.rs`.

- **Spawn placement, as built.** `spawn` calls `pick_spawn_target`: two samples of a per-core
  xorshift PRNG index the online cores, and the lighter by `runnable()` (a relaxed mirror of run
  queue + inbox depth, kept current in `cpu::with_runq` / `note_inbox_len`) wins. The PRNG is seeded
  per core from a fixed constant so a given boot makes the same choices, which keeps the icount
  benches reproducible. On one online core it is a no-op.

- **Stealing, as built.** An idle core's `try_initiate_steal` picks the most-loaded other core by run
  queue depth alone (never its inbox, which is work already in flight to it), CASes a one-slot steal
  request, and pokes it with the reschedule SGI; the victim's `serve_steal_request` hands back one
  queued thread through the requester's inbox at its next scheduler entry. Pull-based, no cross-core
  run-queue lock.

- **The wake SPLIT (the addition).** §28.2 said "wake stays local." That is right for an **IPC
  rendezvous**: the partner wakes on the waker's core, message in registers, cache warm, and the
  serial netd<->std pipeline stays co-located. It is wrong for a **device interrupt**, which carries
  no such locality: pinning the woken driver to the IRQ-handling core re-concentrates the pipeline
  (std_net) or lands it on a busy core. So `irq_notify` wakes LOAD-AWARE via `wake_load_aware` /
  `pick_wake_target`: the least-loaded core, ties won by the current core so a driver taking a
  completion interrupt every request (the block server at mount) is not migrated each time. Rendezvous
  wakes (`ipc_*`, supervision, revocation) stay local, unchanged. This is the split the IRQ-delivery
  work recommended; the device-line affinity that spreads which core takes each IRQ is its companion,
  documented in notes/interrupts.md.

- **Correction: migration needs the per-hart pointer to be right (RISC-V).** §28's scattering is the
  first workload to preempt kernel threads on secondary harts and then move them. That exposed a
  latent RISC-V bug: the trap frame saved and unconditionally restored `tp`, the kernel per-CPU
  pointer, so a thread preempted on one hart and resumed on another came back reading the wrong
  hart's per-CPU state. Fixed in `arch/riscv64/trap.s` (restore `tp` only for a U-mode return; a
  kernel return keeps the live, correct one). Full write-up in notes/riscv-port.md. aarch64 was
  immune (its pointer is `TPIDR_EL1`, a system register the frame never carries).

- **Correction: the hang watchdog now credits real progress, not test starts.** With migration and a
  slow-but-live workload, the old "did a new test begin in the last 60 s" heartbeat could not tell a
  deadlock from a slow test, and it tripped std_net, which legitimately runs about 300 s in netd's
  userspace smoltcp poll (CPU-bound, no wakes and no output for stretches over a minute). The
  watchdog now counts progress as a completed wake or a line of output OR any core running a
  non-idle thread; only a genuine lost wakeup (every thread blocked, every core on its idle thread)
  stalls it. See `kernel/src/testing.rs`.

- **Correction to that correction: a progress-only heartbeat traded a flake for a silent hang, so the
  test harness also enforces a per-test wall-clock ceiling** (2026-07-29). The caveat recorded above,
  that the progress heartbeat cannot see a busy-spin livelock, was accepted on the argument that the
  leaked-spinner regression test and `scripts/qemu-bounded.sh` covered it. **That reasoning was
  incomplete, and the machine showed it:** the RedoxFS repeat-write livelock spins in an allocator
  commit *while still serving blk IPC*, so every rendezvous reset the heartbeat, and a failure that
  had been a loud 60 s watchdog trip became an infinite silent hang at about 400% CPU with no
  watchdog fire at all. A livelock that makes IPC progress is indistinguishable from healthy work to a
  progress-only instrument, and turning a loud failure into a silent one is strictly worse than the
  flake the heartbeat fixed.

  So the harness now asks two questions and either can fail the run. **The heartbeat** ("is anything
  happening at all?", ~60 s) is unchanged and still catches a deadlock fast, anywhere, including
  before the first test. **The per-test ceiling** stamps each test with a wall-clock budget and fails
  when it is exceeded *even while progress is being made*, which is exactly the case the heartbeat
  cannot see. The failure names the test, its runtime, and its budget, and says which of the two
  failures it is, so a livelock is diagnosable rather than an anonymous timeout.

  **Budgets are per test, not one global ceiling**, and that is the judgment call worth recording.
  std_net honestly runs 300 to 344 s, so a single ceiling would have to sit near 700 s, which would
  let a two-second unit test spin for eleven minutes before failing: a limit that catches almost
  nothing is not worth the false confidence. Instead the default is a tight 90 s and the known-slow
  tests declare their own cost in `SLOW_TESTS`, each entry carrying the reason. The exception stays
  visible and reviewable instead of being absorbed into a number that protects nothing, and the cost
  is one table with (today) one row.

  **What each mechanism can and cannot see** is stated in `testing.rs` and notes/scheduler.md rather
  than left implicit, because that is what went wrong the first time. Neither can distinguish a
  livelock from slow-but-correct work while it runs; only the budget, a human declaration of expected
  cost, separates them. A feature-gated probe (`watchdog_probe`) loops forever doing a full rendezvous
  each pass, so the heartbeat sees a healthy kernel and only the ceiling stops it; it is expected to
  fail and stays out of the normal suite. And `qemu-bounded.sh` remains the outermost backstop, for a
  kernel wedged so hard the timer IRQ stops: it did not fire in the reported case only because that
  run invoked `cargo` directly instead of the wrapper, which is the argument for the in-kernel check.
  **A bypassable backstop is not a backstop.**

## 29. The framebuffer is a bigger grant, not an exemption (milestone 29, the display ladder's rung one)

**Built 2026-07-29**, both ISAs, in QEMU. The demonstrator's first pixels: a userspace virtio-gpu
driver that puts a known image in a scanout framebuffer, confined exactly like the disk and net
drivers, plus a *separate* client process that draws into a shared surface through a capability. Font
rendering, the VT state engine, scrollback, and input are deliberately not in this rung; they arrive
as clients of the contract this one draws. See notes/framebuffer-contract.md.

**The split is the deliverable, not the picture.** The driver holds the `Virtio` capability, the
device's interrupt, and the whole DMA region; the client holds an endpoint and the pixels and is
handed no physical address at all. It cannot program a queue, ring a doorbell, or see a descriptor
ring (they are in a page it is not mapped), so the worst a hostile client can do is draw nonsense.
Rung two (the compositor, milestone 33) takes the client's place unchanged, which is why the contract
is written down as a note and a host-tested crate (`crates/gfx_proto`) rather than left implicit in
two programs.

**The memory decision, stated as a rule because it will recur.** A 128x64 surface at 4 bytes a pixel
is 32 KiB, and every other driver here gets one 4 KiB DMA page. The tempting shortcut was to let the
framebuffer live outside the registered DMA region, since it is "just pixels". That is exactly
backwards: it would put the one device that reads bulk memory outside the confinement everything else
is inside. So the region is **wider, not special**: `1 + SURFACE_FRAMES` contiguous frames, page 0 for
the rings and control buffers (driver-private) and pages 1.. for the surface, registered whole.
**A device that needs more memory gets a bigger grant, never an exemption.** The block server's
two-page region (§27 era) was the first instance; this is the general form.

**`crates/dma_validate` needed no change,** and that is a property rather than luck: it bounds
`addr..addr+len` inside a region whose size is a parameter, so the region growing ninefold left the
proof covering it. Recorded because the increment was explicitly allowed to stop and ask if the
framebuffer's size had required touching a proved crate, and it did not.

**The confinement hazard a GPU adds, and the barrier that actually stops it.** This is the first
device here whose DMA addresses do not all arrive in descriptors. A virtio-gpu's *backing* addresses
ride in a `RESOURCE_ATTACH_BACKING` **command payload**; the kernel bounds the descriptor carrying
that command, but the addresses inside it are bytes it does not parse. It should not start parsing
them, because that would put device knowledge in the transport, which is the line §18 draws, and it
would be a per-device arms race. So the **IOMMU** (§20) is the barrier for this class of address, and
it is proved rather than assumed: `the_iommu_refuses_the_gpu_a_framebuffer_outside_the_drivers_grant`
gives an attacker exactly the honest driver's authority, points a resource's backing at a frame the
kernel left out of its domain, and asserts the IOMMU recorded a fault there. Both ISAs.

Two consequences follow and are recorded so they are not discovered later. `iommu_platform=on` carries
more weight for the GPU than for the disk (drop it and the disk still has the shadow ring; the GPU has
nothing). And **on a board with no IOMMU this hazard is open**: the VisionFive 2 has none, so a display
driver on first silicon is either trusted or the transport grows a virtio-gpu-aware check. That is a
decision for whoever sequences 16a, not one this milestone gets to make silently.

**Correction, on the record: a device's "command accepted" is not evidence of a DMA.** The escape test
first asserted that the device *refused* the out-of-grant backing. It did not. QEMU's DMA layer
answers a translation failure by handing the device a bounce buffer rather than failing the mapping,
so the command returns OK while the bytes the device gets are not the victim frame's. The confinement
held; only the error reporting did not survive the trip. The test now asserts on the IOMMU's fault
queue, the hardware's own account, and the response code is printed for the record. An earlier
iteration also aimed the escape at "the frame just past my region", which was wrong because the
kernel's shadow page is allocated immediately after the region and *is* in the domain; the kernel now
picks the victim frame and hands it to the attacker.

**A found limit, recorded for whoever handles faults in production: the RISC-V IOMMU's fault queue
overflows silently.** The driver gives it 128 records and never clears the queue's overflow bit, so a
flood of faults latches the overflow and no further fault is recorded at all. Found the right way: the
escape test's first version attached a 4096-byte backing, produced a flood, and the *next* test in the
suite (§20's `the_iommu_faults_a_dma_that_escapes_the_domain`) then reported the IOMMU as not confining
the device. Mitigated locally (the escape attaches four bytes, so one translation and one fault; the
test drains the queue afterwards) rather than by changing the arch driver, which is a different lane.
What is left for a fault-handling milestone: clear the overflow bit when draining, and decide what a
production kernel does when a confined device faults. See notes/framebuffer-contract.md.

**Correction: the PCI transport was synthesizing a device id nobody had checked.** `Transport::Pci`
answered a driver's virtio-mmio `DeviceID` read with a hardcoded 2 ("I am a block device") for every
device on the bus. Harmless while only the disk and NIC rode it, since neither reads the register, but
it is a manufactured fact of the shape the runners were taught to fail loudly on. The GPU driver is
the first that checks what it is talking to, and it found the lie. The transport now carries the
virtio device type recovered from the PCI id (`0x1040 + type`).

**PCIe only, and that is the honest parity statement.** Neither `virt` board has a virtio-gpu on its
virtio-mmio bus in any configuration, so unlike the disk and the NIC there is no mmio twin to prove
the transport seam over twice. The parity that §19 demands is aarch64 `virt` and riscv `virt`, and
both carry `virtio-gpu-pci` over the §18 transport, proven by **one arch-neutral test** rather than
two copies that can drift.

**What the pixels are proven by, in two halves, because one half cannot reach the whole path.**

*In the guest, the framebuffer.* The pattern is a per-coordinate function rather than a fill (a blank,
filled, transposed, one-row-shifted, or one-pixel-shifted surface all fail), the digest is position
sensitive, and two independent witnesses in two address spaces report it (the client from its mapping
after the flush, the driver from a different mapping after the device reported the transfer complete),
both compared against a value the kernel computed itself.

*From the host, the scanout.* An in-guest test cannot go further: `-display none`, and nothing in the
guest can read QEMU's host-side surface back, so a wrong pixel format or scanout rectangle would pass
the guest's half while showing garbage on a screen. So the **host** proves that half. QEMU's monitor
works headlessly, and `cargo xtask` drives it beside the ordinary test run (no second boot: the pattern
stays on the scanout until QEMU exits, so nothing needs synchronizing), dumps the scanout with
`screendump`, and compares the PPM against `gfx_proto::pixel` pixel for pixel. **Both ISAs, and the
checker has its own negative control** (`cargo test -p xtask`: it must reject black, red/blue-swapped,
row-shifted, one-pixel-wrong, and the default 640x480 console). The geometry is part of the assertion,
which means a dump that is 128x64 at all is evidence `SET_SCANOUT` reached the device.

One ordering fact is load-bearing and deliberately fail-loud: the confinement test resets the device,
which destroys the scanout, so it must run before the pixel test; it is named
`a_backing_outside_the_grant_is_refused_by_the_iommu` to sort first, and a reordering fails the scanout
check rather than quietly skipping it. What remains unproven is only what QEMU cannot answer: that a
physical panel would show this, which is a silicon question. See notes/framebuffer-contract.md.

**Deferred, untouched:** the VT engine's language (libghostty-vt in Zig through its C ABI, or `vte` in
Rust as the single-toolchain fallback). This rung needs neither, and the contract carries pixels, not
text, so either slots in above it later.

## 30. The DMA boundary is proved for descriptors, and the proof says where it stops (milestone 35)

**The decision.** DMA confinement was the one isolation boundary in the system carried by tests rather
than proof, and it is the boundary that makes "you need not trust the driver" true. It is now
machine-checked, and **the milestone's deliverable is as much the boundary statement as the proof**:
the record must not let a reader conclude the whole DMA surface is proved when one path is proved and
another is mitigated by hardware we will not always have.

**Why now, and why it is not merely tidiness.** Milestone 16a's board, the VisionFive 2, has no IOMMU.
§20's hardware confinement demoted the software validator to defence in depth; on first silicon there
is no hardware underneath it, so it becomes the *sole* DMA confinement. A tested-but-unproved validator
is exactly the wrong thing to put in that position, and the ordering follows: prove it before or with
16a, not after.

**What is proved.** Three things, all `#[cfg(kani)]`, all in `script/verify`:

1. **The validator** (`crates/dma_validate`, seven harnesses). No descriptor the kernel copies into the
   shadow ring the device reads is ever out-of-region or indirect, for every descriptor bit pattern and
   every region: both directions (flags fully symbolic, so RX device-writes are covered), indirect
   descriptors, chains including cycles, ring-index wraparound through `u16`, overflowing address
   arithmetic, multi-queue block isolation, the oversized-batch bound, and the
   mutated-after-validation (TOCTOU) case the shadow ring exists to close. Termination is part of the
   property, not an assumption: the loop bounds are set one above what the code can need, so Kani's
   unwinding assertion fails if any input could spin the walk.
2. **The `Untyped::SPLIT` mint site** (`caps::split_never_widens_rights`). See the amendment below for
   what the property actually is, because §16's `GRANT` change makes the naive phrasing wrong.
3. **The IOMMU domain's page set** (`paging::domain`, six harnesses). The domain maps every whole page
   of the grant and no byte outside it, proved in both directions and format-independently, so one
   proof covers SMMUv3 (VMSAv8-64) and the RISC-V IOMMU (Sv39). **This reverses the milestone's own
   first answer**, which declined the property as the build-and-translate BMC wall. That was the right
   diagnosis of the wrong target: the wall is a symbolic IOVA walking a *built* table, and the page set
   is loopless arithmetic needing no tables. Factoring it out (`grant_pages`, `grant_page`) and having
   the builder call it took the property from "tested" to "proved" in a quarter of a second of solver
   time. The correction is recorded rather than smoothed over, because the lesson is a standing one:
   `notes/verification.md`'s rule "prefer refactoring the logic to shrinking the proof" applies to
   *declining* a proof too.

**The amendment §16 forces on the SPLIT property.** `Untyped::SPLIT` grants the child `GRANT` so a
budget is delegable, so "SPLIT never changes rights" is false and "SPLIT never widens rights" needs
saying precisely. The property proved is: **the child's rights are exactly the parent's**, `SPLIT`
being an *inheriting* mint (`Cap::mint_child`) with no rights argument at all. That is strictly
stronger than "no wider" and it is the shape that makes the delegable-budget behaviour correct rather
than an exception: a root untyped is minted once with `READ|WRITE|GRANT` (`untyped_root_cap`), `SPLIT`
inherits whatever the parent holds, and `CAP_INSERT` narrows on the way into a child. So rights along a
budget tree are monotonically non-increasing from the root, `GRANT` reaches a child only because the
root had it, and a spend-only untyped provably cannot split itself a `GRANT`-bearing child. The
delegability is a property of the *root's* mint, not a widening at `SPLIT`. Because `SPLIT` takes no
rights argument, there is no input by which a caller could ask for more.

**The residual gap, stated as the deliverable it is.** Milestone 29 (§29) found that a virtio-gpu's
backing addresses ride in a `RESOURCE_ATTACH_BACKING` **command payload**, not in a descriptor. The
validator **structurally cannot see them**: they are not in its input, so no amount of proving it
harder reaches them, and teaching the transport to parse device commands would put device knowledge in
the layer §18 keeps neutral and start a per-device arms race. So:

- an address reaching a device through a **descriptor** is *provably* confined to the driver's grant;
- an address reaching a device inside a **command payload** is confined by the **IOMMU alone**, and
  that confinement is **attacker-tested, not proved**
  (`the_iommu_refuses_the_gpu_a_framebuffer_outside_the_drivers_grant`, both ISAs, asserting on the
  hardware's own fault queue);
- on a board with **no IOMMU, nothing confines the payload path.** Not the validator, not the hardware.

That last point is where 16a's reasoning inverts, and it is the thing this section exists to prevent
being discovered later. The argument "prove the validator, because on the VisionFive 2 it is all there
is" works only for the path the validator covers. On that board a display driver is either **trusted**
with all of physical memory, or the transport grows a virtio-gpu-aware check and pays the §18 cost
knowingly. Whoever sequences 16a chooses; §29 already recorded that it is not milestone 29's call, and
milestone 35 does not get to make it silently either. The same gap is open under HVF, where PCIe DMA
runs unconfined by standing default.

**Bounds, because a proof whose bounds hide the interesting case reads as stronger than a test.** The
queue size the harnesses fix is 8, which is the kernel's own `QSIZE` and not a proof convenience:
`setup_queue` refuses a larger ring, so no unproved configuration exists. To keep that true rather than
merely currently-true, the ring layout constants now **live in `crates/dma_validate` and the kernel
aliases them**, because a proof about a copy of the layout proves nothing about the layout that runs.
Every attacker-controlled value (region base and size, descriptor `addr`/`len`/`flags`/`next`, both
ring indices) is unbounded. notes/verification.md carries the full table with each bound's
justification, and the one place the composition is an argument over four harnesses rather than a fifth
harness is named there too.

## Reading

- **The seL4 manual**, and Klein et al., *seL4: Formal Verification of an OS Kernel* (SOSP'09)
- **Liedtke**, *On µ-Kernel Construction* (SOSP'95) — why Mach was slow and why that was not a law
- **xv6 book** (MIT, ~100pp) for how a real Unix-shaped kernel is structured. Read it as the
  road not taken (§10), not as a template.
- `rust-raspberrypi-OS-tutorials` for the aarch64-specific mechanics
- OSDev wiki as a reference, not a tutorial
- *Operating Systems: Three Easy Pieces* for the theory
