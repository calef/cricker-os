//! **The region table itself, and the claim that makes reclamation single-winner.**
//!
//! The arithmetic in the crate root ([`split_new_watermark`], [`destroy_outcome`]) is what one
//! caller decides. This module is what happens when there are two, which is a different question
//! and needs a different tool: Kani proves the arithmetic and does not model threads, so the
//! racing case was argued from lock discipline and gated by nothing until milestone 135.
//!
//! # The bug this exists to have caught
//!
//! `untyped::destroy` used to check the region under the `REGIONS` lock, **release** it, revoke
//! every mapping, free every page, and remove the table slot last. Two callers holding a name for
//! one region (an owner's `Untyped::DESTROY` and a supervisor's `endpoint::REAP`, both landing in
//! `sched::reclaim_region`) could each pass the refusal check inside that gap and each run the free
//! loop over the same pages. It surfaced once in 45 loaded runs on riscv64 as
//! `double free of frame 0x82a3e000`, and it needed two cores.
//!
//! Pull request #316 fixed it by removing the slot under the same hold that decided to destroy it.
//! This module is that fix made structural: [`RegionTable::claim_for_destroy`] takes `&mut self`
//! and does both, so **the gap is not expressible**. A caller cannot hold a decision without
//! holding the table, because the decision and the removal are one call.
//!
//! # What a claim is
//!
//! [`DestroyClaim`] is the exclusive right to reclaim one region's pages. It is minted only by
//! `claim_for_destroy`, it has no public constructor, and it is not `Copy`, so the kernel's free
//! loop runs against a value nothing else in the system can be holding. Its fields are the ones the
//! caller needs to do the I/O the table cannot do: revoke the mappings, and then either free the run
//! to the frame allocator (a root) or give it back to the parent's budget (a child, via
//! [`RegionTable::return_to_parent`]).
//!
//! # Page numbers, not addresses
//!
//! Everything here is in **page units**, including `base_page`, so the crate stays `FRAME_SIZE`-
//! and address-agnostic exactly as the crate root's doc comment claims. The kernel multiplies.
//!
//! # EXAMPLES
//!
//! The kernel's own shape, minus the I/O:
//!
//! ```
//! use regions::RegionTable;
//!
//! let mut table = RegionTable::<8>::new();
//!
//! // A root region over 16 pages starting at page 0x1000.
//! let root = table.insert_root(0x1000, 16).expect("a free slot");
//!
//! // Carve a child off it. The parent now has a live child and cannot be reclaimed.
//! let child = table.split(root, 4).expect("budget and a slot");
//! assert!(table.claim_for_destroy(root).is_none(), "a parent with a live child refuses");
//!
//! // Reclaim the child: it returns its pages to the parent, never to the allocator.
//! let claim = table.claim_for_destroy(child).expect("a childless, unpinned region");
//! assert!(!claim.is_root(), "a split child is not a root");
//! // ... the kernel revokes claim.base_page()..+claim.pages() here ...
//! table.return_to_parent(&claim);
//!
//! // And now the parent is reclaimable, with the child's pages back in its budget.
//! assert_eq!(table.usage(root), Some((0, 16)));
//! let claim = table.claim_for_destroy(root).expect("no children left");
//! assert!(claim.is_root(), "a created region frees to the allocator");
//! ```
//!
//! The claim is single-winner, which is the whole point:
//!
//! ```
//! use regions::RegionTable;
//!
//! let mut table = RegionTable::<4>::new();
//! let r = table.insert_root(0, 2).unwrap();
//! assert!(table.claim_for_destroy(r).is_some());
//! assert!(table.claim_for_destroy(r).is_none(), "the name stopped resolving at the first claim");
//! ```
//!
//! Name: `RegionTable`, `DestroyClaim`, `insert_root`, `claim_for_destroy` and `return_to_parent`
//! are **provisional**, minted 2026-08-18 by milestone 135's lane and not yet put to calef. Nouns
//! per the naming tenet; `RegionTable` inherits the crate's own unratified `regions`.

use crate::{DestroyOutcome, destroy_outcome, split_new_watermark};

/// A region's parent field naming no parent: this region came from the frame allocator rather than from
/// a [`split`](RegionTable::split), so it is a **root** and its pages go back to the allocator.
///
/// `u64::MAX` never resolves as a `slots` name (its slot bits exceed any table's capacity), so it is
/// a safe sentinel rather than a value that could collide with a real region.
pub const NO_PARENT: u64 = u64::MAX;

