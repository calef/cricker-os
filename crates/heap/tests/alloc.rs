//! Host tests for the kernel heap.
//!
//! The arena is a real, aligned host allocation. The heap's pointer arithmetic is identical
//! to what it will do in the kernel; only the source of the memory differs.

use heap::{Heap, MIN_BLOCK};
use std::alloc::Layout;

/// A 64 KiB arena, 4096-aligned, leaked so it outlives everything.
fn arena(size: usize) -> (usize, usize) {
    let layout = Layout::from_size_align(size, 4096).unwrap();
    // SAFETY: nonzero size.
    let ptr = unsafe { std::alloc::alloc(layout) };
    assert!(!ptr.is_null());
    (ptr as usize, size)
}

fn heap_with(size: usize) -> Heap {
    let (start, size) = arena(size);
    let mut h = Heap::new();
    // SAFETY: the arena is ours, mapped, writable, and leaked.
    unsafe { h.add_region(start, size) };
    h
}

fn layout(size: usize, align: usize) -> Layout {
    Layout::from_size_align(size, align).unwrap()
}

#[test]
fn allocates_and_the_memory_is_usable() {
    let mut h = heap_with(64 * 1024);

    let p = h.alloc(layout(100, 8)).expect("should allocate");

    // SAFETY: the heap just gave us 100 bytes.
    unsafe {
        std::ptr::write_bytes(p.as_ptr(), 0xab, 100);
        assert_eq!(*p.as_ptr(), 0xab);
        assert_eq!(*p.as_ptr().add(99), 0xab);
    }
}

#[test]
fn allocations_do_not_overlap() {
    let mut h = heap_with(64 * 1024);

    let mut blocks = Vec::new();
    for i in 0..64u8 {
        let p = h.alloc(layout(64, 8)).unwrap();
        // SAFETY: 64 bytes we own.
        unsafe { std::ptr::write_bytes(p.as_ptr(), i, 64) };
        blocks.push((p, i));
    }

    // If any two overlapped, an earlier block's bytes would have been overwritten.
    for (p, expected) in blocks {
        // SAFETY: still ours.
        unsafe {
            for j in 0..64 {
                assert_eq!(*p.as_ptr().add(j), expected, "block {expected} was clobbered");
            }
        }
    }
}

#[test]
fn freed_memory_comes_back() {
    let mut h = heap_with(64 * 1024);
    let before = h.free();

    let p = h.alloc(layout(1000, 8)).unwrap();
    assert!(h.free() < before);

    // SAFETY: same layout we allocated with.
    unsafe { h.dealloc(p, layout(1000, 8)) };
    assert_eq!(h.free(), before, "freeing did not return the memory");
}

#[test]
fn running_out_returns_none_rather_than_panicking() {
    let mut h = heap_with(4096);

    assert!(h.alloc(layout(8192, 8)).is_none(), "asked for more than exists");

    // And the heap is still usable afterwards.
    assert!(h.alloc(layout(64, 8)).is_some());
}

#[test]
fn alignment_is_honoured() {
    let mut h = heap_with(64 * 1024);

    for align in [16usize, 32, 64, 128, 256, 512, 1024, 2048, 4096] {
        let p = h.alloc(layout(64, align)).unwrap_or_else(|| panic!("align {align}"));
        assert_eq!(
            p.as_ptr() as usize % align,
            0,
            "alignment {align} not honoured"
        );
    }
}

#[test]
fn a_large_alignment_does_not_leak_the_gap_before_it() {
    // Asking for 4096-alignment inside an arena that isn't at a 4096 boundary leaves a gap
    // at the front. That gap must go back on the free list, not vanish.
    //
    // This is the failure mode of the naive implementation: it aligns forward and forgets
    // what it stepped over. You then leak a few hundred bytes per aligned allocation, and
    // the heap slowly dies over hours in a way no single test would catch.
    let mut h = heap_with(64 * 1024);
    let before = h.free();

    let p = h.alloc(layout(64, 4096)).unwrap();
    // SAFETY: same layout.
    unsafe { h.dealloc(p, layout(64, 4096)) };

    assert_eq!(h.free(), before, "the alignment gap was leaked");
}

// --- coalescing: the thing that keeps the heap alive over time ---

