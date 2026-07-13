# Physical memory

The bottom of the memory hierarchy. Page tables (milestone 4), the kernel heap, DMA buffers
(milestone 8), and user process pages (milestone 7) all ask this for memory, and there is
nothing underneath it to ask.

## Two questions

1. **Where is the RAM?** Answered by the [device tree](device-tree.md), which the machine
   hands us in `x0` ([boot-protocol.md](boot-protocol.md)).
2. **Which parts of it are ours to give away?** That's this note.

## Bitmap, not free list

The classic hobby-OS answer is a **free list**: link each free page to the next by writing a
pointer *into the free page itself*. Zero metadata overhead, O(1) alloc and free, genuinely
elegant. It's what xv6 does.

We use a **bitmap** (one bit per frame, `1` = used). Two reasons:

**Contiguity.** A free list cannot answer "give me 8 physically *adjacent* frames." A device
doing DMA reads physical addresses directly, with no MMU in the way to hide a scattered
buffer, so virtio at milestone 8 needs genuinely contiguous memory. Retrofitting that means
throwing the free list away.

**Testability.** A free list stores its metadata *inside the memory it manages*, so testing
it means handing it real memory and doing unsafe pointer writes. A bitmap's logic is **pure**:
given a bitmap and a request, which frame? We test it exhaustively on the host with no memory
at all, in milliseconds, with no emulator (DECISIONS.md §7).

The cost is 1 bit per frame: **32 KiB of bitmap per GiB of RAM**. Or, for QEMU's 128 MiB:
4 KiB. One frame of overhead to manage 32768 frames. Cheap.

## The bootstrap problem

**The allocator needs memory for its bitmap. There is no allocator.**

The way out: carve the bitmap, by hand, out of the very memory it is about to manage. We know
where the kernel image ends (the linker told us via `__image_end`), so the bitmap goes
immediately after it, and then the allocator reserves those frames *from itself*.

**The allocator's first act is to allocate itself.**

## Guilty until proven innocent

`FrameAllocator::new` marks **every frame used**. Nothing is handed out until someone
explicitly says "this address range is real RAM."

This ordering is not fussiness. If the bitmap defaulted to *free*, the allocator would
cheerfully hand out the MMIO hole at `0x0900_0000` and give you the UART's control registers
as scratch space. Default-used means an unknown address is never allocated, and the worst
case is wasted memory rather than a machine that writes garbage to a device.

So `init` goes:

```
1. everything used
2. mark_free()  each /memory region from the device tree
3. mark_used()  the kernel image      (__image_start .. __image_end)
4. mark_used()  the bitmap itself
5. mark_used()  the device tree blob  (it's sitting in the RAM we just freed!)
6. mark_used()  every /memreserve region the firmware declared
```

Free first, reserve second. **Reserving has to win.**

## The rounding asymmetry, which is the whole ballgame

`mark_used` rounds the start **down** and the end **up**. It claims every frame the region so
much as *touches*.

Consider: our image ends at `0x4009_7010`. That is not frame-aligned. The frame at
`0x4009_7000` holds the last `0x10` bytes of the kernel, and 4080 bytes of nothing.

Round that end **down** and the frame is declared free. The allocator hands it out. Something
writes to it. **The tail of the kernel is silently overwritten**, and the crash lands
somewhere else entirely, much later, in code that did nothing wrong.

Over-reserving wastes at most 4 KiB per region. Under-reserving corrupts the kernel. The
asymmetry in the cost is why the asymmetry in the rounding.

There is a host test for exactly this shape (`mark_used_claims_partially_covered_frames`) and
a kernel test that states the invariant directly (`every_frame_of_the_kernel_image_is_reserved`,
which walks every frame of the image and asserts each is marked used).

## Span, not sum

We track every frame between the lowest RAM address and the highest, including any holes.

A board with RAM at `0x4000_0000` and again at `0x8_0000_0000` would give us a bitmap covering
the enormous gap between them, and we simply never free those frames. A bit of wasted bitmap
buys a much simpler index calculation: `frame_index = (addr - base) / 4096`, with no region
table to search.

If that ever becomes expensive, the fix is a per-region allocator. It isn't expensive yet.

## What we don't have

**No heap.** No `Vec`, no `Box`. The regions coming out of the device tree go into fixed-size
arrays, because a fixed-size array is what you use when there is no allocator. `Vec` arrives at
milestone 4, on top of this ([no-std.md](no-std.md)).

**No guard page.** See [stack.md](stack.md), and the incident that produced that note.

**No interrupt-safe locking.** `ALLOCATOR` is a `spin::Mutex`, which is a formality on one core
with no interrupts. The moment an interrupt handler wants a frame while the interrupted code
holds the lock, we deadlock instantly and permanently: the handler spins for a lock only the
code it interrupted can release. **We need a written-down locking discipline before milestone
5, not after it.**

---

*Add to this file as new memory concepts come up.*
