# 10. Process model: capability-based, microkernel. Untyped memory deferred.

Decided 2026-07-14, at the §8 decision point, before any of milestone 7 was written.

**A process names a resource by holding an unforgeable token it was handed. There is no
ambient authority, and there is no global namespace.** Drivers and services are userspace
processes. IPC is the primary syscall.

## What a capability is, so the word means something

A capability is **a file descriptor that can point at anything, not just files**. Same
mechanism, generalized: a per-process table living in *kernel* memory, indexed by a small
integer. The unforgeability is not cryptographic and there is no magic. You cannot
fabricate slot 7 for the same reason you cannot fabricate `fd 7`: the table is not yours to
write.

The difference from Unix is not the fd. **Unix already has capabilities.** The difference is
that Unix *also* has a back door, `open(path)` checked against your uid, which lets a process
**mint** authority out of who it is. We are not building the back door.

## Rejected: Unix-like (fork/exec, paths, uids)

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

## And what the Unix path actually costs us

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

## Not a reason: differentiation

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

## The performance question, answered so it stops being asked

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

## The three things this actually buys, none of which is speed

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

## An interrupt becomes a message

Worth stating early, because it is where §5's exception model meets this one. A driver holds an
**IRQ capability** bound to a notification, and blocks. The kernel's handler does one thing:
signal it. The driver has no interrupt handler. It has a loop:

```rust
loop {
    wait(irq_notification);          // sleeps until the device interrupts
    let packet = read_device_fifo();
    send(net_stack_endpoint, packet);
    ack(irq_cap);
}
```

Ordinary code, in a process, at EL0. If it deadlocks, it deadlocks by itself.

## What we are NOT doing yet: untyped memory

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

## The rules this adds

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
