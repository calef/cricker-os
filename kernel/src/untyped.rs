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
    /// **A kernel object lives in this region** (milestone 19a): a page here was retyped into an
    /// endpoint (later: an address space, a TCB), so [`destroy`] refuses the region. Object
    /// revocation clears this through [`unpin`] once the objects are torn down (`sched::reclaim_region`).
    pinned: bool,
    /// **This region was carved into a child untyped** (via [`split`]), so part of its run now
    /// belongs to a child region that will free those pages itself. [`destroy`] refuses a parent,
    /// because freeing its whole run would double-free the child's pages. This is the no-CDT
    /// stand-in for seL4's "an untyped with children cannot be revoked until the children are":
    /// a single bool, never reset, so a split region is committed for the spawner's lifetime (the
    /// tradeoff, recorded in DECISIONS.md: no return-of-pages-to-parent, the parent just commits).
    has_children: bool,
}

/// The most untyped regions that can be live **at once**. Object revocation made region slots
/// reusable (see [`Regions`]), so this now bounds concurrent regions, not creations over the
/// kernel's lifetime the way the old count-based table did. A system that runs workloads which come
/// and go can create regions without end, as long as no more than this many live at a time.
const MAX_REGIONS: usize = 256;

/// The untyped regions, a **generational table** (`crates/slots`, notes/generational-names.md).
/// This is the reuse the old fixed count-based array lacked: [`destroy`] removes a region, which
/// bumps its slot's generation, so every `Untyped` capability minted for that region stops
/// resolving (stale-safe, the same machinery as Tids and endpoint names), and the slot is reused by
/// the next `create`. What an `Object::Untyped` capability carries is the generational `u64` name.
struct Regions {
    table: slots::Table<Region, MAX_REGIONS>,
}

static REGIONS: IrqSafeMutex<Regions> = IrqSafeMutex::new(
    rank::UNTYPED,
    Regions {
        table: slots::Table::new(),
    },
);

impl Regions {
    fn get(&self, name: u64) -> Option<&Region> {
        self.table.get(name)
    }

    fn get_mut(&mut self, name: u64) -> Option<&mut Region> {
        self.table.get_mut(name)
    }
}

/// Carve `pages` of physical memory out of the frame allocator, once, and make it an untyped
/// region. **This is the kernel's one allocation for this memory** — the seL4 boundary, where all
/// free RAM becomes untyped handed to the first process. Everything the owner does afterward
/// spends this, not the allocator.
pub fn create(pages: u64) -> Option<u64> {
    let base = memory::alloc_contiguous(pages as usize)?.addr();

    let name = REGIONS.lock().table.insert_with(|_| Region {
        base,
        pages,
        watermark: 0,
        pinned: false,
        has_children: false,
    });
    if name.is_none() {
        // No free region slot: give the memory back rather than leak it. With reuse this is now a
        // genuine concurrency limit (too many live regions), not a lifetime one.
        for i in 0..pages {
            memory::free(Frame::from_addr(base + i * FRAME_SIZE));
        }
    }
    name
}

/// **Carve `pages` off `parent`'s unspent budget into a new child untyped region**, and return its
/// index. seL4's untyped-retype-into-untyped: the subdivision that lets a spawner give each child
/// its own independently-reclaimable region. The parent's watermark advances by `pages` (the run is
/// spent from it, bump-only as ever) and the parent is marked `has_children`, so it can no longer be
/// destroyed; the child is an ordinary region over that run, freeing its pages to the allocator at
/// its own `destroy`. `None` if the parent is unknown, exhausted (`pages` beyond its remaining
/// budget), asks for zero, or the region table is full.
///
/// **The tradeoff (DECISIONS.md):** a child does not return its pages to the parent (the bump
/// allocator has no free list), so the parent is permanently committed once split. A spawner sizes
/// its budget for the children it will ever carve. seL4 returns pages to the parent via its
/// derivation tree, which we deliberately do not build.
pub fn split(parent: u64, pages: u64) -> Option<u64> {
    let base = {
        let mut regions = REGIONS.lock();
        let r = regions.get_mut(parent)?;
        if pages == 0 || r.watermark + pages > r.pages {
            return None; // zero, or beyond the parent's remaining budget
        }
        let base = r.base + r.watermark * FRAME_SIZE;
        r.watermark += pages;
        r.has_children = true;
        base
    };
    // A new region entry over the carved run. If the table has no free slot the run stays spent on
    // the parent (bump-only, the B.4 rule): the caller loses that budget, nobody else's.
    REGIONS.lock().table.insert_with(|_| Region {
        base,
        pages,
        watermark: 0,
        pinned: false,
        has_children: false,
    })
}

