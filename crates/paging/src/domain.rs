//! **The portable DMA-domain seam** (milestone 16b; DECISIONS §20).
//!
//! An IOMMU confines a device by translating every address it emits through page tables the kernel
//! programs, in the CPU's own page-table format: SMMUv3 walks VMSAv8-64 tables (the aarch64 format,
//! [`Aarch64`](crate::Aarch64)); the RISC-V IOMMU walks Sv39 ([`Sv39`](crate::Sv39)). Those are the
//! *same two formats* [`Mapper`] already builds for process address spaces, so a DMA domain is not a
//! new kind of table. It is an [`Mapper`] filled with an identity map over exactly the frames the
//! device is allowed to reach, and nothing else.
//!
//! # Why identity, and why the low half
//!
//! The userspace virtio driver already puts *physical* addresses into its descriptors (its DMA
//! region's frames, and the kernel's shadow page). With an IOMMU in front, the device emits those
//! same numbers as **IOVAs**, and the domain translates each IOVA to the identical PA. An address
//! the driver was never granted has no mapping, so the IOMMU faults instead of letting the DMA
//! through. That is the whole confinement, now in hardware: the domain is an allow-list of frames,
//! expressed as a page table. The driver's ABI does not change, because IOVA == PA means the
//! addresses it computes still name the right memory. (Both `virt` boards place RAM in the low half:
//! aarch64 at `0x4000_0000`, riscv at `0x8000_0000`, both far below either format's [`Half`] split.)
//!
//! # The RISC-V U-bit, stated once
//!
//! A single-stage (`iosatp`, no process context) RISC-V IOMMU translation faults on a leaf PTE
//! whose U bit is clear, because the device is not "requesting supervisor privilege". So the domain
//! is built with [`Flags::user_data`], which sets U on Sv39 and read/write on both formats. It is a
//! device's data window, so user-accessible read/write with no execute is exactly right.

use crate::{Flags, Half, MapError, Mapper, PAGE_SIZE, PageFormat, PageTable};

/// A physical region a device is allowed to touch: `[base, base + size)`, 4 KiB aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaRegion {
    /// Physical address of the first byte.
    pub base: u64,
    /// Length in bytes.
    pub size: u64,
}

/// **How many whole 4 KiB pages a grant contains, or why it is not a grant a page-granular domain
/// can express.** The domain maps pages, so the grant it can express *exactly* is the set of whole
/// pages inside `[base, base + size)`. This is the decision half of [`build_identity_domain`], split
/// out as loopless arithmetic so Kani can quantify it over every region (see
/// `an_enumerated_page_lies_inside_the_grant` and `every_whole_page_of_the_grant_is_enumerated`).
///
/// Refused, rather than approximated:
///
/// - **an unaligned `base`**, because a page-granular domain cannot start mid-page. Rounding down
///   would map the bytes below `base` (an over-map: memory the device was never granted becomes
///   reachable), and rounding up would silently shift the whole grant. [`Mapper::map`] would have
///   refused the first page with [`MapError::Misaligned`] anyway; deciding it here makes the refusal
///   provable instead of incidental.
/// - **a region whose `base + size` wraps `u64`**, which is not a region at all: it has no end, so
///   "inside the grant" is not even a statement that can be made about it, let alone proved. That is
///   literally how the proof found this: `an_enumerated_page_lies_inside_the_grant` cannot compute the
///   grant's limit without the check, and fails on it. The old builder had no check, handed `map_range`
///   a page count derived from such a size, and relied on `map_range`'s unchecked `va + i * PAGE_SIZE`.
///
///   **Stated honestly, because the first draft of this comment overstated it:** this is a proof
///   obligation closed, not a reachable bug fixed. Reaching a wrapped address needs a low-half
///   page-aligned base *and* a `size` within a page or so of `2^64`, and the builder would run the
///   frame allocator dry ([`MapError::OutOfFrames`]) after a few thousand pages, tens of orders of
///   magnitude before the index at which the multiply wraps. No caller can construct such a region
///   anyway; every grant is frame-allocator memory bounded by RAM. What the check buys is that the
///   confinement property is now *total*: it holds for every `DmaRegion` value, so nobody has to
///   re-derive that argument when a new caller appears.
///
/// A `size` that is not a whole number of pages is *not* refused: the trailing partial page is simply
/// not mapped. That is the safe direction (the device reaches less than it was granted, never more)
/// and it is what the old `size / PAGE_SIZE` already did.
pub fn grant_pages(r: DmaRegion) -> Result<u64, MapError> {
    if !r.base.is_multiple_of(PAGE_SIZE) {
        return Err(MapError::Misaligned);
    }
    if r.base.checked_add(r.size).is_none() {
        return Err(MapError::BadRegion);
    }
    Ok(r.size / PAGE_SIZE)
}

