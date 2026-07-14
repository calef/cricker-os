//! The kernel allocator: a slab in front, a general-purpose heap behind.
//!
//! # This is the `no_std` debt being paid
//!
//! From notes/no-std.md, written at milestone 1:
//!
//! > At milestone 4 we write a `#[global_allocator]`, add `extern crate alloc;`, and `Vec`
//! > starts working. **Not because we imported it. Because we built the heap it needed.**
//!
//! Nothing was imported. The chain runs all the way down:
//!
//! ```text
//!   Vec, Box, String, BTreeMap
//!         |  #[global_allocator]
//!   +-----+------------------+
//!   | slab (<= 2048 B)       |  O(1) alloc, O(1) free       (crates/slab)
//!   | heap (everything else) |  coalescing free list        (crates/heap)
//!   +-----+------------------+
//!   frame allocator            fixed 4 KiB pages, bitmap    (crates/frames)
//!         |
//!   physical RAM               read out of the device tree  (crates/dtb)
//! ```
//!
//! # Why two allocators, and how we chose
//!
//! The general heap keeps one address-sorted free list and coalesces adjacent blocks. Both
//! `alloc` and `free` walk that list, so both are O(n) in the number of free blocks. We
//! **measured** how large `n` gets (`crates/heap/tests/fragmentation.rs`):
//!
//! | Workload | free-list length |
//! |---|---|
//! | uniform 64 B, 1000 live | **1** |
//! | mixed 16-256 B, freed out of order | **3** |
//! | uniform 64 B, **every other one freed** | **1001** |
//!
//! So the O(n) is a non-issue for most workloads and catastrophic for exactly one shape: **many
//! isolated, same-sized holes.** Which is precisely what a kernel produces: two thousand
//! threads, half of them exit; a file descriptor table with gaps.
//!
//! A slab kills that case dead. Every object in a class is the same size, so a freed one is
//! immediately reusable and **coalescing becomes unnecessary rather than fast**. O(1) both ways,
//! zero metadata in allocated objects. It is what Linux does (SLUB), for exactly this reason.
//!
//! The general heap stays, as the fallback for allocations over 2 KiB or with an alignment no
//! size class can serve. Those are rare, so its O(n) never gets exercised on a long list.
//!
//! # Why it must come after the MMU
//!
//! Both allocators hand out *addresses*, and with paging on an address is only usable if it is
//! mapped. Our RAM is direct-mapped read/write, so the frames we take here are usable the
//! moment we get them. That is not free; it is something `mmu::init` arranged.

use crate::arch::mmu::phys_to_virt;
use crate::memory;
use crate::println;
use crate::sync::{IrqSafeMutex, rank};
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use frames::FRAME_SIZE;
use heap::Heap;
use slab::SlabAllocator;

/// 1 MiB of contiguous frames for the general-purpose heap.
///
/// Contiguous because the heap treats its region as one flat span of addresses. **This is the
/// reason `frames` is a bitmap and not a free list**: a free list could not have answered the
/// request. See notes/physical-memory.md.
///
/// The slab does *not* need this: it takes one page at a time, as classes run dry.
const HEAP_FRAMES: usize = 256;

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator {
    slab: IrqSafeMutex::new(rank::SLAB, SlabAllocator::new()),
    heap: IrqSafeMutex::new(rank::HEAP, Heap::new()),
};

/// `IrqSafeMutex`, not a bare spinlock, and the reason is DECISIONS.md §9: an interrupt handler
/// that allocated while the interrupted code held one of these would spin forever waiting for
/// code that cannot run. On one core. Permanently.
///
/// The rule that goes with it: **interrupt handlers do not allocate.** The locks are the belt;
/// that rule is the braces.
///
/// Two separate locks rather than one, because the two allocators never touch each other's
/// state. A request goes to exactly one of them, decided purely by its `Layout`.
struct KernelAllocator {
    slab: IrqSafeMutex<SlabAllocator>,
    heap: IrqSafeMutex<Heap>,
}

