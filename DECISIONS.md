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
  already hands off to the portable `sysinit`. `shell_service` was kept as dead code for reference at
  the time, and **milestone 41 deleted it outright on 2026-07-30**, along with `input_service`. That
  supersedes the sentence this one replaces, and the reasoning is the project's existing rule rather
  than a new one: the heap and slab crates were deleted the same way on 2026-07-27, because *the git
  history preserves the work and a demonstrator's tree should hold what it ships* (notes/heap.md).
  Nothing was lost that this decision had not already replaced: the capability milestone 10 delivered,
  a shell at EL0 spawning processes on command, is exactly what userspace init does now, and doing it
  in userspace is the thesis rather than a consolation.

  **One honest caveat on that claim**, since it is the sort of thing that decays: no test in the suite
  boots the interactive shell, so "the capability still exists" rests on the hand-validated boot path
  rather than on the gate. Milestone 31's phase 3 is that one item, and it should gate that boot
  before anything else leans on it.
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
name. **Honestly `Unsupported`, because the contract has no verb for them** *(the create and truncate
half of this list is superseded by the phase-2 amendment two paragraphs down; the rest still holds)*:
creating a file and
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

**Amendment (milestone 31 phase 2, 2026-07-30): `File::create` and `std::fs::write` work.** The first
of those two reported items is built (§27's amendment carries the contract side), so the PAL binds
`create`/`create_new` to `CREATE` and `truncate` to `TRUNCATE`, and the "creating a file and
truncating one are honestly Unsupported" line above is retired. The order in `File::open` is POSIX's
and it matters: open, then create only if the open reported `NotFound` and the caller asked for it,
then truncate after a successful open. `std::fs::write` is `create(true).truncate(true)`, so getting
that order wrong would leave the old tail behind on exactly the path that exists to *replace* a
file's contents, which is the day-costing confusion §27 records being corrected four times. A
`create_new` over a name that exists closes the handle the probing open minted and returns
`AlreadyExists`, rather than leaking it for the life of the process: the error path is the one nobody
exercises, so it is the one that leaks.

Creating a *file* was never what §27 kept host-side. That was creating a *filesystem*, which needs
uuid and getrandom; `Transaction::create_node` is not std-gated, so a file is made on-device without
entropy ever becoming a userspace dependency. The read of §27 that conflated the two is corrected
there.

Still Unsupported, each because no verb backs it: directory iteration, `mkdir`/`unlink`/`rename`,
symlinks and hard links, `canonicalize`, permissions, file times, locks, and `duplicate`. And a
program holding a **per-file** grant rather than a directory (§27's caretaker) sees the narrowing
through ordinary `std::fs` errors: the one granted name opens, any other is `NotFound`, and a write
through a read-only grant is `ReadOnlyFilesystem`. No std API had to change to express that, which is
the point of having bound the PAL to a capability contract rather than to a namespace.

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

**Amendment (2026-07-29, corrected four times in one day; THIS paragraph is the settled account, and
it is the one that explains the other three). There was never a filesystem bug. The write always
succeeded.** Everything below this paragraph is superseded and kept only because how this fact
wobbled is worth more than the fact.

The cause is **the missing `TRUNCATE` verb meeting a whole-file comparison.** A write shorter than
the file does not truncate it. One boot's FS client left a **64-byte** payload in `scratch`; the next
boot's `std::fs` test wrote its **61-byte** pattern, asserted the whole file equalled it, got 64
bytes back (61 new plus the old three-byte tail), and panicked *inside its write block*. That panic,
read as "the server refused the write," is the entire bug. No allocator loop, no heap exhaustion, no
accumulated mount state, no device-only defect, and no error reply, which is why nobody ever found
the errno: **there was none to find.**

That also explains why three investigations produced three incompatible answers while every one of
them reported honestly. The symptom depended on what the *previous boot's* client happened to leave
behind, and that changed as the client changed, so each round measured a genuinely different thing.
Two lessons worth carrying off, because neither is about filesystems:

- **An order-coupled gate manufactures facts.** `mkredoxfs` ran once for both ISA legs and the
  aarch64 leg mutates the image, so whichever leg ran second failed and neither was reproducible
  alone. Each leg now regenerates its own fixture, and `CRICKER_KEEP_REDOXFS=1` makes the cross-boot
  case *deliberate* rather than an accident of ordering.
- **A test that asserts on whole-file equality asserts on history it did not write.** The fix is at
  that layer, not in the engine: the client restores the fixture as its last write, all its payloads
  are one length, and the post-run host check compares content **and length**, so a future client
  leaving a longer file fails the gate instead of corrupting a later boot's assertion. Pinned by a
  millisecond host test carrying the real 64/61/3 byte counts, so if it ever fails, the contract grew
  a verb and that was a decision.

Two hypotheses died by measurement, and both are recorded as dead rather than left looking plausible.
Heap exhaustion and accumulated mount state: the real engine under the FS server's own allocator,
capped identically, image in a `static` so it stays off the heap exactly as a real disk does, runs 30
mount-and-write cycles with the high-water **flat at 352 KiB**, four percent of the 8 MiB budget, the
cap never once refusing a growth. Raising `FS_BUDGET_PAGES` would have fixed nothing, and a number
chosen to make a test pass would have been a coincidence rather than an argument.

The errno plumbing built to chase this stays, because the reason it was unreadable was real: the
client routed every failed reply through a panicking `check`, so a trapped client told the waiting
test only that something went wrong while the server's reason died with the process. A negative reply
is now sent, carrying the **raw reply word alongside** the decoded errno rather than instead of it,
because the wire's negated errnos overlap the kernel's own `invoke` errors at −1..−8 (the
notes/std.md wart) and a small value is otherwise ambiguous between "the server returned this errno"
and "the IPC itself failed."

*Superseded (2026-07-29), kept for the record.* The previous settled account said there was no
allocator loop but that a second mount of a used image failed its write for an unrelated reason. The
first half was right. The second half named a real symptom and mislocated it: the mount was fine and
the *assertion* was wrong.

Measured on a clean build: the FS client writes the same block **three times in one run** and passes
on both ISAs, and the image afterwards carries the third payload, so the repeat write reached the
disk. A `VERIFY_WRITES` switch that reads every written block back through `IpcDisk` and compares
never fired, so the blk IPC transport is faithful (nothing lost, nothing misdirected, no stale read).
And the observed failure was never a spin: the std program's own `expect` panics, which is what
truncated the transcript that got read as a hang. The "400% CPU looping in
`Transaction::sync_allocator`" reading does not survive a correct build.

What was actually broken: `mkredoxfs` ran **once for both ISA legs**, and the aarch64 leg writes the
image, so the riscv leg mounted an image a previous *boot* had mutated. Whichever leg ran second
failed, and neither leg was reproducible on its own. That is why three separate investigations,
each measuring a differently-broken setup, produced three incompatible answers, and it is worth
naming as a failure mode: **an order-coupled gate manufactures facts.** Each ISA leg now regenerates
the same known-good fixture.

That is determinism, not a fix. A second mount of a *used* image still fails its write, and the
recipe is recorded in notes/fs-server.md (generate once, run one leg, then the other without
regenerating) along with the leading hypothesis: accumulated **mount** state rather than bad data. A
used image carries a higher header generation, a longer allocator log and more live tree blocks, so
the second mount allocates more heap (capped at 8 MiB in `fsserver.rs`, bounded by
`FS_BUDGET_PAGES`) and may reach an allocator squash path a pristine mount never does. The next step
is reading the errno the server returns, which nothing currently surfaces. Note the cost of the fix:
the gate no longer exercises the cross-boot case at all, so this bug is now known-and-untested,
which is the same shape of invisibility that hid it in the first place.

The missing test layer, which should have existed from the start, now does: the EL0 binary's chunking
was extracted into a host-testable `BlockDisk`/`BlockIo`, because chunking that lives only in the EL0
binary is chunking no host test can reach. Ten host tests run in milliseconds (repeat writes,
record-sized writes across the multi-block and compressed-tail paths, and write then drop the mount
with no unmount then reopen and write again) and all pass. That is the decisive comparison: the host
does not loop, so there is no upstream RedoxFS bug and no vendored patch to offer.

*Superseded, kept for the record.* An earlier amendment claimed a first write works and a repeat
write to the same block still loops, reasoning that `mkredoxfs` rewriting the target to a placeholder
made every gated write a first write. The premise about `mkredoxfs` was right and the conclusion was
wrong: the harness was indeed hiding something, but not a loop. This
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

### Amendment (milestone 31 phase 2, 2026-07-30): `CREATE` and `TRUNCATE` exist, and a per-file grant is a caretaker process

**The two verbs are built.** `CREATE` (opcode 6) resolves a name under the bound directory and makes
it, answering `EEXIST` if it is already there and modifying nothing: create is create, not
create-or-open, because a caller that wants either has to say which it got, and the alternative is
what makes a partly-working write read as a working one. `TRUNCATE` (opcode 7) sets a file's size in
**both** directions, growing with zeroes and shrinking by discarding, with the new size in the second
word rather than the length field (the length field is clamped to one page, which would silently cap
a truncate at 4096 bytes). Both are host-tested in the sans-IO core and bound in `std::fs`, so
`File::create` and `std::fs::write` work rather than returning `Unsupported` (§22's amendment).

**One gap closed while adding them, and it was ours rather than RedoxFS's.** RedoxFS's `check_name`
rejects `:`, over-long names, and duplicates; `/`, `.` and `..` pass straight through. Nothing walked
paths, so nothing escaped, which made the "one component, no `..`" rule true by the absence of a
walker rather than by a check. `CREATE` turned that from a latent oddity into something a client
could *write*: `create_file("../escape")` made an entry literally named that. `check_component` now
enforces the rule at our boundary, deliberately there and not patched into the vendored engine,
because it is a rule of this contract and not a bug in a component whose callers may name entries
whatever they like.

**A per-file grant is a separate process, and that is the decision.** Milestone 31's `run wc
report.txt` must hand over one file; the unit of authority here is a directory. The narrowing is
`user/src/fwarden.rs`, a **caretaker** (Mark Miller's term): it holds the directory capability, opens
the granted name once at startup, and serves the same `fs_proto::fs` contract on its own endpoint
with a namespace of exactly one name. Three rules, each phrased as a fact about what the holder has
rather than as a permission refusal, because there is no policy here to consult:

- `OPEN` of any other name is `ENOENT`. In this scope there is no such name. The holder cannot
  enumerate and cannot learn what else the directory holds.
- `CREATE` is `ENOTDIR`. A file capability is not a directory, so "make a name in it" is not a
  request that means anything.
- `WRITE` and `TRUNCATE` are `EROFS` without the write direction. `EACCES` was rejected on purpose:
  it implies a policy that could have said yes.

**Why a process and not a check inside the FS server.** The server receives on one endpoint. Serving
a second, narrower one would need a receive over a *set* of endpoints, which this kernel does not
offer; adding it means giving endpoint capabilities a **badge** (seL4's answer), which is a design
fork and is recorded here as the alternative rather than taken. The caretaker needs nothing new: it
is an ordinary FS client above and an ordinary FS server below. And it is the stronger form of the
claim. The confined program holds an endpoint to the warden and nothing that names the FS server, so
"it cannot reach a second file" is a property of its cspace rather than of a branch it is trusted to
take. The boundary is an address space, which is the same reason §31's checker lives outside the
component it checks.

The grant costs no memory: the name and direction ride in the warden's three `START` argument words
(`fs_proto::grant`, 16 bytes of name), and the one frame is shared by all three processes, which is
sound because every request on both hops is a blocking `CALL`, so the client is parked inside its own
call for the whole time the warden is using the page.

**Proven on both ISAs by an attacker, twice, and the second run is what makes the first mean
anything.** The attacker reports a bitmap of what got through rather than a pass. Read-only: every
bit clear, against a neighbouring file that really exists and that the warden really could open.
Read/write, same shape: the two write bits **set** and everything else clear. A warden that refused
every request passes the first test and fails the second. Each accepted write is read straight back,
because "the server accepted my write" and "my write landed" are different claims.

**The interactive shell still refuses `file:`, and that refusal is true rather than pending.** The
boot that starts the shell wires no FS service, so the shell holds no directory to narrow, and `caps`
says so in those words. `capsh` carries the whole vocabulary (a `FileSpec` in the manifest, a
`FileGrant` in the endowment, refusals both ways) and the decision is a function of what the shell
*holds*, not of the calendar; phase 1 hardcoded that refusal, which was true when written and would
have quietly become a lie. Wiring an FS service into the interactive boot is the remaining step, and
nothing in the suite gates that boot, which is why it is recorded here instead of built.

**Also settled, by measurement: the FS server's stack.** RedoxFS recurses in 8 KiB frames, and the 33
pages it had were **528 bytes short** once `CREATE` and `TRUNCATE` added a level of tree recursion.
The server died mid-request and its client blocked forever on a `CALL` nobody would answer. The size
is now measured rather than chosen: the kernel poisons every FS-server stack page and
`fs_service::fs_stack_used` reports the deepest word that is no longer poison (135,696 bytes on
aarch64, 135,824 on riscv64, of a 397,312-byte grant), with a test on both ISAs that prints it every
run and fails under a quarter left. notes/fs-server.md carries the incident, including the two
instruments it blinded and why a ceiling failure reports the ceiling and not the cost. *Milestone 37
measures 127,408 and 127,536 for the same grant, 8 KiB lower, and does not attribute the drop; both
numbers and the reasoning are in the note. It also widened the instrument to a maximum over every FS
server a boot starts, so the mount that recovers a crashed disk is measured too, which is the case
most likely to recurse further than a clean one.*

**The remaining honest gap: a client of a dead server blocks forever.** §26's fault endpoint is the
mechanism that would turn that into a message a supervisor can act on, and wiring the FS service into
a supervision tree belongs to milestone 23.

**The missing `TRUNCATE` is no longer only a missing feature; it is a sharp edge that cost a day.**
The four-times-corrected amendment above traces to exactly this gap: a short write leaves the old
tail, so a caller that reasonably expects `write` to replace a file's contents gets a longer file than
it wrote. `std::fs::write` reporting `Unsupported` is honest, but the *partial* capability underneath
it is the trap, because a write that half-works reads as a write that failed. Adding `TRUNCATE` would
remove the edge rather than merely add a verb, which is the strongest argument yet for taking the
decision, and it belongs with `CREATE` in milestone 31 phase 2.

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
- an address reaching a device inside a **command payload** is confined by the **IOMMU alone**. Item 3
  above is the one useful thing this milestone could prove about that path, and it is a narrowing rather
  than a closing: such an address is stopped by having no translation in the device's domain, so "the
  domain maps exactly the grant" is precisely the property the barrier rests on, and it is now proved for
  every grant. That the hardware then faults an out-of-grant address stays an attacker test
  (`the_iommu_refuses_the_gpu_a_framebuffer_outside_the_drivers_grant`, both ISAs, asserting on the
  hardware's own fault queue). The transport still cannot see these addresses, and the enforcement is
  still the hardware's;
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

## 31. The foreign-language seam: C holds no capabilities and makes no syscalls (milestone 36)

**Built 2026-07-29**, both ISAs, in QEMU. *(Section number claimed while two other lanes were open;
if one of them also took 30, this is the entry to renumber. It depends on §4 rule 3, §15, §16, §22,
and §26, and nothing depends on it yet.)*

A memory-unsafe C component, compiled by bare-metal clang, confined by the kernel like any other
process, faulting on a deliberate out-of-bounds write, and restarted by its supervisor. The component
itself (`user/c/cseam.c`, 150 lines) is throwaway on purpose: **what this milestone de-risks is the
seam**, before milestone 29's libghostty-vt rung and milestone 23's vendor-component claim owe
anything to another project's toolchain and API churn. Concept note: notes/c-seam.md.

**Why C is the right thing to run, not a dilution of the thesis.** §14 promises a verified core that
confines unverified workloads. C is the most unverified workload available, so it is the strongest
available test rather than a compromise, and the contrast with a monolith is concrete: in-kernel C
means one bad index is a kernel compromise (the peer project Atom keeps FAT32, AHCI, and xHCI in the
kernel today); confined C means one bad index scribbles its own grant and gets restarted. Isolation
here is enforced by mechanisms that do not know what a language is: page tables, unforgeable
capabilities, the DMA validator, the IOMMU.

### The seam's rules, which are the decision

1. **The C makes no syscalls and holds no capabilities.** A Rust `user_rt` shell (`user/src/cshim.rs`)
   holds every capability and performs every IPC; the C is called over the C ABI and gets a pointer
   and a length. This is not a request the C is trusted to honour, it is a property of what it can
   name: a syscall needs a capability slot, and the C never sees one. So a foreign component **cannot
   widen the kernel's syscall surface** (§4 rule 3), and the confinement claim is exactly as narrow
   as it should be: the C can corrupt memory inside a grant the shell already had, and nothing else.
2. **What crosses is scalars and buffers only.** `(u8*, usize) -> u32`. No structs, no callbacks into
   Rust, no ownership transfer, no error type. The layout of the shared page is agreed by a comment in
   both languages rather than generated bindings, which is the right trade for one page and would not
   be for a real API. Same sans-IO shape RedoxFS's `Disk` trait already uses (§27), across a language
   boundary instead of a trait boundary.
3. **The libc is two symbols, `malloc` and `free`.** Tier two of the roadmap's three tiers
   (freestanding / a handful of symbols / full POSIX; design/roadmap.md's milestone-36 block). The C object references five (`malloc`, `free`,
   `memcpy`, `memset`, `strlen`, identical on both ISAs at every optimization level, with no
   compiler-rt helper and no `__stack_chk_fail`), and the linker demands only two, because
   `compiler_builtins` already supplies the other three weakly for the bare targets. **Tier three is
   not walked**: a component needing `open`, `fork`, `socket`, or threads needs a real libc port, which
   is §15's "later, if ever" road, and saying so is what keeps this from becoming that project.
4. **`malloc` comes from the process's own untyped budget** (§22 / milestone 27's `UntypedHeap`), wired
   to the very region the instance was built in. So the C heap is the process's own memory, a C leak
   exhausts that instance and nothing else, and the single `Untyped::DESTROY` that reaps the corpse
   reclaims the heap with it. `free` carries no size while `GlobalAlloc::dealloc` needs a `Layout`, so
   the shim stores a 16-byte header; that is a real and unavoidable cost of the C ABI, not a shortcut.
5. **Bare-metal clang, one compiler for both ISAs, resolved rather than assumed.** `user/build.rs`
   looks for a clang whose `-print-targets` lists **both** aarch64 and riscv64 (`$CRICKER_CC`, then
   Homebrew's llvm keg, then `clang` on `PATH`) and fails with installation instructions otherwise, the
   same discipline `xtask`'s `llvm_tool` uses for `llvm-objcopy`. Requiring both backends from one
   compiler even when building one ISA is §19 applied to the toolchain: a machine where the two
   architectures are compiled by two different clangs is a machine where "works on aarch64" stops
   predicting anything about riscv64. Apple's clang is therefore **rejected on purpose** (no RISC-V
   backend), and `script/bootstrap` grew `brew install llvm` / `apt-get install clang`.

### What the confinement test proves, and how each claim is proven rather than assumed

`kernel/src/user::c_seam_tests`, both ISAs. `cwarden` builds the shim, supervises it, and holds the
witness pages; every assertion is made from **outside** the faulting address space after the component
is dead, because a checker inside it could only report what that address space could see.

- **It faults.** The death message exists at all, with `EVENT_FAULT` and a non-zero kernel-stamped tid.
- **The fault is the planted bug.** The kernel's reported fault address equals the address the C code
  computed. Without this the witness checks would be vacuous: a crash on the way to the bug would look
  identical.
- **Nothing outside the grant changed, proven twice because there are two different claims.**
  `WITNESS_RO` is the **same physical frame** mapped read-only into the component and read/write into
  the warden, so an unchanged page is not "the store landed elsewhere"; the page was reachable and the
  store did not happen. `WITNESS_FAR` is a **different frame at the same virtual address**, which is
  the statement that a virtual address means nothing outside the address space that owns it. Both
  patterns are position-derived and checked byte by byte (milestone 29's two-witness discipline).
- **The restart works.** Three instances run in sequence: two crash, the third computes a checksum and
  a transform in C, writes them into the shared grant, and exits cleanly. The warden checks that output
  against an independent Rust implementation of the same definition, so a restart producing a process
  that merely reports for duty fails. The clean exit arrives as `EVENT_EXIT` and is **not** restarted,
  which is the other half of §26.3.
- **The control that makes the rest mean anything.** Each misbehaving C function stores *inside* its
  grant first, and that store must be visible. A process whose stores never worked would satisfy every
  witness check while proving nothing.

### What authority the supervisor had to hold, and what it would have preferred

The open fork this feeds. `cwarden` is builder, supervisor, and checker in one process, and the reason
is authority, not convenience: **reaping a corpse needs `WRITE` on the region it lives in, which is the
same right that builds one.** So a supervisor that restarts its child holds construction authority, or
proxies the reap through something that does.

- **What it had to hold:** a full-rights untyped budget, for its whole life. From that it can
  `SPLIT` a region, `RETYPE` frames and kernel objects, build any address space and any thread, and
  `DESTROY` regions. The reap needs the last of those; everything else came attached.
- **What it would have preferred:** a **reap-only right** over the instance region and nothing more.
  That is enough to collect a corpse and return its pages, and it is not enough to build a process.
- **The alternative that exists today and why this milestone did not use it.** Milestone 22 phase
  B.2's proxy: a supervisor holding no memory that asks a construction sub-server to reap
  (`subsup` -> `spawner`). That is the right answer for a system's init, where the point is that init
  can no longer build. It is the wrong answer here, because it moves the requirement behind an IPC hop
  and the requirement is the interesting part. **The concrete requirement, for whoever decides the
  fork: a supervisor needs exactly `DESTROY` on one region it did not create.** Neither a rights bit
  on `WRITE` nor an `Untyped::REAP` method was invented here; that is a rights-model and
  syscall-surface decision, and §26's phase-B block already records it as one.

### The honest caveats, including what a spike does not prove

- **A throwaway component does not prove a vendor component.** What is untested: a real build system
  (this is one `clang -c` invocation, not autotools, CMake, or `build.zig`), multiple translation units
  and their link order, headers we do not control, a component that wants `errno`, `assert`, `stdio`,
  locales, `setjmp`, floating point, or thread-local storage, and API churn across upstream versions.
  Milestone 29's libghostty-vt is a tier-one (freestanding) component by design, which is the cheapest
  possible next step up from here, and that sequencing is deliberate.
- **The C ABI's surface is one function shape.** No struct passed by value, no varargs, no callback
  from C into Rust, no C++ (name mangling, exceptions, static initializers, `operator new`), and no
  bitfield or enum-width question. Each of those is a real seam decision this spike did not have to
  make.
- **Nothing here is verified.** §18's proof toolchain does not reach C and never will; the C is
  confined, not correct. That is the whole point, and it is also the limit of the claim.
- **`-mgeneral-regs-only` is load-bearing on aarch64, and the reason is worth keeping.** Without it
  clang vectorizes the component's byte loops into NEON (53 vector-register operands in the object).
  The Rust target is `-softfloat`, the kernel never enables FP/SIMD for EL0, and the context switch
  saves no FP state, so vector registers in a confined component would be a trap or a corruption
  depending on which of those two bit first.
- **A cross-ISA difference in fault reporting, found here.** aarch64's `ESR_EL1` distinguishes the two
  bugs (`0x9200004f` permission, `0x92000047` translation); RISC-V's `scause` reports both as `0xf`,
  Store/AMO page fault, with no permission-versus-translation distinction. Both deliver the exact
  byte address, which is what the test asserts on, so the difference costs nothing today. It would
  matter to a userspace pager, which is on the SUSPEND tracker.
- **A trap worth one line, because the next person will hit it.** The obvious Rust `memcpy` shim is
  `core::ptr::copy_nonoverlapping`, which *lowers to a call to `memcpy`*: the shim calls itself. The
  symptom is a store fault exactly at `sp` at whatever stack depth the process was given, which reads
  like a stack-size problem and is not one. `compiler_builtins` avoids it with `#[no_builtins]`; a
  program crate cannot, so the right answer is to not define the three symbols the runtime already
  owns.

**Cost to a fresh clone:** one dependency. `script/bootstrap` installs a cross-capable clang, and from
this milestone on `cargo build -p user` needs one; without it `user/build.rs` fails with what to
install rather than with an undefined symbol. The roadmap already accepted that cost for Zig at 29, so
paying it here, where the component is disposable, is the point of doing the seam first.

## 32. A supervisor may collect a corpse without being able to build one

**Decided 2026-07-29 (Chris).** Reaping a dead child stops requiring the authority to construct
one. The supervision relationship, not the memory, becomes the unit of authority.

### The problem, measured rather than anticipated

Reaping is §16's `Untyped::DESTROY`, which requires `WRITE` on the region capability, and `WRITE`
is also what builds a process out of that region. So a supervisor whose entire job is "notice
`netd` died, restart it" has to hold the authority to construct arbitrary threads and address
spaces. That is a large right granted for a small purpose, and it is backwards for a capability
system: a compromised supervisor should be able to restart what it supervises and nothing else.

This was a prediction until milestone 36 made it a measurement. Its `cwarden` had to hold a
full-rights untyped budget for its whole life (`SPLIT`, `RETYPE`, `RETYPE_OBJ`, `DESTROY`) because
it needed the last one; everything else came attached. What it wanted was `DESTROY` on one region
it did not create: not `RETYPE`, not `SPLIT`, not a budget. Recorded in §31.

### The decision

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

### The refinement I made to Chris's ratification, stated so he can object

He approved putting the right on the child's fault endpoint rather than on a rights bit, on the
argument that the supervision relationship should be the unit of authority. I described it at the
time as an `Untyped::REAP` method gated on the fault endpoint. Designing it, hanging the method off
**the endpoint** rather than off `Untyped` is the better placement for the same reason: an
`Untyped` method has to name a region, and the entire premise is that the supervisor does not hold
one. So the invocation moves to the capability the supervisor actually has. Same principle, one
surface less. This is a placement change inside the ratified direction, not a second fork, and the
alternative is recorded here in case it reads otherwise.

### Alternatives rejected

- **A reap-only rights bit derived from the untyped at spawn time.** Cheap and fits the existing
  rights machinery, but it says "you may free memory" and then requires the kernel to work out
  *which* memory, which is the same coupling with an extra indirection. It also keeps the region
  capability in the supervisor's hands, which is the thing being removed.
- **Leaving it on construction authority until milestone 23 forces it.** Defensible, and rejected
  because 23 is the flagship and this is not work to be designing under that deadline. Milestone
  36 having already hit it in anger is the argument against waiting for a third instance.
## 33. The compositor's authority is memory, not messages (milestone 33, the display ladder's rung two)

**Built 2026-07-29**, both ISAs, in QEMU. One screen multiplexed among mutually distrusting clients,
each holding a capability to its own surface: software composition honouring a damage rectangle, input
routed by capability, and no ambient display. Concept note: notes/compositor.md. (Section number chosen
against main at `ab2c2bb`, where §30 is the DMA proof, §31 the C seam, and §32 the reap right. If a
concurrent lane has claimed 33 by merge time, renumber; the content does not depend on it.)

**Rung one's seam held exactly as promised.** The compositor takes `painter`'s place at the display
contract and `gpud` cannot tell the difference: `gfx_proto` and the driver needed **no change**, and the
only kernel-side addition is a wiring entry point that starts the driver with no client
(`display_service::start_driver`). Three of the four tests replace `gpud` with the kernel itself and the
compositor does not notice that either, which is milestone 23's swappable-component claim falling out of
a contract rather than being demonstrated on purpose.

**The decision, and it is the one thing to read here: authority is a mapping, not a message.** Every
client rings **one shared doorbell endpoint**, and both verbs on it (`HELLO`, `COMMIT`) are
content-free. A shared endpoint carries no sender identity (§26.5: no badged capabilities), so any
request that *named* a surface, a window, or a rectangle would be forgeable by any client. So nothing in
a message is trusted:

- every per-client fact lives in that client's own **control page** (geometry, id, damage rectangle,
  sequence), which only it and the compositor map. The only surface a client can describe is its own;
- every privileged answer travels through **privileged memory**, never a reply. A screenshot is a
  read-only mapping of the screen; the window list is a read-only page the compositor publishes. There
  is **no enumerate verb and no screenshot verb** to guard;
- keystrokes arrive in an **input ring** shared with the input source alone, so input cannot be
  injected by a client that can only ring the doorbell;
- the reply words carry status only, routed to the caller by the kernel's one-shot Reply (§12), so a
  request is answered without the compositor learning who asked.

The consequence is the point: **the compositor contains no authorization code at all.** It never asks
"may you?", because there is no request that would need the question, and it cannot leak the screen to a
client that asks because handing over the screen is not an operation it has. That is the difference in
kind from Wayland, which attaches client identity at the transport and then decides in code; its
security properties are properties of that code. Wayland's model approximates capability routing; this
is capability routing.

**No new syscall, no new method, no widened surface (§4).** The whole rung is endpoints, shared frames,
and `Spawn` grants that already existed. The one kernel-resource change is a constant: `KERNEL_EP_PAGES`
128 → 160, the third bump of a number whose comment has always said it grows with the suite. Recorded
with the standing suggestion it repeats: next time, reap the harness's boot services instead, which is
its own piece of work because endpoint teardown does not exist (§13 pins a region hosting an endpoint).

**The isolation is proved, not asserted, and the attacker is given every advantage short of a
capability.** It is the same binary as an honest client with the same grants, it paints its own window
correctly first, and the kernel hands it the **exact virtual address** of its neighbour's pixels. That
address is real twice over: every client maps its surface at the same virtual address (so it is the
number the neighbour itself uses), and the kernel allocates all the clients' frames as **one contiguous
run** so the page past a client's grant genuinely is its neighbour's memory, which the test asserts
before believing anything else. Then: the write faults (both ISAs, exact address checked on aarch64,
which is the ISA that records one); the attacker's report endpoint stays silent, so the "I read it back"
message it would otherwise send did not happen; the victim's witness pattern digests identically before
and after through the kernel's direct map; and the victim, held in a `CALL` across the whole attack,
re-reads its own surface afterwards and reports the same digest from its own address space.

**No ambient display, and the refusal has two dialects.** A client not granted an input endpoint has an
*empty cspace slot*: `NoSuchSlot` (-1), "there is nothing there", asserted by value because
`NotPermitted` would describe a weaker world. A client not granted the screen has *no mapping* where the
screen would be: its read faults. Same sentence, one in the cspace and one in the address space. A
capture client holds the screen and the window list **read-only**, so it can screenshot and enumerate
with no server involved, and its attempt to write the screen faults: a thing that may look at the screen
may not draw on it. Screen sharing is that grant aimed at a third party, and being a frame mapping it is
revocable through §13.

**The boundary this rung proves is client-to-client, and that is stated rather than implied.** The
compositor sees every client's pixels because compositing is reading them, so `compd` is in every
client's TCB for the contents of its own window, exactly as a Wayland compositor is. The question was
never whether a compositor could be prevented from reading a surface; it was whether a *client* could
be. What the capability model buys is that the compositor's authority is enumerated in one spawn literal
and cannot grow: no device, no interrupt, no DMA authority, no physical address, no way to name a frame
it was not handed. A compromised compositor can lie about the screen and read the windows it
composites, and cannot reach the disk, the network, another process, or the GPU's command stream (that
last one being rung one's confinement, and the reason the driver is a separate process).

**Damage is honoured, and that is observed rather than claimed.** The kernel plays the display server in
three tests precisely so the flush rectangle is a value it can compare: one commit produces one flush,
the flush is exactly the client's rectangle placed on the screen, and the poison the kernel wrote over
the rest of the scanout between two frames is **still there** afterwards. The same property is checked on
the host in microseconds by `crates/compose`.

**The picture is proved by four witnesses, one of which has to be the host.** The driver's digest of the
frames the device read (the compositor's startup frame, which is the background alone, so an empty screen
is a defined picture); the kernel's own pixel-for-pixel comparison through the direct map; a capture
client's digest taken in a third address space; and QEMU's `screendump` compared against the same
per-pixel definition. The fourth is not decoration: `-display none` means no in-guest witness can see the
device's own surface, so a wrong format or scanout rectangle would satisfy all three and show garbage.
Milestone 29's checker now proves **two** pictures over one boot in order (composed screen, then rung
one's pattern), both must be seen, and the composed check has its own negative control because rung two's
failure modes are not rung one's: it must reject a z-order inversion and a missing window, pictures made
entirely of correct pixels in almost the right places.

**The open fork, and it is the most useful finding of the milestone: this kernel has no wait-any.** A
process has exactly one blocking wait point (a thread parks in one `RECV`; there is no non-blocking
receive, and two threads cannot share an address space because `Tcb::CONFIGURE` consumes the aspace
capability and the space dies with the thread). A compositor has three classes of sender (clients, an
input source, a screen reader), and distinguishing classes of sender is what endpoints are for, so one
endpoint per class needs one wait point per class. **The constraint is structural: a component that must
distinguish more than one class of sender must be more than one process, or carry authority somewhere
other than its messages.** This rung took the second road and it turned out stronger than the first
would have been. But if the primitive existed, a compositor could hold one endpoint per client and get
unforgeable identity for free (letting a bad damage rectangle be *refused to its author* rather than
clipped), a screenshot could be a served consistent snapshot rather than a live read-only mapping that
can tear, and input delivery would stop being a blocking `CALL` into a client. Both candidate forms are
real work with real consequences (a shared address space raises lifetime and revocation questions; a
wait-any widens §4), so **this is Chris's call, not a thing to build quietly.** notes/compositor.md
carries the full argument.

**Honest limits, recorded because a demonstrator's caveats are part of the deliverable.** The scene is a
compile-time constant: three windows, fixed sizes, positions and stacking order, no surface negotiation,
no move, resize, raise or close. That is what makes the composed screen a value a test can predict, and
it is also the thing rung three would have to change. No alpha, no scaling. One damage rectangle per
frame as a bounding box rather than a region list. Software composition only, which at 128x64 is nothing
and at 4K would be the whole cost (rung four, milestone 34, and deliberately not started). A screenshot
can tear. And **no defence against denial of service**: a client can spam the doorbell or refuse to
answer an input `CALL` and slow or stall the compositor's single thread. Confidentiality and integrity
are what this rung proves; availability wants the missing primitive and a policy, and Wayland does not
solve it either.

## 34. RedoxFS is the primary filesystem, on three conditions

**Decided 2026-07-29 (Chris), with the conditions attached deliberately so the label and its caveats
land together.** RedoxFS is the primary on-disk filesystem. It is not yet the *root* filesystem, and
§34.3 below is why that is a separate piece of work rather than a relabelling.

### Why this commitment is cheaper here than the words suggest

In a monolithic kernel, choosing a primary filesystem means linking tens of thousands of lines of
someone else's code into the TCB, where a bug in it is a kernel bug. Here the FS server is a confined
userspace component holding a capability to a block device, so a RedoxFS defect is a **data-integrity
bug, not a system compromise**: the kernel does not trust it and cannot be broken by it. That is what
the structure was for, and it means this decision is revisable at the cost of one component, which
milestone 23 exists to demonstrate. Recording it as a decision is still right, because a default
nobody wrote down is a decision nobody can revisit.

### What earns it the role

- **It is already somebody's root filesystem.** Redox OS runs on it. That is the exact use being asked
  of it, exercised by a real system rather than inferred from a design document, and it is the single
  strongest argument in its favour.
- **Copy-on-write with transactions**, so crash consistency is designed in rather than bolted on. That
  is the one property a primary filesystem must have and the most expensive thing to write oneself.
- **Rust, and no_std on both bare targets**, proven by us. It does not drag a libc into the FS server.
  (§31 makes a C component possible now, but possible is not free.)
- **Maintained upstream**, pinned at 0.9.1 with a patch discipline (`patches/`, two `Vec` imports).
- It is the reuse thesis made concrete: a real filesystem we did not write, running confined.

### The three conditions

1. **Crash consistency must be tested, not asserted (milestone 37). MET, 2026-07-30**; the
   measurement and the exact claim it earns are in the amendment below. It is RedoxFS's central
   selling point and we had never injected a torn write or a power cut. The claim rested on the
   upstream design description. For a project whose rule is measure rather than argue, that gap was
   worse than the missing verbs were, and it is the first thing a skeptic should ask about.
2. **Throughput must be measured (milestone 38).** `fs_read` reports the whole-path cost of a real
   read (~204 us under HVF, device-dominated, with `relay_rtt` putting the isolation tax three orders
   of magnitude below it), and it is deliberately ungated because the path is interrupt-driven. What
   does not exist is any MB/s figure, or any comparison against ext4 or APFS. The phrase "primary
   filesystem" invites a comparison we currently cannot make.
3. **The write path must be honestly complete**, which is `CREATE` and `TRUNCATE` (milestone 31 phase
   two, in flight). §27 records why `TRUNCATE` is not merely a feature: a write that half-works reads
   as a write that failed, and that sharp edge cost a day and produced three wrong root causes.

One encouraging measurement already in hand: the real engine under the FS server's own allocator, at
the 8 MiB cap, held a high-water of **352 KiB across thirty mount-and-write cycles**. So the budget is
generous headroom rather than a requirement, which was the main worry about it on small hardware.

### What would reverse this

If RedoxFS turned out to need `std`, or to need an allocation guarantee the budget model cannot give,
or if its **repair and recovery tooling is absent**. The first two have been probed and came out fine.
The third is unchecked, and for a primary filesystem "what do you do with a corrupted one" is a fair
question that deserves an answer before the label hardens.

### Alternatives considered

`crickerfs` is not among them, because it is not a competitor: a boot archive and a read-write
filesystem are different jobs, and the initrd wants exactly what crickerfs is. It stays.

- **Write our own.** Rejected on the same grounds as milestone 32 originally: the thesis is the kernel
  confining the filesystem, not the filesystem. A crash-consistent CoW filesystem is a large, subtle
  project that proves nothing the thesis needs.
- **ext2.** Simple, well documented, Rust implementations exist, and it buys real interop (mount the
  image on Linux). Rejected for a *root*: no journaling, so power loss means `fsck` and possible loss,
  which is a step down from CoW. ext4 has no serious no_std Rust write implementation, and writing one
  correctly is its own multi-month project.
- **FAT32 / exFAT.** `fatfs` is mature and no_std. Rejected for a root on semantics: no crash
  consistency at all, no permissions, no symlinks. It is the right answer for a future *boot* partition
  where interop is the point, and wrong for anything that must survive a power cut.
- **littlefs.** Genuinely power-fail-resilient, and wrong on two axes: it targets raw NAND/NOR with
  wear levelling rather than a block device, at microcontroller scale, and it is C, so it would put a
  foreign component in the storage path for no thesis gain.
- **btrfs / ZFS / F2FS.** No no_std Rust implementation, and a size that would dominate the project.
- **Build on a proven transactional store** (SQLite being the most battle-tested crash-consistency
  implementation in existence, needing only a VFS shim, which is precisely the seam §31 built).
  Interesting and not recommended: file-data performance would be poor and the novelty would need
  defending for no thesis benefit. Recorded because the crash-consistency argument for it is real.

### The alternative that could supersede this, and is not a filesystem choice

**A read-only measured root plus a writable layer.** §22 already gives us measured boot, so hashing a
read-only root image would extend integrity verification from init to the entire system, with writes
landing in a smaller, less critical layer (RedoxFS or anything else). That is a *stronger security
story* than a writable RedoxFS root, it sidesteps the repair question above by making the root
reproducible rather than repairable, and it is the shape Android and ChromeOS chose (dm-verity plus an
overlay). It is recorded here as the thing most likely to make this section a footnote, and it competes
on architecture rather than on engine quality, which is why choosing RedoxFS now costs little.

Note that switching engines would not address condition 1 at all: **no candidate's crash consistency
is tested here.** That is a gap in our harness, not in RedoxFS, and it is why the conditions matter
more than the choice. *That sentence was true when it was written and is now the thing milestone 37
fixed; the harness exists, and it would measure any engine put behind the same trait.*

### Amendment (milestone 37, 2026-07-30): condition 1 is met, and the claim is narrower than the words it replaces

**RedoxFS is crash consistent, in a sense that is now measured rather than described.** The docs may
stop saying "designed for crash consistency". They should not start saying it without the scope
below, because the scope is where the interesting part is.

**What is proven.** Take a workload of operations, each acknowledged only after the engine commits
it, and call the filesystem after the first `p` of them `S(p)`. Then:

> **For every point at which the device could stop, a fresh mount recovers exactly `S(p)` for some
> `p`.** Never a blend of two states, never a half-applied operation, never a length nobody wrote,
> never a mount that fails.

That is **prefix consistency**, and it is deliberately stated as a stronger property than the one the
milestone asked for. "Every acknowledged write is either wholly present or wholly absent" falls out
of it, and so does the thing that phrasing leaves open: a state where a later operation survived and
an earlier one did not. Two further assertions make it a measurement rather than a shape. `p` must be
**non-decreasing** as the cut point advances, so a later crash can never lose more than an earlier
one; and at the last cut point `p` must be the whole workload, so a filesystem that recovered the
initial state every time (perfectly prefix-consistent, perfectly useless) fails.

**The numbers, host side, exhaustive** (`fs-server/tests/crash_consistency.rs`, 0.6 s):

| injection | fault points | result |
|---|---|---|
| power cut, every write | 93 | all prefix-consistent, `p` monotonic, `p` = 7 with nothing lost |
| power cut with the last write **torn**, 4 offsets | 372 | all prefix-consistent |
| a device that **lies** (drop or tear one write, keep persisting after) | 186 | 112 recovered, 1 refused at the mount, 73 refused at a read, **0 silently wrong** |

**The limit, stated as plainly as the guarantee, because it is real.** RedoxFS's `Disk` trait has no
flush and no barrier, so write *ordering* is the device's job. A device that acknowledges a write it
has not persisted and then persists later ones can leave a valid commit pointing at a block that
never landed, and no filesystem promises otherwise. What RedoxFS does promise, and what the third row
measures, is that this is **never silent**: every `BlockPtr` carries a seahash of the block it names,
checked on every read, so a lost or torn block is an error rather than a wrong answer. Our block
server issues no `VIRTIO_BLK_T_FLUSH`, so on real hardware with a volatile write cache the durability
of the *last* acknowledged write is the device's word rather than ours; that is a gap in our driver,
not in the engine, and it is recorded here rather than in a footnote.

**The controls, which are the reason the rest counts.** Three, of increasing directness. The
lying-device sweep needs no tampering at all and still produces 74 images the filesystem refuses, so
the injector is demonstrably destructive. Removing the header ring's older generations leaves **92 of
93 fault points unmountable**, which isolates the fallback as the mechanism rather than a guess. And
a commit torn at 2048 bytes fails `Header::valid()` outright while the previous generation's slot
stays valid and stays older, which is the whole recovery argument in three assertions and no mount.

**A fourth control arrived unbidden and is worth more than the other three.** The first version of
the harness read *any* failed lookup as "the name is absent", so a dropped write to a directory's
tree block produced what looked like a filesystem that never existed, empty root and all. Nine fault
points reported a filesystem bug that was a test bug: `ENOENT` is the only error that means absence,
and the engine refusing to guess at a block whose checksum did not match is the property working.
An instrument that can produce a false positive and did is an instrument that is connected to
something.

**Two mechanisms we had named wrong, corrected here.** `cleanup: true` is **not** the header-ring
replay; `FileSystem::open` scans all 256 slots and keeps the newest valid one unconditionally, and
`cleanup` only releases unused nodes and commits on top. And the recovery is not "the newest
consistent generation" in any sense the engine computes: it is the newest generation whose *header*
still hashes, which is enough only because a commit's blocks are all written before it.

**On device, both ISAs, on a disk of its own.** The FS server is killed one block write into its
second transaction, with that block torn in half by a real virtio write, announcing the cut on its
readiness endpoint so the kill is provably the injector's. A **different FS-server process** then
mounts the same disk through the same block server, which is endpoint-only naming doing its job: the
block server never learns its client died and was replaced, because it never knew who its client was.
Its readiness sentinel is the consistency result, since `Server::open` refuses an image it cannot
make sense of. Both legs recover the acknowledged payload, whole, and the host tool re-reads the image
afterwards with the pinned engine and agrees.

**The crash test owns its disk, and that is §27's lesson applied before the fact rather than after.**
This test deliberately leaves a filesystem half-written. On the shared fixture, every other FS test's
result would have depended on whether this one ran first, which is precisely the order-coupled gate
that manufactured three incompatible root causes from three honest investigations. The fixture is
regenerated every run, `CRICKER_KEEP_REDOXFS` deliberately does not apply to it, and on the host every
fault point starts from a byte-identical clone of one image built in-process.

**What is still a design claim.** Ordering and durability at the device, above. Repair and recovery
tooling for an image the checksums *do* reject, which "What would reverse this" already names as
unchecked, and which this milestone sharpens rather than answers: we can now produce such an image
deliberately, and there is still nothing to hand a user who has one. Condition 2 (throughput,
milestone 38) is untouched.

## 35. What a scanner is for here, and how its findings get dispositioned

**Decided 2026-07-30 (milestone 45), the first time code scanning actually ran.** CodeQL found nine
things, and **all nine are fixed**: seven CI jobs holding a `GITHUB_TOKEN` with permissions they never
used, and two `rust/access-invalid-pointer` in `crates/intrusive` that moving the queue API to
`NonNull` cleared outright: `/language:rust` reports **2 results on `refs/heads/main`** and **0 on
`refs/pull/5/head`**, holding across four commits on each side, the oldest zero being the NonNull
commit itself.

**The evidence path is worth recording, because the first one was invalid.** I originally checked
`?ref=refs/heads/<branch>` and read the zero it returned as "cleared". CodeQL does not store a PR's
analyses under the branch ref: that ref has **zero analyses**, so the query would have returned zero
whatever the code did. A right answer from a query that could not have produced a wrong one is not
evidence, and this section is the wrong place to be sloppy about that. The controlled comparison
above is the real result.

The policy below is recorded anyway, and deliberately, because the *next* finding will not be so tidy
and the question milestone 44 left open is still open: what happens to a finding we do not intend to
change code for.

**A prediction worth recording because it was wrong.** I twice said the `NonNull` change would
probably improve the code *without* satisfying CodeQL, reasoning that the rule is about pointer
validity in general rather than nullness specifically, and that the honest outcome would be a written
dismissal. It cleared both alerts; the rule was more precise than I credited. The lesson is not "trust
the scanner", it is that a hedge stated confidently is still a guess, and this one cost nothing only
because the fix was worth making on its own merits.

### The rule

**Every alert gets a disposition, and a dismissal is a written argument, not a click.** An alert list
nobody triages decays into wallpaper, and then the scanner is worse than nothing: it manufactures the
appearance of review. Three dispositions, and only three:

1. **Fixed.** The code changes. Default for anything where the fix is real.
2. **Dismissed with a reason**, recorded where the *code* is, not only in GitHub's UI. GitHub's
   dismissal comment is fine as the audit trail; it is not fine as the only copy, because it is
   invisible to anyone reading the source and it does not survive a change of tool.
3. **Deferred to a milestone.** For a finding worth fixing that is bigger than the alert.

### The distinction that made this concrete

The two `intrusive` alerts look like one finding and are two, and separating them is what made the
milestone tractable:

- **Nullness was structurally fixable, and the type was failing to say so.** Every pointer entering
  the queues comes from `tcb_ptr`, which derives it from a `&mut Thread` the thread table hands out,
  or from a `&mut Thread` directly. Non-nullness is a fact of construction, not a promise the caller
  keeps. So the API moved to `NonNull<T>`, and **every conversion at every call site is infallible**;
  nothing was relocated into an `unwrap`, which is the move that would have made this cosmetic.
- **Validity and aliasing are not structurally fixable**, and that is the design rather than a gap. An
  intrusive queue borrows nodes it does not own with no lifetime the borrow checker can see. That is
  the entire reason it exists (no allocation, no lookup, a pop hands back the object), and the price
  is stated in the crate docs as a three-rule caller contract. **No type available to us can carry
  rule 2**, "a node outlives its time on the queue", for a structure whose whole purpose is that the
  queue does not own its nodes.

### What actually upholds the half no tool covers

This is no longer a dismissal justification, since nothing was dismissed. It stands as the queue's
standing caveat, which is the more useful role: the kernel's own state
machine. A thread is on exactly one run queue or inbox, or blocked on one endpoint queue, or running,
and never two at once, because there is only one link inside it. Only `Finished` threads are ever
freed, and a `Finished` thread is on no queue. All access is serialized (a run queue is single-core
with interrupts masked; an inbox is behind its mutex; endpoint queues are under `SCHED`). Those are
the three rules, and they are enforced by the scheduler's structure and the lock ranking of §9, not by
the type system.

### The honest limit, which is the point of writing this down

**Zero alerts is not a proof of safety.** The queue is safe because
the scheduler uses it correctly, and nothing in `crates/intrusive` can check that. A future caller
that violates rule 2 gets a use-after-free that neither CodeQL nor Kani would catch: Kani proves the
queue's *logic* over a symbolic operation sequence with nodes it holds valid by construction, so it
answers "is the FIFO correct" and never "did a caller free a queued node". That gap between the two
tools is real and worth naming rather than papering over with a green checkmark on both.

### Rejected

- **Suppressing the rule crate-wide.** It would also silence a genuine future null or dangling
  dereference in the same file, which is the one place we most want to hear about one.
- **Restructuring to satisfy the tool.** An owning queue would reintroduce allocation on the IPC path,
  which is what milestone 14 removed and what `VecDeque` cost us. Chasing a scanner into a worse
  design is the failure mode this section exists to prevent.

## 36. The repository is part of the TCB (milestones 44 and 42)

**Decided 2026-07-30.** §14 promises a verified core that confines code we did not write. That
promise is only as strong as our ability to say *which* code we are running and *how it got in*, and
both of those are properties of a GitHub repository rather than of the kernel. So the repository gets
the same treatment as a kernel boundary: state what is claimed, make the claim checkable, and write
down what is not claimed.

This section covers milestone 44 (policy, private reporting, code scanning, pull requests) and
milestone 42's non-fuzzing half (advisories, licences, vendored integrity), because they are one
question wearing two milestone numbers.

### The scope line in SECURITY.md, which is the only interesting part of it

A security policy for a demonstrator is mostly a scope argument. `SECURITY.md` draws it at
**confinement**: capability forgery or widening, MMU escape, DMA escape, IPC confusion, a syscall
argument that panics or corrupts the kernel, TOCTOU across the shared pages every service contract
uses, and the foreign-language and vendored seams (§27, §31). Out of scope: that a demonstrator under
QEMU is not a production system, that a hardening feature on design/roadmap.md is missing, and
anything that requires already being init, which §14 already names as the privileged unverified
component.

**The distinction that carries the weight** is "a missing feature is a roadmap item; a defence that
is *claimed* and does not work is a vulnerability." That is the honest version, and it is also the
demanding one: every claim this project publishes becomes something a reporter may hold us to.

### Pull requests into `main`: a ruleset, because discipline is not a property

Today "merge when green" is a decision a human makes each time. The evidence that this is not enough
arrived on its own: `gh pr merge --auto` was used on 2026-07-30 and **silently did nothing**, because
GitHub only queues auto-merge when something is actually blocking the merge, and with no required
status checks nothing was. The merge went through immediately, unchecked, and looked identical to
one that had waited. A red `main` had already gone unnoticed for two days in exactly that way.

So: required status checks on `main`, applying to the repository owner, with linear history. The
"applies to the owner" part is the whole point on a solo repository; an exemption for the one person
who pushes is an exemption for every push. The gate is not there because the maintainer is
untrustworthy, it is there because `--auto` failing open is invisible and human vigilance is not
version-controlled. The exact ruleset is in notes/repo-hardening.md, because it is applied through a
web UI and nothing in the tree can enforce it.

**The cost is real and accepted:** every change becomes a branch and a PR, and a one-line typo fix
waits for a Kani job. The mitigation is that the checks are already fast enough to be tolerable and
the alternative has already failed twice this week.

### Code scanning: stay on default setup, and record the coverage number as the caveat

Default setup is running (§35) and finds all five cargo workspaces by itself, so the obvious argument
for an advanced (committed-workflow) setup, "it would see more of the tree", is **false** and was
checked rather than assumed: the extractor reports `176 out of 176 Rust files`.

What the same log shows is the caveat that matters more. **60 of those 176 files were extracted with
errors** and 116 without; the extractor ran with `cargo_target: None` (the host) and `cargo_features:
[]`. The kernel is `no_std` on two bare-metal targets and does not build for the host at all, so
CodeQL is analysing it in a configuration that does not exist, with macro expansion failing across
`assert_eq!`, `vec!` and friends. **"Zero alerts" therefore means less than it looks**, which is the
same honesty §35 applied to the gap between Kani and CodeQL, aimed at CodeQL itself.

Advanced setup could set `cargo_target` and exclude `vendor/**`. It is still not worth it yet:
the Rust extractor is in preview and moving (CodeQL 2.26.2, rust-queries 0.1.39), default setup
tracks its improvements for free, and a pinned workflow would freeze today's limitations while adding
a maintained file and a matrix over two ISAs. **Revisit on a stated trigger**, not on a feeling: an
alert lands in `vendor/**` (upstream's to fix, per SECURITY.md, and noise here), or the
extracted-with-errors fraction stops falling, or a query we want is unavailable by default.

### Supply chain: configure it deliberately, and expect the first run to find something

`deny.toml` is written rather than defaulted, with a reason next to every knob, because a default
config is wrong in both directions at once. It narrows the graph to targets we actually build (the
default drags `windows-sys`, `wasi` and RedoxFS's redox-native half into the verdict for code nothing
here compiles, and noise is how an alert list becomes wallpaper), and it tightens what remains:
`unmaintained = "all"`, `yanked = "deny"`, an allow-list of licences rather than a deny-list, and
`unknown-git`/`unknown-registry` denied so a dependency repointed at somebody's fork is loud.

First run: no advisories, no yanked crates, no unknown sources, everything permissive. Three real
findings: one duplicate (`getrandom` 0.2 and 0.4, both under redoxfs, host-side only, skipped with a
reason), three licences beyond MIT/Apache-2.0 that are genuinely needed (BSD-3-Clause, 0BSD,
Apache-2.0 WITH LLVM-exception), and two crates that could not be distinguished from a `version = "*"`
dependency until they declared `publish = false`.

**Vendored integrity is the half that changes a claim rather than adding a check.** §34 and milestone
32 say we run *upstream RedoxFS 0.9.1*, and vendor/README.md listed the divergence "exhaustively".
Nothing verified that sentence and it was already wrong: the vendored `Cargo.lock` had been deleted
and regenerated, re-resolving 25 dependencies. `script/vendor-verify` now hashes the published
tarball, applies a committed divergence patch with zero fuzz, and requires byte identity with the
tracked tree. Applying the patch *and then* comparing is what makes it airtight, since a hunk landing
at the wrong offset still exits 0.

**What none of this covers:** whether upstream 0.9.1 was trustworthy in the first place. That is a
trust decision made by reading the code (notes/redoxfs-audit.md), and a hash cannot make it for us.

### Deliberately not here

**Fuzzing** (milestone 42's third leg). Which parsers get harnesses, what a corpus is committed
against, and how a fuzzer's findings get triaged against §35's three dispositions is its own design
pass, and bolting a `cargo-fuzz` job onto this milestone would have produced a job nobody reads.

### Rejected

- **A `SECURITY.md` that promises a response time.** A one-person project that publishes an SLA it
  will miss has published a falsehood, not a policy. "About a week to acknowledge, no remediation
  timeline, no bounty" is worth more than a number nobody will hit.
- **Branch protection without required checks.** It would block direct pushes while still letting a
  PR merge red, which is the failure that already happened wearing a different hat.
- **Loosening cargo-deny until it passes.** The `getrandom` duplicate is skipped with a written
  reason and an expiry condition (the next redoxfs pin); the alternative, `multiple-versions = "warn"`,
  would have silenced every future duplicate to avoid explaining one.

## 37. Text is a value three witnesses compute, not a screenshot (milestone 29's remaining increment)

**Built 2026-07-30**, both ISAs, in QEMU. Font rendering, a VT state engine, a display terminal, and a
real keyboard: the piece that makes the display ladder's framebuffer readable. Concept note
notes/glyphs.md. (Section number chosen against `origin/main` at `92a0491`, where §35 is the scanner
policy. If a concurrent lane has claimed 36 by merge time, renumber; nothing here depends on it.)

**The decision, and it is the one to read: rendering is a pure function, so the expected picture is a
value.** A bitmap font and a sans-IO grid engine mean `pixel(x, y)` is computable by anyone holding
the script, and three parties do compute it, independently: the terminal runs the engine to draw, the
kernel runs it to predict the framebuffer pixel for pixel through the direct map, and `cargo xtask`
runs it on the host to grade what QEMU is actually scanning out. Text is where "it looked right" is
most tempting and least sufficient, and this is what replaces it. The host checker has its own
negative control and the assertion that carries the weight is **one letter changed** (`o` for a zero,
the closest pair of glyphs in the font): a checker that could not tell those apart would report
"readable text reached the scanout" for a terminal that drew the wrong text. It must also reject the
typed input missing and every rendition ignored, both of which are screens made of correct glyphs.

**The font is public domain, and the licence is the reason.** `font8x8` (Daniel Hepper, from Marcel
Sondaar's, from IBM's public-domain VGA fonts). A bitmap font is **compiled into the image**, so its
licence travels with the artefact rather than with a build-time tool; Terminus (OFL-1.1) and Spleen
(BSD-2-Clause) are fine fonts that would each have attached an attribution obligation to every binary
that draws text. Bitmap rather than scalable because a rasteriser wants an allocator, floating point,
and a font file, and because a pure function is what makes the paragraph above possible at all.

**Neither display contract needed a line changed, and that is now a spawn literal rather than a
claim.** The same `vterm` binary runs in two wirings: holding rung one's display endpoint and the
scanout with **exactly `painter`'s authority**, and holding rung two's doorbell and one window with
**exactly `window`'s authority**. `gpud` cannot tell it from the client that painted a test pattern;
`compd` cannot tell it from the client that painted a coordinate function. Both contracts carry
pixels, and a terminal draws pixels. §29's note said a terminal would arrive as another client of that
contract; it did, twice.

**A found deadlock, and the better design it forced.** A terminal that answers a keystroke by ringing
the compositor's doorbell deadlocks as soon as two keystrokes arrive in one drain: the compositor is
blocked in its `CALL` to the terminal while the terminal is blocked in its `CALL` to the compositor.
That is §33's recorded cost of input-as-a-blocking-`CALL`, arriving in practice. It does not need to
ring: the compositor rescans every client's control page on every `COMMIT` from anyone, and the input
source rings `COMMIT` itself, so **the frame that delivers a keystroke is the frame that shows it**.
Application output still rings, because nobody else will, and that is safe because the caller blocked
in `CALL` there is the application. The design the deadlock ruled out was also the worse one.

**Input: the authority to type is a mapping, and the authority to route is a capability.** The
keyboard driver's power to inject a keystroke is the **input ring's mapping**, which no client has;
it is not the doorbell, which every client holds and which carries nothing. The driver holds no
client's endpoint and cannot name a client, so it cannot influence who receives what it types. That
is the compositor's, expressed as which of the per-client input endpoints *it* holds it uses, and a
client granted none has an empty cspace slot. So focus never becomes ambient: there is no verb that
grabs the keyboard, no message that names a recipient, and no page a client can write that would
inject input. The forgeable parts do not exist rather than being guarded (§33's idea, from the
producing side).

**The keyboard rides PCIe by choice, unlike the GPU.** Both `virt` machines *do* offer a
`virtio-keyboard-device` on the virtio-mmio bus, so this is not §29's "there is no mmio twin". It
rides PCIe so it lands in the same IOMMU domain the GPU does, because a keyboard is the device whose
DMA one would least like unconfined: its buffers are where every keystroke lands. Its event queue is
the **device-writes-into-driver-memory** direction, which the validator already proved for virtio-net
(§23, §30), so nothing in the confinement needed widening.

**The host is an actor for the first time.** Nothing in the guest can press a key, so `cargo xtask`
sends `sendkey` on the same QEMU monitor connection the scanout check already holds open, every poll,
from the start of the run. No synchronization is needed because QEMU **drops key events until a driver
sets `DRIVER_OK`**. The keyboard test then proves the path from a physical key event to a terminal
byte; the compositor test proves the ring to a focused terminal's pixels; the seam between them is the
ring, exactly where §33 put the authority boundary. Naming the seam is better than one test that hides
it. Verified headlessly on QEMU 11.0.2, both ISAs.

**No new syscall, no new method, no widened surface (§4).** The whole increment is endpoints, shared
frames, and `Spawn` grants that already existed. One constant moved: `MAX_DEVICES` 24 → 26, because a
third `gpud` programs the same physical GPU and no transport is ever released. Recorded with the
standing suggestion the number keeps earning, the same one §33 made about `KERNEL_EP_PAGES`: the
honest fix is releasing a transport when its driver dies.

**A bug worth recording because it is a real terminal bug.** The VT parser had no string state, so an
OSC sequence (`ESC ]0;title BEL`, how every program sets a window title) printed the title onto the
grid. Found on the host, in milliseconds, by the test that now feeds a title-setting sequence on
purpose. The interoperability test found its own footing the same way: the escape sequences a display
terminal must understand are the ones `linedisc` emits (§21), so rather than assert that from a list
that could drift, the test **runs the real line discipline** and feeds its echo stream to this parser.

**Deferred, and stated rather than implied:** no scrollback (the roadmap named it; it wants a ring of
off-screen rows and a viewport, which changes the damage model), no UTF-8 (the grid holds bytes and
the font covers basic latin), no line editing in the display terminal (`termd` composes in front of it
through `OP_WRITE` with no new protocol, which the `vt` crate proves on the host by running both), no
reflow (nothing resizes), a US layout's main block only, and no mouse. notes/glyphs.md carries the
full list.

**The libghostty-vt question is left open on purpose, with the cost now measurable.** The roadmap
names it as milestone 23's strongest form and §31 built the C seam to de-risk it. Building the Rust
engine first was the right order, and it changed what the comparison is about: a VT engine fits the C
seam's shape almost perfectly (bytes in, cells out, no IO), so the port is a shim rather than a
rewrite, and **the work is the proof structure, not the rendering**. Our engine's `pixel(x, y)` is
what makes the three-witness check possible; libghostty-vt's C ABI gives cells, so the
expected-picture definition would have to be rebuilt against its layout. The recommendation in
notes/glyphs.md is to adopt it as a *second* engine behind the same seam rather than a replacement,
because a suite that grades two engines against each other is a better milestone-23 demonstration
than either alone. **Architect's call, not taken here.**

## 38. A suppression is scoped to an item and carries a reason, or it does not ship (milestone 41)

**Decided 2026-07-30**, after triaging every `allow(dead_code)` / `allow(unused)` in the tree. This
extends §35's disposition rule from scanner alerts to compiler warnings, which is where the same
failure was already happening and nobody was counting.

### The rule

**A dead-code suppression is an item attribute (`#[allow(...)]`), never an inner attribute
(`#![allow(...)]`), and it says which configuration has no caller.** `script/lint` enforces the
first half; the second half is a review expectation, and the phrasing that satisfies it is a `cfg`
predicate rather than prose. Prefer, in order:

1. **Delete the code.** Nothing calls it, git remembers it, and a reader who trusts the comments
   should not have to find out the hard way that a function is decorative.
2. **`#[cfg_attr(<the configuration with no caller>, allow(dead_code))]`.** `not(test)` when the
   tests are the callers; `feature = "bench"` when a boot mode compiles the caller out;
   `target_arch = "riscv64"` when the item is one ISA's. The attribute then makes a *checkable*
   claim: if the caller disappears from the configuration that had one, the gate says so.
3. **A bare `#[allow(dead_code)]` with a written reason**, for the cases where nothing calls it in
   any configuration and it stays on purpose: an arch-contract stub, a diagnostic that is off by
   default. Rare, and the reason is the whole justification.

### Why an inner attribute is different in kind

An item allow is a decision about one item. An inner attribute is a decision about **every item the
module will ever contain**, including the ones written after it, by someone who never saw the
comment. That is not a suppression, it is a policy, and it decays the moment the module grows.

The measured cost was not hypothetical. Six files carried module-wide `#![allow(dead_code)]` over
**5,831 lines**, including `sched.rs` (3,166) and `arch/aarch64/mmu.rs` (1,275), and `main.rs`
carried `#![cfg_attr(target_arch = "riscv64", allow(dead_code))]`, which blindfolded the **entire
kernel crate** on one of two supported architectures. `script/lint` runs clippy with `-D warnings`
and reported success across all of it. Same class as the conflict markers that survived a full gate
run and §27's four-times-corrected record: **the tooling said fine because nothing was looking.**

### What the un-blindfolding actually found, which is the part worth keeping

Mostly not dead code. That is the honest result and it is why the ratchet matters more than the
cleanup: the value was never in the deletions, it was in learning that the claims were unchecked.

- `sched.rs`: five dead items out of 3,166 lines, but one of them (`spawn_balanced`) carried a doc
  comment asserting "which is why the SMP balance test uses it", and the test had moved to plain
  `spawn` when §28 landed. A false comment in a codebase commented this heavily is the expensive
  kind of dead code.
- `mmu.rs`: two. Four more functions were suppressed unconditionally while being exercised by tests,
  so the gate could not have noticed a test dropping them.
- **A parity gap on the second ISA**, found only because the crate-wide riscv allow came off:
  `user_can_read`/`user_can_write` had no caller anywhere on riscv64, because the confused-deputy
  test is `cfg(target_arch = "aarch64")`. The check that stands between U-mode and the kernel was
  proved on one ISA, and on the ISA where it matters *less*: RISC-V has one root register, so the
  same tables translate user and kernel addresses and the `U` bit is the only line of defence.
- **A vestigial input path**: `console::rx_read` and `Ns16550::read_byte` were dead in every
  configuration including `--features shell`, because the byte is read by the userspace input driver
  through its device capability. Milestone 20's kernel-side reader had outlived its own design.

### Rejected

- **Deleting `user_can_read`/`user_can_write` as unused.** They are the worked example
  notes/capabilities.md leans on, and a test can prove them, which is strictly better than either
  deleting them or allowing them. The rule's first preference is deletion; a *test* beats it.
- **Keeping a narrowed crate-wide allow for riscv64** (`all(target_arch = "riscv64",
  not(feature = "shell"))`). It would have been true, and it would still have covered every future
  item in the crate. The whole point is that scope, not accuracy, is what makes an inner attribute
  the wrong tool.

## 39. A component is named for what it is, and nothing is named for a daemon

**Decided 2026-07-30 (Chris).** Userspace components take names that describe what they do.
Specifically: **no `-d` suffix**, and no term of art that requires archaeology to parse.

### The argument, which is Chris's

Milestone 39's naming section had already argued that "daemon" is the wrong word here, on technical
rather than aesthetic grounds: a Unix daemon is defined by what it detaches from (no controlling
terminal, inherited ambient authority, a pid file, started by a privileged init), and every one of
those is something this OS deliberately does not have. `netd` holds five explicit capabilities, cannot
name its own callers, is supervised, and can be reaped by something that lacks the authority to build
it. It is about as far from a daemon as a long-running process gets.

I then argued to keep the `-d` names anyway, weighing churn against benefit. Chris's response is the
better argument and settles it: **if we are not going to use "daemon", we should not name things `d`
for daemon.** A name is a claim, made before a reader sees a line of code, and this one is false. It
is the same defect as a stale comment, which this project spends real effort correcting; a name is
just a comment that every reader is guaranteed to read.

### The second half: jargon is the same failure

`termd` was to become `linedisc`, the correct Unix term of art. Chris did not recognise the phrase and
asked what a line discipline is — **and he built this system.** That is decisive evidence about the
name, not about him: `linedisc` imports vocabulary from exactly the system whose model we rejected,
which is the `-d` failure wearing a different hat. It became `lineedit`, which someone who has never
read a tty manual understands immediately and which is accurate about the visible behaviour.

The crate `crates/linedisc` renames too, rather than being kept as the implementer's term of art. If
the phrase is jargon to the system's author, it is jargon in the crate as well.

### The rule going forward

- Name a component for **what it is** (`netstack`, `compositor`, `display`, `lineedit`), not for what
  Unix would have called it.
- **Never `-d`.** Not `netd`, not a future `logd` or `authd`.
- Prefer a word a reader can parse without prior Unix exposure. `blk`, `spawner`, `console`, `input`,
  `shell`, `painter`, `window` were already right, and were always the majority of the tree; the four
  `-d` names were the outliers, not the convention.
- Milestone 39's vocabulary is now the tree's: a **component** is the shippable unit, a **service** is
  what it offers, a **contract** is the wire protocol. "Server" stays a fine role word inside a
  component. "Daemon" appears nowhere.

The rename itself is milestone 46, deliberately its own mechanical commit, which also carries the
naming conventions and the checks for the ones a machine can check. That pairing is on purpose: this
rule and the three inconsistencies found alongside it (crate-name word separation, four spellings of
"the wire contract", and a `feature/`-versus-`feat/` branch-prefix duplicate) are each the kind that
decays without enforcement, and the checker is what makes a convention survive the first inconvenient
moment. The part that cannot be checked, "name it for what it is", stays prose because it needs
judgement.

## Reading

- **The seL4 manual**, and Klein et al., *seL4: Formal Verification of an OS Kernel* (SOSP'09)
- **Liedtke**, *On µ-Kernel Construction* (SOSP'95) — why Mach was slow and why that was not a law
- **xv6 book** (MIT, ~100pp) for how a real Unix-shaped kernel is structured. Read it as the
  road not taken (§10), not as a template.
- `rust-raspberrypi-OS-tutorials` for the aarch64-specific mechanics
- OSDev wiki as a reference, not a tutorial
- *Operating Systems: Three Easy Pieces* for the theory
