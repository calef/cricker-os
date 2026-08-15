//! **What RISC-V machine is this?** Parsed out of the device tree, because there is no CPUID.
//!
//! `misa` exists and is not enough: it is permitted to read as zero, its single-letter fields cannot
//! name a multi-letter extension, and every extension ratified after 2015 is multi-letter. So the
//! architected answer is a property firmware wrote into the device tree, and this module reads it.
//!
//! Three properties carry the answer, and which of them a given machine has depends on how old its
//! firmware is:
//!
//! | Property | Since | Shape |
//! |---|---|---|
//! | `riscv,isa-extensions` | Linux 6.6 (2023) | a stringlist, one entry per extension |
//! | `riscv,isa-base` | Linux 6.6 (2023) | `"rv64i"` |
//! | `riscv,isa` | forever, deprecated | one string, `"rv64imafdc_zicsr_..."` |
//!
//! **The legacy string is not a fallback we are being polite about.** The VisionFive 2's vendor tree
//! predates the split, and it is the board this milestone exists for, so [`Isa::from_device_tree`]
//! parses both forms and records which one it got in [`Isa::legacy_isa_string`].
//!
//! # Why every CPU node, and not `cpu@0`
//!
//! A RISC-V machine may be heterogeneous, and the one arriving in August is: the JH7110 on a
//! VisionFive 2 pairs four U74 application cores with a small S7 monitor core, described as separate
//! `cpu@` nodes with **different `riscv,isa` strings**. A kernel that reads the first node and then
//! schedules a thread onto any hart has read the wrong node half the time.
//!
//! So [`Isa::common`] is the **intersection** over every CPU node that describes itself, which is
//! what "an instruction the kernel may emit" actually means, and [`Isa::any`] is the union, which is
//! the only way to tell "this machine has no FPU" from "one of its harts has no FPU". The `mmu-type`
//! is taken the same way: the narrowest any hart declares.
//!
//! # BUGS
//!
//! - **A `status = "disabled"` CPU node contributes nothing to the record** (the VisionFive 2 prep,
//!   2026-08-14; this used to be the first bug on this list). The real board showed the case this
//!   list was waiting for: the JH7110's hart 0 is an MMU-less S7 monitor core marked disabled, and
//!   counting it would narrow `common` to the S7's `rv64imac` and, on a tree that spells the S7's
//!   MMU as `riscv,none`, refuse to boot a machine whose four real harts are exactly our contract.
//!   The kernel never runs on a disabled hart (`smp` refuses to start one), so its self-description
//!   is not a fact about the machine the kernel schedules on. Such a node still counts in
//!   [`Isa::harts`], which is an honest node count, not a roster.
//! - **A hart whose own `riscv,isa` disclaims supervisor mode contributes nothing either** (the
//!   VisionFive 2 bench, 2026-08-14, same day, second stop). The vendor tree in the flashed
//!   firmware marks the S7 `status = "okay"` **and** gives it `mmu-type = "riscv,sv39"`, both
//!   false, so the `status` gate above never fires there. The one truthful property is the ISA
//!   string itself: `rv64imacu` spells user mode and omits supervisor. A hart that cannot enter
//!   S-mode never runs this kernel, so it may not narrow `common` or `mmu` regardless of what
//!   `status` claims. See [`supervisor_mode_claim`] for why the rule needs the `u` witness: absence
//!   of `s` alone would exclude every modern machine, QEMU included.
//! - **Absence of `zicsr` or `zifencei` from a legacy `riscv,isa` string is not evidence of
//!   absence.** Both were carved out of the base `I` extension in 2019, and a string written before
//!   that (or by firmware that never caught up) simply says `rv64imafdc` while the hardware has
//!   them. This is exactly why neither is in [`REQUIRED`] even though the kernel uses both on every
//!   trap and in `sync_icache`: a check that fails on the board we are buying is worse than no
//!   check. `M`, `A` and `C` carry no such ambiguity, so they are the ones that gate the boot.

use dtb::{Dtb, Error};

/// The most CPU nodes [`Isa::from_device_tree`] will read. Sixteen is twice the kernel's
/// `MAX_CPUS` and costs one pointer pair each on the stack; a machine with more is reported as
/// truncated ([`Isa::harts`] saturates) rather than silently half-read.
pub const MAX_HARTS: usize = 16;

