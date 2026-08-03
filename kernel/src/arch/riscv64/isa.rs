//! **Ask the RISC-V machine what it is, once, at boot.**
//!
//! Milestone 60. The decoding is all in [`isa::riscv64`], which is host-testable; what can only
//! happen on the machine is here: reading the device tree firmware handed us, asking that firmware
//! what it implements over SBI, and refusing to continue if the answer is a machine this kernel
//! cannot run on.
//!
//! Three sources, in the order they arrive:
//!
//! 1. **The device tree** ([`init`]), which is firmware's claim about the silicon.
//! 2. **SBI** ([`init`] again), which is firmware's claim about itself. `sbi_probe_extension` is
//!    the only way to know whether the four `ecall`s this kernel makes unconditionally will do
//!    anything at all.
//! 3. **The `satp.ASID` probe** ([`record_asid_bits`]), which is a measurement and arrives later,
//!    because it cannot run until `mmu::init` has installed the real page tables.
//!
//! # Where this is called from, and why in that order
//!
//! `kernel_main` runs [`init`] after `memory::init` (which needs the device tree first) and before
//! `mmu::init` (which is what would build tables a refused machine cannot walk). `mmu::init` then
//! calls [`record_asid_bits`] after its probe, and the boot prints [`print_summary`] once paging is
//! on, before any of the test, shell or bench branches, so every boot mode reports the machine.

use core::arch::asm;

use isa::riscv64::{EID_BASE, Isa, SBI_TABLE, Sbi, SbiExtensions};

use crate::sync::{IrqSafeMutex, rank};
use crate::{print, println};

/// The record, written once by [`init`] and read by the boot print and the tests.
///
/// `None` until then, so a read before discovery is a panic naming this file rather than a
/// plausible all-zero machine. The same discipline as `mmu::asid_bits`.
static ISA: IrqSafeMutex<Option<Isa>> = IrqSafeMutex::new(rank::ISA, None);

/// **Discover the machine, and refuse to run on one we cannot.**
///
/// `dtb_ptr` is the physical device-tree address firmware left in `a1`, the same pointer
/// `memory::init` reads, named through the direct map.
///
/// # Panics
///
/// Deliberately, when the machine is missing something the kernel needs. DECISIONS's truthfulness
/// habit applied to hardware: a kernel that silently assumed Sv39 on an Sv32 machine, or called
/// SBI RFENCE on firmware without it, would run degraded and report success. The panic prints what
/// is missing and stops. **Silence is not a failure**: a tree that does not describe its ISA, or
/// firmware too old to be asked, is reported as unknown and the boot continues, because the kernel
/// is demonstrably executing by the time this runs.
pub fn init(dtb_ptr: usize) {
    // SAFETY: the same pointer `memory::init` has already validated the magic of, named through the
    // boot table's direct map because we are running virtual.
    let dt = unsafe { dtb::Dtb::from_ptr(super::mmu::phys_to_virt(dtb_ptr as u64) as *const u8) }
        .expect("device tree is unreadable");

    let mut cpu = Isa::from_device_tree(&dt).expect("cannot read the CPU nodes");
    cpu.sbi = probe_sbi();

    let missing = cpu.missing_requirements();
    if missing.any() {
        println!();
        println!("cricker-os cannot run on this machine:");
        if missing.base {
            println!(
                "  base ISA    : the device tree declares {}, not rv64i",
                cpu.base.name()
            );
        }
        for row in &isa::riscv64::TABLE {
            if missing.extensions.contains(row.bit) {
                println!("  extension   : {} is absent ({})", row.name, row.why);
            }
        }
        if missing.mmu {
            println!(
                "  mmu         : {} cannot hold the kernel's Sv39 high half",
                cpu.mmu.name()
            );
        }
        for row in &SBI_TABLE {
            if missing.sbi.contains(row.bit) {
                println!("  sbi         : {} is absent ({})", row.name, row.why);
            }
        }
        panic!("required hardware or firmware facility is absent");
    }

    *ISA.lock() = Some(cpu);
}

/// The record. Panics if read before [`init`].
pub fn get() -> Isa {
    ISA.lock()
        .expect("arch::isa::get() before arch::isa::init()")
}

