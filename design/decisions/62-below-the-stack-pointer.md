# 62. Nothing of yours lives below the live stack pointer

**Status: DECIDED.**

Milestone 71 spent a day on a bug whose whole content is one sentence: **an object parked below the
live `sp` is not yours.** Everything under `sp` belongs to a callee's frame, or to the trap vector,
which subtracts 288 bytes on RISC-V and 272 on aarch64 to build its own frame the instant an
interrupt arrives.

The kernel placed a user thread's `TrapFrame` there on purpose:

```rust
let slot = (crate::arch::current_sp().min(top) - size_of::<TrapFrame>() as u64) & !15;
```

and the comment above it explained why, convincingly: RISC-V's TCB entry path is shallow, so a frame
at the stack top would have overlapped the placing function's own frame. Both halves of that
reasoning were right. The conclusion was still wrong, because "below `sp`" is not a free region, it
is the region with the most contention in the kernel.

**What made it hard to see is worth more than the rule.** `current_sp()` is a real call at
opt-level 0 with a 16-byte frame, so it returned `sp - 16` and the user frame landed 16 bytes below
where the trap frame would be built. Sixteen bytes is two register slots, so every field read as a
different field of the trap frame:

| user field | aliases | reads as |
|---|---|---|
| `x[2]`, the user `sp` | trap `x[0]` | **literally 0**, because `trap_entry` does `sd zero, 0*8(sp)` |
| `sepc` | trap `x[30]` | `t5`, which is zero only sometimes |
| `sstatus` | trap `scause` | `UXL = 0`, an illegal U-mode XLEN |

That table is why the fault presented as `user sp 0x0000000000000000` **exactly** rather than as
garbage, and it is why the guard added to catch it saw only a third of the cases: it tested `sepc`,
which aliased a register that happened to be nonzero most of the time. **A guard on one field of a
corrupted structure reports a fraction of the corruption**, and its silence is not evidence.

## The rule

A structure the hardware will return to lives **above** the live `sp`, at a fixed offset from the
stack top, and the space is reserved so no frame can be built over it. Fixed, because an address
computed from `sp` moves with call depth and with optimization level, which turns a placement bug
into an intermittent one that reproduces on CI and never locally.

The corollary, learned twice on the way: **instrumentation near a stack boundary must be call-free.**
The first two probes written to reproduce this were `spin_loop()` and `write_volatile`, both real
calls at opt-level 0, and both reproduced their own frames clobbering the trap frame rather than the
defect under test. Check the disassembly for `jalr` in the hot path before trusting a probe.
