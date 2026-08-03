//! The two spellings of "what extensions does this hart have", and the corners of the older one.
//!
//! `riscv,isa-extensions` is a stringlist and needs no cleverness. The deprecated `riscv,isa` is a
//! small grammar with one genuine trap in it, and the trap is on the board this milestone exists
//! for: `rv64gc` and `rv64imafdc_zicsr_zifencei` name the same machine.

use isa::riscv64::*;

#[test]
fn extension_lists_are_nul_separated() {
    let value = b"i\0m\0a\0c\0zicsr\0zifencei\0";
    let e = parse_extension_list(value);

    assert!(e.contains(I.union(M).union(A).union(C).union(ZICSR).union(ZIFENCEI)));
    assert!(!e.contains(F));
}

/// Names outside [`TABLE`] are ignored rather than being an error. QEMU's default `rv64` declares
/// about thirty of them; a kernel that failed on an extension it had never heard of would be a
/// kernel that stops booting every time the ecosystem ratifies something.
#[test]
fn unknown_extension_names_are_ignored() {
    let e = parse_extension_list(b"i\0m\0a\0c\0zicond\0smaia\0ssaia\0xtheadba\0");

    assert!(e.contains(I.union(M).union(A).union(C)));
    assert_eq!(e, parse_extension_list(b"i\0m\0a\0c\0"));
}

/// **`g` is an abbreviation, not an extension.** A tree that says `rv64gc` is saying `imafdc` plus
/// `zicsr` and `zifencei`, and a parser that looks `g` up in a table finds nothing and silently
/// reports a machine with no multiplier.
#[test]
fn g_expands() {
    let g = parse_isa_string(b"rv64gc");
    let spelled_out = parse_isa_string(b"rv64imafdc_zicsr_zifencei");

    assert_eq!(g, spelled_out);
    assert!(g.contains(M), "the failure this test exists for");
    assert!(g.contains(D));
    assert!(g.contains(ZIFENCEI));
}

/// The shape a pre-2019 vendor tree has: single letters, no separators, nothing after them. This is
/// the string the VisionFive 2's own firmware is expected to carry.
#[test]
fn a_bare_letter_run_parses() {
    let e = parse_isa_string(b"rv64imafdc");

    assert!(e.contains(I.union(M).union(A).union(C).union(F).union(D)));
    assert!(
        !e.contains(ZICSR),
        "and it says nothing about zicsr, which is why zicsr is not required"
    );
    assert!(REQUIRED.contains(M.union(A).union(C)));
    assert!(
        !REQUIRED.contains(ZICSR),
        "see the module BUGS in riscv64.rs"
    );
}

#[test]
fn the_base_prefix_is_decoded_and_skipped() {
    assert_eq!(Base::from_isa_string(b"rv64imafdc"), Base::Rv64);
    assert_eq!(Base::from_isa_string(b"rv32imac"), Base::Rv32);
    assert_eq!(Base::from_isa_string(b"rv128i"), Base::Rv128);
    assert_eq!(Base::from_isa_string(b"nonsense"), Base::Unknown);

    // The `c` in `rv32imac` is the compressed extension, and the `3`/`2` of the prefix must not be
    // mistaken for anything. Nothing in the letter run comes from the prefix.
    assert_eq!(parse_isa_string(b"rv32imac"), parse_isa_string(b"rv64imac"));
}

#[test]
fn mmu_type_tolerates_a_missing_vendor_prefix() {
    assert_eq!(MmuType::from_property(b"riscv,sv39\0"), MmuType::Sv39);
    assert_eq!(MmuType::from_property(b"sv48\0"), MmuType::Sv48);
    assert_eq!(MmuType::from_property(b"riscv,none\0"), MmuType::None);
    assert_eq!(MmuType::from_property(b"riscv,sv57\0"), MmuType::Sv57);
    assert_eq!(
        MmuType::from_property(b"something-else\0"),
        MmuType::Unknown
    );
}

/// The ordering exists for exactly one comparison, so state what it has to mean: an undeclared MMU
/// must not read as "wide enough", and `none` must not read as "wider than sv32".
#[test]
fn mmu_types_order_by_how_much_address_space_they_reach() {
    assert!(MmuType::Unknown < MmuType::Sv39);
    assert!(MmuType::None < MmuType::Sv32);
    assert!(MmuType::Sv39 < MmuType::Sv48);
    assert!(MmuType::Sv48 < MmuType::Sv57);
}

/// Every row of the table is reachable by the name a device tree would spell, in both properties.
/// A row whose name is misspelled is a fact the kernel would report as absent forever.
#[test]
fn every_row_round_trips_through_both_properties() {
    for row in &TABLE {
        let mut listed = row.name.as_bytes().to_vec();
        listed.push(0);
        assert_eq!(
            parse_extension_list(&listed),
            row.bit,
            "row {:?} is unreachable from riscv,isa-extensions",
            row.name
        );

        // The legacy string spells single letters in the run and multi-letter names after a `_`.
        let string = if row.name.len() == 1 {
            format!("rv64{}", row.name)
        } else {
            format!("rv64i_{}", row.name)
        };
        assert!(
            parse_isa_string(string.as_bytes()).contains(row.bit),
            "row {:?} is unreachable from riscv,isa",
            row.name
        );
    }
}

/// **The extension ids, against the specification, written out in hex once.**
///
/// The crate derives these from their tags so that no hand-written hex can be wrong; this is the
/// one place the hex appears, so a reader with the SBI specification open can check all four
/// without reading a `const fn`. Three of them are also spelled at the kernel's `ecall` sites,
/// which now take them from here.
///
/// Two of the four tags are not the extension's human name, which is the whole reason this is
/// worth a test: `IPI` is assigned `sPI` and `RFENCE` is assigned `RFNC`.
#[test]
fn sbi_extension_ids_match_the_specification() {
    assert_eq!(EID_TIME, 0x5449_4D45, "\"TIME\"");
    assert_eq!(EID_IPI, 0x0073_5049, "\"sPI\", not \"IPI\"");
    assert_eq!(EID_RFENCE, 0x5246_4E43, "\"RFNC\", not \"RFENCE\"");
    assert_eq!(EID_HSM, 0x0048_534D, "\"HSM\"");
    assert_eq!(EID_BASE, 0x10, "the base extension is a number, not a tag");

    for row in &SBI_TABLE {
        assert!(row.eid != 0, "{} has no extension id", row.name);
        assert!(!row.why.is_empty(), "{} has no call site", row.name);
    }
}
