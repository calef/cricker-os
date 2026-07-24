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
//! # Where the boundary sits now (updated across milestone 14)
//!
//! Milestone 11 converted the memory a process **asks for** (`Untyped::MAP` pages) to untyped.
//! Milestone 14 phase B.4 converted the memory a process **is made of**: `exec` carves one
//! region per process and the address space's root, tables, and image pages are all retyped
//! from it, so teardown is [`destroy`] and the whole budget returns in one call. The kernel's
//! own objects went fixed instead of untyped-backed (TCBs in a static pool, endpoints in a
//! fixed table; notes/tcb.md records why retype earns nothing while the kernel is the only
//! payer). What remains heap-backed is the revocation database, phase C's work.

use crate::memory;
use crate::sync::{IrqSafeMutex, rank};
use frames::{FRAME_SIZE, Frame};

/// One untyped region: a run of physical pages, and how far into it we have retyped.
#[derive(Clone, Copy)]
struct Region {
    base: u64,
    pages: u64,
    /// Pages handed out so far. A bump pointer, and the whole of the allocator.
    watermark: u64,
}

/// The most untyped regions that can ever be created. Region ids live inside capabilities and
/// slots are never reused (`destroy` empties a region but keeps its slot), so this bounds
/// creations over the kernel's lifetime, not concurrent use. Phase B.4 gives every user
/// process one, spent at exec and dead-slotted at teardown, so a long test run burns a slot
/// per process it ever ran; 256 leaves the full suite headroom. Slot reuse (generational,
/// like the thread table) is the known fix if this ever binds.
const MAX_REGIONS: usize = 256;

/// The untyped regions, in a fixed table (milestone 14 phase B.1): the kernel's own bookkeeping
/// no longer grows either. Indexed by the `usize` inside an `Object::Untyped` capability.
struct Regions {
    entries: [Region; MAX_REGIONS],
    count: usize,
}

static REGIONS: IrqSafeMutex<Regions> = IrqSafeMutex::new(
    rank::UNTYPED,
    Regions {
        entries: [Region {
            base: 0,
            pages: 0,
            watermark: 0,
        }; MAX_REGIONS],
        count: 0,
    },
);

impl Regions {
    fn get(&self, i: usize) -> Option<&Region> {
        (i < self.count).then(|| &self.entries[i])
    }

    fn get_mut(&mut self, i: usize) -> Option<&mut Region> {
        (i < self.count).then(|| &mut self.entries[i])
    }
}

/// Carve `pages` of physical memory out of the frame allocator, once, and make it an untyped
/// region. **This is the kernel's one allocation for this memory** — the seL4 boundary, where all
/// free RAM becomes untyped handed to the first process. Everything the owner does afterward
/// spends this, not the allocator.
pub fn create(pages: u64) -> Option<usize> {
    let base = memory::alloc_contiguous(pages as usize)?.addr();

    let mut regions = REGIONS.lock();
    if regions.count == MAX_REGIONS {
        // Out of region slots: give the memory back rather than leak it. A bounded table is the
        // point (B.1); the bound is sized so this is an image misconfiguration, not a runtime path.
        for i in 0..pages {
            memory::free(Frame::from_addr(base + i * FRAME_SIZE));
        }
        return None;
    }
    let id = regions.count;
    regions.entries[id] = Region {
        base,
        pages,
        watermark: 0,
    };
    regions.count += 1;
    Some(id)
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

/// Return a region's whole backing to the frame allocator, **safely** (milestone 13). The region is
/// emptied but its slot stays (indices are stable).
///
/// # This was a tripwire, and revocation is what disarmed it
///
/// It used to be unused on purpose, because reclaiming a region while a peer still maps one of its
/// frames dangles that mapping onto memory the allocator can hand out again: a use-after-free. The
/// safety of the whole system rested on retyped frames being **spend-only, never reused**, so a
/// surviving peer mapped valid, non-reused memory (notes/capability-lifecycle.md, notes/teardown.md).
///
/// That precondition is now *met* rather than assumed. Before freeing anything, this revokes every
/// mapped page in the region (revoke.rs, §13): each is unmapped from every address space that held
/// it and every `Frame` capability to it is deleted. So "no live mapping survives" replaces
/// "spend-only, never reused", and returning the pages to the allocator is safe. `REGIONS` is
/// released before the revoke so revocation can take the scheduler lock (a higher rank) without
/// inverting the order.
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
    crate::revoke::revoke_region(base, pages * FRAME_SIZE);
    for i in 0..pages {
        memory::free(Frame::from_addr(base + i * FRAME_SIZE));
    }
}
