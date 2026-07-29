//! The interrupt controller, RISC-V: the PLIC.
//!
//! The arch side of the portable `arch::irq` surface, the twin of the aarch64 GIC adapter. Portable
//! code names `arch::irq`, and here it resolves to `drivers::plic`, keeping the driver name inside
//! `arch/` (rule #1, DECISIONS §4). Thin by design: the PLIC does the work.

use core::sync::atomic::{AtomicU16, AtomicUsize, Ordering};

/// This hart's S-mode PLIC context: `2*hart+1` on QEMU `virt`. On RISC-V the logical cpu id equals
/// the hart id (see `send_reschedule`), so the current hart's context is derived, never stored. The
/// external-interrupt handler claims and completes against this, so a secondary hart uses its own
/// context, not the boot hart's.
pub fn this_s_context() -> usize {
    2 * crate::cpu::id() + 1
}

/// Per-source target PLIC context, or [`CTX_UNASSIGNED`] until the first `enable` chooses one. A
/// device source's target must be **stable** across re-enables: the handler masks the source when it
/// fires, and the driver's ACK re-enables it (this file's `enable`); re-rolling the target each time
/// would leave the mask and the re-enable on different contexts. Assign once, then reuse. Indexed by
/// source number; QEMU `virt` uses small source ids (the UART is 10, virtio-mmio 1..8).
const CTX_UNASSIGNED: u16 = u16::MAX;
const MAX_SOURCES: usize = 128;
static SOURCE_CTX: [AtomicU16; MAX_SOURCES] =
    [const { AtomicU16::new(CTX_UNASSIGNED) }; MAX_SOURCES];

/// Round-robin cursor for spreading device sources over the online harts' contexts, the PLIC twin of
/// the GIC's `ITARGETSR` distribution. Consecutive sources get consecutive harts regardless of their
/// source numbers.
static NEXT_IRQ_HART: AtomicUsize = AtomicUsize::new(0);

/// The PLIC context a source should target: its stable assignment if it has one, otherwise the next
/// online hart's S-context in the round-robin, opened (its threshold lowered) so that hart can take
/// the interrupt, and recorded so every later re-enable reuses it. Bounded to the online harts, each
/// of which has `SEIE` set in [`init_this_cpu`], so we never route to a hart that would drop it.
fn target_context(source: u32) -> usize {
    let slot = &SOURCE_CTX[source as usize % MAX_SOURCES];
    let existing = slot.load(Ordering::Relaxed);
    if existing != CTX_UNASSIGNED {
        return existing as usize;
    }
    let n = crate::smp::online_count();
    let hart = NEXT_IRQ_HART.fetch_add(1, Ordering::Relaxed) % n;
    let ctx = 2 * hart + 1;
    // Open the target hart's context before the first source lands on it. The boot hart runs the
    // enables (test wiring, driver spawn), and the threshold register is global MMIO, so it can open
    // any hart's context. Idempotent, so a race just opens it twice.
    crate::drivers::plic::open_context(ctx);
    // First writer wins, so a racing second enable of the same source agrees on the context.
    match slot.compare_exchange(
        CTX_UNASSIGNED,
        ctx as u16,
        Ordering::Relaxed,
        Ordering::Relaxed,
    ) {
        Ok(_) => ctx,
        Err(other) => other as usize,
    }
}

/// Re-enable (unmask) an interrupt source at the PLIC.
///
/// Called by the `Irq` capability's ACK, after a userspace driver has serviced its device, and once
/// at setup. The external-interrupt handler `disable`d the source at the PLIC when it fired
/// (drivers/plic.rs), so a level-triggered device would not re-fire before the driver read it; this
/// brings the source back. The source is routed to its stable target hart (see [`target_context`]),
/// so device interrupts spread across harts instead of all landing on the boot hart.
pub fn enable(intid: u32) {
    crate::drivers::plic::enable(intid, target_context(intid));
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
/// (`sie.SSIE`), the reschedule-IPI source, so a thread another hart hands it wakes it promptly, and
/// its supervisor external interrupts (`sie.SEIE`), so IRQ affinity ([`enable`]) can route a device
/// source to this hart's PLIC context and have it taken here. The context's threshold is opened, and
/// sources routed, later and by the boot hart (`target_context`); this only unmasks the CSR.
pub fn init_this_cpu() {
    super::exceptions::enable_software_interrupts();
    // Unmask this hart's supervisor external interrupts (`sie.SEIE`) too, so IRQ affinity can route a
    // device source to any online hart and have it actually take the interrupt. Setting SEIE with no
    // source enabled for this hart's context delivers nothing, so this is safe to do here, before the
    // PLIC base is even stored (a secondary runs this as it comes online, ahead of `plic::init`); the
    // context is opened and sources routed later, by the boot hart. The boot hart's own SEIE is set
    // here too (the test path's later `enable_external` becomes a harmless repeat).
    super::exceptions::enable_external();
}

/// Send a reschedule IPI to `target_cpu`. On RISC-V the logical cpu id equals the hart id, and the
/// IPI goes through the SBI, which sets the target hart's `sip.SSIP` so it takes a supervisor
/// software interrupt (`scause` = 1) and drains its inbox. Unlike the PLIC's external interrupts this
/// is a software interrupt, the firmware's mechanism rather than this controller's, but the
/// `arch::irq` seam is the same one aarch64 fills with a GIC SGI.
pub fn send_reschedule(target_cpu: usize) {
    crate::arch::sbi_send_ipi(target_cpu);
}