#[test]
fn adjacent_frees_merge_back_into_one_block() {
    // Without coalescing, this heap is dead. Allocate three blocks, free all three, and the
    // free list should be ONE big block again, not three fragments.
    //
    // We prove it by asking for something bigger than any individual piece.
    let mut h = heap_with(64 * 1024);

    let a = h.alloc(layout(4096, 16)).unwrap();
    let b = h.alloc(layout(4096, 16)).unwrap();
    let c = h.alloc(layout(4096, 16)).unwrap();

    // SAFETY: matching layouts.
    unsafe {
        h.dealloc(a, layout(4096, 16));
        h.dealloc(b, layout(4096, 16));
        h.dealloc(c, layout(4096, 16));
    }

    // 12 KiB is bigger than any of the three pieces. It only fits if they merged.
    assert!(
        h.alloc(layout(12 * 1024, 16)).is_some(),
        "the three freed blocks did not coalesce"
    );
}

#[test]
fn freeing_between_two_free_blocks_collapses_all_three() {
    // The three-way merge. Free A, free C, then free B (which sits between them). B must
    // absorb both neighbours in one pass, leaving a single block.
    let mut h = heap_with(64 * 1024);

    let a = h.alloc(layout(4096, 16)).unwrap();
    let b = h.alloc(layout(4096, 16)).unwrap();
    let c = h.alloc(layout(4096, 16)).unwrap();
    let guard = h.alloc(layout(64, 16)).unwrap(); // stops C merging with the tail

    // SAFETY: matching layouts.
    unsafe {
        h.dealloc(a, layout(4096, 16));
        h.dealloc(c, layout(4096, 16));
        h.dealloc(b, layout(4096, 16)); // <- the one in the middle
    }

    assert!(
        h.alloc(layout(12 * 1024, 16)).is_some(),
        "A + B + C did not collapse into one"
    );

    // SAFETY: matching layout.
    unsafe { h.dealloc(guard, layout(64, 16)) };
}

#[test]
fn thrashing_does_not_fragment_the_heap_to_death() {
    // The real test of coalescing. Allocate and free in a churning pattern thousands of
    // times. Without merging, the free list fragments into dust: thousands of 16-byte
    // blocks, no room for anything, while reporting plenty of free memory.
    //
    // At the end we ask for nearly the whole arena. It only succeeds if the heap is still
    // one big block.
    let mut h = heap_with(64 * 1024);
    let total_free = h.free();

    for round in 0..2000usize {
        let size = 16 + (round * 17) % 512;
        let l = layout(size, 16);

        let a = h.alloc(l).expect("allocation failed mid-thrash");
        let b = h.alloc(l).expect("allocation failed mid-thrash");

        // SAFETY: matching layouts.
        unsafe {
            h.dealloc(a, l);
            h.dealloc(b, l);
        }
    }

    assert_eq!(h.free(), total_free, "memory leaked during the thrash");
    assert!(
        h.alloc(layout(total_free - 16, 16)).is_some(),
        "the heap fragmented: {} bytes free but cannot allocate them",
        h.free()
    );
}

#[test]
fn accounting_is_honest() {
    let mut h = heap_with(64 * 1024);
    assert_eq!(h.allocated(), 0);
    assert_eq!(h.free(), h.total());

    let p = h.alloc(layout(100, 8)).unwrap();

    // 100 rounds up to 112 (a multiple of MIN_BLOCK). The heap must report what it actually
    // took, not what was asked for, or `free()` and `allocated()` drift apart forever.
    assert_eq!(h.allocated() % MIN_BLOCK, 0);
    assert_eq!(h.allocated() + h.free(), h.total());

    // SAFETY: matching layout.
    unsafe { h.dealloc(p, layout(100, 8)) };
    assert_eq!(h.allocated(), 0);
}

#[test]
fn a_zero_sized_allocation_still_gets_a_unique_address() {
    // Rust does allow `Layout` with size 0 through some paths. Handing back two identical
    // pointers, or null, would be a soundness hole. Round up to MIN_BLOCK and move on.
    let mut h = heap_with(64 * 1024);

    let a = h.alloc(layout(0, 1)).unwrap();
    let b = h.alloc(layout(0, 1)).unwrap();
    assert_ne!(a, b);
}

