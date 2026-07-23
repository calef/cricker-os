//! Capability revocation and untyped reclamation (milestone 13, DECISIONS §13).
//!
//! Until now a granted capability could not be retracted and a spent page could not be reclaimed.
//! That was safe only by a structural accident: retyped frames are **spend-only, never reused**
//! (untyped.rs), so a peer that still mapped a shared frame after the granter left was mapping
//! valid, non-reused memory. `untyped::destroy` carried a tripwire saying exactly this: wiring up
//! any reclamation before revocation exists turns those "harmless" dangling mappings into a
//! use-after-free.
//!
//! This module is that revocation. It keeps a **mapping database, lite**: every mapping of an
//! untyped-derived page. To revoke a page it unmaps it from *every* address space that held it and
//! deletes every `Frame` capability to it, after which no holder maps it and no capability names it,
//! so the page is safe to return to the allocator. seL4 keeps a full capability-derivation tree and
//! revokes a *subtree*; this keeps only the unmap side and revokes *all* derivatives of a page,
//! which is precisely what reclamation wants (§13 explains why the tree is deferred, not on the
//! path to an inevitable rewrite).

use crate::arch::mmu;
use crate::sync::{IrqSafeMutex, rank};
use alloc::vec::Vec;

/// One recorded mapping of a page that came out of an untyped region.
#[derive(Clone, Copy)]
struct Mapping {
    phys: u64,
    /// The address-space root (`TTBR0` L0 table) that maps `phys`. Keyed by root, not thread, so a
    /// page can be unmapped from an address space without a scheduler lookup, and so the tests can
    /// drive it with bare `AddressSpace`s.
    root: u64,
    va: u64,
}

/// **The mapping database, lite.** A flat `Vec`: the count of shared pages is small, and every
/// operation here is a linear pass, which is fine at this scale and honest about it.
static MAPPINGS: IrqSafeMutex<Vec<Mapping>> = IrqSafeMutex::new(rank::MAPPINGS, Vec::new());

/// Record that the address space rooted at `root` mapped `phys` at `va`. Called from every path that
/// maps a region-derived page (`Untyped::MAP`, `Frame::MAP`), so revocation can find it later.
pub fn record_mapping(phys: u64, root: u64, va: u64) {
    MAPPINGS.lock().push(Mapping { phys, root, va });
}

/// Forget every mapping in the address space rooted at `root`. Called when that address space is torn
/// down (`AddressSpace::drop`): its page tables are about to be freed and reused, and a stale
/// `(root, va)` would send a later revoke to walk memory that now belongs to someone else.
pub fn forget_root(root: u64) {
    MAPPINGS.lock().retain(|m| m.root != root);
}

/// Take the recorded mappings of `phys` out of the database and unmap each from its address space.
/// The lock is held only long enough to lift the entries out (DECISIONS §9); the unmapping, which
/// flushes the TLB across cores, happens after it is released.
fn unmap_everywhere(phys: u64) {
    let victims: Vec<Mapping> = {
        let mut db = MAPPINGS.lock();
        let mut mine = Vec::new();
        db.retain(|m| {
            if m.phys == phys {
                mine.push(*m);
                false
            } else {
                true
            }
        });
        mine
    };
    for m in &victims {
        mmu::unmap_user_at(m.root, m.va);
    }
}

/// **Revoke a page from everyone.** Delete every `Frame` capability to `phys` from every cspace, then
/// unmap it from every address space. After this no capability names the page and no address space
/// maps it, so it is safe to return to the allocator. Caps go **first**, so a `Frame::MAP` that
/// starts after this cannot re-establish a mapping we would then miss. (The remaining window, an
/// in-flight map on another core between the cap delete and the unmap, is the SMP race §13 names; a
/// full mapping-database lock is seL4's answer and this milestone's deferral.)
pub fn revoke_frame(phys: u64) {
    crate::sched::delete_frame_caps(phys);
    unmap_everywhere(phys);
}

/// Revoke every mapped page in `[base, base + size)`. `untyped::destroy` calls this before returning
/// a region to the allocator, which is what turns the old "spend-only, never reused" invariant (the
/// reason dangling mappings were harmless) into the stronger "no live mapping survives" one that
/// makes reuse actually safe.
pub fn revoke_region(base: u64, size: u64) {
    let mut pages: Vec<u64> = {
        let db = MAPPINGS.lock();
        db.iter()
            .filter(|m| m.phys >= base && m.phys < base + size)
            .map(|m| m.phys)
            .collect()
    };
    pages.sort_unstable();
    pages.dedup();
    for phys in pages {
        revoke_frame(phys);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user::AddressSpace;
    use paging::Flags;

    /// **Revocation unmaps a shared page from every address space that held it.** Two address spaces
    /// map one physical page; after `revoke_frame` neither maps it. This is the property the whole
    /// reclamation story rests on: a page may be reused only once no holder still maps it.
    #[test_case]
    fn revoke_unmaps_a_shared_page_from_every_address_space() {
        let mut a = AddressSpace::new().expect("no space A");
        let mut b = AddressSpace::new().expect("no space B");
        let shared = crate::memory::alloc().expect("no frame").addr();
        let (va_a, va_b) = (0x40_0000u64, 0x80_0000u64);

        a.map_physical(va_a, shared, Flags::user_data())
            .expect("map A");
        b.map_physical(va_b, shared, Flags::user_rodata())
            .expect("map B");
        record_mapping(shared, a.root(), va_a);
        record_mapping(shared, b.root(), va_b);

        assert!(
            mmu::translate_at(a.root(), va_a).is_some(),
            "A does not map the page"
        );
        assert!(
            mmu::translate_at(b.root(), va_b).is_some(),
            "B does not map the page"
        );

        revoke_frame(shared);

        assert!(
            mmu::translate_at(a.root(), va_a).is_none(),
            "A still maps the revoked page"
        );
        assert!(
            mmu::translate_at(b.root(), va_b).is_none(),
            "B still maps the revoked page"
        );

        crate::memory::free(frames::Frame::from_addr(shared));
    }

    /// **Destroying an untyped region unmaps its pages, THEN reclaims them.** A page from the region
    /// is mapped into an address space; `untyped::destroy` must remove that mapping before the page
    /// returns to the allocator, or a later allocation hands out memory a live process still maps
    /// (the use-after-free the tripwire in untyped.rs warns of). Both halves are asserted: the
    /// mapping is gone, and the region's frames come back.
    #[test_case]
    fn destroy_unmaps_a_region_before_reclaiming_it() {
        let mut space = AddressSpace::new().expect("no space");
        let region = crate::untyped::create(4).expect("no region");
        let phys = crate::untyped::retype_page(region).expect("retype");
        let va = 0x40_0000u64;
        space
            .map_physical(va, phys, Flags::user_data())
            .expect("map");
        record_mapping(phys, space.root(), va);
        assert!(
            mmu::translate_at(space.root(), va).is_some(),
            "the page was not mapped"
        );

        let free_before = crate::memory::stats().unwrap().free();
        crate::untyped::destroy(region);
        let free_after = crate::memory::stats().unwrap().free();

        assert!(
            mmu::translate_at(space.root(), va).is_none(),
            "destroy reclaimed a page a live address space still maps: the tripwire's use-after-free",
        );
        assert_eq!(
            free_after,
            free_before + 4,
            "destroy did not return the region's 4 frames to the allocator",
        );
    }
}
