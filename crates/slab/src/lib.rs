//! A slab allocator: O(1) allocation and free, for the sizes a kernel actually uses.
//!
//! # Why this exists, measured rather than assumed
//!
//! The general-purpose heap (`crates/heap`) keeps one address-sorted free list and coalesces
//! adjacent blocks. Both `alloc` and `free` walk that list, so both are O(n) in the number of
//! free blocks. We measured how large `n` gets:
//!
//! | Workload | free-list length |
//! |---|---|
//! | uniform 64 B, 1000 live | **1** |
//! | mixed 16–256 B, freed out of order | **3** |
//! | uniform 64 B, **every other one freed** | **1001** |
//!
//! So the O(n) is a non-issue for most workloads and catastrophic for exactly one shape: **many
//! isolated, same-sized holes.**
//!
//! And that shape is what a kernel produces. Two thousand threads, half of them exit. A file
//! descriptor table with gaps. It is not hypothetical.
//!
//! # The trade nobody escapes
//!
//! **You cannot coalesce in O(1) without per-block metadata.** To merge with your physical
//! neighbour you must know whether the block at `p + size` is free, and where the block before
//! `p` begins. Neither is knowable without a header on *every* block, allocated ones included.
//! That is why glibc carries 8–16 bytes of overhead per allocation.
//!
//! A slab sidesteps the whole question. **Every object in a slab is the same size**, so a freed
//! object is immediately reusable by the next request of that size. Coalescing becomes
//! *unnecessary* rather than fast, and the pathological case stops existing.
//!
//! | | alloc | free | overhead/alloc | coalesces |
//! |---|---|---|---|---|
//! | `crates/heap` | O(n) | O(n) | **0** | yes |
//! | boundary tags (glibc) | O(1) | O(1) | 8–16 B | yes |
//! | **this** (Linux SLUB) | **O(1)** | **O(1)** | **0** | *doesn't need to* |
//!
//! # How it works
//!
//! Eight size classes: 16, 32, 64, 128, 256, 512, 1024, 2048.
//!
//! Each class owns a free list of objects of *exactly* that size. Allocation pops the head.
//! Free pushes onto the head. Both are a couple of pointer writes.
//!
//! When a class's list runs dry it takes a whole 4 KiB page from the frame allocator and carves
//! it into objects, all of which go on the list. The list node lives **inside the free object**,
//! the same trick as the heap's free blocks and for the same reason: a free object is by
//! definition space nobody is using.
//!
//! Alignment falls out for free. A page is 4096-aligned, and an object of size `16 << i` sits at
//! offset `k * (16 << i)` within it, so it is naturally aligned to its own size. A 256-byte
//! class hands out 256-aligned objects without trying.
//!
//! # What it does not do
//!
//! **Slabs are never returned to the frame allocator.** Once a page belongs to the 64-byte
//! class it belongs to it forever, even if every object in it is free. Real SLUB tracks
//! per-slab occupancy and frees empty ones. We don't yet, and the memory is bounded by the
//! high-water mark of each class rather than by current usage.

#![cfg_attr(not(test), no_std)]

use core::alloc::Layout;
use core::ptr::NonNull;

/// 16, 32, 64, 128, 256, 512, 1024, 2048.
pub const CLASSES: usize = 8;

/// The smallest object, and the size of the free-list node stored inside one.
pub const MIN_SIZE: usize = 16;

/// Anything larger goes to the general-purpose heap. 2048 is half a page: at 4096 we would get
/// one object per slab, and a "slab" of one object is just a page allocation with extra steps.
pub const MAX_SIZE: usize = MIN_SIZE << (CLASSES - 1);

/// The page we carve slabs out of.
pub const PAGE_SIZE: usize = 4096;

/// The size of class `i`.
pub const fn class_size(i: usize) -> usize {
    MIN_SIZE << i
}

/// Which class serves this layout, if any.
///
/// The class must be at least as large as the request **and** at least as large as its
/// alignment, because that is the only alignment guarantee a slab gives: an object of size
/// `16 << i` inside a 4096-aligned page is aligned to `16 << i` and nothing stronger.
///
/// A request for 16 bytes aligned to 4096 therefore does *not* fit any class, and correctly
/// falls through to the general heap.
pub fn class_for(layout: Layout) -> Option<usize> {
    let need = layout.size().max(layout.align()).max(MIN_SIZE);
    if need > MAX_SIZE {
        return None;
    }
    // The smallest class that is >= need. `need` is at most 2048, so this terminates.
    let mut i = 0;
    while class_size(i) < need {
        i += 1;
    }
    Some(i)
}

