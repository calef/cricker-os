//! The RISC-V Sv39 page-table format: three levels, 4 KiB pages, 39-bit virtual addresses.
//!
//! ```text
//!  38      30 29      21 20      12 11         0
//! ┌──────────┬──────────┬──────────┬────────────┐
//! │ VPN[2]   │ VPN[1]   │ VPN[0]   │   offset   │
//! │  9 bits  │  9 bits  │  9 bits  │  12 bits   │
//! └──────────┴──────────┴──────────┴────────────┘
//! ```
//!
//! A Sv39 PTE is flatter than an aarch64 descriptor: the permission bits (R/W/X/U/G) are one bit
//! each, and an entry is a *leaf* when any of R/W/X is set and a *pointer* when none are. Bits 9:8
//! are RSW, reserved for the OS; we use one to carry the "device memory" flag, which base Sv39 (no
//! Svpbmt) has no architectural PTE bit for. [`Sv39`] implements [`PageFormat`] by
//! translating the crate's portable [`Flags`] to and from these bits.

use crate::{
    CAP_DEVICE, CAP_GLOBAL, CAP_KERNEL_EXEC, CAP_USER, CAP_USER_EXEC, CAP_WRITE, Flags, PageFormat,
};

const V: u64 = 1 << 0; // Valid
const R: u64 = 1 << 1; // Readable
const W: u64 = 1 << 2; // Writable
const X: u64 = 1 << 3; // eXecutable
const U: u64 = 1 << 4; // User-accessible
const G: u64 = 1 << 5; // Global
const A: u64 = 1 << 6; // Accessed
const D: u64 = 1 << 7; // Dirty

/// Bit 8, one of the two RSW (reserved-for-software) bits. We use it to record device-typed memory,
/// which base Sv39 has no PTE encoding for (real hardware would use Svpbmt's bits 62:61). On QEMU's
/// `virt` the memory type actually comes from the physical address, so this bit is bookkeeping that
/// keeps the portable [`Flags`] round-trip exact rather than something the hardware reads.
const SW_DEVICE: u64 = 1 << 8;

/// The PPN occupies bits [53:10] of a PTE: 44 bits.
const PPN_SHIFT: u32 = 10;
const PPN_MASK: u64 = (1 << 44) - 1;

/// The RISC-V Sv39 three-level page-table format.
pub struct Sv39;

impl Sv39 {
    /// Encode the permission/attribute bits (not the PPN or V) for a leaf with `flags`. A leaf is
    /// always readable (R), and we set A/D eagerly so the hardware never faults to update them (the
    /// Sv39 analog of always setting aarch64's Access Flag). D is set only for writable pages.
    const fn attrs(flags: Flags) -> u64 {
        let mut bits = R | A;
        if flags.is_writable() {
            bits |= W | D;
        }
        // Sv39 has one execute bit; the U bit decides at which privilege it applies (U-mode can
        // execute a U page, S-mode a non-U page, and never the other's). So the aarch64 PXN/UXN
        // split maps to X together with U.
        if flags.is_user_executable() || flags.is_kernel_executable() {
            bits |= X;
        }
        if flags.is_user_accessible() {
            bits |= U;
        }
        if flags.is_global() {
            bits |= G;
        }
        if flags.is_device() {
            bits |= SW_DEVICE;
        }
        bits
    }
}

impl PageFormat for Sv39 {
    const LEVELS: usize = 3;
    const SPLIT_SHIFT: u32 = 38;

    fn is_present(entry: u64) -> bool {
        entry & V != 0
    }

    fn entry_pa(entry: u64) -> u64 {
        ((entry >> PPN_SHIFT) & PPN_MASK) << 12
    }

    fn table_entry(pa: u64) -> u64 {
        // A pointer PTE: PPN plus V, and R=W=X=0 (that zero is what marks it a pointer, not a leaf).
        (((pa >> 12) & PPN_MASK) << PPN_SHIFT) | V
    }

    fn leaf_entry(pa: u64, flags: Flags) -> u64 {
        (((pa >> 12) & PPN_MASK) << PPN_SHIFT) | V | Self::attrs(flags)
    }