/// One untyped region: a run of physical pages, and how far into it we have retyped.
///
/// Private on purpose. Every field is a decision input to something in this module, and a caller
/// that could read them could reconstruct the pre-#316 check-then-release-then-remove shape by
/// hand; the whole value of the claim is that it cannot.
#[derive(Clone, Copy)]
struct Region {
    /// Physical page number of the first page. Page units, not bytes: see the module docs.
    base_page: u64,
    pages: u64,
    /// Pages handed out so far. A bump pointer, and the whole of the allocator.
    watermark: u64,
    /// **A kernel object lives in this region**: a page here was retyped into an endpoint, an
    /// address space or a TCB, so a claim is refused until `sched::reclaim_region` has torn the
    /// objects down and called [`unpin`](RegionTable::unpin).
    pinned: bool,
    /// The region this one was split out of, or [`NO_PARENT`].
    parent: u64,
    /// **How many live children were split off this region.** A count rather than a bool, so a
    /// parent becomes reclaimable again once its last child returns, which is what stops a split
    /// parent being committed for its whole lifetime.
    children: u32,
}

/// **The exclusive right to reclaim one region.** Minted only by
/// [`claim_for_destroy`](RegionTable::claim_for_destroy), which removes the region's table slot in
/// the same borrow, so at most one of these can ever exist for a given region.
///
/// Not `Copy` and not `Clone`, deliberately: duplicating a claim would be duplicating the right to
/// free a run of pages, which is the bug this whole module is a reaction to.
///
/// # The two doctests below are a gate (milestone 136)
///
/// Both properties above are enforced by the type system, and until milestone 136 **nothing
/// watched them**. `#[derive(Clone)]` is one word, making a field `pub` is four characters, and
/// either would hand the kernel back the ability to hold a right to free pages that the table no
/// longer knows it gave out. `script/lint` pins the *set* of public items in this module; these
/// pin the things a set cannot see, which is the visibility of the fields and the traits the
/// claim does not implement.
///
/// They carry explicit error codes because a bare `compile_fail` passes when the snippet fails to
/// compile for **any** reason, including a typo, which is how a compile-fail test rots into an
/// assertion nobody has watched fail (milestone 62 deleted two of those this week).
///
/// **A claim cannot be forged.** The fields are private, so a caller cannot read the table, decide
/// for itself, and then build its own right to free the run: that reassembled shape is exactly the
/// pre-#316 `untyped::destroy`, whose gap double-freed.
///
/// ```compile_fail,E0451
/// let claim = regions::DestroyClaim { base_page: 0, pages: 4, parent: u64::MAX, is_root: true };
/// let _ = claim.pages();
/// ```
///
/// **A claim cannot be duplicated.** Two claims for one region is two callers in the free loop,
/// which is the bug in its original units.
///
/// ```compile_fail,E0599
/// let mut table = regions::RegionTable::<2>::new();
/// let name = table.insert_root(0x1000, 4).unwrap();
/// let claim = table.claim_for_destroy(name).unwrap();
/// let second = claim.clone();
/// let _ = (claim.pages(), second.pages());
/// ```
///
/// And the same sequence without the offending line compiles and runs, which is what keeps the two
/// above honest: they fail on the one line that is supposed to be impossible, not on the setup.
///
/// ```
/// let mut table = regions::RegionTable::<2>::new();
/// let name = table.insert_root(0x1000, 4).unwrap();
/// let claim = table.claim_for_destroy(name).unwrap();
/// assert_eq!(claim.pages(), 4);
/// assert!(claim.is_root());
/// ```
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a claim that is dropped without being spent leaks the region's pages"]
pub struct DestroyClaim {
    base_page: u64,
    pages: u64,
    parent: u64,
    is_root: bool,
}

impl DestroyClaim {
    /// Physical page number of the first page in the claimed run.
    #[must_use]
    pub fn base_page(&self) -> u64 {
        self.base_page
    }

    /// How many pages the claimed run holds.
    #[must_use]
    pub fn pages(&self) -> u64 {
        self.pages
    }

    /// **Whether these pages belong to the frame allocator.** `true` for a region created from it,
    /// so the caller frees the run; `false` for a split child, whose pages return to its parent via
    /// [`return_to_parent`](RegionTable::return_to_parent) and **never** to the allocator. That
    /// asymmetry is the no-double-free crux the crate root's Kani proof states.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.is_root
    }
}

