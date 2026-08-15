//! The System allocator for nife: the untyped-backed heap, inside std.
//!
//! The algorithm is `crates/user_heap`, generated verbatim into `user_heap.rs` next door by
//! `cargo xtask std-src` (so the host-tested source is the only source). This file is the same
//! policy layer as `user_rt::heap::UntypedHeap`, re-stated for std because std cannot depend on
//! the out-of-tree crate: a spinlock, lazy wiring to the untyped in slot 0, and grow-on-demand
//! by `untyped::MAP` at the top of the committed range.
//!
//! Lazy, because std has no moment where a program says "init the heap": the first allocation
//! (often std's own rt setup) simply finds committed == 0 and grows. A program whose slot 0 is
//! not an untyped gets a failed allocation, which std turns into the OOM abort: honest, if
//! blunt. The spinlock is uncontended today (no `thread::spawn`, phase one) and stays correct
//! under preemption when threads arrive; see notes/std.md for the caveat.

use crate::alloc::Layout;
use crate::sys::pal::nife::{abi, rt};

mod user_heap;

const PAGE: u64 = 4096;
const MIN_GROW_PAGES: u64 = 8;

struct Inner {
    heap: user_heap::Heap,
    committed: u64,
}

struct SpinHeap {
    locked: crate::sync::atomic::AtomicBool,
    inner: crate::cell::UnsafeCell<Inner>,
}

// SAFETY: all access to `inner` goes through the `locked` spinlock.
unsafe impl Sync for SpinHeap {}

static HEAP: SpinHeap = SpinHeap {
    locked: crate::sync::atomic::AtomicBool::new(false),
    inner: crate::cell::UnsafeCell::new(Inner { heap: user_heap::Heap::new(), committed: 0 }),
};

struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        HEAP.locked.store(false, crate::sync::atomic::Ordering::Release);
    }
}

fn lock() -> Guard {
    use crate::sync::atomic::Ordering;
    while HEAP
        .locked
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        crate::hint::spin_loop();
    }
    Guard
}

impl Guard {
    #[allow(clippy::mut_from_ref)]
    fn inner(&mut self) -> &mut Inner {
        // SAFETY: holding the guard means holding the spinlock; access is exclusive.
        unsafe { &mut *HEAP.inner.get() }
    }
}

/// Map at least `need` more contiguous bytes at the top of the committed range, from the
/// untyped in slot 0. Same growth policy as `user_rt::heap`: geometric, floor of 8 pages,
/// capped by `rt::HEAP_MAX`; only a fresh run big enough for the request counts as success.
fn grow(inner: &mut Inner, need: u64) -> bool {
    let need_pages = need.div_ceil(PAGE);
    let want_pages = need_pages
        .max(inner.committed / PAGE)
        .max(MIN_GROW_PAGES)
        .min((rt::HEAP_MAX - inner.committed) / PAGE);
    if want_pages < need_pages {
        return false;
    }
    let start = rt::HEAP_BASE + inner.committed;
    let mut got: u64 = 0;
    while got < want_pages {
        let va = start + got * PAGE;
        // SAFETY: plain syscall; the kernel validates the slot, the va, and the budget.
        if unsafe { rt::invoke(rt::UNTYPED_SLOT, abi::untyped::MAP, va, 0, 0) } != 0 {
            break;
        }
        got += 1;
    }
    if got > 0 {
        inner.committed += got * PAGE;
        // SAFETY: the range was just mapped writable for this process alone, adjacent to (or
        // the start of) the range already donated.
        unsafe { inner.heap.add_region(start as *mut u8, (got * PAGE) as usize) };
    }
    got >= need_pages
}

#[inline]
pub unsafe fn alloc(layout: Layout) -> *mut u8 {
    let mut g = lock();
    let inner = g.inner();
    loop {
        if let Some(p) = inner.heap.alloc(layout) {
            return p.as_ptr();
        }
        let slack = layout.align().saturating_sub(PAGE as usize) as u64;
        let need = user_heap::effective_size(layout) as u64 + slack;
        if !grow(inner, need) {
            return crate::ptr::null_mut();
        }
    }
}

#[inline]
pub unsafe fn alloc_zeroed(layout: Layout) -> *mut u8 {
    // Fresh pages from untyped::MAP arrive zeroed, but recycled free-list memory does not, so
    // zero unconditionally rather than track provenance.
    let p = unsafe { alloc(layout) };
    if !p.is_null() {
        unsafe { crate::ptr::write_bytes(p, 0, layout.size()) };
    }
    p
}

#[inline]
pub unsafe fn dealloc(ptr: *mut u8, layout: Layout) {
    let Some(p) = crate::ptr::NonNull::new(ptr) else {
        return;
    };
    let mut g = lock();
    // SAFETY: forwarded GlobalAlloc contract: `ptr` came from `alloc(layout)` on this heap.
    unsafe { g.inner().heap.dealloc(p, layout) };
}

#[inline]
pub unsafe fn realloc(ptr: *mut u8, old_layout: Layout, new_size: usize) -> *mut u8 {
    // SAFETY: the GlobalAlloc realloc contract guarantees `new_size` is valid for the align.
    unsafe {
        let new_layout = Layout::from_size_align_unchecked(new_size, old_layout.align());
        let new_ptr = alloc(new_layout);
        if !new_ptr.is_null() {
            crate::ptr::copy_nonoverlapping(ptr, new_ptr, usize::min(old_layout.size(), new_size));
            dealloc(ptr, old_layout);
        }
        new_ptr
    }
}
