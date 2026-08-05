use super::*;

/// The boot-program entry both architectures' kernels load themselves. (riscv's shell boot loads
/// `system_initializer` instead, measured under that name by the same trust root; `init` is the entry both
/// ISAs have, so it is the one this test can assert on portably.)
const BOOT_PROGRAM: &str = "init";

/// **The initrd in RAM is the initrd this kernel was built against.** The end-to-end build
/// composition check: nothing here is hard-coded, the digest comes out of the kernel's own
/// `.rodata` and the bytes come out of the archive QEMU loaded, and they have to agree. If the
/// build ever writes the manifest after compiling the kernel, or measures the wrong entry, or the
/// archive is repacked without a kernel relink, this fails.
#[test_case]
fn the_boot_program_measures_to_the_compiled_in_trust_root() {
    let bytes = program(BOOT_PROGRAM).expect("no boot program in the initrd archive");
    assert!(
        crate::trust::expected(BOOT_PROGRAM).is_some(),
        "the kernel image carries no measurement for '{BOOT_PROGRAM}': the build's measurement \
         step did not run, and an unmeasured boot would be refused at boot time",
    );
    assert_eq!(
        crate::trust::verify(BOOT_PROGRAM, bytes),
        Ok(()),
        "the boot program in RAM is not the one this kernel image was built against",
    );
}

/// **One flipped bit is refused, and an unmeasured name is refused too.** The tamper is measured
/// by streaming (flip the first byte, then the rest untouched) rather than by copying a
/// 300 KiB ELF, because there is no heap to copy it into; the digest is the same one a real
/// tampered initrd would produce.
#[test_case]
fn a_tampered_boot_program_and_an_unmeasured_name_are_both_refused() {
    let bytes = program(BOOT_PROGRAM).expect("no boot program in the initrd archive");
    let mut h = measured_boot::Sha256::new();
    h.update(&[bytes[0] ^ 1]);
    h.update(&bytes[1..]);
    let tampered = h.finalize();

    assert_eq!(
        measured_boot::verify_digest(crate::trust::TRUST_ROOT, BOOT_PROGRAM, &tampered),
        Err(measured_boot::VerifyError::Mismatch),
        "a boot program with one bit flipped still satisfied the trust root",
    );
    // Fail-closed on the other axis: a program the trust root says nothing about is refused, not
    // waved through. This is what makes an empty or stale trust root safe.
    assert_eq!(
        crate::trust::verify("no-such-program", bytes),
        Err(measured_boot::VerifyError::Unmeasured),
        "the kernel vouched for a program it has no measurement for",
    );
}

// ===========================================================================================
// The second link (milestone 104): init measures what init loads.
//
// The kernel's part is one digest, checked above's way. Init's part is a table in the archive it
// looks every program up in before loading it. These prove the link composes without booting init:
// the digests come out of the archive the running kernel was handed, the trust root comes out of
// this image's own `.rodata`, and the names come from the same places init reads them from.
// ===========================================================================================

/// **The six components init builds the interactive system out of**, mirroring
/// `system_initializer::boot`. Written out here rather than imported because the kernel cannot
/// depend on that crate (it would drag `user_rt`'s EL0 syscall stubs into the kernel), which is the
/// same seam `CHILD_STACK_PAGES` / `SHELL_EXTRA_STACK` already lives on. If a component is added
/// there and not here, this test simply proves less; if one is added here and not packed, it fails.
const BOOT_COMPONENTS: [&str; 6] = [
    "console",
    "input",
    "line_editor",
    "swish",
    "terminal_sink_caretaker",
    "job_undertaker",
];

