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

## What was still open, and what closed it

Both of these were open on 2026-08-14 and both are answered below by the guard-page faults of
2026-08-16 and the walker they finally forced into the tree.

- **Per-function frames are not call chains.** `-Z emit-stack-sizes` says what one function costs, not
  which functions stack on top of each other, so it cannot produce a worst-case depth. The watermark
  in notes/stack-high-water.md is the other half. Neither is sufficient alone, and a call-graph
  walker would close the gap. **Nothing in this tree had one until `script/stack-depth-check`.**
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

# The guard-page faults of 2026-08-16, which were not overflows

Two more guard-page faults, one per architecture, in the same test
(`user::supervision_tests::a_faulting_child_reports_to_its_supervisor_and_is_reaped_then_respawned`),
intermittent on both. They read exactly like the 2026-08-14 pair above and they are a different
thing, and the reason the difference took a day to see is that **the kernel's own report asserted
the wrong half of it.**

## What the two faults said

```
*** KERNEL STACK OVERFLOW ***
0xffff0010001b3000 is in THREAD stack slot 87's guard page (thread.rs).
bottom 0xffff0010001b4000, so sp went 4096 bytes past it, on a 16384-byte stack.
ESR_EL1 0x0000000096000047   FAR_EL1 0xffff0010001b3000     (aarch64, run 31920141776)

0xffffffd0001fe008 is in THREAD stack slot 102's guard page (thread.rs).
bottom 0xffffffd0001ff000, so sp went 4088 bytes past it, on a 16384-byte stack.
scause=0xf (code 15)                                        (riscv64, PR #213's cpu matrix, rv64)
```

## The fact that settles it, and the tree was already holding every piece of it

**There are two addresses, not six faults.** Every recorded guard-page fault on this project lands
on one of exactly two, and they were written down in three different files over five days without
anyone putting them side by side:

| when | where recorded | address | slot |
|---|---|---|---|
| 2026-08-11ish, two `cpu matrix` runs | `sched.rs`, `guard_page_at`'s doc | `0xffffffd0001fe000` | riscv64 slot 102 |
| 2026-08-13, one run in five | notes/frames.md, milestone 108's BUGS | `0xffff0010001b3000` | aarch64 slot 87 |
| 2026-08-14 | this file, "the overflows of 2026-08-14" | `0xffffffd0001fe008` | riscv64 slot 102 |
| 2026-08-16, merge queue | above | `0xffff0010001b3000` | aarch64 slot 87 |
| 2026-08-16, PR #213 `cpu matrix` | above | `0xffffffd0001fe008` | riscv64 slot 102 |

Same slot per architecture, every time, over five days, across milestone 124's restructuring of the
entire spawn path (the worst `spawn_on` instantiation went from 4592 bytes to 1040) and across
#157's fix to `reap_region_objects`. The addresses did not move by one byte.

