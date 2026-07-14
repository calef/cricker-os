# Locking

## The deadlock, and why one core doesn't save you

A plain spinlock in a kernel that takes interrupts is a hang waiting for a schedule.

```
  kernel code:  ALLOCATOR.lock()   <- acquired
                ...working...
       ⚡ TIMER INTERRUPT
  handler:      ALLOCATOR.lock()   <- spins
                                      spins waiting for a lock that only the code it
                                      interrupted can release, and that code cannot run
                                      until the handler returns.
```

**This is not a race.** It is not "sometimes, under load." It is a *deterministic* hang the
moment the timing lines up, and it will look exactly like the mystery in
[stack.md](stack.md) that cost us two hours.

And note what it does **not** need: a second core. Our "single-core for now" decision
([DECISIONS.md](../DECISIONS.md) §6) does not protect us here at all. One core, one lock,
one interrupt, dead machine.

## The fix

**Mask interrupts for as long as the lock is held.** The interrupt cannot fire, so it
cannot try to take the lock. Linux calls this `spin_lock_irqsave`; ours is `IrqSafeMutex`
in `kernel/src/sync.rs`.

On aarch64 that means the `I` bit of **`PSTATE.DAIF`**:

| Bit | Name | Masks |
|---|---|---|
| 9 | `D` | Debug exceptions |
| 8 | `A` | SError |
| **7** | **`I`** | **IRQ** |
| 6 | `F` | FIQ |

Set the bit to *mask*, clear it to *unmask*. Note the polarity: `1` means "masked," which
is the opposite of how most people read a flag called `I`.

`msr daifset, #2` masks IRQs; `msr daifclr, #2` unmasks them. Single instructions that
touch only the bits you name, so there is no read-modify-write window to lose a race in.

## Two orderings that are the entire point

**Acquire: mask interrupts FIRST, then take the lock.**

```rust
let irqs_were_enabled = interrupts::disable();   // <- first
let guard = self.inner.lock();                   // <- second
```

The other order leaves a window where you hold the lock with interrupts still live. That
window is one instruction wide. It is also the exact deadlock.

**Release: drop the lock FIRST, then restore interrupts.**

```rust
unsafe { ManuallyDrop::drop(&mut self.guard) };  // <- first
interrupts::restore(self.irqs_were_enabled);     // <- second
```

Same window, arrived at from the other side. An interrupt fires, the handler wants the
lock, and you still hold it.

Both windows are a couple of instructions. Both are fatal. Both work perfectly in testing
for months.

## Restore is not the same as enable

`IrqSafeGuard::drop` **restores the interrupt state that was in effect when the lock was
taken.** It does not simply enable interrupts.

The difference bites when a lock is taken inside a context that *already* had interrupts
masked: an interrupt handler, or inside an outer lock. Blindly enabling on release would
unmask interrupts **inside an interrupt handler**, and the resulting fault is one you will
not enjoy explaining.

This is precisely why Linux's is called `irqsave`/`irqrestore` and not `irqoff`/`irqon`,
and it is the single easiest thing to get wrong here. There is a test named
`irq_safe_mutex_restores_rather_than_enables` whose only job is to catch it, and it was
verified against a deliberately broken `restore()`.

## Why we didn't build per-CPU reserves

Considered seriously, and it turned out to be **an answer to a different question**.

