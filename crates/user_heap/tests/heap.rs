//! Host tests for the userspace heap algorithm. These prove the invariants QEMU never would
//! cheaply: coalescing back to one block, alignment beyond the grid, sliver-free splitting, and
//! the grow-at-top pattern `user_rt::heap` relies on.

use core::alloc::Layout;

use user_heap::{Heap, MIN_ALIGN, effective_size};

/// A 16-aligned arena on the host stack-of-the-test; the heap only ever sees pointers into it.
#[repr(align(4096))]
struct Arena([u8; 64 * 1024]);

fn arena() -> Box<Arena> {
    Box::new(Arena([0; 64 * 1024]))
}

fn layout(size: usize, align: usize) -> Layout {
    Layout::from_size_align(size, align).unwrap()
}

#[test]
fn everything_freed_coalesces_to_one_block() {
    let mut a = arena();
    let mut h = Heap::new();
    // SAFETY: the arena is a freshly boxed 4096-aligned buffer this test owns; nothing else touches it while `h` lives.
    unsafe { h.add_region(a.0.as_mut_ptr(), a.0.len()) };
    assert_eq!(h.block_count(), 1);

    // Allocate a mix of sizes, then free in an order that is neither LIFO nor FIFO. If any
    // adjacency case in insert_free is wrong, the final block count betrays it.
    let sizes = [24usize, 1, 4096, 100, 16, 333, 2048, 7];
    let ptrs: Vec<_> = sizes
        .iter()
        .map(|&s| (h.alloc(layout(s, 8)).unwrap(), s))
        .collect();
    for i in [3, 0, 6, 1, 7, 2, 5, 4] {
        let (p, s) = ptrs[i];
        // SAFETY: each `p` came from `h.alloc` above with exactly this layout, and the index permutation frees each one once.
        unsafe { h.dealloc(p, layout(s, 8)) };
    }
    assert_eq!(h.block_count(), 1);
    assert_eq!(h.free_bytes(), a.0.len());
}

#[test]
fn effective_size_is_the_grid() {
    assert_eq!(effective_size(layout(1, 1)), MIN_ALIGN);
    assert_eq!(effective_size(layout(16, 8)), 16);
    assert_eq!(effective_size(layout(17, 1)), 32);
    // A zero-size allocation still costs one grid cell, so the pointer is unique and freeable.
    assert_eq!(
        effective_size(Layout::from_size_align(0, 1).unwrap()),
        MIN_ALIGN
    );
}

#[test]
fn over_aligned_allocations_are_aligned_and_leak_nothing() {
    let mut a = arena();
    let total = a.0.len();
    let mut h = Heap::new();
    // SAFETY: the arena is a freshly boxed 4096-aligned buffer this test owns; nothing else touches it while `h` lives.
    unsafe { h.add_region(a.0.as_mut_ptr(), total) };

    // Force front padding: a small allocation first skews the next block off 4096 alignment.
    let small = h.alloc(layout(16, 16)).unwrap();
    let big = h.alloc(layout(8192, 4096)).unwrap();
    assert_eq!(big.as_ptr().addr() % 4096, 0);

    // SAFETY: `big` came from `h.alloc` with exactly this layout, and is freed once.
    unsafe { h.dealloc(big, layout(8192, 4096)) };
    // SAFETY: `small` came from `h.alloc` with exactly this layout, and is freed once.
    unsafe { h.dealloc(small, layout(16, 16)) };
    // The front-padding remainder must have been a real free block, not a leaked sliver.
    assert_eq!(h.block_count(), 1);
    assert_eq!(h.free_bytes(), total);
}

