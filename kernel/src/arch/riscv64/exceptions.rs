//! **Traps, RISC-V.** The `stvec` vector, the saved [`TrapFrame`], and the dispatch into the
//! portable syscall and fault handlers. The S-mode analog of aarch64's `VBAR` table + `ESR` decode.
//!
//! Scaffold for the compile milestone: the frame type and the one externally-read counter are here,
//! `init` is the traps step. RISC-V has a single trap entry (`stvec`), not aarch64's 16-slot table;
//! interrupt-versus-exception is the top bit of `scause`, and the syscall path is the `ecall` cause.
//!
//! **A known ABI leak to resolve at the traps step:** portable `syscall.rs` reads the syscall number
//! from `frame.x[8]` and arguments from `frame.x[0..]`, which is the aarch64 `svc`+`x8` convention
//! (DECISIONS §10/§16). RISC-V's natural `ecall` ABI puts the number in `a7` (x17) and arguments in
//! `a0`..`a5` (x10..x15). The index array below makes it *compile*; making it *correct* means either
//! arranging this frame so the dispatcher's indices line up, or giving `syscall.rs` named accessors.
//! See notes/riscv-port.md.

use core::sync::atomic::AtomicUsize;

/// The registers saved on a trap. `x` is the RISC-V general-register file `x0`..`x31` (`x[0]` is the
/// hardwired zero); the trap CSRs follow. `#[repr(C)]` because the trap-entry assembly (the traps
/// step) will fill it field for field.
///
/// `x` is `pub` because the portable syscall dispatcher indexes it; the rest is arch-internal.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    /// The general registers `x0`..`x31`. `x0` is always zero; it is kept in the array so an index
    /// *is* a register number.
    pub x: [u64; 32],
    /// `sepc`: the PC the trap interrupted, where `sret` resumes.
    pub sepc: u64,
    /// `scause`: the trap cause (top bit = interrupt vs exception).
    pub scause: u64,
    /// `stval`: the trap value (faulting address, bad instruction, ...).
    pub stval: u64,
    /// `sstatus` at the trap, restored on the way out.
    pub sstatus: u64,
}

/// Interrupts routed to a userspace handler (delegated IRQs). Bumped by the trap dispatcher.
pub static ROUTED_IRQS: AtomicUsize = AtomicUsize::new(0);

/// System calls served (`ecall` from U-mode). Read by the boot tour; bumped by the trap dispatcher.
pub static SVC_COUNT: AtomicUsize = AtomicUsize::new(0);

/// User faults taken (a page fault or illegal instruction from U-mode). Read by the boot tour;
/// bumped by the trap dispatcher.
pub static USER_FAULTS: AtomicUsize = AtomicUsize::new(0);

/// Install the trap vector (`stvec`) and the trap stack. The traps step.
pub fn init() {
    unimplemented!("riscv trap init (stvec + trap entry + dispatch): the traps step")
}