/// **The table in RAM is the table this kernel was built against.** [`the_boot_program_measures_to_the_compiled_in_trust_root`]
/// one link down, and the reason init's refusals are worth anything: a table an attacker could
/// substitute would let them vouch for whatever they liked.
#[test_case]
fn the_measurement_table_measures_to_the_compiled_in_trust_root() {
    let name = measured_boot::PROGRAM_MEASUREMENTS;
    let bytes = program(name).expect("the initrd archive carries no measurement table");
    assert!(
        crate::trust::expected(name).is_some(),
        "the kernel image carries no measurement for '{name}': init would be handed a table this \
         kernel cannot vouch for, and the boot path refuses that",
    );
    assert_eq!(
        crate::trust::verify(name, bytes),
        Ok(()),
        "the measurement table in RAM is not the one this kernel image was built against",
    );
}

/// **Every program init loads is named in the table, and the bytes agree.** The end-to-end proof of
/// the chain's second link, and the one that would catch the failure that actually threatens it:
/// a program packed into the archive and left out of the table boots fine and is silently
/// unloadable, because `Unmeasured` is a refusal. Nothing here is hard-coded; the expected digests
/// come out of the archive's own table and the bytes out of the archive's own entries.
#[test_case]
fn every_program_init_loads_is_vouched_for_by_the_measurement_table() {
    let table = program(measured_boot::PROGRAM_MEASUREMENTS)
        .and_then(|b| core::str::from_utf8(b).ok())
        .expect("the measurement table is missing or is not text");

    let mut checked = 0;
    for name in BOOT_COMPONENTS {
        let bytes = program(name).unwrap_or_else(|| panic!("the archive has no '{name}'"));
        assert_eq!(
            measured_boot::verify_in_manifest(table, name, bytes),
            Ok(()),
            "init would refuse the boot component '{name}'",
        );
        checked += 1;
    }
    // The spawnable half, taken from the same enum init indexes (`grant_plan::Prog`), so a program
    // added to the prompt is covered here without anyone remembering to add it.
    for id in 0..grant_plan::PROG_COUNT {
        let Some(name) = grant_plan::Prog::from_id(id as u64).map(|p| p.name()) else {
            continue;
        };
        let Some(bytes) = program(name) else { continue };
        assert_eq!(
            measured_boot::verify_in_manifest(table, name, bytes),
            Ok(()),
            "init would refuse the spawnable program '{name}'",
        );
        checked += 1;
    }
    assert!(
        checked >= BOOT_COMPONENTS.len(),
        "the table check ran over nothing",
    );
}

/// **The table refuses a tampered program and one it does not name.** The same two axes the trust
/// root is proved on, because the whole claim is that the second link fails closed the way the
/// first one does. The tamper streams (flip the first byte, then the rest) rather than copying a
/// program into a heap the kernel does not have.
#[test_case]
fn the_measurement_table_refuses_a_tampered_program_and_an_unnamed_one() {
    let table = program(measured_boot::PROGRAM_MEASUREMENTS)
        .and_then(|b| core::str::from_utf8(b).ok())
        .expect("the measurement table is missing or is not text");
    let bytes = program("console").expect("the archive has no 'console'");

    let mut h = measured_boot::Sha256::new();
    h.update(&[bytes[0] ^ 1]);
    h.update(&bytes[1..]);
    let tampered = h.finalize();
    assert_eq!(
        measured_boot::expected_in_manifest(table, "console"),
        Some(measured_boot::sha256(bytes)),
        "the table's entry for 'console' is not the console in this archive",
    );
    assert_ne!(
        measured_boot::expected_in_manifest(table, "console"),
        Some(tampered),
        "a console with one bit flipped still satisfied the table",
    );
    assert_eq!(
        measured_boot::verify_in_manifest(table, "no-such-program", bytes),
        Err(measured_boot::VerifyError::Unmeasured),
        "the table vouched for a program it does not name",
    );
    // And the table does not vouch for itself, which is not an oversight: an entry cannot carry its
    // own digest, so the kernel's trust root is what covers it (the test above).
    assert_eq!(
        measured_boot::expected_in_manifest(table, measured_boot::PROGRAM_MEASUREMENTS),
        None,
        "the table claims to measure itself",
    );
}