/// **The untyped regions**, a generational table (`crates/slots`) plus every decision taken over it.
///
/// Generational because reclamation reuses slots: removing a region bumps its slot's generation, so
/// every `Untyped` capability minted for it stops resolving and the slot is available to the next
/// create. That is also what makes [`claim_for_destroy`](Self::claim_for_destroy) single-winner: the
/// loser's name is dead by the time it looks.
///
/// `N` is the capacity, which bounds the regions live **at once** rather than over the kernel's
/// lifetime. The kernel picks 256; the loom harnesses pick 2 or 4, because a model checker's search
/// is exponential and nothing in the protocol depends on capacity.
pub struct RegionTable<const N: usize> {
    table: slots::Table<Region, N>,
}

impl<const N: usize> Default for RegionTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RegionTable<N> {
    /// An empty table.
    ///
    /// `const` so a kernel static can hold one behind its lock.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            table: slots::Table::new(),
        }
    }

    /// **Record a region carved straight out of the frame allocator**, returning its generational
    /// name. `None` when every slot is live, in which case the caller still owns the pages and must
    /// give them back rather than leak them.
    ///
    /// This is the table half of the kernel's `create`; the allocation itself is the kernel's,
    /// because a crate that can be model-checked cannot also own a physical allocator.
    pub fn insert_root(&mut self, base_page: u64, pages: u64) -> Option<u64> {
        self.table.insert_with(|_| Region {
            base_page,
            pages,
            watermark: 0,
            pinned: false,
            parent: NO_PARENT,
            children: 0,
        })
    }

    /// **Carve `pages` off `parent`'s unspent budget into a new child region**, returning the
    /// child's name. seL4's untyped-retype-into-untyped: the subdivision that lets a spawner give
    /// each child its own independently-reclaimable region.
    ///
    /// `None` if the parent's name is dead, its budget cannot cover the carve (the proved
    /// [`split_new_watermark`] decides), or the table is full. In the last case the run stays spent
    /// on the parent and the parent keeps its bumped child count, which is the honest record that a
    /// page of it is unaccounted; that is the bump-only rule, not an oversight.
    ///
    /// **One borrow, where the kernel used to take its lock twice.** Nothing could observe the
    /// half-carved parent before, because the intermediate state is legal, but a window that exists
    /// is a window someone eventually reasons about; this closes it for free.
    pub fn split(&mut self, parent: u64, pages: u64) -> Option<u64> {
        let base_page = {
            let r = self.table.get_mut(parent)?;
            let new_watermark = split_new_watermark(r.pages, r.watermark, pages)?;
            let base_page = r.base_page + r.watermark;
            r.watermark = new_watermark;
            r.children += 1;
            base_page
        };
        self.table.insert_with(|_| Region {
            base_page,
            pages,
            watermark: 0,
            pinned: false,
            parent,
            children: 0,
        })
    }

    /// Whether this region has live children, so a claim on it will be refused. `false` for a dead
    /// name.
    #[must_use]
    pub fn has_children(&self, name: u64) -> bool {
        self.table.get(name).is_some_and(|r| r.children > 0)
    }

    /// **Retype one page out of the region**, returning its physical page number. `None` when the
    /// region is exhausted or its name is dead: the *process* is out of budget, not the kernel.
    ///
    /// The caller zeroes the page. This cannot, and the split is the reason the kernel keeps the
    /// I/O: a crate loom can run has no direct map to write through.
    pub fn retype_page(&mut self, name: u64) -> Option<u64> {
        let r = self.table.get_mut(name)?;
        if r.watermark >= r.pages {
            return None;
        }
        let page = r.base_page + r.watermark;
        r.watermark += 1;
        Some(page)
    }

    /// **Retype one page for a kernel object, pinning the region in the same breath.** The pin and
    /// the carve happen in one borrow, so no claim can slip between them and reclaim a page that is
    /// about to hold an endpoint.
    ///
    /// `None` on an exhausted or dead region, and **nothing is pinned** in that case: a caller that
    /// got no page owes no unpin.
    pub fn retype_object_page(&mut self, name: u64) -> Option<u64> {
        let r = self.table.get_mut(name)?;
        if r.watermark >= r.pages {
            return None;
        }
        r.pinned = true;
        let page = r.base_page + r.watermark;
        r.watermark += 1;
        Some(page)
    }

    /// **Clear a region's object pin**, after its objects have been torn down. A no-op on a dead
    /// name.
    ///
    /// Deliberately separate from the claim, and the split is not cosmetic: tearing down a TCB needs
    /// the scheduler lock, and the claim must never take it, because reclamation is reachable from
    /// an address space's `Drop`.
    pub fn unpin(&mut self, name: u64) {
        if let Some(r) = self.table.get_mut(name) {
            r.pinned = false;
        }
    }

    /// How many pages the region has retyped, and how many it holds. `None` for a dead name.
    #[must_use]
    pub fn usage(&self, name: u64) -> Option<(u64, u64)> {
        self.table.get(name).map(|r| (r.watermark, r.pages))
    }

    /// This region's physical span as `(base_page, pages)`, or `None` for a dead name. Object
    /// revocation needs it to find which kernel objects live inside the region.
    #[must_use]
    pub fn bounds(&self, name: u64) -> Option<(u64, u64)> {
        self.table.get(name).map(|r| (r.base_page, r.pages))
    }

    /// **Decide whether this region may be reclaimed, and claim it in the same borrow.**
    ///
    /// `Some(claim)` transfers the exclusive right to reclaim the run; the name is dead the instant
    /// this returns, so a second caller gets `None` whatever the interleaving. `None` means either
    /// the name was already dead (somebody else won, or it never resolved) or the region refuses:
    /// a live object pins it, or a child still owns part of its run.
    ///
    /// **The single borrow is the mechanism, and it is rung one of CLAUDE.md's ladder.** The
    /// pre-#316 kernel decided under one hold of the region lock and removed the slot under a later
    /// one, and two callers inside that gap both freed the same pages. There is no way to write that
    /// against this signature, because there is no intermediate state to hold: the caller either has
    /// the table or has a claim, never a decision about a table it no longer holds.
    ///
    /// Removing the slot *before* the caller does its I/O is also right for everything else that
    /// resolves the name. A retype on a region whose pages are on their way back to the allocator
    /// now fails, where before it could hand out a page about to be freed.
    pub fn claim_for_destroy(&mut self, name: u64) -> Option<DestroyClaim> {
        let r = self.table.get(name)?;
        let is_root = r.parent == NO_PARENT;
        // The refuse/root/child decision is the Kani-proved arithmetic in the crate root. The LIFO
        // input is irrelevant to a refusal and to the root case, so a placeholder is honest here;
        // `return_to_parent` computes the real one against the parent's current watermark.
        if destroy_outcome(r.pinned, r.children, is_root, false, r.pages) == DestroyOutcome::Refused
        {
            return None;
        }
        let claim = DestroyClaim {
            base_page: r.base_page,
            pages: r.pages,
            parent: r.parent,
            is_root,
        };
        // The generation bump. Every `Untyped` capability for this region stops resolving here, and
        // that is what makes the claim single-winner rather than merely first.
        self.table.remove(name);
        Some(claim)
    }

    /// **Give a claimed child's pages back to its parent's budget**, after the caller has revoked
    /// every mapping in the run. A no-op for a root claim (whose pages go to the frame allocator
    /// instead) or when the parent's own name has since died.
    ///
    /// The LIFO test runs against the parent's *current* watermark, in the same borrow as the
    /// un-bump, so nothing can carve from the parent in between: a child sitting at the top of the
    /// parent's run gives its pages back and the run is re-splittable, and a child freed out of
    /// order leaves a hole until the parent itself is reclaimed. That is the LIFO half of seL4's
    /// return-to-parent, without the derivation tree the general case would need.
    ///
    /// **The child count drops here rather than at the claim**, and the ordering carries weight: it
    /// is what stops the parent being reclaimed while this child's pages are still in flight. A
    /// caller that decremented at claim time would open exactly the window this module exists to
    /// close, one level up the tree.
    pub fn return_to_parent(&mut self, claim: &DestroyClaim) {
        if claim.is_root {
            return;
        }
        let Some(p) = self.table.get_mut(claim.parent) else {
            return;
        };
        let is_lifo_top = claim.base_page + claim.pages == p.base_page + p.watermark;
        if let DestroyOutcome::ReturnToParent { unbump } =
            destroy_outcome(false, 0, false, is_lifo_top, claim.pages)
        {
            p.watermark -= unbump;
        }
        p.children = p.children.saturating_sub(1);
    }
}

