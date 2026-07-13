# Registers

## What one physically is

A storage slot **inside the CPU**. Not in RAM, not on a memory stick. On the silicon of the
processor itself, nanometers from the arithmetic circuits that use it.

aarch64 has 31 of them, 64 bits each. **248 bytes.** That is the entire amount of data the
CPU can hold at any one instant.

## The fact everything follows from

**The CPU cannot do arithmetic on memory. It can only do arithmetic on registers.**

`add x0, x1, x2` works because `x1` and `x2` are physically inside the processor, wired to
the adder. The result lands in `x0`, also inside the processor. No memory is involved.

To add two numbers that live in RAM you have no choice:

```asm
ldr x1, [x3]        ; pull a number in from memory
ldr x2, [x4]        ; pull the other one in
add x0, x1, x2      ; NOW you can add them
str x0, [x5]        ; push the answer back out
```

That is what "load/store architecture" means, and why `ldr`/`str` are the only instructions
that touch memory. **Registers are the workbench. RAM is the warehouse. Nothing gets worked
on in the warehouse.**

## Why only 31? They're absurdly expensive

Register speed is a physical fact about how far electricity has to travel.

| Where | Access cost | How much |
|---|---|---|
| **Register** | ~0 cycles (it's right there) | **248 bytes** |
| L1 cache | ~4 cycles | ~64 KB |
| L2 cache | ~14 cycles | ~1 MB |
| L3 cache | ~50 cycles | ~32 MB |
| **RAM** | **~200-300 cycles** | gigabytes |

A single trip to RAM costs a couple hundred cycles, during which the CPU could have run a
couple hundred instructions. Modern processors spend an enormous share of their design
budget on *not having to go to RAM*.

Adding more registers isn't free either: each costs die area, and naming one of 32 takes 5
bits inside a 32-bit instruction.

When the compiler **"spills to the stack"**, this is what happened: more live values than
registers to hold them, so some got dumped to memory. Choosing what stays is **register
allocation**, one of the hardest problems a compiler solves.

## Registers have no types

`x0` is 64 bits. Integer? Pointer? `bool`? Float?

**The CPU has no idea and does not care.** It's 64 bits. The "type" exists entirely in the
mind of whoever wrote the code. `add` two pointers together and the hardware will
cheerfully do it.

Worth sitting with as a Rust person: every guarantee Rust gives evaporates at this level.
The borrow checker, the type system, `Option<T>` niche-optimized into a null pointer, all
of it is a fiction the compiler maintains *above* the hardware. Down here there are only
64-bit words. When we write `unsafe` in the kernel, we are stepping down to this layer,
where nothing is checked because there is nothing to check with.

## There's only one `x0`, and everyone wants it

Registers are global to the core. Every function uses `x0`. So how does `foo` call `bar`
without `bar` trashing what `foo` was holding?

By **agreement**. The aarch64 calling convention (AAPCS64):

| Registers | Role |
|---|---|
| `x0`–`x7` | function **arguments**; `x0` is the **return value**. Caller-saved: a callee may freely destroy them. |
| `x8`–`x18` | scratch. Also caller-saved. |
| `x19`–`x28` | **callee-saved.** To use `x19`, a function must save the old value and restore it before returning. |
| `x29` | frame pointer |
| `x30` | link register (return address) |

**None of this is enforced by hardware.** It is a convention, and it is the only reason
Rust and C code can call each other. It's also why `kernel_main` is declared `extern "C"`:
we're telling Rust "follow this exact agreement, because assembly is going to call you."

## The punchline

**The register file *is* the CPU's state.** All of it.

Capture all 31 general registers, plus `sp`, plus the program counter, plus a few status
bits, and you have completely described what the processor was doing. Every bit of "what
this program is in the middle of" lives in those ~300 bytes.

Which means: **restore them and execution resumes as if nothing happened.** The code cannot
detect that it was ever interrupted.

That is not an analogy. It is the literal mechanism behind three things:

- **A context switch** = save this thread's registers to memory, load another thread's
  registers from memory, `ret`. The CPU is now running a different thread. (Milestone 6.
  About thirty instructions.)
- **An interrupt** fires between any two instructions. The handler is about to use `x0`,
  but the interrupted code had something important in `x0`. So the handler saves every
  register, works, restores every register, returns. The interrupted code never knows time
  passed. (Milestones 2 and 5.)
- **"A thread is a stack plus a set of register values"** ([stack.md](stack.md)) was not a
  slogan. It is the complete and literal definition.

Everything in this operating system, from here to milestone 10, is some variation on
**carefully saving and restoring 248 bytes**.

---

*Add to this file as new register concepts come up.*
