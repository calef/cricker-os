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

**We didn't have that when this was written**, and the paragraph is kept in its original tense
because the incident below happened while it was true. We had 64 KiB reserved in a linker script and
nothing below it but more of our own kernel, so blowing the stack silently overwrote `.bss`, then
`.data`, then `.text`, and then executed the corrupted result.

**We have it now.** Milestone 4 put an unmapped page below the boot stack once the MMU was on;
milestone 90 finished the job, so today the boot stack, every per-CPU secondary stack and every
kernel thread stack has one. See "The overflows of 2026-08-14" at the end of this note for what that
buys, what it does not, and the two real overflows that tested it.

---

# The milestone 3 incident

The paragraph above was written during milestone 1 as a hypothetical. It happened during
milestone 3. Recording it in full, because how it was *diagnosed* is more useful than the
bug.

## The symptom

A kernel test hung. Forever. Under a 150-second timeout, it never finished. No panic, no
fault, no output. The last thing printed was the name of the test.

## The bug

```rust
let mut taken = [None; 1024];        // [Option<Frame>; 1024] = 16 KiB
...
for frame in taken.into_iter().flatten() {
    memory::free(frame);
}
```

`into_iter()` on an array **moves it by value**. `flatten()` wraps the result in another
struct, which gets moved again. In a debug build (no optimization, nothing elided) those
copies are all real, and they all land on the stack:

```
  16 KiB   taken
+ 16 KiB   the array moved into core::array::IntoIter
+ 16 KiB   the IntoIter moved into Flatten
--------
  48 KiB   on a 64 KiB stack that already had frames on it
```

`sp` walked below `__stack_bottom`, through `.bss`, through `.data`, and into `.text`. The
kernel then executed its own overwritten code, and hung.

**`into_iter()` on a large array is a real kernel footgun.** Use `iter()` and borrow.

## Three wrong turns, and what actually worked

**Wrong turn 1: "it printed `sp=` and stopped, so it dies inside `println!`."** It didn't.
That was QEMU's *unflushed stdout buffer* being discarded when the timeout killed it. The
output we saw was simply the last thing that made it out of the buffer, not the last thing
that executed. **Never infer a hang location from where output stops** unless you know the
output is unbuffered.

**Wrong turn 2: "the stack is fine."** A probe measured `headroom()` right after declaring
the array and found plenty of room. True, and irrelevant: it measured *before* the three
copies that actually blew it. **A measurement is only as good as where you put it.**

**Wrong turn 3: diagnosing before bisecting.** Two hypotheses were argued from arithmetic
before anyone bisected. Both were wrong.

**What worked:** semihosting exit codes as markers.

```rust
memory::alloc_loop();
semihosting::exit(31);      // do we even get here?
memory::free_loop();
```

Exit code 31 came back. The alloc loop was fine; the free loop was the problem. That single
bit of information was worth more than all the theorizing, and it took two minutes.

**Why exit codes and not prints:** the failing kernel had corrupted `.text`, and
`println!` runs through `core::fmt`, which lives in `.text`. Using the broken thing to
diagnose the broken thing is circular. A semihosting exit is a single `hlt` instruction and
two register writes ([semihosting.md](semihosting.md)). It works when almost nothing else
does.

## What we added

A **canary**: four magic words at `__stack_bottom` (`kernel/src/stack.rs`), checked after
every test, and in the panic handler and the fault handler.

**And it did not catch this bug.** Be clear about that. The overflow destroyed `.text`
before any check could run, so there was no surviving code to notice. The canary catches
the *milder* case, where an overflow dips below the stack, corrupts `.bss`, and returns.
That is worth having, and the after-each-test check pins the blame on the test that did it
rather than on some later victim. But it is a mitigation, not a fix.

**The fix is the guard page at milestone 4.** An unmapped page below `__stack_bottom` means
the MMU faults on the *first* byte written past the end, before any damage. Precise, free
at runtime, impossible to miss. That is the whole reason `link-aarch64.ld` carries a TODO about it.

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
[DECISIONS](../design/decisions/05-preemptive-threads.md) §5). Async tasks are state machines the compiler builds on
the heap, which is why they don't each need a stack, which is why async looked cheaper.
But a real user program is not a state machine we built. It is arbitrary machine code with
an arbitrary call depth, and it needs a real stack.

So **milestone 6 (threads) is really**: allocate a stack per thread, and write assembly
that saves the current register set, swaps `sp`, and restores a different register set.
That is a context switch. It's about thirty instructions, and the stack is the thing being
switched.

---

# The overflows of 2026-08-14

