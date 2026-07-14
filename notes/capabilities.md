# Capabilities, and why the kernel has no `open()`

The milestone 7 decision. [DECISIONS.md §10](../DECISIONS.md) is the record of *what* we chose
and what we rejected. This is the note on *what the words mean*.

## The one sentence

> **A capability is a file descriptor that can point at anything, not just files.**

That is not an analogy that mostly works. It is the mechanism, generalized.

## Which means Unix already has capabilities

Look at what an `fd` actually is:

- **Unforgeable.** You cannot invent `fd 7` and get a file. There is nothing to guess.
- **Per-process.** Your `fd 3` and my `fd 3` are different files.
- **A token you hold**, not a name you can say.
- **Delegable.** You can pass one to another process over a Unix socket.

That is a capability. Every property. Unix had them all along.

**The difference is that Unix also has a back door.**

```c
int fd = open("/etc/passwd", O_RDONLY);
//            ^^^^^^^^^^^^^
//            A NAME. In a namespace every process can see.
```

You did not *hold* anything. You **said a name**, and the kernel checked whether *you* (your
uid) were allowed. Your authority came from **who you are**, not from something you were
handed. That is **ambient authority**: authority that surrounds you, that you carry
everywhere, that you never had to be given.

And it lets a process **mint** a capability out of thin air. Say the name, get the fd.

**We are not building the back door.** That is the whole decision.

## What it is in memory, because there is no magic here

No cryptography. No unguessable number. The unforgeability is boring, and that is the point.

Each process has a **capability space** (a CSpace): a table of slots living **in kernel
memory**. Userspace never sees a single byte of it. Userspace sees **an integer**.

```
  What the process says            What the kernel has
  ---------------------            -----------------------------------------------
      cap 0            ────────►    { Endpoint,  obj: 0x4012_3000,  rights: SEND       }
      cap 1            ────────►    { Frame,     obj: 0x4008_1000,  rights: READ|WRITE }
      cap 2            ────────►    { Tcb,       obj: 0x4009_2000,  rights: ALL        }
      cap 3            ────────►    { empty }
```

You call `send(3, msg)`. The kernel looks in **your** CSpace at slot 3, finds it empty, and
returns an error.

**You cannot forge a capability for exactly the same reason you cannot forge a file
descriptor: the table is not yours to write.** That is it. That is the entire security
mechanism, and it is one array bounds-check away from what we already know how to do.

> "But could I not just write to my own CSpace?"
>
> Only if you hold a capability to your CNode. Which someone would have had to hand you.

The recursion bottoms out **at boot**, where the kernel hands the first process a capability
to everything, and that process gives away slices and never gets them back.

## The argument, in one story: the confused deputy

This is why anyone cares. It is a 1988 paper by Norm Hardy and it has never stopped being
relevant.

A **compiler service** runs on a shared machine. It has permission to write its own billing log
at `/var/log/billing`, because it bills you for compiles. You invoke it:

```
compile foo.c -o /var/log/billing
```

**And it does it.** It destroys the billing log.

Not because of a bug. Because it *had* permission and it acted on *your* request using **its
own** authority. You just deleted a file you have no right to touch, and every line of code
involved behaved exactly as designed.

The compiler is the **deputy**, and it was **confused about whose authority it was acting
under**. Ambient authority is what made the confusion possible: the compiler's right to write
that file came from *who it was*, and nothing in `-o /var/log/billing` told it that this
particular write was on *your* behalf.

**With capabilities the story cannot be told.** To have the compiler write your output file,
you must **hand it a capability** to that file. And you cannot hand it one you do not have.

The bug is not defended against. **It is not constructible.**

That is the same move as [`TlbFlush`'s `Drop`](page-tables.md) and the [lock
ranking](deadlock.md): make the bad state *unrepresentable* rather than checking for it at
runtime and hoping.

## Why we cannot just add this to Unix later

