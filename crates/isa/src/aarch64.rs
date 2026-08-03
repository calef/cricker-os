//! **What aarch64 machine is this?** Decoded from the ID registers, because the CPU says so itself.
//!
//! This half of the crate is deliberately much shorter than [`riscv64`](crate::riscv64), and the
//! asymmetry is the finding rather than a gap. ARM never removed the CPU's self-description:
//! `MIDR_EL1` names the part, and the `ID_AA64*` space is architected, mandatory, and readable at
//! EL1, so the answer comes from the silicon in front of you with no firmware in the path. There is
//! nothing here to parse and nothing to disbelieve.
//!
//! So the aarch64 record is a **decoder**, not a parser: the kernel reads three registers and hands
//! the raw words here, and everything below is shifts, masks and the ARM ARM's encodings.
//!
//! # What the kernel already did, and what changed
//!
//! One field was read before this milestone: `TCR_EL1.IPS` is written from
//! `ID_AA64MMFR0_EL1.PARange`, with a comment saying "read it from the hardware rather than
//! guessing; a value larger than the implementation supports is UNPREDICTABLE". That was the whole
//! of ISA discovery on this ISA, and it was right. This module gives it a record to live in and
//! three neighbours that were being assumed:
//!
//! * the **4 KiB granule**, which the page tables are built on and which `TGran4` may refuse,
//! * the **ASID width**, which `crates/asid` is built on and which RISC-V had to measure,
//! * the **VA range**, the direct twin of RISC-V's `mmu-type`.
//!
//! # BUGS
//!
//! - **`VARange` reporting 52 does not mean the kernel could use 52.** ARMv8.2-LVA needs a 64 KiB
//!   granule and ARMv8.7-LPA2 is a separate feature bit this record does not read. The field is
//!   reported because it is what the machine says; acting on it is a milestone, not a branch.
//! - **`TGran16`'s encoding is inverted relative to its siblings.** `TGran4` and `TGran64` spell
//!   "supported" as `0b0000` and "not supported" as `0b1111`; `TGran16` spells them `0b0001` and
//!   `0b0000`. A decoder that treats the three uniformly reports 16 KiB backwards on every part in
//!   existence, which is why each has its own line below.

/// Which translation granules the machine supports at stage 1.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Granules {
    /// The one this kernel's page tables are built on. Its absence stops the boot.
    pub k4: bool,
    pub k16: bool,
    pub k64: bool,
}

/// **What this aarch64 machine is.** One record, populated once at boot, printed at boot.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Isa {
    /// `MIDR_EL1.Implementer`, an ARM-assigned vendor code. See [`Isa::implementer_name`].
    pub implementer: u8,
    /// `MIDR_EL1.PartNum`. Vendor-specific, so it prints as a number: `0xd08` is a Cortex-A72 to
    /// somebody with the vendor's list, and a guess to us.
    pub part: u16,
    /// `MIDR_EL1.Variant` and `.Revision`, which ARM writes as `r{variant}p{revision}` and which is
    /// how every errata document identifies a part.
    pub variant: u8,
    pub revision: u8,
    /// `ID_AA64MMFR0_EL1.PARange`, kept in its **raw encoding** because that is what `TCR_EL1.IPS`
    /// takes. [`Isa::pa_bits`] decodes it for the boot line.
    pub pa_range: u8,
    /// `ID_AA64MMFR0_EL1.ASIDBits`, decoded to 8 or 16. The aarch64 twin of RISC-V's `satp.ASID`
    /// probe, and the reason that probe had to exist: ARM **mandates** one of two values here, so
    /// the number is architected and readable, while RISC-V permits any width including zero and
    /// publishes nothing, so it has to be measured. `crates/asid` assumes at least 8 either way.
    pub asid_bits: u8,
    pub granules: Granules,
    /// `ID_AA64MMFR2_EL1.VARange`, decoded to 48 or 52. See BUGS: 52 is a claim about the part, not
    /// a configuration this kernel could adopt by flipping one field.
    pub va_bits: u8,
}

