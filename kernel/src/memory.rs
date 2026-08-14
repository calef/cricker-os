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

use dtb::{Dtb, Region};
use frames::{FRAME_SIZE, Frame, FrameAllocator, Stats};

use crate::arch::mmu::{phys_to_virt, virt_to_phys};
use crate::println;
use crate::sync::{IrqSafeMutex, rank};

/// The frame allocator.
///
/// `IrqSafeMutex`, not a bare spinlock: an interrupt handler that tried to allocate while
/// the interrupted code held this lock would spin forever waiting for code that cannot
/// run. See sync.rs and DECISIONS.md §9.
///
/// The discipline that goes with it: **interrupt handlers do not allocate.** They record
/// what happened and defer the work. The lock being interrupt-safe is the belt; that rule
/// is the braces.
static ALLOCATOR: IrqSafeMutex<Option<FrameAllocator<'static>>> =
    IrqSafeMutex::new(rank::FRAMES, None);

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
    // `dtb_ptr` is PHYSICAL: boot.s passed it straight through from x0, and QEMU speaks in
    // physical addresses. We are running virtual now, so name it through the direct map.
    let dtb = unsafe { Dtb::from_ptr(phys_to_virt(dtb_ptr as u64) as *const u8) }
        .expect("device tree is unreadable");

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

    // The interrupt controller. Two register blocks, and the order is part of the binding:
    // distributor first, then the per-core CPU interface. Milestone 5 wants both.
    {
        let mut gic = [Region { start: 0, size: 0 }; 4];
        let n = dtb
            .node_reg(b"intc@", &mut gic)
            .expect("cannot read the GIC's reg");
        if n >= 2 {
            *GIC_REGIONS.lock() = (
                Some((gic[0].start, gic[0].size)),
                Some((gic[1].start, gic[1].size)),
            );
        }
    }

    // The RISC-V interrupt controller (milestone 20): the PLIC. Found by its binding, because the
    // node's label is the board author's: QEMU spells it `plic@c000000` and the JH7110
    // `interrupt-controller@c000000`, so the old `plic@` name-prefix read found nothing on the
    // board this has to boot next (notes/visionfive2.md). The name prefix stays as a fallback for
    // a tree that names the node conventionally but states a compatible we do not know. aarch64
    // has neither, so there `plic_region` stays None; the GIC above is its equivalent. One
    // register block, unlike the GIC's distributor/CPU-interface pair.
    {
        let mut plic = [Region { start: 0, size: 0 }; 1];
        let found = match dtb.node_reg_compatible(b"sifive,plic-1.0.0", &mut plic) {
            Ok(n) if n >= 1 => true,
            _ => matches!(dtb.node_reg(b"plic@", &mut plic), Ok(n) if n >= 1),
        };
        if found {
            *PLIC_REGION.lock() = Some((plic[0].start, plic[0].size));
        }
    }

    // The SMMUv3 (milestone 16b), present only when the machine was started with
    // `iommu=smmuv3`. Absent, the kernel runs exactly as before; present, iommu::init drives it.
    {
        let mut smmu = [Region { start: 0, size: 0 }; 1];
        if let Ok(n) = dtb.node_reg(b"smmuv3@", &mut smmu)
            && n >= 1
        {
            *SMMU_REGION.lock() = Some((smmu[0].start, smmu[0].size));
        }
    }

    // The real-time clock (milestone 51), matched on `compatible` rather than on a node name.
    // This is the device where the name shortcut runs out: aarch64 `virt` calls it
    // `pl031@9010000` and riscv64 `virt` calls it `rtc@101000`, so no prefix finds both, while
    // `arm,pl031` and `google,goldfish-rtc` name exactly the register layouts a driver knows.
    // Both boots probe for both, and a machine with neither leaves this None, which the clock
    // service reports as "I do not know what time it is" (DECISIONS §42, §43).
    {
        let mut rtc = [Region { start: 0, size: 0 }; 1];
        for (compat, kind) in [
            (&b"arm,pl031"[..], clock_proto::rtc::PL031),
            (&b"google,goldfish-rtc"[..], clock_proto::rtc::GOLDFISH),
        ] {
            if let Ok(n) = dtb.node_reg_compatible(compat, &mut rtc)
                && n >= 1
            {
                *RTC_REGION.lock() = Some((rtc[0].start, rtc[0].size, kind));
                break;
            }
        }
    }

    // The whole span we have to be able to describe. Note this is the *span*, not the
    // *sum*: if a board has RAM at 0x4000_0000 and again at 0x8_0000_0000, we track
    // every frame between them and simply never free the hole. A bit of wasted bitmap
    // buys a much simpler index calculation.
    {
        let mut map = RAM.lock();
        for (i, r) in ram.iter().enumerate() {
            map.regions[i] = (r.start, r.size);
        }
        map.count = ram.len();
    }

    let base = ram.iter().map(|r| r.start).min().unwrap();
    let top = ram.iter().map(|r| r.end()).max().unwrap();
    let total_frames = FrameAllocator::frames_in(top - base);

    // Everything that is already spoken for, and must not be allocated or scribbled on.
    //
    // The initrd matters and is easy to miss: the bootloader loaded a file into RAM for
    // us and told us where. Nobody else will protect it. Milestone 8 and 10 want to read
    // it, and if the allocator hands that memory out first, the bug lands a long way from
    // its cause.
    // Image + DTB + initrd (3), plus the legacy reservation block and the /reserved-memory node
    // (up to MAX_REGIONS each).
    let mut forbidden = [Region { start: 0, size: 0 }; 2 * MAX_REGIONS + 3];
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
    // The /reserved-memory node's children, distinct from the legacy reservation block above. This
    // is where RISC-V firmware (OpenSBI) reserves its own region below the kernel; without it the
    // allocator would hand out OpenSBI's PMP-protected memory and the first write would fault.
    let mut resv_mem = [Region { start: 0, size: 0 }; MAX_REGIONS];
    let resv_mem_count = dtb
        .reserved_memory_regions(&mut resv_mem)
        .expect("cannot read /reserved-memory");
    for r in &resv_mem[..resv_mem_count] {
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
    // `bitmap_start` is a physical address (it names frames). To *write* to it we need the
    // virtual name for the same bytes.
    let bitmap: &'static mut [u8] = unsafe {
        core::slice::from_raw_parts_mut(phys_to_virt(bitmap_start) as *mut u8, bitmap_bytes)
    };

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

