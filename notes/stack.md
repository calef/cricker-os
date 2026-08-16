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
arm64 kernel threads. **It was never measured for this tree**, which turns out to matter.

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

## The fact that settles it, and it was sitting in this file the whole time

**Both addresses are byte-identical to the 2026-08-14 pair recorded above.** Read them side by
side:

| | 2026-08-14 | 2026-08-16 |
|---|---|---|
| aarch64 | `FAR_EL1 0xffff0010001b3000`, slot 87 | `FAR_EL1 0xffff0010001b3000`, slot 87 |
| riscv64 | `0xffffffd0001fe008`, slot 102 | `0xffffffd0001fe008`, slot 102 |

Same slot, same offset into it, same test family, on both architectures, four days and one milestone
apart. Milestone 124 restructured the entire spawn path in between (the worst `spawn_on`
instantiation went from 4592 bytes to 1040) and the addresses did not move by one byte.

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

**The deepest chain a kernel thread stack can carry is 13712 bytes on aarch64 and 13344 on
riscv64**, measured by `script/stack-depth-check` over the same test binary CI builds:

| | aarch64 | riscv64 |
|---|---|---|
| longest chain from `thread_entry` (a kernel thread's own work) | 9456 | 9168 |
| trap frame the vector builds | 272 | 288 |
| handler chain that can nest on kernel code (no syscall or user-fault arm) | 3984 | 3888 |
| **worst total on a 16384-byte stack** | **13712** | **13344** |
| measured high water, same suite, milestone 84's watermark, this machine | 9536 | 9344 |

Two things make that close to a bound. The call graph is **acyclic**: no recursion, so the longest
path is the worst case for everything the graph contains. And **no frame over the 4096-byte guard
page is reachable from a thread-stack entry point at all** on either ISA, so milestone 124's fix
does cover every path that reaches a thread, and the frame-jumps-the-guard hazard is genuinely
closed there.

**The third thing is a correction, and it is the useful one.** The first draft of this section said
the walker and the watermark agreed to the byte on aarch64. They do not. The measured watermark is
**above** the walker's chain on both ISAs, by 80 bytes on aarch64 (9536 against 9456) and 176 on
riscv64 (9344 against 9168), and the aarch64 "9536 = 9536" that read as a perfect match came from an
intermediate run of a script that was still mis-parsing RISC-V local labels.

The direction is the walker's own declared blind spot rather than a surprise: **assembly carries no
`.stack_sizes` entry**, so `switch_to`'s 96-byte frame, `user_entry_trampoline`'s 272-byte trap-frame
reservation, and the closure slot `spawn_into` parks at the stack top are all invisible to it. Those
alone more than cover 80 and 176 bytes. So the number is a **lower** bound with a small measured
bias, not an upper one, and every use of it here is a comparison against a gap of thousands of bytes
rather than tens.

The riscv64 watermark row is this machine's own run, not the 11672 in notes/stack-high-water.md's
table. **That number predates milestone 124**, which took the worst `spawn_on` instantiation from
4592 bytes to 1040, so it describes a kernel that no longer exists.

A fault at a slot's guard-page **base** needs `sp` at 20480 bytes into a 16384-byte stack. That is
6768 bytes deeper than anything the binary can produce, and 10944 deeper than anything a run of the
suite has ever been measured to reach. Neither figure is within a hundred bytes of the other, which
is why the walker's small downward bias does not touch the conclusion.

## What the report was actually entitled to say

`stack::warn_if_guard_page` derived every line from the faulting **address** and then wrote "so sp
went N bytes past it", which is a claim about the stack **pointer** that the function never read.
The two agree only when the fault is `sp` walking off the end.

And the addresses point away from that reading. Both landed at **guard-page offset 0 and 8**, the
far end of the guard, 4096 and 4088 bytes below their stack's bottom. A gradual overflow arrives at
the *near* end, within a few hundred bytes of the bottom. Offset 0 is also, exactly, **one word past
the top of the stack in the slot below**: the slots are contiguous and each slot's guard page is its
first page, so `slot N guard base == slot N-1 stack top`. Two ISAs, two runs, both within eight
bytes of that boundary.

The handler also proves `sp` was mapped. On aarch64 the vector's `SAVE_CONTEXT` builds a 272-byte
frame at the live `SP_EL1` before any Rust runs; if `sp` had been inside the unmapped guard, that
store would have faulted again and the machine would have printed nothing. It printed a full
register dump. RISC-V's `trap.s` stays on the interrupted `sp` for an S-mode trap and has the same
property.

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

So the report now prints `sp` beside the faulting address in the same units and lets the reader
compare, rather than asserting the answer. `kernel/src/stack.rs`, and
`sched::tests::a_slots_guard_page_begins_where_the_slot_below_it_ends` pins the geometry the
comparison rests on.

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
- **A stray store through a corrupted pointer** that happens to name a slot base. Four faults across
  four days landing on two addresses argues against a random one, and argues for a computation with
  a fixed input.

**The two slot numbers are themselves a clue nobody has spent.** Why 87 on aarch64 and 102 on
riscv64, every time? A slot index is `(va - STACK_AREA) / 0x5000`, so a repeatable index means a
repeatable *count* of stacks handed out before the offending one. Whatever computes the faulting
address is reached at the same point in the same suite on each run, which is another way of saying
this is deterministic in the sequence of allocations and not in the timing. The intermittency then
has to come from something else deciding whether that path runs at all, not from where it lands.

**It did not reproduce here.** Full-suite aarch64 runs under host load on a 4-CPU Linux box with
`-smp 4` under TCG, all green, with the thread watermark reading 9536 every single time; the riscv64
leg is green too, at 9344. The reproduction cost is the honest blocker: CI hit it perhaps two runs
in six on a runner this machine does not resemble.

**The next occurrence should be legible without another investigation**, which is what the `sp` line
buys. If it prints a slot *below* the faulting address, the store-past-the-top reading is confirmed
and the search narrows to pointers derived from a stack top. If it prints the same slot, the depth
reading comes back and the walker's bound is wrong somewhere, which would mean an indirect call it
cannot see.

---

*Add to this file as new stack concepts come up.*
