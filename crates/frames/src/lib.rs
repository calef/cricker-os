//! A physical frame allocator.
//!
//! Hands out 4 KiB pages of physical memory. This is the bottom of the memory
//! hierarchy: everything above it (page tables at milestone 4, the kernel heap, DMA
//! buffers at milestone 8, user process pages at milestone 7) ultimately asks this for
//! its memory, and there is nothing underneath it to ask.
//!
//! # Why a bitmap, and not a free list
//!
//! The classic hobby-OS answer is a free list: link each free page to the next by
//! writing a pointer *into the free page itself*. Zero metadata overhead, O(1)
//! allocation, and genuinely elegant (it is what xv6 does).
//!
//! We use a bitmap instead, for two reasons:
//!
//! 1. **Contiguity.** A free list cannot answer "give me 8 physically contiguous
//!    frames." Milestone 8's virtio DMA buffers need exactly that, and retrofitting it
//!    means throwing the free list away.
//!
//! 2. **Testability.** A free list stores its metadata inside the memory it manages, so
//!    testing it requires handing it real memory and doing unsafe pointer writes. A
//!    bitmap's logic is *pure*: given a bitmap and a request, which frame? We can test
//!    it exhaustively on the host with no memory at all, which is DECISIONS.md §7.
//!
//! The cost is 1 bit per frame: 32 KiB of bitmap per GiB of RAM. Cheap.
//!
//! # Why this is a separate crate
//!
//! Pure logic. Bytes in, frame numbers out. No hardware, no `unsafe`. Its tests run on
//! the host in milliseconds instead of booting an emulator.

#![cfg_attr(not(test), no_std)]

/// 4 KiB. The unit the MMU translates in ([notes/mmu.md]), so it is the unit we
/// allocate in.
pub const FRAME_SIZE: u64 = 4096;

/// A physical address, guaranteed frame-aligned.
///
/// A newtype rather than a bare `u64`, because a physical address and a virtual address
/// are about to become genuinely different things (milestone 4), and the compiler should
/// stop us confusing them. Right now, with the MMU off, they happen to be identical.
/// That coincidence ends soon and this type is what will keep it from hurting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Frame(u64);

impl Frame {
    /// Name the frame at `addr`.
    ///
    /// Naming a frame is not the same as owning it. This type carries one invariant
    /// (the address is frame-aligned) and no claim about who the frame belongs to.
    /// Ownership lives entirely in the allocator's bitmap, which is where it can
    /// actually be checked.
    ///
    /// # Panics
    /// If `addr` is not frame-aligned.
    pub const fn from_addr(addr: u64) -> Self {
        assert!(
            addr.is_multiple_of(FRAME_SIZE),
            "address is not frame-aligned"
        );
        Frame(addr)
    }

    /// The frame containing `addr`, rounding down.
    pub const fn containing(addr: u64) -> Self {
        Frame(addr - addr % FRAME_SIZE)
    }

    pub const fn addr(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub total: usize,
    pub used: usize,
}

impl Stats {
    pub fn free(&self) -> usize {
        self.total - self.used
    }
}

/// One bit per frame. `1` means used.
///
/// **Everything starts used.** Memory is guilty until proven innocent: a frame is only
/// handed out after someone has explicitly said "this address range is real RAM." A
/// bitmap that defaulted to free would cheerfully allocate the MMIO hole at
/// `0x0900_0000` and hand you the UART's registers as scratch space.
pub struct FrameAllocator<'a> {
    bitmap: &'a mut [u8],
    /// Physical address of frame 0.
    base: u64,
    /// How many frames we track. May be less than `bitmap.len() * 8`.
    total: usize,
    used: usize,
    /// Where the next linear scan starts. Purely an optimization; correctness does not
    /// depend on it.
    hint: usize,
}

impl<'a> FrameAllocator<'a> {
    /// Bytes of bitmap needed to track `frames` frames.
    pub const fn bitmap_bytes(frames: usize) -> usize {
        frames.div_ceil(8)
    }

    /// How many frames span `[base, base + span)`.
    pub const fn frames_in(span: u64) -> usize {
        (span / FRAME_SIZE) as usize
    }

    /// Create an allocator covering `total` frames starting at `base`, with **every
    /// frame marked used**.
    ///
    /// Call [`mark_free`](Self::mark_free) for each region of real RAM, then
    /// [`mark_used`](Self::mark_used) for everything inside it that is already spoken
    /// for: the kernel image, the device tree, and the bitmap itself.
    ///
    /// # Panics
    /// If `bitmap` is too small, or `base` is not frame-aligned.
    pub fn new(base: u64, total: usize, bitmap: &'a mut [u8]) -> Self {
        assert!(
            base.is_multiple_of(FRAME_SIZE),
            "base must be frame-aligned"
        );
        assert!(
            bitmap.len() >= Self::bitmap_bytes(total),
            "bitmap too small: {} bytes for {} frames",
            bitmap.len(),
            total
        );

        bitmap.fill(0xff);

        Self {
            bitmap,
            base,
            total,
            used: total,
            hint: 0,
        }
    }

