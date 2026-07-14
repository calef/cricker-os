# Vec, Box, String, BTreeMap

The four types the [heap](heap.md) gave back. Each solves a problem the stack cannot.

They all live in the **`alloc` crate** — the middle layer from [no-std.md](no-std.md), between
`core` (needs nothing) and `std` (needs an OS). `extern crate alloc;` pulls it in, and it only
works because we supplied a `#[global_allocator]`.

```
BTreeMap ─┐
String ───┤
Vec ──────┼──▶ #[global_allocator] ──▶ our heap ──▶ our frame allocator ──▶ RAM
Box ──────┘                            (crates/heap)   (crates/frames)     (from the DTB)
```

## `Box<T>` — "this lives on the heap, and I own it"

The simplest, and the foundation for the rest.

```rust
let b = Box::new(42u64);
```

Allocates 8 bytes on the heap, moves the value in, hands you a pointer. Dropping it frees.
**`Box` is `malloc` + `free`, with the `free` proved by the compiler.**

`Box<T>` is exactly one pointer on the stack. Zero overhead.

Three reasons you need it:

**Recursive types cannot be sized.** This is infinite:

```rust
struct Node { value: u64, next: Node }     // size = 8 + (8 + (8 + ...
```

This is 16 bytes:

```rust
struct Node { value: u64, next: Option<Box<Node>> }
```

The indirection is what makes the size finite. Every linked list, tree, and graph in Rust
rests on it.

**Trait objects.** `Box<dyn Write>`: you don't know the concrete type, so you can't know its
size, so it must be behind a pointer.

**Large values you don't want on the stack.** `Box<[Option<Frame>; 1024]>` would have put the
16 KiB array on the heap, and the [milestone 3 incident](stack.md) would never have happened.

## `Vec<T>` — a growable array

Three fields on the stack (24 bytes). The elements are on the heap.

```
stack:            heap:
┌──────────┐      ┌────┬────┬────┬────┬────┬────┐
│ ptr  ────┼─────▶│ 10 │ 20 │ 30 │    │    │    │
│ len   3  │      └────┴────┴────┴────┴────┴────┘
│ cap   6  │       used: 3               spare: 3
└──────────┘
```

`push` writes at `len` and increments. When `len == cap`, it **allocates a bigger buffer
(double), copies everything, and frees the old one.**

**The doubling is the whole trick.** Grow by one each time and N pushes cost O(N²) in copying.
Double each time and the copies are rare enough that the average cost per push is constant:
*amortized O(1)*.

Which is why our `vec_works` test (1000 pushes) is a real workout for the allocator: it
reallocates about ten times, and each one is an allocate, a copy, and a free through code we
wrote.

**`Vec` is why `MAX_REGIONS = 16` existed.** A fixed array must guess its maximum in advance
and fail if it guessed low. `memory.rs` still returns `TooManyRegions` on a machine with more
than 16 memory regions, purely because `Vec` didn't exist when it was written.

## `String` — growable, owned text

Literally a `Vec<u8>` with one extra promise: **the bytes are valid UTF-8.** Same three fields,
same doubling.

The distinction from `&str` is the same distinction as `Vec<T>` vs `&[T]`, and it is one of
the first walls people hit in Rust:

| | Owns the memory? | Can grow? | What it is |
|---|---|---|---|
| `String` | **yes** — heap buffer, freed on drop | yes | ptr + len + capacity |
| `&str` | no — it is a **view** | no | ptr + len |

A `&str` can point at a literal in `.rodata`, into the middle of a `String`'s heap buffer, or
at bytes on the stack. It doesn't care and it doesn't own.

**That's why `&str` works in `no_std` and `String` doesn't: a view needs no allocator.**

`format!` builds a `String`, which is why it only started working at milestone 4.

## `BTreeMap<K, V>` — an ordered map

A **B-tree**: a balanced search tree where each node holds *many* keys, not one.

### Why not `HashMap`?

**`HashMap` is not in `alloc`. It is `std`-only.**

It needs a randomly-seeded hasher (to resist hash-collision denial-of-service attacks), and a
random seed needs entropy from the operating system. **We are the operating system.**

So in a kernel you use `BTreeMap`, and it isn't a compromise: you also get ordered iteration
and range queries for free.

### Why a B-tree and not a binary search tree?

**Cache locality**, and it traces straight back to the memory hierarchy in
[registers.md](registers.md).

A binary tree node holds one key and two pointers. Every step down is a pointer chase, and
every pointer chase is potentially a **~200-cycle trip to RAM**. Twenty levels deep is twenty
cache misses.

Rust's `BTreeMap` packs **up to 11 keys per node**, laid out contiguously. One cache-line
fetch buys eleven comparisons. Far fewer memory round-trips for the same number of elements,
and memory round-trips are the only thing that matters.

Milestone 7 wants one: a process table mapping PID → process.

## What they share

Every one is **"owns some heap memory, frees it when dropped."** That's `Drop`, and it is what
makes the heap safe in Rust at all: the compiler proves the free happens exactly once, at the
right time. See the table in [heap.md](heap.md) — use-after-free, double-free, and leaks are
*heap* problems, and ownership is the answer to all three.

| Type | On the stack | On the heap |
|---|---|---|
| `Box<T>` | 8 bytes (a pointer) | `size_of::<T>()` |
| `Vec<T>` | 24 bytes (ptr, len, cap) | `cap × size_of::<T>()` |
| `String` | 24 bytes (same) | `cap` bytes of UTF-8 |
| `BTreeMap` | a couple of words | the nodes |

---

*Add to this file as new collections come up.*
