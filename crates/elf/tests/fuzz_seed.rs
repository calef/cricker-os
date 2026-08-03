//! The committed fuzz seed must still be a valid ELF.
//!
//! **A seed that no longer parses is not a seed.** `fuzz/fuzz_targets/elf_parse` returns immediately
//! on anything `Elf::parse` rejects, so the moment this file stops being accepted, the fuzzer is
//! starting from an empty corpus and has to rediscover five constants and a program header table
//! before it reaches a single interesting branch. Nothing about the fuzz run would say so: it would
//! report the same "no crashes" it reports when it is working.
//!
//! That is the silent degradation this test exists to make loud, and it costs a millisecond. The
//! other three targets seed from fixtures that already have tests of their own
//! (`crates/dtb/tests/qemu_aarch64_virt.rs`, `crates/gpt/tests/real_disks.rs`); this seed had none, because
//! it has no other reason to exist.
//!
//! Regenerate it with the script in notes/fuzzing.md if a deliberate tightening of `parse` makes it
//! invalid.

use elf::Elf;

/// The one seed under `fuzz/seeds/`. Everything else the fuzz targets start from is a real fixture
/// already committed for another purpose.
const SEED: &[u8] = include_bytes!("../../../fuzz/seeds/elf_parse/minimal_rx.elf");

#[test]
fn the_fuzz_seed_is_a_valid_executable() {
    let elf = Elf::parse(SEED).expect("the seed must parse, or the fuzzer starts from nothing");

    assert_eq!(elf.entry(), 0x4000_0000);

    let segments: Vec<_> = elf.segments().collect();
    assert_eq!(
        segments.len(),
        1,
        "one PT_LOAD, which is the point of minimal"
    );

    // Read and execute, not write: the seed has to get past the W^X refusal as well as the header
    // checks, or it teaches the fuzzer nothing about the paths after validation.
    let seg = segments[0];
    assert!(seg.is_readable());
    assert!(seg.is_executable());
    assert!(!seg.is_writable());
    assert!(
        seg.vaddr <= elf.entry() && elf.entry() < seg.vaddr + seg.memsz,
        "the entry point must land inside the executable segment"
    );
}

/// **The machine number is the one thing about this seed that is host-dependent**, so it is stated
/// here rather than left to be discovered on a riscv64 developer machine.
///
/// `elf`'s expected machine is a compile-time constant: the riscv64 build accepts riscv ELFs and
/// every other build, including both hosts this project is developed and CI'd on, accepts aarch64.
/// The seed carries `EM_AARCH64 = 183`. On a riscv64 host it would be rejected as `WrongMachine`,
/// and the fuzz corpus would quietly be empty.
#[test]
fn the_seed_carries_this_builds_machine() {
    assert_eq!(
        u16::from_le_bytes([SEED[18], SEED[19]]),
        elf::NATIVE_MACHINE,
        "the seed's e_machine must match what this build accepts (see notes/fuzzing.md)"
    );
}