Two real kernel stack overflows in one day, on both architectures, found by CI. Written up from the
ground up, because the mechanism is worth understanding before the incident is, and because the
question "how did we introduce it" turned out to have an uncomfortable answer.

## The shape of a kernel thread stack

Every kernel thread gets **four pages, 16 KiB**, decided when the thread is created, and that is all
it will ever have. `thread::STACK_PAGES` is 4, and the figure came from what Linux uses for its own
arm64 kernel threads. **It was never measured for this tree**, which turns out to matter. *(This
section describes 2026-08-14. The next day's overflow, below, raised `STACK_PAGES` to 6, so the
worked addresses here are the old five-page stride.)*

Directly beneath it sits one unmapped page:

```
0xffffffd0001fe000  +--------------+
                    |  GUARD PAGE  |   unmapped: any access faults
0xffffffd0001ff000  +--------------+  <- stack bottom
                    |              |
                    |   16 KiB     |   frames pile downward from the top
                    |   of stack   |
0xffffffd000203000  +--------------+  <- stack top, where sp starts
```

The per-thread stride is therefore five pages (`0x5000`), guard page first, and that arithmetic is
how a faulting address gets attributed to a stack: subtract `thread::STACK_AREA`, divide by `0x5000`
for the slot, and a remainder under `0x1000` means the guard page.

**Why fixed size at all.** In userspace a stack grows on demand: run off the end and the kernel maps
more. In a kernel there is nobody underneath to do that. The size is the size.

**What the guard page buys, exactly.** Without it, overflow is silent: thread A writes below its own
stack, lands in thread B's, and B crashes later doing something unrelated, arbitrarily far from the
cause. With it, the CPU faults on the first access and the crash is precise. The guard does not
prevent overflow. It converts an invisible failure into a legible one, which is the whole of its
value.

## Why an overflow is intermittent

Depth is a property of the path, not of the binary. A test that spawns a program, faults it, reaps it
and respawns stacks far more frames than one that reads a file. Test ordering, interrupt timing and
which core picked up the work all shift it. So **the same kernel image overflows on one run and not
the next**: milestone 108's branch went four green and one red on a byte-identical binary.

That is also why "re-run it and see" is the wrong instinct. A four-in-five pass rate looks like flake
and is actually a margin that has already run out.

## How it was introduced, which is the uncomfortable part

**No single commit introduced it.** There is no bad change to point at, and the milestone that was
held for hours on suspicion of causing it had a largest new frame of **128 bytes**.

The margin was spent gradually, by ordinary code:

- **`sched::reap_region_objects` carried a 6816-byte frame**, of which 6144 was three arrays sized to
  their table maxima. `let mut doomed_eps = [0u64; MAX_ENDPOINTS]` is 4096 bytes of stack, and it
  reads as a bound rather than as an allocation. `MAX_ENDPOINTS` is 512 because that is a sensible
  ceiling on live endpoints; nothing about that number was ever a claim about stack.
- **`sched::spawn_on` carries 4592 bytes**, because a `Thread` travels by value into the thread table
  and a debug build copies it at each step rather than eliding. It is generic over the spawned
  closure, so every service gets its own instantiation: ten of them, all over the guard page.
- **Milestone 84 had already measured the peak at 11672 of 16384 bytes, 71%**, and written it in a
  table in notes/stack-high-water.md. 4712 bytes of headroom reads as comfortable. Nobody put that
  number next to a 6816-byte frame, and the two had never appeared on the same page.

Put them together and the arithmetic was impossible: **one frame wanted 2104 bytes more than all the
headroom there was.** It needed the right call chain on the right run to expose it, and when it did,
the change that happened to be in flight got the blame.

**Nothing caught it because nothing was looking.** A 6816-byte frame compiles without a warning. The
compiler will hand you a frame larger than the entire stack it will run on, because it has no idea
how big that stack is. No gate in the tree measured frame sizes until this day.

## The two faults, and what each taught

**aarch64**, on milestone 108's branch. `FAR_EL1` was `0xffff0010001b3000`, exactly the guard page of
thread stack slot 87, during the supervision and reap tests. Cause: `reap_region_objects`. Fixed by
rescanning for one endpoint at a time instead of collecting them all first, which took the frame from
6816 to 2560 bytes.

A wrong turn worth keeping: the first decode read `FAR` through `phys_to_virt` and concluded the
pointer was corrupted, because the physical half looked like `0x1b3000` with a stray bit 36. Bit 36
is `STACK_AREA` itself, placed 64 GiB up **precisely so a stack address can never collide with the
virtual name of a physical one**. A high-half address is not automatically a physmap address, and
masking off `KERNEL_VA_BASE` is not a decode until you know which region you are in.