/// Free frames right now. For tests that prove reclamation actually returns memory (the untyped
/// flat-frame-count property, notes/untyped.md): a region reclaimed by object revocation should
/// bring this exactly back to where it stood before the region was created.
#[cfg_attr(not(test), allow(dead_code))] // the reclamation tests in sched.rs and user/tests.rs
pub fn free_frames() -> usize {
    ALLOCATOR.lock().as_ref().map_or(0, |a| a.stats().free())
}

/// Is this address inside the kernel image?
#[cfg_attr(not(test), allow(dead_code))] // this file's bootstrap tests are the callers
pub fn is_in_kernel_image(addr: u64) -> bool {
    (image_start()..image_end()).contains(&addr)
}

/// Where the kernel image begins and ends, per the linker.
#[cfg_attr(not(test), allow(dead_code))] // this file's bootstrap tests are the callers
pub fn image_bounds() -> (u64, u64) {
    (image_start(), image_end())
}

/// Is this frame currently marked used?
#[cfg_attr(not(test), allow(dead_code))] // this file's bootstrap tests are the callers
pub fn is_frame_used(frame: Frame) -> Option<bool> {
    ALLOCATOR.lock().as_ref()?.is_used(frame)
}

/// Where the interrupt controller is, as the device tree describes it.
///
/// (distributor, `cpu_interface`), both **physical**. Stashed at `init` because that is the only
/// moment we have the device tree parsed, and milestone 5 needs it much later.
#[cfg_attr(target_arch = "riscv64", allow(dead_code))] // riscv has a PLIC, not a GIC
pub fn gic_regions() -> Option<((u64, u64), (u64, u64))> {
    let g = GIC_REGIONS.lock();
    g.0.map(|d| {
        (
            d,
            g.1.expect("a GIC with a distributor but no CPU interface"),
        )
    })
}

/// The PLIC's register block (start, size), both **physical**, from the device tree. `None` on
/// aarch64 or before `init`. RISC-V's `mmu::init` maps it device-typed, like the GIC.
#[cfg_attr(target_arch = "aarch64", allow(dead_code))] // no PLIC on aarch64; the name is portable
pub fn plic_region() -> Option<(u64, u64)> {
    *PLIC_REGION.lock()
}

/// The SMMUv3's register block (start, size), both **physical**, from the device tree. `None`
/// when the machine has no SMMU (riscv, or aarch64 without `iommu=smmuv3`). Presence here is what
/// gates the whole aarch64 IOMMU path: no node, no register reads, no faults on a machine that
/// never had the device.
pub fn smmu_region() -> Option<(u64, u64)> {
    *SMMU_REGION.lock()
}