/// The base integer ISA the machine declares (`riscv,isa-base`, or the prefix of `riscv,isa`).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Base {
    /// No CPU node declared one. Not the same as "not RV64": see [`Isa::described`].
    #[default]
    Unknown,
    Rv32,
    Rv64,
    Rv128,
}

impl Base {
    /// The name to print. Deliberately spelled as the device tree spells it.
    pub fn name(self) -> &'static str {
        match self {
            Base::Unknown => "unknown",
            Base::Rv32 => "rv32i",
            Base::Rv64 => "rv64i",
            Base::Rv128 => "rv128i",
        }
    }

    /// Decode the `rv32` / `rv64` / `rv128` prefix of an ISA string. Ignores everything after it, so
    /// it works on `riscv,isa-base` (`"rv64i"`) and on `riscv,isa` (`"rv64imafdc_..."`) alike.
    pub fn from_isa_string(s: &[u8]) -> Base {
        // `rv128` first: `rv1` is not a prefix of anything else, but checking `rv32`/`rv64` first
        // and falling through would be fine too. Order stated so nobody has to re-derive it.
        if s.starts_with(b"rv128") {
            Base::Rv128
        } else if s.starts_with(b"rv64") {
            Base::Rv64
        } else if s.starts_with(b"rv32") {
            Base::Rv32
        } else {
            Base::Unknown
        }
    }
}

/// The widest page-table format any hart's MMU declares (`mmu-type`).
///
/// Ordered by how much address space each reaches, with [`MmuType::Unknown`] at the bottom so that
/// `mmu >= MmuType::Sv39` is false when nothing declared one. That comparison is the only reason the
/// ordering exists; see [`Isa::missing_requirements`].
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum MmuType {
    /// No CPU node declared `mmu-type`.
    #[default]
    Unknown,
    /// `"riscv,none"`: no MMU at all. A machine we cannot run on.
    None,
    Sv32,
    Sv39,
    Sv48,
    Sv57,
}

impl MmuType {
    pub fn name(self) -> &'static str {
        match self {
            MmuType::Unknown => "not declared",
            MmuType::None => "none",
            MmuType::Sv32 => "sv32",
            MmuType::Sv39 => "sv39",
            MmuType::Sv48 => "sv48",
            MmuType::Sv57 => "sv57",
        }
    }

    /// Decode a `mmu-type` value. The binding spells these `"riscv,sv39"`; the vendor prefix is
    /// stripped here so a tree that omits it (they exist) still parses.
    pub fn from_property(value: &[u8]) -> MmuType {
        let s = value.split(|&b| b == 0).next().unwrap_or(b"");
        let s = s.strip_prefix(b"riscv,").unwrap_or(s);
        match s {
            b"sv57" => MmuType::Sv57,
            b"sv48" => MmuType::Sv48,
            b"sv39" => MmuType::Sv39,
            b"sv32" => MmuType::Sv32,
            b"none" => MmuType::None,
            _ => MmuType::Unknown,
        }
    }
}

/// A set of the extensions this kernel has a reason to name. **Not every extension a machine has**:
/// the QEMU `virt` default declares thirty-odd, and a bitset of all of them would be a list nobody
/// maintains. See [`TABLE`] for the rows and the reason each one is there.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Extensions(u32);

impl Extensions {
    pub const NONE: Extensions = Extensions(0);

