//! **Ask the aarch64 machine what it is, once, at boot.**
//!
//! Milestone 60's other half, and it is three `mrs` instructions. That is the whole difference
//! between the two ISAs: ARM never removed the CPU's self-description, so there is no device tree
//! in this path, no firmware to believe, and nothing to parse. `MIDR_EL1` names the part and the
//! `ID_AA64*` space describes what it can do, architected and mandatory and readable at EL1.
//!
//! The decoding is in [`isa::aarch64`], host-tested against words real parts report. What can only
//! happen on the machine is here: the reads, the record, and the refusal.
//!
//! # One of these was already being read, and that is the point
//!
//! `mmu::init` has always written `TCR_EL1.IPS` from `ID_AA64MMFR0_EL1.PARange`, with a comment
//! saying to read it from the hardware rather than guess. It was right, and it was the only one:
//! the 4 KiB granule the page tables are built on and the ASID width `crates/asid` is built on were
//! both simply assumed. They are the same register.

use aarch64_cpu::registers::{ID_AA64MMFR0_EL1, ID_AA64MMFR2_EL1, MIDR_EL1};
use isa::aarch64::Isa;
use tock_registers::interfaces::Readable;

use crate::sync::{IrqSafeMutex, rank};
use crate::{print, println};

/// The record, written once by [`init`] and read by the boot print, `mmu::init` and the tests.
///
/// `None` until then, so a read before discovery panics naming this file rather than reporting a
/// plausible all-zero machine.
static ISA: IrqSafeMutex<Option<Isa>> = IrqSafeMutex::new(rank::ISA, None);

/// **Discover the machine, and refuse to run on one we cannot.**
///
/// Takes the device-tree pointer and ignores it, so the two architectures' `arch::isa::init` have
/// one signature and `kernel_main` has one call. On RISC-V the tree is the only source there is;
/// here it says nothing about the ISA, because the CPU already does.
///
/// # Panics
///
/// Deliberately, when the machine cannot run this kernel: no 4 KiB granule, or an `ASIDBits`
/// encoding ARM has never defined. Both are unreachable on a conforming part and both are silent if
/// unchecked, which is the combination that makes a boot-time comparison worth its cost.
pub fn init(_dtb_ptr: usize) {
    let cpu = Isa::decode(
        MIDR_EL1.get(),
        ID_AA64MMFR0_EL1.get(),
        ID_AA64MMFR2_EL1.get(),
    );

    let missing = cpu.missing_requirements();
    if missing.any() {
        println!();
        println!("cricker-os cannot run on this machine:");
        if missing.granule_4k {
            println!(
                "  granule     : no 4 KiB stage-1 granule (ID_AA64MMFR0_EL1.TGran4), and every \
                 page table here is 4 KiB"
            );
        }
        if missing.asid_bits {
            println!(
                "  asid        : ID_AA64MMFR0_EL1.ASIDBits is a reserved encoding, so the ASID \
                 width is unknown and crates/asid needs at least 8"
            );
        }
        panic!("required hardware facility is absent");
    }

    *ISA.lock() = Some(cpu);
}

/// The record. Panics if read before [`init`].
pub fn get() -> Isa {
    ISA.lock()
        .expect("arch::isa::get() before arch::isa::init()")
}

/// The boot line: who made this part, and the three numbers the kernel's own configuration rests on.
///
/// Its only caller is the banner in `main.rs`, which the test build and the `bench` boot mode both
/// compile out, so it has no caller in exactly those two configurations. The same shape as
/// `mmu::print_summary` beside it. The RISC-V twin has no such exemption, because that boot prints
/// its ISA line before the branch into the test, shell or bench paths.
#[cfg_attr(any(test, feature = "bench"), allow(dead_code))]
pub fn print_summary() {
    let cpu = get();

    match cpu.implementer_name() {
        Some(name) => print!("  cpu             : {name}"),
        None => print!("  cpu             : implementer {:#04x}", cpu.implementer),
    }
    println!(" part {:#05x} r{}p{}", cpu.part, cpu.variant, cpu.revision);
    println!(
        "                  : {}-bit PA, {}-bit VA available, {}-bit ASIDs, granules {}{}{}",
        cpu.pa_bits(),
        cpu.va_bits,
        cpu.asid_bits,
        if cpu.granules.k4 { "4K " } else { "" },
        if cpu.granules.k16 { "16K " } else { "" },
        if cpu.granules.k64 { "64K" } else { "" },
    );
}

#[cfg(test)]
mod tests {
    //! What the part said, checked on the part.
    //!
    //! The decoding is proved on the host (`crates/isa/tests`). These are the assertions that need
    //! a real boot: that the record was populated, and that what the CPU says about itself agrees
    //! with what the kernel independently configured on the strength of it.

    use super::*;

    /// **Discovery ran, and the part identified itself.** Reaching this test means `init` did not
    /// refuse; this is the other half, that the record holds a machine rather than a default.
    #[test_case]
    fn the_part_identified_itself() {
        let cpu = get();

        assert!(cpu.granules.k4, "we are running on 4 KiB pages");
        assert!(
            cpu.pa_bits() > 0,
            "a reserved PARange would be reported as zero"
        );
        assert!(!cpu.missing_requirements().any());
    }

    /// **The ASID width the part reports is enough for the allocator built on it.**
    ///
    /// The RISC-V twin of this has to *measure* the number, because RISC-V permits any width
    /// including zero and publishes nothing. ARM mandates 8 or 16, so here it is a read. Same
    /// assertion either way, and the same consequence if it failed: `crates/asid` hands out 255
    /// numbers on the stated assumption that hardware can tell them apart.
    #[test_case]
    fn the_asid_width_supports_the_allocator() {
        assert!(get().asid_bits >= 8, "crates/asid assumes at least 8");
    }

    /// **`TCR_EL1.IPS` holds what the part reported, and nothing else.**
    ///
    /// This is the one field the kernel was already reading, and it is now read once into the
    /// record. The test closes the loop: a change that made `mmu::init` write a constant, or read a
    /// different field, would leave a live register disagreeing with the boot line that claims to
    /// describe it.
    #[test_case]
    fn the_configured_physical_address_size_is_the_one_the_part_reported() {
        use aarch64_cpu::registers::TCR_EL1;

        assert_eq!(
            TCR_EL1.read(TCR_EL1::IPS) as u8,
            get().pa_range,
            "TCR_EL1.IPS is ID_AA64MMFR0_EL1.PARange, straight through"
        );
    }
}
