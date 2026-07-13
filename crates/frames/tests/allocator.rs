//! Host tests for the frame allocator. No emulator, no hardware, milliseconds.

use frames::{FRAME_SIZE, Frame, FrameAllocator};

const BASE: u64 = 0x4000_0000;

/// 64 frames = 256 KiB of pretend RAM, 8 bytes of bitmap.
fn allocator(bitmap: &mut [u8]) -> FrameAllocator<'_> {
    let mut a = FrameAllocator::new(BASE, 64, bitmap);
    a.mark_free(BASE, 64 * FRAME_SIZE);
    a
}

#[test]
fn everything_starts_used() {
    let mut bits = [0u8; 8];
    let a = FrameAllocator::new(BASE, 64, &mut bits);

    // Memory is guilty until proven innocent. If this ever defaults to free, the
    // allocator will happily hand out the MMIO hole where the UART lives.
    assert_eq!(a.stats().used, 64);
    assert_eq!(a.stats().free(), 0);
}

#[test]
fn alloc_hands_out_distinct_frames() {
    let mut bits = [0u8; 8];
    let mut a = allocator(&mut bits);

    let mut seen = Vec::new();
    while let Some(f) = a.alloc() {
        assert!(!seen.contains(&f), "handed out {f:?} twice");
        assert_eq!(f.addr() % FRAME_SIZE, 0, "not frame-aligned");
        seen.push(f);
    }

    assert_eq!(seen.len(), 64);
    assert_eq!(a.stats().free(), 0);
}

#[test]
fn free_returns_a_frame_to_the_pool() {
    let mut bits = [0u8; 8];
    let mut a = allocator(&mut bits);

    let f = a.alloc().unwrap();
    assert_eq!(a.stats().used, 1);

    a.free(f);
    assert_eq!(a.stats().used, 0);

    assert_eq!(a.alloc(), Some(f), "should reuse the frame we just freed");
}

#[test]
#[should_panic(expected = "double free")]
fn double_free_panics() {
    let mut bits = [0u8; 8];
    let mut a = allocator(&mut bits);

    let f = a.alloc().unwrap();
    a.free(f);
    a.free(f);
}

#[test]
#[should_panic(expected = "don't own")]
fn freeing_a_foreign_frame_panics() {
    let mut bits = [0u8; 8];
    let mut a = allocator(&mut bits);
    a.free(Frame::from_addr(0xdead_0000));
}

// --- the one that actually matters ---

#[test]
fn mark_used_claims_partially_covered_frames() {
    let mut bits = [0u8; 8];
    let mut a = allocator(&mut bits);

    // A region covering frame 0 entirely and ONE BYTE of frame 1.
    //
    // This is the shape of our real kernel image, which ends at 0x40097010: not
    // frame-aligned. If mark_used rounds the end DOWN, frame 1 stays free, gets handed
    // out, and something writes over the tail of the kernel.
    a.mark_used(BASE, FRAME_SIZE + 1);

    assert_eq!(a.stats().used, 2, "must claim BOTH frames, not just the full one");

    // And prove it by exhausting the allocator: neither frame may come back.
    let mut handed_out = Vec::new();
    while let Some(f) = a.alloc() {
        handed_out.push(f.addr());
    }
    assert!(!handed_out.contains(&BASE), "handed out frame 0");
    assert!(
        !handed_out.contains(&(BASE + FRAME_SIZE)),
        "handed out frame 1, which contains the last byte of the kernel"
    );
}

#[test]
fn mark_used_of_an_unaligned_start_claims_the_frame_it_starts_in() {
    let mut bits = [0u8; 8];
    let mut a = allocator(&mut bits);

    // Starts halfway through frame 2, ends halfway through frame 3.
    a.mark_used(BASE + 2 * FRAME_SIZE + 100, FRAME_SIZE);

    assert_eq!(a.stats().used, 2, "must claim frames 2 and 3");
}

