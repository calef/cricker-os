//! **The RISC-V (rv64) architecture layer.** The second implementation of the `arch/` contract
//! (milestone 20, notes/riscv-port.md), the one that proves rule #1: the rest of the kernel calls
//! everything here through `crate::arch`, exactly the names it calls on aarch64.
//!
//! This is a scaffold. The pieces that are pure and portable-in-spirit (the per-CPU register, the
//! halt/idle/barrier primitives, the initial thread contexts) are real. The pieces that are the
//! work of later steps (MMU/Sv39, traps, the timer, SMP bring-up, the test-exit) are loud
//! `unimplemented!()` stubs, each naming the step that fills it, so nobody mistakes a stub for a
//! working port. What is proved *today* is that the boundary is complete: a second architecture
//! compiles and links against the whole kernel with no change above `arch/`.

use core::arch::{asm, global_asm};

pub mod context;
pub mod exceptions;
pub mod interrupts;
pub mod mmu;
pub mod semihosting;
pub mod timer;

// The saved thread context and how a new one is faked (the Rust half of context.s). Re-exported
// flat so `crate::arch::{Context, switch_to}` names them regardless of architecture.
pub use context::{Context, switch_to};

// The S-mode entry (_start), the .bss zeroing, and the stack handoff to `kernel_main`.
global_asm!(include_str!("boot.s"));

// The context switch and the two first-run trampolines (the asm half of context.rs).
global_asm!(include_str!("context.s"));

/// Set this hart's per-CPU pointer. RISC-V's `tp` (thread pointer) is the direct analog of
/// aarch64's `TPIDR_EL1`: a scratch register the kernel owns in S-mode and reads to find the
/// current core's `PerCpu`. See `crate::percpu`.
pub fn set_percpu(ptr: usize) {
    // SAFETY: writes a general register the kernel reserves for per-CPU data. No memory effect.
    unsafe { asm!("mv tp, {}", in(reg) ptr, options(nomem, nostack, preserves_flags)) };
}

/// Read this hart's per-CPU pointer (the value last handed to [`set_percpu`]).
pub fn percpu() -> usize {
    let tp: usize;
    // SAFETY: reads a general register. No side effects.
    unsafe { asm!("mv {}, tp", out(reg) tp, options(nomem, nostack, preserves_flags)) };
    tp
}

/// Start a secondary hart. The SMP analog of aarch64's PSCI `CPU_ON`, implemented via the SBI HSM
/// (hart state management) extension `sbi_hart_start`. Filled in at the SMP step.
pub fn psci_cpu_on(target_hart: u64, entry: u64, context: u64) -> i64 {
    let _ = (target_hart, entry, context);
    unimplemented!("riscv SMP bring-up (SBI HSM sbi_hart_start): the timer + interrupts step")
}

/// Bring the architecture up: install the trap vector (`stvec`), start the timer, enable the
/// interrupt sources. Filled in at the traps step.
pub fn init() {
    unimplemented!("riscv arch init (stvec + timer + interrupts): the traps step")
}

/// Stop this hart forever, cheaply. `wfi` parks the hart until an interrupt; with nothing left to
/// wake it, that is the rest of time at zero host CPU. The same discipline as aarch64: `wfi`, never
/// a spin. See CLAUDE.md, "Never leave QEMU running".
pub fn halt() -> ! {
    loop {
        // SAFETY: wait-for-interrupt is always safe; it only affects when the next instruction runs.
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}

/// Park until the next interrupt (the scheduler's idle primitive).
pub fn wait_for_interrupt() {
    // SAFETY: as `halt`, but returns when an interrupt arrives.
    unsafe { asm!("wfi", options(nomem, nostack)) };
}

/// A DMA write memory barrier: order all prior stores before any device sees a later one. RISC-V's
/// `fence ow, ow` orders outer (device/IO) writes; the plain `fence` here is the conservative full
/// barrier, matching aarch64's `dsb sy`. Tightened when a real DMA driver lands.
pub fn dma_wmb() {
    // SAFETY: a fence has no memory effect of its own; it only constrains ordering.
    unsafe { asm!("fence", options(nostack, preserves_flags)) };
}