/// Record the measured `satp.ASID` width. Called by `mmu::init` right after its probe, which is the
/// earliest moment the number exists: the probe writes the live `satp`, so it needs the real kernel
/// tables installed first.
pub fn record_asid_bits(bits: usize) {
    if let Some(cpu) = ISA.lock().as_mut() {
        cpu.asid_bits = bits.min(u8::MAX as usize) as u8;
    }
}

/// The boot line. Two of them: what the silicon is, and what the firmware under it is.
pub fn print_summary() {
    let cpu = get();

    print!("  isa         : {}", cpu.base.name());
    for row in &isa::riscv64::TABLE {
        // `i` is skipped: the base already said it, and "rv64i i m a c" reads like a stutter.
        if cpu.common.contains(row.bit) && row.bit != isa::riscv64::I {
            print!(" {}", row.name);
        }
    }
    if cpu.described == 0 {
        print!(" (the device tree does not describe this machine's ISA)");
    }
    println!();

    print!(
        "              : {} hart(s), mmu {} declared and sv39 in use, satp.ASID {} bits measured",
        cpu.harts,
        cpu.mmu.name(),
        cpu.asid_bits,
    );
    if cpu.heterogeneous() {
        // The union minus the intersection: what some harts have and others do not. This is the
        // only line that would tell you the machine is not uniform, and on a JH7110 it is the
        // difference between "no FPU" and "no FPU on one core".
        print!("; harts differ (");
        for row in &isa::riscv64::TABLE {
            if cpu.any.contains(row.bit) && !cpu.common.contains(row.bit) {
                print!("{} ", row.name);
            }
        }
        print!("on some but not all)");
    }
    if cpu.legacy_isa_string {
        print!("; read from the deprecated riscv,isa");
    }
    println!();

    print!("  firmware    : ");
    if !cpu.sbi.answered() {
        println!("SBI base extension did not answer, so nothing here is verified");
        return;
    }
    match cpu.sbi.impl_name() {
        Some(name) => print!("{name}"),
        None => print!("implementation {}", cpu.sbi.impl_id),
    }
    print!(
        " {:#x}, SBI {}.{},",
        cpu.sbi.impl_version, cpu.sbi.spec_major, cpu.sbi.spec_minor,
    );
    for row in &SBI_TABLE {
        if cpu.sbi.extensions.contains(row.bit) {
            print!(" {}", row.name);
        }
    }
    println!();
}

/// Ask the firmware what it is and what it implements.
///
/// Everything here goes through the SBI **base** extension, which is itself the thing that might be
/// absent: SBI v0.1 predates it. That case is not an error, it is an unanswered question, and
/// `Sbi::answered` is how the rest of the kernel tells the two apart.
fn probe_sbi() -> Sbi {
    const GET_SPEC_VERSION: usize = 0;
    const GET_IMPL_ID: usize = 1;
    const GET_IMPL_VERSION: usize = 2;
    const PROBE_EXTENSION: usize = 3;

    let version = sbi_call(EID_BASE, GET_SPEC_VERSION, 0);
    if version == 0 {
        return Sbi::default();
    }

    let mut extensions = SbiExtensions::NONE;
    for row in &SBI_TABLE {
        // `probe_extension` returns zero for absent and an extension-defined nonzero for present.
        if sbi_call(EID_BASE, PROBE_EXTENSION, row.eid) != 0 {
            extensions = extensions.union(row.bit);
        }
    }

    Sbi {
        // The minor number is the low **24** bits and the major is the seven above it, which is not
        // the split anyone guesses. The first draft here read 16 and 16, so QEMU's `0x0300_0000`
        // (SBI 3.0) decoded as 0.0, `Sbi::answered` said no, and the boot line reported firmware
        // that had answered perfectly well as silent. Found by printing the raw word.
        spec_major: ((version >> 24) & 0x7f) as u8,
        spec_minor: (version & 0x00ff_ffff) as u32,
        impl_id: sbi_call(EID_BASE, GET_IMPL_ID, 0) as u32,
        impl_version: sbi_call(EID_BASE, GET_IMPL_VERSION, 0) as u32,
        extensions,
    }
}