impl KernelAllocator {
    /// Which allocator owns this layout.
    ///
    /// **`alloc` and `dealloc` must agree, and they do because both ask this.** If they ever
    /// disagreed, memory would be freed into the wrong allocator, which is the kind of
    /// corruption that surfaces somewhere else entirely, much later.
    ///
    /// The decision is a pure function of the `Layout`, which Rust hands us on *both* paths.
    /// C's `free(ptr)` gets no such thing, which is why C allocators need a header on every
    /// block just to remember its size. We get it for nothing.
    fn use_slab(layout: Layout) -> bool {
        slab::class_for(layout).is_some()
    }
}

// SAFETY: the locks provide exclusion, and each allocator owns the memory it manages.
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if Self::use_slab(layout) {
            // A page for the slab, if a class ran dry. One frame from the frame allocator, named
            // through the direct map so we can actually write to it.
            let page = || memory::alloc().map(|f| phys_to_virt(f.addr()) as usize);

            self.slab
                .lock()
                .alloc(layout, page)
                .map_or(core::ptr::null_mut(), |p| p.as_ptr())
        } else {
            self.heap
                .lock()
                .alloc(layout)
                .map_or(core::ptr::null_mut(), |p| p.as_ptr())
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let Some(p) = NonNull::new(ptr) else { return };

        // SAFETY: the caller's contract on `GlobalAlloc::dealloc`, and `use_slab` is the same
        // pure function of `layout` that `alloc` used, so this reaches the allocator the memory
        // actually came from.
        unsafe {
            if Self::use_slab(layout) {
                self.slab.lock().dealloc(p, layout);
            } else {
                self.heap.lock().dealloc(p, layout);
            }
        }
    }

    /// Try to resize without moving, before falling back to allocate-copy-free.
    ///
    /// **The default implementation of this method always copies**, and `Vec::push` doubling is
    /// the most common allocation pattern in Rust. A `Vec` grown to 1000 elements reallocates
    /// about ten times; without this, every one is a full `memcpy` of the whole buffer *and* a
    /// block abandoned somewhere else, which is exactly the churn a coalescing free list exists
    /// to prevent.
    ///
    /// Two ways to avoid the copy:
    ///
    /// - **The slab**: if the new size lands in the *same class*, there is literally nothing to
    ///   do. A 100-byte `Vec` growing to 120 bytes is still in the 128 class. Free.
    /// - **The heap**: grow into the free block above, or shrink and hand back the tail.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let Some(p) = NonNull::new(ptr) else {
            return core::ptr::null_mut();
        };
        let Ok(new_layout) = Layout::from_size_align(new_size, layout.align()) else {
            return core::ptr::null_mut();
        };

        let (old_slab, new_slab) = (Self::use_slab(layout), Self::use_slab(new_layout));

        if old_slab && new_slab {
            // Same size class? Then the object is already big enough and already small enough.
            if slab::class_for(layout) == slab::class_for(new_layout) {
                return ptr;
            }
        } else if !old_slab && !new_slab {
            // Both in the general heap: try to grow or shrink where it stands.
            //
            // Scoped, because the lock is NOT reentrant: `self.alloc` below takes it again.
            // Holding it across that call would deadlock against ourselves, which is the
            // single-thread special case of DECISIONS.md §9.
            let mut h = self.heap.lock();
            // SAFETY: caller's contract.
            if unsafe { h.realloc_in_place(p, layout, new_size) } {
                return ptr;
            }
        }

        // Couldn't stay put, or crossed the slab/heap boundary. Do it the honest way.
        // SAFETY: caller's contract, and `new_layout` is valid.
        unsafe {
            let new_ptr = self.alloc(new_layout);
            if !new_ptr.is_null() {
                core::ptr::copy_nonoverlapping(ptr, new_ptr, layout.size().min(new_size));
                self.dealloc(ptr, layout);
            }
            new_ptr
        }
    }
}