/// **The IOVA of the `i`th whole page of a grant, which is also its PA (the domain is identity).**
/// `None` if `i` does not name a whole page of this region, so a caller cannot walk off the end by
/// arithmetic: every add and multiply is checked, which is what makes the enumeration total for any
/// region and any index.
///
/// Which of the two guards here is load-bearing is worth being exact about, because trying to falsify
/// the soundness proof revealed it. **Soundness rests on [`grant_pages`] flooring**, not on the
/// partial-page test below: for `i` under the page count the test can never fire, and weakening it to
/// `off >= r.size` leaves the soundness harness verifying happily (checked, not assumed). Rounding the
/// *count* up is what breaks it. So the test below is defence in depth for a caller who passes an `i`
/// the builder never would, and the checked arithmetic is what makes the function total; the
/// confinement itself comes from the count.
pub fn grant_page(r: DmaRegion, i: u64) -> Option<u64> {
    let off = i.checked_mul(PAGE_SIZE)?;
    // The whole page must fit: a trailing partial page is not one of the grant's pages.
    if off.checked_add(PAGE_SIZE)? > r.size {
        return None;
    }
    r.base.checked_add(off)
}

/// **Build a device's DMA domain: an identity map over `regions` and nothing else.**
///
/// `root` is a zeroed, page-aligned frame that becomes the domain's top-level table (the value the
/// IOMMU's STE/context descriptor will point at). `alloc_frame` supplies zeroed intermediate tables;
/// `phys_to_ptr` turns a physical table address into a pointer the same way [`Mapper`] requires. The
/// generic `F` selects the format, so one call site builds a VMSAv8-64 domain on aarch64 and an Sv39
/// domain on riscv from the same code, which is the seam's entire point.
///
/// Every page is mapped [`Flags::user_data`] (read/write, no execute, U-bit set): a device's data
/// window. Each region is mapped IOVA == PA, so a device emitting an in-region physical address is
/// translated to itself and an out-of-region address faults.
///
/// # Safety
/// `root` and every frame `alloc_frame` returns must be zeroed and page-aligned, and `phys_to_ptr`
/// must satisfy [`Mapper`]'s contract (the tables are reachable through it). The caller owns `root`
/// and must not install it anywhere until this returns `Ok`.
pub unsafe fn build_identity_domain<A, P, F>(
    root: u64,
    alloc_frame: A,
    phys_to_ptr: P,
    regions: &[DmaRegion],
) -> Result<(), MapError>
where
    A: FnMut() -> Option<u64>,
    P: Fn(u64) -> *mut PageTable,
    F: PageFormat,
{
    // A domain translates IOVAs, which we place equal to physical addresses in the low half. The
    // Mapper's WrongHalf guard then catches a region that strays into the high half rather than
    // silently building a mapping the IOMMU would never consult.
    // SAFETY: forwarded from this function's contract.
    let mut m = unsafe { Mapper::<A, P, F>::new(root, Half::Low, alloc_frame, phys_to_ptr) };
    for r in regions {
        // The page set is decided by [`grant_pages`]/[`grant_page`], the proved arithmetic, rather
        // than by `map_range`'s unchecked `va + i * PAGE_SIZE`. That is what makes "the domain maps
        // exactly the whole pages of the grant" a proved property of the code that runs: no page
        // outside the grant is enumerated, and no whole page inside it is skipped.
        let pages = grant_pages(*r)?;
        for i in 0..pages {
            // Unreachable for `i < pages`, and *proved* unreachable by
            // `a_page_index_below_the_count_always_resolves`; the branch stays because the compiler
            // cannot see that and a silent `continue` would be a hole rather than an error.
            let page = grant_page(*r, i).ok_or(MapError::BadRegion)?;
            // A fresh domain: every leaf is new, so map() returns () (no TlbFlush) and there is
            // nothing to invalidate. The IOMMU has never walked these tables; they are not installed
            // yet. `AlreadyMapped` here would mean two grants in `regions` overlap, which fails the
            // whole build closed rather than quietly merging them.
            m.map(page, page, Flags::user_data())?;
        }
    }
    Ok(())
}

