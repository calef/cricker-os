//! How bad is the O(n) actually?
//!
//! Both `alloc` (first fit) and `dealloc` (address-sorted insert) walk the free list. So the
//! cost of *both* operations is the **number of free blocks**, and the whole question is how
//! large that number gets under a realistic workload.
//!
//! Measure before optimizing.

use heap::Heap;
use std::alloc::Layout;

fn heap_with(size: usize) -> Heap {
    let layout = Layout::from_size_align(size, 4096).unwrap();
    // SAFETY: nonzero size; leaked deliberately.
    let ptr = unsafe { std::alloc::alloc(layout) };
    let mut h = Heap::new();
    // SAFETY: the arena is ours.
    unsafe { h.add_region(ptr as usize, size) };
    h
}

fn l(size: usize) -> Layout {
    Layout::from_size_align(size, 16).unwrap()
}

#[test]
fn measure_the_free_list_under_realistic_workloads() {
    println!();
    println!("  free-list length (= the n in O(n)) under different patterns:");
    println!();

    // 1. Uniform sizes, LIFO free. The kernel's common case: thread structs, file
    //    descriptors, inode entries. Lots of same-sized objects.
    {
        let mut h = heap_with(1024 * 1024);
        let mut live = Vec::new();
        for _ in 0..1000 {
            live.push(h.alloc(l(64)).unwrap());
        }
        let peak = h.free_blocks();
        for p in live.drain(..).rev() {
            // SAFETY: matching layout.
            unsafe { h.dealloc(p, l(64)) };
        }
        println!(
            "     uniform 64B, 1000 live      : {peak:>5} free blocks (after free: {})",
            h.free_blocks()
        );
        assert!(peak <= 2, "uniform allocation should not fragment at all");
    }

    // 2. Uniform sizes, free every other one. The pathological case for coalescing: every
    //    freed block is isolated between two live ones, so nothing can merge.
    {
        let mut h = heap_with(1024 * 1024);
        let mut live = Vec::new();
        for _ in 0..2000 {
            live.push(h.alloc(l(64)).unwrap());
        }
        for (i, p) in live.iter().enumerate() {
            if i % 2 == 0 {
                // SAFETY: matching layout.
                unsafe { h.dealloc(*p, l(64)) };
            }
        }
        println!(
            "     64B, every OTHER one freed  : {:>5} free blocks   <-- worst case",
            h.free_blocks()
        );
    }

    // 3. Mixed sizes, random-ish free order. A more honest kernel workload.
    {
        let mut h = heap_with(1024 * 1024);
        let mut live: Vec<(std::ptr::NonNull<u8>, usize)> = Vec::new();

        for i in 0..3000usize {
            let size = 16 << (i % 5); // 16, 32, 64, 128, 256
            if let Some(p) = h.alloc(l(size)) {
                live.push((p, size));
            }
            // free something older, out of order
            if i % 3 == 0 && live.len() > 10 {
                let j = (i * 7) % live.len();
                let (p, size) = live.swap_remove(j);
                // SAFETY: matching layout.
                unsafe { h.dealloc(p, l(size)) };
            }
        }
        println!(
            "     mixed 16-256B, out of order : {:>5} free blocks",
            h.free_blocks()
        );
    }

    println!();
}
