//! Host tests for the slab allocator.
//!
//! Pages come from the host heap, 4096-aligned. The slab's pointer arithmetic is identical to
//! what it does in the kernel; only the source of the pages differs.

use slab::{CLASSES, MAX_SIZE, PAGE_SIZE, SlabAllocator, class_for, class_size};
use std::alloc::Layout;
use std::cell::Cell;

/// A page source backed by the host heap. Leaks, deliberately: the objects must outlive it and
/// the test process is about to exit.
fn pages(budget: &Cell<usize>) -> impl FnOnce() -> Option<usize> + '_ {
    move || {
        if budget.get() == 0 {
            return None;
        }
        budget.set(budget.get() - 1);
        let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
        // SAFETY: nonzero size.
        let p = unsafe { std::alloc::alloc(layout) };
        assert!(!p.is_null());
        Some(p as usize)
    }
}

fn l(size: usize, align: usize) -> Layout {
    Layout::from_size_align(size, align).unwrap()
}

#[test]
fn size_classes_are_powers_of_two_from_16_to_2048() {
    assert_eq!(class_size(0), 16);
    assert_eq!(class_size(CLASSES - 1), 2048);
    assert_eq!(MAX_SIZE, 2048);

    // The smallest class that fits.
    assert_eq!(class_for(l(1, 1)), Some(0)); // 16
    assert_eq!(class_for(l(16, 1)), Some(0));
    assert_eq!(class_for(l(17, 1)), Some(1)); // 32
    assert_eq!(class_for(l(2048, 1)), Some(7));

    // Too big: falls through to the general heap.
    assert_eq!(class_for(l(2049, 1)), None);
}

#[test]
fn alignment_is_served_by_choosing_a_bigger_class() {
    // A slab's only alignment guarantee: an object of size `16 << i` inside a 4096-aligned page
    // is aligned to `16 << i`, and nothing stronger. So a strongly-aligned request needs a class
    // at least as large as its alignment.
    assert_eq!(class_for(l(16, 64)), Some(2)); // 64-byte class, for 64-byte alignment
    assert_eq!(class_for(l(8, 256)), Some(4)); // 256-byte class

    // And an alignment no class can serve correctly falls through, rather than silently
    // handing back a misaligned pointer.
    assert_eq!(class_for(l(16, 4096)), None);
}

#[test]
fn allocated_objects_are_aligned_to_their_class() {
    let budget = Cell::new(64);
    let mut s = SlabAllocator::new();

    for class in 0..CLASSES {
        let size = class_size(class);
        let p = s.alloc(l(size, size), pages(&budget)).unwrap();
        assert_eq!(
            p.as_ptr() as usize % size,
            0,
            "class {class} ({size} B) handed back a misaligned object"
        );
    }
}

#[test]
fn objects_are_distinct_and_usable() {
    let budget = Cell::new(4);
    let mut s = SlabAllocator::new();

    let mut got = Vec::new();
    for i in 0..64u8 {
        let p = s.alloc(l(64, 8), pages(&budget)).unwrap();
        // SAFETY: 64 bytes we own.
        unsafe { std::ptr::write_bytes(p.as_ptr(), i, 64) };
        got.push((p, i));
    }

    // 4096 / 64 = 64 objects, so exactly one page.
    assert_eq!(s.slabs(2), 1, "should have carved exactly one page");

    for (p, expected) in got {
        // SAFETY: still ours.
        unsafe {
            for j in 0..64 {
                assert_eq!(
                    *p.as_ptr().add(j),
                    expected,
                    "object {expected} was clobbered"
                );
            }
        }
    }
}

#[test]
fn a_freed_object_is_handed_straight_back() {
    // The whole point. A freed 64-byte object goes onto the 64-byte class's list and the very
    // next 64-byte request takes it as-is. No coalescing, no search.
    let budget = Cell::new(4);
    let mut s = SlabAllocator::new();

    let a = s.alloc(l(64, 8), pages(&budget)).unwrap();
    // SAFETY: matching layout.
    unsafe { s.dealloc(a, l(64, 8)) };

    let b = s.alloc(l(64, 8), pages(&budget)).unwrap();
    assert_eq!(a, b, "the freed object was not reused");
}