/// Hand the general-purpose heap its region. The slab needs no init: it takes pages lazily.
///
/// Must run **after** `mmu::init`: allocators deal in addresses, and an address is only usable
/// once something has mapped it.
pub fn init() {
    let first = memory::alloc_contiguous(HEAP_FRAMES)
        .expect("no contiguous run of frames for the kernel heap");

    // The frame allocator speaks PHYSICAL. The heap deals in addresses it can dereference. The
    // direct map is the bridge.
    let start = phys_to_virt(first.addr()) as usize;
    let size = HEAP_FRAMES * FRAME_SIZE as usize;

    // SAFETY: `alloc_contiguous` gave us these frames exclusively and contiguously, and
    // `mmu::init` direct-mapped all of RAM read/write, so this range is real, writable memory
    // that nobody else owns.
    unsafe { ALLOCATOR.heap.lock().add_region(start, size) };
}

/// (allocated, total) for the general-purpose heap. Only serves allocations over 2 KiB.
pub fn stats() -> (usize, usize) {
    let h = ALLOCATOR.heap.lock();
    (h.allocated(), h.total())
}

/// (allocated, pages taken from the frame allocator) for the slab.
pub fn slab_stats() -> (usize, usize) {
    let s = ALLOCATOR.slab.lock();
    (s.allocated(), s.capacity())
}

pub fn print_summary() {
    let (heap_used, heap_total) = stats();
    let (slab_used, slab_cap) = slab_stats();

    println!(
        "  slab            : {slab_used} B used, {} KiB of pages   (<= 2 KiB: O(1) alloc AND free)",
        slab_cap / 1024,
    );
    println!(
        "  heap            : {heap_used} B used, {} KiB total      (> 2 KiB: coalescing free list)",
        heap_total / 1024,
    );
}

#[cfg(test)]
mod tests {
    //! Tests for the kernel heap.
    //!
    //! The allocator's logic lives in `crates/heap` and is tested on the host (17 tests,
    //! milliseconds). These prove the *wiring*: that `#[global_allocator]` is hooked up, that the
    //! region we handed it is real mapped memory, and that `Vec` and friends actually work.

    /// `Vec` works. Not because we imported it: **because we built the heap it needed.**
    ///
    /// notes/no-std.md promised this at milestone 1 and this is the promise coming due. The
    /// chain runs Vec -> #[global_allocator] -> our heap -> our frame allocator -> RAM we
    /// read out of the device tree. Every link is ours.
    #[test_case]
    fn vec_works() {
        use alloc::vec::Vec;

        let mut v: Vec<u64> = Vec::new();
        for i in 0..1000 {
            v.push(i * 3);
        }

        // It reallocated several times getting here, which means it allocated, copied, and
        // freed the old buffer. All of that went through code we wrote.
        assert_eq!(v.len(), 1000);
        assert_eq!(v[999], 2997);
        assert_eq!(v.iter().sum::<u64>(), (0..1000u64).map(|i| i * 3).sum());
    }

    /// `Box` works, and the memory really is distinct.
    #[test_case]
    fn box_works() {
        use alloc::boxed::Box;

        let a = Box::new(0xdead_beefu64);
        let b = Box::new(0xcafe_f00du64);

        assert_eq!(*a, 0xdead_beef);
        assert_eq!(*b, 0xcafe_f00d);
        assert_ne!(
            &raw const *a, &raw const *b,
            "two Boxes at the same address"
        );
    }

    /// `String` and `format!` work, which means `core::fmt` can now allocate.
    #[test_case]
    fn string_and_format_work() {
        use alloc::format;

        let s = format!("{:#x} and {}", 0x1234, "text");
        assert_eq!(s, "0x1234 and text");
    }

    /// `BTreeMap` works. Milestone 7 wants one for the process table.
    #[test_case]
    fn btreemap_works() {
        use alloc::collections::BTreeMap;

        let mut m = BTreeMap::new();
        for i in 0..100u32 {
            m.insert(i, i * i);
        }
        assert_eq!(m.get(&12), Some(&144));
        assert_eq!(m.len(), 100);
    }

