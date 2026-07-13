//! Build real aarch64 page tables, in real memory, on the host.
//!
//! The trick: a `Box<PageTable>` is 4 KiB-aligned (the type says so) and has a real
//! address. So we hand those addresses to the mapper as "physical" frames, and
//! `phys_to_ptr` is the identity cast. **The pointer arithmetic is bit-for-bit what the
//! kernel does.** We are testing the actual code path, not a model of it.
//!
//! Runs in milliseconds. No emulator, no hardware, no MMU.

use paging::{Flags, Half, MapError, Mapper, PAGE_SIZE, PageTable, index, mair};
use std::cell::Cell;

/// A pretend physical frame allocator backed by the host heap.
///
/// Leaks. That's fine, and deliberate: the tables must outlive the mapper, and a test
/// process is about to exit anyway.
fn frame_source(budget: &Cell<usize>) -> impl FnMut() -> Option<u64> + '_ {
    move || {
        if budget.get() == 0 {
            return None;
        }
        budget.set(budget.get() - 1);
        let table = Box::new(PageTable::new());
        Some(Box::into_raw(table) as u64)
    }
}

fn phys_to_ptr(pa: u64) -> *mut PageTable {
    pa as *mut PageTable
}

fn mapper_in(
    half: Half,
    budget: &Cell<usize>,
) -> Mapper<impl FnMut() -> Option<u64> + '_, fn(u64) -> *mut PageTable> {
    let root = Box::into_raw(Box::new(PageTable::new())) as u64;
    // SAFETY: `root` is a fresh, zeroed, 4 KiB-aligned table, and `phys_to_ptr` is the
    // identity, which is correct because these "physical" addresses ARE host addresses.
    unsafe {
        Mapper::new(
            root,
            half,
            frame_source(budget),
            phys_to_ptr as fn(u64) -> *mut PageTable,
        )
    }
}

fn mapper(budget: &Cell<usize>) -> Mapper<impl FnMut() -> Option<u64> + '_, fn(u64) -> *mut PageTable> {
    mapper_in(Half::Low, budget)
}

#[test]
fn index_slices_the_address_correctly() {
    // Each level takes a 9-bit slice. L0 = 47:39, L1 = 38:30, L2 = 29:21, L3 = 20:12.
    let va = (1u64 << 39) | (2 << 30) | (3 << 21) | (4 << 12) | 0xabc;

    assert_eq!(index(va, 0), 1);
    assert_eq!(index(va, 1), 2);
    assert_eq!(index(va, 2), 3);
    assert_eq!(index(va, 3), 4);
}

#[test]
fn index_wraps_at_512() {
    // 9 bits. Anything above bit 47 belongs to the sign-extension, not to L0.
    assert_eq!(index(0x1ff << 39, 0), 511);
    assert_eq!(index(0xfff << 12, 3), 511);
}

#[test]
fn map_then_translate_round_trips() {
    let budget = Cell::new(16);
    let mut m = mapper(&budget);

    m.map(0x4008_0000, 0x4008_0000, Flags::kernel_code()).unwrap();

    let (pa, flags) = m.translate(0x4008_0000).expect("should be mapped");
    assert_eq!(pa, 0x4008_0000);
    assert_eq!(flags, Flags::kernel_code());
}

#[test]
fn translate_carries_the_offset_within_the_page() {
    let budget = Cell::new(16);
    let mut m = mapper(&budget);

    m.map(0x1000, 0x4000_0000, Flags::kernel_data()).unwrap();

    // The low 12 bits never go through translation. They are the offset.
    let (pa, _) = m.translate(0x1abc).unwrap();
    assert_eq!(pa, 0x4000_0abc);
}

#[test]
fn unmapped_addresses_translate_to_nothing() {
    let budget = Cell::new(16);
    let mut m = mapper(&budget);
    m.map(0x1000, 0x4000_0000, Flags::kernel_data()).unwrap();

    assert_eq!(m.translate(0x2000), None);
    assert_eq!(m.translate(0xffff_0000_0000_0000), None);
}

#[test]
fn a_virtual_address_can_differ_from_its_physical_one() {
    // The entire point of the exercise, and what milestone 4 step 4 depends on.
    let budget = Cell::new(16);
    let mut m = mapper(&budget);

    m.map(0x1000, 0x4008_0000, Flags::kernel_code()).unwrap();

    assert_eq!(m.translate(0x1000).unwrap().0, 0x4008_0000);
    assert_eq!(
        m.translate(0x4008_0000),
        None,
        "the physical address itself is not mapped; only the VA we chose is"
    );
}

