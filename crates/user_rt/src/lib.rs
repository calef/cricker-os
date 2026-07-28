//! **user_rt**: the tiny EL0 runtime shared by cricker-os userspace programs (milestone 19f.6).
//!
//! One syscall wrapper (`invoke`) and the three things every program builds on it: `send`, `recv`,
//! and `exit`. That is the whole crate. It exists because milestones 19f.2-5 split the userspace
//! into distinct binaries (`worker`, `console`, `input`, `shell`, plus `hello`), each of which had
//! copied these functions verbatim. The extraction waited on purpose until the split was done: only
//! then was the shared surface known rather than guessed, which is the DECISIONS rule about not
//! building an abstraction before its requirements exist.
//!
//! Milestone 27 added one more thing every program *may* build on: [`heap`], the untyped-backed
//! `GlobalAlloc` that turns the budget a program was granted into `Vec` and `String`. It is a
//! module, not a default: a program that never allocates links no allocator.
//!
//! What is deliberately **not** here: the `#[panic_handler]`. A panic handler is per-final-binary,
//! and putting one in this library would force it on every program that links the crate and collide
//! with any program that wants its own (as `hello` does). Each binary keeps its own one-line handler;
//! it is trivial and it keeps the linking simple. Device helpers (a UART `putc`, echo logic) also
//! stay in the drivers that own them: those are not runtime, they are the program.
//!
//! # Two ABIs, one surface
//!
//! The syscall instruction and the register file differ by architecture, and this is the one place
//! in userspace that names them. aarch64 uses `svc #0` with the syscall number in `x8` and arguments
//! in `x0..x5`; RISC-V uses `ecall` with the number in `a7` and arguments in `a0..a5`. Both return in
//! the first argument register (`x0` / `a0`). The kernel reconciles the two in `TrapFrame`
//! (DECISIONS §17); here we simply select the right asm at compile time. Every function's signature,
//! semantics, and the `abi` constants are identical across both.

#![no_std]

pub mod heap;

/// Invoke a capability: the one syscall a userspace program makes. `cap` names a capability in the
/// process's cspace, `method` selects the operation, and `a0..a2` are its arguments; the return is
/// the kernel's `i64` result. Everything else in this crate is built on this.
///
/// # Safety
/// `svc`/`ecall` traps to the kernel. The kernel validates the capability and the method before
/// acting; that is its whole job. The caller is trusting the kernel, not the other way around.
#[cfg(target_arch = "aarch64")]
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

/// Invoke a capability (RISC-V). See the aarch64 twin above for the contract; only the trap
/// instruction and register file differ: `ecall`, number in `a7`, args in `a0..a4`, result in `a0`.
///
/// # Safety
/// `ecall` traps to the kernel, which validates the capability and method before acting. Same
/// contract as the aarch64 twin: the caller trusts the kernel, not the other way around.
#[cfg(target_arch = "riscv64")]
pub unsafe fn invoke(cap: u64, method: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") abi::SYS_INVOKE,
            inlateout("a0") cap => ret,
            in("a1") method,
            in("a2") a0,
            in("a3") a1,
            in("a4") a2,
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
#[cfg(target_arch = "aarch64")]
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

/// `RECV` three words (RISC-V). See the aarch64 twin; `ecall`, slot in `a0`, `RECV` in `a1`, the
/// three returned words in `a0`/`a1`/`a2`.
#[cfg(target_arch = "riscv64")]
pub fn recv(slot: u64) -> (u64, u64, u64) {
    let (mut w0, mut w1, mut w2): (u64, u64, u64);
    // SAFETY: `ecall`. RECV returns three words in a0/a1/a2.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") abi::SYS_INVOKE,
            inlateout("a0") slot => w0,
            inlateout("a1") abi::endpoint::RECV => w1,
            lateout("a2") w2,
            in("a3") 0u64,
            in("a4") 0u64,
            options(nostack),
        );
    }
    (w0, w1, w2)
}

/// Give up the CPU (`SYS_YIELD`). Returns when the scheduler runs this thread again; if another
/// thread is ready, control goes there and back, which is one context-switch round trip.
#[cfg(target_arch = "aarch64")]
pub fn yield_now() {
    // SAFETY: `svc`; SYS_YIELD gives up the CPU and returns with nothing to clean up.
    unsafe {
        core::arch::asm!("svc #0", in("x8") abi::SYS_YIELD, options(nostack, nomem));
    }
}

