//! **Which cores does this machine have?** The `/cpus` node, decoded.
//!
//! Milestone 100. Until this existed the kernel started cores `0..cpu::MAX_CPUS` and called that
//! the machine's core list. On QEMU `virt` the two happen to agree; on anything else they do not,
//! and the way they disagreed was the worst available: a `CPU_ON` for a core that is not there
//! returns an error nobody has to look at, and a core that *is* there past the fourth was never
//! named at all, so it stayed parked with nothing to report.
//!
//! Both architectures describe their cores the same way, which is why this is not under
//! [`crate::aarch64`] or [`crate::riscv64`]: a `cpu@` node per core, under `/cpus`, with the
//! hardware id in `reg`. What the id *means* is arch-specific (an `MPIDR_EL1` affinity value on
//! aarch64, a hart id on RISC-V) and neither this module nor its caller needs to care, because the
//! only thing done with it is handing it back to the firmware call that starts that core.
//!
//! # The `#address-cells` of `/cpus` is the whole decode
//!
//! A `cpu@` node's `reg` is one address in its parent's cells, and `/cpus` declares them. One cell
//! is the usual case and the only one QEMU produces. **Two is not exotic**: the ARM CPU binding
//! requires `#address-cells = <2>` the moment any core's `MPIDR_EL1.Aff3` is nonzero, which is
//! every machine with more than 256 cores' worth of affinity space, so a decoder that assumes one
//! cell reads a 64-bit id as its high half and starts nothing.
//!
//! # BUGS
//!
//! - **A core past [`MAX_CPU_NODES`] is counted and not recorded.** [`CpuList::described`] is the
//!   honest total and [`CpuList::cpus`] is the prefix that fit, so a caller can see it read part of
//!   a machine. It cannot see *which* part it missed.
//! - **`status`, `enable-method` and `supervisor` are recorded, not obeyed.** This module reports
//!   what the tree said; *acting* on it is the caller's call, because the caller is the one that
//!   knows what it can speak and where it runs. [`Cpu::startable`] is that caller's decision as a
//!   predicate, kept here (one copy, host-testable against the board fixtures) and applied by
//!   `smp::read_cpu_list`, which is still the only place it has any effect.
//! - **The `/cpus` node is found by name, not by binding.** There is no `compatible` on it to match;
//!   the device tree specification names the node itself. Same simplification [`dtb::Dtb::node_reg`]
//!   records, and here it is not a simplification at all.

use dtb::{Dtb, Error};

/// The most `cpu@` nodes [`CpuList::from_device_tree`] will record.
///
/// Sixteen, matching [`crate::riscv64::MAX_HARTS`], which is twice the kernel's `MAX_CPUS` and
/// costs 24 bytes each. A machine with more is reported as truncated ([`CpuList::described`] keeps
/// counting) rather than silently half-read.
pub const MAX_CPU_NODES: usize = 16;

/// How firmware says a core is started, from `enable-method`.
///
/// Recorded rather than acted on: see the module BUGS. The variants are the two the ARM CPU binding
/// defines plus the two ways a tree can decline to answer.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum EnableMethod {
    /// No `enable-method` property. Normal on RISC-V, where SBI HSM is the only mechanism and the
    /// binding does not ask; on aarch64 it means the tree did not say.
    #[default]
    Unstated,
    /// `"psci"`: a firmware call. What QEMU `virt` states and what this kernel speaks.
    Psci,
    /// `"spin-table"`: the core is already running, spinning on a mailbox address the node's
    /// `cpu-release-addr` names. A different bring-up mechanism entirely, and one this kernel does
    /// not implement.
    SpinTable,
    /// Something else, which for aarch64 means a per-SoC method (`"brcm,bcm2836-smp"` and friends).
    Other,
}

impl EnableMethod {
    /// Decode an `enable-method` value. The property is a string, so the value carries its NUL.
    pub fn from_property(value: &[u8]) -> EnableMethod {
        match value.split(|&b| b == 0).next().unwrap_or(b"") {
            b"psci" => EnableMethod::Psci,
            b"spin-table" => EnableMethod::SpinTable,
            _ => EnableMethod::Other,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            EnableMethod::Unstated => "unstated",
            EnableMethod::Psci => "psci",
            EnableMethod::SpinTable => "spin-table",
            EnableMethod::Other => "other",
        }
    }
}