#[test]
fn mark_used_is_idempotent() {
    let mut bits = [0u8; 8];
    let mut a = allocator(&mut bits);

    a.mark_used(BASE, 4 * FRAME_SIZE);
    a.mark_used(BASE, 4 * FRAME_SIZE);
    a.mark_used(BASE + FRAME_SIZE, FRAME_SIZE);

    // The kernel image, the DTB, and the bitmap can overlap. Reserving a frame twice
    // must not corrupt the used count, or the allocator's accounting drifts and the
    // "free memory" number in the banner becomes a lie.
    assert_eq!(a.stats().used, 4);
}

#[test]
fn regions_outside_our_range_are_ignored() {
    let mut bits = [0u8; 8];
    let mut a = allocator(&mut bits);

    // The device tree can describe reserved regions in memory we don't manage (MMIO,
    // secure-world firmware). Clamping rather than panicking is the whole point.
    a.mark_used(0, FRAME_SIZE); // below our base
    a.mark_used(BASE + 1000 * FRAME_SIZE, FRAME_SIZE); // above our top

    assert_eq!(a.stats().used, 0);
}

// --- contiguous allocation, the reason we're a bitmap ---

#[test]
fn alloc_contiguous_returns_adjacent_frames() {
    let mut bits = [0u8; 8];
    let mut a = allocator(&mut bits);

    let first = a.alloc_contiguous(4).unwrap();
    assert_eq!(a.stats().used, 4);

    for i in 0..4u64 {
        let f = Frame::from_addr(first.addr() + i * FRAME_SIZE);
        assert_eq!(a.is_used(f), Some(true), "frame {i} of the run isn't marked");
    }
}

#[test]
fn alloc_contiguous_skips_a_fragmented_gap() {
    let mut bits = [0u8; 8];
    let mut a = FrameAllocator::new(BASE, 64, &mut bits);
    a.mark_free(BASE, 64 * FRAME_SIZE);

    // Punch a hole so the first plausible run is too short: frames 0-2 free, 3 used,
    // then everything else free. A request for 4 must skip past the hole.
    a.mark_used(BASE + 3 * FRAME_SIZE, FRAME_SIZE);

    let first = a.alloc_contiguous(4).unwrap();
    assert_eq!(
        first.addr(),
        BASE + 4 * FRAME_SIZE,
        "should start after the hole, not before it"
    );
}

#[test]
fn alloc_contiguous_fails_when_no_run_is_long_enough() {
    let mut bits = [0u8; 8];
    let mut a = FrameAllocator::new(BASE, 64, &mut bits);
    a.mark_free(BASE, 64 * FRAME_SIZE);

    // Reserve every other frame. Now the longest free run is 1.
    for i in (0..64).step_by(2) {
        a.mark_used(BASE + i * FRAME_SIZE, FRAME_SIZE);
    }

    assert_eq!(a.alloc_contiguous(2), None);
    assert!(a.alloc_contiguous(1).is_some());
}

#[test]
fn alloc_contiguous_of_everything_works_exactly_once() {
    let mut bits = [0u8; 8];
    let mut a = allocator(&mut bits);

    assert!(a.alloc_contiguous(64).is_some());
    assert_eq!(a.stats().free(), 0);
    assert_eq!(a.alloc_contiguous(1), None);
}

#[test]
fn bitmap_sizing() {
    assert_eq!(FrameAllocator::bitmap_bytes(0), 0);
    assert_eq!(FrameAllocator::bitmap_bytes(1), 1);
    assert_eq!(FrameAllocator::bitmap_bytes(8), 1);
    assert_eq!(FrameAllocator::bitmap_bytes(9), 2);

    // 128 MiB of RAM (what QEMU virt gives us) is 32768 frames, so 4 KiB of bitmap.
    // One frame's worth of overhead to manage 128 MiB. That's the deal.
    assert_eq!(FrameAllocator::frames_in(0x800_0000), 32768);
    assert_eq!(FrameAllocator::bitmap_bytes(32768), 4096);
}
