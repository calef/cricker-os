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

use core::sync::atomic::{AtomicUsize, Ordering};

/// **The per-hart trap stash**, the thing `sscratch` points at in both U- and S-mode. RISC-V's `tp`
/// is a general register that U-mode owns, so a trap from U-mode arrives with the user's `tp` and the
/// kernel must recover its own per-CPU pointer from a *hart-private* source. That source is this
/// struct: `sscratch` holds `&TRAP_STASH[hart]`, and `trap.s` reads the kernel `tp` from `percpu` and
/// the kernel stack from `kernel_sp`. One global `KERNEL_TP` could not do this once there is more than
/// one hart (every hart would reload hart 0's pointer); an array indexed by hart, reached through the
/// per-hart `sscratch`, is what makes the trap path SMP-correct.
///
/// `#[repr(C)]` and the field order are load-bearing: `trap.s` accesses these by fixed byte offset
/// (0, 8, 16, 24), checked below. Each hart touches only its own entry, and only during its own trap,
/// so the `AtomicUsize`s are for interior mutability through the shared static, not cross-core
/// synchronization (the asm reads/writes them plainly).
#[repr(C)]
struct TrapStash {
    /// The current thread's kernel-stack top; where a U-mode trap lands. Set on every return to
    /// U-mode (trap.s `trap_return`), so it always names the thread about to run in U-mode.
    kernel_sp: AtomicUsize,
    /// This hart's `PerCpu` pointer: the kernel `tp` the trap entry restores.
    percpu: AtomicUsize,
    /// Two scratch words the trap entry uses to free registers before it has a stack.
    scratch0: AtomicUsize,
    scratch1: AtomicUsize,
}

impl TrapStash {
    const fn new() -> Self {
        Self {
            kernel_sp: AtomicUsize::new(0),
            percpu: AtomicUsize::new(0),
            scratch0: AtomicUsize::new(0),
            scratch1: AtomicUsize::new(0),
        }
    }
}

// trap.s hardcodes these offsets; keep them honest.
const _: () = {
    assert!(core::mem::offset_of!(TrapStash, kernel_sp) == 0);
    assert!(core::mem::offset_of!(TrapStash, percpu) == 8);
    assert!(core::mem::offset_of!(TrapStash, scratch0) == 16);
    assert!(core::mem::offset_of!(TrapStash, scratch1) == 24);
};

/// One trap stash per hart, indexed like `cpu::PERCPU`. A static so it exists before any allocator,
/// which the very first trap (and every secondary's bring-up) needs.
static TRAP_STASH: [TrapStash; crate::cpu::MAX_CPUS] =
    [const { TrapStash::new() }; crate::cpu::MAX_CPUS];

/// Set this hart's per-CPU pointer. RISC-V's `tp` (thread pointer) is the analog of aarch64's
/// `TPIDR_EL1`, but a general register, so this also arms the per-hart trap path: it records the
/// pointer in this hart's [`TrapStash`] and points `sscratch` at that stash, so `trap.s` can recover
/// the kernel `tp` after a U-mode round trip. See `crate::percpu` and trap.s.
pub fn set_percpu(ptr: usize) {
    // Set `tp` first so `cpu::id()` (which reads `tp`) resolves this hart's index into TRAP_STASH.
    // SAFETY: writes a general register the kernel reserves for per-CPU data. No memory effect.
    unsafe { asm!("mv tp, {}", in(reg) ptr, options(nomem, nostack, preserves_flags)) };
    let stash = &TRAP_STASH[crate::cpu::id()];
    stash.percpu.store(ptr, Ordering::Relaxed);
    let stash_ptr = stash as *const TrapStash as usize;
    // SAFETY: `sscratch` now names this hart's stash; trap.s reads it as `&TrapStash` on every trap.
    unsafe {
        asm!("csrw sscratch, {}", in(reg) stash_ptr, options(nomem, nostack, preserves_flags))
    };
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