    /// Is every extension in `other` present here?
    pub const fn contains(self, other: Extensions) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Extensions) -> Extensions {
        Extensions(self.0 | other.0)
    }

    pub const fn intersection(self, other: Extensions) -> Extensions {
        Extensions(self.0 & other.0)
    }

    /// Everything in `self` that is not in `other`. Used to report *which* requirement is missing
    /// rather than only that one is.
    pub const fn difference(self, other: Extensions) -> Extensions {
        Extensions(self.0 & !other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// One row of [`TABLE`]: an extension this kernel names, and why it names it.
///
/// The `why` is not decoration. An extension with no reason on this list is an extension somebody
/// added because it was in the tree, and the whole discipline of this milestone is that the record
/// stays small enough to read.
pub struct Row {
    /// The name as `riscv,isa-extensions` spells it, lowercase.
    pub name: &'static str,
    pub bit: Extensions,
    /// Does the kernel refuse to boot without it? See the module's BUGS for why `zicsr` and
    /// `zifencei` are used everywhere and still not required.
    pub required: bool,
    pub why: &'static str,
}

/// The one function in this crate that a coverage report will always show as unreached, and
/// deliberately so: every caller below is a `const` initializer, so the compiler folds it and the
/// body never executes at runtime. Left uncalled rather than given a test, because the only
/// assertion available (`bit(3)` is `1 << 3`) restates the body and proves nothing. Contrast
/// [`eid`], which is also const-only and *is* tested, because there the thing that can be wrong is
/// the tag rather than the arithmetic.
const fn bit(n: u32) -> Extensions {
    Extensions(1 << n)
}

pub const I: Extensions = bit(0);
pub const M: Extensions = bit(1);
pub const A: Extensions = bit(2);
pub const C: Extensions = bit(3);
pub const F: Extensions = bit(4);
pub const D: Extensions = bit(5);
pub const H: Extensions = bit(6);
pub const V: Extensions = bit(7);
pub const ZICSR: Extensions = bit(8);
pub const ZIFENCEI: Extensions = bit(9);
pub const SSTC: Extensions = bit(10);
pub const SVINVAL: Extensions = bit(11);
pub const ZICBOM: Extensions = bit(12);

/// Every extension this kernel names, with the reason. Printed in this order at boot.
///
/// The four `required` rows are exactly the target triple: the kernel and userspace are both built
/// for `rv64imac` (`targets/riscv64-unknown-nife.json` says `+m,+a,+c`), so the compiler will
/// emit multiply, atomic and compressed instructions whether or not the machine has them.
pub const TABLE: [Row; 13] = [
    Row {
        name: "i",
        bit: I,
        required: true,
        why: "the base integer ISA; nothing runs without it",
    },
    Row {
        name: "m",
        bit: M,
        required: true,
        why: "multiply/divide, emitted by the compiler (target is rv64imac)",
    },
    Row {
        name: "a",
        bit: A,
        required: true,
        why: "atomics; every lock in the kernel is lr/sc or amo",
    },
    Row {
        name: "c",
        bit: C,
        required: true,
        why: "compressed encodings, emitted throughout (target is rv64imac)",
    },
    Row {
        name: "f",
        bit: F,
        required: false,
        why: "single-precision float; the kernel and userspace are both soft-float (lp64)",
    },
    Row {
        name: "d",
        bit: D,
        required: false,
        why: "double-precision float; same, soft-float, so this is information not need",
    },
    Row {
        name: "h",
        bit: H,
        required: false,
        why: "hypervisor; nothing uses it, and its absence would scope a future milestone",
    },
    Row {
        name: "v",
        bit: V,
        required: false,
        why: "vector; the QEMU rva23s64 model has it and no real board here does",
    },
    Row {
        name: "zicsr",
        bit: ZICSR,
        required: false,
        why: "CSR access; used on every trap, but see BUGS on the legacy string",
    },
    Row {
        name: "zifencei",
        bit: ZIFENCEI,
        required: false,
        why: "fence.i; arch::sync_icache is one, but see BUGS on the legacy string",
    },
    Row {
        name: "sstc",
        bit: SSTC,
        required: false,
        why: "stimecmp: would let the timer arm itself instead of paying an SBI ecall",
    },
    Row {
        name: "svinval",
        bit: SVINVAL,
        required: false,
        why: "fine-grained TLB invalidation: the alternative to sfence.vma's blunt sweep",
    },
    Row {
        name: "zicbom",
        bit: ZICBOM,
        required: false,
        why: "cache-block management: what non-coherent DMA on a real board would need",
    },
];

/// The extensions the kernel refuses to boot without, derived from [`TABLE`] so the two cannot drift.
pub const REQUIRED: Extensions = {
    let mut acc = Extensions::NONE;
    let mut i = 0;
    while i < TABLE.len() {
        if TABLE[i].required {
            acc = acc.union(TABLE[i].bit);
        }
        i += 1;
    }
    acc
};

/// Every bit any row names. Used by the "no duplicate bits" assertion below.
const NAMED: Extensions = {
    let mut acc = Extensions::NONE;
    let mut i = 0;
    while i < TABLE.len() {
        acc = acc.union(TABLE[i].bit);
        i += 1;
    }
    acc
};

// A bit used by two rows would make one of them silently unreportable, and a bit used by none would
// be a constant nothing can ever set. Both are the kind of mistake a table invites and a compile
// error is the cheapest place to catch it.
const _: () = {
    assert!(
        NAMED.0.count_ones() as usize == TABLE.len(),
        "two rows of TABLE share a bit"
    );
    assert!(REQUIRED.contains(I.union(M).union(A).union(C)));
};

/// **The SBI extensions the kernel calls.** Firmware, not silicon, so this is a third source
/// alongside the device tree and the probes: the answer comes from `sbi_probe_extension`.
///
/// All four are called unconditionally today, which is why all four are required. That is worth
/// naming precisely because it is the failure that would be silent otherwise: `sbi_remote_sfence_vma`
/// on firmware without RFENCE returns an error nobody reads, the other harts never invalidate, and a
/// migrated thread runs on a stale translation.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct SbiExtensions(u32);

impl SbiExtensions {
    pub const NONE: SbiExtensions = SbiExtensions(0);

    pub const fn contains(self, other: SbiExtensions) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: SbiExtensions) -> SbiExtensions {
        SbiExtensions(self.0 | other.0)
    }

    pub const fn difference(self, other: SbiExtensions) -> SbiExtensions {
        SbiExtensions(self.0 & !other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// One row of [`SBI_TABLE`]: an SBI extension, its EID, and which kernel function calls it.
pub struct SbiRow {
    pub name: &'static str,
    pub bit: SbiExtensions,
    /// The extension id to pass to `sbi_probe_extension` (base extension, function 3).
    pub eid: usize,
    pub why: &'static str,
}

pub const SBI_TIME: SbiExtensions = SbiExtensions(1 << 0);
pub const SBI_IPI: SbiExtensions = SbiExtensions(1 << 1);
pub const SBI_RFENCE: SbiExtensions = SbiExtensions(1 << 2);
pub const SBI_HSM: SbiExtensions = SbiExtensions(1 << 3);

/// An SBI extension id is the big-endian ASCII of a short tag the specification assigns, so the id
/// is **derived from the tag** here rather than written as hex.
///
/// That is not tidiness. The first draft of this table wrote `TIME` as `0x0054_494D`, which is the
/// ASCII of `"TIM"`, and the failure it buys is one of the nastier shapes there is: the probe asks
/// firmware about an extension that does not exist, gets "no", and the kernel refuses to boot on
/// correct firmware. Deriving it makes the tag the only thing anyone can get wrong, and a wrong tag
/// is legible.
pub const fn eid(tag: &str) -> usize {
    let b = tag.as_bytes();
    let mut acc = 0usize;
    let mut i = 0;
    while i < b.len() {
        acc = (acc << 8) | b[i] as usize;
        i += 1;
    }
    acc
}

pub const EID_TIME: usize = eid("TIME");
/// `sPI`, not `IPI`. The specification's own tag, and one of the two that are not simply the
/// extension's human name.
pub const EID_IPI: usize = eid("sPI");
/// `RFNC`, the other one.
pub const EID_RFENCE: usize = eid("RFNC");
pub const EID_HSM: usize = eid("HSM");
/// The base extension, which is how the other four are probed at all: function 3 is
/// `probe_extension(eid)`, and functions 0 through 2 report the spec version and the implementation.
/// The one id that is a number rather than a tag.
pub const EID_BASE: usize = 0x10;

/// Every SBI extension the kernel calls, and the call site. Four rows, four `ecall` sites.
pub const SBI_TABLE: [SbiRow; 4] = [
    SbiRow {
        name: "TIME",
        bit: SBI_TIME,
        eid: EID_TIME,
        why: "arch::timer::init arms every tick through sbi_set_timer",
    },
    SbiRow {
        name: "IPI",
        bit: SBI_IPI,
        eid: EID_IPI,
        why: "arch::sbi_send_ipi is how one hart reschedules another",
    },
    SbiRow {
        name: "RFENCE",
        bit: SBI_RFENCE,
        eid: EID_RFENCE,
        why: "arch::sbi_remote_sfence_vma; RISC-V has no hardware TLB broadcast",
    },
    SbiRow {
        name: "HSM",
        bit: SBI_HSM,
        eid: EID_HSM,
        why: "arch::psci_cpu_on starts every secondary hart",
    },
];

/// The SBI extensions the kernel refuses to boot without, derived from [`SBI_TABLE`].
pub const SBI_REQUIRED: SbiExtensions = {
    let mut acc = SbiExtensions::NONE;
    let mut i = 0;
    while i < SBI_TABLE.len() {
        acc = acc.union(SBI_TABLE[i].bit);
        i += 1;
    }
    acc
};

/// What the firmware underneath us is, and what it can do.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Sbi {
    /// SBI spec version, from `sbi_get_spec_version`. `0.0` means the base extension itself did not
    /// answer, which no conforming firmware does.
    ///
    /// **The minor number is 24 bits wide, not 16**, and the major is the seven bits above it. That
    /// is a lopsided split and it is the specification's; a decoder that assumes the obvious 16/16
    /// reads QEMU's `0x0300_0000` (SBI 3.0) as version 0.0 and concludes the firmware never
    /// answered. This one did, on the machine, after the host tests were green.
    pub spec_major: u8,
    pub spec_minor: u32,
    /// `sbi_get_impl_id`. 1 is `OpenSBI`, 0 is BBL, 4 is `RustSBI`; see [`Sbi::impl_name`].
    pub impl_id: u32,
    /// `sbi_get_impl_version`. The implementation's own encoding, printed raw because only the
    /// implementation knows how to read it.
    pub impl_version: u32,
    pub extensions: SbiExtensions,
}

impl Sbi {
    /// **Did the base extension answer at all?**
    ///
    /// No conforming firmware reports spec version 0.0, so a zero here means the `ecall` went
    /// nowhere: SBI v0.1 (2019) predates the base extension, and firmware that old cannot be asked
    /// what it implements.
    ///
    /// This is the same rule the device-tree half follows, one source over: **silence is not
    /// evidence of absence.** Refusing to boot because a probe could not run would refuse on a
    /// machine that works, so [`Isa::missing_requirements`] skips the SBI check entirely when this
    /// is false, and the boot line says the four extensions are unverified rather than present.
    pub fn answered(self) -> bool {
        self.spec_major != 0 || self.spec_minor != 0
    }

    /// The name of the firmware, for the boot line. The ids are assigned in the SBI specification's
    /// implementation-id registry; unknown ones print as a number rather than a guess.
    pub fn impl_name(self) -> Option<&'static str> {
        let mut i = 0;
        while i < IMPLEMENTATIONS.len() {
            if IMPLEMENTATIONS[i].id == self.impl_id {
                return Some(IMPLEMENTATIONS[i].name);
            }
            i += 1;
        }
        None
    }
}

/// One row of [`IMPLEMENTATIONS`].
pub struct Implementation {
    /// The id `sbi_get_impl_id` returns.
    pub id: u32,
    pub name: &'static str,
}

/// **The SBI implementations the specification has assigned ids to.**
///
/// A table rather than a `match`, for the same reason [`TABLE`] and
/// [`crate::aarch64::IMPLEMENTERS`] are tables: a duplicated key in a `match` is legal and silent
/// and makes the second arm unreachable, while a duplicate here is the compile error below. Ids are
/// sequential from zero, so the assertion also catches a row inserted in the wrong place.
pub const IMPLEMENTATIONS: [Implementation; 7] = [
    Implementation { id: 0, name: "BBL" },
    Implementation {
        id: 1,
        name: "OpenSBI",
    },
    Implementation {
        id: 2,
        name: "Xvisor",
    },
    Implementation { id: 3, name: "KVM" },
    Implementation {
        id: 4,
        name: "RustSBI",
    },
    Implementation {
        id: 5,
        name: "Diosix",
    },
    Implementation {
        id: 6,
        name: "Coffer",
    },
];

// The registry assigns these sequentially, so "row i has id i" is both the duplicate check and a
// check that nobody inserted a row in the middle and shifted the rest off their real ids.
const _: () = {
    let mut i = 0;
    while i < IMPLEMENTATIONS.len() {
        assert!(
            IMPLEMENTATIONS[i].id as usize == i,
            "IMPLEMENTATIONS is out of order or has a gap; the registry assigns ids sequentially"
        );
        i += 1;
    }
};

/// **What this RISC-V machine is.** One record, populated once at boot, printed at boot.
///
/// Filled from three sources, and the field docs say which:
///
/// * the device tree ([`from_device_tree`](Isa::from_device_tree)), which is firmware's claim,
/// * the firmware itself over SBI ([`sbi`](Isa::sbi)), which the kernel fills,
/// * a measurement ([`asid_bits`](Isa::asid_bits)), which the kernel fills after paging is on.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Isa {
    /// Tier 1. The narrowest base any hart declares.
    pub base: Base,
    /// Tier 1. Present on **every** hart that describes itself: the set the kernel may assume.
    pub common: Extensions,
    /// Tier 1. Present on **at least one** hart. Differs from [`common`](Isa::common) exactly when
    /// the machine is heterogeneous, which is the only way to see that from a boot line.
    pub any: Extensions,
    /// Tier 1. The narrowest `mmu-type` any hart declares.
    pub mmu: MmuType,
    /// How many `cpu@` nodes the tree has, saturating at [`MAX_HARTS`].
    pub harts: u16,
    /// How many of those declared an ISA at all, not counting disabled harts or harts whose own
    /// `riscv,isa` disclaims supervisor mode (the module BUGS say why neither may speak).
    /// `common` is an intersection over these, so `described == 0` means `common` is meaningless
    /// and the tree simply did not say.
    pub described: u16,
    /// Did the answer come from the deprecated `riscv,isa` string rather than
    /// `riscv,isa-extensions`? Changes how an absent `zicsr` should be read (see the module BUGS).
    pub legacy_isa_string: bool,
    /// Tier 2, filled by `arch::mmu::init`'s probe. `0` until it runs. This is the fact no emulator
    /// can tell us: QEMU reports 16 implemented `satp.ASID` bits on every CPU model it offers,
    /// including `sifive-u54`, so the number here is only interesting on real silicon.
    pub asid_bits: u8,
    /// Firmware, filled by `arch::isa::init` over SBI.
    pub sbi: Sbi,
}

/// What a machine is missing that this kernel needs. Every field is empty on a machine we can run.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Missing {
    /// The tree declares a base other than RV64. `false` when it declares nothing at all: a tree
    /// that says nothing is not evidence of a machine that cannot run us, and the kernel is
    /// demonstrably executing by the time this is read.
    pub base: bool,
    /// Required extensions (see [`REQUIRED`]) absent from at least one described hart.
    pub extensions: Extensions,
    /// The tree declares an MMU narrower than Sv39. `false` when it declares nothing.
    pub mmu: bool,
    /// Required SBI extensions (see [`SBI_REQUIRED`]) the firmware says it does not implement.
    /// Empty when the base extension did not answer at all (see [`Sbi::answered`]).
    pub sbi: SbiExtensions,
}