/// One SBI call, returning the value in `a1` and discarding the error in `a0`.
///
/// Discarding the error is right for exactly these four calls and would be wrong anywhere else: the
/// base extension's getters cannot fail, and `probe_extension` reports absence in the **value**
/// (zero) rather than as an error. A firmware that does not implement the base extension at all
/// returns an error and leaves `a1` alone, which is why [`probe_sbi`] treats a zero spec version as
/// "did not answer" rather than trusting anything after it.
fn sbi_call(eid: usize, fid: usize, arg0: usize) -> usize {
    let value: usize;
    // SAFETY: an SBI call into M-mode firmware. a7 = extension, a6 = function, a0 = the one
    // argument any of these takes. The firmware writes a0 (error) and a1 (value) and touches
    // nothing else.
    unsafe {
        asm!(
            "ecall",
            in("a7") eid,
            in("a6") fid,
            inout("a0") arg0 => _,
            lateout("a1") value,
            options(nostack),
        );
    }
    value
}

#[cfg(test)]
mod tests {
    //! What the machine said, checked on the machine.
    //!
    //! The parsing is proved on the host (`crates/isa/tests`), so these are the assertions that
    //! need a real boot: that discovery ran at all, that the two sources agree with what the kernel
    //! independently knows, and that the measurement did not come back as a plausible zero.

    use super::*;

    /// **Discovery ran, and it found a machine that can run us.** The requirement check is what
    /// `init` would have panicked on, so reaching this test at all is half the assertion; the
    /// other half is that the record is populated rather than defaulted.
    #[test_case]
    fn the_machine_described_itself() {
        let cpu = get();

        assert!(
            cpu.harts >= 1,
            "a machine with no CPU node is not running this"
        );
        assert_eq!(
            cpu.described, cpu.harts,
            "QEMU virt describes every hart it has"
        );
        assert_eq!(cpu.base, isa::riscv64::Base::Rv64);
        assert!(!cpu.missing_requirements().any());
    }

    /// **Every hart that came online is one the device tree described.**
    ///
    /// Two independent answers to overlapping questions: `cpu@` nodes in the tree, and harts that
    /// checked in through SBI HSM. Not an equality, deliberately: the kernel starts at most
    /// `cpu::MAX_CPUS` of them, so a machine with more cores than that is a legitimate
    /// configuration in which the tree honestly describes harts nobody booted. The direction that
    /// would be a bug is the other one, a hart running on a machine the tree never mentioned.
    #[test_case]
    fn every_online_hart_is_one_the_tree_described() {
        assert!(
            get().harts as usize >= crate::smp::online_count(),
            "more harts online than the device tree describes"
        );
    }

    /// **The machine offers more address space than the kernel takes.**
    ///
    /// QEMU `virt` declares `mmu-type = "riscv,sv57"` and we run Sv39. Recorded as a test rather
    /// than only a comment because it is the concrete gap between what the hardware is and what the
    /// kernel assumes, and because the day this stops being true (a board declaring exactly Sv39)
    /// is the day the assumption stops being free.
    #[test_case]
    fn the_declared_mmu_is_wide_enough_and_then_some() {
        let mmu = get().mmu;
        assert!(
            mmu >= isa::riscv64::MmuType::Sv39,
            "we are running Sv39 on it"
        );
        assert_eq!(
            mmu,
            isa::riscv64::MmuType::Sv57,
            "and QEMU virt offers two levels more"
        );
    }

    /// **The measurement made it into the record**, and agrees with `mmu::asid_bits`.
    ///
    /// The record's copy is written by `mmu::init` after the probe, so a reordering of the boot
    /// that moved discovery after paging (or the probe before it) would leave a zero here while
    /// every other test still passed.
    #[test_case]
    fn the_asid_probe_reached_the_record() {
        assert_eq!(get().asid_bits as usize, super::super::mmu::asid_bits());
        assert!(get().asid_bits >= 8, "crates/asid assumes at least 8");
    }

    /// **The firmware answered, and implements all four extensions the kernel calls.**
    ///
    /// The kernel makes these four `ecall`s unconditionally, so this is not a curiosity: without
    /// RFENCE a TLB shootdown is an error code nobody reads, and the failure surfaces as a thread
    /// reading a stale translation on another hart, arbitrarily far from the cause.
    #[test_case]
    fn the_firmware_implements_what_the_kernel_calls() {
        let sbi = get().sbi;

        assert!(
            sbi.answered(),
            "OpenSBI has had the base extension since 2020"
        );
        assert!(sbi.spec_major >= 1, "and speaks at least SBI 1.0");
        assert_eq!(sbi.impl_name(), Some("OpenSBI"), "which is what QEMU ships");
        assert!(sbi.extensions.contains(isa::riscv64::SBI_REQUIRED));
    }
}
