//! The kernel heap, and the `#[global_allocator]` that makes `Vec` work.
//!
//! # This is the `no_std` debt being paid
//!
//! From notes/no-std.md, written at milestone 1:
//!
//! > At milestone 4 we write a `#[global_allocator]`, add `extern crate alloc;`, and `Vec`
//! > starts working. **Not because we imported it. Because we built the heap it needed.**
//!
//! That's this file. Nothing was imported. The chain runs all the way down:
//!
//! ```text
//!   Vec, Box, String, BTreeMap
//!         |  #[global_allocator]
//!   kernel heap          arbitrary sizes, free list, coalescing  (crates/heap)
//!         |
//!   frame allocator      fixed 4 KiB pages, bitmap               (crates/frames)
//!         |
//!   physical RAM         read out of the device tree             (crates/dtb)
//! ```
//!
//! # Why it must come after the MMU
//!
//! The heap hands out *addresses*, and with paging on, an address is only usable if it is
//! mapped. Our RAM is identity-mapped read/write, so the frames we take here are usable the
//! moment we get them. That is not free; it is something `mmu::init` arranged.

use crate::memory;
use crate::println;
use crate::sync::IrqSafeMutex;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use frames::FRAME_SIZE;
use heap::Heap;

/// 1 MiB, in contiguous frames.
///
/// Contiguous because the heap treats its region as one flat span of addresses, and with an
/// identity map, contiguous physical means contiguous virtual. **This is the first real use
/// of `alloc_contiguous`, and the reason `frames` is a bitmap and not a free list**: a free
/// list could not have answered this request. See notes/physical-memory.md.
const HEAP_FRAMES: usize = 256;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap(IrqSafeMutex::new(Heap::new()));

/// The heap, behind the kernel's interrupt-safe lock.
///
/// `IrqSafeMutex`, not a bare spinlock, and the reason is DECISIONS.md §9: an interrupt
/// handler that allocated while the interrupted code held this lock would spin forever
/// waiting for code that cannot run. On one core. Permanently.
///
/// The rule that goes with it: **interrupt handlers do not allocate.** The lock is the belt,
/// that rule is the braces.
struct LockedHeap(IrqSafeMutex<Heap>);

// SAFETY: the lock provides the exclusion, and `Heap` owns the memory it manages.
unsafe impl GlobalAlloc for LockedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0
            .lock()
            .alloc(layout)
            .map_or(core::ptr::null_mut(), |p| p.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(p) = NonNull::new(ptr) {
            // SAFETY: the caller's contract on `GlobalAlloc::dealloc`.
            unsafe { self.0.lock().dealloc(p, layout) };
        }
    }

    /// Try to resize in place before falling back to allocate-copy-free.
    ///
    /// **The default implementation of this method always copies**, and `Vec::push` doubling
    /// is the most common allocation pattern in Rust. A `Vec` grown to 1000 elements
    /// reallocates about ten times; without this, every one of those is a full `memcpy` of the
    /// entire buffer *and* a block abandoned somewhere else, which is exactly the churn a
    /// coalescing free list exists to prevent.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let Some(p) = NonNull::new(ptr) else {
            return core::ptr::null_mut();
        };

        // Scoped, because the lock is NOT reentrant: `self.alloc` below takes it again.
        // Holding it across that call would deadlock against ourselves, which is the
        // single-thread special case of DECISIONS.md §9.
        {
            let mut h = self.0.lock();
            // SAFETY: the caller's contract on `GlobalAlloc::realloc`.
            if unsafe { h.realloc_in_place(p, layout, new_size) } {
                return ptr;
            }
        }

        // Couldn't grow where it stands. Fall back to the honest way.
        let Ok(new_layout) = Layout::from_size_align(new_size, layout.align()) else {
            return core::ptr::null_mut();
        };

        // SAFETY: caller's contract, plus `new_layout` is valid.
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

/// Take frames from the physical allocator and hand them to the heap.
///
/// Must run **after** `mmu::init`: the heap deals in addresses, and an address is only
/// usable once something has mapped it.
pub fn init() {
    let first = memory::alloc_contiguous(HEAP_FRAMES)
        .expect("no contiguous run of frames for the kernel heap");

    // The allocator deals in PHYSICAL frames. The heap deals in addresses it can dereference.
    // The direct map is the bridge.
    let start = crate::arch::mmu::phys_to_virt(first.addr()) as usize;
    let size = HEAP_FRAMES * FRAME_SIZE as usize;

    // SAFETY: `alloc_contiguous` gave us these frames exclusively and contiguously, and
    // `mmu::init` direct-mapped all of RAM read/write, so this range is real, writable memory
    // that nobody else owns.
    unsafe { ALLOCATOR.0.lock().add_region(start, size) };
}

pub fn stats() -> (usize, usize) {
    let h = ALLOCATOR.0.lock();
    (h.allocated(), h.total())
}

pub fn print_summary() {
    let (used, total) = stats();
    println!(
        "  heap            : {} KiB, {} bytes in use  (Vec and Box now work)",
        total / 1024,
        used,
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

    /// Memory actually comes back. A leak here compounds silently until the kernel dies.
    #[test_case]
    fn the_heap_does_not_leak() {
        use alloc::vec::Vec;

        let (before, _) = crate::heap::stats();

        for _ in 0..200 {
            let v: Vec<u8> = Vec::with_capacity(1024);
            core::hint::black_box(&v);
            // dropped here
        }

        let (after, _) = crate::heap::stats();
        assert_eq!(
            after, before,
            "the heap leaked across 200 alloc/free cycles"
        );
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
