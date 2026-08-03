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