/// The real-time clock's register block and which binding it is: `(start, size, kind)`, the
/// address **physical** and the kind one of `clock_proto::rtc`. `None` on a machine whose device
/// tree describes no RTC we can drive, which is a state the clock service has an answer for rather
/// than a case it papers over.
///
/// The kind travels with the address on purpose. The clock service picks its register layout from
/// what the machine said, not from `target_arch`, because the VisionFive 2 is riscv64 and has
/// neither of these devices; an ISA-keyed driver would compile clean and read garbage on the first
/// real board (DECISIONS §43).
#[cfg_attr(
    not(any(test, feature = "shell", feature = "initboot")),
    allow(dead_code)
)]
pub fn rtc_region() -> Option<(u64, u64, u64)> {
    *RTC_REGION.lock()
}

/// The RAM regions the device tree told us about.
///
/// The MMU needs these: with paging on, a physical address the kernel cannot *name* is a
/// physical address it cannot use, and it must be able to touch any frame the allocator
/// hands it (to zero a new page table, to fill a new user page).
pub fn ram_regions() -> impl Iterator<Item = (u64, u64)> {
    // Copy the whole map out under ONE lock acquisition, then iterate freely. 256 bytes.
    //
    // The alternative (an iterator that holds the lock, or takes it per element) would keep a
    // kernel lock live across arbitrary caller code, with interrupts masked the whole time.
    // That violates "keep critical sections short" (DECISIONS.md §9) for no benefit at all.
    let map = *RAM.lock();
    (0..map.count).map(move |i| map.regions[i])
}

/// Where the frame bitmap landed, and how big it is. Test support.
#[cfg_attr(not(test), allow(dead_code))] // this file's bootstrap tests are the callers
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

/// The RAM map, kept so the MMU can map it.
///
/// **One lock, not sixteen.** This started life as `[IrqSafeMutex<(u64, u64)>; 16]`, which was
/// sixteen locks for one piece of data and took one of them *per element* while iterating.
/// That got the concurrency story exactly backwards: this is not shared mutable state, it is a
/// **constant that happens to be computed at boot**, written once while single-threaded and
/// read forever after.
///
/// Fixed-size rather than a `Vec` because `memory::init` runs *before* the heap exists. It is
/// the last place in the kernel with that excuse.
#[derive(Clone, Copy)]
struct RamMap {
    regions: [(u64, u64); MAX_REGIONS],
    count: usize,
}

static RAM: IrqSafeMutex<RamMap> = IrqSafeMutex::new(
    rank::RAM,
    RamMap {
        regions: [(0, 0); MAX_REGIONS],
        count: 0,
    },
);

/// (distributor, cpu interface), each (base, size). Physical.
type GicRegions = (Option<(u64, u64)>, Option<(u64, u64)>);
static GIC_REGIONS: IrqSafeMutex<GicRegions> = IrqSafeMutex::new(rank::RAM, (None, None));

/// The PLIC's single register block, from the device tree (milestone 20). `None` on aarch64 (no such
/// node) and until `init` has run.
static PLIC_REGION: IrqSafeMutex<Option<(u64, u64)>> = IrqSafeMutex::new(rank::RAM, None);

/// The SMMUv3's register block, from the device tree (milestone 16b). `None` unless the machine was
/// started with `-machine virt,iommu=smmuv3` (and always on riscv, whose IOMMU is a PCI function
/// found by enumeration, not a platform device with a node).
static SMMU_REGION: IrqSafeMutex<Option<(u64, u64)>> = IrqSafeMutex::new(rank::RAM, None);

/// The RTC's register block and binding kind (milestone 51). `None` until `init` has run, and on a
/// machine whose device tree describes neither RTC we can drive.
static RTC_REGION: IrqSafeMutex<Option<(u64, u64, u64)>> = IrqSafeMutex::new(rank::RAM, None);

static BITMAP_START: AtomicUsize = AtomicUsize::new(0);
static BITMAP_BYTES: AtomicUsize = AtomicUsize::new(0);
static INITRD_START: AtomicUsize = AtomicUsize::new(0);
static INITRD_SIZE: AtomicUsize = AtomicUsize::new(0);

/// The boot banner's memory line. Only the banner in `main.rs` calls it, and the test build and the
/// `bench` boot mode both compile the banner out, so it has no caller in exactly those two.
#[cfg_attr(any(test, feature = "bench"), allow(dead_code))]
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

