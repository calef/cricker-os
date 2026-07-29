//! PLIC driver: the RISC-V Platform-Level Interrupt Controller (milestone 20).
//!
//! The RISC-V analog of the GIC. Where an aarch64 device raises an SPI into the GIC and the CPU
//! takes an IRQ exception, a RISC-V device raises a wire into the **PLIC**, which routes it to a
//! *context* (a hart at a privilege level) as a supervisor external interrupt (`scause` = 9). The
//! PLIC's job is priority arbitration and the claim/complete handshake; it does no masking of its
//! own beyond the per-context enable bits and threshold.
//!
//! The register model, all 32-bit MMIO at fixed offsets from the base (which comes from the device
//! tree, like the GIC's):
//!
//! ```text
//!   base + 0x0000 + 4*source          per-source priority (0 = never interrupt)
//!   base + 0x1000 + ...               per-source pending bits (read-only; we do not use them)
//!   base + 0x2000 + 0x80*context      per-context enable bits, one bit per source
//!   base + 0x20_0000 + 0x1000*context per-context priority threshold (interrupt if prio > threshold)
//!   base + 0x20_0004 + 0x1000*context per-context claim (read) / complete (write)
//! ```
//!
//! A *context* on QEMU's `virt` is `2*hart + 1` for S-mode (`2*hart` is M-mode, which OpenSBI owns).
//! Hart 0 S-mode is context 1, which is where our external interrupts land.
//!
//! Same rule as every driver here (DECISIONS §4): **it reaches into no kernel globals.** [`init`]
//! is handed the base address and the context; everything else works from the two atomics it stored.
//! The claim/complete handshake is naturally serialized on one hart (it runs in the external-interrupt
//! handler), so the state is two lock-free atomics rather than a mutex.

use core::sync::atomic::{AtomicUsize, Ordering};

/// The PLIC's MMIO base (a kernel virtual address in the direct map), stored by [`init`].
static PLIC_BASE: AtomicUsize = AtomicUsize::new(0);

const PRIORITY_BASE: usize = 0x0000;
const ENABLE_BASE: usize = 0x2000;
const ENABLE_STRIDE: usize = 0x80; // per context
const THRESHOLD_BASE: usize = 0x20_0000;
const CONTEXT_STRIDE: usize = 0x1000; // per context, for threshold and claim/complete
const CLAIM_OFFSET: usize = 0x0004; // claim/complete sits one word past the threshold

fn base() -> usize {
    PLIC_BASE.load(Ordering::Relaxed)
}

/// Read a 32-bit PLIC register at `off` from the base.
fn read(off: usize) -> u32 {
    // SAFETY: `off` names a PLIC register within the block `init` mapped and promised.
    unsafe { core::ptr::read_volatile((base() + off) as *const u32) }
}

/// Write a 32-bit PLIC register at `off` from the base.
fn write(off: usize, val: u32) {
    // SAFETY: as `read`.
    unsafe { core::ptr::write_volatile((base() + off) as *mut u32, val) }
}

/// Bring the PLIC up for one context: record the base and context, and set the threshold to 0 so
/// every source with a nonzero priority can interrupt. Sources are still individually disabled
/// until [`enable`]; this only opens the gate.
///
/// # Safety
/// `base` must be the PLIC's MMIO base as a mapped, device-typed kernel virtual address, and
/// `context` must be this hart's supervisor context number.
pub unsafe fn init(base: usize, context: usize) {
    PLIC_BASE.store(base, Ordering::Relaxed);
    // Threshold 0: an interrupt is taken when its priority is strictly greater, so priority >= 1
    // gets through. (Threshold at max would mask everything.) Opens the boot context; a secondary
    // hart's context is opened lazily by `arch::irq` the first time a source is routed to it.
    open_context(context);
}

/// Open `context`'s threshold so any source with a nonzero priority can interrupt it. [`init`] does
/// this for the boot context; IRQ affinity (`arch::irq`) calls this for a secondary hart's context
/// the first time it routes a device source there, so that hart can actually take the interrupt. A
/// context whose threshold is never opened takes nothing, which is the safe default. Idempotent.
pub fn open_context(context: usize) {
    write(THRESHOLD_BASE + context * CONTEXT_STRIDE, 0);
}

/// Enable `source` for `context` and give it a nonzero priority, so the PLIC will deliver it there.
///
/// `context` is a hart's S-mode context (`2*hart+1` on QEMU `virt`); the affinity policy in
/// `arch::irq` chooses which one, so a device line lands on a chosen hart rather than always the
/// boot hart. The driver is told the context; it does not read the hartid (rule #2, DECISIONS §4).
pub fn enable(source: u32, context: usize) {
    // Priority 1 (the lowest that still interrupts; we do not prioritize among sources yet).
    write(PRIORITY_BASE + source as usize * 4, 1);
    let word = ENABLE_BASE + context * ENABLE_STRIDE + (source as usize / 32) * 4;
    let bit = 1u32 << (source % 32);
    write(word, read(word) | bit);
}

/// Disable `source` for `context` (clear its enable bit). The complement of [`enable`]. Called from
/// the external-interrupt handler with the context that took the interrupt, which is the same
/// context the source is enabled on (a source targets exactly one hart), so the mask and the later
/// ACK re-enable stay in agreement.
pub fn disable(source: u32, context: usize) {
    let word = ENABLE_BASE + context * ENABLE_STRIDE + (source as usize / 32) * 4;
    let bit = 1u32 << (source % 32);
    write(word, read(word) & !bit);
}

/// **Claim the highest-priority pending interrupt** for `context`, and mask it: the PLIC will not
/// deliver this source again until [`complete`] is called for it. Returns 0 if nothing is pending
/// (0 is not a valid source; source numbering starts at 1). Reading the claim register is the
/// acknowledge, so call it exactly once per interrupt. `context` must be the **claiming hart's own**
/// context, so a secondary hart claims from its context, not the boot hart's.
pub fn claim(context: usize) -> u32 {
    read(THRESHOLD_BASE + context * CONTEXT_STRIDE + CLAIM_OFFSET)
}

/// **Complete** a claimed interrupt: tell the PLIC we are done with `source`, so it may deliver that
/// source again. The counterpart to [`claim`]; between the two the source is masked at the PLIC.
/// `context` must be the context that claimed it (the completing hart's own).
pub fn complete(source: u32, context: usize) {
    write(
        THRESHOLD_BASE + context * CONTEXT_STRIDE + CLAIM_OFFSET,
        source,
    );
}
