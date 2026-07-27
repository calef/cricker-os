//! **Interrupt masking, RISC-V.** The S-mode analog of aarch64's `PSTATE.DAIF` handling.
//!
//! On RISC-V, S-mode interrupts are gated by one bit, `SIE` (Supervisor Interrupt Enable) in the
//! `sstatus` CSR. Setting it lets enabled sources (in `sie`) interrupt; clearing it masks them all.
//! These are leaf primitives with no dependencies, so they are implemented for real even though the
//! rest of the RISC-V trap path is not yet built. They are correct standard rv64, but unexercised
//! until the kernel boots on RISC-V.

use core::arch::asm;

/// `sstatus.SIE`, bit 1. Small enough for the CSR immediate-form instructions.
const SIE: usize = 1 << 1;

/// Are S-mode interrupts currently enabled?
pub fn enabled() -> bool {
    let sstatus: usize;
    // SAFETY: reads a CSR. No side effects.
    unsafe { asm!("csrr {}, sstatus", out(reg) sstatus, options(nomem, nostack, preserves_flags)) };
    sstatus & SIE != 0
}

/// Mask S-mode interrupts, returning whether they were enabled before (for [`restore`]). Atomic:
/// `csrrci` reads the old `sstatus` and clears `SIE` in one instruction, so no interrupt can land
/// between the read and the clear.
pub fn disable() -> bool {
    let prev: usize;
    // SAFETY: clears the interrupt-enable bit and reports the prior value. No memory effect.
    unsafe {
        asm!("csrrci {}, sstatus, 2", out(reg) prev, options(nomem, nostack, preserves_flags))
    };
    prev & SIE != 0
}

/// Restore the interrupt-enable state [`disable`] reported. The paired half of `disable`; nesting
/// composes because each `disable` returns the exact bit its `restore` puts back.
pub fn restore(was_enabled: bool) {
    if was_enabled {
        enable();
    } else {
        // SAFETY: clears SIE. No memory effect.
        unsafe { asm!("csrci sstatus, 2", options(nomem, nostack, preserves_flags)) };
    }
}

/// Unmask S-mode interrupts.
pub fn enable() {
    // SAFETY: sets SIE. No memory effect.
    unsafe { asm!("csrsi sstatus, 2", options(nomem, nostack, preserves_flags)) };
}
