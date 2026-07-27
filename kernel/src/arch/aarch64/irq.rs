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

/// Bring the interrupt controller up on the boot core. Reads the GIC's two register blocks from the
/// device tree (distributor and CPU interface) and initializes it. Called once, from the shared
/// `interrupts_init`.
pub fn init() {
    let ((gicd, _), (gicc, _)) =
        crate::memory::gic_regions().expect("no interrupt controller in the DTB");
    // SAFETY: the addresses came from the device tree, and `mmu::init` mapped both as DEVICE memory.
    // Mapping them as normal memory would let the CPU cache and reorder writes to an interrupt
    // controller, which is exactly as bad as it sounds.
    unsafe {
        crate::drivers::gic::init(
            crate::arch::mmu::phys_to_virt(gicd),
            crate::arch::mmu::phys_to_virt(gicc),
        )
    };
}

/// Per-core interrupt-controller setup, run by each core as it comes online. The GIC's CPU interface
/// is private to each core, so every core enables its own.
pub fn init_this_cpu() {
    crate::drivers::gic::init_this_cpu();
}

/// Send a reschedule inter-processor interrupt to `target_cpu`. On aarch64 this is a software-
/// generated interrupt (SGI) on the reschedule vector; the target core's handler drains its inbox
/// and reschedules. See sched.rs and DECISIONS §11 (SMP).
pub fn send_reschedule(target_cpu: usize) {
    crate::drivers::gic::send_sgi(crate::sched::RESCHED_SGI, target_cpu);
}