impl Missing {
    pub const fn any(self) -> bool {
        self.base || !self.extensions.is_empty() || self.mmu || !self.sbi.is_empty()
    }
}

impl Isa {
    /// **Can this machine run us?** The only verb on this record that a call site is meant to
    /// branch on, and it is meant to branch exactly one way: say what is missing, and stop.
    ///
    /// Deliberately silent about what the tree does not mention. A device tree with no
    /// `riscv,isa` at all describes a machine that is nonetheless executing this function, so
    /// treating silence as failure would refuse to boot on hardware that works. Silence is
    /// reported by [`described`](Isa::described) being zero and printed as such.
    pub fn missing_requirements(&self) -> Missing {
        Missing {
            base: self.base != Base::Unknown && self.base != Base::Rv64,
            extensions: if self.described == 0 {
                Extensions::NONE
            } else {
                REQUIRED.difference(self.common)
            },
            mmu: self.mmu != MmuType::Unknown && self.mmu < MmuType::Sv39,
            sbi: if self.sbi.answered() {
                SBI_REQUIRED.difference(self.sbi.extensions)
            } else {
                SbiExtensions::NONE
            },
        }
    }

    /// Do the harts differ from each other?
    pub fn heterogeneous(&self) -> bool {
        self.common != self.any
    }

