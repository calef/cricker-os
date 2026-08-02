//! Untyped memory. **The kernel stops allocating.**
//!
//! Milestone 11, and DECISIONS.md §10's deliberately-deferred third axis. The idea, from seL4:
//! the kernel does not own a pool it hands out from. Instead a process holds a capability to a
//! chunk of raw memory (an `Untyped` region, `capability::Object::Untyped`), and to get a page it
//! **retypes** part of that memory into the thing it wants. The kernel is a bookkeeper: it advances a watermark and hands
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
    /// **The region this one was [`split`] out of**, or [`NO_PARENT`] if it was [`create`]d from the
    /// frame allocator (a root). A child's pages belong to its parent, so [`destroy`] returns them to
    /// the parent (not the allocator); only a root frees to the allocator.
    parent: u64,
    /// **How many live children were split off this region.** A region with children cannot be
    /// destroyed (freeing its whole run would double-free a child's pages). Unlike the old single
    /// bool, this is a count, so a parent becomes destroyable again once its last child is reclaimed,
    /// which is what lets a split parent be reclaimed rather than committed for its lifetime.
    children: u32,
}

/// A [`Region::parent`] naming no parent: this region was created from the frame allocator, not
/// split from another. `u64::MAX` never resolves as a `slots` name (its slot bits exceed
/// `MAX_REGIONS`), so it is a safe sentinel.
const NO_PARENT: u64 = u64::MAX;

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
/// region. **This is the kernel's one allocation for this memory**: the seL4 boundary, where all
/// free RAM becomes untyped handed to the first process. Everything the owner does afterward
/// spends this, not the allocator.
pub fn create(pages: u64) -> Option<u64> {
    let base = memory::alloc_contiguous(pages as usize)?.addr();

    let name = REGIONS.lock().table.insert_with(|_| Region {
        base,
        pages,
        watermark: 0,
        pinned: false,
        parent: NO_PARENT,
        children: 0,
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
/// **Return-of-pages (DECISIONS.md §16):** a child destroyed at the top of the parent's watermark
/// (the LIFO case, which a spawn-then-reap loop always is) gives its pages *back* to the parent's
/// budget, so a split parent is not committed for its lifetime. A child freed out of order leaves a
/// hole until the parent itself is destroyed. This is the LIFO half of seL4's return-to-parent,
/// without the derivation tree that would handle the general case.
pub fn split(parent: u64, pages: u64) -> Option<u64> {
    let base = {
        let mut regions = REGIONS.lock();
        let r = regions.get_mut(parent)?;
        // The budget/overflow/progress check is the proved `regions` arithmetic (crates/regions, Kani).
        let new_watermark = regions::split_new_watermark(r.pages, r.watermark, pages)?;
        let base = r.base + r.watermark * FRAME_SIZE;
        r.watermark = new_watermark;
        r.children += 1;
        base
    };
    // A new region entry over the carved run, linked to its parent. If the table has no free slot the
    // run stays spent on the parent (bump-only, the B.4 rule): the caller loses that budget, and the
    // parent keeps the bumped `children` count (it can never drop to a destroyable zero, which is the
    // honest record that a page of it is unaccounted). Practically unreachable, sized for headroom.
    REGIONS.lock().table.insert_with(|_| Region {
        base,
        pages,
        watermark: 0,
        pinned: false,
        parent,
        children: 0,
    })
}

/// Whether this region has live children (was split and they are not all reclaimed), so it cannot be
/// reclaimed (`sched::reclaim_region` refuses it, as `destroy` does). `false` for an unknown or stale
/// name. A parent with zero live children is destroyable again, the point of the count over a bool.
pub fn has_children(region: u64) -> bool {
    REGIONS.lock().get(region).is_some_and(|r| r.children > 0)
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
#[cfg_attr(not(test), allow(dead_code))] // the untyped property test is the only caller
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
    let (base, pages, parent, is_root) = {
        let regions = REGIONS.lock();
        let Some(r) = regions.get(region) else {
            return;
        };
        let is_root = r.parent == NO_PARENT;
        // The refuse/root/child decision is the proved `regions::destroy_outcome` (crates/regions,
        // Kani). Refuse means a live object pins it, or a child still owns part of its run; the LIFO
        // input is irrelevant to a refusal, so a placeholder is fine here.
        if regions::destroy_outcome(r.pinned, r.children, is_root, false, r.pages)
            == regions::DestroyOutcome::Refused
        {
            return;
        }
        (r.base, r.pages, r.parent, is_root)
    };
    // Unmap any page still mapped anywhere before the pages leave this region, whether they go back
    // to the allocator (a root) or back to the parent's budget (a child); a returned page that a
    // peer still maps would be the §13 use-after-free either way.
    crate::revoke::revoke_region(base, pages * FRAME_SIZE);

    if is_root {
        // A root region: its pages came from the frame allocator, so they go back to it. The proved
        // `destroy_outcome` guarantees only roots reach this path, which is what makes double-free
        // impossible: a page reaches the allocator only through the one root that owns it.
        for i in 0..pages {
            memory::free(Frame::from_addr(base + i * FRAME_SIZE));
        }
    } else {
        // A child region: its pages return to the parent, never the allocator. Compute the LIFO test
        // against the parent's *current* watermark and apply the un-bump under one lock (no TOCTOU),
        // with the amount the proved `destroy_outcome` dictates: the child's pages if it sits at the
        // top of the parent's run (re-splittable), else 0 (a hole until the parent itself dies).
        let mut regions = REGIONS.lock();
        if let Some(p) = regions.get_mut(parent) {
            let is_lifo_top = base + pages * FRAME_SIZE == p.base + p.watermark * FRAME_SIZE;
            if let regions::DestroyOutcome::ReturnToParent { unbump } =
                regions::destroy_outcome(false, 0, false, is_lifo_top, pages)
            {
                p.watermark -= unbump;
            }
            p.children = p.children.saturating_sub(1);
        }
    }

    // Remove the slot, bumping its generation: every `Untyped` capability minted for this region now
    // fails to resolve, and the slot is reused by the next `create`/`split`.
    REGIONS.lock().table.remove(region);
}
