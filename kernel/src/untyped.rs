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

//!
//! # Where the bookkeeping lives (milestone 135)
//!
//! The table itself, and every decision taken over it, is `crates/regions`: host-testable, Kani-
//! proved for the arithmetic, and searched by loom for the racing case (`script/interleaving-check`).
//! What stays here is what a crate a model checker can run must not have, which is the I/O: the
//! frame allocator, the direct map, the revoke, and the lock. The division is not tidiness. The
//! double free at `crates/frames/src/lib.rs:315` was a *protocol* bug, and a protocol that lives in
//! a `no_std` kernel module is reachable by neither of this project's two verification tools.

use frames::{FRAME_SIZE, Frame};
use regions::RegionTable;

use crate::memory;
use crate::sync::{IrqSafeMutex, rank};

/// The most untyped regions that can be live **at once**. Object revocation made region slots
/// reusable (the table is generational), so this bounds concurrent regions, not creations over the
/// kernel's lifetime the way the old count-based table did. A system that runs workloads which come
/// and go can create regions without end, as long as no more than this many live at a time.
const MAX_REGIONS: usize = 256;

/// The untyped regions (`crates/regions`, notes/generational-names.md). Generational, which is the
/// reuse the old fixed count-based array lacked: reclaiming a region removes it, which bumps its
/// slot's generation, so every `Untyped` capability minted for that region stops resolving
/// (stale-safe, the same machinery as Tids and endpoint names), and the slot is reused by the next
/// `create`. What an `Object::Untyped` capability carries is the generational `u64` name.
///
/// **Page units inside, byte addresses outside.** The crate is `FRAME_SIZE`-agnostic on purpose, so
/// every conversion happens here, at the boundary where the constant is already in scope.
static REGIONS: IrqSafeMutex<RegionTable<MAX_REGIONS>> =
    IrqSafeMutex::new(rank::UNTYPED, RegionTable::new());