    pub fn stats(&self) -> Stats {
        Stats {
            total: self.total,
            used: self.used,
        }
    }

    /// Mark every frame **overlapping** `[start, start+size)` as free.
    ///
    /// Note "overlapping" and not "contained in". This rounds the range *outward*,
    /// which is correct here (usable RAM regions from the device tree are page-aligned
    /// in practice) but is the **opposite** of what [`mark_used`](Self::mark_used) must
    /// do. See the comment there; the asymmetry is the whole ballgame.
    pub fn mark_free(&mut self, start: u64, size: u64) {
        for i in self.frame_range(start, size) {
            if self.get(i) {
                self.set(i, false);
                self.used -= 1;
            }
        }
        self.hint = 0;
    }

    /// Mark every frame **touched by** `[start, start+size)` as used.
    ///
    /// This is the function where an off-by-one hands out a frame containing kernel
    /// code, and the kernel then overwrites itself. Consider: our image ends at
    /// `0x4009_7010`, which is not frame-aligned. The frame at `0x4009_7000` holds our
    /// last 0x10 bytes *and* 4080 bytes of nothing.
    ///
    /// If we round that end *down*, we declare the frame free and hand it out, and
    /// something writes over the tail of our own kernel.
    ///
    /// So: round the start **down** and the end **up**. Claim every frame the region so
    /// much as touches. Over-reserving wastes at most 4 KiB per region. Under-reserving
    /// corrupts the kernel.
    pub fn mark_used(&mut self, start: u64, size: u64) {
        for i in self.frame_range(start, size) {
            if !self.get(i) {
                self.set(i, true);
                self.used += 1;
            }
        }
    }

    /// Allocate one frame.
    pub fn alloc(&mut self) -> Option<Frame> {
        let i = (self.hint..self.total)
            .chain(0..self.hint)
            .find(|&i| !self.get(i))?;

        self.set(i, true);
        self.used += 1;
        self.hint = i + 1;
        Some(Frame(self.base + i as u64 * FRAME_SIZE))
    }

    /// Allocate `count` **physically contiguous** frames, and return the first.
    ///
    /// This is the reason we are a bitmap and not a free list. A device doing DMA reads
    /// physical addresses directly, with no MMU in the way to paper over a scattered
    /// buffer, so a virtio ring at milestone 8 needs its frames genuinely adjacent.
    ///
    /// The scan is O(total) and dumb. It is called rarely and never in a hot path.
    pub fn alloc_contiguous(&mut self, count: usize) -> Option<Frame> {
        if count == 0 || count > self.total {
            return None;
        }

        let mut run_start = 0;
        let mut run = 0;

        for i in 0..self.total {
            if self.get(i) {
                run = 0;
                run_start = i + 1;
                continue;
            }

            run += 1;
            if run == count {
                for j in run_start..run_start + count {
                    self.set(j, true);
                }
                self.used += count;
                return Some(Frame(self.base + run_start as u64 * FRAME_SIZE));
            }
        }

        None
    }

    /// Return a frame to the pool.
    ///
    /// # Panics
    /// On a double free, and on a frame we never owned. Both are kernel bugs, and a
    /// kernel that keeps running after one is a kernel that corrupts memory somewhere
    /// far away and blames innocent code. Fail loudly and immediately.
    pub fn free(&mut self, frame: Frame) {
        let i = self.index_of(frame).expect("freeing a frame we don't own");
        assert!(self.get(i), "double free of frame {:#x}", frame.addr());

        self.set(i, false);
        self.used -= 1;
        self.hint = self.hint.min(i);
    }

    /// Is this frame currently allocated? Test-support, and useful in assertions.
    pub fn is_used(&self, frame: Frame) -> Option<bool> {
        Some(self.get(self.index_of(frame)?))
    }

    fn index_of(&self, frame: Frame) -> Option<usize> {
        let addr = frame.addr();
        if addr < self.base {
            return None;
        }
        let i = ((addr - self.base) / FRAME_SIZE) as usize;
        (i < self.total).then_some(i)
    }

    /// Every frame index touched by `[start, start+size)`, clamped to what we manage.
    ///
    /// Start rounds **down**, end rounds **up**. See [`mark_used`](Self::mark_used).
    fn frame_range(&self, start: u64, size: u64) -> core::ops::Range<usize> {
        if size == 0 || start.saturating_add(size) <= self.base {
            return 0..0;
        }

        let first = start.max(self.base) - self.base;
        let last = (start.saturating_add(size)).saturating_sub(self.base);

        let lo = (first / FRAME_SIZE) as usize;
        let hi = (last.div_ceil(FRAME_SIZE) as usize).min(self.total);

        if lo >= hi { 0..0 } else { lo..hi }
    }

    fn get(&self, i: usize) -> bool {
        self.bitmap[i / 8] & (1 << (i % 8)) != 0
    }

    fn set(&mut self, i: usize, used: bool) {
        let byte = &mut self.bitmap[i / 8];
        let mask = 1 << (i % 8);
        if used {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
    }
}
