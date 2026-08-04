# 9. Locking: IrqSafeMutex, plus a discipline

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

## Rejected: per-CPU reserve pools

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

## The rules

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

## The ordering rule is now enforced, not merely written down

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

- **SLAB (50) → FRAMES (30)**: a size class runs dry and takes a page while holding its lock.
- **anything → CONSOLE (10)**: a panic prints while holding a lock. Which is *why* the console
  must be the leaf.

The panic path calls `sync::force_reset_ranks()` alongside `console::force_unlock()`. Panicking
while holding the console lock would otherwise trip the ranking assertion *inside the panic
handler* and lose the original message to a recursive panic. **The bookkeeping is a debugging
aid; it must never be the thing that stops us saying what went wrong.**