    /// Memory actually comes back. **From both allocators.**
    ///
    /// This test used to allocate 1 KiB and check `heap::stats()`. Since the slab went in,
    /// 1 KiB is served by the *slab*, so it was checking a counter that could not move: a test
    /// that cannot fail. Now it exercises both sides of the split and checks both.
    #[test_case]
    fn neither_allocator_leaks() {
        use alloc::vec::Vec;

        let (slab_before, _) = crate::heap::slab_stats();
        let (heap_before, _) = crate::heap::stats();

        for _ in 0..200 {
            let small: Vec<u8> = Vec::with_capacity(1024); // <= 2 KiB: the slab
            let large: Vec<u8> = Vec::with_capacity(8192); // >  2 KiB: the heap
            core::hint::black_box((&small, &large));
            // both dropped here
        }

        assert_eq!(
            crate::heap::slab_stats().0,
            slab_before,
            "the SLAB leaked across 200 alloc/free cycles"
        );
        assert_eq!(
            crate::heap::stats().0,
            heap_before,
            "the HEAP leaked across 200 alloc/free cycles"
        );
    }

    /// The split is real: a small allocation goes to the slab, a large one to the heap.
    ///
    /// If this ever fails, `alloc` and `dealloc` may be disagreeing about which allocator owns
    /// a block, and memory freed into the wrong allocator is the kind of corruption that
    /// surfaces somewhere else entirely, much later.
    #[test_case]
    fn small_goes_to_the_slab_and_large_goes_to_the_heap() {
        use alloc::vec::Vec;

        let (slab0, _) = crate::heap::slab_stats();
        let (heap0, _) = crate::heap::stats();

        let small: Vec<u8> = Vec::with_capacity(64);
        assert!(
            crate::heap::slab_stats().0 > slab0,
            "64 B did not go to the slab"
        );
        assert_eq!(
            crate::heap::stats().0,
            heap0,
            "64 B touched the general heap"
        );

        let large: Vec<u8> = Vec::with_capacity(64 * 1024);
        assert!(
            crate::heap::stats().0 > heap0,
            "64 KiB did not go to the heap"
        );

        core::hint::black_box((&small, &large));
    }

    /// The slab makes the pathological workload cheap.
    ///
    /// Allocate many same-sized objects, free every OTHER one, so no two freed blocks are
    /// adjacent and coalescing could do nothing. That is the shape that drives the general
    /// heap's free list to 1001 blocks (measured: crates/heap/tests/fragmentation.rs).
    ///
    /// For a slab it is not a special case at all: every freed object goes back to its class's
    /// list and the next request of that size takes it as-is. Coalescing is not *fast* here.
    /// It is **unnecessary**.
    #[test_case]
    fn the_pathological_workload_costs_the_slab_nothing() {
        use alloc::boxed::Box;
        use alloc::vec::Vec;

        let (_, pages_before) = crate::heap::slab_stats();

        let mut live: Vec<Box<[u8; 64]>> = Vec::new();
        for _ in 0..1000 {
            live.push(Box::new([0u8; 64]));
        }

        // Free every other one: maximum isolation, zero coalescing opportunity.
        let mut kept = Vec::new();
        for (i, b) in live.into_iter().enumerate() {
            if i % 2 == 1 {
                kept.push(b);
            }
            // even ones dropped here
        }

        // Now allocate 500 more of the same size. Every one must come from the free list, so
        // the slab must take NO new pages.
        let (_, pages_mid) = crate::heap::slab_stats();
        for _ in 0..500 {
            kept.push(Box::new([0u8; 64]));
        }
        let (_, pages_after) = crate::heap::slab_stats();

        assert_eq!(
            pages_after, pages_mid,
            "the slab took new pages when 500 objects were sitting free"
        );
        assert!(pages_mid > pages_before);

        core::hint::black_box(&kept);
    }

    /// The heap lives in memory the MMU can actually reach.
    #[test_case]
    fn heap_memory_is_mapped_and_writable() {
        use crate::arch::mmu;
        use alloc::boxed::Box;

        let b = Box::new(0u64);
        let va = (&raw const *b) as u64;

        let (_, flags) = mmu::translate(va).expect("heap memory is NOT MAPPED");
        assert!(flags.is_writable(), "heap memory is not writable");
        assert!(!flags.is_kernel_executable(), "the heap is EXECUTABLE");
    }
}
