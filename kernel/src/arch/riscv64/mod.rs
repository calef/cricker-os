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
pub mod irq;
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

// The S-mode trap vector (the asm half of exceptions.rs): save the frame, dispatch, restore, sret.
global_asm!(include_str!("trap.s"));

/// This hart's kernel per-CPU pointer, kept where `trap.s` can reload `tp` from it on a trap from
/// U-mode. Unlike aarch64's `TPIDR_EL1` (a system register that survives an EL0 round trip), RISC-V's
/// `tp` is a general register that U-mode owns, so the trap entry must restore the kernel's from a
/// hart-private source rather than trust the register. **Single-hart for now:** one global holds hart
/// 0's pointer; SMP wants a per-hart slot (the sscratch-trapframe approach), noted in riscv-port.md.
#[unsafe(no_mangle)]
static KERNEL_TP: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Set this hart's per-CPU pointer. RISC-V's `tp` (thread pointer) is the analog of aarch64's
/// `TPIDR_EL1`. It is a general register, so we also stash it in [`KERNEL_TP`] for the trap entry to
/// restore after a U-mode round trip (see `crate::percpu` and trap.s).
pub fn set_percpu(ptr: usize) {
    KERNEL_TP.store(ptr, core::sync::atomic::Ordering::Relaxed);
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

/// This core's current stack pointer, for the stack-overflow canary check (stack.rs).
pub fn current_sp() -> u64 {
    let sp: u64;
    // SAFETY: reads a register. No side effects.
    unsafe { asm!("mv {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags)) };
    sp
}

/// A DMA write memory barrier: order all prior stores before any device sees a later one. RISC-V's
/// `fence ow, ow` orders outer (device/IO) writes; the plain `fence` here is the conservative full
/// barrier, matching aarch64's `dsb sy`. Tightened when a real DMA driver lands.
pub fn dma_wmb() {
    // SAFETY: a fence has no memory effect of its own; it only constrains ordering.
    unsafe { asm!("fence", options(nostack, preserves_flags)) };
}

/// Make the instruction fetcher aware of code just written as data. Where aarch64 needs a
/// clean/invalidate loop over cache lines, RISC-V has one instruction: `fence.i` synchronizes this
/// hart's instruction stream with its prior stores. It has no address range (it covers everything),
/// so `va`/`len` are ignored; a multi-hart port will additionally need to fence the other harts. See
/// notes/riscv-port.md, leak #3.
pub fn sync_icache(va: u64, len: usize) {
    let _ = (va, len);
    // SAFETY: `fence.i` only orders instruction fetch against prior stores on this hart.
    unsafe { asm!("fence.i", options(nostack, preserves_flags)) };
}
