//! The std runtime contract, and the syscall glue that meets it.
//!
//! This is the PAL's twin of `crates/user_rt`: the same `svc #0` / `ecall` instructions, the
//! same register convention, deliberately re-stated here because std cannot depend on an
//! out-of-tree crate. The ABI *constants* are not re-stated: `abi.rs` next door is generated
//! verbatim from `crates/abi/src/lib.rs` by `cargo xtask std-src`, so the numbers cannot drift.
//! Only these few asm wrappers are hand-copied; if `user_rt`'s change, change these.
//!
//! # The std slot convention
//!
//! A std program's loader owes it, per the out-of-band-contract rule of notes/abi.md §4:
//!
//! - **slot 0**: an untyped budget. The global allocator draws heap pages from it lazily via
//!   `untyped::MAP`, at [`HEAP_BASE`], capped at [`HEAP_MAX`].
//! - **slot 1**: an endpoint with WRITE. `stdout` and `stderr` SEND here, 16 bytes per message
//!   (w0 = byte count, w1|w2 = the bytes, little-endian). Interleaving of out and err is the
//!   phase-one price of one endpoint; milestone 28's terminal contract owns fixing it.
//!
//! Programs that never allocate or print never touch the slots they do not use.

pub const UNTYPED_SLOT: u64 = 0;
pub const STDOUT_SLOT: u64 = 1;

/// Where the heap lives: 1 GiB, clear of the program image (0x40_0000), stacks, shared pages,
/// and the initrd window (0x2000_0000). Same value as `user_rt::heap::DEFAULT_BASE`.
pub const HEAP_BASE: u64 = 0x4000_0000;

/// The heap's growth cap. Generous because the untyped budget is the real, per-program limit
/// (`untyped::MAP` returns OutOfMemory when it is spent); this only bounds the VA range.
pub const HEAP_MAX: u64 = 256 * 1024 * 1024;

use super::abi;

/// Invoke a capability. See `crates/user_rt::invoke`, of which this is a verbatim twin.
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

/// Invoke a capability (RISC-V): `ecall`, number in `a7`, args in `a0..a4`, result in `a0`.
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

/// SEND three words on the endpoint in `slot`. Blocks until a receiver takes them.
pub fn send(slot: u64, w0: u64, w1: u64, w2: u64) -> i64 {
    unsafe { invoke(slot, abi::endpoint::SEND, w0, w1, w2) }
}

/// Give up the CPU (`SYS_YIELD`); the timed sleep loop is built on this.
pub fn yield_now() {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("svc #0", in("x8") abi::SYS_YIELD, options(nostack, nomem));
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ecall", in("a7") abi::SYS_YIELD, options(nostack, nomem));
    }
}

/// Terminate this process (`SYS_EXIT`). The kernel reaps the thread and frees the address space.
pub fn exit(code: i64) -> ! {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_EXIT,
            in("x0") code as u64,
            options(nostack, nomem),
        );
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") abi::SYS_EXIT,
            in("a0") code as u64,
            options(nostack, nomem),
        );
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Fault on purpose (`brk` / `ebreak`): the kernel kills the process and reports where. This is
/// `abort()` on an OS whose failure story is "a fault the kernel attributes", and it is what
/// `panic!` reaches after printing, since the target is panic=abort.
pub fn abort() -> ! {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem));
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem));
    }
    loop {
        core::hint::spin_loop();
    }
}

/// The monotonic tick count: the one ambient readable this ABI grants (notes/abi.md, "the one
/// ambient thing"). aarch64 `CNTVCT_EL0`; RISC-V `rdtime`.
pub fn now() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        let t: u64;
        unsafe {
            core::arch::asm!("mrs {}, cntvct_el0", out(reg) t, options(nomem, nostack));
        }
        t
    }
    #[cfg(target_arch = "riscv64")]
    {
        let t: u64;
        unsafe {
            core::arch::asm!("rdtime {}", out(reg) t, options(nomem, nostack));
        }
        t
    }
}

/// Ticks per second. aarch64 reports it in `CNTFRQ_EL0`; RISC-V has no architectural register
/// for the timebase, so this is the QEMU `virt` constant, the same honest gap `user_rt::cntfrq`
/// records (10 MHz until the ABI grows an aux-vector-style handoff).
pub fn cntfrq() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        let f: u64;
        unsafe {
            core::arch::asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack));
        }
        f
    }
    #[cfg(target_arch = "riscv64")]
    {
        10_000_000
    }
}