**riscv64**, on the `thead-c906` CPU model, hours later. This one the kernel diagnosed itself, because
milestone 78's `stack::warn_if_guard_page` had merged in between:

```
*** KERNEL STACK OVERFLOW ***
0xffffffd0001fe008 is in THREAD stack slot 102's guard page (thread.rs).
```

The address is the lesson. It is **4088 bytes below the stack bottom, on a 4096-byte guard page**.
Eight more bytes and there would have been no fault at all, just a corrupted neighbour.

## The rule that came out of it

**A frame larger than the guard page defeats the guard page.** One page is 4096 bytes; a function
whose frame exceeds that can move `sp` from inside the stack to below the guard in a single step,
touching nothing in between. No access lands in the guard, so nothing faults, and the write goes into
the neighbouring thread's stack. The mechanism that makes overflow legible is bypassed entirely.

`script/stack-frame-check` gates exactly this, at 4096 rather than at any fraction of the stack, and
the first version of that gate got it wrong by picking a third of the stack instead. Ten `spawn_on`
instantiations sit over the line today, held at their current size by a ratchet until milestone 124
restructures them.

**Growing the stack does not fix this shape**, which is the counterintuitive part. `STACK_PAGES` 4 to
8 buys headroom, but **the guard page stays one page**, so an oversized frame still steps over it.
Growing the stack moves the overflow further away while leaving it silent when it finally arrives.
Shrinking the frame is what restores the fault.

## What is still open

- **Per-function frames are not call chains.** `-Z emit-stack-sizes` says what one function costs, not
  which functions stack on top of each other, so it cannot produce a worst-case depth. The watermark
  in notes/stack-high-water.md is the other half. Neither is sufficient alone, and a call-graph
  walker would close the gap. Nothing in this tree has one.
- **The riscv64 overflow is not proven fixed.** The aarch64 cause was found and fixed; the riscv64
  fault is a different chain on a different slot, and milestone 124 is the prime suspect rather than
  a demonstrated cause. §19 parity says a fix that works on one architecture and silently not the
  other is the bug.

# The overflow of 2026-08-15, which was a different shape

One day after the section above was written, CI overflowed a thread stack again, on both ISAs, in
the same test neighbourhood: aarch64 run 31907966383 attempt 1 (slot 87, `ESR` `0x96000047`,
`ELR` `0xffff000040130214`) and riscv64 `thead-c906` run 31910308865 attempt 1 (slot 102,
`scause` `0xf`, `sepc` `0xffffffc080257aec`), both during
`supervision_tests::a_faulting_child_reports_to_its_supervisor_and_is_reaped_then_respawned`, both
immediately after the user-fault kill report, both on loaded 2-core runners, and both with `sp`
exactly 4096 bytes past the bottom of the 16 KiB stack. Later attempts of the same runs passed.
`reap_region_objects` was already fixed and `script/stack-frame-check` was already gating at the
guard page, so this was not a recurrence of the 2026-08-14 mechanism.

## Symbolizing it, and what the addresses said

