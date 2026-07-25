//! **user_rt**: the tiny EL0 runtime shared by cricker-os userspace programs (milestone 19f.6).
//!
//! One syscall wrapper (`invoke`) and the three things every program builds on it: `send`, `recv`,
//! and `exit`. That is the whole crate. It exists because milestones 19f.2-5 split the userspace
//! into distinct binaries (`worker`, `console`, `input`, `shell`, plus `hello`), each of which had
//! copied these functions verbatim. The extraction waited on purpose until the split was done: only
//! then was the shared surface known rather than guessed, which is the DECISIONS rule about not
//! building an abstraction before its requirements exist.
//!
//! What is deliberately **not** here: the `#[panic_handler]`. A panic handler is per-final-binary,
//! and putting one in this library would force it on every program that links the crate and collide
//! with any program that wants its own (as `hello` does). Each binary keeps its own one-line handler;
//! it is trivial and it keeps the linking simple. Device helpers (a UART `putc`, echo logic) also
//! stay in the drivers that own them: those are not runtime, they are the program.

#![no_std]

/// Invoke a capability: the one syscall a userspace program makes. `cap` names a capability in the
/// process's cspace, `method` selects the operation, and `a0..a2` are its arguments; the return is
/// the kernel's `i64` result. Everything else in this crate is built on this.
///
/// # Safety
/// `svc` traps to EL1. The kernel validates the capability and the method before acting; that is
/// its whole job. The caller is trusting the kernel, not the other way around.
pub unsafe fn invoke(cap: u64, method: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_INVOKE,
            inlateout("x0") cap => ret,
            in("x1") method,
            in("x2") a0,
            in("x3") a1,
            in("x4") a2,
            options(nostack),
        );
    }
    ret
}

/// `SEND` three words on the endpoint capability in `slot`. Blocks until a receiver takes them.
pub fn send(slot: u64, w0: u64, w1: u64, w2: u64) -> i64 {
    // SAFETY: `svc` traps to EL1, which validates the capability named by `slot`.
    unsafe { invoke(slot, abi::endpoint::SEND, w0, w1, w2) }
}

/// `RECV` three words on the endpoint capability in `slot`. Blocks until a sender arrives; returns
/// the three words the sender passed in `x0`, `x1`, `x2`.
pub fn recv(slot: u64) -> (u64, u64, u64) {
    let (mut w0, mut w1, mut w2): (u64, u64, u64);
    // SAFETY: `svc`. RECV returns three words in x0/x1/x2.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_INVOKE,
            inlateout("x0") slot => w0,
            in("x1") abi::endpoint::RECV,
            lateout("x1") w1,
            lateout("x2") w2,
            in("x3") 0u64,
            in("x4") 0u64,
            options(nostack),
        );
    }
    (w0, w1, w2)
}

/// The virtual counter, `CNTVCT_EL0`: a monotonic tick count for self-timing. Readable at EL0 only
/// because the kernel opened `CNTKCTL_EL1.EL0VCTEN` (see kernel timer::init and notes/abi.md); the
/// read is a plain register move, no syscall. Pair with [`cntfrq`] to turn tick deltas into seconds.
pub fn now() -> u64 {
    let t: u64;
    // SAFETY: reading a system register the kernel made EL0-readable. No side effects.
    unsafe {
        core::arch::asm!("mrs {}, cntvct_el0", out(reg) t, options(nomem, nostack));
    }
    t
}

/// The counter frequency in Hz, `CNTFRQ_EL0`: how many [`now`] ticks make a second. Constant for the
/// life of the machine (QEMU reports 62.5 MHz under TCG, the host's counter frequency under HVF).
pub fn cntfrq() -> u64 {
    let f: u64;
    // SAFETY: reading a system register; EL0-readable once EL0VCTEN is set.
    unsafe {
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack));
    }
    f
}

/// Terminate this process. The kernel reaps the thread and frees its whole address space. Never
/// returns; the trailing spin is only there to satisfy the `-> !` type if `svc` ever came back.
pub fn exit() -> ! {
    // SAFETY: `svc`; SYS_EXIT never returns.
    unsafe {
        core::arch::asm!("svc #0", in("x8") abi::SYS_EXIT, in("x0") 0u64, options(nostack, nomem));
    }
    loop {
        core::hint::spin_loop();
    }
}