// --- The ordinary host tests -------------------------------------------------------------------
//
// Single-threaded: what each operation means, and that the refusals are refusals. The
// *interleavings* are the loom module below, which is a different question and a different tool.
#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn a_claim_kills_the_name_so_a_second_caller_gets_nothing() {
        let mut t = RegionTable::<4>::new();
        let r = t.insert_root(0x100, 8).unwrap();
        assert!(t.claim_for_destroy(r).is_some());
        assert!(t.claim_for_destroy(r).is_none());
        assert_eq!(t.bounds(r), None, "the name is dead, not merely claimed");
    }

    #[test]
    fn a_pinned_region_refuses_and_stays_alive() {
        let mut t = RegionTable::<4>::new();
        let r = t.insert_root(0x100, 8).unwrap();
        assert_eq!(t.retype_object_page(r), Some(0x100));
        assert!(t.claim_for_destroy(r).is_none(), "an object pins it");
        assert_eq!(t.bounds(r), Some((0x100, 8)), "a refusal must not remove");
        t.unpin(r);
        assert!(t.claim_for_destroy(r).is_some());
    }

    #[test]
    fn a_parent_refuses_until_its_last_child_returns() {
        let mut t = RegionTable::<4>::new();
        let root = t.insert_root(0, 16).unwrap();
        let a = t.split(root, 4).unwrap();
        let b = t.split(root, 4).unwrap();
        assert!(t.claim_for_destroy(root).is_none());

        // `b` sits at the top, so it un-bumps; `a` does not, so it leaves a hole.
        let cb = t.claim_for_destroy(b).unwrap();
        t.return_to_parent(&cb);
        assert_eq!(t.usage(root), Some((4, 16)));
        assert!(t.claim_for_destroy(root).is_none(), "`a` is still live");

        let ca = t.claim_for_destroy(a).unwrap();
        t.return_to_parent(&ca);
        assert_eq!(
            t.usage(root),
            Some((0, 16)),
            "the last return closes the hole"
        );
        assert!(t.claim_for_destroy(root).is_some());
    }

    #[test]
    fn a_child_freed_out_of_order_leaves_a_hole() {
        let mut t = RegionTable::<4>::new();
        let root = t.insert_root(0, 16).unwrap();
        let a = t.split(root, 4).unwrap();
        let _b = t.split(root, 4).unwrap();
        let ca = t.claim_for_destroy(a).unwrap();
        assert!(!ca.is_root());
        t.return_to_parent(&ca);
        assert_eq!(
            t.usage(root),
            Some((8, 16)),
            "an out-of-order return un-bumps nothing"
        );
    }

    #[test]
    fn a_root_claim_frees_to_the_allocator_and_a_child_never_does() {
        let mut t = RegionTable::<4>::new();
        let root = t.insert_root(0x200, 8).unwrap();
        let child = t.split(root, 2).unwrap();
        let cc = t.claim_for_destroy(child).unwrap();
        assert!(!cc.is_root(), "a split child is never a root");
        assert_eq!((cc.base_page(), cc.pages()), (0x200, 2));
        t.return_to_parent(&cc);
        let cr = t.claim_for_destroy(root).unwrap();
        assert!(cr.is_root());
        assert_eq!((cr.base_page(), cr.pages()), (0x200, 8));
    }

    #[test]
    fn retype_spends_the_budget_and_then_refuses() {
        let mut t = RegionTable::<4>::new();
        let r = t.insert_root(7, 2).unwrap();
        assert_eq!(t.retype_page(r), Some(7));
        assert_eq!(t.retype_page(r), Some(8));
        assert_eq!(t.retype_page(r), None, "exhausted, not an error");
        assert_eq!(t.usage(r), Some((2, 2)));
    }

    #[test]
    fn an_exhausted_region_pins_nothing() {
        let mut t = RegionTable::<4>::new();
        let r = t.insert_root(0, 1).unwrap();
        assert_eq!(t.retype_page(r), Some(0));
        assert_eq!(t.retype_object_page(r), None);
        assert!(
            t.claim_for_destroy(r).is_some(),
            "a failed object retype must not leave the region pinned"
        );
    }

    #[test]
    fn a_full_table_refuses_rather_than_overwriting() {
        let mut t = RegionTable::<2>::new();
        assert!(t.insert_root(0, 1).is_some());
        assert!(t.insert_root(1, 1).is_some());
        assert!(t.insert_root(2, 1).is_none(), "no slot left");
    }

    #[test]
    fn a_dead_name_is_inert_everywhere() {
        let mut t = RegionTable::<2>::new();
        let r = t.insert_root(0, 4).unwrap();
        let claim = t.claim_for_destroy(r).unwrap();
        assert!(claim.is_root());
        assert_eq!(t.retype_page(r), None);
        assert_eq!(t.retype_object_page(r), None);
        assert_eq!(t.split(r, 1), None);
        assert!(!t.has_children(r));
        assert_eq!(t.usage(r), None);
        t.unpin(r); // must not panic
    }
}