The CI binaries were debug builds at known SHAs, so the honest move was to rebuild them exactly.
A local rebuild at the same SHA did NOT reproduce CI's text layout (two tries, two different
layouts: incremental compilation and something host-specific both move functions). What worked was
an `ubuntu:24.04` arm64 container with the repo mounted at CI's own path (`/home/runner/work/nife/
nife`), `CARGO_INCREMENTAL=0`, and the pinned toolchain: cargo then reproduced CI's artifact hash
(`kernel-7f83536acfad25b4`, `kernel-04da2562c61a7429`), which makes the symbolization exact rather
than plausible.

On aarch64 the two addresses cohere into a story:

- `ELR_EL1` = `exception_vectors + 0x214`, which is the fifth `stp` of the **same-EL synchronous
  entry stub** pushing its 0x110-byte frame. The faulting store is the exception entry itself.
- `FAR_EL1` = the guard page's lowest byte, with the fault mid-guard: the entry stub had already
  cascaded. A store into the guard raises a same-EL sync abort, whose entry pushes another frame
  0x110 lower, which faults again, and so on down the guard page; the dump we finally get is the
  cascade's last inner fault, printed once a frame lands whole in the mapped page below the guard.
- `x30` = `IrqSafeMutex<Option<Scheduler>>::lock + 0x190`, the instruction after that function's
  `bl spin_loop_hint`, a value that is only live inside the **contended spin** of `SCHED.lock`.
  The thread that died
  was spinning for the scheduler lock with interrupts already masked, within ~272 bytes of its
  stack bottom, and the deepest call of the spin loop is what first touched the guard.

The riscv64 report agrees on everything measurable (`stval` = guard base, same test, same moment)
except that its `sepc` points at an `auipc` in a test-runner print, an instruction that cannot
raise a store fault. `thead-c906` under QEMU is the model whose timing already surfaced one
unrelated flake that day; treat a c906 `sepc` in this failure class as approximate and lean on
`stval`.

## The arithmetic, which is the actual cause

No single frame was over the guard page; the gate is green on the failing SHAs. The stack was
consumed by an honest sum (aarch64 debug numbers, from `-Z emit-stack-sizes`):

| layer | cost |
|---|---|
| deepest standing path the suite reaches on a thread stack | ~11.7 KiB (the measured high-water) |
| blocking from that depth: `ipc_recv` 656 + `SCHED.lock` 256 + `schedule` 448 + the switch | ~1.4 KiB, resident while blocked |
| one preemption at the deepest instant: trap frame 272 + dispatch + GIC claim + `canary::check` 592 + `schedule` 448 + contended `SCHED.lock` 256 + spin | ~2.3 KiB |

Total ~15.5 KiB against 16384 bytes, and the load correlation falls out of the last row: QEMU's
timer runs on host wall clock, so a loaded host delivers many more timer interrupts per guest
instruction, and one of them eventually lands on the deepest frame of the deepest thread, with the
scheduler lock contended by the other core's death-report work, which is why the fault sits right
after the kill report. Every layer is doing its job; the budget was simply spent.

## Why growing the stack IS the fix for this shape

The section above says "growing the stack does not fix this shape", and both sentences are right
because the shapes differ. That rule is about a **single frame bigger than the guard page**, which
steps over the guard and corrupts silently; growing the stack leaves the silent step-over silent,
and shrinking the frame is the fix. Here every frame is modest, the guard page **fired exactly as
designed**, and the failure is the sum. For a sum, the levers are shrink the chain, bound the
chain, or grow the budget:

- **Shrunk**: `sched::canary::check` reserved its whole 592-byte frame before its disarmed early
  return, on every tick, on every thread stack. It is now a ~16-byte armed-check wrapper around an
  outlined `#[inline(never)]` body (a debug prologue reserves the whole frame no matter how early
  the return; an early `return` is not an early frame).
- **Grown**: `thread::STACK_PAGES` 4 to 6 (24 KiB), sized against the sum above with ~8 KiB over
  the measured worst case, and the thread high-water tripwire moved from 14336 to 18432 so it
  alarms ~3 KiB past the measured worst-case stacking and 6 KiB before the guard. The old limit
  could pass a run whose true worst case was already past the stack, which these two CI runs
  proved by example.
- **Not yet bounded**: the structural fix is a per-CPU interrupt stack, so a preemption stops
  billing the interrupted thread ~2.3 KiB at its deepest instant. That is trap-entry surgery on
  both ISAs and wants a lane of its own; until then the preemption cost is part of every thread's
  budget, and the high-water margin has to carry it.

**What the enlargement cost elsewhere, which took three suite runs to find.** Thread stacks come
from the kmem carve, not the frame allocator, and 6 pages x `MAX_THREADS` is 768 pages, the whole
768-page carve; the carve grew to 1024, which in turn took the last spare megabyte of the 128 MiB
test machine, so the machine grew to 256 MiB (both runners, and memory.rs's RAM assert moves with
them). Both exhaustions surfaced the same way: an unrelated test's spawn failing late in the
aarch64 suite with a message that ORed two causes. The refusal sites in `kmem::page` and the shell
wiring now print which budget said no.

**The repeated fault address is a signature, not a coincidence.** Every guard-page fault in this
family lands on the guard page's base (aarch64) or base and base+8 (riscv64), across days and
across fixes, and that is arithmetic rather than evidence of one recurring caller: `sp` is 16-byte
aligned, the entry stub's stores walk upward from `sp`, and the cascade only ends once a frame
clears the page-aligned guard base, so the terminal faulting store is always the first aligned
address at or above the base. aarch64's 16-byte `stp`s give exactly the base; riscv64's 8-byte
`sd`s give base or base+8, which are precisely the two values ever observed. A depth-driven
overflow through the entry-stub cascade therefore DOES repeat an address, exactly this one; do not
read address stability as proof of a single fixed-site writer.

The overflow report also got the instrument this diagnosis lacked: on a thread-guard fault,
`stack::warn_if_guard_page` now prints every word of the dead stack that points into `.text`,
deepest first. This kernel keeps no frame pointers, so that conservative scan is the only
backtrace it can produce, and it turns the next CI-only report into a symbolizable chain instead
of a container-rebuild archaeology project.

---

*Add to this file as new stack concepts come up.*