// =============================================================================================
// Machine-checked proofs (Kani): **the domain maps exactly the grant.** Milestone 35 item 3.
//
// The property wanted is the hardware sibling of the DMA validator's: a device's domain maps
// precisely the frames it was granted and nothing else. Stated over the *built page tables* it is a
// `Mapper` build-and-translate round trip over a symbolic IOVA, which is the BMC-over-real-memory
// wall notes/verification.md declined for the ELF parser and for `Mapper` itself. So it is
// decomposed, the way that note says to decompose ("prefer refactoring the logic to shrinking the
// proof"): the *page set* the builder feeds the mapper is loopless arithmetic, and that is proved
// here for every region and every index. What the harnesses give, in the two directions that matter:
//
//   soundness (the security direction): every page the builder maps lies wholly inside a granted
//     region, so no ungranted byte becomes device-reachable;
//   completeness (the functional direction): every whole page of a granted region is mapped, so the
//     confinement does not silently starve an honest driver;
//   injectivity: no page is enumerated twice, so a build cannot fail `AlreadyMapped` against itself;
//   totality: the arithmetic never panics or wraps, for any base, size, or index.
//
// This is format-independent, which is the right shape for a §19 parity gate: one proof covers the
// SMMUv3 (VMSAv8-64) domain and the RISC-V IOMMU (Sv39) domain, because the page set does not depend
// on the format. What is *not* proved here is the remaining link, "`Mapper::map` writes exactly one
// leaf for the page it is told and touches nothing else." That stays underwritten by the proved walk
// arithmetic (`index_is_always_in_bounds`, `distinct_pages_take_distinct_paths`, the leaf codec on
// both formats) plus this module's build-and-translate tests below, on both formats. notes/dma.md
// states that residual plainly.
// =============================================================================================
#[cfg(kani)]
mod verification {
    use super::*;

    /// **The enumeration is total: no region and no index makes it panic or wrap.** Both entry points
    /// over fully symbolic inputs. Kani checks arithmetic overflow and panics on every operation, so a
    /// run that trips nothing is the totality proof; it is worth its own harness because the builder
    /// calls these on a region a caller supplied.
    #[kani::proof]
    fn the_grant_enumeration_is_total() {
        let r = DmaRegion {
            base: kani::any(),
            size: kani::any(),
        };
        let i: u64 = kani::any();
        let _ = grant_pages(r);
        let _ = grant_page(r, i);
    }