Per-CPU page caches (Linux's PCP lists, slab's per-CPU caches) exist for **scalability**
(on 64 cores, one global lock is a catastrophe) and **cache locality** (a frame this core
just freed is warm in this core's L1). They are not an interrupt-safety mechanism, and
**Linux still wraps them in `local_irq_save`.** Same core, same structure, same deadlock.

They belong to the SMP conversation, where the problem is lock *contention*, not deadlock.

## Why masking is genuinely sufficient

Not a compromise. Walk the handlers we will actually have:

| Handler | Milestone | Allocates? |
|---|---|---|
| Timer tick | 5 | No. Bump a counter, flag "reschedule wanted." |
| UART receive | 5 | No. Push a byte into a pre-allocated ring. |
| virtio completion | 8 | No. Mark an I/O done, wake a thread. |
| **Page fault** | 4, 7 | **Yes.** Demand paging, copy-on-write. |

So the whole question is the page fault. And a page fault is **synchronous**: taken from the
exact instruction that touched the bad address, on behalf of that context. Which gives the
rule real kernels use:

> **Kernel memory is never demand-paged.** Kernel pages are mapped eagerly. A page fault
> taken from **EL1 is a bug** and is fatal (already true in our `fatal()`).

Then every allocating fault comes from **EL0**, and userspace was holding no kernel locks,
because it *cannot*. There is nothing to deadlock against, and no reserve pool is needed.

## The panic path must be able to break the lock

If we fault in the middle of a `println!`, the fault handler's own attempt to print takes
the console lock again and hangs. **We lose the one message that mattered, at the exact
moment we needed it.**

So the panic handler and the fatal exception path both call `console::force_unlock()`
first. Output may be spliced. That is a fine price for getting the message out at all.

Linux does the same thing and calls it `bust_spinlocks`.

`force_unlock` is `unsafe` for a real reason: it means "whatever the previous holder was
doing is now half-finished, and its data may be inconsistent." Acceptable when the
alternative is a silent hang. Unacceptable at literally any other time.

## The ordering rule, enforced

The other deadlock, and the nastier one: **AB-BA**. Thread 1 takes lock A then wants B; thread
2 takes B then wants A. Neither can proceed. Unlike the interrupt deadlock, this one is a
*real race*: it needs the timing to line up, so it passes tests for months.

We wrote "define a global order and always take them in it," and then relied on remembering.

Now every lock carries a **rank**, and `lock()` asserts:

> **You may only acquire a lock strictly LOWER than everything you currently hold.**

```
  50  HEAP, SLAB      the allocators
       |
  30  FRAMES, RAM     the physical memory map
       |
  10  CONSOLE         the leaf: everyone may take it, it takes nothing
```

**If every acquisition strictly decreases the rank, a cycle is unrepresentable.** Not unlikely.
Impossible. Look at [deadlock.md](deadlock.md): this destroys condition 4, circular wait,
outright.

That makes it **prevention, not detection**. Linux's `lockdep` builds a dependency graph at
runtime and hunts for cycles — powerful, and expensive. Ranking costs three instructions and
*cannot be wrong*. FreeBSD (WITNESS) and Solaris use the same mechanism.

Two locks at the **same** rank may never nest (`R < R` is false), which is exactly right: equal
rank means we have declared no order between them, so nesting them would be choosing one at
random.

### A design it would have caught

`memory::ram_regions()` used to return an iterator that **held the RAM lock while the caller
iterated**. `mmu::map_everything` iterates it and *allocates frames inside the loop* — so it
would have held RAM (30) while taking FRAMES (30), and `30 < 30` is false. The ranking would
have failed it on the spot. (We happened to fix it for other reasons first, which is not a
system.)

### The panic path must reset it

If we panic while holding the console lock, `HELD_RANK` is 10, and the panic handler's own
print would try to take rank 10 again. `10 < 10` is false, so the ranking would fire a
violation **inside the panic handler**, and we'd lose the original message to a recursive
panic.

So the panic path calls `sync::force_reset_ranks()` alongside `console::force_unlock()`.

**The bookkeeping is a debugging aid. It must never be the thing that stops us saying what went
wrong.**

## The rules

See [DECISIONS.md](../DECISIONS.md) §9 for the full table. The short version:

1. All kernel locks are `IrqSafeMutex`.
2. Mask, then lock. Unlock, then restore.
3. **Restore**, never blindly enable.
4. Keep critical sections short: interrupts are off for the whole of it.
5. Never allocate, block, or `wfi` while holding a lock.
6. Two locks? Define a global order and always take them in it. Otherwise AB-BA deadlock,
   which is a *real* race and far nastier than the one this note is about.
7. Interrupt handlers record and defer. They do not do work.

---

*Add to this file as new locking concerns come up.*
