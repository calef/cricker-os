//! **The untyped-region accounting, as pure logic** (object revocation, DECISIONS §16).
//!
//! The kernel's untyped allocator (`kernel/src/untyped.rs`) is a bump watermark per region, with a
//! parent/child tree from `SPLIT` and a LIFO return-of-pages on `DESTROY`. The scary property is
//! **no double-free**: a physical page must reach the frame allocator at most once across any
//! sequence of create/split/destroy. That safety rests on two arithmetic decisions, and this crate
//! is those decisions, extracted so Kani can prove them and the kernel then *calls* the proved code
//! rather than keeping a parallel copy (the discipline in notes/verification.md).
//!
//! The two decisions:
//!
//! - [`split_new_watermark`]: carving a child off a parent's budget. It advances the parent's
//!   watermark, and the property is that a successful carve stays within budget, does not overflow,
//!   and strictly progresses, so consecutive carves are disjoint runs within the parent.
//! - [`destroy_outcome`]: reclaiming a region. The property is that a region with a live object
//!   (`pinned`) or a live child refuses; a **root** frees its run to the allocator; and a **child**
//!   *never* frees to the allocator, its pages return to the parent (LIFO un-bump, or a hole). "A
//!   child never frees to global" is the no-double-free crux: a page reaches the allocator only when
//!   the root that owns it is destroyed, and each root owns one contiguous run from one allocation.
//!
//! Everything here works in **page units** (a watermark and page counts), so it is `FRAME_SIZE`- and
//! address-agnostic; the kernel supplies the base addresses and does the byte arithmetic and the I/O.
//!
//! # Examples
//!
//! ```
//! use regions::split_new_watermark;
//!
//! // A 100-page parent that has spent 40: carving 10 more moves the watermark to 50.
//! assert_eq!(split_new_watermark(100, 40, 10), Some(50));
//!
//! // Exactly exhausting the budget is fine.
//! assert_eq!(split_new_watermark(100, 90, 10), Some(100));
//!
//! // One page beyond it is not. A child can never be bigger than what is left.
//! assert_eq!(split_new_watermark(100, 90, 11), None);
//! ```
//!
//! The two refusals that are not about the budget at all, and are the reason this is a function
//! rather than a subtraction written at the call site:
//!
//! ```
//! use regions::split_new_watermark;
//!
//! // An empty carve would mint a zero-page region, which is not a thing.
//! assert_eq!(split_new_watermark(100, 40, 0), None);
//!
//! // And a wrap would forge budget out of arithmetic.
//! assert_eq!(split_new_watermark(100, 40, u64::MAX), None);
//! ```
//!
//! Name: unrecorded, and flagged. The naming tenet names it among the crate names that are generic
//! words which could label almost anything in an operating system (`compose`, `measure`, `regions`,
//! `slots`, `caps`, `frames`); three of those six were settled on 2026-08-01 and this one was not.
//! Nothing records who chose it.

#![cfg_attr(not(test), no_std)]

mod table;

pub use table::{DestroyClaim, NO_PARENT, RegionTable};

/// The result of carving `want` pages off a parent whose budget is `parent_pages` and whose spent
/// watermark is `parent_watermark`. `Some(new_watermark)` on success (the caller carves the run
/// `[parent_watermark, new_watermark)` and sets the parent's watermark to `new_watermark`); `None`
/// if the carve is empty, would overflow, or exceeds the parent's remaining budget.
pub fn split_new_watermark(parent_pages: u64, parent_watermark: u64, want: u64) -> Option<u64> {
    if want == 0 {
        return None; // an empty carve makes no child; refuse rather than mint a zero-page region
    }
    let new = parent_watermark.checked_add(want)?; // no overflow: a wrap would forge budget
    if new > parent_pages {
        return None; // beyond the parent's remaining budget
    }
    Some(new)
}

/// What [`destroy`](destroy_outcome) decides for a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestroyOutcome {
    /// A live object pins the region, or it still has children: reclaiming would dangle or double-free.
    Refused,
    /// A **root** region (created from the frame allocator): free its whole run back to the allocator.
    FreeToGlobal,
    /// A **child** region: return its pages to the parent, never to the allocator. `unbump` is how
    /// many pages to give back to the parent's watermark: the child's page count when it sits at the
    /// top (LIFO, the run is re-splittable), or `0` when it does not (a hole until the parent dies).
    ReturnToParent {
        /// How many pages to give back to the parent's watermark.
        unbump: u64,
    },
}