#[test]
fn exhaustion_returns_none_and_growth_at_the_top_coalesces() {
    let mut a = arena();
    let mut h = Heap::new();
    // Donate only the first 8 KiB; keep the rest as "unmapped pages" to grow into.
    let (first, rest) = a.0.split_at_mut(8 * 1024);
    // SAFETY: `first` is the low half of an arena this test owns, handed over whole.
    unsafe { h.add_region(first.as_mut_ptr(), first.len()) };

    assert!(
        h.alloc(layout(16 * 1024, 8)).is_none(),
        "must refuse, not corrupt"
    );

    // Grow the way user_rt::heap does: donate the pages right above the committed top.
    // SAFETY: `rest` is the OTHER half of the same `split_at_mut`, so it is disjoint from the region donated above, which is what `add_region` requires of a second donation.
    unsafe { h.add_region(rest.as_mut_ptr(), 16 * 1024) };
    // The donation is adjacent to the free tail of the first region, so a block bigger than
    // either donation alone must now exist.
    let p = h.alloc(layout(16 * 1024, 8)).unwrap();
    // SAFETY: `p` came from `h.alloc` with exactly this layout, and is freed once.
    unsafe { h.dealloc(p, layout(16 * 1024, 8)) };
    assert_eq!(h.block_count(), 1);
}

#[test]
fn first_fit_reuses_a_hole_of_exactly_the_right_size() {
    let mut a = arena();
    let mut h = Heap::new();
    // SAFETY: the arena is a freshly boxed 4096-aligned buffer this test owns; nothing else touches it while `h` lives.
    unsafe { h.add_region(a.0.as_mut_ptr(), a.0.len()) };

    let keep_a = h.alloc(layout(64, 8)).unwrap();
    let hole = h.alloc(layout(128, 8)).unwrap();
    let keep_b = h.alloc(layout(64, 8)).unwrap();
    // SAFETY: `hole` came from `h.alloc` with exactly this layout, and is freed once.
    unsafe { h.dealloc(hole, layout(128, 8)) };

    // The freed hole sits below the big tail block; first-fit must land exactly in it.
    let p = h.alloc(layout(128, 8)).unwrap();
    assert_eq!(p, hole);

    // SAFETY: `p` came from `h.alloc` with exactly this layout, and is freed once.
    unsafe { h.dealloc(p, layout(128, 8)) };
    // SAFETY: `keep_a` came from `h.alloc` with exactly this layout, and is freed once.
    unsafe { h.dealloc(keep_a, layout(64, 8)) };
    // SAFETY: `keep_b` came from `h.alloc` with exactly this layout, and is freed once.
    unsafe { h.dealloc(keep_b, layout(64, 8)) };
    assert_eq!(h.block_count(), 1);
}

#[test]
fn thrashing_does_not_fragment_the_heap_to_death() {
    // The kernel heap's milestone-4 test, re-proven for the userspace twin: interleaved
    // allocate/free churn must end with the heap as one block, not confetti.
    let mut a = arena();
    let mut h = Heap::new();
    // SAFETY: the arena is a freshly boxed 4096-aligned buffer this test owns; nothing else touches it while `h` lives.
    unsafe { h.add_region(a.0.as_mut_ptr(), a.0.len()) };

    let mut live: Vec<(core::ptr::NonNull<u8>, Layout)> = Vec::new();
    for round in 0..1000usize {
        let size = 1 + (round * 37) % 500;
        let l = layout(size, 8);
        if let Some(p) = h.alloc(l) {
            // Scribble over the whole allocation: if the heap ever hands out overlapping
            // blocks, the free-list nodes get corrupted and a later op explodes.
            // SAFETY: `p` is a live allocation of at least `size` bytes, so writing `size` bytes into it stays in bounds. Scribbling is the point: overlapping blocks would corrupt a free-list node.
            unsafe { core::ptr::write_bytes(p.as_ptr(), 0xA5, size) };
            live.push((p, l));
        }
        if round % 3 == 0 && !live.is_empty() {
            let (p, l) = live.remove(round % live.len());
            // SAFETY: `p` and `l` were pushed together after a successful `alloc`, and `remove` takes each pair out exactly once.
            unsafe { h.dealloc(p, l) };
        }
    }
    for (p, l) in live.drain(..) {
        // SAFETY: the pairs still live, each allocated with that layout and not yet freed.
        unsafe { h.dealloc(p, l) };
    }
    assert_eq!(h.block_count(), 1);
    assert_eq!(h.free_bytes(), a.0.len());
}
