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

impl TrapFrame {
    /// Build the frame that drops a brand-new thread to U-mode at `entry` on `user_sp`, with `args`
    /// in `a0`..`a2`. The RISC-V side of the userspace-entry seam (notes/riscv-port.md, leak #3),
    /// mirroring aarch64's `for_user_entry`. `sret` will resume at `sepc` in the privilege named by
    /// `sstatus.SPP`: SPP = 0 is U-mode, and SPIE = 1 makes interrupts enabled after the return, so a
    /// tight-loop user thread stays preemptible (the RISC-V analog of aarch64's DAIF = 0).
    ///
    /// The register indices are the RISC-V ABI: `a0`..`a2` are `x10`..`x12`, `sp` is `x2`. This is
    /// also where the syscall-ABI reconciliation (the traps step) will settle, since the dispatcher
    /// reads its arguments from this same frame.
    pub fn for_user_entry(entry: u64, user_sp: u64, args: [u64; 3]) -> Self {
        const SPIE: u64 = 1 << 5; // sstatus.SPIE: interrupts enabled after sret (SPP stays 0 = U-mode)
        let mut x = [0u64; 32];
        x[10] = args[0]; // a0: _start's first argument
        x[11] = args[1]; // a1
        x[12] = args[2]; // a2
        x[2] = user_sp; // sp
        TrapFrame {
            x,
            sepc: entry, // where sret resumes
            scause: 0,
            stval: 0,
            sstatus: SPIE,
        }
    }
}

/// Drop to U-mode by loading `frame` and executing `sret`. The RISC-V side of the userspace-entry
/// seam. The traps step implements it (the U-mode `sret` path with the trap frame restore).
///
/// # Safety
/// As aarch64's `enter_user`: `frame` must be a correctly-built, writable `TrapFrame` at the top of
/// the current thread's kernel stack, with the user address space installed.
pub unsafe fn enter_user(frame: *mut TrapFrame) -> ! {
    let _ = frame;
    unimplemented!("riscv drop to U-mode (restore trap frame + sret): the traps step")
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
