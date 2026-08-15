//! The online-cpu set as pure logic: **which cpus a hart mask names, and the k-th of them.**
//!
//! The kernel's set of online cpus is a bitmask (`smp::ONLINE_MASK`), and the bug class this crate
//! exists to kill is treating that set as if it were `0..count`. On QEMU `virt` the online set is
//! contiguous from zero, so `0..online_count()` works by coincidence; on the VisionFive 2 it is
//! `{1, 2, 3}` (slot 0 is the excluded S7, an M-mode monitor core with no MMU), and count-as-index
//! does two wrong things at once: it touches a parked core's per-CPU state, and it skips an online
//! cpu entirely. The first-silicon bench hit exactly that on 2026-08-14, as spawn placement putting
//! `init` into parked slot 0's inbox, which nothing will ever drain (three boots to diagnose; see
//! notes/visionfive2.md and the sched.rs placement fix).
//!
//! The functions here are the mask arithmetic with no kernel state attached, so the `{1, 2, 3}`
//! shape the QEMU suite can never boot is proven on the host, in milliseconds, below. The kernel's
//! `smp::online_cpus` and `smp::nth_online` delegate here and add only the live mask.
//!
//! Name: ratified 2026-08-14 (calef, the same day the first-silicon online-set sweep introduced
//! it). A noun for the thing it holds, a set of cpus, and the POSIX term for exactly this concept
//! (`cpu_set_t`, `sched_setaffinity`), which puts it in the naming tenet's protected class: a name
//! the reader already knows from outside costs them nothing. `cpus` alone was rejected as a
//! generic word, and `hart_mask` as RISC-V vocabulary for a crate both ISAs use.
//!
//! # EXAMPLES
//!
//! The VisionFive 2's shape, the one QEMU cannot represent:
//!
//! ```
//! let vf2 = 0b1110usize; // harts {1, 2, 3} online, slot 0 parked
//! assert_eq!(cpu_set::cpus_in(vf2).collect::<Vec<_>>(), vec![1, 2, 3]);
//! assert_eq!(cpu_set::nth_in(vf2, 0), Some(1)); // the 0th online cpu is 1, not 0
//! assert_eq!(cpu_set::nth_in(vf2, 3), Some(1)); // wraps around the set
//! ```
#![cfg_attr(not(test), no_std)]

/// Iterator over the set bit positions of a mask, ascending. See [`cpus_in`].
#[derive(Clone, Copy)]
pub struct Cpus(usize);

impl Iterator for Cpus {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        if self.0 == 0 {
            return None;
        }
        let i = self.0.trailing_zeros() as usize;
        self.0 &= self.0 - 1; // clear the lowest set bit
        Some(i)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.0.count_ones() as usize;
        (n, Some(n))
    }
}

impl ExactSizeIterator for Cpus {}

/// The cpus a mask names, by id, in ascending order. This is the only correct way to enumerate
/// per-cpu state for "every online cpu": a range `0..count` is the same loop only when the set is
/// contiguous from zero, which is a property of QEMU `virt` and not of machines.
#[inline]
pub fn cpus_in(mask: usize) -> Cpus {
    Cpus(mask)
}

/// The k-th cpu the mask names, wrapping around the set; `None` only for an empty mask. The
/// sampler's safe form of "a random cpu from the set": `nth_in(mask, rng())` never lands outside
/// the mask, where `rng() % count` as an *index* lands on parked cpus and skips online ones.
#[inline]
pub fn nth_in(mask: usize, k: usize) -> Option<usize> {
    let n = mask.count_ones() as usize;
    if n == 0 {
        return None;
    }
    cpus_in(mask).nth(k % n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The VisionFive 2's online set, the shape that found the bug: harts {1, 2, 3}, slot 0 parked.
    const VF2: usize = 0b1110;

    #[test]
    fn the_visionfive2_mask_yields_exactly_its_harts_in_order() {
        assert_eq!(cpus_in(VF2).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn count_as_index_is_not_this_iterator() {
        // The disease, stated as arithmetic: the count-3 range and the 3-cpu set share a length
        // and not a single element beyond {1, 2}. Index 0 is a parked core; cpu 3 is skipped.
        let range: Vec<usize> = (0..cpus_in(VF2).len()).collect();
        assert_eq!(range, vec![0, 1, 2]);
        assert!(
            cpus_in(VF2).all(|c| c != 0),
            "the set never names parked slot 0"
        );
        assert!(
            range.iter().all(|&c| c != 3),
            "the range never reaches online cpu 3"
        );
    }

    #[test]
    fn nth_wraps_around_the_set_and_never_leaves_it() {
        for k in 0..32 {
            let c = nth_in(VF2, k).expect("non-empty mask");
            assert!(VF2 & (1 << c) != 0, "nth_in({k}) = {c} is not in the mask");
            assert_eq!(c, [1, 2, 3][k % 3], "wrap order");
        }
    }

    #[test]
    fn a_contiguous_mask_agrees_with_the_range_it_replaced() {
        // On QEMU virt the old idiom was right by coincidence; the new one must be identical there,
        // or the sweep changed behavior on the machine every merge boots.
        for n in 1..=8usize {
            let mask = (1 << n) - 1;
            assert_eq!(
                cpus_in(mask).collect::<Vec<_>>(),
                (0..n).collect::<Vec<_>>()
            );
            for k in 0..2 * n {
                assert_eq!(nth_in(mask, k), Some(k % n));
            }
        }
    }

    #[test]
    fn the_empty_mask_is_empty_and_says_so() {
        assert_eq!(cpus_in(0).count(), 0);
        assert_eq!(nth_in(0, 0), None);
        assert_eq!(nth_in(0, 7), None);
    }

    #[test]
    fn every_mask_up_to_a_byte_yields_its_own_bits() {
        // Exhaustive over 8-bit masks, which since the 2026-08-14 bump is exactly MAX_CPUS: the iterator
        // yields exactly the set bits, ascending, and its length is the popcount.
        for mask in 0usize..256 {
            let got: Vec<usize> = cpus_in(mask).collect();
            let want: Vec<usize> = (0..8).filter(|i| mask & (1 << i) != 0).collect();
            assert_eq!(got, want, "mask {mask:#b}");
            assert_eq!(cpus_in(mask).len(), mask.count_ones() as usize);
        }
    }
}
