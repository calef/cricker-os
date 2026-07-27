//! **Traps, RISC-V.** The `stvec` vector, the saved [`TrapFrame`], and the dispatch into the
//! portable syscall and fault handlers. The S-mode analog of aarch64's `VBAR` table + `ESR` decode.
//!
//! RISC-V has a single trap entry (`stvec`), not aarch64's 16-slot table; interrupt-versus-exception
//! is the top bit of `scause`, and the syscall path is the `ecall` cause. The trap-entry assembly is
//! in trap.s; it fills a [`TrapFrame`] and calls [`riscv_trap_dispatch`], which fans out on `scause`.
//! The syscall-ABI reconciliation is done: the portable dispatcher reads the number and arguments
//! through `TrapFrame::{syscall_nr, arg, set_arg}` (see this module's `impl`), so `ecall`'s a7/a0..a5
//! map correctly without `syscall.rs` naming a register.

use core::sync::atomic::{AtomicUsize, Ordering};

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
    /// The syscall number the caller passed. RISC-V `ecall` ABI: register `a7` (`x17`). The portable
    /// dispatcher reads it here so it never names a register directly. This, with `arg`/`set_arg`, is
    /// the resolution of the syscall-ABI leak flagged during the port (DECISIONS §17): aarch64 uses
    /// x8 + x0..x5, RISC-V uses a7 + a0..a5, and each maps its own registers.
    pub fn syscall_nr(&self) -> u64 {
        self.x[17] // a7
    }

    /// Syscall argument register `i` (RISC-V: `a0`..`a5`, i.e. `x10`..`x15`).
    pub fn arg(&self, i: usize) -> u64 {
        self.x[10 + i]
    }

    /// Set syscall argument/return register `i`. The return value and IPC message words ride in
    /// `a0`..`a2` (`x10`..`x12`).
    pub fn set_arg(&mut self, i: usize, v: u64) {
        self.x[10 + i] = v;
    }

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

/// Interrupts taken with no source enabled to explain them (should stay zero until the timer/PLIC
/// steps enable real sources).
pub static SPURIOUS_IRQS: AtomicUsize = AtomicUsize::new(0);

/// System calls served (`ecall` from U-mode). Read by the boot tour; bumped by the trap dispatcher.
pub static SVC_COUNT: AtomicUsize = AtomicUsize::new(0);

/// User faults taken (a page fault or illegal instruction from U-mode). Read by the boot tour;
/// bumped by the trap dispatcher.
pub static USER_FAULTS: AtomicUsize = AtomicUsize::new(0);

/// Breakpoints (`ebreak`) caught. Exists so a test can prove the trap round-trip actually ran,
/// rather than proving only that we did not crash. The aarch64 analog is its own `BRK_COUNT`.
pub static BRK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// The `sstatus.SPP` bit: the privilege the trap came from (0 = U-mode, 1 = S-mode).
const SPP: u64 = 1 << 8;
/// The high bit of `scause`: set for an interrupt, clear for an exception.
const INTERRUPT: u64 = 1 << 63;
/// `scause` exception code for `ecall` taken from U-mode: the syscall.
const CAUSE_ECALL_U: u64 = 8;
/// `scause` exception code for a breakpoint (`ebreak`).
const CAUSE_BREAKPOINT: u64 = 3;

/// Install the trap vector: `stvec` = [`trap_entry`], direct mode (all traps to one handler; the low
/// two bits of `stvec` select the mode and must be 0, which trap.s's `.balign 4` guarantees).
pub fn init() {
    unsafe extern "C" {
        fn trap_entry();
    }
    let vector = trap_entry as usize;
    // SAFETY: `vector` is our 4-byte-aligned trap entry; writing stvec has no memory effect.
    unsafe { core::arch::asm!("csrw stvec, {}", in(reg) vector, options(nomem, nostack)) };
}

/// Advance `sepc` past the instruction that trapped: 2 bytes if it is compressed (low two bits not
/// `0b11`), otherwise 4. Used to step over a handled breakpoint. `ecall` is always 4 bytes.
fn advance_past_trapping_insn(frame: &mut TrapFrame) {
    // SAFETY: `sepc` is the address of the instruction that trapped, which is mapped and readable.
    let low = unsafe { core::ptr::read_volatile(frame.sepc as *const u16) };
    frame.sepc += if low & 0b11 == 0b11 { 4 } else { 2 };
}

/// The Rust half of the trap path, called from trap.s with the saved [`TrapFrame`]. Fans out on
/// `scause`: an `ecall` from U-mode is a syscall; a breakpoint is a debug/self-test trap; an
/// interrupt goes to the (not-yet-built) interrupt path; anything else is a fault.
#[unsafe(no_mangle)]
extern "C" fn riscv_trap_dispatch(frame: &mut TrapFrame) {
    let scause = frame.scause;

    if scause & INTERRUPT != 0 {
        // Timer and external interrupts arrive here once the timer and PLIC steps enable them.
        // Nothing is enabled yet, so a stray interrupt is unexpected; count it and return.
        SPURIOUS_IRQS.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let code = scause & 0xff;
    let from_user = frame.sstatus & SPP == 0;

    match code {
        CAUSE_ECALL_U => {
            // The syscall. `sepc` points AT the `ecall` (unlike aarch64, where the hardware advances
            // ELR past `svc`), so step over it before dispatching, and `ecall` is always 4 bytes.
            SVC_COUNT.fetch_add(1, Ordering::Relaxed);
            frame.sepc += 4;
            crate::syscall::dispatch(frame);
        }
        CAUSE_BREAKPOINT => {
            BRK_COUNT.fetch_add(1, Ordering::Relaxed);
            advance_past_trapping_insn(frame);
        }
        _ => {
            USER_FAULTS.fetch_add(1, Ordering::Relaxed);
            // The user-fault path (kill the thread, keep the kernel) arrives with the user thread
            // path. Until a user thread can run on RISC-V there is nothing to kill, so a trap here is
            // a kernel bug: report it with the detail that makes it legible.
            panic!(
                "unexpected RISC-V trap: scause={scause:#x} (code {code}) stval={:#x} sepc={:#x} \
                 from_user={from_user}",
                frame.stval, frame.sepc,
            );
        }
    }
}

/// Prove the trap path works end to end: execute a breakpoint and return. If traps are wired,
/// [`riscv_trap_dispatch`] catches `scause` = breakpoint, [`advance_past_trapping_insn`] steps `sepc`
/// past the `ebreak`, and `sret` lands us right back here. If they are not, this never returns.
/// Returns the breakpoint count so the caller can confirm the handler actually ran.
pub fn self_test() -> usize {
    let before = BRK_COUNT.load(Ordering::Relaxed);
    // SAFETY: `ebreak` raises a breakpoint the dispatcher handles; it has no other effect.
    unsafe { core::arch::asm!("ebreak") };
    BRK_COUNT.load(Ordering::Relaxed) - before
}
