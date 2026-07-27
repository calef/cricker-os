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

/// The PLIC context that targets the **boot hart's** S-mode: `2*hart + 1` on QEMU `virt` (each
/// hart has an M context at `2*hart` and an S context at `2*hart + 1`). This must be derived, not
/// hardcoded to 1: OpenSBI elects the boot hart by lottery, and a kernel that programs hart 0's
/// context while running (and setting `sie.SEIE`) on hart 3 leaves every external interrupt
/// pending at the PLIC forever, with all harts parked in `wfi`. Found by the parity-C disk test
/// hanging on some runs and passing on others, exactly the lottery's coin flip.
pub fn boot_s_context() -> usize {
    2 * super::boot_hartid() + 1
}

/// Bring the interrupt controller up on the boot core. On RISC-V this is the PLIC, but the shared
/// `interrupts_init` that calls this is on the full-boot path, which the port does not reach yet (the
/// RISC-V boot tour halts earlier and initializes the PLIC directly in its device-driver step, from
/// `memory::plic_region`). A no-op until the two paths converge; the PLIC is real (drivers/plic.rs).
pub fn init() {}

/// Per-core interrupt setup, run on each hart as it becomes a scheduler participant (the primary in
/// its boot tour, each secondary in `secondary_main`). Unmasks this hart's software interrupts
/// (`sie.SSIE`), the reschedule-IPI source, so a thread another hart hands it wakes it promptly. The
/// PLIC's per-hart external-interrupt context (`2*hart+1` threshold/enables) will be programmed here
/// too once a device interrupt is routed to a non-boot hart; the boot hart's is set in `plic::init`.
pub fn init_this_cpu() {
    super::exceptions::enable_software_interrupts();
}

/// Send a reschedule IPI to `target_cpu`. On RISC-V the logical cpu id equals the hart id, and the
/// IPI goes through the SBI, which sets the target hart's `sip.SSIP` so it takes a supervisor
/// software interrupt (`scause` = 1) and drains its inbox. Unlike the PLIC's external interrupts this
/// is a software interrupt, the firmware's mechanism rather than this controller's, but the
/// `arch::irq` seam is the same one aarch64 fills with a GIC SGI.
pub fn send_reschedule(target_cpu: usize) {
    crate::arch::sbi_send_ipi(target_cpu);
}
