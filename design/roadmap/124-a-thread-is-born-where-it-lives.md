# 124. A thread is born where it lives: the spawn path's copies

**Status: BUILT** 2026-08-14. Minted the same day by Chris, out of the riscv64 stack overflow
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

## What the work is

A `Thread` is built in `Thread::spawn`'s frame, returned by value, and stored by
`Table::insert_with`, which is `self.slots[slot] = Some(f(name))`. The closure constructs it, returns
it, it is wrapped in `Some`, and then it is stored. A debug build elides none of that, and a debug
build is what CI runs.

The fix is to let the closure construct **into** the destination rather than hand a value back:

```rust
// crates/slots
pub fn insert_in_place(&mut self, f: impl FnOnce(u64, &mut Option<T>)) -> Option<u64>

// kernel/src/thread.rs
fn spawn_into<F: FnOnce() + Send + 'static>(f: F, tid: Tid, dst: &mut Option<Thread>) -> bool
```

`insert_in_place` has to decide what a closure that declines to fill the slot means, and say so:
returning `None` without bumping `live` is the shape that matches today's failure path, where
`Thread::spawn` can fail on `KernelStack::new`.

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

## What "done" means

Every `spawn_on` instantiation under 4096 bytes on both ISAs, `script/stack-frame-check`'s `RATCHET`
entry for `kernel::sched::spawn_on::<` deleted rather than lowered, and the `slots` harnesses read
and updated with the new method. The kernel suite is the check that matters: this is the spawn path,
and every test that starts a program exercises it.

## Prior art

**A design to copy:** seL4 retypes a TCB out of untyped memory *in place*, at an address the caller
names, so there is no "construct then move" step for the same reason this milestone exists. The
object is born where it lives.

**A mistake to avoid:** treating this as a debug-build artifact worth ignoring because release builds
elide the copies. CI runs debug, the guard-page fault is real in debug, and a demonstrator whose
proofs and tests run in a configuration nobody checks is not demonstrating.

## BUGS

- **`crates/slots` is Kani-verified**, and `a_deleted_capability_stays_deleted` and
  `delete_touches_only_its_slot` reason over this table. A new insert path is a new way for a slot to
  become occupied, so the harnesses have to be read against it rather than assumed to still cover it.
  That is the main cost of this milestone and the reason it is not a ten-minute change.
- **`-Z emit-stack-sizes` measures frames, not call chains**, so "every instantiation under 4096"
  bounds one frame and not the depth of the path it sits in. The watermark in
  notes/stack-high-water.md is the other half, and neither is sufficient alone.
- **The ratchet holds the line meanwhile**, so this is not urgent in the sense of a regression: the
  ten frames cannot grow. It is urgent in the sense that a frame over the guard page turns a caught
  overflow into a silent one, and that is the failure this tree can least afford to ship.
- **A second overflow on riscv64 is unexplained.** #157 fixed the aarch64 one by shrinking
  `reap_region_objects`; the riscv64 fault is a different chain on a different slot and this
  milestone is the prime suspect rather than a proven cause. If fixing `spawn_on` does not stop it,
  the call-graph walker is the next instrument and nothing in the tree has one.