/// Decide what destroying a region does. `pinned` and `children` gate it; `is_root` distinguishes a
/// created region from a split child; `is_lifo_top` says whether a child sits at the top of the
/// parent's watermark; `child_pages` is the child's size (used only for the LIFO un-bump).
pub fn destroy_outcome(
    pinned: bool,
    children: u32,
    is_root: bool,
    is_lifo_top: bool,
    child_pages: u64,
) -> DestroyOutcome {
    if pinned || children > 0 {
        return DestroyOutcome::Refused;
    }
    if is_root {
        return DestroyOutcome::FreeToGlobal;
    }
    DestroyOutcome::ReturnToParent {
        unbump: if is_lifo_top { child_pages } else { 0 },
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// A successful carve stays within budget, does not overflow, and strictly progresses (so the
    /// run it hands out, `[parent_watermark, new)`, is fresh and disjoint from every earlier carve).
    /// A refused carve is exactly: empty, overflowing, or beyond budget.
    #[kani::proof]
    fn split_stays_within_budget_and_progresses() {
        let parent_pages: u64 = kani::any();
        let parent_watermark: u64 = kani::any();
        let want: u64 = kani::any();
        // The table's own invariant: a region's watermark never exceeds its page count.
        kani::assume(parent_watermark <= parent_pages);

        match split_new_watermark(parent_pages, parent_watermark, want) {
            Some(new) => {
                assert!(want > 0, "a zero carve must be refused");
                assert!(
                    new == parent_watermark + want,
                    "watermark advances by exactly want"
                );
                assert!(
                    new <= parent_pages,
                    "the carve stays within the parent's budget"
                );
                assert!(
                    new > parent_watermark,
                    "the carve strictly progresses, so it is fresh"
                );
            }
            None => {
                // Refused: it must be one of the three legitimate reasons.
                assert!(
                    want == 0
                        || parent_watermark.checked_add(want).is_none()
                        || parent_watermark + want > parent_pages
                );
            }
        }
    }

    /// **The no-double-free crux:** a region with a live object or a live child refuses; a root frees
    /// to the allocator; and a child NEVER frees to the allocator (its pages return to the parent).
    /// So a page reaches the allocator only through the root that owns it, exactly once.
    #[kani::proof]
    fn destroy_never_frees_a_child_to_the_allocator() {
        let pinned: bool = kani::any();
        let children: u32 = kani::any();
        let is_root: bool = kani::any();
        let is_lifo_top: bool = kani::any();
        let child_pages: u64 = kani::any();

        let outcome = destroy_outcome(pinned, children, is_root, is_lifo_top, child_pages);

        if pinned || children > 0 {
            assert!(
                outcome == DestroyOutcome::Refused,
                "a pinned or parent region must refuse"
            );
        } else if is_root {
            assert!(
                outcome == DestroyOutcome::FreeToGlobal,
                "a root frees to the allocator"
            );
        } else {
            // A child returns to its parent, never to the allocator: this is what prevents the
            // double-free when the parent's run is later freed.
            match outcome {
                DestroyOutcome::ReturnToParent { unbump } => {
                    assert!(unbump == if is_lifo_top { child_pages } else { 0 });
                }
                _ => panic!("a child must not free to the allocator"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_refuses_zero_overflow_and_over_budget() {
        assert_eq!(split_new_watermark(8, 0, 0), None); // zero carve
        assert_eq!(split_new_watermark(8, 4, 5), None); // 4+5 > 8, over budget
        assert_eq!(split_new_watermark(8, u64::MAX, 1), None); // overflow
        assert_eq!(split_new_watermark(8, 4, 4), Some(8)); // exact fit
        assert_eq!(split_new_watermark(8, 0, 4), Some(4)); // first carve
    }

    #[test]
    fn destroy_decides_by_ownership() {
        assert_eq!(
            destroy_outcome(true, 0, true, false, 0),
            DestroyOutcome::Refused
        );
        assert_eq!(
            destroy_outcome(false, 1, true, false, 0),
            DestroyOutcome::Refused
        );
        assert_eq!(
            destroy_outcome(false, 0, true, false, 0),
            DestroyOutcome::FreeToGlobal
        );
        assert_eq!(
            destroy_outcome(false, 0, false, true, 4),
            DestroyOutcome::ReturnToParent { unbump: 4 }
        );
        assert_eq!(
            destroy_outcome(false, 0, false, false, 4),
            DestroyOutcome::ReturnToParent { unbump: 0 }
        );
    }
}