    /// Read the CPU nodes. Fills the tier-1 fields and leaves `asid_bits` and `sbi` at their
    /// defaults for the kernel to complete.
    pub fn from_device_tree(dt: &Dtb<'_>) -> Result<Isa, Error> {
        let mut extensions = [None; MAX_HARTS];
        let harts = dt.node_props(b"cpu@", b"riscv,isa-extensions", &mut extensions)?;

        let mut legacy = [None; MAX_HARTS];
        dt.node_props(b"cpu@", b"riscv,isa", &mut legacy)?;

        let mut isa_base = [None; MAX_HARTS];
        dt.node_props(b"cpu@", b"riscv,isa-base", &mut isa_base)?;

        let mut mmu_type = [None; MAX_HARTS];
        dt.node_props(b"cpu@", b"mmu-type", &mut mmu_type)?;

        // Which harts may speak for the machine at all. A disabled hart never runs kernel code,
        // and neither does one whose own ISA string disclaims supervisor mode, so what they
        // declare says nothing about the machine the kernel schedules on; see the module BUGS.
        // Both gates are live on the same silicon: mainline JH7110 trees exclude the S7 by
        // `status`, the vendor tree marks it "okay" (and Sv39, both lies) and only the missing
        // `s` in `rv64imacu` tells the truth (bench, 2026-08-14).
        let mut status = [None; MAX_HARTS];
        dt.node_props(b"cpu@", b"status", &mut status)?;
        let enabled = |i: usize| {
            status[i].is_none_or(crate::cpu_list::is_okay)
                && legacy[i].is_none_or(|s| supervisor_mode_claim(s) != Some(false))
        };

        let mut out = Isa {
            harts: harts.min(u16::MAX as usize) as u16,
            ..Isa::default()
        };
        // The intersection starts full and is narrowed by each hart; the union starts empty. With no
        // described hart the intersection would stay full, which would be a lie, so `described`
        // gates every reader of `common` and the loop below only touches it when a hart speaks.
        let mut common = Extensions(!0);

        for i in 0..harts.min(MAX_HARTS) {
            if !enabled(i) {
                continue;
            }
            let (base, exts) = match (extensions[i], legacy[i]) {
                // The modern stringlist wins where both are present: it is the one firmware is
                // expected to keep current, and QEMU emits both.
                (Some(list), _) => (Base::Unknown, parse_extension_list(list)),
                (None, Some(s)) => {
                    out.legacy_isa_string = true;
                    (Base::from_isa_string(s), parse_isa_string(s))
                }
                (None, None) => continue,
            };

            // `riscv,isa-base` is the modern spelling and beats the prefix of the legacy string.
            let base = match isa_base[i] {
                Some(b) => Base::from_isa_string(b),
                None => base,
            };
            if base != Base::Unknown && (out.base == Base::Unknown || base < out.base) {
                out.base = base;
            }

            common = common.intersection(exts);
            out.any = out.any.union(exts);
            out.described += 1;
        }

        if out.described > 0 {
            // Mask off bits no row names, so an all-ones seed cannot leak into the record.
            out.common = common.intersection(NAMED);
        }

        for (i, m) in mmu_type.iter().enumerate().take(harts.min(MAX_HARTS)) {
            let Some(m) = m else { continue };
            // Same gate as the ISA loop: an unschedulable hart's MMU must not touch what the
            // schedulable harts declare, in either direction. Mainline spells the S7's truth
            // (`riscv,none`, which would read as a machine that cannot run us); the vendor tree
            // spells a lie (`riscv,sv39` on a core with no MMU at all), and neither is a fact
            // about the machine the kernel schedules on.
            if !enabled(i) {
                continue;
            }
            let t = MmuType::from_property(m);
            if t != MmuType::Unknown && (out.mmu == MmuType::Unknown || t < out.mmu) {
                out.mmu = t;
            }
        }

        Ok(out)
    }
}

