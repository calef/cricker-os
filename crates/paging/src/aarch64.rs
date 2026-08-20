//! The aarch64 page-table format: four levels, 4 KiB pages, 48-bit virtual addresses.
//!
//! ```text
//!  47      39 38      30 29      21 20      12 11         0
//! ┌──────────┬──────────┬──────────┬──────────┬────────────┐
//! │ L0 index │ L1 index │ L2 index │ L3 index │   offset   │
//! │  9 bits  │  9 bits  │  9 bits  │  9 bits  │  12 bits   │
//! └──────────┴──────────┴──────────┴──────────┴────────────┘
//! ```
//!
//! The descriptor format is compact and full of traps. The constants below carry the ones that
//! actually bite. [`Aarch64`] implements [`PageFormat`] by translating the
//! crate's portable [`Flags`] to and from these bits.

use crate::{
    CAP_DEVICE, CAP_GLOBAL, CAP_KERNEL_EXEC, CAP_USER, CAP_USER_EXEC, CAP_WRITE, Flags, PageFormat,
};

/// Bits [1:0] = 0b11: "valid, and a table pointer" at L0-L2, or **"valid, and a page"** at L3. The
/// same two bits mean different things by level: a descriptor is not self-describing, which is why
/// you cannot walk a page table without knowing what level you are at.
const VALID: u64 = 1 << 0;
const TABLE_OR_PAGE: u64 = 1 << 1;

/// **Bit 10, the Access Flag. Forget this and nothing works.** If AF is clear, the first access
/// faults instead of succeeding (it exists for page-replacement policy we do not do). We set it
/// eagerly. Leaving it clear produces a fault that looks nothing like "you forgot a bit".
const AF: u64 = 1 << 10;

/// Bits [9:8], shareability. `0b11` = inner shareable: how far coherency must extend for normal
/// cacheable memory on a multi-core system. Wrong here and caches quietly stop being coherent.
const SH_INNER: u64 = 0b11 << 8;

/// Bits [4:2]: which `MAIR_EL1` slot describes this memory's *type* (one level of indirection so the
/// eight attribute combinations fit in three bits per page).
const fn attr_index(slot: u64) -> u64 {
    (slot & 0b111) << 2
}

/// Bits [7:6], access permission. Read it as: **bit 7 = read-only, bit 6 = userspace may touch it.**
///
/// | AP | EL1 (kernel) | EL0 (user) |
/// |----|--------------|------------|
/// | 00 | read/write   | no access  |
/// | 01 | read/write   | read/write |
/// | 10 | read-only    | no access  |
/// | 11 | read-only    | read-only  |
const AP_RW_EL1: u64 = 0b00 << 6;
const AP_RW_EL0: u64 = 0b01 << 6;
const AP_RO_EL1: u64 = 0b10 << 6;
const AP_RO_EL0: u64 = 0b11 << 6;
const AP_RO: u64 = 1 << 7; // read-only bit
const AP_USER: u64 = 1 << 6; // user-accessible bit

/// **Bit 11, not-global (`nG`).** A TLB entry from an `nG` mapping is tagged with the ASID live at
/// walk time and only matches that ASID (milestone 15). Every user mapping sets it, so a context
/// switch flushes nothing; kernel mappings leave it clear, global on purpose.
const NG: u64 = 1 << 11;

/// Bit 53: Privileged eXecute Never. The kernel may not execute this page.
const PXN: u64 = 1 << 53;

/// Bit 54: Unprivileged eXecute Never. Userspace may not execute this page.
const UXN: u64 = 1 << 54;

/// Physical address bits of a descriptor: [47:12].
const ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

/// Which `MAIR_EL1` slot describes what. Set up in the kernel; the numbers must agree. Kept public
/// because the kernel's `mmu::init` programs `MAIR_EL1` from these and a test cross-checks the UART.
pub mod mair {
    /// Slot 0: device memory, nGnRnE (no gathering, reordering, or early write ack): the strictest
    /// ordering, which MMIO needs. Mapping the UART as normal memory would let the CPU cache and
    /// reorder register writes, which is catastrophic.
    pub const DEVICE: u64 = 0;
    /// Slot 1: normal write-back cacheable memory.
    pub const NORMAL: u64 = 1;

    /// The `MAIR_EL1` value: slot 1 = normal (0xff), slot 0 = device (0x00, so it contributes
    /// nothing to the value but is the default and is what makes MMIO device-typed).
    pub const VALUE: u64 = 0xff << 8;
}

/// The aarch64 four-level descriptor format.
pub struct Aarch64;