/// One core, as the tree describes it.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Cpu {
    /// The `reg` value: the id firmware knows this core by, and the id a bring-up call must name.
    /// An `MPIDR_EL1[39:0]` affinity value on aarch64, a hart id on RISC-V.
    pub hwid: u64,
    /// `status` is absent or says `"okay"`. A `"disabled"` core is one firmware is telling us not to
    /// use, and starting it is at best wasted and at worst a core running code we did not plan for.
    pub usable: bool,
    /// Can this core enter supervisor mode, as far as its own `riscv,isa` says? `false` only on
    /// positive evidence: the string spells privilege modes and omits `s` (see
    /// [`crate::riscv64::supervisor_mode_claim`]). A node with no such string, or one silent about
    /// privilege modes, is `true`; every aarch64 tree and every modern RISC-V spelling lands there,
    /// because silence is not evidence of absence.
    ///
    /// This field exists because `status` can lie where the ISA string does not: the VisionFive 2's
    /// vendor tree marks its M/U-only S7 monitor core `"okay"` with `mmu-type = "riscv,sv39"`, both
    /// false (bench, 2026-08-14), and starting it crashed the firmware. An S-mode kernel cannot run
    /// on a core without S-mode, whatever the rest of the node claims.
    pub supervisor: bool,
    /// What `enable-method` said, if anything.
    pub enable_method: EnableMethod,
}

impl Cpu {
    /// **Would this kernel start this core?** Firmware calls it usable, its own ISA string does not
    /// disclaim supervisor mode, and it is started by a mechanism this kernel speaks (PSCI, or SBI
    /// HSM behind an unstated method, which is the RISC-V binding's shape).
    ///
    /// This is `smp::read_cpu_list`'s decision, lifted here so the board fixtures can hold it: the
    /// VisionFive 2 tests assert which harts this predicate admits (the four U74s) and which it
    /// refuses (the S7, through either tree's lie), and a kernel-side copy would put the exact
    /// expression the bench crash taught us out of the host suite's reach. `Unstated` counts as
    /// startable on purpose: the RISC-V CPU binding has no `enable-method` at all (SBI HSM is the
    /// only mechanism), so demanding the property would refuse every RISC-V machine including the
    /// ones this kernel already runs on. `supervisor` is required because `status` can lie where a
    /// hart's own `riscv,isa` does not (bench, 2026-08-14; notes/visionfive2.md, the second stop).
    pub fn startable(&self) -> bool {
        self.usable
            && self.supervisor
            && matches!(
                self.enable_method,
                EnableMethod::Psci | EnableMethod::Unstated
            )
    }
}

/// **The machine's cores**, in the order the tree lists them.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct CpuList {
    /// The cores that fit, `[..len]` of them.
    pub cpus: [Cpu; MAX_CPU_NODES],
    /// How many entries of [`cpus`](CpuList::cpus) are written.
    pub len: usize,
    /// How many `cpu@` nodes the tree has. Greater than `len` exactly when the machine has more
    /// cores than [`MAX_CPU_NODES`], which is the case a caller must be able to see.
    pub described: usize,
    /// How many address cells `/cpus` declared, kept for the boot line: a two-cell tree is worth
    /// seeing, because it means the machine has affinity beyond `Aff2`.
    pub address_cells: u32,
    /// `/cpus/timebase-frequency`, the rate of RISC-V's `time` CSR, in Hz.
    ///
    /// It lives here rather than in [`crate::riscv64::Isa`] because it is a property of the `/cpus`
    /// node, and because the RISC-V timer needs it *before* the ISA record exists: `timer::init`
    /// runs several steps ahead of `isa::init` in that boot. `None` on aarch64, whose counter
    /// frequency is architected in `CNTFRQ_EL0` and needs no tree.
    pub timebase_hz: Option<u64>,
}

impl Default for CpuList {
    fn default() -> Self {
        Self {
            cpus: [Cpu::default(); MAX_CPU_NODES],
            len: 0,
            described: 0,
            address_cells: 1,
            timebase_hz: None,
        }
    }
}

impl CpuList {
    /// The cores that fit, as a slice.
    pub fn cpus(&self) -> &[Cpu] {
        &self.cpus[..self.len]
    }

    /// Did the tree describe more cores than this record holds?
    pub fn truncated(&self) -> bool {
        self.described > self.len
    }