// --- the halves: the thing a failing test taught us ---

#[test]
fn the_top_16_bits_are_not_translated_they_choose_the_TABLE() {
    // This is the crux of how a higher-half kernel works, and it is not what you'd guess.
    //
    // Bits 63:48 are NOT part of any index. `index()` reads bits 47:12 and nothing else. So
    // within a single table, these two addresses are THE SAME ENTRY:
    let high = 0xffff_0000_4008_0000u64;
    let low = 0x0000_0000_4008_0000u64;

    for level in 0..4 {
        assert_eq!(
            index(high, level),
            index(low, level),
            "level {level} indices differ, but they must not"
        );
    }

    // Which means the kernel does not live in the high half because high addresses index
    // somewhere else. It lives there because TTBR1 IS A DIFFERENT SET OF TABLES, and the
    // hardware picks between TTBR0 and TTBR1 using exactly those untranslated top bits.
    assert!(Half::High.contains(high));
    assert!(Half::Low.contains(low));
    assert!(!Half::Low.contains(high));
    assert!(!Half::High.contains(low));
}

#[test]
fn mapping_into_the_wrong_half_is_refused() {
    // Without this check, mapping a kernel address into the userspace tables silently
    // builds a mapping the CPU will never consult, because it would pick TTBR1 for that
    // address and never look at this table at all. You would then chase the ghost for
    // hours.
    let budget = Cell::new(16);

    let mut low = mapper_in(Half::Low, &budget);
    assert_eq!(
        low.map(0xffff_0000_0000_0000, 0x1000, Flags::kernel_data()),
        Err(MapError::WrongHalf)
    );

    let mut high = mapper_in(Half::High, &budget);
    assert_eq!(
        high.map(0x1000, 0x1000, Flags::kernel_data()),
        Err(MapError::WrongHalf)
    );
}

#[test]
fn non_canonical_addresses_belong_to_neither_half() {
    // Top bits neither all-zero nor all-one. There is no memory there and there never can
    // be: the hardware faults before it consults any table.
    let junk = 0x0001_0000_0000_0000u64;
    assert!(!Half::Low.contains(junk));
    assert!(!Half::High.contains(junk));
}

#[test]
fn the_high_half_maps_normally_once_you_are_in_it() {
    let budget = Cell::new(16);
    let mut m = mapper_in(Half::High, &budget);

    let va = 0xffff_0000_4008_0000;
    m.map(va, 0x4008_0000, Flags::kernel_code()).unwrap();

    assert_eq!(m.translate(va).unwrap().0, 0x4008_0000);
}

#[test]
fn nearby_pages_share_their_intermediate_tables() {
    // Two pages in the same 2 MiB region differ only in their L3 index, so the walk should
    // create L1/L2/L3 once and reuse them. If it doesn't, we burn a frame per page and run
    // out of memory mapping a kernel.
    let budget = Cell::new(3); // exactly enough for ONE chain of L1+L2+L3
    let mut m = mapper(&budget);

    m.map(0x4000_0000, 0x4000_0000, Flags::kernel_data()).unwrap();
    assert_eq!(budget.get(), 0, "first mapping should consume L1+L2+L3");

    // The next page needs no new tables at all.
    m.map(0x4000_1000, 0x4000_1000, Flags::kernel_data())
        .expect("should reuse the existing tables");
    assert_eq!(budget.get(), 0);
}

#[test]
fn running_out_of_frames_is_an_error_not_a_panic() {
    let budget = Cell::new(0);
    let mut m = mapper(&budget);

    assert_eq!(
        m.map(0x1000, 0x1000, Flags::kernel_data()),
        Err(MapError::OutOfFrames)
    );
}

#[test]
fn misaligned_addresses_are_rejected() {
    let budget = Cell::new(16);
    let mut m = mapper(&budget);

    assert_eq!(
        m.map(0x1001, 0x1000, Flags::kernel_data()),
        Err(MapError::Misaligned)
    );
    assert_eq!(
        m.map(0x1000, 0x1001, Flags::kernel_data()),
        Err(MapError::Misaligned)
    );
}

