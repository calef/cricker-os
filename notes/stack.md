# The stack, `sp`, and `x30`

## What problem it solves

A function needs scratch space: somewhere for its local variables, and somewhere to park
`x30` when it calls something else.

You can't statically assign every function a fixed chunk of memory, for two reasons.
**Recursion** (one function can be in progress many times at once, each instance needing
its own locals) and **waste** (a program with 10,000 functions would reserve scratch space
for all of them when only a handful are ever active).

## The insight

**Function lifetimes are strictly nested.** If `foo` calls `bar`, `bar` always finishes
before `foo` does. There is no way for `foo` to return while `bar` is still running.

That's a strong property. It means scratch space can be allocated and freed in **strict
LIFO order**, which means you don't need a memory allocator at all. You need **a pointer
that moves down when you enter a function and up when you leave.**

That pointer is `sp`. The region it moves through is the stack. That's the whole idea;
everything else is bookkeeping.

## What it costs

Allocating 32 bytes of locals:

```asm
sub sp, sp, #32
```

One instruction. Freeing them: one instruction. No free list, no search, no fragmentation.

This is why stack allocation is effectively free and heap allocation isn't. `malloc` has
to *search* for a suitable hole, because heap objects don't have nested lifetimes and can
be freed in any order. The stack skips all of that by exploiting a structural fact about
how function calls work.

## `sp` is a register that holds an address

That's all it is. A 64-bit CPU register whose value is a memory address: the "stack
pointer."

And **the stack is not a data structure the CPU knows about. It's just a region of RAM.**
The only thing that makes it a stack is that everyone agrees to treat it like one: `sp`
points at the current top, and the region grows **downward** into lower addresses.

Which is why the [linker script](linker-scripts.md) has to reserve a chunk of memory and
export `__stack_top`. There is nothing magic to allocate. We are picking a stretch of RAM
and declaring "this is the stack now."

## Stack frames, and why backtraces exist

One function's slice of the stack (its locals, its saved registers, its parked `x30`) is
a **stack frame**. The stack is a pile of them, one per function currently in progress.

Each frame also saves `x29`, the **frame pointer**, which points at the previous frame. So
the frames form a linked list running back down the call chain, and each one has a saved
`x30` sitting right next to it.

**That linked list is a stack trace.** Printing a backtrace means walking `x29` from frame
to frame, reading the saved `x30` out of each, and mapping those addresses to function
names. There is no magic table. The information was already lying in memory because of how
calls work.

## Stack overflow (and a thing we have to deal with)

`sp` moves down and **nothing checks it**. Recurse too deeply and `sp` walks off the
bottom of the reserved region into whatever memory is below.

In a normal program the OS puts an unmapped **guard page** just past the end of the stack,
so touching it raises a page fault and you get a clean crash. That is what "stack
overflow" *is*: you hit the guard page.

**We don't have that.** We have 64 KiB reserved in a linker script and nothing below it
but more of our own kernel. Blow the stack in cricker-os today and we will silently
overwrite our own `.bss` and then behave insanely.

TODO (milestone 4): once the MMU is on, leave a page unmapped below the stack and get real
overflow detection.

## `bl` does *not* push the return address (this is not x86)

On **x86**, `call` pushes the return address onto the stack.

On **aarch64**, `bl kernel_main` ("branch with link") puts the return address in a
**register**: `x30`, also called `lr` (link register). It never touches memory.

So a call with a garbage `sp` technically succeeds. The problem arrives one instruction
later, in the callee's prologue:

```asm
stp  x29, x30, [sp, #-32]!   ; save frame pointer + link register, sp -= 32
mov  x29, sp                 ; establish the frame pointer
...                          ; locals live at [sp, #16], etc.
ldp  x29, x30, [sp], #32     ; restore them, sp += 32
ret                          ; branch to whatever is in x30
```

A function needs the stack for two reasons:

1. Its **local variables** live there.
2. It must **spill `x30` to memory** before making any call of its own, because a nested
   `bl` overwrites `x30` and would destroy its own return address.

(Corollary: a *leaf* function with no locals touches the stack not at all, and would run
fine with a garbage `sp`. Don't rely on this.)

**With a garbage `sp`, the callee's first instruction stores registers to a random
address.** Which is worse than crashing, because it might not crash. It might quietly
corrupt something and fail ten thousand instructions later.

**Rule: set `sp` before calling any Rust function.**

## Two details that will bite you

**There is no `push` or `pop` instruction.** ARM removed them. You use `stp` / `ldp`
(store pair / load pair) with pre- and post-indexed addressing. That's what the `#-32]!`
and `], #32` above are doing; the `!` means "write the updated address back into `sp`."
It is push and pop, spelled out.

**`sp` must always be 16-byte aligned.** Not 8. Sixteen. A misaligned `sp` raises an
alignment fault when used. This is why the prologue above subtracts 32 and not 24. It is
a classic source of mysterious early-boot crashes.

## One stack pointer per exception level

aarch64 does not have one stack pointer. It has **`SP_EL0`, `SP_EL1`, `SP_EL2`,
`SP_EL3`** (see [exception levels](aarch64.md)).

Consider what that buys us. A userspace program at EL0 uses `SP_EL0` and can set it to
any garbage it likes, because it's the program's own stack and its own problem. When an
exception fires and the CPU enters EL1, **the hardware automatically switches to
`SP_EL1`**, the kernel's stack pointer, which userspace cannot touch.

So a malicious or broken user program **cannot** corrupt the kernel's stack by handing it
a bad `sp`. The hardware will not allow the two to be confused. That is not a convention
the kernel enforces. It is silicon.

This is the mechanism that makes milestone 7 (user mode) safe, and it's another place
aarch64's clean-sheet design visibly beats x86, where the equivalent is bolted together
out of the TSS and a privilege-change stack switch.

## The part that connects to everything else

**A thread is, essentially, a stack plus a set of register values.**

That is not a metaphor. It is what a thread *is* at the hardware level. Two threads
running concurrently means two independent chains of nested function calls in progress,
which means two separate stacks. There is no way around it.

This is why the async-vs-preemptive decision mattered so much (see
[DECISIONS.md](../DECISIONS.md) §5). Async tasks are state machines the compiler builds on
the heap, which is why they don't each need a stack, which is why async looked cheaper.
But a real user program is not a state machine we built. It is arbitrary machine code with
an arbitrary call depth, and it needs a real stack.

So **milestone 6 (threads) is really**: allocate a stack per thread, and write assembly
that saves the current register set, swaps `sp`, and restores a different register set.
That is a context switch. It's about thirty instructions, and the stack is the thing being
switched.

---

*Add to this file as new stack concepts come up.*
