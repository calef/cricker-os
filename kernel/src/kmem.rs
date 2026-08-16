//! **The kernel's own object budget** (milestone 19c.1; design/init-and-granular-spawn.md).
//!
//! One untyped region, carved once at first use, that every kernel-owned object draws from:
//! kernel endpoints (moved here from the scheduler's private region), kernel thread stacks
//! (which used to draw open-endedly from the frame allocator), and, with 19c.2, the kernel's
//! own TCB pages. This is the milestone 14 thesis extended to its last uncovered corner: after
//! this carve, **the kernel cannot spend beyond it, structurally**. The frame allocator's count
//! does not move when kernel threads come and go; a steady-state test asserts exactly zero.
//!
//! # Recycling, and why it is not an allocator
//!
//! Region pages are watermark-carved and never individually returned (untyped.rs). Kernel
//! threads churn constantly (every test spawns some), so spend-only pages would exhaust any
//! carve. The fix is a fixed free-stack of dead object pages, reused page-for-page: a reaped
//! thread's stack pages and (19c.2) TCB page go on it, the next spawn takes them back. This is
//! page-granular, bounded by the carve, FIFO-free, and deliberately not a general allocator:
//! one size (a page), one owner (the kernel), no headers, no coalescing, nothing to verify
//! beyond a bounded stack. The distinction between this and the heap milestone 14 deleted is
//! the distinction between a budget and a market.
//!
//! Creator-paid, resolved (the 19c conversation): a kernel-created thread's stack comes from
//! this carve because **the kernel is its creator**; an init-created TCB's memory comes from
//! init's untyped because init is. One principle, two payers, no third regime.

use frames::FRAME_SIZE;

use crate::sync::{IrqSafeMutex, rank};

/// The carve: 1024 pages (4 MiB). Endpoints (one page each, ~60 across a full test run), live
/// kernel stacks (6 pages x up to `MAX_THREADS` = 768; see thread.rs `STACK_PAGES`), TCB pages
/// (19c.2), and slack. Exhaustion fails the spawn or create cleanly, the same contract as every
/// budget since milestone 11; raising this constant is the fix if the image ever legitimately
/// needs more, and 2026-08-15 was such a day: this was 768 when stacks were 4 pages (512 of 768
/// budgeted for stacks, the same two-to-one ratio kept here), and the 24 KiB stacks made stacks
/// alone outgrow the whole carve. The symptom was `kmem::page()` returning `None` late in the
/// aarch64 suite, surfacing as an unrelated test's spawn failing with "no memory to wire one",
/// exactly the failure shape this module's budget contract promises (clean refusal, far from the
/// cause).
const KERNEL_OBJ_PAGES: u64 = 1024;

struct Pool {
    /// The region id, once carved. Lazy: the first object need triggers the carve, which is
    /// early (the idle thread's stack) and cannot fragment-fail at that point.
    region: Option<u64>,
    /// Dead object pages, ready for reuse. Bounded by the carve itself.
    free: [u64; KERNEL_OBJ_PAGES as usize],
    free_len: usize,
}

/// Rank note: taken alone or under SCHED (`create_endpoint` holds SCHED, then this, then the
/// region lock: 60 -> 59 -> 58, a legal descent). Shares 59 with INBOX and MAPPINGS, with which
/// it is never nested: inboxes are scheduling traffic, the mapping registry is user-space
/// bookkeeping, and this is the kernel shopping for its own pages.
static POOL: IrqSafeMutex<Pool> = IrqSafeMutex::new(
    rank::KMEM,
    Pool {
        region: None,
        free: [0; KERNEL_OBJ_PAGES as usize],
        free_len: 0,
    },
);

/// One page of the kernel's budget, zeroed, recycled if a dead object's page is waiting, carved
/// from the region otherwise. `None` when the carve is exhausted (raise `KERNEL_OBJ_PAGES`).
pub fn page() -> Option<u64> {
    let phys = {
        let mut pool = POOL.lock();
        if pool.free_len > 0 {
            pool.free_len -= 1;
            let phys = pool.free[pool.free_len];
            drop(pool);
            // Recycled pages carry the dead object's bytes; zero for parity with retype, so a
            // recycled page is indistinguishable from a fresh one to whatever it becomes next.
            // SAFETY: the page is the pool's own, off every live structure, direct-mapped.
            unsafe {
                core::ptr::write_bytes(
                    crate::arch::mmu::phys_to_virt(phys) as *mut u8,
                    0,
                    FRAME_SIZE as usize,
                );
            }
            return Some(phys);
        }
        let region = match pool.region {
            Some(r) => r,
            None => {
                let Some(r) = crate::untyped::create(KERNEL_OBJ_PAGES) else {
                    crate::println!("kmem: the {KERNEL_OBJ_PAGES}-page carve itself was refused");
                    return None;
                };
                pool.region = Some(r);
                r
            }
        };
        drop(pool);
        // Pin-and-carve, like every object page since 19a: this region hosts kernel objects for
        // the machine's lifetime, and nothing may ever destroy it.
        let Some(phys) = crate::untyped::retype_object_page(region) else {
            crate::println!(
                "kmem: carve exhausted ({KERNEL_OBJ_PAGES} pages spent, none recycled); raise \
                 KERNEL_OBJ_PAGES"
            );
            return None;
        };
        phys
    };
    Some(phys)
}

/// A dead object's page comes home. The caller guarantees nothing references it any more (the
/// object was dropped in place, its mappings unmapped); the next `page()` may hand it out.
pub fn recycle(phys: u64) {
    let mut pool = POOL.lock();
    debug_assert!(
        pool.free_len < pool.free.len(),
        "more recycled pages than the carve holds"
    );
    if pool.free_len < pool.free.len() {
        let n = pool.free_len;
        pool.free[n] = phys;
        pool.free_len += 1;
    } // else: leak rather than corrupt; unreachable per the bound above
}
