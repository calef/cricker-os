//! aarch64 support.
//!
//! Assembly, system registers, and CPU-specific behaviour live here and nowhere
//! else. When the Raspberry Pi port happens, this is the module that gets a
//! sibling, and everything above `arch::` should be untouched. See
//! notes/portability.md and DECISIONS.md §4.

use core::arch::global_asm;

pub mod exceptions;
pub mod semihosting;

// The kernel's actual entry point. `_start` lands in section `.text.boot`, which
// link.ld places first, at the ELF entry address QEMU jumps to.
global_asm!(include_str!("boot.s"));

// The exception vector table. VBAR_EL1 will point here once `init` runs.
global_asm!(include_str!("vectors.s"));

/// Bring the CPU into a state where the kernel can safely run.
///
/// Right now that means one thing: install the exception vectors, so that a fault
/// produces a report instead of a silent death. Note the ordering constraint in
/// `main.rs`: the console has to come up first, because the fault handler's whole
/// job is to *print*.
pub fn init() {
    exceptions::init();
}

/// Park this core forever.
///
/// `wfe` ("wait for event") sleeps the core at low power until something wakes it.
/// The loop is there because "something" includes spurious wakeups.
pub fn halt() -> ! {
    loop {
        aarch64_cpu::asm::wfe();
    }
}