/// Give up the CPU (RISC-V). `ecall`, `SYS_YIELD` in `a7`.
#[cfg(target_arch = "riscv64")]
pub fn yield_now() {
    // SAFETY: `ecall`; SYS_YIELD gives up the CPU and returns with nothing to clean up.
    unsafe {
        core::arch::asm!("ecall", in("a7") abi::SYS_YIELD, options(nostack, nomem));
    }
}

/// Drop the capability in `slot` from this thread's cspace (`SYS_CAP_DELETE`). Deleting an empty
/// slot is a no-op. A program that retypes many objects (a loader, a spawner) frees each slot as
/// soon as it is done with it, so its fixed cspace does not fill.
#[cfg(target_arch = "aarch64")]
pub fn cap_delete(slot: u64) {
    // SAFETY: `svc`; SYS_CAP_DELETE frees a slot in the caller's own cspace, nothing to clean up.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_CAP_DELETE,
            in("x0") slot,
            options(nostack, nomem),
        );
    }
}

/// Drop the capability in `slot` (RISC-V). `ecall`, `SYS_CAP_DELETE` in `a7`, slot in `a0`.
#[cfg(target_arch = "riscv64")]
pub fn cap_delete(slot: u64) {
    // SAFETY: `ecall`; SYS_CAP_DELETE frees a slot in the caller's own cspace, nothing to clean up.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") abi::SYS_CAP_DELETE,
            in("a0") slot,
            options(nostack, nomem),
        );
    }
}

/// The virtual counter, `CNTVCT_EL0`: a monotonic tick count for self-timing. Readable at EL0 only
/// because the kernel opened `CNTKCTL_EL1.EL0VCTEN` (see kernel timer::init and notes/abi.md); the
/// read is a plain register move, no syscall. Pair with [`cntfrq`] to turn tick deltas into seconds.
#[cfg(target_arch = "aarch64")]
pub fn now() -> u64 {
    let t: u64;
    // SAFETY: reading a system register the kernel made EL0-readable. No side effects.
    unsafe {
        core::arch::asm!("mrs {}, cntvct_el0", out(reg) t, options(nomem, nostack));
    }
    t
}

/// The monotonic tick count (RISC-V): `rdtime`, which reads the `time` CSR. Readable from U-mode
/// only because the kernel sets `scounteren.TM`; without that bit `rdtime` traps as illegal, the
/// same shape as aarch64 needing `CNTKCTL_EL1.EL0VCTEN`. Pair with [`cntfrq`] to get seconds.
#[cfg(target_arch = "riscv64")]
pub fn now() -> u64 {
    let t: u64;
    // SAFETY: reading the time CSR the kernel made U-mode-readable. No side effects.
    unsafe {
        core::arch::asm!("rdtime {}", out(reg) t, options(nomem, nostack));
    }
    t
}

/// The counter frequency in Hz, `CNTFRQ_EL0`: how many [`now`] ticks make a second. Constant for the
/// life of the machine (QEMU reports 62.5 MHz under TCG, the host's counter frequency under HVF).
#[cfg(target_arch = "aarch64")]
pub fn cntfrq() -> u64 {
    let f: u64;
    // SAFETY: reading a system register; EL0-readable once EL0VCTEN is set.
    unsafe {
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack));
    }
    f
}

/// The counter frequency in Hz (RISC-V). Unlike aarch64's `CNTFRQ_EL0`, RISC-V has **no** register
/// that reports the timebase; it lives in the device tree's `timebase-frequency` (10 MHz on QEMU
/// `virt`), which userspace cannot read. This is a real ABI gap: a complete port hands the frequency
/// to a process at start (an aux-vector entry, the way Linux passes `AT_HWCAP`). Until that exists,
/// this returns the known QEMU `virt` constant so self-timing works on the one machine we run.
#[cfg(target_arch = "riscv64")]
pub fn cntfrq() -> u64 {
    10_000_000
}

/// Terminate this process. The kernel reaps the thread and frees its whole address space. Never
/// returns; the trailing spin is only there to satisfy the `-> !` type if `svc` ever came back.
pub fn exit() -> ! {
    // SAFETY: the syscall never returns; the trailing spin only satisfies the `-> !` type.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("svc #0", in("x8") abi::SYS_EXIT, in("x0") 0u64, options(nostack, nomem));
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ecall", in("a7") abi::SYS_EXIT, in("a0") 0u64, options(nostack, nomem));
    }
    loop {
        core::hint::spin_loop();
    }
}
