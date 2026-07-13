//! A kernel heap: first-fit, address-sorted free list, with coalescing.
//!
//! This is what pays off the `no_std` debt. Once the kernel has one of these behind a
//! `#[global_allocator]`, `Vec`, `Box`, `String`, and `BTreeMap` all start working, not
//! because we imported them but **because we built the heap they needed**. See
//! notes/no-std.md.
//!
//! # How it differs from the frame allocator
//!
//! [`frames`] hands out fixed 4 KiB pages and tracks them in a bitmap. This hands out
//! *arbitrary-sized, arbitrarily-aligned* byte ranges, and the sizes are unpredictable
//! because `Vec` decides them.
//!
//! Which means we need the thing the frame allocator deliberately avoided: **metadata
//! stored inside the free memory itself.** A free block holds its own size and a pointer to
//! the next free block, right there in the bytes nobody is using. It costs zero overhead
//! for allocated memory, and it is only possible because a free block is, by definition,
//! space we may scribble on.
//!
//! # The two things that make it correct
//!
//! **Everything is 16-byte aligned, in both address and size.** That single invariant is
//! what makes splitting always work: any gap left over from a split is a multiple of 16, so
//! it is either exactly zero or big enough to hold a free-block header. Without it you get
//! slivers too small to track, and you leak them one at a time until the heap dies.
//!
//! **The list is sorted by address, and `free` coalesces with both neighbours.** Without
//! coalescing, a heap that allocates and frees in a loop fragments into dust: you end up
//! with thousands of 16-byte free blocks and no room for a 32-byte allocation, while
//! reporting megabytes free.

#![cfg_attr(not(test), no_std)]

use core::alloc::Layout;
use core::ptr::NonNull;

/// The smallest block we will ever track, and the alignment everything is rounded to.
///
/// It is exactly `size_of::<Block>()`: a free block must be able to hold its own header, or
/// we cannot put it on the list.
pub const MIN_BLOCK: usize = 16;

/// The header of a free block, stored **inside the free memory itself**.
#[repr(C)]
struct Block {
    size: usize,
    next: Option<NonNull<Block>>,
}

const _: () = assert!(size_of::<Block>() == MIN_BLOCK);

pub struct Heap {
    /// Head of the free list, sorted by address. Address-sorted is what makes coalescing a
    /// local operation rather than a search.
    head: Option<NonNull<Block>>,
    total: usize,
    allocated: usize,
}

// SAFETY: `Heap` owns the memory it manages. Sharing it across threads is the caller's
// problem to solve with a lock (in the kernel: `IrqSafeMutex`).
unsafe impl Send for Heap {}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap {
    pub const fn new() -> Self {
        Self {
            head: None,
            total: 0,
            allocated: 0,
        }
    }

    /// Give the heap a region of memory to manage.
    ///
    /// # Safety
    /// `start` must point at `size` bytes that are mapped, writable, and owned exclusively
    /// by this heap for as long as it lives.
    pub unsafe fn add_region(&mut self, start: usize, size: usize) {
        let aligned = align_up(start, MIN_BLOCK);
        let end = start + size;

        // A region too small to hold a single block header is a region we cannot track. Drop
        // it rather than pretending.
        if end <= aligned || end - aligned < MIN_BLOCK {
            return;
        }

        let size = align_down(end - aligned, MIN_BLOCK);
        self.total += size;

        // SAFETY: caller's contract.
        unsafe { self.insert(aligned, size) };
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn allocated(&self) -> usize {
        self.allocated
    }

    pub fn free(&self) -> usize {
        self.total - self.allocated
    }

    /// First fit.
    ///
    /// Walks the list and takes the first block that can satisfy the request. Not the *best*
    /// fit, which would walk the whole list every time to find the tightest block; first-fit
    /// is O(fragments) instead of O(all), and with coalescing the list stays short enough
    /// that the difference doesn't matter.
    pub fn alloc(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let (size, align) = normalize(layout);

        let mut prev: Option<NonNull<Block>> = None;
        let mut cur = self.head;

        while let Some(block) = cur {
            // SAFETY: every node on the list is a valid `Block` we placed there.
            let (block_start, block_size, next) = unsafe {
                let b = block.as_ref();
                (block.as_ptr() as usize, b.size, b.next)
            };

            let alloc_start = align_up(block_start, align);
            let alloc_end = alloc_start.checked_add(size)?;
            let block_end = block_start + block_size;

            if alloc_end <= block_end {
                // It fits. Unlink the block, then hand back the two gaps it leaves.
                //
                // Both gaps are guaranteed to be 0 or >= MIN_BLOCK, because everything here
                // is a multiple of 16. That invariant is what makes this always work; see
                // the module docs.
                unsafe { self.unlink(prev, block, next) };

                let front = alloc_start - block_start;
                if front > 0 {
                    // SAFETY: still inside the block we just unlinked.
                    unsafe { self.insert(block_start, front) };
                }

                let back = block_end - alloc_end;
                if back > 0 {
                    // SAFETY: same.
                    unsafe { self.insert(alloc_end, back) };
                }

                self.allocated += size;
                return NonNull::new(alloc_start as *mut u8);
            }

            prev = cur;
            cur = next;
        }

        None
    }

    /// # Safety
    /// `ptr` must have come from [`alloc`](Self::alloc) with an equal `layout`, and must not
    /// be used again.
    pub unsafe fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) {
        let (size, _) = normalize(layout);
        self.allocated -= size;

        // SAFETY: caller's contract.
        unsafe { self.insert(ptr.as_ptr() as usize, size) };
    }