/// Carve `pages` of physical memory out of the frame allocator, once, and make it an untyped
/// region. **This is the kernel's one allocation for this memory**: the seL4 boundary, where all
/// free RAM becomes untyped handed to the first process. Everything the owner does afterward
/// spends this, not the allocator.
pub fn create(pages: u64) -> Option<u64> {
    let base = memory::alloc_contiguous(pages as usize)?.addr();

    let name = REGIONS.lock().insert_root(base / FRAME_SIZE, pages);
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
/// name. seL4's untyped-retype-into-untyped: the subdivision that lets a spawner give each child
/// its own independently-reclaimable region. The parent's watermark advances by `pages` (the run is
/// spent from it, bump-only as ever) and its live-child count rises, so it can no longer be
/// destroyed; the child is an ordinary region over that run. `None` if the parent is unknown,
/// exhausted (`pages` beyond its remaining budget), asks for zero, or the region table is full.
///
/// **Return-of-pages (DECISIONS.md §16):** a child destroyed at the top of the parent's watermark
/// (the LIFO case, which a spawn-then-reap loop always is) gives its pages *back* to the parent's
/// budget, so a split parent is not committed for its lifetime. A child freed out of order leaves a
/// hole until the parent itself is destroyed. This is the LIFO half of seL4's return-to-parent,
/// without the derivation tree that would handle the general case.
pub fn split(parent: u64, pages: u64) -> Option<u64> {
    REGIONS.lock().split(parent, pages)
}

/// Whether this region has live children (was split and they are not all reclaimed), so it cannot be
/// reclaimed (`sched::reclaim_region` refuses it, as [`destroy`] does). `false` for an unknown or
/// stale name. A parent with zero live children is destroyable again, the point of the count over a
/// bool.
pub fn has_children(region: u64) -> bool {
    REGIONS.lock().has_children(region)
}

/// **Retype one page out of the region**, zeroed, returning its physical address. `None` when the
/// region is exhausted: the *process* is out of budget, not the kernel.
///
/// Zeroed because the caller may make this page a page table, where a stale descriptor is a
/// pointer to nowhere followed at speed, and because a process should not see the previous
/// contents of its own untyped.
pub fn retype_page(region: u64) -> Option<u64> {
    // `REGIONS` is released at this statement's semicolon, before the write below: zeroing a page
    // is a memory touch of `FRAME_SIZE` bytes and has no business happening under the region lock.
    let page = REGIONS.lock().retype_page(region)?;
    let phys = page * FRAME_SIZE;

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
/// carve happen under one hold of the region lock, so no [`destroy`] can slip between them and
/// free a page that is about to hold an endpoint. Zeroed like every retyped page.
pub fn retype_object_page(region: u64) -> Option<u64> {
    // Released at the semicolon, as in `retype_page`: the pin and the carve are under the lock, the
    // zeroing is not.
    let page = REGIONS.lock().retype_object_page(region)?;
    let phys = page * FRAME_SIZE;

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
    REGIONS.lock().usage(region)
}

/// This region's physical span `(base, size_in_bytes)`, or `None` if the name is stale. Object
/// revocation needs it to find which kernel objects live in the region (`sched::reclaim_region`
/// scans the registries for TCB/endpoint/aspace pages that fall inside this span).
pub fn region_bounds(region: u64) -> Option<(u64, u64)> {
    REGIONS
        .lock()
        .bounds(region)
        .map(|(base_page, pages)| (base_page * FRAME_SIZE, pages * FRAME_SIZE))
}

/// Clear a region's object pin, **after** its objects have been torn down. Object revocation
/// (`sched::reclaim_region`): reap the objects with the scheduler lock, unpin, then [`destroy`].
///
/// This is deliberately separate from [`destroy`], and the split is not cosmetic. Tearing down a TCB
/// needs `SCHED`; [`destroy`] must never take `SCHED`, because it is reachable from
/// `AddressSpace::Drop`, which already runs under the reaper's `SCHED` (see [`destroy`]'s note). So
/// the `SCHED`-taking reap is one call, and the `SCHED`-free `unpin` + `destroy` are the next.
pub fn unpin(region: u64) {
    REGIONS.lock().unpin(region);
}

/// Return a region's whole backing to the frame allocator, **safely** (milestone 13). The name goes
/// with it: its slot's generation is bumped, so every `Untyped` capability minted for this region
/// stops resolving and the slot is reused by the next `create`/[`split`]. (An older line here said
/// the slot stays and indices are stable; that stopped being true when regions became generational,
/// and the sentence outlived the fact.)
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
///
/// # The claim, and why it is one call
///
/// `RegionTable::claim_for_destroy` decides and removes the name in **one borrow**, so at most one
/// caller ever reaches the free loop below for a given region. This used to be two hold of the
/// region lock with a gap between them, and the gap was a real double free rather than an
/// untidiness: two callers with a name for one region (an owner's `Untyped::DESTROY` and a
/// supervisor's `endpoint::REAP`, both landing in `sched::reclaim_region`) could each pass the
/// refusal check, each release the lock, and each run the free loop over the same pages. It
/// surfaced once in 45 loaded runs on riscv64 (notes/object-revocation.md). The fix landed as pull
/// request #316; milestone 135 moved it into the crate so `script/interleaving-check` searches every
/// interleaving of it rather than the tree arguing it from lock discipline.
pub fn destroy(region: u64) {
    // The guard is a temporary of this `let` statement, so `REGIONS` is released at the semicolon,
    // **before** the revoke below. That order is load-bearing rather than incidental: revocation
    // takes the scheduler lock, a higher rank, and holding `REGIONS` across it would invert the
    // order and trip the rank checker. The pre-#316 code said the same thing with an explicit
    // `drop`; the claim says it with a shorter borrow.
    let Some(claim) = REGIONS.lock().claim_for_destroy(region) else {
        return; // refused (pinned, or has live children), or a racing caller already won
    };
    let base = claim.base_page() * FRAME_SIZE;
    let pages = claim.pages();

    // Unmap any page still mapped anywhere before the pages leave this region, whether they go back
    // to the allocator (a root) or back to the parent's budget (a child); a returned page that a
    // peer still maps would be the §13 use-after-free either way.
    crate::revoke::revoke_region(base, pages * FRAME_SIZE);

    if claim.is_root() {
        // A root region: its pages came from the frame allocator, so they go back to it. The proved
        // `destroy_outcome` guarantees only roots reach this path, which is what makes double-free
        // impossible: a page reaches the allocator only through the one root that owns it.
        for i in 0..pages {
            memory::free(Frame::from_addr(base + i * FRAME_SIZE));
        }
    } else {
        // A child region: its pages return to the parent, never the allocator. The LIFO test and the
        // un-bump happen under one hold, with the amount the proved `destroy_outcome` dictates.
        REGIONS.lock().return_to_parent(&claim);
    }
}