/// A free object. Lives **inside** the object it describes.
#[repr(C)]
struct Free {
    next: Option<NonNull<Free>>,
}

const _: () = assert!(size_of::<Free>() <= MIN_SIZE);

pub struct SlabAllocator {
    free: [Option<NonNull<Free>>; CLASSES],
    /// Bytes currently handed out (rounded to the class size).
    allocated: usize,
    /// Bytes of pages we have taken from the frame allocator and never give back.
    capacity: usize,
    /// How many pages each class owns. Diagnostics, and it is how you would notice a class
    /// growing without bound.
    slabs: [usize; CLASSES],
}

// SAFETY: owns the memory it manages; sharing is the caller's problem, solved with a lock.
unsafe impl Send for SlabAllocator {}

impl Default for SlabAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl SlabAllocator {
    pub const fn new() -> Self {
        Self {
            free: [None; CLASSES],
            allocated: 0,
            capacity: 0,
            slabs: [0; CLASSES],
        }
    }

    pub fn allocated(&self) -> usize {
        self.allocated
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn slabs(&self, class: usize) -> usize {
        self.slabs[class]
    }

    /// How many objects are on class `i`'s free list. Test support: this is the number that
    /// stays *constant* under the workload that makes the general heap's free list explode.
    pub fn free_objects(&self, class: usize) -> usize {
        let mut n = 0;
        let mut cur = self.free[class];
        while let Some(o) = cur {
            n += 1;
            // SAFETY: a valid node we placed on the list.
            cur = unsafe { o.as_ref().next };
        }
        n
    }

    /// Allocate. **O(1)**: pop the head of a free list.
    ///
    /// `get_page` supplies a fresh, mapped, 4096-aligned page when a class runs dry. In the
    /// kernel that is the frame allocator; in the tests it is a host allocation. Keeping it a
    /// closure is what lets this whole file be pure logic and run on the host.
    ///
    /// Returns `None` if the layout doesn't fit any class (the caller should fall back to the
    /// general heap) or if `get_page` is out of memory.
    pub fn alloc(
        &mut self,
        layout: Layout,
        get_page: impl FnOnce() -> Option<usize>,
    ) -> Option<NonNull<u8>> {
        let class = class_for(layout)?;

        if self.free[class].is_none() {
            self.grow(class, get_page()?);
        }

        // Pop. Two loads and a store; no search, no walk, no coalescing.
        let head = self.free[class]?;
        // SAFETY: a valid node we placed on the list.
        self.free[class] = unsafe { head.as_ref().next };

        self.allocated += class_size(class);
        Some(head.cast())
    }

    /// Free. **O(1)**: push onto the head of a free list.
    ///
    /// No coalescing, and none needed: the object is exactly the size of everything else in its
    /// class, so the very next request of that size can take it as-is. That is the whole point
    /// of the design.
    ///
    /// # Safety
    /// `ptr` must have come from [`alloc`](Self::alloc) with an equal `layout`.
    pub unsafe fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) {
        let Some(class) = class_for(layout) else {
            debug_assert!(false, "dealloc of a layout no class serves");
            return;
        };

        let node: NonNull<Free> = ptr.cast();

        // SAFETY: the object is ours and nobody is using it, so we may store the list node
        // inside it. Same trick as the heap's free blocks, same justification.
        unsafe {
            node.write(Free {
                next: self.free[class],
            });
        }

        self.free[class] = Some(node);
        self.allocated -= class_size(class);
    }

    /// Carve a fresh page into objects and put them all on the class's free list.
    ///
    /// Builds the list back-to-front so the objects come out in ascending address order, which
    /// costs nothing and makes the allocator's behaviour easier to read in a dump.
    fn grow(&mut self, class: usize, page: usize) {
        debug_assert!(page % PAGE_SIZE == 0, "slab pages must be page-aligned");

        let size = class_size(class);
        let count = PAGE_SIZE / size;

        let mut head = self.free[class];
        for i in (0..count).rev() {
            let addr = page + i * size;

            // SAFETY: a fresh page from the caller, ours exclusively. Every object is
            // `size`-aligned because the page is 4096-aligned and `size` divides 4096.
            let node = unsafe {
                let node = addr as *mut Free;
                node.write(Free { next: head });
                NonNull::new_unchecked(node)
            };
            head = Some(node);
        }

        self.free[class] = head;
        self.capacity += PAGE_SIZE;
        self.slabs[class] += 1;
    }
}
