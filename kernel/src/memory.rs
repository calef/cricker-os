//! Physical memory: find out where RAM is, and hand it out in 4 KiB frames.
//!
//! This is the bottom of the memory hierarchy. Page tables (milestone 4), the kernel
//! heap, DMA buffers (milestone 8), and user process pages (milestone 7) all ultimately
//! ask this for their memory, and there is nothing underneath it to ask.
//!
//! The allocator itself lives in the `frames` crate and the device tree parser in
//! `dtb`, because both are pure logic and belong in host-testable crates (DECISIONS.md
//! §7). What's left here is the part that can only happen on the real machine: the
//! **bootstrap**.

// The allocator's API exists before its callers do. `alloc`/`free` are exercised by the
// kernel tests today, and milestone 4 (page tables) is the first non-test consumer.
#![allow(dead_code)]

use crate::println;
use dtb::{Dtb, Region};
use frames::{FRAME_SIZE, Frame, FrameAllocator, Stats};
use spin::Mutex;

/// TODO (milestone 5): this lock is not interrupt-safe.
///
/// Today there is one core and no interrupts, so a spinlock is a formality
/// (DECISIONS.md §6). The moment an interrupt handler wants to allocate a frame while
/// the interrupted code is holding this lock, we deadlock instantly and permanently:
/// the handler spins waiting for a lock that only the code it interrupted can release.
///
/// The fix is a lock that disables interrupts while held. We will need a written-down
/// locking discipline before milestone 5, not after it.
static ALLOCATOR: Mutex<Option<FrameAllocator<'static>>> = Mutex::new(None);

/// The most `/memory` nodes and `/memreserve` entries we'll cope with.
///
/// QEMU's `virt` has exactly one of the former and none of the latter. Real boards have
/// more, and a fixed-size array is the right shape here because **we have no heap yet**.
/// The `Vec` we'd reach for in userspace is precisely the thing this milestone is a
/// prerequisite for.
const MAX_REGIONS: usize = 16;

pub fn init(dtb_ptr: usize) {
    // SAFETY: QEMU handed us this pointer in x0 under the Linux boot protocol, and two
    // tests assert that it is nonzero and carries the DTB magic. `from_ptr` re-checks
    // the magic before trusting anything else in the blob.
    let dtb = unsafe { Dtb::from_ptr(dtb_ptr as *const u8) }.expect("device tree is unreadable");

    let mut ram = [Region { start: 0, size: 0 }; MAX_REGIONS];
    let ram_count = dtb
        .memory_regions(&mut ram)
        .expect("cannot read the memory map");
    let ram = &ram[..ram_count];
    assert!(!ram.is_empty(), "the device tree describes no RAM at all");

    let mut reserved = [Region { start: 0, size: 0 }; MAX_REGIONS];
    let reserved_count = dtb
        .reserved_regions(&mut reserved)
        .expect("cannot read the memory reservations");
    let reserved = &reserved[..reserved_count];

    // The whole span we have to be able to describe. Note this is the *span*, not the
    // *sum*: if a board has RAM at 0x4000_0000 and again at 0x8_0000_0000, we track
    // every frame between them and simply never free the hole. A bit of wasted bitmap
    // buys a much simpler index calculation.
    let base = ram.iter().map(|r| r.start).min().unwrap();
    let top = ram.iter().map(|r| r.end()).max().unwrap();
    let total_frames = FrameAllocator::frames_in(top - base);

    // --- the bootstrap problem ---
    //
    // The allocator needs somewhere to put its bitmap. We have no allocator.
    //
    // The way out is to carve it, by hand, out of the very memory it is about to
    // manage. We know where the kernel image ends (the linker told us), so we put the
    // bitmap immediately after it, and then reserve those frames from itself.
    //
    // The allocator's first act is to allocate itself.
    let bitmap_bytes = FrameAllocator::bitmap_bytes(total_frames);
    let bitmap_start = align_up(image_end(), FRAME_SIZE);

    assert!(
        bitmap_start + bitmap_bytes as u64 <= top,
        "no room for a {bitmap_bytes}-byte bitmap after the kernel image"
    );

    // SAFETY: this memory is inside RAM, past the end of our image, and nothing else in
    // the kernel touches it. We are about to mark it used so that nothing ever will.
    // The reference is 'static because the allocator outlives everything.
    let bitmap: &'static mut [u8] =
        unsafe { core::slice::from_raw_parts_mut(bitmap_start as *mut u8, bitmap_bytes) };

    // Everything starts USED. Memory is guilty until proven innocent: a frame is only
    // handed out once someone has said "this is real RAM." Default-free would cheerfully
    // allocate the MMIO hole and hand out the UART's registers as scratch space.
    let mut allocator = FrameAllocator::new(base, total_frames, bitmap);

    // Now prove innocence, region by region.
    for r in ram {
        allocator.mark_free(r.start, r.size);
    }

    // And immediately take back everything that is already spoken for. Order matters:
    // free first, then reserve, because reserving is what has to win.
    allocator.mark_used(image_start(), image_end() - image_start());
    allocator.mark_used(bitmap_start, bitmap_bytes as u64);
    allocator.mark_used(dtb_ptr as u64, dtb.total_size() as u64);
    for r in reserved {
        allocator.mark_used(r.start, r.size);
    }

    *ALLOCATOR.lock() = Some(allocator);
}

pub fn alloc() -> Option<Frame> {
    ALLOCATOR.lock().as_mut()?.alloc()
}

/// Physically contiguous frames, for hardware that does DMA and has no MMU to hide a
/// scattered buffer behind. Milestone 8 needs this.
pub fn alloc_contiguous(count: usize) -> Option<Frame> {
    ALLOCATOR.lock().as_mut()?.alloc_contiguous(count)
}

pub fn free(frame: Frame) {
    ALLOCATOR
        .lock()
        .as_mut()
        .expect("freeing a frame before memory::init")
        .free(frame);
}

pub fn stats() -> Option<Stats> {
    Some(ALLOCATOR.lock().as_ref()?.stats())
}

/// Is this address inside the kernel image?
pub fn is_in_kernel_image(addr: u64) -> bool {
    (image_start()..image_end()).contains(&addr)
}

/// Where the kernel image begins and ends, per the linker.
pub fn image_bounds() -> (u64, u64) {
    (image_start(), image_end())
}

/// Is this frame currently marked used?
pub fn is_frame_used(frame: Frame) -> Option<bool> {
    ALLOCATOR.lock().as_ref()?.is_used(frame)
}

pub fn print_summary() {
    let Some(s) = stats() else {
        println!("  memory          : uninitialized");
        return;
    };

    let mib = |frames: usize| (frames as u64 * FRAME_SIZE) / (1024 * 1024);
    let kib = |frames: usize| (frames as u64 * FRAME_SIZE) / 1024;

    println!(
        "  memory          : {} MiB total, {} MiB free ({} KiB in use)",
        mib(s.total),
        mib(s.free()),
        kib(s.used),
    );
}

/// `__image_start` and `__image_end` are invented by the linker script, which is the
/// only thing that knows where we ended up. See notes/linker-scripts.md.
fn image_start() -> u64 {
    unsafe extern "C" {
        static __image_start: core::ffi::c_void;
    }
    (&raw const __image_start) as u64
}

fn image_end() -> u64 {
    unsafe extern "C" {
        static __image_end: core::ffi::c_void;
    }
    (&raw const __image_end) as u64
}

fn align_up(value: u64, to: u64) -> u64 {
    value.div_ceil(to) * to
}