/// What a machine is missing that this kernel needs.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Missing {
    /// No 4 KiB granule at stage 1. Every page table in `crates/paging` is 4 KiB.
    pub granule_4k: bool,
    /// Fewer than 8 ASID bits. Unreachable on a conforming part (ARM's only two encodings are 8 and
    /// 16), which is exactly why it is worth checking: a reserved encoding decodes here as 0 and
    /// would otherwise be a silently aliasing TLB.
    pub asid_bits: bool,
}

impl Missing {
    pub const fn any(self) -> bool {
        self.granule_4k || self.asid_bits
    }
}

impl Isa {
    /// Decode the three registers the kernel reads. Pure: no `mrs` here, so this is host-testable
    /// against words captured from real parts.
    ///
    /// The field positions are the ARM ARM's (D19.2 in DDI 0487): `MIDR_EL1` is
    /// implementer[31:24] / variant[23:20] / architecture[19:16] / partnum[15:4] / revision[3:0],
    /// and the `ID_AA64MMFR*` fields are 4 bits each at the offsets named below.
    pub fn decode(midr_el1: u64, mmfr0_el1: u64, mmfr2_el1: u64) -> Isa {
        let f = |reg: u64, shift: u32| ((reg >> shift) & 0xf) as u8;

        Isa {
            implementer: (midr_el1 >> 24) as u8,
            part: ((midr_el1 >> 4) & 0xfff) as u16,
            variant: f(midr_el1, 20),
            revision: f(midr_el1, 0),
            pa_range: f(mmfr0_el1, 0),
            asid_bits: match f(mmfr0_el1, 4) {
                0b0000 => 8,
                0b0010 => 16,
                // Reserved. Reported as zero rather than rounded up to 8, so
                // `missing_requirements` refuses the boot instead of assuming the kind answer.
                _ => 0,
            },
            granules: Granules {
                // 0b0000 supported, 0b1111 not. Any other value is reserved and read as absent.
                k4: f(mmfr0_el1, 28) == 0b0000,
                // 0b0001 supported, 0b0000 not. The inverted one; see the module BUGS.
                k16: f(mmfr0_el1, 20) == 0b0001 || f(mmfr0_el1, 20) == 0b0010,
                k64: f(mmfr0_el1, 24) == 0b0000,
            },
            va_bits: match f(mmfr2_el1, 16) {
                0b0001 => 52,
                _ => 48,
            },
        }
    }

    /// **Can this machine run us?** The aarch64 twin of
    /// [`riscv64::Isa::missing_requirements`](crate::riscv64::Isa::missing_requirements), and the
    /// only verb here a call site is meant to branch on.
    pub fn missing_requirements(&self) -> Missing {
        Missing {
            granule_4k: !self.granules.k4,
            asid_bits: self.asid_bits < 8,
        }
    }

    /// How many physical address bits [`pa_range`](Isa::pa_range) encodes.
    ///
    /// Zero for a reserved encoding, which is honest rather than useful: the kernel writes the raw
    /// field into `TCR_EL1.IPS` regardless, because ARM's rule is that a value *larger* than the
    /// implementation supports is UNPREDICTABLE, and the machine's own encoding cannot be larger
    /// than itself.
    pub fn pa_bits(&self) -> u8 {
        match self.pa_range {
            0b0000 => 32,
            0b0001 => 36,
            0b0010 => 40,
            0b0011 => 42,
            0b0100 => 44,
            0b0101 => 48,
            0b0110 => 52,
            0b0111 => 56,
            _ => 0,
        }
    }

    /// The vendor name for [`implementer`](Isa::implementer), or `None` for a code ARM has not
    /// published. Printing the number is better than printing a guess.
    pub fn implementer_name(&self) -> Option<&'static str> {
        match self.implementer {
            0x41 => Some("Arm"),
            0x42 => Some("Broadcom"),
            0x43 => Some("Cavium"),
            0x44 => Some("DEC"),
            0x46 => Some("Fujitsu"),
            0x49 => Some("Infineon"),
            0x4d => Some("Motorola/Freescale"),
            0x4e => Some("NVIDIA"),
            0x50 => Some("AppliedMicro"),
            0x51 => Some("Qualcomm"),
            0x56 => Some("Marvell"),
            0x61 => Some("Apple"),
            0x69 => Some("Intel"),
            0xc0 => Some("Ampere"),
            _ => None,
        }
    }
}
