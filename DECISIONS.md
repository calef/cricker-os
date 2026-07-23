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

## Reading

- **The seL4 manual**, and Klein et al., *seL4: Formal Verification of an OS Kernel* (SOSP'09)
- **Liedtke**, *On µ-Kernel Construction* (SOSP'95) — why Mach was slow and why that was not a law
- **xv6 book** (MIT, ~100pp) for how a real Unix-shaped kernel is structured. Read it as the
  road not taken (§10), not as a template.
- `rust-raspberrypi-OS-tutorials` for the aarch64-specific mechanics
- OSDev wiki as a reference, not a tutorial
- *Operating Systems: Three Easy Pieces* for the theory