We can. It has been done. **It does not work**, and the reason is the whole asymmetry.

**FreeBSD's Capsicum** (2010) added `cap_enter()`: a process calls it and drops into capability
mode with no ambient authority. No `open()` by path. Only what you already hold.

It works. It is in the base system. It has been there for fifteen years.

**Almost nothing uses it.**

Because every program on the machine assumes it may call `open("/etc/resolv.conf")`. Once that
assumption is baked into a million lines of userspace, you cannot take it back without
rewriting all of it. OpenBSD's `pledge`/`unveil`, Linux's `seccomp` and Landlock: same story,
all revoke-after-the-fact, all partial, **none achieving no-ambient-authority**, because by the
time they arrived the world was built on it.

> **Ambient authority, once granted, cannot be withdrawn.**

And in the other direction it is easy. Fuchsia's `fdio` is a **userspace library** that gives you
`open`, `read`, and `write` on top of capability handles. The convenience comes back whenever we
want it. It just is not the primitive.

| Direction | Cost |
|---|---|
| capabilities to a Unix-shaped API | **additive.** A library. Nothing is thrown away. |
| Unix to capabilities | **a rewrite**, and it has never once succeeded. |

Which is the same table as [DECISIONS §5](../DECISIONS.md)'s threads-versus-async, and it decides
this the same way. **When one direction is cheap and the other is a rewrite, take the one that
keeps the option open.**

## The kernel has almost no syscalls

```
Send   Recv   Call   Reply   NBSend   NBRecv   Signal   Wait   Yield
```

Roughly that. **There is no `open`. No `read`. No `mmap`. No `fork`.**

Everything else is **a message to a userspace process**. To open a file you send a message to a
filesystem server, and it sends you back a capability. To map memory you invoke a capability on a
page table. To start a thread you invoke a capability on a TCB.

> **The kernel is not a service provider. It is a message router.**

## An interrupt is a message