/// `__image_start` and `__image_end` are invented by the linker script, which is the only
/// thing that knows where we ended up. See notes/linker-scripts.md.
///
/// **They are VIRTUAL addresses**, because the kernel is linked high. Everything in this
/// module deals in *physical* frames, so we convert on the way in. Getting this backwards
/// would reserve frames that don't exist and hand out the ones that hold our code.
fn image_start() -> u64 {
    unsafe extern "C" {
        static __image_start: core::ffi::c_void;
    }
    virt_to_phys((&raw const __image_start) as u64)
}

fn image_end() -> u64 {
    unsafe extern "C" {
        static __image_end: core::ffi::c_void;
    }
    virt_to_phys((&raw const __image_end) as u64)
}

fn align_up(value: u64, to: u64) -> u64 {
    value.div_ceil(to) * to
}

#[cfg(test)]
mod tests {
    //! Tests for the physical memory map and the frame allocator.
    //!
    //! The allocator's *logic* is tested exhaustively on the host (`cargo test -p frames`, 14
    //! tests, no emulator). What only the real machine can tell us is whether we pointed it at the
    //! right memory, and whether the frames it hands out are actually reachable. That is all these
    //! check.

    /// Proves we read a plausible memory map out of the device tree.
    ///
    /// The allocator logic is tested exhaustively on the host (`cargo test -p frames`,
    /// 14 tests, no emulator). What *only* the real machine can tell us is whether we
    /// pointed it at the right memory, so that's all this checks.
    #[test_case]
    fn memory_map_came_from_the_device_tree() {
        use frames::FRAME_SIZE;

        let s = crate::memory::stats().expect("allocator not initialized");

        // QEMU virt gives us 128 MiB by default. If this ever reads zero, or something
        // absurd, we have misparsed `reg` (which is big-endian, and whose cell width is
        // declared by the *parent* node, both of which are easy to get wrong).
        let total_bytes = s.total as u64 * FRAME_SIZE;
        assert_eq!(total_bytes, 128 * 1024 * 1024, "unexpected RAM size");

        // Some memory must already be spoken for: at minimum the kernel image, the
        // bitmap, and the device tree. A zero here means we reserved nothing, which
        // means we are about to hand out our own code.
        assert!(s.used > 0, "nothing is reserved?");
        assert!(s.free() > 0, "no free memory at all?");
    }

    /// **The one that matters.** Every frame the kernel image touches must be reserved.
    ///
    /// This states the invariant `mark_used` exists to maintain, directly. Our image ends
    /// at 0x40097010, which is not frame-aligned, so the last frame is only *partly*
    /// ours. Round that end down instead of up and the frame stays free, the allocator
    /// hands it out, something writes to it, and the tail of the kernel is quietly
    /// overwritten. The crash lands somewhere else entirely, much later, in code that did
    /// nothing wrong.
    ///
    /// Checking the bitmap directly is both stronger and cheaper than draining the
    /// allocator: it covers *every* frame of the image, and it allocates nothing.
    #[test_case]
    fn every_frame_of_the_kernel_image_is_reserved() {
        use frames::{FRAME_SIZE, Frame};

        let (start, end) = crate::memory::image_bounds();
        let mut addr = start - start % FRAME_SIZE; // round DOWN to the containing frame

        while addr < end {
            assert_eq!(
                crate::memory::is_frame_used(Frame::from_addr(addr)),
                Some(true),
                "frame {addr:#x} overlaps the kernel image but is marked FREE"
            );
            addr += FRAME_SIZE;
        }
    }

    /// And prove `alloc` actually respects that bitmap.
    ///
    /// Keep this array SMALL. It was `[Option<Frame>; 1024]` (16 KiB) on a 64 KiB stack,
    /// and it silently overflowed into .bss, .data, and .text, and hung the machine while
    /// printing something unrelated. See notes/stack.md. The canary catches that now, but
    /// the right move is to not do it.
    #[test_case]
    fn allocator_never_hands_out_the_kernel() {
        let mut taken = [None; 64];

        for slot in taken.iter_mut() {
            let Some(frame) = crate::memory::alloc() else {
                break;
            };
            assert!(
                !crate::memory::is_in_kernel_image(frame.addr()),
                "allocator handed out {:#x}, which is inside the kernel image",
                frame.addr()
            );
            *slot = Some(frame);
        }

        // `iter_mut().take()` and not `into_iter()`, which is the same lesson as the array size
        // above one step further on. `into_iter` on an array consumes it BY VALUE, and a debug
        // build gives that move its own stack temporary: `script/stack-frame-check` measured
        // `<[Option<Frame>; 64] as IntoIterator>::into_iter` at **4224 bytes**, over the 4096-byte
        // guard page, on both ISAs. Taking each slot in place copies one `Option<Frame>` instead.
        for slot in taken.iter_mut() {
            if let Some(frame) = slot.take() {
                crate::memory::free(frame);
            }
        }
    }

