//! The interrupt controller, aarch64: the GIC.
//!
//! This is the arch side of the portable `arch::irq` surface. Portable code (the `Irq` capability's
//! ACK, and from here the interrupt setup) names `arch::irq`, never `drivers::gic` directly, so the
//! driver name stays inside `arch/` where rule #1 (DECISIONS §4) puts it. It is a thin adapter: the
//! GIC does the work, this only gives it an architecture-neutral name. The RISC-V twin is the PLIC.

/// Re-enable (unmask) an interrupt source at the controller.
///
/// Called by the `Irq` capability's ACK, after a userspace driver has serviced its device: the IRQ
/// handler masked the source when it fired (so a level-triggered device would not re-fire in a storm
/// before the driver quieted it), and this brings it back. seL4's IRQHandler protocol, the aarch64
/// half.
pub fn enable(intid: u32) {
    crate::drivers::gic::enable(intid);
}
