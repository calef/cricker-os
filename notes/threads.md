# Threads, the context switch, and preemption

Milestone 6. The thing the whole project has been arguing about since before a line of it
existed.

## A thread is a stack plus a set of register values

From [registers.md](registers.md), milestone 1:

> **That is not a metaphor. It is the complete and literal definition.**

And here it is, written down:

```rust
pub struct Thread {
    pub id: Tid,
    pub state: State,
    pub context: *mut Context,     // <- 8 bytes. The ENTIRE saved CPU state.
    pub stack: Option<KernelStack>,
}
```

**A suspended thread's whole CPU state is one stack pointer.** Everything else — the registers,
the return address, the frame pointer — is sitting on the stack it points at, pushed there by
`switch_to`. Eight bytes.

## The context switch is fifteen instructions

```asm
switch_to:                      // x0 = &prev.context,  x1 = next.context
    sub  sp, sp, #96
    stp  x19, x20, [sp, #0]     // push OUR callee-saved registers
    ...
    stp  x29, x30, [sp, #80]

    mov  x2, sp
    str  x2, [x0]               // prev.context = sp    <- the whole of "saving a thread"

    mov  sp, x1                 // and now we are on somebody else's stack

    ldp  x19, x20, [sp, #0]     // pop THEIR callee-saved registers
    ...
    ldp  x29, x30, [sp, #80]
    add  sp, sp, #96

    ret                         // <- returns into a DIFFERENT THREAD
```

**The last instruction is the trick.** `ret` jumps to `x30`, and by that point `x30` has been
loaded from the *other thread's* stack. So it does not return to the caller. **It resumes a
different thread, at the point where that thread last called `switch_to`, possibly seconds
ago.**

A context switch is a function call that returns somewhere else.

### Why only twelve registers, when the trap frame saves thirty-three

`vectors.s` saves **all 31** general registers, because an exception lands between two
arbitrary instructions and the interrupted code has no idea.

`switch_to` is an ordinary **function call**. AAPCS64 already says the caller must assume
`x0`–`x18` are destroyed by any call it makes — the compiler has already spilled anything it
cared about. So we save only what the convention promises a callee will preserve: `x19`–`x28`,
`x29` (frame pointer), `x30` (link register), and `sp`.

That isn't an optimization we invented. It falls out of the calling convention, and it is why
real kernels have both a trap frame *and* a much cheaper voluntary switch.

(And no floating-point registers, because the kernel is `softfloat` — a decision made at
milestone 1 for a completely unrelated reason. `d8`–`d15` simply do not appear.)

## Starting a thread that has never run

A new thread has no saved registers to restore. So we **fake them**: write a `Context` onto the
fresh stack with `x30` pointing at `thread_trampoline`.

Which means the very same `ret` that *resumes* an existing thread also *starts* a new one.
**There is no separate first-run path.** The trampoline just happens to be what `x30` points at.

The closure travels in `x19` — a callee-saved register, chosen precisely because `switch_to`
restores it on the way in.

> `Box<dyn FnOnce()>` is a **fat** pointer (data + vtable), two words, and we have one register.
> So box it twice: `Box<Box<dyn FnOnce()>>` is a *thin* pointer to a fat one, and fits.

### The trampoline must unmask interrupts, and this is easy to miss

```asm
thread_trampoline:
    msr  daifclr, #2        // <- ENABLE INTERRUPTS
    mov  x0, x19
    bl   thread_entry
```

We usually arrive here **from inside the timer IRQ handler**, which the hardware entered with
IRQs masked.

Every *resumed* thread gets its interrupt state back from `eret` restoring `SPSR_EL1`. A
brand-new thread has **no `SPSR` to restore** — it was never interrupted, because it has never
run at all.

Without that one instruction, the first thread you spawn runs with interrupts masked **forever**.
It can never be preempted. If it loops, the machine is gone.

Which would be a cooperative scheduler with extra steps, and an ironic way to lose this
particular argument.

## Preemption is four lines, because milestone 2 did the hard part

At the bottom of the IRQ handler:

```rust
gic::end_of_interrupt(intid);

if sched::take_need_resched() {
    sched::schedule();          // <- may not return here for a long time
}
```

We're still on the interrupted thread's kernel stack, with its **full TrapFrame sitting below
us** — `vectors.s` saved it before Rust ever ran.

`schedule()` may switch to another thread entirely. When it does, that call **does not return
here**. It returns *in some other thread*. We come back only when somebody schedules us again —
and then `exception_restore` pops the TrapFrame and `eret` resumes the instruction we
interrupted, **which never knew any of this happened.**

That is the whole of preemption. The register-saving machinery already existed, built at
milestone 2 for exception handling, with no thought of threads.

Note the EOI comes **first**. Switching away with the interrupt unacknowledged would leave the
GIC refusing to deliver anything of equal or lower priority to the thread we switch *to*.

## Three rules, and each is a bug if you get it wrong

**1. Release the run-queue lock BEFORE switching.** Switch away holding it and the lock is now
held by a thread that is not running. The next thread to want it spins forever, waiting for a
thread that can only be scheduled by taking the lock. A deadlock that would take a day to find.

**2. Interrupts stay masked across the switch.** Between "I decided to switch" and "I switched"
there must be no window for the timer to decide *again*.

And the saved interrupt state is a **local, on this thread's stack** — which is exactly what
makes it work. When somebody eventually switches back to us, `switch_to` returns into
`schedule()`, and that frame, with the right `was_enabled` in it, is still sitting where we left
it.

**3. A thread cannot free the stack it is standing on.** `exit()` marks itself `Finished` and
calls `schedule()`. The **next** thread reaps it, once we are safely off it. Every kernel has
something called a reaper, and this is why.

## The address-space leak that hid behind a two-frame test failure

The reaper test failed with **two frames leaked** — not thirty-two. So the stacks *were* being
freed.

The two were **page tables**. The stack area is a fresh region of virtual address space, so the
first `map_page` there had to build an L2 and an L3. And `unmap_page` frees the leaf mapping but
**leaves the intermediate tables standing** (the TODO on `paging::unmap`).

Which exposed a real leak hiding behind it: stack virtual addresses were **bump-allocated and
never reused**. Threads come and go, but every 2 MiB of address space ever consumed keeps its
page tables forever.

The fix is a free list of dead threads' address ranges, so a new thread lands in page tables that
already exist. There is now a test asserting that a **second** batch of eight threads costs
**exactly zero** additional frames.

## The test

```rust
sched::spawn(|| {
    while !STOP.load(Relaxed) {
        SPINNING.fetch_add(1, Relaxed);
        // Deliberately nothing else. No yield. No syscall. Not even a function call.
    }
});
```

From [DECISIONS.md](../DECISIONS.md) §5, written before any of this existed:

> A userspace process is an arbitrary ELF binary. It has its own stack, **it never yields**, and
> it will loop forever because we will write a bug. Under cooperative scheduling, one bad user
> program hangs the machine permanently.

Under async/await, or Go before 1.14, or any cooperative runtime, that thread takes the CPU and
never gives it back. The **only** thing that can take it back is a timer interrupt landing
between two instructions of that loop and switching the stack out from under it.

At boot:

```
  half a second later, having spawned two threads that NEVER yield:

    thread 1 (hostile) :    3304328 iterations
    thread 2 (polite)  :    3133498 iterations
    preemptions        :         48

  neither asked to be interrupted. both were.
```

**The argument was right, and the kernel can host untrusted code.**

---

*Add to this file as new scheduling concepts come up.*
