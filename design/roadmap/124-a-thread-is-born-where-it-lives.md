# 124. A thread is born where it lives: the spawn path's copies

**Status: BUILT** 2026-08-14. Minted the same day by calef, out of the riscv64 stack overflow
milestone 108 was held on. The hold turned out to be the wrong suspect twice over, and this is what
was underneath.

**The worst `spawn_on` instantiation went from 4592 bytes to 1040**, every one of them now clears the
4096-byte guard page on its own merits, and `script/stack-frame-check`'s ratchet is deleted rather
than lowered.

**Two predictions in this block were wrong, and the corrections are the useful part.** It said the
fix needed `insert_in_place` on `crates/slots` and that reading the Kani harnesses alongside it was
the main cost. **Neither was true**: the table stores a `TcbPtr`, not a `Thread`, because a TCB lives
on its own page. `crates/slots` was never touched and no harness moved. The copies were all between
`Thread::spawn`'s frame and `ptr.write`, so the fix is `Thread::spawn_into` writing through a pointer
the caller already holds, plus `Threads::insert_in_place` to hand that pointer down.

## The number, and why it is the interesting kind of number

**`sched::spawn_on` carries a 4592-byte frame, and the guard page under every kernel thread stack is
4096 bytes.** It is generic over the spawned closure, so every service that spawns gets its own
instantiation: ten of them, 3888 to 4592 bytes, over the guard page on **both ISAs**, measured with
`script/stack-frame-check`.

A frame larger than the guard page is not merely close to overflowing. It can move `sp` from inside
the stack to below the guard **in a single step**, touching nothing in between, so the guard never
faults and the write lands in the neighbouring thread's stack instead. The overflow stops being a
legible fault and becomes corruption that surfaces somewhere else entirely, arbitrarily later.

That is not hypothetical here. On 2026-08-14 a `thead-c906` run faulted **4088 bytes below the stack
bottom** on a 4096-byte guard. Eight more bytes and there would have been no fault at all.

## Where the bytes are, measured rather than guessed

| symbol | frame |
|---|---|
| `sched::spawn_on::<fs_service::spawn_fs_server>` | 4592 |
| `sched::spawn_on::<compositor_service::start>` | 4576 |
| ... eight more instantiations ... | 3888 to 4560 |
| `Thread::spawn::<fs_service::spawn_fs_server>` | 2720 |
| `capability::CSpace<cap::Object, 16>`, one field of `Thread` | 384 bytes of type |

The per-instantiation spread is only about 700 bytes and tracks `size_of::<F>()`, so **the closure is
the small part**. Roughly 3900 bytes is constant, and it is the `Thread` travelling by value.

## What the work was

**This section proposed the wrong fix, and what replaced it is worth reading.** The proposal was
`insert_in_place` on `crates/slots`, on the belief that `Table::insert_with` stored the `Thread`:
`self.slots[slot] = Some(f(name))`, so the closure builds it, returns it, it is wrapped in `Some`,
and then stored.

**The table does not store a `Thread`. It stores a `TcbPtr`**, because a TCB lives on its own page,
and `Threads::insert_at` is where the value actually lands:

```rust
let ptr = crate::arch::mmu::phys_to_virt(page) as *mut Thread;
self.table.insert_with(|tid| { unsafe { ptr.write(f(tid)) }; TcbPtr(ptr) })
```

So `crates/slots` was never touched. The copies were all between `Thread::spawn`'s frame and that
`ptr.write`: build a `Thread`, return it by value, hold it in `spawn_on`, move it into a closure,
return it from the closure, write it. Five hops, each a real memcpy in a debug build, and a debug
build is what CI runs.

What shipped instead:

```rust
// kernel/src/thread.rs
pub unsafe fn spawn_into<F: FnOnce() + Send + 'static>(f: F, id: Tid, dst: *mut Thread) -> bool

// kernel/src/sched.rs
fn insert_in_place(&mut self, build: impl FnOnce(Tid, *mut Thread) -> bool) -> Option<Tid>
```

