# Exceptions

## One mechanism, three purposes

The most important thing to see before building any of it: on aarch64 these are **the same
mechanism**.

| | Cause | Milestone |
|---|---|---|
| **Fault** | bad memory access, illegal instruction, misaligned `sp` | **2** (here) |
| **Interrupt** | the timer fires, the UART has a byte | **5** |
| **Syscall** | userspace executes `svc` | **7** |

All three suspend the current instruction stream, switch to EL1, and jump to an address the
kernel chose. Only the *reason* differs. So we build the plumbing once, in milestone 2, and
milestones 5 and 7 are mostly a matter of adding cases to a `match`.

## The table's shape is dictated by silicon

`VBAR_EL1` holds the base address of the vector table. When an exception fires, the hardware
computes the target as **`VBAR_EL1 + a fixed offset`**, where the offset is determined by two
things: where the exception came from, and what kind it is.

Four sources × four kinds = **16 entries, each exactly 128 bytes**, and the whole table must
be **2048-byte aligned**.

```
offset  source                    kind
0x000   Current EL, SP_EL0        Synchronous
0x080                             IRQ
0x100                             FIQ
0x180                             SError
0x200   Current EL, SP_ELx        Synchronous   <- a kernel bug lands HERE
0x280                             IRQ           <- the timer, milestone 5
0x300                             FIQ
0x380                             SError
0x400   Lower EL, AArch64         Synchronous   <- `svc` lands HERE, milestone 7
0x480                             IRQ
0x500                             FIQ
0x580                             SError
0x600   Lower EL, AArch32         Synchronous
0x680                             IRQ
0x700                             FIQ
0x780                             SError
```

**The 2048-byte alignment is not style.** The CPU assumes the low 11 bits of `VBAR_EL1` are
zero. A misaligned table sends every exception in the machine to a wrong address. There's a
test for it.

### The four kinds

- **Synchronous** — caused by the instruction being executed. Data abort, instruction abort,
  illegal instruction, `svc`, `brk`, alignment fault. *Precise*: `ELR_EL1` points at (or just
  past) the offending instruction.
- **IRQ** — a normal interrupt. Asynchronous; it has nothing to do with what you were running.
- **FIQ** — "fast interrupt," a higher-priority class. Mostly used by the secure world these
  days. We treat it as fatal.
- **SError** — System Error. Asynchronous, and usually a bus error or ECC failure. Genuinely
  bad news; the machine is broken.

### The four sources

- **Current EL, SP_EL0** — we're at EL1 but using `SP_EL0`. Kernels almost never do this.
- **Current EL, SP_ELx** — we're at EL1 using `SP_EL1`. **This is where our own bugs land.**
- **Lower EL, AArch64** — came from EL0. **This is where userspace lands**, and where milestone
  7's syscalls will arrive.
- **Lower EL, AArch32** — 32-bit userspace. We will never support it.

## 128 bytes is 32 instructions

That's enough to save state and branch. It is nowhere near enough to do real work. **This
constraint is why every aarch64 kernel on earth looks nearly identical right here**: each slot
saves the register file, records which slot it was, and branches to common code.

Our `VECTOR_ENTRY` macro comes to 24 instructions. Comfortable, but not by much.

## The trap frame

`SAVE_CONTEXT` pushes 272 bytes onto the kernel stack: `x0`–`x30`, plus `ELR_EL1`, plus
`SPSR_EL1`, plus 8 bytes of padding to keep `sp` 16-byte aligned ([stack.md](stack.md)).

This is the [registers.md](registers.md) punchline made real. **The register file is the CPU's
state.** Save it, and you have frozen exactly what the machine was doing. Restore it, and
execution resumes with no way to detect the interruption ever happened.

Two registers beyond the general-purpose file are essential:

- **`ELR_EL1`** — Exception Link Register. *Where the interrupted code resumes.* `eret` reloads
  the program counter from it.
- **`SPSR_EL1`** — Saved Program Status Register. The processor state (condition flags,
  exception level, interrupt masks) at the moment of the exception. `eret` restores it, and
  **that includes the exception level**, which is exactly how milestone 7 will enter userspace:
  set `SPSR_EL1` to say EL0, and `eret` drops privilege.

