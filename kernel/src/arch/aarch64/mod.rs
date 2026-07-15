//! aarch64 support.
//!
//! Assembly, system registers, and CPU-specific behaviour live here and nowhere
//! else. When the Raspberry Pi port happens, this is the module that gets a
//! sibling, and everything above `arch::` should be untouched. See
//! notes/portability.md and DECISIONS.md §4.

use core::arch::global_asm;

pub mod exceptions;
pub mod interrupts;
pub mod mmu;
pub mod semihosting;
pub mod timer;

// The arm64 Image header. `_start` lands at byte 0 of the image, which is where QEMU
// begins executing. It does nothing but branch to `_boot`.
global_asm!(include_str!("image_header.s"));

// The real entry point.
global_asm!(include_str!("boot.s"));

// The exception vector table. VBAR_EL1 will point here once `init` runs.
global_asm!(include_str!("vectors.s"));

// The context switch, and where a new thread begins. Milestone 6.
global_asm!(include_str!("context.s"));

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
/// **`wfi`, not `wfe`, and the difference is not academic.**
///
/// `wfe` waits for an *event*: an `sev` from another core, or a lock release. QEMU's
/// emulation treats it as little more than a hint, so `loop { wfe() }` keeps translating
/// and executing, and a halted kernel burns **99.7% of a host CPU core**. We discovered
/// this the way you'd expect: eleven abandoned QEMU processes cooking the laptop overnight
/// at a combined 729%.
///
/// `wfi` waits for an *interrupt*, and QEMU implements it as an actual vCPU halt: the host
/// thread sleeps. An idle kernel becomes genuinely idle.
///
/// It is also the more correct instruction for what we mean. We are not waiting for an
/// event from a sibling core. We are idling until something interrupts us, of which there
/// is currently nothing, which is exactly the point.
pub fn halt() -> ! {
    loop {
        aarch64_cpu::asm::wfi();
    }
}

/// Wait for one interrupt, then return. **The idle thread's whole body.**
///
/// When every other thread is blocked (all waiting on I/O, say), the scheduler runs the idle
/// thread, which parks the CPU here until *something* interrupts: the timer, or the device a
/// blocked driver is waiting on. The handler may wake a thread; when `wfi` returns, the idle
/// thread yields and the scheduler picks up whatever became runnable.
///
/// `wfi`, not `wfe`, for the reason in `halt`: QEMU implements `wfi` as a real vCPU halt (the
/// host thread sleeps), so an idle kernel is genuinely idle. See notes/scheduler and CLAUDE.md.
pub fn wait_for_interrupt() {
    aarch64_cpu::asm::wfi();
}