    /// Put a block back on the address-sorted list, merging with whichever neighbours it
    /// touches.
    ///
    /// **The coalescing is the whole point.** Without it, a heap that allocates and frees in
    /// a loop fragments into dust: thousands of 16-byte free blocks, no room for a 32-byte
    /// allocation, and a "free memory" number that says megabytes. The list stays sorted by
    /// address precisely so that "the block before" and "the block after" are the two we
    /// need to check, rather than all of them.
    ///
    /// # Safety
    /// `[start, start+size)` must be memory this heap owns and nobody is using.
    unsafe fn insert(&mut self, start: usize, size: usize) {
        debug_assert!(start % MIN_BLOCK == 0 && size % MIN_BLOCK == 0 && size >= MIN_BLOCK);

        // Find the insertion point: the last block whose address is below ours.
        let mut prev: Option<NonNull<Block>> = None;
        let mut cur = self.head;
        while let Some(block) = cur {
            if block.as_ptr() as usize > start {
                break;
            }
            prev = cur;
            // SAFETY: a valid node.
            cur = unsafe { block.as_ref().next };
        }

        // SAFETY: `start` is ours to write, per the caller's contract.
        let node = unsafe {
            let node = start as *mut Block;
            node.write(Block { size, next: cur });
            NonNull::new_unchecked(node)
        };

        match prev {
            // SAFETY: a valid node.
            Some(mut p) => unsafe { p.as_mut().next = Some(node) },
            None => self.head = Some(node),
        }

        // Merge forward first, then backward. Order matters: merging with `next` first means
        // the backward merge sees the already-grown block and absorbs it whole, so a free
        // between two free neighbours collapses all three in one pass.
        // SAFETY: all nodes on the list are valid.
        unsafe {
            merge_with_next(node);
            if let Some(p) = prev {
                merge_with_next(p);
            }
        }
    }

    /// # Safety
    /// `block` must currently be on the list, with `prev` immediately before it and `next`
    /// immediately after.
    unsafe fn unlink(
        &mut self,
        prev: Option<NonNull<Block>>,
        block: NonNull<Block>,
        next: Option<NonNull<Block>>,
    ) {
        let _ = block;
        match prev {
            // SAFETY: a valid node.
            Some(mut p) => unsafe { p.as_mut().next = next },
            None => self.head = next,
        }
    }
}

/// If `block` ends exactly where the next block begins, they are one block.
///
/// # Safety
/// `block` must be a valid node on the list.
unsafe fn merge_with_next(mut block: NonNull<Block>) {
    // SAFETY: caller's contract.
    unsafe {
        let b = block.as_mut();
        let Some(next) = b.next else { return };

        let block_end = block.as_ptr() as usize + b.size;
        if block_end == next.as_ptr() as usize {
            b.size += next.as_ref().size;
            b.next = next.as_ref().next;
        }
    }
}

/// Round a request up to the heap's invariants: at least `MIN_BLOCK`, a multiple of
/// `MIN_BLOCK`, and aligned to at least `MIN_BLOCK`.
///
/// **`alloc` and `dealloc` must agree**, which is why this is one function and not two
/// sprinkled calculations. If they ever disagreed, `dealloc` would return a block of the
/// wrong size, and the heap would either leak the difference forever or hand out memory that
/// is still in use.
fn normalize(layout: Layout) -> (usize, usize) {
    let size = align_up(layout.size().max(MIN_BLOCK), MIN_BLOCK);
    let align = layout.align().max(MIN_BLOCK);
    (size, align)
}

fn align_up(value: usize, to: usize) -> usize {
    (value + to - 1) & !(to - 1)
}

fn align_down(value: usize, to: usize) -> usize {
    value & !(to - 1)
}