`Thread::spawn` survives as a thin `MaybeUninit` wrapper over `spawn_into`, because `sched::init`'s
idle thread and `spawn_blocked` hold no TCB page when they build. They keep the copies, and nothing
on their paths is near a guard page.

The decline path is the part to read twice. `insert_at_in_place` mints the name before `build` runs,
so a build that fails has to give it back, and `Table::remove` is the right primitive rather than a
leak: the slot holds a `TcbPtr`, and dropping that drops a pointer. The `Thread` drop lives in
`Threads::remove`, which this path never reaches because no `Thread` was ever constructed.

## A hypothesis that was measured and refuted, kept so nobody repeats it

The first proposal was that the closure's opening `let mut thread = thread;` cost a whole `Thread`
copy, and that deleting the rebind would fix it. **It changes nothing.** Removing it and rebuilding
produced byte-identical frames across all ten instantiations (4592 to 4592, 4576 to 4576, and so on):
the compiler already elides that rebind. The copies are in the value-passing chain, not in the
rebinding, which is why this milestone proposes an API change rather than an edit.

## Why this is not just a stack-size question

Raising `STACK_PAGES` from 4 to 8 would buy headroom and is one number. It is the wrong lever here
for a reason specific to this shape: **the guard page stays one page** no matter how large the stack
is, so a 4592-byte frame can still step over it. Growing the stack moves the overflow further away
without restoring the mechanism that makes an overflow *legible*. Shrinking the frame below 4096
restores it.

Growing the stack is still worth considering on its own merits, and the two are independent.

## What "done" meant, and what it measured

Every `spawn_on` instantiation under 4096 bytes on both ISAs, and the `RATCHET` entry deleted rather
than lowered. Both hold: the worst went 4592 to 1040, and the gate now reports "no ratchet" on
aarch64 and riscv64. The `slots` harnesses needed no reading, for the reason above.

**The independent confirmation is the better evidence.** The icount tripwire failed this branch on an
*improvement*: `spawn_reap` moved 154725 against a 173742 baseline, 10.9% fewer instructions, and
12.9% on riscv64. A benchmark with no idea what changed measured the memcpy that is no longer there.
Both baselines were updated from CI's own numbers rather than computed, and the riscv64 case earned
that discipline: scaling the aarch64 ratio would have written 25382 against a real 24835, which is
both wrong and still outside the tolerance band.

## Prior art

**A design to copy:** seL4 retypes a TCB out of untyped memory *in place*, at an address the caller
names, so there is no "construct then move" step for the same reason this milestone exists. The
object is born where it lives.

**A mistake to avoid:** treating this as a debug-build artifact worth ignoring because release builds
elide the copies. CI runs debug, the guard-page fault is real in debug, and a demonstrator whose
proofs and tests run in a configuration nobody checks is not demonstrating.

## BUGS

- **The prediction that `crates/slots` was the main cost was wrong**, and it is kept here because it
  is the plausible reading of `Table::insert_with` and the next person will make it too. The table
  stores a `TcbPtr`; the `Thread` is on its own page. No harness moved.
- **`Thread::spawn` still carries the copies** for `sched::init`'s idle thread and `spawn_blocked`,
  which hold no page when they build. Nothing on those paths is near a guard page today, and if one
  ever is, this is where to look first.
- **`-Z emit-stack-sizes` measures frames, not call chains**, so "every instantiation under 4096"
  bounds one frame and not the depth of the path it sits in. The watermark in
  notes/stack-high-water.md is the other half, and neither is sufficient alone.
- **The ratchet is gone**, so nothing now holds these frames except the gate's own 4096-byte ceiling,
  which is the state a ratchet exists to reach and then be deleted from.
- **The riscv64 overflow is still unexplained, and this milestone did not prove it fixed.** #157
  fixed the aarch64 one by shrinking `reap_region_objects`; the riscv64 fault was a different chain on
  a different slot, and this was the prime suspect rather than a demonstrated cause. What closed here
  is the separate hazard that ten frames could step over the guard entirely. **If it recurs, the
  call-graph walker is the next instrument and nothing in the tree has one**, which is now the answer
  to "why did we not catch this" twice over.
