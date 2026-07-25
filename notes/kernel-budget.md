# The kernel's own budget

*(Milestone 19c.1. `kernel/src/kmem.rs`, and the last open-ended draw milestone 14 left standing.)*

## What milestone 14 missed

Milestone 14's thesis was: the kernel allocates nothing after boot; every byte it touches is
static or comes from an untyped a process paid for. It got almost everything. The one draw it
left open-ended was the **kernel stack**: every thread has a 16 KiB (4-page) kernel stack for
its syscalls and exceptions, and `KernelStack::new` took those pages straight from the frame
allocator, open-endedly, one thread at a time. Bounded only by MAX_THREADS in the worst case,
but drawn from the shared allocator rather than a budget the kernel owns.

## The fix, and the decision behind it

`kmem` is one untyped region, carved once at first use, that kernel stacks draw from and recycle
within. After the carve, **the kernel cannot spend beyond it**, structurally: the frame
allocator's count does not move when kernel threads come and go. A steady-state test asserts
exactly that (`kernel_stacks_do_not_touch_the_frame_allocator_in_steady_state`).

The decision this closed was "who pays for a thread's kernel stack," and it went three rounds
worth recording:

1. First answer, kernel-paid-as-today, defended on consistency. Challenged: is it *better* or
   just *what exists*? Honest answer: what exists.
2. Second answer, creator-paid-now, would have forced an owned-vs-borrowed `KernelStack` split
   for one caller while every other thread kept the old rule: two regimes, the worst position.
3. The question that resolved it: *don't we also have the path of reworking everything to the
   better end state?* Costed out, that path was cheaper than claimed, and it exposed a fact that
   collapsed the whole difficulty.

## The fact that made it simple

**A thread runs on its kernel stack, so it cannot swap it.** The stack is fixed at the thread's
creation. And every thread is created by `spawn` (a kernel act) before it ever becomes a user
process: `exec` runs *on* an already-built thread and cannot move the stack out from under
itself. So there is no such thing as a user-created kernel stack to give a separate budget to.
Every kernel stack is kernel-created, so **one source** (the kernel's budget) covers all of
them. The owned-vs-borrowed split feared in round 2, and dismissed as "fifteen lines" in round
3, turned out to be **zero lines**: there is only one owner.

This is why the kernel stack is kernel-budget-paid and not creator-paid in the per-process
sense, and that is correct on its own terms besides: the kernel stack is the memory kernel code
executes on during a syscall, and letting a process supply it would hand a process the ability
to corrupt kernel execution. seL4 sidesteps the whole question by being an event kernel (one
stack per core, no per-thread kernel stack); we chose a process kernel at milestone 6 (blocking
IPC resumes on a per-thread kernel stack, which is why `schedule()` is forty lines), so we own
the stacks, now from a bounded budget.

## Recycling, not allocating

Kernel threads churn (every test spawns some), and region pages are watermark-carved and never
individually returned. So `kmem` keeps a fixed free-stack of dead pages: a reaped thread's stack
pages go on it, the next spawn takes them back, page for page. This is a *budget with reuse*,
not an allocator: one size (a page), one owner, no headers, no coalescing, nothing to verify
beyond a bounded stack. The difference between this and the heap milestone 14 deleted is the
difference between a budget and a market.
