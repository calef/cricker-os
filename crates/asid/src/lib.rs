//! The ASID allocator: **which number tags this address space's TLB entries.**
//!
//! Milestone 15 (design/roadmap.md). An ASID (Address Space IDentifier) is a small number the
//! hardware attaches to every TLB entry created from a non-global (`nG`) mapping, and compares
//! on every lookup against the ASID in `TTBR0_EL1`'s top bits. With every user mapping tagged,
//! a context switch stops flushing anything: entries from other address spaces simply stop
//! matching. Before this, every switch ran `tlbi vmalle1is`, discarding every EL1 translation
//! on every core, the kernel's included.
//!
//! # Why a bitmap and not Linux's generation scheme
//!
//! Linux allocates ASIDs by *generation*: processes outnumber ASIDs (256 or 65536), so on
//! exhaustion it bumps a generation counter, flushes everything once, and lazily re-assigns.
//! That machinery guards one case: more live address spaces than ASID numbers. **Our own bounds
//! make that case unreachable**: milestone 14 fixed concurrent address spaces at MAX_SPACES
//! (160), below even the smallest hardware ASID space (8-bit, 256). So each address space keeps
//! one ASID for its whole life, allocation is a bitmap scan, and the rollover path that could
//! never be honestly exercised was never built. If MAX_SPACES ever outgrows 255, the fix is one
//! TCR bit (16-bit ASIDs) before it is ever a new algorithm.
//!
//! # The invariant the caller keeps
//!
//! Reusing a number is only safe once the TLB holds no entries tagged with it. The kernel
//! invalidates by ASID (`tlbi aside1is`) in `AddressSpace::drop`, *before* calling
//! [`free`](Allocator::free). This crate proves the numbers are managed soundly; the flush is
//! the kernel's half of the contract, stated at the call site.

#![cfg_attr(not(test), no_std)]

/// One more than the largest ASID we hand out. 8-bit ASIDs: every aarch64 implementation has at
/// least these, and 255 usable numbers exceed MAX_SPACES (160) with room.
pub const ASIDS: usize = 256;

/// The allocator: one bit per ASID, set = in use. ASID 0 is born allocated and is never freed:
/// it is the kernel's, carried by the reserved "nobody is home" table, so a user address space
/// can never share a tag with the state that means no user at all.
pub struct Allocator {
    bitmap: [u64; ASIDS / 64],
}

impl Allocator {
    pub const fn new() -> Self {
        let mut bitmap = [0u64; ASIDS / 64];
        bitmap[0] = 1; // ASID 0: reserved for "no user address space", forever
        Self { bitmap }
    }

    /// Claim a free ASID. `None` only if all 255 are live, which MAX_SPACES makes unreachable
    /// in the kernel; the type is honest anyway.
    pub fn alloc(&mut self) -> Option<u16> {
        for (i, word) in self.bitmap.iter_mut().enumerate() {
            if *word != u64::MAX {
                let bit = word.trailing_ones() as usize;
                *word |= 1 << bit;
                return Some((i * 64 + bit) as u16);
            }
        }
        None
    }

    /// Return an ASID. The caller has already invalidated the TLB by this ASID on every core
    /// (see the crate doc); after this, the number may tag a different address space.
    pub fn free(&mut self, asid: u16) {
        debug_assert!(asid != 0, "ASID 0 is the kernel's and is never freed");
        debug_assert!((asid as usize) < ASIDS, "ASID out of range");
        if asid != 0 && (asid as usize) < ASIDS {
            self.bitmap[asid as usize / 64] &= !(1 << (asid as usize % 64));
        }
    }
}

impl Default for Allocator {
    fn default() -> Self {
        Self::new()
    }
}

/// Machine-checked proofs (DECISIONS §14). notes/verification.md predicted "the ASID allocator
/// when it lands" as a frontier crate; this is it landing. The properties are the ones the
/// privilege boundary rests on: no user space ever tags its entries 0 (the kernel's), and no
/// two live spaces ever share a tag (sharing one would let the TLB serve one process the
/// other's memory).
#[cfg(kani)]
mod verification {
    use super::*;

    /// **Zero is never handed out**, from any reachable allocator state. State is modeled
    /// symbolically: any bitmap with bit 0 set, which is every state `new` + any operation
    /// sequence can produce, since `alloc` only sets bits and `free` refuses ASID 0.
    #[kani::proof]
    fn the_kernel_asid_is_never_allocated() {
        let mut a = Allocator {
            bitmap: [kani::any(), kani::any(), kani::any(), kani::any()],
        };
        a.bitmap[0] |= 1; // the invariant free() preserves: bit 0 stays set
        if let Some(asid) = a.alloc() {
            assert_ne!(asid, 0);
            assert!((asid as usize) < ASIDS);
        }
    }

    /// **Two allocations never alias.** From any state, two back-to-back allocs return distinct
    /// numbers: alloc claims the bit before returning, so the second scan cannot find it.
    #[kani::proof]
    fn two_live_asids_are_distinct() {
        let mut a = Allocator {
            bitmap: [kani::any(), kani::any(), kani::any(), kani::any()],
        };
        if let (Some(x), Some(y)) = (a.alloc(), a.alloc()) {
            assert_ne!(x, y);
        }
    }

    /// **Free really frees, and only its own bit.** From any state with `asid` live, freeing it
    /// makes it allocatable again and leaves every other bit exactly as it was.
    #[kani::proof]
    fn free_releases_exactly_its_own_asid() {
        let mut a = Allocator {
            bitmap: [kani::any(), kani::any(), kani::any(), kani::any()],
        };
        let asid: u16 = kani::any();
        kani::assume(asid != 0 && (asid as usize) < ASIDS);

        let before = a.bitmap;
        a.free(asid);

        let (word, bit) = (asid as usize / 64, asid as usize % 64);
        assert_eq!(a.bitmap[word] & !(1 << bit), before[word] & !(1 << bit));
        assert_eq!(a.bitmap[word] & (1 << bit), 0);
        for i in 0..a.bitmap.len() {
            if i != word {
                assert_eq!(a.bitmap[i], before[i]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first user address space does not get the kernel's tag.
    #[test]
    fn the_first_asid_is_one_not_zero() {
        let mut a = Allocator::new();
        assert_eq!(a.alloc(), Some(1));
    }

    /// All 255 usable ASIDs allocate, distinctly; the 256th request is refused.
    #[test]
    fn every_usable_asid_allocates_once_then_none() {
        let mut a = Allocator::new();
        let mut seen = [false; ASIDS];
        for _ in 0..ASIDS - 1 {
            let asid = a.alloc().expect("ran out early") as usize;
            assert!(!seen[asid], "ASID {asid} handed out twice");
            assert_ne!(asid, 0);
            seen[asid] = true;
        }
        assert_eq!(a.alloc(), None, "a 256th ASID appeared from nowhere");
    }

    /// Freed numbers come back; the kernel's never does.
    #[test]
    fn free_then_alloc_reuses() {
        let mut a = Allocator::new();
        let x = a.alloc().unwrap();
        let y = a.alloc().unwrap();
        a.free(x);
        assert_eq!(a.alloc(), Some(x), "the freed ASID was not reused");
        a.free(y);
        assert_eq!(a.alloc(), Some(y));
    }
}
