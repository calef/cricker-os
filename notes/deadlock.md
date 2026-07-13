# Deadlock

## The definition

**A state where every member of a set of tasks is waiting for something that only another
member of that set can provide.** Nobody can proceed, and nobody ever will.

The important word is *ever*. This is not slow. It is not "under load." It is a terminal
state that no amount of waiting resolves, because each party needs another to move first,
and that one is waiting too.

And note what it is **not**: not a crash, not an error, not something that trips an
assertion. **Every participant is behaving perfectly correctly by its own local rules.** The
system is simply, permanently, stopped. That's what makes it insidious. There is nothing to
catch.

## Three things it gets confused with

**Livelock** — everybody is *doing* something, nobody progresses. Two people in a corridor,
each stepping aside for the other, forever. CPU is busy. Nothing happens.

**Starvation** — a task *could* proceed but never gets the chance; the lock keeps going to
someone else. The system progresses. Just not for you.

**A busy-wait hang** looks like livelock and **is** a deadlock. Our spinlock case burns 100%
CPU while deadlocked, which fools people. The distinction is about *waiting*, not about
whether you happen to be burning cycles while you wait.

## The four conditions (Coffman, 1971)

Deadlock requires **all four** at once:

1. **Mutual exclusion** — at least one resource is held exclusively.
2. **Hold and wait** — a party holds one resource while waiting for another.
3. **No preemption** — a resource cannot be taken by force; only the holder releases it.
4. **Circular wait** — there is a cycle in the "waiting for" graph.

**Break any single one and deadlock becomes impossible.** Not unlikely. *Impossible.*

Every deadlock-prevention technique ever invented is "pick a condition and destroy it." Once
you see that, the subject collapses into something tractable.

## Every rule in DECISIONS §9 maps to one

| Our rule | Kills which condition |
|---|---|
| **Lock ordering** (global order, always) | **4, circular wait.** A cycle needs A-before-B *and* B-before-A. A global order makes that unrepresentable. |
| **Never allocate while holding a lock** | **2, hold and wait.** |
| **`IrqSafeMutex`** (mask IRQs while held) | **4.** The handler cannot *enter*, so it can never become the other half of the cycle. |
| **`console::force_unlock()`** in the panic path | **3, no preemption.** We rip the lock away from whoever held it. |

Stare at that last row. **`force_unlock` is a deliberate violation of the no-preemption
condition**, and that is exactly why it is `unsafe`. Condition 3 is what makes a lock *mean*
anything. We are breaking the thing that makes a lock a lock, on purpose, because the
alternative is a silent hang at the moment we most need to speak.

The `unsafe` is not bureaucracy. It is the type system saying *you are about to invalidate
the invariant this abstraction rests on.*

## Why one core can deadlock with itself

It has one thread of execution. How can it wait on itself?

**Because the CPU is a resource in the graph.**

```
interrupted code:   holds the LOCK,  waiting for the CPU
interrupt handler:  holds the CPU,   waiting for the LOCK
```

A cycle of length two, both parties on the same core. The handler didn't *ask* for the CPU;
it **preempted** it. And it can't give it back until it returns, and it can't return until it
gets the lock, and the lock can't be released until the interrupted code gets the CPU back.

Once you see the CPU as a resource, the single-core interrupt deadlock stops being a weird
special case and becomes an ordinary circular wait. See [locking.md](locking.md).

## Prevention, detection, or the ostrich

Three strategies. Which you can afford depends entirely on what you're allowed to do when it
happens.

**Prevention** — structurally make a condition impossible. Lock ordering. Interrupt masking.
**This is what kernels do, and they have no choice**, because:

**Detection and recovery** — let it happen, notice the cycle, and **kill somebody**.
Databases genuinely do this: Postgres maintains a wait-for graph, finds cycles, and aborts a
transaction with a deadlock error. A fine answer *when you have someone you're allowed to
kill and a transaction you can roll back*.

A kernel has neither. Who do you kill? The memory allocator?

**The ostrich algorithm** — ignore it and hope. Named for the myth about heads and sand.
Unix used it deliberately for certain resources, on the grounds that the cost of prevention
exceeded the cost of the occasional reboot.

## Rust does not save you from this

The most counterintuitive part, coming from Rust.

The borrow checker prevents **data races**: two threads touching the same memory, at least
one writing, without synchronization. That is a **safety** property, and Rust genuinely
eliminates it, which is remarkable.

**Deadlock is a liveness failure, not a safety failure.** Nothing is corrupted. No memory is
misused. No invariant is violated. The program simply stops making progress.

Rust has **nothing to say about this**. `Mutex<T>` is entirely safe in Rust's sense and will
deadlock you without a word of complaint. `std::sync::Mutex` is not even reentrant: lock it
twice on the same thread and you deadlock against *yourself*, in safe code, with no `unsafe`
anywhere.

> Safe Rust guarantees your program will not do something **incorrect**. It does not
> guarantee it will do anything **at all**.

Same theme as [registers.md](registers.md): the guarantees are real, and they are narrower
than they feel.

---

*Add to this file as new deadlock scenarios come up.*
