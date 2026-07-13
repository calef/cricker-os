# The heap

## Why the stack isn't enough

The stack works because **function lifetimes nest** ([stack.md](stack.md)). If `foo` calls
`bar`, `bar` finishes first. Always. That strict LIFO ordering is what lets the stack be a
single moving pointer, with `sub sp, sp, #32` as its entire allocation algorithm.

Now consider:

```rust
fn read_config() -> Vec<String> { ... }
```

That `Vec`'s buffer must **outlive the function that created it**. It cannot live on the
stack, because the frame is gone the instant `read_config` returns. Its lifetime doesn't nest
with anything.

**The heap is memory you can allocate at any time, in any size, and free at any time, in any
order.**

That last clause, *in any order*, is where all the difficulty comes from. The stack gets to
be one pointer because it may assume LIFO. The heap may assume nothing.

## What that costs

| | Stack | Heap |
|---|---|---|
| Allocate | `sub sp, sp, #32`. One instruction. | **Search** a free list. Maybe split a block. |
| Free | `add sp, sp, #32`. One instruction. | Insert back. Merge with neighbours. |
| Fragmentation | impossible | **the permanent enemy** |
| Forgetting to free | impossible | a leak |
| Use-after-free | impossible | a security bug |

`malloc` is roughly a hundred times slower than a stack push. That isn't sloppiness; it's the
price of dropping the LIFO assumption.

**Fragmentation is the one that really bites.** Allocate and free in a loop and you can end up
with thousands of tiny free blocks, none big enough for a 32-byte request, while the "free
memory" counter reports megabytes. The heap has failed *while claiming to be fine*.

## Rust's whole thesis, restated

Look at that table again. Use-after-free, double-free, and leaks are **heap** problems. None
of them exist on the stack.

**Ownership, `Box`, `Drop`, and lifetimes are the compiler proving you free the heap exactly
once, at the right moment.** In a real sense the borrow checker is a heap-correctness checker,
which is why `no_std` feels so strange: take away the heap and half of Rust's reason for
existing goes quiet.

## Two allocators, and why they're different

| | `frames` (milestone 3) | `heap` (milestone 4) |
|---|---|---|
| Hands out | fixed 4 KiB pages | arbitrary sizes, arbitrary alignment |
| Metadata | a **bitmap, outside** the memory | **inside the free blocks themselves** |
| Why | a page might go to a device for DMA, or to userspace. **You cannot store bookkeeping inside memory you are giving away.** | a free block is by definition space nobody is using. Storing the list node *in* it costs zero overhead for allocated memory. |

And they stack:

```
Vec, Box, String, BTreeMap
        |  #[global_allocator]
   kernel heap        arbitrary sizes, free list, coalescing
        |
  frame allocator     fixed 4 KiB pages, bitmap
        |
   physical RAM       read out of the device tree
```

The heap asks the frame allocator for 256 contiguous pages and carves them up. **This is the
first real use of `alloc_contiguous`**, and it is why `frames` is a bitmap and not a free
list: a free list could not have answered the request. See
[physical-memory.md](physical-memory.md).

## The two things that make ours correct

**Everything is 16-byte aligned, in both address and size.**

That single invariant is what makes splitting always work. Any gap left over from a split is a
multiple of 16, so it is either exactly zero or big enough to hold a free-block header
(`size_of::<Block>()` is 16). Without it you get slivers too small to track, and you leak them
one at a time until the heap dies.

**The free list is sorted by address, and `free` coalesces with both neighbours.**

Sorted by address so that "the block before" and "the block after" are the only two to check,
rather than all of them. And merging *forward first, then backward*, so that freeing a block
between two free neighbours collapses all three in one pass.

There is a test (`thrashing_does_not_fragment_the_heap_to_death`) that allocates and frees
2000 times in a churning pattern and then asks for nearly the entire arena. It only passes if
the heap is still one block.

## The alignment gap, which is where naive implementations leak

Ask for 4096-byte alignment inside a block that doesn't start at a 4096 boundary, and you must
step forward to the aligned address. **The bytes you stepped over do not disappear.**

The obvious implementation aligns forward and forgets. It then leaks a few hundred bytes per
aligned allocation, and the heap slowly dies over hours, in a way no single test catches. Ours
puts the front gap back on the list, and `a_large_alignment_does_not_leak_the_gap_before_it`
is there to keep it that way.

## What it unblocks

Every list in the kernel was a **fixed-size array** (`MAX_REGIONS = 16`) purely because there
was no heap. `memory.rs` still declares `[Region; 16]` and returns `TooManyRegions` if a
machine has more, a limitation accepted only because `Vec` didn't exist.

Ahead of us, everything is "an unknown number of things, sized only at runtime":

| Milestone | Wants |
|---|---|
| 6 | a thread structure per thread |
| 7 | a process table, and page tables per process |
| 8 | a cache of filesystem inodes |

## The promise from milestone 1, now kept

[no-std.md](no-std.md), written before a single line of kernel existed:

> At milestone 4 we write a `#[global_allocator]`, add `extern crate alloc;`, and `Vec` starts
> working. **Not because we imported it. Because we built the heap it needed.**

Nothing was imported. Every link in the chain is ours.

*(Unrelated aside, because the collision genuinely confuses people: "the heap" here has nothing
to do with the heap **data structure**, the binary tree used for priority queues. Same word,
different thing.)*

---

*Add to this file as new allocator concepts come up.*