    fn leaf_flags(entry: u64) -> Flags {
        let mut caps = 0;
        if entry & W != 0 {
            caps |= CAP_WRITE;
        }
        if entry & U != 0 {
            caps |= CAP_USER;
        }
        if entry & X != 0 {
            // Execute permission belongs to whichever privilege the U bit names.
            if entry & U != 0 {
                caps |= CAP_USER_EXEC;
            } else {
                caps |= CAP_KERNEL_EXEC;
            }
        }
        if entry & G != 0 {
            caps |= CAP_GLOBAL;
        }
        if entry & SW_DEVICE != 0 {
            caps |= CAP_DEVICE;
        }
        Flags::from_caps(caps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Every leaf is present, readable, and accessed.** V and R make it a valid readable leaf; A
    /// set eagerly avoids an access fault on first touch (the Sv39 analog of aarch64's AF).
    #[test]
    fn every_leaf_is_valid_readable_and_accessed() {
        for flags in [
            Flags::kernel_code(),
            Flags::kernel_data(),
            Flags::user_code(),
            Flags::user_data(),
        ] {
            let leaf = Sv39::leaf_entry(0x8020_0000, flags);
            assert_ne!(leaf & V, 0, "not valid: {flags:?}");
            assert_ne!(leaf & R, 0, "not readable: {flags:?}");
            assert_ne!(leaf & A, 0, "accessed bit not set: {flags:?}");
        }
    }

    /// **A writable leaf is pre-dirtied, and a read-only one has nothing to dirty.** D is set for
    /// the same reason as A: hardware that has to set it itself takes a store fault on the first
    /// write to every page. Neither bit reaches the portable [`Flags`], so the round-trip test below
    /// cannot see D go missing.
    #[test]
    fn a_writable_leaf_is_pre_dirtied() {
        assert_ne!(
            Sv39::leaf_entry(0x8020_0000, Flags::kernel_data()) & D,
            0,
            "a writable leaf faults on its first store to set D",
        );
        assert_eq!(
            Sv39::leaf_entry(0x8020_0000, Flags::kernel_code()) & D,
            0,
            "a read-only page cannot be dirtied",
        );
    }

    /// **A table-pointer entry has R=W=X=0** (that is what distinguishes it from a leaf) and V=1.
    #[test]
    fn a_table_entry_is_a_pointer_not_a_leaf() {
        let e = Sv39::table_entry(0x8100_0000);
        assert_ne!(e & V, 0);
        assert_eq!(e & (R | W | X), 0, "a pointer must have R=W=X clear");
        assert_eq!(Sv39::entry_pa(e), 0x8100_0000);
    }

    /// **Every constructor round-trips through encode/decode**, including the RSW device bit.
    #[test]
    fn flags_round_trip_through_a_leaf() {
        for flags in [
            Flags::kernel_code(),
            Flags::kernel_rodata(),
            Flags::kernel_data(),
            Flags::device(),
            Flags::user_code(),
            Flags::user_rodata(),
            Flags::user_data(),
            Flags::user_device(),
        ] {
            let leaf = Sv39::leaf_entry(0x8020_0000, flags);
            assert_eq!(Sv39::entry_pa(leaf), 0x8020_0000);
            assert_eq!(
                Sv39::leaf_flags(leaf),
                flags,
                "round-trip failed for {flags:?}"
            );
        }
    }
}

/// Machine-checked proofs of the Sv39 format, mirroring the aarch64 module's. The shared `Mapper`
/// walk inherits both formats' guarantees. See notes/verification.md.
#[cfg(kani)]
mod verification {
    use super::*;
    use crate::{Half, PAGE_SIZE};

    /// **The walk never indexes past a table** (three levels here).
    #[kani::proof]
    fn index_is_always_in_bounds() {
        let va: u64 = kani::any();
        let level: usize = kani::any();
        kani::assume(level < Sv39::LEVELS);
        assert!(Sv39::index(va, level) < crate::ENTRIES);
    }

    /// **The three indices and the offset tile the low 39 bits exactly.**
    #[kani::proof]
    fn the_indices_and_offset_tile_the_address() {
        let va: u64 = kani::any();
        let reconstructed = ((Sv39::index(va, 0) as u64) << 30)
            | ((Sv39::index(va, 1) as u64) << 21)
            | ((Sv39::index(va, 2) as u64) << 12)
            | (va & (PAGE_SIZE - 1));
        assert_eq!(reconstructed, va & 0x0000_007f_ffff_ffff);
    }

    /// **Distinct pages take distinct paths** within the 39-bit VA.
    #[kani::proof]
    fn distinct_pages_take_distinct_paths() {
        let a: u64 = kani::any::<u64>() & 0x0000_007f_ffff_f000;
        let b: u64 = kani::any::<u64>() & 0x0000_007f_ffff_f000;
        kani::assume(
            Sv39::index(a, 0) == Sv39::index(b, 0)
                && Sv39::index(a, 1) == Sv39::index(b, 1)
                && Sv39::index(a, 2) == Sv39::index(b, 2),
        );
        assert_eq!(a, b);
    }

    /// **The two halves are disjoint** at the Sv39 split (bit 38).
    #[kani::proof]
    fn the_two_halves_are_disjoint() {
        let va: u64 = kani::any();
        assert!(!(Sv39::in_half(Half::Low, va) && Sv39::in_half(Half::High, va)));
    }

    /// **The user-VA gate admits exactly the aligned low half**, never the high one.
    #[kani::proof]
    fn the_user_va_gate_admits_only_the_aligned_low_half() {
        let va: u64 = kani::any();
        assert_eq!(
            crate::is_user_page_va::<Sv39>(va),
            va & 0xfff == 0 && va >> 38 == 0
        );
        if crate::is_user_page_va::<Sv39>(va) {
            assert!(Sv39::in_half(Half::Low, va) && !Sv39::in_half(Half::High, va));
        }
    }

    /// **A leaf keeps the address and the permissions apart, and the permissions round-trip.**
    #[kani::proof]
    fn the_leaf_keeps_address_and_permissions_apart() {
        // Page-aligned physical address within Sv39's 56-bit physical space.
        let pa: u64 = kani::any();
        kani::assume(pa & !(PPN_MASK << 12) == 0);

        let all = [
            Flags::kernel_code(),
            Flags::kernel_rodata(),
            Flags::kernel_data(),
            Flags::device(),
            Flags::user_code(),
            Flags::user_rodata(),
            Flags::user_data(),
            Flags::user_device(),
        ];
        let i: usize = kani::any();
        kani::assume(i < all.len());
        let flags = all[i];

        let leaf = Sv39::leaf_entry(pa, flags);
        assert_eq!(Sv39::entry_pa(leaf), pa);
        assert_eq!(Sv39::leaf_flags(leaf), flags);
    }
}
