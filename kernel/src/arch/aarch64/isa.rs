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

use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use aarch64_cpu::registers::{ID_AA64MMFR0_EL1, ID_AA64MMFR2_EL1, MIDR_EL1};
use isa::aarch64::{Conduit, Isa, Psci};
use tock_registers::interfaces::Readable;

use crate::sync::{IrqSafeMutex, rank};
use crate::{print, println};

/// The record, written once by [`init`] and read by the boot print, `mmu::init` and the tests.
///
/// `None` until then, so a read before discovery panics naming this file rather than reporting a
/// plausible all-zero machine.
static ISA: IrqSafeMutex<Option<Isa>> = IrqSafeMutex::new(rank::ISA, None);

/// The `/psci` record, in three plain atomics rather than behind the mutex above (milestone 100).
///
/// `psci_cpu_on` reads these, and it runs on the bring-up path with interrupts enabled; putting the
/// PSCI facts behind a lock would put a lock rank on the path that starts a core, for a value that
/// is written once before any secondary exists and never again. Atomics say that directly.
const PSCI_ABSENT: u8 = 0;
const PSCI_NO_METHOD: u8 = 1;
const PSCI_HVC: u8 = 2;
const PSCI_SMC: u8 = 3;
static PSCI_CONDUIT: AtomicU8 = AtomicU8::new(PSCI_ABSENT);
/// The `CPU_ON` function id, or 0 for "the node did not publish one". No PSCI implementation
/// publishes zero (SMCCC reserves the low identifiers and every published `CPU_ON` sets the
/// fast-call bit), so the sentinel cannot collide with a real id.
static PSCI_CPU_ON: AtomicU32 = AtomicU32::new(0);
/// Did the id come from the node's own `cpu_on` property, rather than from the 0.2 standard? Boot
/// line only; see [`psci_record`].
static PSCI_ID_FROM_PROPERTY: AtomicU8 = AtomicU8::new(0);

/// **Discover the machine, and refuse to run on one we cannot.**
///
/// Two sources, and the split is the finding milestone 60 recorded: the ISA comes from ID registers,
/// because ARM never removed the CPU's self-description, and the device tree says nothing about it.
///
/// **The pointer is no longer ignored** (milestone 100). One aarch64 fact is not in any ID register
/// and could not be: whether a firmware call leaves EL1 at `hvc` or at `smc` depends on what runs
/// above the kernel, not on the part. `/psci` states it, along with the `CPU_ON` function id, and
/// both were compiled in until this read them.
///
/// # Panics
///
/// Deliberately, when the machine cannot run this kernel: no 4 KiB granule, or an `ASIDBits`
/// encoding ARM has never defined. Both are unreachable on a conforming part and both are silent if
/// unchecked, which is the combination that makes a boot-time comparison worth its cost.
///
/// **A machine with no usable `/psci` is not one of those cases**, and does not stop the boot: it is
/// a machine this kernel runs on with one core, which it says at bring-up rather than here.
pub fn init(dtb_ptr: usize) {
    // SAFETY: the pointer firmware handed us, whose magic `memory::init` has already checked (it
    // runs first and panics otherwise), named through the boot map's direct region.
    let dt = unsafe { dtb::Dtb::from_ptr(super::mmu::phys_to_virt(dtb_ptr as u64) as *const u8) }
        .expect("device tree is unreadable");
    match Psci::from_device_tree(&dt).expect("cannot read /psci") {
        None => PSCI_CONDUIT.store(PSCI_ABSENT, Ordering::Relaxed),
        Some(psci) => {
            PSCI_CONDUIT.store(
                match psci.conduit {
                    None => PSCI_NO_METHOD,
                    Some(Conduit::Hvc) => PSCI_HVC,
                    Some(Conduit::Smc) => PSCI_SMC,
                },
                Ordering::Relaxed,
            );
            PSCI_CPU_ON.store(psci.cpu_on.unwrap_or(0), Ordering::Relaxed);
            PSCI_ID_FROM_PROPERTY.store(u8::from(psci.cpu_on_from_property), Ordering::Relaxed);
        }
    }

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

/// **What a `CPU_ON` call needs**: the conduit to make it on and the function id to make it with,
/// or `None` when this machine stated neither well enough to try. Read by
/// [`super::psci_cpu_on`](super::psci_cpu_on) on every core it starts.
pub fn psci() -> Option<(Conduit, u32)> {
    let conduit = match PSCI_CONDUIT.load(Ordering::Relaxed) {
        PSCI_HVC => Conduit::Hvc,
        PSCI_SMC => Conduit::Smc,
        _ => return None,
    };
    match PSCI_CPU_ON.load(Ordering::Relaxed) {
        0 => None,
        id => Some((conduit, id)),
    }
}

/// The whole `/psci` answer, for the boot line and the tests: `None` when the tree had no such node,
/// otherwise the record with whatever it did and did not state. [`psci`] is the version the bring-up
/// path wants, which is the two facts or nothing.
pub fn psci_record() -> Option<Psci> {
    if PSCI_CONDUIT.load(Ordering::Relaxed) == PSCI_ABSENT {
        return None;
    }
    let cpu_on = match PSCI_CPU_ON.load(Ordering::Relaxed) {
        0 => None,
        id => Some(id),
    };
    Some(Psci {
        conduit: match PSCI_CONDUIT.load(Ordering::Relaxed) {
            PSCI_HVC => Some(Conduit::Hvc),
            PSCI_SMC => Some(Conduit::Smc),
            _ => None,
        },
        cpu_on,
        cpu_on_from_property: PSCI_ID_FROM_PROPERTY.load(Ordering::Relaxed) != 0,
        // Not recorded: the only thing `standard` changes is which source the id came from, and
        // `cpu_on_from_property` already says that. Reconstructing it here would be a second
        // encoding of one fact, and the boot line reads the one that exists.
        standard: PSCI_ID_FROM_PROPERTY.load(Ordering::Relaxed) == 0 && cpu_on.is_some(),
    })
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

    /// **The PSCI conduit and function id came from `/psci`, and they are what used to be compiled
    /// in** (milestone 100).
    ///
    /// Two assertions and they are doing different jobs. The first is that discovery ran at all: a
    /// build that skipped the parse leaves `psci()` at `None`, and every secondary would then fail
    /// to start with `PSCI_NOT_DISCOVERED` rather than with a firmware error. The second holds the
    /// retired hardcodes against the machine, which is `crates/pci`'s discipline: on QEMU `virt` the
    /// old constants were right, and the value of saying so here is that the day they stop being
    /// right, this line says which one moved.
    #[test_case]
    fn the_psci_call_was_read_from_the_machine() {
        let (conduit, cpu_on) = super::psci().expect("QEMU virt has a usable /psci node");

        assert_eq!(conduit, Conduit::Hvc, "the method /psci states on virt");
        assert_eq!(cpu_on, 0xC400_0003, "the id smp.rs used to hold as a const");
        assert_eq!(cpu_on, isa::aarch64::PSCI_CPU_ON_64);
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
