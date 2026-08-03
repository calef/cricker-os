//! **The untyped-backed heap: `alloc` for userspace, paid from the process's own budget**
//! (milestone 27).
//!
//! A [`GlobalAlloc`] that grows by mapping pages out of the untyped capability the process was
//! granted, via `untyped::MAP` (one page per invoke, zeroed by the kernel, writable). The
//! algorithm itself lives in `crates/user_heap` and is host-tested; this module is the policy and the
//! syscall glue: a spinlock, a committed-range cursor, and grow-on-demand.
//!
//! # The contract a program signs
//!
//! Declaring the allocator is two lines and one call, all in the program's own source, because
//! the heap's inputs are part of that program's capability contract (notes/abi.md §4):
//!
//! ```ignore
//! #[global_allocator]
//! static HEAP: user_rt::heap::UntypedHeap = user_rt::heap::UntypedHeap::new();
//! // first thing in _start:
//! HEAP.init(UNTYPED_SLOT, user_rt::heap::DEFAULT_BASE, 4 * 1024 * 1024);
//! ```
//!
//! - **Which untyped slot** feeds the heap is the program's convention with its parent, like
//!   every other slot. There is no ambient memory to fall back on: no untyped granted, no heap.
//! - **Where the heap lives** is a virtual range the program promises not to use for anything
//!   else. [`DEFAULT_BASE`] (1 GiB) clears every VA convention in the tree today (program text at
//!   `0x40_0000`, stacks, the shared pages, init's scratch at `0x1000_0000`, the initrd at
//!   `0x2000_0000`).
//! - **`max_bytes`** caps growth so a leak exhausts the heap, visibly, before it silently eats
//!   the budget the program also builds children from. The untyped itself is the hard ceiling
//!   either way: `MAP` returns `OutOfMemory` when the region is spent, and the allocator reports
//!   that as an allocation failure (which `alloc`'s default OOM handler turns into a panic).
//!
//! Allocating before `init` fails cleanly (null, then the OOM panic): there is nothing to grow
//! from until the program says which capability pays.
//!
//! # Why a spinlock is enough
//!
//! `GlobalAlloc` must be `Sync`. Today a process is one thread (std's `thread::spawn` is
//! `Unsupported` in milestone 27 phase one), so the lock is never contended; when threads within
//! one process arrive, a spinlock stays correct under this kernel's preemptive scheduler because
//! the holder always gets scheduled again to release it. What it is *not* is efficient under real
//! contention; that is a problem worth having, and notes/heap.md carries the caveat.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, Ordering};

/// The suggested heap base: 1 GiB, clear of every VA the userspace tree currently hardcodes.
/// A convention, not a law; a program with its own layout passes its own base to [`init`].
///
/// [`init`]: UntypedHeap::init
pub const DEFAULT_BASE: u64 = 0x4000_0000;

const PAGE: u64 = 4096;

/// The smallest growth step, in pages. Growing one page per allocation would make every early
/// `Vec` push a syscall; 8 pages (32 KiB) amortizes that without hoarding a small budget.
const MIN_GROW_PAGES: u64 = 8;

struct Inner {
    heap: user_heap::Heap,
    /// Which cspace slot holds the untyped that pays for pages. `u64::MAX` until `init`.
    untyped_slot: u64,
    /// The heap's virtual range: `[base, base + max)`, of which `[base, base + committed)` is
    /// mapped and donated to the allocator so far.
    base: u64,
    committed: u64,
    max: u64,
}

/// The untyped-backed global allocator. One static per program, named in its
/// `#[global_allocator]` attribute; see the module docs for the contract.
pub struct UntypedHeap {
    locked: AtomicBool,
    inner: core::cell::UnsafeCell<Inner>,
}

// SAFETY: all access to `inner` goes through the `locked` spinlock (see `lock`).
unsafe impl Sync for UntypedHeap {}

/// A held lock on the heap; releases on drop. Internal only.
struct Guard<'a>(&'a UntypedHeap);

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.locked.store(false, Ordering::Release);
    }
}

impl Guard<'_> {
    fn inner(&mut self) -> &mut Inner {
        // SAFETY: holding the guard means holding the spinlock; access is exclusive.
        unsafe { &mut *self.0.inner.get() }
    }
}