Where [milestone 2's exception model](exceptions.md) meets this one, and it is the prettiest
consequence.

A driver holds an **IRQ capability**, binds it to a notification, and blocks. The kernel's handler
does exactly one thing: signal the notification. The driver thread wakes up.

**The driver has no interrupt handler.** It has a loop:

```rust
loop {
    wait(irq_notification);          // sleeps until the device interrupts
    let packet = read_device_fifo();
    send(netstack_endpoint, packet);
    ack(irq_cap);
}
```

Compare that to `handle_irq` in `arch/aarch64/exceptions.rs`, which runs in exception context with
interrupts masked and has to be extremely careful about what it touches. This is **ordinary code,
in a process, at EL0**. If it deadlocks, it deadlocks by itself, and we restart it.

## Is it slow?

**One IPC is not slow.** seL4's fastpath is a few hundred cycles: comparable to a Linux syscall,
and *better* than one now that Spectre mitigations made Linux's syscall boundary expensive. Mach
was slow in the 90s and it poisoned the word "microkernel" for twenty years, but Liedtke showed in
1995 that this was an **implementation failure, not a law**, and it has stayed fixed.

**The cost is that you need more crossings.** A `read()` that was one syscall becomes six. And the
real bite is not the cycles, it is the **cache and TLB pollution** of landing in a different working
set. You can make the switch cost 200 cycles. You cannot make the other process's data be in L1.

The discipline that recovers most of it, which every serious microkernel arrives at independently:

> **IPC carries control. Shared memory carries data.**

Put the bytes *in* the message and you copy twice, and you are Mach, and you are slow. Put a **frame
capability** in the message and the receiver maps it. **Zero copies.**

And the honest 2026 note: `io_uring` exists because Linux's syscall boundary got expensive, and its
answer (a shared-memory ring, batched, stop crossing per operation) **is this discipline under
another name**. DPDK and SPDK moved the network and storage drivers into userspace for the same
reason. Those are microkernels. They just had to bolt the isolation on afterwards with an IOMMU,
instead of getting it free from an address space they were going to have anyway.

**For us it is zero, and we should never let it argue either way.** We run on QEMU with no workload.
We will never measure it.

## What this is not: differentiation

Floated, and **wrong**, and written down here so it does not come back.

aarch64 is not virgin ground for capability microkernels. **It is their home turf.** seL4 is
primarily an ARM story. L4 runs on every Qualcomm baseband. An L4 derivative runs the Secure Enclave
in the phone in your pocket. QNX runs most cars. Trusty is on essentially every Android phone. And
in the hobby-Rust space, **Redox is already a Rust microkernel that runs on aarch64.**

A capability microkernel on ARM is the single most ARM-shaped thing one could build.

The real reason is smaller and better: **on the Unix path you transcribe, and on this path you
derive.** xv6 has a canonical answer to every question the Unix path raises. There is no xv6 here.
Every design question is ours, and for a project whose purpose is understanding, that is not a cost.
It is the product.

---

*Add to this file as capability concepts come up: rights, delegation, revocation, the derivation
tree, untyped memory.*

---

# Milestone 7d: the syscall surface, and refusing to be a deputy

The decision (§10) is now code. Three things landed: an ABI that is one artifact, a capability
table, and a syscall dispatcher whose most important function is the one that says *no*.

## The surface is three calls

```text
  exit(code)                          authority over yourself
  yield()                             likewise
  invoke(cap, method, a0, a1, a2)     EVERYTHING ELSE
```

No `open`. No `read`. No `write`. No `fork`. **A process acts on the things it was handed, and
`invoke` is how.** `exit` and `yield` are plain syscalls rather than methods on a TCB capability,
because *a capability is authority over something else* and you do not need to be granted the
right to stop running.

This is DECISIONS §4 rule 3 ("the syscall surface stays narrow and explicit, a boundary not a
habit") meeting §10, and §10 is what makes three enough. A monolithic kernel needs a syscall per
verb because the verb is where the authority check lives. Here the authority is the capability,
and there is one verb: invoke it.

## The ABI is a crate, not a convention

`crates/abi` is a `no_std` crate that **both the kernel and every user program depend on**. The
syscall numbers, the register layout, the method constants, the error codes: one definition. If
it changes, both sides fail to compile. A boundary that can drift silently is not a boundary, and
two files that "agree" by having been edited to match are exactly that.

## A capability is a file descriptor that can point at anything

`crates/caps` is the table, and it is pure logic, so it is host-tested in milliseconds. The
mechanism is deliberately boring: a `Vec<Option<Cap>>`, indexed by a small integer, living in
kernel memory. Userspace sees the integer. **You cannot forge slot 7 for the same reason you
cannot forge `fd 7`: the table is not yours to write.** No cryptography. A bounds check.

The one operation with teeth is `derive`, and its whole job is that **rights only ever narrow**:

```rust
if !rights.is_subset_of(src.rights) {
    return Err(Error::CannotWiden);
}
```

If delegation could widen authority the model is theatre, because you would simply derive
yourself a better capability from the one you hold. And `NONE`/`READ`/`WRITE`/`GRANT` includes a
right Unix cannot express: **`GRANT`, the right to pass a capability on.** Our console capability
has `WRITE` and not `GRANT`, so the program may print and may not lend printing to anyone. Unix's
"the child inherits every fd" is the opposite default, and `FD_CLOEXEC` is the afterthought that
tries to walk it back.

## A new process holds nothing

`CSpace::empty()`, and every thread starts with one. That constructor **is** the decision. A Unix
process inherits its parent's descriptors and can `open()` anything its uid allows. A cricker-os
thread can name nothing until `sched::grant` puts something in its table, and the only thing
`user::exec_elf` grants is a `Console` with `WRITE`.

The demo spawns the **byte-identical** binary twice: once with the console, once with
`exec_elf_with(image, false)`. The first prints. The second cannot, and not because it is denied
when it tries. Slot 0 holds nothing, `invoke` returns `NoSuchSlot`, and there is no other way to
ask. Same program. Different world.

## The interesting refusal: the confused deputy, in our own kernel

The console `write` takes a pointer and a length, both chosen by the user. So the user hands us
`0xffff_0000_4008_0000` (our own `.text`) and a length, and asks us to print it.

**The kernel can read that address.** It reads it all day. So a `write` that simply dereferences
the pointer prints the kernel's memory *on the user's behalf, using the kernel's authority*, and
the program that could not read one byte of it receives all of it.

**No capability check catches this.** The console capability is genuine; the program is entitled
to print. The authority that leaks is not the console's. It is the kernel's own. That is exactly
the compiler service and the billing log at the top of this note: the deputy is confused about
*whose authority it is acting under*.

And it is real. With the check deleted, the demo prints raw kernel bytes, including the four
bytes `ARMd`, which is the [arm64 Image header magic](boot-protocol.md) sitting at the start of
our `.text`.

### The refusal is one question, asked of the hardware

Not re-implemented in software and hoped to agree with the silicon. **Asked of the silicon:**

```text
  AT S1E0R, <addr>     translate this address AS EL0 WOULD, for a read
  ISB
  MRS  x, PAR_EL1      bit 0 (F) set  =>  it FAULTED  =>  EL0 could not have done this
```

One instruction does the stage-1 walk with EL0's permissions and reports the same "no" the
hardware would have given the user program itself. So `syscall::user_slice` asks, for every page
the range touches:

> **Could EL0 have read this itself?** If not, then neither may we, on its behalf.

`PAR_EL1` is a single shared register, so the `at`/`mrs` pair runs with interrupts masked: a
preemption in between could return with another thread's translation result sitting in it. Two
instructions, one masked window, and a bug that would never reproduce if you skipped it.

### The TOCTOU note, which is a comment today and a bug tomorrow

Between "the hardware says EL0 can read this" and "the kernel reads it", the mapping could
change. Nothing can currently change it: an address space belongs to exactly one thread, so there
is no second thread to unmap it, and there is no demand paging. **When either arrives, borrowing
the user's bytes becomes unsound**, and the fix is to copy them under a lock. The comment is in
`user_slice` so the next person meets it before the bug does.

## What it prints

```text
      hello from EL0, through a capability in slot 0.
      and it refused to read the kernel's memory on my behalf.

      every one of its expectations held, including that one.

      and spawned again with an EMPTY capability table, the very
      same binary cannot print one byte. slot 0 holds nothing,
      and there is no other way to ask.
```

## What is deliberately still kernel-served

Invoking the `Console` capability lands in the **kernel**, which owns the PL011. That is a
milestone away from what §10 actually promised. At **milestone 8** the console driver leaves the
kernel, `Object::Console` becomes an `Endpoint` to a userspace console *server*, and the kernel
stops knowing what a UART is. Until that happens we have a capability system with a monolithic
kernel underneath it, which is honest scaffolding and not the destination.

---

# Milestone 7e: IPC, and the scheduler learning to wait

7d gave a process one thing it could do to a capability: invoke a `Console` and have the kernel
act. 7e adds the capability that lets a process talk to **another process**, which is the whole
point of a microkernel and the thing milestone 8 cannot happen without.

## An endpoint is a rendezvous

```text
  invoke(cap, SEND, w0, w1, w2)   ->  0     blocks until a receiver takes the message
  invoke(cap, RECV, _,  _,  _)    ->  w0    (with w1 in x1, w2 in x2) blocks until one arrives
```

Synchronous. There is no buffer, no queue of messages, no "mailbox that fills up." A `SEND` and a
`RECV` on the same endpoint **meet**, the three words move from one thread's registers toward the
other, and both go on their way. If one side arrives first, it waits.

Three words, in registers, and **memory is never touched on the way**. That is the fastpath, and
it is DECISIONS §10's rule taken literally: *IPC carries control; shared memory carries data.*
Bulk data will move later by handing over a frame capability, not by copying bytes into a message.
The moment we copy a buffer through the kernel we have rebuilt Mach, and Mach was slow.

## Which end you are is a matter of rights, not of the endpoint

`SEND` needs `WRITE`. `RECV` needs `READ`. So the same endpoint, handed to two processes with
opposite rights, is a **one-way pipe neither side can run backwards**. The client holds a
`WRITE`-only capability: it can send and it *cannot express* receiving. Nobody had to tell it
which end it is. It could not be the other end if it tried.

This is [`caps::Rights`](../crates/caps/src/lib.rs) doing real work, and it is a distinction Unix
does not have: a pipe fd is a pipe fd.

## The scheduler learned a third thing a thread can be

Threads were `Ready`, `Running`, `Finished`. Now they can be **`Blocked`**, and blocking is the
substance of this milestone.

A blocked thread is **in no run queue**. It sits in an endpoint's wait queue instead, and the
thread on the other side of that endpoint is the only thing in the universe that will move it back
to `Ready`. That is what makes "wait for a message" a real thing a thread does, rather than a spin
loop that asks "is it here yet" and burns a core.

And it took **one line** in `schedule()`, which is the interesting part:

```rust
let runnable = state == Some(State::Running);
// ...only requeue `current` if runnable...
```

`schedule()` can be called from the timer IRQ *while a thread is halfway through blocking itself*
— it has marked itself `Blocked` and joined an endpoint queue but has not yet reached its own
`schedule()` call. The timer must not helpfully requeue it. One equality check is the whole
defense, and a test (`a_receiver_blocks_until_a_sender_arrives`) fails loudly without it: with the
check relaxed to "anything not Finished is runnable", the blocked receiver gets rescheduled and
returns from `ipc_recv` holding a message nobody sent.

## Where the message actually lives

A thread has a `mailbox: [u64; 3]`. It is a `Thread` field and not a stack local **because the
two halves of a rendezvous run at different times on different stacks**: a sender deposits its
message and blocks, and a receiver, running later, reaches into the *sender's* `Thread` to collect
it (or the reverse). The mailbox is the one place both threads can agree on when only one of them
is running at a time.

For a user thread, the received words have to end up in its EL0 registers. Word 0 rides back the
way every syscall result does, in `x0` (the dispatcher writes it from the return value). Words 1
and 2 the kernel places into the trap frame's `x1`/`x2` directly, because a syscall return is one
register and a message is three. Writing the trap frame is writing the user's registers ([see
7a](userspace.md)).