// `Base` needs an ordering for "narrowest wins" above, and the derive would order it by declaration,
// which puts `Unknown` first. That is what we want (it is handled separately) but it is worth being
// explicit that the ordering is by address width.
impl PartialOrd for Base {
    fn partial_cmp(&self, other: &Base) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Base {
    fn cmp(&self, other: &Base) -> core::cmp::Ordering {
        fn width(b: Base) -> u8 {
            match b {
                Base::Unknown => 0,
                Base::Rv32 => 32,
                Base::Rv64 => 64,
                Base::Rv128 => 128,
            }
        }
        width(*self).cmp(&width(*other))
    }
}

/// Look a name up in [`TABLE`]. Not `pub`: the whole point of the table is that the set of names
/// this kernel cares about is closed.
fn lookup(name: &[u8]) -> Extensions {
    let mut i = 0;
    while i < TABLE.len() {
        if TABLE[i].name.as_bytes() == name {
            return TABLE[i].bit;
        }
        i += 1;
    }
    Extensions::NONE
}

/// Parse `riscv,isa-extensions`: a device-tree stringlist, which is NUL-separated and
/// NUL-terminated, one full extension name per entry.
pub fn parse_extension_list(value: &[u8]) -> Extensions {
    let mut acc = Extensions::NONE;
    for entry in value.split(|&b| b == 0) {
        if !entry.is_empty() {
            acc = acc.union(lookup(entry));
        }
    }
    acc
}

/// Parse the deprecated `riscv,isa` string, e.g. `"rv64imafdc_zicsr_zifencei"`.
///
/// The grammar is not quite as regular as it looks. After the `rv64` prefix comes a run of
/// single-letter extensions with no separator, and after the first `_` comes a run of multi-letter
/// extensions separated by `_`. **`g` is not an extension**, it is an abbreviation for `imafd`
/// plus `zicsr` and `zifencei` (the 2019 spelling), and a machine that writes `rv64gc` is saying
/// exactly what `rv64imafdc_zicsr_zifencei` says.
pub fn parse_isa_string(value: &[u8]) -> Extensions {
    let mut acc = Extensions::NONE;
    let mut parts = strip_base_prefix(value).split(|&b| b == b'_');

    // The single-letter run.
    for &c in parts.next().unwrap_or(b"") {
        let c = c.to_ascii_lowercase();
        if c == b'g' {
            acc = acc
                .union(I)
                .union(M)
                .union(A)
                .union(F)
                .union(D)
                .union(ZICSR)
                .union(ZIFENCEI);
        } else {
            acc = acc.union(lookup(&[c]));
        }
    }

    // The multi-letter run.
    for part in parts {
        if !part.is_empty() {
            acc = acc.union(lookup(part));
        }
    }

    acc
}

/// A legacy `riscv,isa` value up to its NUL, with the `rv32`/`rv64`/`rv128` prefix removed:
/// the single-letter run first, then the `_`-separated multi-letter extensions.
fn strip_base_prefix(value: &[u8]) -> &[u8] {
    let s = value.split(|&b| b == 0).next().unwrap_or(b"");
    for p in [&b"rv128"[..], b"rv64", b"rv32"] {
        if let Some(tail) = s.strip_prefix(p) {
            return tail;
        }
    }
    s
}

/// **What a legacy `riscv,isa` string says about supervisor mode**, which is a privilege level
/// rather than an extension, and which the string's grammar changed its mind about spelling.
///
/// Early strings list privilege letters in the single-letter run: QEMU before 5.1 wrote
/// `rv64imafdcsu`, and the VisionFive 2's vendor firmware still writes that generation
/// (`rv64imacu` on the S7, `rv64imafdcbsux` on the U74s, read at the bench 2026-08-14). Modern
/// strings write **neither** letter, because Linux rejects them: QEMU `virt` today says
/// `rv64imafdch_...` on a machine that certainly has S-mode, and the mainline JH7110 dtsi spells
/// no privilege letter on any hart. So a missing `s` alone proves nothing, and a rule that read it
/// as "no supervisor mode" would refuse every hart of every modern machine, boot hart included.
///
/// The reading that is honest on both generations: a bare `u` is the witness that the writer was
/// spelling privilege modes at all, and only then is a missing `s` a statement.
///
/// - `Some(true)`: a bare `s` in the single-letter run. The hart claims S-mode.
/// - `Some(false)`: a bare `u` and no `s`. The writer spelled privilege modes and left supervisor
///   out. This hart cannot enter S-mode, so it cannot run this kernel, whatever its node's
///   `status` and `mmu-type` claim; the vendor S7 lies about both and this is the tell.
/// - `None`: neither letter. The string is silent about privilege modes (the modern spelling),
///   and silence is not evidence of absence, same rule as [`Sbi::answered`].
///
/// Only the run before the first `_` is scanned, so `_s`-prefixed multi-letter extensions
/// (`_sstc`, `_svadu`, `_sdtrig`, all in QEMU's string today) can never read as a bare `s`.
pub fn supervisor_mode_claim(value: &[u8]) -> Option<bool> {
    let single_letters = strip_base_prefix(value)
        .split(|&b| b == b'_')
        .next()
        .unwrap_or(b"");
    let mut saw_s = false;
    let mut saw_u = false;
    for &c in single_letters {
        match c.to_ascii_lowercase() {
            b's' => saw_s = true,
            b'u' => saw_u = true,
            _ => {}
        }
    }
    match (saw_s, saw_u) {
        (true, _) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
    }
}