/// Whether this region has been carved into a child untyped, so it cannot be reclaimed
/// (`sched::reclaim_region` refuses it, as `destroy` does). `false` for an unknown or stale name.
pub fn has_children(region: u64) -> bool {
    REGIONS.lock().get(region).is_some_and(|r| r.has_children)
}

/// **Retype one page out of the region**, zeroed, returning its physical address. `None` when the
/// region is exhausted: the *process* is out of budget, not the kernel.
///
/// Zeroed because the caller may make this page a page table, where a stale descriptor is a
/// pointer to nowhere followed at speed, and because a process should not see the previous
/// contents of its own untyped.
pub fn retype_page(region: u64) -> Option<u64> {
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

/// **Retype one page for a kernel object, pinning the region in the same breath** (19a). Pin and
/// carve happen under one hold of the region lock, so no `destroy` can slip between them and
/// free a page that is about to hold an endpoint. Zeroed like every retyped page.
pub fn retype_object_page(region: u64) -> Option<u64> {
    let mut regions = REGIONS.lock();
    let r = regions.get_mut(region)?;
    if r.watermark >= r.pages {
        return None; // exhausted: the caller is out of budget, and nothing was pinned for it
    }
    r.pinned = true;
    let phys = r.base + r.watermark * FRAME_SIZE;
    r.watermark += 1;
    drop(regions);

    // SAFETY: as retype_page: exclusively ours, direct-mapped; zero before anyone reads it.
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
pub fn usage(region: u64) -> Option<(u64, u64)> {
    let regions = REGIONS.lock();
    regions.get(region).map(|r| (r.watermark, r.pages))
}

/// This region's physical span `(base, size_in_bytes)`, or `None` if the index is stale. Object
/// revocation needs it to find which kernel objects live in the region (`sched::reclaim_region`
/// scans the registries for TCB/endpoint/aspace pages that fall inside this span).
pub fn region_bounds(region: u64) -> Option<(u64, u64)> {
    let regions = REGIONS.lock();
    regions.get(region).map(|r| (r.base, r.pages * FRAME_SIZE))
}

/// Clear a region's object pin, **after** its objects have been torn down. Object revocation
/// (`sched::reclaim_region`): reap the objects with the scheduler lock, unpin, then `destroy`.
///
/// This is deliberately separate from `destroy`, and the split is not cosmetic. Tearing down a TCB
/// needs `SCHED`; `destroy` must never take `SCHED`, because it is reachable from
/// `AddressSpace::Drop`, which already runs under the reaper's `SCHED` (see `destroy`'s note). So
/// the `SCHED`-taking reap is one call, and the `SCHED`-free `unpin` + `destroy` are the next.
pub fn unpin(region: u64) {
    let mut regions = REGIONS.lock();
    if let Some(r) = regions.get_mut(region) {
        r.pinned = false;
    }
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
pub fn destroy(region: u64) {
    let (base, pages) = {
        let mut regions = REGIONS.lock();
        let Some(r) = regions.get_mut(region) else {
            return;
        };
        // Pinned: a kernel object lives in one of these pages (object revocation unpins first).
        // has_children: part of this run was split off to a child region that frees it itself, so
        // freeing the whole run here would double-free the child's pages. Either way, refuse.
        if r.pinned || r.has_children {
            return;
        }
        (r.base, r.pages)
    };
    crate::revoke::revoke_region(base, pages * FRAME_SIZE);
    for i in 0..pages {
        memory::free(Frame::from_addr(base + i * FRAME_SIZE));
    }
    // Remove the slot, bumping its generation: every `Untyped` capability minted for this region
    // now fails to resolve, and the slot is reused by the next `create`. This is the lifetime-cap
    // fix, the region table no longer fills up permanently the way the count-based one did.
    REGIONS.lock().table.remove(region);
}
