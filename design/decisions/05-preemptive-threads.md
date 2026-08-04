# 5. Execution model: preemptive threads with real stacks

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

## The claim, sharpened (revisited after milestone 5)

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

## The corroboration: Go had to build preemption

Go's goroutines were originally **cooperative**. They yielded at function calls, via the
stack-growth check in every function prologue. And Go owns its compiler, owns its runtime, and
compiles **every line that executes**: every assumption async needs, satisfied.

It still didn't work. A goroutine in a **tight loop with no function calls** never yields. The
garbage collector's stop-the-world could never stop it. The program hangs.

**Go 1.14 (2020) added asynchronous preemption**: the runtime sends a signal to the OS thread,
and the signal handler forces the goroutine to yield.

Which is to say: **Go built a timer interrupt in userspace, because cooperative scheduling
could not take the CPU back from a loop.**

If a language that owns its entire toolchain could not get away with cooperative scheduling, a
kernel running arbitrary ELF binaries certainly cannot.

## The asymmetry, which is the whole decision

| Direction | Cost |
|---|---|
| threads → async | **additive.** Run an executor on top. Nothing is thrown away. |
| async → threads | **a rewrite.** You need per-task stacks and a context switch: exactly what the executor existed to avoid. The executor goes in the bin. |

When one direction is cheap and the other is a rewrite, take the one that keeps the option
open. That generalizes well beyond this decision.

## And the hard part turned out to be already written

The instinct that async was "more tractable" was measuring the wrong thing.

`SAVE_CONTEXT` and `RESTORE_CONTEXT` in `vectors.s` were written at **milestone 2**, for
exception handling, with no thought of threads. They save `x0`–`x30`, `ELR_EL1`, and
`SPSR_EL1` into a `TrapFrame`.

**That is the register file.** A context switch is: save into thread A's frame, restore from
thread B's frame, swap `sp`. About thirty instructions, and most of them already exist,
because a kernel needs them anyway.

Writing a scheduler is not hard. Saving a register file is not hard. What is hard is the part
async cannot do at all.