    /// Proves a frame we were given is real, writable memory that nothing else owns.
    ///
    /// Host tests prove the *bookkeeping* is right. Only the machine can prove the
    /// bookkeeping corresponds to actual RAM. Writing a pattern and reading it back is
    /// the cheapest way to find out we've been handing out an MMIO hole.
    #[test_case]
    fn an_allocated_frame_is_real_memory() {
        use frames::FRAME_SIZE;

        let frame = crate::memory::alloc().expect("out of memory");
        // The allocator speaks physical; we must name it virtually to touch it.
        let ptr = crate::arch::mmu::phys_to_virt(frame.addr()) as *mut u64;
        let words = (FRAME_SIZE / 8) as usize;

        // SAFETY: the allocator just gave us this frame, so we own it exclusively. The
        // MMU is off, so the physical address is directly usable.
        unsafe {
            for i in 0..words {
                core::ptr::write_volatile(ptr.add(i), 0xcafe_f00d_0000_0000 | i as u64);
            }
            for i in 0..words {
                assert_eq!(
                    core::ptr::read_volatile(ptr.add(i)),
                    0xcafe_f00d_0000_0000 | i as u64,
                    "frame {:#x} word {i} did not hold what we wrote",
                    frame.addr()
                );
            }
        }

        crate::memory::free(frame);
    }

    /// The bitmap must not sit on top of anything already spoken for.
    ///
    /// We used to place it immediately after the kernel image and hope. That worked, but
    /// only because QEMU happens to put the device tree 64 MiB higher up. `image_size` in
    /// the arm64 Image header stops at `__stack_top`, so everything past `__image_end` is
    /// memory we never told the bootloader we wanted, and different firmware need not
    /// leave it alone. Now the placement is scanned and proven; this checks it.
    #[test_case]
    fn bitmap_overlaps_nothing() {
        let (bstart, bsize) = crate::memory::bitmap_region();
        assert!(bsize > 0, "bitmap has no size?");

        let (istart, iend) = crate::memory::image_bounds();
        assert!(
            bstart + bsize <= istart || bstart >= iend,
            "bitmap {bstart:#x}+{bsize:#x} overlaps the kernel image {istart:#x}..{iend:#x}"
        );

        let dtb = crate::DTB.load(core::sync::atomic::Ordering::Relaxed) as u64;
        assert!(
            bstart + bsize <= dtb || bstart >= dtb + 64 * 1024,
            "bitmap {bstart:#x}+{bsize:#x} is sitting on the device tree at {dtb:#x}"
        );

        if let Some((istart, isize)) = crate::memory::initrd_region() {
            assert!(
                bstart + bsize <= istart || bstart >= istart + isize,
                "bitmap {bstart:#x}+{bsize:#x} is sitting on the initrd"
            );
        }
    }

    /// If the bootloader gave us an initrd, the allocator must never hand it out.
    ///
    /// Only meaningful when QEMU is run with `-initrd`, which the default test run isn't.
    /// It asserts the invariant when there IS one, and passes trivially when there isn't,
    /// which is the right shape: the check exists so that the day someone adds `-initrd`
    /// to the runner, this catches it rather than milestone 10 catching it.
    #[test_case]
    fn initrd_is_reserved_if_present() {
        use frames::{FRAME_SIZE, Frame};

        let Some((start, size)) = crate::memory::initrd_region() else {
            return;
        };

        let mut addr = start - start % FRAME_SIZE;
        while addr < start + size {
            assert_eq!(
                crate::memory::is_frame_used(Frame::from_addr(addr)),
                Some(true),
                "frame {addr:#x} is part of the initrd but is marked FREE"
            );
            addr += FRAME_SIZE;
        }
    }

    /// Proves alloc and free actually balance, on the real memory map.
    #[test_case]
    fn alloc_and_free_balance() {
        let before = crate::memory::stats().unwrap();

        let a = crate::memory::alloc().unwrap();
        let b = crate::memory::alloc_contiguous(8).unwrap();

        assert_eq!(crate::memory::stats().unwrap().used, before.used + 9);

        crate::memory::free(a);
        for i in 0..8u64 {
            crate::memory::free(frames::Frame::from_addr(b.addr() + i * frames::FRAME_SIZE));
        }

        assert_eq!(
            crate::memory::stats().unwrap(),
            before,
            "frames leaked or were double-counted"
        );
    }
}
