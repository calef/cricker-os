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

/// Bring the interrupt controller up on the boot core. On RISC-V this is the PLIC, but the shared
/// `interrupts_init` that calls this is on the full-boot path, which the port does not reach yet (the
/// RISC-V boot tour halts earlier and initializes the PLIC directly in its device-driver step, from
/// `memory::plic_region`). A no-op until the two paths converge; the PLIC is real (drivers/plic.rs).
pub fn init() {}

/// Per-core interrupt-controller setup. The RISC-V port is single-hart for now (no SMP bring-up via
/// SBI HSM), so there is no secondary context to program; the PLIC's one S-mode context is set in
/// `plic::init`. A no-op until SMP.
pub fn init_this_cpu() {}

/// Send a reschedule IPI to another hart. On RISC-V, inter-processor interrupts go through the CLINT
/// (software interrupts, `sip.SSIP`), not the PLIC, and the port is single-hart, so there is no other
/// hart to poke and the target is always self. A no-op until SMP; the thread is already enqueued and
/// the current hart reschedules at its next scheduling point.
pub fn send_reschedule(_target_cpu: usize) {}
