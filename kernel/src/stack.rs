//! Stack overflow detection.
//!
//! # Why this exists
//!
//! Milestone 3 hung the machine. A test put a 16 KiB array on the 64 KiB boot stack,
//! `sp` walked below `__stack_bottom`, and the frame wrote straight through `.bss`,
//! `.data`, and into `.text`. The kernel then executed its own corrupted code.
//!
//! There was no crash, no fault, no message. It hung *while printing*, and the print
//! had nothing to do with it. The bug was thousands of instructions upstream, in a
//! function prologue that had already returned.
//!
//! That is the worst failure mode this codebase can produce, and it was completely
//! invisible. So now it isn't.
//!
//! # What the real fix is
//!
//! A **guard page**: leave the page below the stack unmapped, and the MMU faults the
//! instant anything touches it. Precise, free at runtime, and impossible to miss.
//!
//! We can't do that yet, because we have no MMU. That's milestone 4, and the TODO is
//! recorded in link.ld.
//!
//! Until then, a canary. It is strictly worse than a guard page: it detects the damage
//! *after* it has happened rather than preventing it, and only if the overflow actually
//! wrote over the canary words. But it turns "the machine went insane for no reason"
//! into "you blew the stack," which is the difference between an afternoon and five
//! seconds.

use core::ffi::c_void;

/// Written at the very bottom of the stack. Nothing legitimate should ever touch it.
///
/// Four words rather than one: a big stack frame decrements `sp` past the bottom and
/// then writes throughout the frame, so a wider target is more likely to be hit.
// Arbitrary, but deliberately not zero (fresh RAM and `.bss` are full of zeroes) and not
// a plausible pointer or small integer, so a stray write is unlikely to reproduce one by
// accident.
const CANARY: [u64; 4] = [
    0x57ac_c0de_57ac_c0de,
    0xc0ff_ee00_1eaf_babe,
    0xdead_c0de_5111_c0de,
    0xfeed_face_cafe_f00d,
];

/// Paint the canary. Call this before anything can use much stack.
pub fn init() {
    // SAFETY: `__stack_bottom` is inside our own image, and by definition nothing has
    // pushed a frame that deep, or we would already be dead.
    unsafe {
        core::ptr::write_volatile(bottom() as *mut [u64; 4], CANARY);
    }
}

/// Has anything scribbled below the stack?
pub fn intact() -> bool {
    // SAFETY: reading our own image.
    unsafe { core::ptr::read_volatile(bottom() as *const [u64; 4]) == CANARY }
}

/// How many bytes are left between `sp` and the bottom of the stack.
///
/// Negative means we are *already* below it and are actively corrupting the kernel.
pub fn headroom() -> i64 {
    let sp: u64;
    // SAFETY: reads a register.
    unsafe { core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
    sp.wrapping_sub(bottom()) as i64
}

/// Shout if the canary is dead. Called from the panic handler and the fault handler,
/// because a corrupted stack makes every *other* diagnostic a potential lie.
pub fn warn_if_smashed() {
    if !intact() {
        crate::println!();
        crate::println!("  *** STACK OVERFLOW ***");
        crate::println!("  The canary below __stack_bottom is dead, so we have written");
        crate::println!("  through our own .bss/.data/.text. Nothing printed above this");
        crate::println!("  line can be trusted. See notes/stack.md.");
        crate::println!("  headroom: {} bytes", headroom());
    }
}

fn bottom() -> u64 {
    unsafe extern "C" {
        static __stack_bottom: c_void;
    }
    (&raw const __stack_bottom) as u64
}

#[cfg(test)]
mod tests {
    //! Tests for stack overflow detection.

    /// Proves the stack canary works, without actually smashing the stack.
    ///
    /// The runner checks this after every test (see testing.rs), so a test that blows the
    /// stack is now caught immediately and by name, rather than corrupting the kernel and
    /// hanging somewhere unrelated. That is exactly how milestone 3 went wrong.
    #[test_case]
    fn stack_canary_is_intact_and_we_have_headroom() {
        assert!(crate::stack::intact(), "stack canary is already dead");
        assert!(
            crate::stack::headroom() > 4096,
            "less than 4 KiB of stack left: {}",
            crate::stack::headroom()
        );
    }
}