impl Aarch64 {
    /// Encode the attribute/permission bits (not the address or valid/type bits) for `flags`.
    const fn attrs(flags: Flags) -> u64 {
        let mut bits = AF;
        if !flags.is_device() {
            bits |= SH_INNER; // shareability is meaningless (and ignored) for device memory
        }
        bits |= attr_index(if flags.is_device() {
            mair::DEVICE
        } else {
            mair::NORMAL
        });
        // Access permission from (user?, writable?).
        bits |= match (flags.is_user_accessible(), flags.is_writable()) {
            (true, true) => AP_RW_EL0,
            (true, false) => AP_RO_EL0,
            (false, true) => AP_RW_EL1,
            (false, false) => AP_RO_EL1,
        };
        if !flags.is_kernel_executable() {
            bits |= PXN;
        }
        if !flags.is_user_executable() {
            bits |= UXN;
        }
        if !flags.is_global() {
            bits |= NG;
        }
        bits
    }
}

impl PageFormat for Aarch64 {
    const LEVELS: usize = 4;
    const SPLIT_SHIFT: u32 = 48;

    fn is_present(entry: u64) -> bool {
        entry & VALID != 0
    }

    fn entry_pa(entry: u64) -> u64 {
        entry & ADDR_MASK
    }

    fn table_entry(pa: u64) -> u64 {
        // A table descriptor: address plus valid/table bits, no attributes. Permissions on an
        // intermediate table would be an additional restriction on everything beneath it; we want
        // the leaf to be the single source of truth.
        (pa & ADDR_MASK) | TABLE_OR_PAGE | VALID
    }

    fn leaf_entry(pa: u64, flags: Flags) -> u64 {
        // At L3, `0b11` means PAGE, not TABLE. Same bits, different meaning by level.
        (pa & ADDR_MASK) | Self::attrs(flags) | TABLE_OR_PAGE | VALID
    }

    fn leaf_flags(entry: u64) -> Flags {
        let mut caps = 0;
        if entry & AP_RO == 0 {
            caps |= CAP_WRITE;
        }
        if entry & AP_USER != 0 {
            caps |= CAP_USER;
        }
        if entry & UXN == 0 {
            caps |= CAP_USER_EXEC;
        }
        if entry & PXN == 0 {
            caps |= CAP_KERNEL_EXEC;
        }
        if entry & NG == 0 {
            caps |= CAP_GLOBAL;
        }
        if (entry >> 2) & 0b111 == mair::DEVICE {
            caps |= CAP_DEVICE;
        }
        Flags::from_caps(caps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Every leaf sets the Access Flag.** If AF is clear, the first access to the page faults
    /// instead of succeeding, and the fault looks nothing like "you forgot a bit", which is the most
    /// common aarch64 paging bug. The bit lives in the format now, so the test does too.
    #[test]
    fn every_leaf_sets_the_access_flag() {
        for flags in [
            Flags::kernel_code(),
            Flags::kernel_rodata(),
            Flags::kernel_data(),
            Flags::device(),
            Flags::user_code(),
            Flags::user_data(),
        ] {
            let leaf = Aarch64::leaf_entry(0x4000_0000, flags);
            assert_ne!(leaf & AF, 0, "AF not set in {flags:?}");
        }
    }

    /// **Device memory encodes to the device MAIR slot.** Mapping MMIO as normal memory would let
    /// the CPU cache and reorder register writes, which is catastrophic (reading a FIFO register has
    /// a side effect).
    #[test]
    fn device_memory_encodes_the_device_mair_slot() {
        let leaf = Aarch64::leaf_entry(0x0900_0000, Flags::device());
        assert_eq!(
            (leaf >> 2) & 0b111,
            mair::DEVICE,
            "MMIO is not device-typed"
        );
    }

    /// **The MAIR value agrees with the slot numbers.** Slot 0 = 0x00 (Device-nGnRnE), slot 1 = 0xff
    /// (Normal WB). The `AttrIndx` in a descriptor is an index into this register; if the two disagree
    /// the UART gets mapped cacheable and the machine behaves like it is haunted.
    #[test]
    fn mair_value_matches_the_slots() {
        assert_eq!((mair::VALUE >> (8 * mair::DEVICE)) & 0xff, 0x00);
        assert_eq!((mair::VALUE >> (8 * mair::NORMAL)) & 0xff, 0xff);
    }

    /// **Both low bits, on both kinds of descriptor.** `is_present` reads bit 0 alone, so a
    /// descriptor that lost bit 1 walks and translates perfectly in software: every test here and in
    /// `tests/mapping.rs` still passes. The hardware disagrees. `0b01` at L3 is a translation fault,
    /// and at L0-L2 it is a *block* descriptor, which maps a 1 GiB or 2 MiB span from bits the walk
    /// meant as a table pointer.
    #[test]
    fn a_descriptor_carries_the_table_or_page_bit_as_well_as_valid() {
        assert_eq!(Aarch64::table_entry(0x4000_0000) & 0b11, 0b11);
        assert_eq!(
            Aarch64::leaf_entry(0x4000_0000, Flags::kernel_data()) & 0b11,
            0b11,
        );
    }

    /// **Normal memory is inner-shareable; device memory is not.** SH is how far hardware coherency
    /// extends: a normal page that loses it gets a core-private view that another core's write never
    /// invalidates, which is a data race the code looks innocent of. Nothing else in the crate
    /// notices, because portable [`Flags`] have no shareability and `leaf_flags` never decodes SH,
    /// so the round-trip below is blind to these bits.
    #[test]
    fn normal_memory_is_inner_shareable_and_device_memory_is_not() {
        let normal = Aarch64::leaf_entry(0x4000_0000, Flags::kernel_data());
        assert_eq!((normal >> 8) & 0b11, 0b11, "normal memory is not shareable");
        let device = Aarch64::leaf_entry(0x0900_0000, Flags::device());
        assert_eq!((device >> 8) & 0b11, 0, "device memory took a shareability");
    }

    /// **Every constructor round-trips through encode/decode**, the property the Kani proof checks
    /// symbolically, here as a fast concrete smoke test.
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
            let leaf = Aarch64::leaf_entry(0x4000_0000, flags);
            assert_eq!(Aarch64::entry_pa(leaf), 0x4000_0000);
            assert_eq!(
                Aarch64::leaf_flags(leaf),
                flags,
                "round-trip failed for {flags:?}"
            );
        }
    }
}

