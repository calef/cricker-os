# Untyped memory: the kernel stops allocating

Milestone 11, and DECISIONS.md §10's deliberately-deferred third axis. It is the strangest idea in
the whole project, and the one that makes seL4 verifiable: **a kernel that does not own a pool of
memory to hand out.**

## The idea

Normally a kernel has an allocator. Ask it for a page and it finds a free one and gives it to you.
Which means a process, by asking enough, can make the kernel allocate until it runs out, and a
kernel out of memory is a dead machine. Kernel-memory exhaustion is a whole class of attack:
fork bombs, fd floods, socket buffers, every one of them is "make the kernel allocate on my
behalf until it can't."

Untyped memory removes the allocator from the hot path. A process holds a capability to a chunk of
raw physical memory (an **untyped region**), and to get a page it **retypes** part of that memory
into the page. The kernel is a bookkeeper: it advances a watermark and hands back a physical
address. It does not choose a page from a pool it owns, because it owns no pool. Every page a
process spends comes from the untyped it was handed.

## The one number that proves it

```
  milestone 11: a process mapped 23 pages out of an untyped it was handed,
  and the kernel's used-frame count went 991 -> 991 (it did not move).
```

That flat frame count is the whole thing. The process allocated twenty-three pages of memory, and
the kernel's free memory **did not change**, because the pages came out of the process's own
untyped, carved once at the start. A process cannot make the kernel allocate, so it cannot exhaust
kernel memory. It runs out of *its own* budget, the retype returns `OutOfMemory`, and the kernel is
untouched. There is a test that asserts exactly this equality, and it fails loudly if the memory
comes from anywhere but the untyped (with the kernel allocator as the source, the same process maps
thirty thousand pages and the kernel loses thirty thousand frames).

## How it works here

- `Object::Untyped(region)` is a capability to a region: a run of physical pages and a bump
  watermark. `kernel/src/untyped.rs` is the whole allocator: `retype_page` advances the watermark
  and returns the next page, zeroed, or `None` when the region is spent.
- `invoke(untyped, MAP, va)` retypes one page and maps it, writable, at `va` in the caller's own
  address space. **Both the page and any page tables it needs come from the untyped** (the mapper's
  only source of memory is a closure that bumps the watermark), so the kernel allocates nothing.
- The region's backing is carved from the frame allocator **once**, when the untyped is created.
  That single allocation is the seL4 boundary, where all free RAM becomes untyped handed to the
  first process. Everything after spends it.

## What this is, and what it is not

This converts **a process's memory** (its pages and their page tables) to untyped, and demonstrates
the property with a hard number. It is honest to be equally clear about the boundary: the kernel's
*own* objects still come from the kernel heap. `Thread` structs, the scheduler's `BTreeMap` and
`VecDeque`, endpoints, capability tables, thread stacks: all still allocated the old way.

Converting each of those is the **same retype mechanism applied to a kernel object** rather than a
page. In seL4 you retype untyped into a TCB, into a CNode, into an endpoint, into a page table, and
the kernel genuinely has no heap at all. That is the long tail, the part seL4 spent years and a
proof on, and it is what "the allocators leave" in the milestone table ultimately means. What
milestone 11 establishes is the mechanism and the property, for the memory a process spends, which
is the load-bearing idea. The rest is the same move, object type by object type.

## What is deliberately not here

**Revocation.** In seL4, destroying an untyped invalidates every object ever retyped out of it,
tracked by a capability derivation tree. We free a region as a whole and do not chase down the
capabilities derived from it, because nothing in the current system outlives its untyped in a way
that would matter. Revocation and the derivation tree are the natural next depth, and they are
where the model gets genuinely hard.
