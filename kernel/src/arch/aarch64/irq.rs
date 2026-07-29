//! The interrupt controller, aarch64: the GIC.
//!
//! This is the arch side of the portable `arch::irq` surface. Portable code (the `Irq` capability's
//! ACK, and from here the interrupt setup) names `arch::irq`, never `drivers::gic` directly, so the
//! driver name stays inside `arch/` where rule #1 (DECISIONS §4) puts it. It is a thin adapter: the
//! GIC does the work, this only gives it an architecture-neutral name. The RISC-V twin is the PLIC.

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

/// Per-INTID target core, or [`UNASSIGNED`] until the first `enable` chooses one. A device line's
/// target must be **stable** across re-enables: the IRQ handler masks the line when it fires, and
/// the driver's ACK re-enables it (this file's `enable`), so if we re-rolled the target on every
/// ACK the line would hop cores on every completion. Assign once, then reuse.
const UNASSIGNED: u8 = 0xff;
static IRQ_TARGET: [AtomicU8; 256] = [const { AtomicU8::new(UNASSIGNED) }; 256];

/// Round-robin cursor for spreading SPI lines over the online cores. Consecutive device lines get
/// consecutive cores regardless of their INTID values, which `intid % ncpus` would not guarantee
/// (three virtio devices at INTIDs 48, 52, 56 would all land on core 0 under mod-4).
static NEXT_IRQ_CORE: AtomicUsize = AtomicUsize::new(0);

/// The core an SPI should target: its stable assignment if it has one, otherwise the next core in
/// the round-robin, recorded so every later re-enable of this line reuses it. Bounded to the cores
/// that are actually online (each has its GIC CPU interface up), so we never target a core that
/// would never take the interrupt.
fn target_cpu(intid: u32) -> usize {
    let slot = &IRQ_TARGET[intid as usize];
    let existing = slot.load(Ordering::Relaxed);
    if existing != UNASSIGNED {
        return existing as usize;
    }
    let n = crate::smp::online_count();
    let chosen = NEXT_IRQ_CORE.fetch_add(1, Ordering::Relaxed) % n;
    // First writer wins, so a racing second enable of the same line agrees on the target.
    match slot.compare_exchange(
        UNASSIGNED,
        chosen as u8,
        Ordering::Relaxed,
        Ordering::Relaxed,
    ) {
        Ok(_) => chosen,
        Err(other) => other as usize,
    }
}

/// Re-enable (unmask) an interrupt source at the controller.
///
/// Called by the `Irq` capability's ACK, after a userspace driver has serviced its device: the IRQ
/// handler masked the source when it fired (so a level-triggered device would not re-fire in a storm
/// before the driver quieted it), and this brings it back. seL4's IRQHandler protocol, the aarch64
/// half.
///
/// SPI lines are spread across the online cores (see [`target_cpu`]) so a device's completion
/// interrupt, and the driver wake `handle_irq` turns it into, lands on the chosen core instead of
/// funnelling every device onto core 0. PPIs and SGIs are per-core; the target is ignored for them.
pub fn enable(intid: u32) {
    crate::drivers::gic::enable(intid, target_cpu(intid));
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