    /// **Read `/cpus`.**
    ///
    /// A tree with no `cpu@` nodes is not an error: it is a machine that declined to describe its
    /// cores, and the answer is an empty list ([`len`](CpuList::len) of zero) rather than a failure,
    /// because the kernel asking the question is demonstrably running on one of them. An `Err` here
    /// means the blob itself is malformed, which is a different thing entirely.
    pub fn from_device_tree(dt: &Dtb<'_>) -> Result<CpuList, Error> {
        let mut list = CpuList {
            address_cells: cpus_address_cells(dt)?,
            ..CpuList::default()
        };

        let mut regs = [None; MAX_CPU_NODES];
        let mut status = [None; MAX_CPU_NODES];
        let mut methods = [None; MAX_CPU_NODES];
        let mut isa_strings = [None; MAX_CPU_NODES];
        list.described = dt.node_props(b"cpu@", b"reg", &mut regs)?;
        dt.node_props(b"cpu@", b"status", &mut status)?;
        dt.node_props(b"cpu@", b"enable-method", &mut methods)?;
        dt.node_props(b"cpu@", b"riscv,isa", &mut isa_strings)?;

        list.len = list.described.min(MAX_CPU_NODES);
        for i in 0..list.len {
            list.cpus[i] = Cpu {
                // A `cpu@` node with no `reg` is malformed, and there is no id to invent. Zero is
                // wrong in the least dangerous direction: it collides with the boot core, which
                // every caller skips, rather than naming some other core to start.
                hwid: regs[i]
                    .and_then(|bytes| hwid_from_reg(bytes, list.address_cells))
                    .unwrap_or(0),
                usable: status[i].is_none_or(is_okay),
                supervisor: isa_strings[i]
                    .is_none_or(|s| crate::riscv64::supervisor_mode_claim(s) != Some(false)),
                enable_method: methods[i]
                    .map_or(EnableMethod::Unstated, EnableMethod::from_property),
            };
        }

        list.timebase_hz = timebase_hz(dt)?;
        Ok(list)
    }
}

/// The `#address-cells` `/cpus` declares for its children.
///
/// Defaults to 1 when absent. The device tree specification's own default for a node that says
/// nothing is 2, and using it here would be following the letter into the wrong answer: the CPU
/// bindings *require* `/cpus` to declare this, so an absent property is a malformed tree, and the
/// overwhelmingly common shape (every tree QEMU emits, on both architectures) is one cell.
fn cpus_address_cells(dt: &Dtb<'_>) -> Result<u32, Error> {
    match dt.node_prop(b"cpus", b"#address-cells")? {
        Some(bytes) if bytes.len() >= 4 => {
            Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }
        _ => Ok(1),
    }
}

/// Decode a `cpu@` node's `reg` as one address in `cells` cells.
///
/// `None` for a width no CPU binding defines, rather than a guess: three cells is not "two and a
/// bit", it is a tree we have misread.
fn hwid_from_reg(bytes: &[u8], cells: u32) -> Option<u64> {
    match cells {
        1 if bytes.len() >= 4 => {
            Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64)
        }
        2 if bytes.len() >= 8 => Some(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])),
        _ => None,
    }
}

/// Is a `status` property one of the two spellings that mean "use this"?
///
/// The specification defines `"okay"`, `"disabled"`, `"reserved"`, `"fail"` and `"fail-sss"`. Only
/// the first means the device is operational; `"ok"` is a deprecated spelling still emitted by older
/// firmware, and accepting it costs nothing. Everything else, including a value we do not recognise,
/// reads as unusable, which is the safe direction: not starting a core we could have started is a
/// smaller machine, while starting one firmware reserved is a core running in someone else's memory.
///
/// `pub(crate)` because [`crate::riscv64::Isa::from_device_tree`] applies the same reading when it
/// decides which harts' `riscv,isa` and `mmu-type` may narrow the machine record: one decoder for
/// `status`, so the two walks cannot disagree about what "disabled" means.
pub(crate) fn is_okay(value: &[u8]) -> bool {
    matches!(
        value.split(|&b| b == 0).next().unwrap_or(b""),
        b"okay" | b"ok"
    )
}

/// `/cpus/timebase-frequency`, falling back to the first `cpu@` node's own.
///
/// The RISC-V binding permits it in either place and requires it in one of them. QEMU puts it on
/// `/cpus`; the JH7110 trees put it there too, but the binding's per-hart form exists for a
/// machine whose harts genuinely differ, so reading only the first is a simplification and not a
/// correct general answer. The kernel treats the counter as machine-wide either way, so the honest
/// place to record that limitation is here rather than at the call site.
fn timebase_hz(dt: &Dtb<'_>) -> Result<Option<u64>, Error> {
    if let Some(bytes) = dt.node_prop(b"cpus", b"timebase-frequency")? {
        return Ok(cells_to_u64(bytes));
    }
    let mut per_hart = [None; 1];
    dt.node_props(b"cpu@", b"timebase-frequency", &mut per_hart)?;
    Ok(per_hart[0].and_then(cells_to_u64))
}

/// A one- or two-cell unsigned value. Both widths appear in the wild for `timebase-frequency`.
fn cells_to_u64(bytes: &[u8]) -> Option<u64> {
    match bytes.len() {
        4 => Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64),
        8 => Some(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])),
        _ => None,
    }
}
