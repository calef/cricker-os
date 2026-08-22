//! **`user_heap`**: the userspace heap algorithm, as pure logic (milestone 27).
//!
//! A first-fit, address-sorted free list with coalescing, the same shape the kernel's original
//! milestone-4 heap had (notes/heap.md) and for the same reason: zero overhead on allocated
//! memory. `GlobalAlloc::dealloc` receives the `Layout` back, so no per-allocation header is
//! needed; the free-list node lives *inside* free memory, and allocated bytes are all payload.
//!
//! This crate is deliberately just the algorithm: it manages address ranges someone else hands it
//! (`add_region`) and never maps a page or makes a syscall. The syscall half (growing the heap out
//! of the process's own untyped via `untyped::MAP`) lives in `user_rt::heap`, which wraps this in
//! a lock and a grow-on-demand policy. The split is the project's standing rule: pure logic
//! compiles for the host, so these invariants are proven in milliseconds by `cargo test -p user_heap`,
//! not in QEMU.
//!
//! # The one invariant that makes it headerless
//!
//! Every block the heap ever hands out or holds free is **16-aligned with a size that is a
//! multiple of 16** (`MIN_ALIGN`, which is also `size_of::<FreeBlock>()`). `effective_size`
//! rounds every request up to that grid, and `alloc`/`dealloc` both compute it from the same
//! `Layout`, so the pair always agrees on how many bytes a pointer owns without storing it
//! anywhere. Front padding for over-aligned requests lands on the same grid, so every remainder
//! is itself a legal free block and no sliver is ever silently leaked.
//!
//! # Examples
//!
//! The heap manages ranges someone else donates; it never maps a page. That split is what lets an
//! example run on the host at all:
//!
//! ```
//! use core::alloc::Layout;
//! use user_heap::{Heap, MIN_ALIGN, effective_size};
//!
//! // A 16-aligned arena. On a real process this would be pages mapped out of the untyped budget.
//! #[repr(align(4096))]
//! struct Arena([u8; 4096]);
//! let mut arena = Box::new(Arena([0; 4096]));
//!
//! let mut h = Heap::new();
//! // SAFETY: the arena is exclusively ours and outlives `h`.
//! unsafe { h.add_region(arena.0.as_mut_ptr(), arena.0.len()) };
//! assert_eq!(h.free_bytes(), 4096);
//! assert_eq!(h.block_count(), 1);
//!
//! let p = h.alloc(Layout::from_size_align(100, 8).unwrap()).unwrap();
//! assert!(p.as_ptr().addr().is_multiple_of(MIN_ALIGN));
//!
//! // SAFETY: `p` came from this heap with this layout, and is freed once.
//! unsafe { h.dealloc(p, Layout::from_size_align(100, 8).unwrap()) };
//!
//! // Everything freed coalesces back to one block. That is the invariant, not a nicety:
//! // without it a long-lived process fragments to death.
//! assert_eq!(h.block_count(), 1);
//! assert_eq!(h.free_bytes(), 4096);
//! ```
//!
//! Sizes round up to the 16-byte grid, which is what makes the allocator headerless: `alloc` and
//! `dealloc` both recompute the same number from the `Layout`, so nothing has to be stored.
//!
//! ```
//! use core::alloc::Layout;
//! use user_heap::{MIN_ALIGN, effective_size};
//!
//! assert_eq!(effective_size(Layout::from_size_align(1, 1).unwrap()), MIN_ALIGN);
//! assert_eq!(effective_size(Layout::from_size_align(16, 1).unwrap()), 16);
//! assert_eq!(effective_size(Layout::from_size_align(17, 1).unwrap()), 32);
//! ```
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `uheap`. Refused `uheap` (the `u` was
//! "userspace" and had to be decoded, while `user_rt` already establishes `user_` as this tree's
//! prefix for it).

#![no_std]

use core::alloc::Layout;
use core::ptr::NonNull;

/// The allocation grid: every block is aligned to this and sized in multiples of it. It is
/// exactly `size_of::<FreeBlock>()`, so any remainder of a split can hold a free-list node.
pub const MIN_ALIGN: usize = 16;

/// A free-list node, stored in the first 16 bytes of the free block it describes. This is the
/// trick that makes allocated memory overhead-free: only *free* memory pays for bookkeeping,
/// and free memory has nothing better to do.
struct FreeBlock {
    size: usize,
    next: Option<NonNull<FreeBlock>>,
}

const _: () = assert!(core::mem::size_of::<FreeBlock>() == MIN_ALIGN);
const _: () = assert!(core::mem::align_of::<FreeBlock>() <= MIN_ALIGN);

/// The bytes a `Layout` actually costs on the 16-byte grid. `alloc` and `dealloc` must agree on
/// this number from the `Layout` alone; this function is that agreement.
pub fn effective_size(layout: Layout) -> usize {
    layout.size().max(1).next_multiple_of(MIN_ALIGN)
}

/// The heap: an address-sorted list of free blocks over regions the caller donated.
pub struct Heap {
    head: Option<NonNull<FreeBlock>>,
    free: usize,
}

// SAFETY: the raw pointers are to memory the heap exclusively manages; the caller provides the
// locking (user_rt::heap wraps this in a spinlock). Same contract as any allocator core.
unsafe impl Send for Heap {}

impl Heap {
    /// An empty heap. `const` so it can back a `static` allocator before any region exists.
    pub const fn new() -> Self {
        Heap {
            head: None,
            free: 0,
        }
    }

    /// Total free bytes. An exact count, not an estimate; the fragmentation tests lean on it.
    pub fn free_bytes(&self) -> usize {
        self.free
    }

    /// The number of free blocks. After every allocation is returned, this must be the number of
    /// disjoint regions donated (1, if they were contiguous); that is the coalescing test.
    pub fn block_count(&self) -> usize {
        let mut n = 0;
        let mut cur = self.head;
        while let Some(b) = cur {
            n += 1;
            // SAFETY: every node in the list points into memory the heap owns.
            cur = unsafe { b.as_ref().next };
        }
        n
    }

    /// Donate `[start, start + size)` to the heap. The range must be 16-aligned at both ends,
    /// unused, and disjoint from everything donated before. Adjacent donations coalesce, which is
    /// what lets `user_rt::heap` grow the committed range page by page and still end up with one
    /// big block.
    ///
    /// # Safety
    /// The caller hands over exclusive ownership of the range; nothing else may touch it while
    /// the heap lives.
    pub unsafe fn add_region(&mut self, start: *mut u8, size: usize) {
        debug_assert!(start.addr().is_multiple_of(MIN_ALIGN));
        debug_assert!(size.is_multiple_of(MIN_ALIGN));
        if size == 0 {
            return;
        }
        // SAFETY: per the caller's contract the range is exclusively ours and correctly aligned.
        unsafe { self.insert_free(start, size) };
    }

    /// Allocate for `layout`, first-fit. Returns `None` when no block fits, which the caller
    /// treats as "grow the heap and retry" rather than as a final failure.
    pub fn alloc(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let size = effective_size(layout);
        let align = layout.align().max(MIN_ALIGN);

        // Walk via a pointer to the *link* (the `Option` that names the current block), so
        // removal is one store whether the block is at the head or mid-list.
        let mut link: *mut Option<NonNull<FreeBlock>> = &mut self.head;
        // SAFETY: `link` always points either at `self.head` or at a live node's `next`.
        while let Some(block) = unsafe { *link } {
            let base = block.as_ptr() as usize;
            // SAFETY: the node is live free memory owned by the heap.
            let (bsize, next) = unsafe { ((*block.as_ptr()).size, (*block.as_ptr()).next) };

            let aligned = base.next_multiple_of(align);
            let front = aligned - base;
            if front + size <= bsize {
                let tail = bsize - front - size;
                // Both remainders are multiples of 16 (base and `aligned` are on the grid), so
                // each is either zero or big enough to be a free block. No slivers.
                let after_front: Option<NonNull<FreeBlock>> = if tail > 0 {
                    let t = (aligned + size) as *mut FreeBlock;
                    // SAFETY: the tail range lies inside the block we are carving.
                    unsafe {
                        (*t).size = tail;
                        (*t).next = next;
                    }
                    NonNull::new(t)
                } else {
                    next
                };
                if front > 0 {
                    // The front remainder keeps the block's list position; only its size shrinks.
                    // SAFETY: writing the node in place, still inside memory we own.
                    unsafe {
                        (*block.as_ptr()).size = front;
                        (*block.as_ptr()).next = after_front;
                    }
                } else {
                    // SAFETY: `link` points at the Option naming this block.
                    unsafe { *link = after_front };
                }
                self.free -= size;
                return NonNull::new(aligned as *mut u8);
            }
            // SAFETY: the node is live; taking a pointer to its `next` field.
            link = unsafe { &raw mut (*block.as_ptr()).next };
        }
        None
    }

    /// Return an allocation. `layout` must be the one passed to `alloc` (the `GlobalAlloc`
    /// contract), because the effective size is recomputed from it rather than stored.
    ///
    /// # Safety
    /// `ptr` must be a live allocation from this heap with this `layout`, freed exactly once.
    pub unsafe fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: per the contract the range belongs to this allocation and returns to us whole.
        unsafe { self.insert_free(ptr.as_ptr(), effective_size(layout)) };
    }

    /// Insert a free range at its address-sorted position and coalesce with both neighbours.
    /// Shared by `dealloc` (returning an allocation) and `add_region` (donating fresh pages);
    /// they are the same operation, which is why growth coalesces for free.
    ///
    /// # Safety
    /// The range is exclusively owned, 16-aligned, a multiple of 16 long, and overlaps nothing
    /// the heap already tracks.
    unsafe fn insert_free(&mut self, start: *mut u8, size: usize) {
        // `expose_provenance`, not `addr`, and it is load-bearing (Miri, milestone 79). This
        // allocator's arithmetic is integer arithmetic: `alloc` minting a tail block at
        // `aligned + size` and the predecessor recovery below both conjure pointers from plain
        // numbers, and a coalesced block can span two donations, so no single donated pointer
        // could carry provenance for it anyway. Rust's rule for that shape is expose-and-reclaim:
        // exposing each incoming range here is what entitles every later integer-minted pointer
        // into it, and with `addr()` (which deliberately does not expose) the first `alloc` that
        // carved a coalesced block was UB, caught by Miri as a write with no exposed tag. At
        // runtime the two are the same instruction; the difference is whether the optimizer is
        // told the escape exists. Strict provenance (`-Zmiri-strict-provenance`) is therefore
        // off the table for this crate, and honestly so: see notes/undefined-behavior.md.
        let addr = start.expose_provenance();
        self.free += size;

        // Taken ONCE, and the predecessor check below compares against this same pointer rather
        // than taking `&mut self.head` again. The original wrote `&mut self.head` a second time
        // at that comparison, and a fresh `&mut` is a fresh unique borrow: it invalidated the
        // tag `link` still carried, so the head-insertion store through `link` at the bottom
        // was a write through a dead borrow. Every compiler we ran emitted the store anyway,
        // which is why it survived 60 native tests; it was still UB, licensed to break on any
        // toolchain bump. Miri's aliasing check is the only gate in the tree that sees this
        // class, and this was the first thing it found (milestone 79, notes/undefined-behavior.md).
        let head_link: *mut Option<NonNull<FreeBlock>> = &mut self.head;
        let mut link = head_link;
        // Find the first block above `addr`; `link` ends up at the insertion point.
        // SAFETY: `link` always points at `self.head` or a live node's `next` field.
        while let Some(block) = unsafe { *link } {
            if block.as_ptr() as usize > addr {
                break;
            }
            // SAFETY: `block` came from `*link` on the line above, so it is a live free-list node; taking a raw pointer to its `next` field does not read through it.
            link = unsafe { &raw mut (*block.as_ptr()).next };
        }

        // SAFETY: reading the successor node, if any; it is live free memory.
        let next = unsafe { *link };
        let mut new_size = size;
        let mut new_next = next;
        if let Some(n) = next
            && n.as_ptr() as usize == addr + size
        {
            // The freed range butts up against the next free block: absorb it.
            // SAFETY: `n` is a live node we are consuming.
            unsafe {
                new_size += (*n.as_ptr()).size;
                new_next = (*n.as_ptr()).next;
            }
        }

        // Is the *predecessor* adjacent? `link` points into it if there is one: recover the node
        // address from the link address, except when `link` is the head link itself (compared
        // against the pointer taken at the top; see the comment there for why not `&mut` again).
        if link != head_link {
            // `link` is `&(*prev).next`, so `prev` starts `offset_of!(next)` bytes before it.
            let prev = (link as usize - core::mem::offset_of!(FreeBlock, next)) as *mut FreeBlock;
            // SAFETY: `prev` is the live node whose `next` field `link` points at.
            let prev_end = unsafe { prev.addr() + (*prev).size };
            if prev_end == addr {
                // SAFETY: extend the predecessor over the freed range (and any absorbed next).
                unsafe {
                    (*prev).size += new_size;
                    (*prev).next = new_next;
                }
                return;
            }
        }

        // Alignment is an invariant of this allocator, not an accident: every block it hands out
        // or reclaims is `MIN_ALIGN`-aligned, and `MIN_ALIGN >= align_of::<FreeBlock>()`. clippy
        // sees only `*mut u8 -> *mut FreeBlock` and cannot see the invariant.
        #[allow(
            clippy::cast_ptr_alignment,
            reason = "start is MIN_ALIGN-aligned by this allocator's own construction"
        )]
        let node = start.cast::<FreeBlock>();
        // SAFETY: the freed range is ours and big enough for a node (>= MIN_ALIGN bytes).
        unsafe {
            (*node).size = new_size;
            (*node).next = new_next;
            *link = NonNull::new(node);
        }
    }
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}