#[test]
fn a_region_too_small_to_track_is_dropped_not_trusted() {
    let mut h = Heap::new();
    let (start, _) = arena(4096);

    // Eight bytes cannot hold a 16-byte free-block header. Accepting it would put a node on
    // the list that overruns its own block.
    // SAFETY: the arena is real; we're just describing 8 bytes of it.
    unsafe { h.add_region(start, 8) };

    assert_eq!(h.total(), 0);
    assert!(h.alloc(layout(8, 8)).is_none());
}

// --- realloc: the thing that makes Vec::push not pathological ---

#[test]
fn growing_into_free_space_above_does_not_move_the_block() {
    // The common case for a Vec that is the only thing growing.
    let mut h = heap_with(64 * 1024);

    let l = layout(1024, 16);
    let p = h.alloc(l).unwrap();

    // SAFETY: matching layout, and there is nothing but free space above.
    let moved = unsafe { !h.realloc_in_place(p, l, 2048) };

    assert!(!moved, "realloc_in_place refused a growth into free space above it");

    // Same address. Not one byte was copied.
    assert_eq!(h.allocated(), 2048);
}

#[test]
fn growing_when_blocked_fails_so_the_caller_can_fall_back() {
    let mut h = heap_with(64 * 1024);

    let l = layout(1024, 16);
    let a = h.alloc(l).unwrap();
    let _blocker = h.alloc(layout(16, 16)).unwrap(); // sits immediately above `a`

    // SAFETY: matching layout.
    let grew = unsafe { h.realloc_in_place(a, l, 2048) };

    assert!(!grew, "grew in place over the top of another allocation");
}

#[test]
fn shrinking_never_copies_and_returns_the_tail() {
    // The default GlobalAlloc::realloc copies even when SHRINKING, which is simply silly.
    let mut h = heap_with(64 * 1024);

    let l = layout(4096, 16);
    let p = h.alloc(l).unwrap();
    assert_eq!(h.allocated(), 4096);

    // SAFETY: matching layout.
    let ok = unsafe { h.realloc_in_place(p, l, 1024) };

    assert!(ok, "shrinking should always succeed: there is nothing to search for");
    assert_eq!(h.allocated(), 1024, "the tail was not returned to the free list");

    // And the 3 KiB we handed back is genuinely usable again.
    assert!(h.alloc(layout(3072 - 16, 16)).is_some());
}

#[test]
fn realloc_preserves_the_data_it_did_not_move() {
    let mut h = heap_with(64 * 1024);

    let l = layout(256, 16);
    let p = h.alloc(l).unwrap();
    // SAFETY: 256 bytes we own.
    unsafe { std::ptr::write_bytes(p.as_ptr(), 0x5a, 256) };

    // SAFETY: matching layout.
    assert!(unsafe { h.realloc_in_place(p, l, 512) });

    // Growing in place means the original bytes are exactly where they were.
    // SAFETY: still ours, now 512 bytes.
    unsafe {
        for i in 0..256 {
            assert_eq!(*p.as_ptr().add(i), 0x5a, "byte {i} changed during an in-place grow");
        }
    }
}

#[test]
fn a_vec_shaped_growth_pattern_stays_in_place() {
    // Simulate `Vec::push` doubling: 16 -> 32 -> 64 -> ... -> 8192, one allocation, nothing
    // else competing for the space above it.
    //
    // WITHOUT realloc-in-place, every doubling is a fresh allocation plus a full memcpy plus
    // a free, and each move abandons a block somewhere else. That is the allocator generating
    // its own fragmentation on the hottest path in Rust.
    let mut h = heap_with(64 * 1024);

    let mut size = 16usize;
    let mut l = layout(size, 16);
    let p = h.alloc(l).unwrap();
    let original = p.as_ptr();

    let mut in_place = 0;
    while size < 8192 {
        let next = size * 2;
        // SAFETY: matching layout each time.
        if unsafe { h.realloc_in_place(p, l, next) } {
            in_place += 1;
        }
        size = next;
        l = layout(size, 16);
    }

    assert_eq!(in_place, 9, "some doublings had to move");
    assert_eq!(p.as_ptr(), original, "the buffer moved");
    assert_eq!(h.allocated(), 8192);
}