#[test]
fn a_class_takes_a_new_page_only_when_it_runs_dry() {
    let budget = Cell::new(4);
    let mut s = SlabAllocator::new();

    // 4096 / 128 = 32 objects per page.
    let mut live = Vec::new();
    for _ in 0..32 {
        live.push(s.alloc(l(128, 8), pages(&budget)).unwrap());
    }
    assert_eq!(s.slabs(3), 1);

    // The 33rd needs a second page.
    live.push(s.alloc(l(128, 8), pages(&budget)).unwrap());
    assert_eq!(s.slabs(3), 2);
}

#[test]
fn running_out_of_pages_returns_none() {
    let budget = Cell::new(0);
    let mut s = SlabAllocator::new();
    assert!(s.alloc(l(64, 8), pages(&budget)).is_none());
}

#[test]
fn classes_do_not_share_memory() {
    let budget = Cell::new(16);
    let mut s = SlabAllocator::new();

    let small = s.alloc(l(16, 8), pages(&budget)).unwrap();
    let big = s.alloc(l(2048, 8), pages(&budget)).unwrap();

    // SAFETY: ours.
    unsafe {
        std::ptr::write_bytes(small.as_ptr(), 0xaa, 16);
        std::ptr::write_bytes(big.as_ptr(), 0xbb, 2048);
        assert_eq!(
            *small.as_ptr(),
            0xaa,
            "the 2048 class overwrote the 16 class"
        );
    }
}

// --- the reason this crate exists ---

#[test]
fn the_pathological_case_is_not_pathological_here() {
    // THIS is the workload that makes the general heap's free list explode to 1001 blocks:
    // allocate many same-sized objects, then free every OTHER one, so no two freed blocks are
    // adjacent and coalescing can do nothing.
    //
    // For a slab it is not a special case at all. Every freed object goes back to its class's
    // list and is immediately reusable, because everything in the class is the same size.
    // Coalescing is not fast here; it is UNNECESSARY.
    let budget = Cell::new(64);
    let mut s = SlabAllocator::new();

    let mut live = Vec::new();
    for _ in 0..2000 {
        live.push(s.alloc(l(64, 8), pages(&budget)).unwrap());
    }

    for (i, p) in live.iter().enumerate() {
        if i % 2 == 0 {
            // SAFETY: matching layout.
            unsafe { s.dealloc(*p, l(64, 8)) };
        }
    }

    // 4096/64 = 64 objects per page, so 2000 live objects needed 32 pages = 2048 objects, of
    // which 48 were never handed out. Free 1000 of the live ones and the class's list holds
    // 1048.
    //
    // The number is not the point. The point is that FINDING one is a single pointer load,
    // where the general heap would be walking a 1001-block list.
    assert_eq!(s.free_objects(2), 1048);

    // Prove it: the next 1000 allocations all succeed, taking NO new pages.
    let slabs_before = s.slabs(2);
    for _ in 0..1000 {
        s.alloc(l(64, 8), || {
            panic!("took a new page when 1000 objects were free")
        })
        .unwrap();
    }
    assert_eq!(s.slabs(2), slabs_before);
}

#[test]
fn thrashing_reuses_memory_rather_than_growing() {
    let budget = Cell::new(8);
    let mut s = SlabAllocator::new();

    for _ in 0..10_000 {
        let a = s.alloc(l(64, 8), pages(&budget)).unwrap();
        let b = s.alloc(l(256, 8), pages(&budget)).unwrap();
        // SAFETY: matching layouts.
        unsafe {
            s.dealloc(a, l(64, 8));
            s.dealloc(b, l(256, 8));
        }
    }

    assert_eq!(s.allocated(), 0, "leaked");
    // One page for the 64 class, one for the 256 class, and that is all 10,000 rounds cost.
    assert_eq!(
        s.capacity(),
        2 * PAGE_SIZE,
        "the slab grew under a steady workload"
    );
}

#[test]
fn accounting_is_honest() {
    let budget = Cell::new(8);
    let mut s = SlabAllocator::new();

    let p = s.alloc(l(100, 8), pages(&budget)).unwrap();

    // 100 rounds up to the 128 class. The allocator must report what it actually took, not
    // what was asked for. The difference is internal fragmentation, and it is the price a slab
    // pays for O(1) everything.
    assert_eq!(s.allocated(), 128);

    // SAFETY: matching layout.
    unsafe { s.dealloc(p, l(100, 8)) };
    assert_eq!(s.allocated(), 0);
}