#[test]
fn mapping_over_an_existing_mapping_is_an_error() {
    // Silently overwriting is how you lose a page and never find out: the old physical
    // frame is still marked used by the allocator, and nothing references it any more.
    let budget = Cell::new(16);
    let mut m = mapper(&budget);

    m.map(0x1000, 0x4000_0000, Flags::kernel_data()).unwrap();
    assert_eq!(
        m.map(0x1000, 0x5000_0000, Flags::kernel_data()),
        Err(MapError::AlreadyMapped)
    );
}

#[test]
fn map_range_maps_every_page() {
    let budget = Cell::new(16);
    let mut m = mapper(&budget);

    m.map_range(0x4000_0000, 0x8000_0000, 4, Flags::kernel_data())
        .unwrap();

    for i in 0..4u64 {
        let (pa, _) = m.translate(0x4000_0000 + i * PAGE_SIZE).unwrap();
        assert_eq!(pa, 0x8000_0000 + i * PAGE_SIZE);
    }
    assert_eq!(m.translate(0x4000_0000 + 4 * PAGE_SIZE), None);
}

// --- the bits that will actually bite ---

#[test]
fn every_mapping_sets_the_access_flag() {
    // Bit 10. If AF is clear, the FIRST access to the page raises an Access Flag fault
    // instead of succeeding. The resulting fault looks nothing like "you forgot a bit,"
    // which is why this is the most common aarch64 paging bug there is.
    const AF: u64 = 1 << 10;

    for flags in [
        Flags::kernel_code(),
        Flags::kernel_rodata(),
        Flags::kernel_data(),
        Flags::device(),
        Flags::user_code(),
        Flags::user_data(),
    ] {
        assert_ne!(flags.bits() & AF, 0, "AF not set in {flags:?}");
    }
}

#[test]
fn nothing_is_both_writable_and_executable() {
    // W^X. A page that is both writable and executable is how a buffer overflow becomes
    // code execution. There is deliberately no constructor that produces one, and this
    // test exists so that adding one is a build failure rather than a security hole.
    for flags in [
        Flags::kernel_code(),
        Flags::kernel_rodata(),
        Flags::kernel_data(),
        Flags::device(),
        Flags::user_code(),
        Flags::user_data(),
    ] {
        let executable = flags.is_kernel_executable() || flags.is_user_executable();
        assert!(
            !(flags.is_writable() && executable),
            "{flags:?} is writable AND executable"
        );
    }
}

#[test]
fn kernel_code_is_executable_by_the_kernel_and_nobody_else() {
    let f = Flags::kernel_code();
    assert!(f.is_kernel_executable());
    assert!(!f.is_user_executable());
    assert!(!f.is_writable());
    assert!(!f.is_user_accessible());
}

#[test]
fn user_code_is_never_executable_by_the_kernel() {
    // PXN is not paranoia. Without it, a bug that jumps the kernel into a user page
    // executes USER-CONTROLLED INSTRUCTIONS AT EL1. Total compromise, and the defence is
    // one bit.
    let f = Flags::user_code();
    assert!(f.is_user_executable());
    assert!(!f.is_kernel_executable(), "PXN is not set on user code");
    assert!(f.is_user_accessible());
}

#[test]
fn device_memory_is_typed_as_device_and_is_never_executable() {
    // Mapping MMIO as *normal* memory lets the CPU cache it, reorder writes to it, merge
    // two writes into one, and speculatively read it. Every one of those is catastrophic
    // for a device, because reading a FIFO register HAS A SIDE EFFECT.
    let f = Flags::device();

    let attr_slot = (f.bits() >> 2) & 0b111;
    assert_eq!(attr_slot, mair::DEVICE, "MMIO is not typed as device memory");

    assert!(!f.is_kernel_executable());
    assert!(!f.is_user_executable());
    assert!(f.is_writable(), "we do need to write to the UART");
}

#[test]
fn mair_value_matches_the_slots() {
    // Slot 0 = 0x00 (Device-nGnRnE), slot 1 = 0xff (Normal WB). The descriptor's AttrIndx
    // is an index INTO this register. If the two ever disagree, the UART gets mapped as
    // cacheable normal memory and the machine behaves like it is haunted.
    assert_eq!((mair::VALUE >> (8 * mair::DEVICE)) & 0xff, 0x00);
    assert_eq!((mair::VALUE >> (8 * mair::NORMAL)) & 0xff, 0xff);
}