/// Machine-checked proofs of the aarch64 format (DECISIONS §14/§17).
///
/// The walk in `Mapper` is memory-safe and isolating only if the index math is right for every
/// address and the leaf keeps address and permissions apart. These prove exactly that for this
/// format, symbolically. The Sv39 module proves the same properties for its format; the shared walk
/// inherits both. See notes/verification.md.
#[cfg(kani)]
mod verification {
    use super::*;
    use crate::{Half, PAGE_SIZE};

    /// **The walk never indexes past a table.** For every address and level, the index is < 512.
    #[kani::proof]
    fn index_is_always_in_bounds() {
        let va: u64 = kani::any();
        let level: usize = kani::any();
        kani::assume(level < Aarch64::LEVELS);
        assert!(Aarch64::index(va, level) < crate::ENTRIES);
    }

    /// **The four indices and the offset tile the address exactly**, which is what proves the shift
    /// arithmetic: if two levels shared a bit, two addresses could walk to one entry.
    #[kani::proof]
    fn the_indices_and_offset_tile_the_address() {
        let va: u64 = kani::any();
        let reconstructed = ((Aarch64::index(va, 0) as u64) << 39)
            | ((Aarch64::index(va, 1) as u64) << 30)
            | ((Aarch64::index(va, 2) as u64) << 21)
            | ((Aarch64::index(va, 3) as u64) << 12)
            | (va & (PAGE_SIZE - 1));
        assert_eq!(reconstructed, va & 0x0000_ffff_ffff_ffff);
    }

    /// **Distinct pages take distinct paths.** Two page-aligned addresses with the same four indices
    /// are the same page: the arithmetic core of address-space isolation.
    #[kani::proof]
    fn distinct_pages_take_distinct_paths() {
        let a: u64 = kani::any::<u64>() & ADDR_MASK;
        let b: u64 = kani::any::<u64>() & ADDR_MASK;
        kani::assume(
            Aarch64::index(a, 0) == Aarch64::index(b, 0)
                && Aarch64::index(a, 1) == Aarch64::index(b, 1)
                && Aarch64::index(a, 2) == Aarch64::index(b, 2)
                && Aarch64::index(a, 3) == Aarch64::index(b, 3),
        );
        assert_eq!(a, b);
    }

    /// **The two halves are disjoint.** No address belongs to both the low and high half.
    #[kani::proof]
    fn the_two_halves_are_disjoint() {
        let va: u64 = kani::any();
        assert!(!(Aarch64::in_half(Half::Low, va) && Aarch64::in_half(Half::High, va)));
    }

    /// **The user-VA gate admits exactly the aligned low half**, and never the high one.
    #[kani::proof]
    fn the_user_va_gate_admits_only_the_aligned_low_half() {
        let va: u64 = kani::any();
        assert_eq!(
            crate::is_user_page_va::<Aarch64>(va),
            va & 0xfff == 0 && va >> 48 == 0
        );
        if crate::is_user_page_va::<Aarch64>(va) {
            assert!(Aarch64::in_half(Half::Low, va) && !Aarch64::in_half(Half::High, va));
        }
    }

    /// **A leaf keeps the address and the permissions apart, and the permissions round-trip.**
    /// `leaf_entry` writes address + attrs + valid/type; `entry_pa` and `leaf_flags` read the two
    /// halves back exactly. No permission bit can redirect the address, no address bit can grant a
    /// permission, and decoding a freshly-encoded leaf returns the same `Flags`.
    #[kani::proof]
    fn the_leaf_keeps_address_and_permissions_apart() {
        let pa: u64 = kani::any();
        kani::assume(pa & !ADDR_MASK == 0);

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

        let leaf = Aarch64::leaf_entry(pa, flags);
        assert_eq!(Aarch64::entry_pa(leaf), pa);
        assert_eq!(Aarch64::leaf_flags(leaf), flags);
    }
}
