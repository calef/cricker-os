//! The interrupt controller, RISC-V: the PLIC.
//!
//! The arch side of the portable `arch::irq` surface, the twin of the aarch64 GIC adapter. Portable
//! code names `arch::irq`, and here it resolves to `drivers::plic`, keeping the driver name inside
//! `arch/` (rule #1, DECISIONS §4). Thin by design: the PLIC does the work.

/// Re-enable (unmask) an interrupt source at the PLIC.
///
/// Called by the `Irq` capability's ACK, after a userspace driver has serviced its device. The
/// external-interrupt handler `disable`d the source at the PLIC when it fired (drivers/plic.rs), so a
/// level-triggered device would not re-fire before the driver read it; this brings the source back.
pub fn enable(intid: u32) {
    crate::drivers::plic::enable(intid);
}
