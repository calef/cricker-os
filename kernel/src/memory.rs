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
use crate::sync::IrqSafeMutex;
use dtb::{Dtb, Region};
use frames::{FRAME_SIZE, Frame, FrameAllocator, Stats};

/// The frame allocator.
///
/// `IrqSafeMutex`, not a bare spinlock: an interrupt handler that tried to allocate while
/// the interrupted code held this lock would spin forever waiting for code that cannot
/// run. See sync.rs and DECISIONS.md §9.
///
/// The discipline that goes with it: **interrupt handlers do not allocate.** They record
/// what happened and defer the work. The lock being interrupt-safe is the belt; that rule
/// is the braces.
static ALLOCATOR: IrqSafeMutex<Option<FrameAllocator<'static>>> = IrqSafeMutex::new(None);

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

    // Everything that is already spoken for, and must not be allocated or scribbled on.
    //
    // The initrd matters and is easy to miss: the bootloader loaded a file into RAM for
    // us and told us where. Nobody else will protect it. Milestone 8 and 10 want to read
    // it, and if the allocator hands that memory out first, the bug lands a long way from
    // its cause.
    let mut forbidden = [Region { start: 0, size: 0 }; MAX_REGIONS + 3];
    let mut n = 0;

    let mut claim = |r: Region| {
        if r.size > 0 {
            forbidden[n] = r;
            n += 1;
        }
    };

    claim(Region {
        start: image_start(),
        size: image_end() - image_start(),
    });
    claim(Region {
        start: dtb_ptr as u64,
        size: dtb.total_size() as u64,
    });
    if let Some(initrd) = dtb.initrd().expect("cannot read /chosen") {
        INITRD_START.store(initrd.start as usize, core::sync::atomic::Ordering::Relaxed);
        INITRD_SIZE.store(initrd.size as usize, core::sync::atomic::Ordering::Relaxed);
        claim(initrd);
    }
    for r in reserved {
        claim(*r);
    }
    let forbidden = &forbidden[..n];

    // --- the bootstrap problem ---
    //
    // The allocator needs somewhere to put its bitmap. We have no allocator.
    //
    // The way out is to carve it, by hand, out of the very memory it is about to manage,
    // and then reserve those frames from itself. **The allocator's first act is to
    // allocate itself.**
    //
    // We used to just drop it immediately after the kernel image and hope. That worked,
    // but only by luck: `image_size` in the arm64 Image header stops at `__stack_top`,
    // so everything past `__image_end` is memory we never told the bootloader we wanted.
    // QEMU happens to place the DTB and the initrd 64 MiB higher up. Different firmware
    // need not.
    //
    // So instead: scan RAM for the first frame-aligned run that clears everything above.
    // Same answer in practice, but now it's proven rather than lucky, and it will keep
    // being right on hardware we haven't met.
    let bitmap_bytes = FrameAllocator::bitmap_bytes(total_frames);
    let bitmap_start = place_bitmap(bitmap_bytes as u64, ram, forbidden);
    BITMAP_START.store(bitmap_start as usize, core::sync::atomic::Ordering::Relaxed);
    BITMAP_BYTES.store(bitmap_bytes, core::sync::atomic::Ordering::Relaxed);

    // SAFETY: `place_bitmap` guarantees this range is inside a RAM region and overlaps
    // nothing that is already spoken for. Nothing else in the kernel touches it, and we
    // mark it used below so nothing ever will. 'static because the allocator outlives
    // everything.
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
    for r in forbidden {
        allocator.mark_used(r.start, r.size);
    }
    allocator.mark_used(bitmap_start, bitmap_bytes as u64);

    *ALLOCATOR.lock() = Some(allocator);
}

/// Find somewhere to put the frame bitmap that overlaps nothing already spoken for.
///
/// Scans RAM in order and returns the first frame-aligned run of `need` bytes that clears
/// every forbidden region. When a candidate collides, jump past the *end* of whatever it
/// hit rather than nudging forward by a frame: the regions are large and stepping through
/// a 200 KiB initrd one page at a time would be silly.
fn place_bitmap(need: u64, ram: &[Region], forbidden: &[Region]) -> u64 {
    for region in ram {
        let mut candidate = align_up(region.start, FRAME_SIZE);

        'scan: while candidate + need <= region.end() {
            for f in forbidden {
                if overlaps(candidate, need, f.start, f.size) {
                    candidate = align_up(f.start + f.size, FRAME_SIZE);
                    continue 'scan;
                }
            }
            return candidate;
        }
    }

    panic!("no room anywhere in RAM for a {need}-byte frame bitmap");
}

/// Do `[a, a+alen)` and `[b, b+blen)` share a byte?
fn overlaps(a: u64, alen: u64, b: u64, blen: u64) -> bool {
    a < b.saturating_add(blen) && b < a.saturating_add(alen)
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

/// Where the frame bitmap landed, and how big it is. Test support.
pub fn bitmap_region() -> (u64, u64) {
    (
        BITMAP_START.load(core::sync::atomic::Ordering::Relaxed) as u64,
        BITMAP_BYTES.load(core::sync::atomic::Ordering::Relaxed) as u64,
    )
}

/// The initrd, if the bootloader gave us one. Test support.
pub fn initrd_region() -> Option<(u64, u64)> {
    let start = INITRD_START.load(core::sync::atomic::Ordering::Relaxed) as u64;
    let size = INITRD_SIZE.load(core::sync::atomic::Ordering::Relaxed) as u64;
    (size > 0).then_some((start, size))
}

use core::sync::atomic::AtomicUsize;
static BITMAP_START: AtomicUsize = AtomicUsize::new(0);
static BITMAP_BYTES: AtomicUsize = AtomicUsize::new(0);
static INITRD_START: AtomicUsize = AtomicUsize::new(0);
static INITRD_SIZE: AtomicUsize = AtomicUsize::new(0);

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
