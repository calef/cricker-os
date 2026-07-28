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
//! aarch64 at 0x4000_0000, riscv at 0x8000_0000, both far below either format's [`Half`] split.)
//!
//! # The RISC-V U-bit, stated once
//!
//! A single-stage (`iosatp`, no process context) RISC-V IOMMU translation faults on a leaf PTE
//! whose U bit is clear, because the device is not "requesting supervisor privilege". So the domain
//! is built with [`Flags::user_data`], which sets U on Sv39 and read/write on both formats. It is a
//! device's data window, so user-accessible read/write with no execute is exactly right.

use crate::{Flags, Half, MapError, Mapper, PageFormat, PageTable};

/// A physical region a device is allowed to touch: `[base, base + size)`, 4 KiB aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaRegion {
    pub base: u64,
    pub size: u64,
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
        let count = r.size / crate::PAGE_SIZE;
        // A fresh domain: every leaf is new, so map() returns () (no TlbFlush) and there is nothing
        // to invalidate. The IOMMU has never walked these tables; they are not installed yet.
        m.map_range(r.base, r.base, count, Flags::user_data())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Aarch64, PAGE_SIZE, Sv39};
    use std::alloc::{Layout, alloc_zeroed};
    use std::vec::Vec;

    /// A host stand-in for physical memory: leak zeroed, page-aligned frames and use their host
    /// addresses as "physical" ones, exactly as the paging crate's other tests do. The pointer
    /// arithmetic is identical, which is the whole reason page-table logic is host-testable.
    fn frame() -> u64 {
        // SAFETY: a valid non-zero layout; alloc_zeroed returns zeroed memory or null (asserted).
        let p = unsafe { alloc_zeroed(Layout::from_size_align(4096, 4096).unwrap()) };
        assert!(!p.is_null());
        p as u64
    }

    fn phys_to_ptr(pa: u64) -> *mut PageTable {
        pa as *mut PageTable
    }

    /// Build a domain over one region and check every in-region page translates to itself while a
    /// page just past the end does not. This is the confinement stated as a property of the tables.
    fn one_region_confines<F: PageFormat>() {
        let mut frames: Vec<u64> = Vec::new();
        let root = frame();
        let base = frame(); // a single "device" data frame; its host address is its "PA"
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
        one_region_confines::<Aarch64>();
    }

    #[test]
    fn sv39_domain_confines_a_region() {
        one_region_confines::<Sv39>();
    }

    /// Two disjoint regions (the driver's DMA region and the kernel's shadow page are not adjacent)
    /// both map, and the gap between them does not: the domain is exactly the allow-list, no more.
    #[test]
    fn two_disjoint_regions_map_and_the_gap_does_not() {
        let mut frames: Vec<u64> = Vec::new();
        let root = frame();
        let a = frame();
        let b = frame();
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
        let m = unsafe {
            Mapper::<fn() -> Option<u64>, _, Sv39>::new(root, Half::Low, || None, phys_to_ptr)
        };
        assert!(m.translate(a).is_some(), "region A did not map");
        assert!(m.translate(b).is_some(), "region B did not map");
        // A frame the domain was never given: unmapped, so the device faults on it.
        let c = frame();
        assert_eq!(
            m.translate(c),
            None,
            "an ungranted frame translated: the domain leaks",
        );
    }
}