impl UntypedHeap {
    /// An empty, un-wired heap, `const` so it can be the `#[global_allocator]` static.
    pub const fn new() -> Self {
        UntypedHeap {
            locked: AtomicBool::new(false),
            inner: core::cell::UnsafeCell::new(Inner {
                heap: user_heap::Heap::new(),
                untyped_slot: u64::MAX,
                base: 0,
                committed: 0,
                max: 0,
            }),
        }
    }

    /// Wire the heap to its budget: pages come from the untyped in `untyped_slot`, mapped at
    /// `[base, base + max_bytes)`. Call once, before the first allocation, first thing in
    /// `_start`. `base` must be page-aligned and inside the user half.
    pub fn init(&self, untyped_slot: u64, base: u64, max_bytes: u64) {
        let mut g = self.lock();
        let inner = g.inner();
        inner.untyped_slot = untyped_slot;
        inner.base = base;
        inner.committed = 0;
        inner.max = max_bytes & !(PAGE - 1);
    }

    /// Bytes of untyped currently mapped for the heap. The demo workload reports this so the
    /// test can assert growth actually happened (and stayed bounded).
    pub fn committed_bytes(&self) -> u64 {
        let mut g = self.lock();
        g.inner().committed
    }

    fn lock(&self) -> Guard<'_> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Guard(self)
    }

    /// Map at least `need` more contiguous bytes at the top of the committed range and donate
    /// them to the allocator. Returns false when the cap or the budget refuses, in which case a
    /// retry cannot succeed and the allocation must fail.
    fn grow(inner: &mut Inner, need: u64) -> bool {
        if inner.untyped_slot == u64::MAX {
            return false; // init() has not run; there is nothing to pay with.
        }
        let need_pages = need.div_ceil(PAGE);
        // Grow geometrically (double the committed range) but never less than the request and
        // never past the cap, so a burst of small allocations costs O(log n) syscall batches.
        let want_pages = need_pages
            .max(inner.committed / PAGE) // doubling: as many again as we have
            .max(MIN_GROW_PAGES)
            .min((inner.max - inner.committed) / PAGE);
        if want_pages < need_pages {
            return false; // the cap is closer than the request; growing cannot help.
        }

        let start = inner.base + inner.committed;
        let mut got: u64 = 0;
        while got < want_pages {
            let va = start + got * PAGE;
            // SAFETY: plain syscall; the kernel validates the slot, the va, and the budget.
            if unsafe { crate::invoke(inner.untyped_slot, abi::untyped::MAP, va, 0, 0) } != 0 {
                break; // budget spent (OutOfMemory); keep what we got.
            }
            got += 1;
        }
        if got > 0 {
            inner.committed += got * PAGE;
            // SAFETY: `[start, start + got * PAGE)` was just mapped writable for us alone, and
            // is adjacent to (or the start of) the range already donated.
            unsafe {
                inner
                    .heap
                    .add_region(start as *mut u8, (got * PAGE) as usize);
            };
        }
        // Only a fresh contiguous run at least as big as the request guarantees the retry
        // succeeds; anything less and the honest answer is allocation failure.
        got >= need_pages
    }
}

impl Default for UntypedHeap {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: alloc/dealloc uphold the GlobalAlloc contract: unique live pointers, layout round-trip
// (user_heap recomputes sizes from the same Layout), and no unwinding (there is none on this target).
unsafe impl GlobalAlloc for UntypedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut g = self.lock();
        let inner = g.inner();
        loop {
            if let Some(p) = inner.heap.alloc(layout) {
                return p.as_ptr();
            }
            // Worst case the fresh run must hold the request plus alignment slack (a fresh run
            // starts page-aligned, so only over-page alignments need the slack).
            let slack = layout.align().saturating_sub(PAGE as usize) as u64;
            let need = user_heap::effective_size(layout) as u64 + slack;
            if !Self::grow(inner, need) {
                return core::ptr::null_mut();
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let Some(p) = core::ptr::NonNull::new(ptr) else {
            return;
        };
        let mut g = self.lock();
        // SAFETY: forwarded GlobalAlloc contract: `ptr` came from `alloc(layout)` on this heap.
        unsafe { g.inner().heap.dealloc(p, layout) };
    }
}
