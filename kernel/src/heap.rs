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
}

/// Take frames from the physical allocator and hand them to the heap.
///
/// Must run **after** `mmu::init`: the heap deals in addresses, and an address is only
/// usable once something has mapped it.
pub fn init() {
    let first = memory::alloc_contiguous(HEAP_FRAMES)
        .expect("no contiguous run of frames for the kernel heap");

    let start = first.addr() as usize;
    let size = HEAP_FRAMES * FRAME_SIZE as usize;

    // SAFETY: `alloc_contiguous` gave us these frames exclusively, they are contiguous, and
    // `mmu::init` identity-mapped all of RAM read/write, so this range is real, writable
    // memory that nobody else owns.
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