## Single core is a gift here

Every worry that makes IPC hard on a real machine — a sender and receiver racing on the queue, a
message half-delivered — cannot arise, because **only one thread runs at a time**. Between a
thread releasing the scheduler lock and calling `schedule()` to block, the *only* thing that can
run is the timer IRQ, and the one-line `runnable` check already handles that. When SMP arrives
(DECISIONS §6) this all gets harder, and the notes here will be the record of what was true before
it did.

## What it looks like

The demo hands a user program a `WRITE`-only endpoint capability in slot 1, and spawns a kernel
thread blocked on the other end:

```text
      hello from EL0, through a capability in slot 0.
      and it refused to read the kernel's memory on my behalf.
      and sent a word to a server through slot 1.

      a server thread received 0x5eed1e55 from a user program that holds
      one capability for it and no other way to find it.
```

## The server is a kernel thread, for one more milestone

The thing on the other end of that endpoint is a kernel thread. That is the last piece of
scaffolding. **Milestone 8 makes it a userspace process**: the console driver leaves the kernel,
`Object::Console` becomes an `Endpoint` to a console *server*, and `write` becomes an ordinary
`SEND` to it. Everything 7e built — endpoints, blocking, the rendezvous, message-in-registers — is
exactly the machinery that move needs, which is why it came first.