// --- The loom model ----------------------------------------------------------------------------
#[cfg(all(test, loom))]
mod interleavings {
    //! **Every interleaving of the region claim, and every reordering C11 permits within it.**
    //!
    //! Run with `script/interleaving-check`. See notes/interleaving.md for the method and
    //! notes/object-revocation.md for the bug that made this the fourth retrofit of milestone 80's
    //! method rather than the first.
    //!
    //! What loom searches here is the interleaving of **critical sections**, not memory orderings:
    //! this protocol is lock-based, like `wake_handshake` and unlike `steal_request`. That makes it
    //! cheap to search and it makes the model's claim narrower, which the roadmap block says out
    //! loud: the model assumes mutual exclusion and says nothing about whether `IrqSafeMutex`
    //! delivers it.
    //!
    //! The harnesses use `RegionTable<2>` or `<4>` rather than the kernel's 256 because the search
    //! is exponential in the interleavings and nothing in the protocol depends on capacity. Every
    //! other input is the kernel's own: the same `claim_for_destroy`, the same `split`, the same
    //! `retype_page`, compiled from the same source the kernel links.

    use loom::sync::{Arc, Mutex};
    use loom::thread;

    use super::*;

    /// **Non-vacuity, which a model checker needs for exactly the reason a solver does.**
    ///
    /// A harness whose interesting branch is never reached passes without checking anything, and
    /// reports success while doing it. Kani's answer is `kani::cover!`; loom has no equivalent, so
    /// these count reachability across the model's executions and the harness asserts on the count
    /// afterwards. The flag is a *real* atomic, deliberately: it lives outside the model and
    /// accumulates across every execution loom runs, where a loom atomic would be reset by each.
    ///
    /// Copied from `crates/canary_gate`, which is where this shape was first written. See
    /// notes/verification.md's non-vacuity section for why it is not optional.
    struct Reached(core::sync::atomic::AtomicBool);