**A depth-driven overflow cannot do that.** Depth is a property of which calls ran and when an
interrupt landed, which this note says in its own words two sections up ("the same kernel image
overflows on one run and not the next"). An overflow's faulting address wanders with the chain that
produced it. These do not wander at all. That is a fixed computation landing on a fixed address, and
the intermittency is in whether the path runs, not in where it ends up.

**And one register in the 2026-08-13 dump argues the same thing, directly under the sentence that
concluded the opposite.** notes/frames.md records `x8 = 0xffff0010001b7a90` and reads it correctly
as slot 87's own stack, "1392 bytes below its top", and then concludes "a 16 KiB kernel stack ran
out". A stack with 1392 bytes used has not run out; it has used 8% of itself. The two sentences are
adjacent and only one of them can be true.

**A depth-driven overflow cannot do that.** Depth is a property of which calls ran and when an
interrupt landed; the note above says so in its own words ("the same kernel image overflows on one
run and not the next"). An overflow's faulting address wanders with the chain that produced it.
These do not wander at all. Two addresses, reproducible to the byte across a change that moved
thousands of bytes of frames, are a **fixed computation landing on a fixed address**, not a stack
pointer arriving somewhere by accumulation.

That also revises the 2026-08-14 entry above. Its aarch64 fault was attributed to
`sched::reap_region_objects`'s 6816-byte frame and closed by #157; the same address came back after
that fix and after milestone 124's. Either the attribution was wrong, or there were two faults at
one address and only one of them was fixed. The frame *was* real and shrinking it *was* right (a
frame larger than the guard page defeats the guard page regardless), so nothing about that work is
wasted. But it did not close this.

## The arithmetic that says it is not depth

**The deepest chain a kernel thread stack can carry is 13792 bytes on aarch64 and 13344 on
riscv64**, measured by `script/stack-depth-check` over the same test binary CI builds:

| | aarch64 | riscv64 |
|---|---|---|
| longest chain from `thread_entry` (a kernel thread's own work) | 9456 | 9168 |
| trap frame the vector builds | 272 | 288 |
| handler chain that can nest on kernel code (no syscall or user-fault arm) | 4064 | 3888 |
| **worst total on a 16384-byte stack** | **13792** | **13344** |
| measured high water, milestone 84's watermark, 31 runs on this machine | 9536 to 10600 | 9344 |

(The aarch64 handler row read 3984 before this work and 4064 after, because the `sp` line added to
`warn_if_guard_page` is itself on the fault path. Eighty bytes to make the report say what it
measures is worth it, and the instrument noticing its own cost is the sort of thing that says it is
measuring the right binary.)

Two things make that close to a bound. The call graph is **acyclic**: no recursion, so the longest
path is the worst case for everything the graph contains. And **no frame over the 4096-byte guard
page is reachable from a thread-stack entry point at all** on either ISA, so milestone 124's fix
does cover every path that reaches a thread, and the frame-jumps-the-guard hazard is genuinely
closed there.

**The third thing is where the measurement lands, and it took two corrections to state properly.**
A first draft said the walker and the watermark agreed to the byte on aarch64 at 9536, which came
from a script still mis-parsing RISC-V local labels. A second said the watermark ran 80 to 176 bytes
*above* the walker's `thread_entry` chain, and blamed the walker's blind spot for assembly frames.
Then 31 aarch64 runs produced 9536 twenty-nine times, 9640 once, and **10600 once**, which is 1144
bytes above that chain and too much for `switch_to`'s 96 bytes to explain.

The second draft was comparing the wrong two numbers. **The watermark measures whatever was on the
stack, nested traps included**; the `thread_entry` chain is only the thread's own work, with no trap
on top. The comparison that means something is against the composed row: a timer interrupt landing
near the deepest point costs a 272-byte trap frame plus a handler, which is exactly the shape of a
1144-byte excursion, and the walker's own model of that is the 13792 in the table. So:

    thread_entry chain   9456    the thread's own work, no trap
    measured watermark   9536 to 10600    what actually happened, traps included
    composed worst       13792   the bound, chain + trap frame + nestable handler

The measurement sits between them, which is where it should sit, and the bound is not contradicted
by anything measured. It is still a **lower** bound in principle, because indirect calls and
assembly frames are invisible to the walker, and the honest reason to trust it here is the size of
the gap it is being used across rather than its precision: 10600 measured against a 20480 the fault
would require.

The riscv64 watermark is one run on this machine, not the 11672 in notes/stack-high-water.md's
table. **That number predates milestone 124**, which took the worst `spawn_on` instantiation from
4592 bytes to 1040, so it describes a kernel that no longer exists.

A fault at a slot's guard-page **base** needs `sp` at 20480 bytes into a 16384-byte stack. That is
6768 bytes deeper than the modelled worst case and 9880 deeper than the deepest thing 31 runs of the
suite ever measured. The walker's imprecision is measured in hundreds of bytes and the gap is
measured in thousands, which is the only reason the imprecision does not matter here.

## What the report was actually entitled to say

`stack::warn_if_guard_page` derived every line from the faulting **address** and then wrote "so sp
went N bytes past it", which is a claim about the stack **pointer** that the function never read.
The two agree only when the fault is `sp` walking off the end.

And the addresses point away from that reading. Both landed at **guard-page offset 0 and 8**, the
far end of the guard, 4096 and 4088 bytes below their stack's bottom. A gradual overflow arrives at
the *near* end, within a few hundred bytes of the bottom. Offset 0 is also, exactly, **one word past
the top of the stack in the slot below**: the slots are contiguous and each slot's guard page is its
first page, so `slot N guard base == slot N-1 stack top`. Six faults, two ISAs, every one within
eight bytes of that boundary.

The handler argues `sp` was mapped, too. On aarch64 the vector's `SAVE_CONTEXT` builds a 272-byte
frame at the live `SP_EL1` before any Rust runs; if `sp` had been inside the unmapped guard, that
store would have faulted again. It printed a full register dump instead. RISC-V's `trap.s` stays on
the interrupted `sp` for an S-mode trap and has the same property. (That argument is not airtight,
and the next subsection is why: aarch64 recovers from a nested vector fault by walking `sp` down
until the frame fits. It is refuted on other grounds.)

So the report now prints `sp` beside the faulting address in the same units and lets the reader
compare, rather than asserting the answer. `kernel/src/stack.rs`, with
`sched::tests::a_slots_guard_page_begins_where_the_slot_below_it_ends` pinning the geometry the
comparison rests on.

### A model that fits the offsets exactly, and why it is still wrong

Worth writing down because it is the reading a careful person reaches next, and because refuting it
costs an hour the second time.

**The two fault offsets are the two ISAs' first trap-frame stores.** aarch64's `SAVE_CONTEXT` opens
`sub sp, sp, #272` then `stp x0, x1, [sp, #16 * 0]`, a store at **sp + 0**. RISC-V's `trap_entry`
opens `addi sp, sp, -288` then `sd x1, 1*8(sp)`, a store at **sp + 8**. The faults are at guard base
**+ 0** and **+ 8**. So: if `sp` were exactly a slot's guard base at trap entry, each ISA's first
store lands precisely where its fault did.

That model even survives the double-fault objection. aarch64 has no double fault; a store fault in
`SAVE_CONTEXT` re-enters the same vector with `sp` another 272 lower, and after one step `sp` is
inside the previous slot's mapped stack, so the frame builds, `exception_dispatch` runs, and
`FAR_EL1` still holds the first failing address. The register dump would be the original context's
(nothing before the store touches `x0`..`x30`), and `SPSR_EL1` would read EL1h with all of `DAIF`
set, which is exactly the `0x3c5` in the aarch64 dump.

**`ELR_EL1` refutes it.** Under that model the reported `ELR` is the PC of the *previous* level's
faulting instruction, which is the `stp` inside the vector table. The dump reads
`0xffff00004013a228`. Two local builds of this tree, with different metadata hashes, both place
`exception_vectors` at `0xffff0000400b4000` and agree instruction for instruction around it, so the
assembly's position is stable across builds and the faulting instruction was ordinary Rust roughly
550 KB further into `.text`. The failing CI build is a different commit and its layout is not
knowable from here, but it is main plus a 59-line markdown pull request, and that does not move half
a megabyte of code.

So the offsets are a coincidence, or they point at some other pair of stores. **Say what the next
occurrence has to print to settle it**: if the new `sp` line names the slot *below* the faulting
address, the store is past a neighbour's top and the trap-entry model is dead for good; if it names
the same slot, `sp` really was in the guard page and this section is back on the table with `ELR` to
explain.

## What it therefore is, and what is still open

**Not settled.** What is settled is what it is *not*: not thread-stack depth, not a frame larger
than the guard page, and not the class milestone 124 closed. What remains, in order of what the
addresses support:

- **A store one or two words past the top of a kernel stack**, from a pointer that treats a stack
  top as inclusive rather than exclusive, or from a stale pointer into a slot whose `KernelStack`
  has been dropped and whose address range went back to `FREE_STACK_VAS`. Every in-tree computation
  from a `KernelStack` was read for this and each is exclusive at the top
  (`paint`/`high_water` over `[bottom, top)`, `spawn_into`'s closure slot and `Context`,
  `arm_for_start`, `enter_frame`'s `top - 272`, `user_pc`), so if this is the shape, the pointer is
  not one of those or it is being used after its stack died.
- **A stray store through a corrupted pointer** that happens to name a slot base. Six faults across
  five days landing on two addresses argues against a random one, and argues for a computation with
  a fixed input.

**And the search space is small, which is the encouraging part.** Only three places in the kernel
can name a slot *base* at all: `KernelStack::new` (which holds it as `base` and `guard`), its `Drop`
(which pushes `self.guard` back to `FREE_STACK_VAS`), and the arithmetic in `stack.rs` that turns an
address into a slot. `grep` for `STACK_AREA`, `STACK_SLOT_SPAN`, `NEXT_STACK_VA` and
`FREE_STACK_VAS` finds nothing else outside tests. Whatever stores there either derives the address
from one of those, or does not know it is a stack address at all.

**The two slot numbers are themselves a clue nobody has spent.** Why 87 on aarch64 and 102 on
riscv64, every time? A slot index is `(va - STACK_AREA) / 0x5000`, so a repeatable index means a
repeatable *count* of stacks handed out before the offending one. Whatever computes the faulting
address is reached at the same point in the same suite on each run, which is another way of saying
this is deterministic in the sequence of allocations and not in the timing. The intermittency then
has to come from something else deciding whether that path runs at all, not from where it lands.

**It did not reproduce here, and that is a result rather than a gap in the effort.** 31 full-suite
aarch64 runs under host load on a 4-CPU Linux box, `-smp 4` under TCG, every one green, plus a green
riscv64 leg. The thread watermark read **9536 twenty-nine times, 9640 once, and 10600 once**, which
is worth recording on its own: the depth is a near-constant with occasional excursions of the size a
nested trap frame explains, and it is nothing like a stack that intermittently runs 4096 bytes past
its own bottom. The reproduction cost is the honest blocker: CI hit it perhaps two runs in six, on a
runner this machine does not resemble. A hunt harness that restores the fixture images and stands in
for xtask's two host actors got the cycle to about 32 seconds, and 32 seconds times zero failures is
still zero information about the cause.

**The next occurrence should be legible without another investigation**, which is what the `sp` line
buys. If it prints a slot *below* the faulting address, the store-past-the-top reading is confirmed
and the search narrows to pointers derived from a stack top. If it prints the same slot, the depth
reading comes back and the walker's bound is wrong somewhere, which would mean an indirect call it
cannot see.

---

*Add to this file as new stack concepts come up.*