    /// **Soundness, the security direction: every page the domain maps lies wholly inside the
    /// grant.** For every region and every index the builder will visit, the enumerated IOVA is
    /// page-aligned and `[page, page + PAGE_SIZE)` is contained in `[base, base + size)`. So the
    /// domain cannot map a byte the device was not granted: not the bytes below an unaligned base
    /// (refused), not a trailing partial page (never enumerated), and not a low address reached by
    /// wrapping (refused). Since IOVA == PA, this is equally the statement that the device translates
    /// only to granted physical memory.
    #[kani::proof]
    fn an_enumerated_page_lies_inside_the_grant() {
        let r = DmaRegion {
            base: kani::any(),
            size: kani::any(),
        };
        let i: u64 = kani::any();
        if let Ok(pages) = grant_pages(r) {
            kani::assume(i < pages);
            let page = grant_page(r, i).expect("a page index below the count always resolves");
            assert!(
                page.is_multiple_of(PAGE_SIZE),
                "an unaligned page was mapped"
            );
            assert!(page >= r.base, "a page below the grant was mapped");
            let end = page
                .checked_add(PAGE_SIZE)
                .expect("an enumerated page cannot end past u64");
            let limit = r
                .base
                .checked_add(r.size)
                .expect("an accepted grant has an end");
            assert!(end <= limit, "a page past the end of the grant was mapped");
            // The harness reaches grants that actually have pages to map (see the non-vacuity note on
            // `every_whole_page_of_the_grant_is_enumerated`).
            kani::cover!(pages > 1, "a multi-page grant satisfies the assumptions");
        }
    }

    /// **The `ok_or` in the builder is dead code, provably.** For every accepted region, every index
    /// below the page count resolves, so the builder's defensive branch cannot fire and the loop maps
    /// exactly `pages` pages. Split from the soundness harness because it is the fact the builder's
    /// control flow depends on, and it should be named where a reader of that branch will look.
    #[kani::proof]
    fn a_page_index_below_the_count_always_resolves() {
        let r = DmaRegion {
            base: kani::any(),
            size: kani::any(),
        };
        let i: u64 = kani::any();
        if let Ok(pages) = grant_pages(r) {
            kani::assume(i < pages);
            assert!(
                grant_page(r, i).is_some(),
                "an index below the page count did not resolve",
            );
        }
    }

    /// **Completeness, the functional direction: no whole page of the grant is left unmapped.** For
    /// every accepted region and every page-aligned address whose whole page lies inside it, there is
    /// an index below the page count that enumerates exactly that address. Proved constructively (the
    /// witness is `(iova - base) / PAGE_SIZE`), which is stronger than an existence claim: it says
    /// *which* iteration maps it.
    ///
    /// Without this, "maps exactly the grant" would be half a property. A domain that mapped nothing
    /// at all would satisfy soundness perfectly and confine the device by starving it, which is not
    /// the confinement anybody wants, and the failure would surface as a mysterious device fault
    /// rather than as a refusal.
    #[kani::proof]
    fn every_whole_page_of_the_grant_is_enumerated() {
        let r = DmaRegion {
            base: kani::any(),
            size: kani::any(),
        };
        let iova: u64 = kani::any();
        if let Ok(pages) = grant_pages(r) {
            kani::assume(iova.is_multiple_of(PAGE_SIZE));
            kani::assume(iova >= r.base);
            let Some(end) = iova.checked_add(PAGE_SIZE) else {
                return;
            };
            let limit = r
                .base
                .checked_add(r.size)
                .expect("an accepted grant has an end");
            kani::assume(end <= limit);

            let i = (iova - r.base) / PAGE_SIZE;
            assert!(i < pages, "a granted page has no index in the enumeration");
            assert_eq!(
                grant_page(r, i),
                Some(iova),
                "a granted page is not the page its own index enumerates",
            );

            // **Non-vacuity, machine-checked.** Four stacked assumptions could in principle be jointly
            // unsatisfiable, in which case this harness would verify while proving nothing at all;
            // notes/verification.md calls that the main failure mode ("it proves what you asserted, not
            // what you meant"). `kani::cover!` fails when a state is *un*reachable, so this asserts the
            // harness really does reach a grant with pages in it.
            kani::cover!(pages > 0, "a non-empty grant satisfies the assumptions");
            kani::cover!(
                pages > 1,
                "so does a multi-page grant, so `i` is not pinned to 0"
            );
        }
    }