    impl Reached {
        const fn new() -> Self {
            Self(core::sync::atomic::AtomicBool::new(false))
        }
        fn mark(&self) {
            self.0.store(true, core::sync::atomic::Ordering::Relaxed);
        }
        fn assert(&self, what: &str) {
            assert!(
                self.0.load(core::sync::atomic::Ordering::Relaxed),
                "vacuous harness: no execution ever reached {what}, so nothing was checked"
            );
        }
    }

    /// **The property the whole reclamation path rests on: two callers, one winner.**
    ///
    /// The kernel reaches `destroy` for one region by two routes that can run concurrently on two
    /// cores, an owner's `Untyped::DESTROY` and a supervisor's `endpoint::REAP`. Each thread here
    /// takes the lock, claims, and (if it won) runs the free loop, counting the run. Loom fails the
    /// model on any execution where the count reaches two.
    ///
    /// The two `Reached` flags are what make it non-vacuous: they assert that loom really explored
    /// both winners, rather than running one order twice.
    #[test]
    fn two_destroyers_race_and_exactly_one_reclaims() {
        static FIRST_WON: Reached = Reached::new();
        static SECOND_WON: Reached = Reached::new();

        loom::model(|| {
            let table = Arc::new(Mutex::new(RegionTable::<2>::new()));
            let name = table.lock().unwrap().insert_root(0x1000, 4).unwrap();
            // Stands in for the frame allocator: how many times the free loop ran over this run,
            // and which thread ran it. Two is the double free, in the units the bug report used.
            let freed = Arc::new(Mutex::new(Vec::<usize>::new()));

            let handles: Vec<_> = (0..2usize)
                .map(|who| {
                    let table = Arc::clone(&table);
                    let freed = Arc::clone(&freed);
                    thread::spawn(move || {
                        let claim = table.lock().unwrap().claim_for_destroy(name);
                        if let Some(claim) = claim {
                            assert!(claim.is_root(), "a created region frees to the allocator");
                            assert_eq!((claim.base_page(), claim.pages()), (0x1000, 4));
                            // The kernel revokes and frees HERE, with the lock released. That
                            // release is what the pre-#316 protocol could not survive.
                            freed.lock().unwrap().push(who);
                        }
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }

            let freed = freed.lock().unwrap();
            assert_eq!(
                freed.len(),
                1,
                "exactly one caller may free a region's pages, and this execution had {}",
                freed.len()
            );
            match freed[0] {
                0 => FIRST_WON.mark(),
                _ => SECOND_WON.mark(),
            }
        });

        FIRST_WON.assert("an execution in which the first destroyer won");
        SECOND_WON.assert("an execution in which the second destroyer won");
    }

    /// **The falsification witness: the pre-#316 protocol, and loom finding its double free.**
    ///
    /// A model that cannot fail proves nothing, and this tree's standard is that an assertion nobody
    /// has watched fail is not a gate (milestone 62). So the broken protocol lives here permanently,
    /// written against this module's private internals in exactly the shape `untyped::destroy` had
    /// before the fix: check under the lock, **release it**, revoke, free every page, and remove the
    /// slot last.
    ///
    /// The assertion is inverted on purpose. This harness passes only when loom **finds** an
    /// execution in which both callers free the same run, so it is the standing evidence that the
    /// harness above is searching something real rather than agreeing with itself.
    #[test]
    fn the_pre_fix_protocol_double_frees_and_this_model_finds_it() {
        static DOUBLE_FREE: Reached = Reached::new();

        /// `untyped::destroy` as it stood before pull request #316. Kept in the test module rather
        /// than in the crate's API so that nothing outside a loom run can call it.
        fn destroy_with_the_gap(table: &Mutex<RegionTable<2>>, name: u64, freed: &Mutex<usize>) {
            // 1. Decide under the lock.
            let captured = {
                let guard = table.lock().unwrap();
                let Some(r) = guard.table.get(name) else {
                    return;
                };
                let is_root = r.parent == NO_PARENT;
                if destroy_outcome(r.pinned, r.children, is_root, false, r.pages)
                    == DestroyOutcome::Refused
                {
                    return;
                }
                (r.base_page, r.pages)
            };
            // 2. Release the lock and revoke every mapping in the run. This is the gap.
            thread::yield_now();
            // 3. Free every page, whether or not anyone else already did.
            let _ = captured;
            *freed.lock().unwrap() += 1;
            // 4. And only now remove the slot, far too late to have decided anything.
            table.lock().unwrap().table.remove(name);
        }

        loom::model(|| {
            let table = Arc::new(Mutex::new(RegionTable::<2>::new()));
            let name = table.lock().unwrap().insert_root(0x1000, 4).unwrap();
            let freed = Arc::new(Mutex::new(0usize));

            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let table = Arc::clone(&table);
                    let freed = Arc::clone(&freed);
                    thread::spawn(move || destroy_with_the_gap(&table, name, &freed))
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }

            let freed = *freed.lock().unwrap();
            assert!(freed >= 1, "somebody must reclaim the region");
            if freed == 2 {
                DOUBLE_FREE.mark();
            }
        });

        DOUBLE_FREE.assert(
            "an execution in which both callers freed the same run, \
             which is the bug pull request #316 fixed and this model must be able to see",
        );
    }

    /// **A retype never hands out a page the reclaim is about to free.**
    ///
    /// The other half of why the slot comes out first. Before the fix, the removal happened after
    /// the free loop, so a `retype_page` landing in the gap resolved the name and returned a page
    /// that was already on its way back to the allocator. Claiming and removing in one borrow makes
    /// the two outcomes exclusive: whichever wins, the loser gets `None`.
    #[test]
    fn a_retype_never_hands_out_a_page_the_reclaim_is_about_to_free() {
        static RETYPE_WON: Reached = Reached::new();
        static CLAIM_WON: Reached = Reached::new();

        loom::model(|| {
            let table = Arc::new(Mutex::new(RegionTable::<2>::new()));
            let name = table.lock().unwrap().insert_root(0x2000, 4).unwrap();
            let outcome = Arc::new(Mutex::new((None::<u64>, false)));

            let retyper = {
                let table = Arc::clone(&table);
                let outcome = Arc::clone(&outcome);
                thread::spawn(move || {
                    let page = table.lock().unwrap().retype_page(name);
                    outcome.lock().unwrap().0 = page;
                })
            };
            let destroyer = {
                let table = Arc::clone(&table);
                let outcome = Arc::clone(&outcome);
                thread::spawn(move || {
                    let claim = table.lock().unwrap().claim_for_destroy(name);
                    if let Some(claim) = claim {
                        assert_eq!(claim.pages(), 4);
                        outcome.lock().unwrap().1 = true;
                    }
                })
            };
            retyper.join().unwrap();
            destroyer.join().unwrap();

            let (page, claimed) = *outcome.lock().unwrap();
            // The claim is unconditional here (nothing pins the region and it has no children), so
            // the only question is which side of the removal the retype landed on.
            assert!(claimed, "an unpinned childless region must be reclaimable");
            match page {
                Some(p) => {
                    assert_eq!(p, 0x2000, "the retype ran first, on a live region");
                    RETYPE_WON.mark();
                }
                None => CLAIM_WON.mark(),
            }
        });

        RETYPE_WON.assert("an execution in which the retype ran before the claim");
        CLAIM_WON.assert("an execution in which the claim killed the name first");
    }

    /// **A split never carves from a region another caller has claimed.**
    ///
    /// The same exclusion one level along, and it matters more than the retype case because a child
    /// that outlived its parent's reclamation would hold a name for pages already back in the
    /// allocator. Exactly one of the two can succeed.
    #[test]
    fn a_split_and_a_claim_on_one_parent_cannot_both_succeed() {
        static SPLIT_WON: Reached = Reached::new();
        static CLAIM_WON: Reached = Reached::new();

        loom::model(|| {
            let table = Arc::new(Mutex::new(RegionTable::<4>::new()));
            let parent = table.lock().unwrap().insert_root(0x3000, 8).unwrap();
            let outcome = Arc::new(Mutex::new((None::<u64>, false)));

            let splitter = {
                let table = Arc::clone(&table);
                let outcome = Arc::clone(&outcome);
                thread::spawn(move || {
                    let child = table.lock().unwrap().split(parent, 2);
                    outcome.lock().unwrap().0 = child;
                })
            };
            let destroyer = {
                let table = Arc::clone(&table);
                let outcome = Arc::clone(&outcome);
                thread::spawn(move || {
                    let claim = table.lock().unwrap().claim_for_destroy(parent);
                    if let Some(claim) = claim {
                        assert!(claim.is_root());
                        outcome.lock().unwrap().1 = true;
                    }
                })
            };
            splitter.join().unwrap();
            destroyer.join().unwrap();

            let (child, claimed) = *outcome.lock().unwrap();
            assert!(
                !(child.is_some() && claimed),
                "a claimed parent must not also have been carved: the child would name freed pages"
            );
            if child.is_some() {
                SPLIT_WON.mark();
            }
            if claimed {
                CLAIM_WON.mark();
            }
        });

        SPLIT_WON.assert("an execution in which the split ran first and the claim was refused");
        CLAIM_WON.assert("an execution in which the claim ran first and the split found no parent");
    }

    /// **A parent is never reclaimed while a child's pages are still in flight.**
    ///
    /// The child's slot comes out at its claim, but the parent's child count drops only in
    /// `return_to_parent`, after the caller has revoked the run. Those two facts together are what
    /// keep the parent refusing across the whole window in which the child's pages are neither the
    /// child's nor yet the parent's.
    ///
    /// The flag lives under the same lock as the table on purpose. A separate lock would let the
    /// parent's claim land between the return and the flag's store, and the harness would be
    /// checking its own bookkeeping rather than the protocol.
    #[test]
    fn a_parent_is_never_reclaimed_while_its_child_is_returning() {
        static PARENT_LOST: Reached = Reached::new();
        static PARENT_WON: Reached = Reached::new();

        loom::model(|| {
            // `.1` is "the child has finished returning its pages", written under the table's lock.
            let shared = Arc::new(Mutex::new((RegionTable::<4>::new(), false)));
            let (parent, child) = {
                let mut guard = shared.lock().unwrap();
                let p = guard.0.insert_root(0x4000, 8).unwrap();
                let c = guard.0.split(p, 4).unwrap();
                (p, c)
            };

            let reaper = {
                let shared = Arc::clone(&shared);
                thread::spawn(move || {
                    let claim = shared.lock().unwrap().0.claim_for_destroy(child).unwrap();
                    // The kernel revokes the child's run here, lock released.
                    thread::yield_now();
                    let mut guard = shared.lock().unwrap();
                    guard.0.return_to_parent(&claim);
                    guard.1 = true;
                })
            };
            let owner = {
                let shared = Arc::clone(&shared);
                thread::spawn(move || {
                    let mut guard = shared.lock().unwrap();
                    if guard.0.claim_for_destroy(parent).is_some() {
                        assert!(
                            guard.1,
                            "the parent was reclaimed while its child's pages were in flight"
                        );
                        true
                    } else {
                        false
                    }
                })
            };
            reaper.join().unwrap();
            if owner.join().unwrap() {
                PARENT_WON.mark();
            } else {
                PARENT_LOST.mark();
            }
        });

        PARENT_LOST.assert("an execution in which the parent's claim was refused");
        PARENT_WON.assert("an execution in which the parent's claim ran after the child returned");
    }
}
