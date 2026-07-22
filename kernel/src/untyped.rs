//! Untyped memory. **The kernel stops allocating.**
//!
//! Milestone 11, and DECISIONS.md §10's deliberately-deferred third axis. The idea, from seL4:
//! the kernel does not own a pool it hands out from. Instead a process holds a capability to a
//! chunk of raw memory (an [`Untyped`] region), and to get a page it **retypes** part of that
//! memory into the thing it wants. The kernel is a bookkeeper: it advances a watermark and hands
//! back a physical address. It calls no allocator.
//!
//! # What this buys, and the one number that proves it
//!
//! After a process is handed its untyped, **the kernel's free-frame count does not move while the
//! process allocates.** Every page the process maps comes out of its own untyped, carved once at
//! the start. A process cannot make the kernel allocate, so it cannot exhaust kernel memory: it
//! can only run out of *its own* budget, and when it does, the retype fails and the kernel is
//! untouched. That is the astonishing property, and `notes/untyped.md` shows the flat frame count.
//!
//! # What this is NOT, said plainly
//!
//! This converts **user memory** (a process's pages and their page tables) to untyped. The
//! kernel's *own* objects (the `Thread` structs, the scheduler's collections, endpoints) still
//! come from the kernel heap. Converting each of those is the same retype mechanism applied to a
//! kernel object, and it is the long tail seL4 spent years on. What milestone 11 establishes is
//! the mechanism and the property, for the memory a process spends.

use crate::memory;
use crate::sync::{IrqSafeMutex, rank};
use alloc::vec::Vec;
use frames::{FRAME_SIZE, Frame};

/// One untyped region: a run of physical pages, and how far into it we have retyped.
struct Region {
    base: u64,
    pages: u64,
    /// Pages handed out so far. A bump pointer, and the whole of the allocator.
    watermark: u64,
}

/// The untyped regions. A `Vec` for the *table* is still heap (the kernel's own bookkeeping); the
/// **memory the regions describe** is the thing that no longer comes from the allocator on the
/// hot path. Indexed by the `usize` inside an `Object::Untyped` capability.
static REGIONS: IrqSafeMutex<Vec<Region>> = IrqSafeMutex::new(rank::UNTYPED, Vec::new());

/// Carve `pages` of physical memory out of the frame allocator, once, and make it an untyped
/// region. **This is the kernel's one allocation for this memory** — the seL4 boundary, where all
/// free RAM becomes untyped handed to the first process. Everything the owner does afterward
/// spends this, not the allocator.
pub fn create(pages: u64) -> Option<usize> {
    let base = memory::alloc_contiguous(pages as usize)?.addr();

    let mut regions = REGIONS.lock();
    regions.push(Region {
        base,
        pages,
        watermark: 0,
    });
    Some(regions.len() - 1)
}

/// **Retype one page out of the region**, zeroed, returning its physical address. `None` when the
/// region is exhausted: the *process* is out of budget, not the kernel.
///
/// Zeroed because the caller may make this page a page table, where a stale descriptor is a
/// pointer to nowhere followed at speed, and because a process should not see the previous
/// contents of its own untyped.
pub fn retype_page(region: usize) -> Option<u64> {
    let mut regions = REGIONS.lock();
    let r = regions.get_mut(region)?;

    if r.watermark >= r.pages {
        return None; // exhausted
    }
    let phys = r.base + r.watermark * FRAME_SIZE;
    r.watermark += 1;
    drop(regions);

    // SAFETY: the page is inside a region we carved from the allocator and own exclusively; the
    // direct map reaches it. Zero it before anyone can read a stale descriptor out of it.
    unsafe {
        core::ptr::write_bytes(
            crate::arch::mmu::phys_to_virt(phys) as *mut u8,
            0,
            FRAME_SIZE as usize,
        );
    }
    Some(phys)
}

/// How many pages the region has retyped, and its size. For the demo and tests.
#[allow(dead_code)] // used by the property test
pub fn usage(region: usize) -> Option<(u64, u64)> {
    let regions = REGIONS.lock();
    regions.get(region).map(|r| (r.watermark, r.pages))
}

/// Return a region's whole backing to the frame allocator. The region is emptied but its slot
/// stays (indices are stable).
///
/// # TRIPWIRE: do not wire this (or any reclamation) up without revocation first
///
/// This function is currently **unused on purpose**, and that is load-bearing. Wiring it up (say,
/// to run on process death) turns a *safe* gap into a use-after-free, and the two changes are far
/// enough apart that the person who wires it will not be thinking about the reason it is unsafe.
///
/// The chain: a `Frame` retyped out of this region can be **shared** (a read-only derivative
/// delegated to a peer, which maps the same physical page — see notes/capability-lifecycle.md).
/// Address-space teardown deliberately does **not** free such a leaf (notes/teardown.md), so today
/// a peer's mapping stays valid because these pages are **spend-only: never reclaimed**. The moment
/// this `free` runs while a peer still maps one of these frames, that mapping dangles onto memory
/// the allocator can hand out again. UAF.
///
/// So reclamation is **blocked on revocation**: before anything calls this, the kernel must be able
/// to unmap a shared frame from *every* holder first (a capability-derivation tree + recursive
/// revoke). That is the deferred work in DECISIONS.md "Open design ideas". Until then, keep this
/// dead, or the no-revocation gap flips from a control limitation to a memory-safety hole.
#[allow(dead_code)]
pub fn destroy(region: usize) {
    let (base, pages) = {
        let mut regions = REGIONS.lock();
        let Some(r) = regions.get_mut(region) else {
            return;
        };
        let bp = (r.base, r.pages);
        r.pages = 0;
        r.watermark = 0;
        bp
    };
    for i in 0..pages {
        memory::free(Frame::from_addr(base + i * FRAME_SIZE));
    }
}