    /// **The enumeration is injective: two distinct indices are two distinct pages.** So one region's
    /// build never maps the same IOVA twice, which matters because [`Mapper::map`] refuses a second
    /// write to a live leaf ([`MapError::AlreadyMapped`]): without injectivity a perfectly legal grant
    /// could fail its own build. Together with soundness and completeness this pins the mapped set to
    /// exactly the grant's whole pages, counted once each.
    #[kani::proof]
    fn the_enumeration_is_injective() {
        let r = DmaRegion {
            base: kani::any(),
            size: kani::any(),
        };
        let i: u64 = kani::any();
        let j: u64 = kani::any();
        if let Ok(pages) = grant_pages(r) {
            kani::assume(i < pages);
            kani::assume(j < pages);
            kani::assume(i != j);
            assert_ne!(
                grant_page(r, i),
                grant_page(r, j),
                "two distinct indices enumerate the same page",
            );
            // Two distinct in-range indices must actually exist, or this proves nothing: a grant of one
            // page satisfies `i < pages && j < pages && i != j` for no `(i, j)` at all.
            kani::cover!(
                pages > 1,
                "a grant with two distinct page indices is reachable"
            );
        }
    }

    /// **A grant the domain cannot express exactly is refused, not approximated.** For every region
    /// with an unaligned base or an end that wraps `u64`, [`grant_pages`] errors, so the builder maps
    /// nothing at all rather than mapping a rounded or wrapped page set. This is the fail-closed half
    /// of the property: the two inputs that could produce an *over*-map are the two that are refused.
    #[kani::proof]
    fn a_grant_the_domain_cannot_express_is_refused() {
        let r = DmaRegion {
            base: kani::any(),
            size: kani::any(),
        };
        if !r.base.is_multiple_of(PAGE_SIZE) || r.base.checked_add(r.size).is_none() {
            assert!(
                grant_pages(r).is_err(),
                "a grant no page-granular domain can express exactly was accepted",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::alloc::{Layout, alloc_zeroed};
    use std::cell::RefCell;
    use std::vec::Vec;

    use super::*;
    use crate::{Aarch64, Sv39};

    // A SYNTHETIC physical address space, and the reason it exists is a CI failure worth recording.
    //
    // These tests used to leak zeroed host frames and use their host addresses directly as "physical"
    // ones, with an identity `phys_to_ptr`. That works only if the host allocator happens to hand back
    // addresses inside the format's addressable low half, which is not a property any allocator
    // promises. On aarch64 macOS it did; on aarch64 Linux it does not, so `build_identity_domain`
    // correctly refused with `MapError::WrongHalf` and two tests failed in CI while passing locally.
    // The guard was right and the test was wrong.
    //
    // So the "physical" addresses are now chosen by the test, in the low half where both Sv39 and
    // Aarch64 can express them, and the host frames sit *behind* `phys_to_ptr`, which is precisely
    // what that indirection is for. The tests are host-independent and deterministic as a result: the
    // addresses no longer depend on where malloc felt like putting something.

    /// Where the synthetic pool starts. Low half, page-aligned, and far below Sv39's 2^38 ceiling.
    const POOL_BASE: u64 = 0x0100_0000;

    thread_local! {
        /// Synthetic PA to host pointer. Thread-local so parallel tests cannot see each other's
        /// frames, which also means each test's pool starts empty.
        static PHYS: RefCell<Vec<(u64, u64)>> = const { RefCell::new(Vec::new()) };
    }

    /// Frees the pool's host frames when a test ends. Each test that allocates frames holds one.
    ///
    /// The frames used to be simply leaked, on the theory that the test process exits anyway,
    /// which was true and still let a real gate rot: Miri's leak check (milestone 79) reports
    /// every one of them, sixteen reports drowning out whatever it might otherwise say about
    /// this crate. A `Drop` guard is also what a test should have done all along; the leak was
    /// a shortcut, not a decision (notes/undefined-behavior.md).
    struct PoolGuard;
    impl Drop for PoolGuard {
        fn drop(&mut self) {
            PHYS.with(|m| {
                for (_, host) in m.borrow_mut().drain(..) {
                    // SAFETY: `host` came from `frame_at`'s alloc_zeroed with this exact layout,
                    // registered exactly once, and the drain removes it so nothing frees it twice.
                    unsafe {
                        std::alloc::dealloc(
                            host as *mut u8,
                            Layout::from_size_align(4096, 4096).unwrap(),
                        );
                    }
                }
            });
        }
    }

    /// Back the synthetic physical address `pa` with a real zeroed host frame.
    fn frame_at(pa: u64) -> u64 {
        assert!(
            pa.is_multiple_of(PAGE_SIZE),
            "a synthetic PA must be page-aligned"
        );
        // SAFETY: a valid non-zero layout; alloc_zeroed returns zeroed memory or null (asserted).
        let p = unsafe { alloc_zeroed(Layout::from_size_align(4096, 4096).unwrap()) };
        assert!(!p.is_null());
        PHYS.with(|m| m.borrow_mut().push((pa, p as u64)));
        pa
    }

    /// The next frame from the sequential pool: for roots and page tables, where the address does not
    /// matter as long as it is legal and distinct.
    fn frame() -> u64 {
        let n = PHYS.with(|m| m.borrow().len()) as u64;
        frame_at(POOL_BASE + n * PAGE_SIZE)
    }

    /// Resolve a synthetic PA to the host frame behind it. Panics on an unregistered address, which
    /// would mean the walker followed a pointer nothing ever allocated: a loud test bug beats a
    /// segfault.
    fn phys_to_ptr(pa: u64) -> *mut PageTable {
        PHYS.with(|m| {
            m.borrow()
                .iter()
                .find(|(p, _)| *p == pa)
                .map(|(_, host)| *host as *mut PageTable)
                .unwrap_or_else(|| panic!("no host frame backs synthetic PA {pa:#x}"))
        })
    }

    /// Build a domain over one region and check every in-region page translates to itself while a
    /// page just past the end does not. This is the confinement stated as a property of the tables.
    fn one_region_confines<F: PageFormat>() {
        let mut frames: Vec<u64> = Vec::new();
        let root = frame();
        // The device's data frame, at an address the TEST chooses so the assertions below mean what
        // they say: `base + PAGE_SIZE` must be a page the domain was not given, and with a sequential
        // pool it would have been the next frame allocated instead.
        let base = frame_at(0x2000_0000);
        let regions = [DmaRegion {
            base,
            size: PAGE_SIZE,
        }];
        // SAFETY: root and every alloc'd frame are zeroed, page-aligned host frames; identity
        // phys_to_ptr satisfies the contract on the host.
        unsafe {
            build_identity_domain::<_, _, F>(
                root,
                || {
                    let f = frame();
                    frames.push(f);
                    Some(f)
                },
                phys_to_ptr,
                &regions,
            )
            .expect("domain build failed");
        }

        // SAFETY: root is a live table; translate only reads it.
        let m = unsafe {
            Mapper::<fn() -> Option<u64>, _, F>::new(root, Half::Low, || None, phys_to_ptr)
        };
        assert_eq!(
            m.translate(base),
            Some((base, Flags::user_data())),
            "an in-region IOVA did not translate to its own PA",
        );
        assert_eq!(
            m.translate(base + PAGE_SIZE),
            None,
            "a page past the region translated: the device is not confined",
        );
    }

    #[test]
    fn aarch64_domain_confines_a_region() {
        let _pool = PoolGuard;
        one_region_confines::<Aarch64>();
    }

    #[test]
    fn sv39_domain_confines_a_region() {
        let _pool = PoolGuard;
        one_region_confines::<Sv39>();
    }

    /// **A grant of more than one page maps every whole page in it, and stops at the partial tail.**
    /// Every other test in this module grants exactly one page, and a one-page region is the shape
    /// that can tell nothing apart: a page count and the constant 1 agree on it, and so do
    /// [`grant_page`]'s whole-page test and its opposite. Two pages and a half separates all three.
    #[test]
    fn a_multi_page_grant_maps_its_whole_pages_and_not_the_partial_tail() {
        let mut frames: Vec<u64> = Vec::new();
        let root = frame();
        let base = frame_at(0x2000_0000);
        let regions = [DmaRegion {
            base,
            size: 2 * PAGE_SIZE + PAGE_SIZE / 2,
        }];
        // SAFETY: as in `one_region_confines`.
        unsafe {
            build_identity_domain::<_, _, Aarch64>(
                root,
                || {
                    let f = frame();
                    frames.push(f);
                    Some(f)
                },
                phys_to_ptr,
                &regions,
            )
            .expect("domain build failed");
        }
        // SAFETY: `root` is the table the build above populated, reachable through `phys_to_ptr`.
        let m = unsafe {
            Mapper::<fn() -> Option<u64>, _, Aarch64>::new(root, Half::Low, || None, phys_to_ptr)
        };
        assert_eq!(m.translate(base), Some((base, Flags::user_data())));
        assert_eq!(
            m.translate(base + PAGE_SIZE),
            Some((base + PAGE_SIZE, Flags::user_data())),
            "the grant's second whole page is missing: an honest driver is starved",
        );
        assert_eq!(
            m.translate(base + 2 * PAGE_SIZE),
            None,
            "the trailing half page was mapped whole: the device reaches past its grant",
        );
    }

    /// Two disjoint regions (the driver's DMA region and the kernel's shadow page are not adjacent)
    /// both map, and the gap between them does not: the domain is exactly the allow-list, no more.
    #[test]
    fn two_disjoint_regions_map_and_the_gap_does_not() {
        let _pool = PoolGuard;
        let mut frames: Vec<u64> = Vec::new();
        let root = frame();
        // Far apart on purpose: the point of this test is the GAP between them, and adjacent regions
        // would prove nothing about it. Sequentially-pooled frames would be adjacent, which is how
        // this assertion could have quietly stopped testing anything.
        let a = frame_at(0x2000_0000);
        let b = frame_at(0x3000_0000);
        let regions = [
            DmaRegion {
                base: a,
                size: PAGE_SIZE,
            },
            DmaRegion {
                base: b,
                size: PAGE_SIZE,
            },
        ];
        // SAFETY: as above.
        unsafe {
            build_identity_domain::<_, _, Sv39>(
                root,
                || {
                    let f = frame();
                    frames.push(f);
                    Some(f)
                },
                phys_to_ptr,
                &regions,
            )
            .expect("domain build failed");
        }
        // SAFETY: `root` is the table the domain build above just populated, and `phys_to_ptr` is this test's own identity map over the frames it allocated, so every physical address the mapper walks is one it can read.
        let m = unsafe {
            Mapper::<fn() -> Option<u64>, _, Sv39>::new(root, Half::Low, || None, phys_to_ptr)
        };
        assert!(m.translate(a).is_some(), "region A did not map");
        assert!(m.translate(b).is_some(), "region B did not map");
        // A frame the domain was never given: unmapped, so the device faults on it. In the gap
        // between A and B, which is the strongest place to ask.
        let c = frame_at(0x2800_0000);
        assert_eq!(
            m.translate(c),
            None,
            "an ungranted frame translated: the domain leaks",
        );
    }
}
