//! Masking and unmasking interrupts.
//!
//! On aarch64 this is the `I` bit of **`PSTATE.DAIF`**:
//!
//! | Bit | Name | Masks |
//! |-----|------|-------|
//! | 9   | `D`  | Debug exceptions |
//! | 8   | `A`  | SError (system errors) |
//! | 7   | `I`  | **IRQ** |
//! | 6   | `F`  | FIQ |
//!
//! Set the bit to **mask** (disable), clear it to **unmask** (enable). Note the polarity:
//! a `1` means "masked", which is the opposite of how most people read a flag called `I`.
//!
//! `msr daifset` / `msr daifclr` take a 4-bit immediate where bit 1 is `I`, so masking
//! IRQs is `msr daifset, #2`. These are single instructions that touch only the bits you
//! name, so there is no read-modify-write window to lose a race in.
//!
//! We only touch `I`. FIQ and SError stay as the boot protocol left them.

use core::arch::asm;

/// Bit 7 of DAIF, as read back by `mrs`.
const I_BIT: u64 = 1 << 7;

/// Are IRQs currently *unmasked*, i.e. can one fire right now?
pub fn enabled() -> bool {
    let daif: u64;
    // SAFETY: reads a system register. No side effects.
    unsafe { asm!("mrs {}, daif", out(reg) daif, options(nomem, nostack)) };
    daif & I_BIT == 0
}

/// Mask IRQs, and report whether they had been enabled.
///
/// The return value is the whole point. Callers must **restore** this state rather than
/// blindly enabling, or a lock taken inside an interrupt handler will re-enable interrupts
/// on release, inside a handler, which is a fine way to get a fault you cannot explain.
#[must_use = "the previous interrupt state must be restored, not discarded"]
pub fn disable() -> bool {
    let was_enabled = enabled();

    // SAFETY: masking IRQs is always sound. If an IRQ fires between the read above and
    // this instruction, it simply runs, and the state we read is still the truth about
    // what to restore.
    unsafe { asm!("msr daifset, #2", options(nomem, nostack)) };

    was_enabled
}

/// Put IRQs back the way [`disable`] found them.
pub fn restore(was_enabled: bool) {
    if was_enabled {
        // SAFETY: unmasking only when they were unmasked before.
        unsafe { asm!("msr daifclr, #2", options(nomem, nostack)) };
    }
}

/// Unconditionally unmask IRQs.
///
/// Only for the one place that legitimately turns interrupts on for the first time
/// (milestone 5's init) and for tests. Everywhere else wants [`disable`] + [`restore`].
#[allow(dead_code)] // milestone 5's first non-test caller
pub fn enable() {
    // SAFETY: unmasking IRQs is sound; whether it is *wise* is the caller's problem.
    unsafe { asm!("msr daifclr, #2", options(nomem, nostack)) };
}