### The layout is a contract with assembly

`struct TrapFrame` in `exceptions.rs` must match `SAVE_CONTEXT` in `vectors.s` byte for byte.
The compiler cannot check this. There is a `const _: () = assert!(size_of::<TrapFrame>() ==
272)`, which catches maybe half the ways to get it wrong. It will not catch two same-typed
fields being swapped.

A bug here is the **nastiest possible failure**: it scrambles a register while still returning
happily to the right address, corrupting the caller's state and blaming a completely innocent
piece of code thousands of instructions later. `registers_survive_an_exception` exists purely
to catch it.

## ESR_EL1 tells you what happened

The **Exception Syndrome Register**. Bits 31:26 hold the **Exception Class (EC)**, and it is
the single most useful field in the machine when something has gone wrong.

| EC | Meaning |
|---|---|
| `0x15` | **SVC from AArch64** — a syscall. Milestone 7. |
| `0x21` | Instruction abort, same EL — we jumped somewhere bad |
| `0x24` | Data abort, **lower** EL — *userspace* touched bad memory. Milestone 7 makes this a segfault. |
| `0x25` | Data abort, same EL — **the kernel** touched bad memory. Our bug. |
| `0x26` | SP alignment fault — `sp` wasn't 16-byte aligned |
| `0x3c` | `BRK` instruction |

**`FAR_EL1`** (Fault Address Register) holds the address that faulted, but **only for aborts
and alignment faults.** For anything else it's stale garbage from some earlier fault. Printing
it unconditionally would be a lie, so `fatal()` checks the EC first.

## `brk` vs `svc`: the gotcha

`ELR_EL1` points at **different places** depending on the exception.

For **`svc`**, the hardware sets `ELR_EL1` to the instruction *after* the `svc`. You handle the
syscall and `eret`, and the program continues normally.

For **`brk`**, `ELR_EL1` points **at the `brk` itself.** So a naive `eret` re-executes it,
forever.

Stepping over it means advancing `ELR_EL1` by one instruction:

```rust
frame.elr += 4;
```

Four, because **every aarch64 instruction is exactly 4 bytes**. The fixed-width design we
admired in [aarch64.md](aarch64.md) is what makes this a `+= 4` rather than an instruction
decode. On x86 you would have to *disassemble the instruction* to know how long it was.

## The ISB that is easy to forget

```rust
VBAR_EL1.set(base);
barrier::isb(barrier::SY);
```

An **Instruction Synchronization Barrier** forces the CPU to discard everything it has already
fetched or speculated past this point, and start again.

Without it, the write to `VBAR_EL1` is **not architecturally guaranteed to be in effect for the
very next instruction**. And "the very next instruction" is exactly when a fault might arrive.

One line. Easy to leave out. Leaving it out produces a bug that appears only under timing you
cannot reproduce.

## What a fault looks like now

Before milestone 2, a bad memory access killed the machine in silence. Now:

```
[EXCEPTION]  Current EL, SP_ELx, Synchronous
             Data abort from the same EL (EC 0x25)

  ESR_EL1   0x0000000096000050   what happened
  FAR_EL1   0x00000000dead0000   the address that faulted
  ELR_EL1   0x0000000040081a40   the instruction that did it
  SPSR_EL1  0x00000000400003c5   the state it was in

  x0  0x0000000000000001  x1  0x0000000000000008  ...
```

`FAR_EL1` is the address we tried to touch. `ELR_EL1` is the instruction that touched it, and
`rust-objdump` or GDB will turn it straight into a source line.

## What moves out of `fatal()` next

Every case currently falls into `fatal()`. As the kernel grows, they migrate into real
handlers:

- **Milestone 4:** data aborts become **page faults**, and most become *recoverable* (the page
  was swapped out, or copy-on-write needs to duplicate it).
- **Milestone 5:** IRQ stops being fatal and becomes the timer tick that drives preemption.
- **Milestone 7:** EC `0x15` (`svc`) stops being fatal and becomes the syscall dispatcher.

The plumbing is already there. Those milestones are mostly a matter of adding arms to a
`match`.

---

*Add to this file as new exception classes come up.*
