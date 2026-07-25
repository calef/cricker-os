//! EL0 microbenchmarks, measured the lmbench way (the cross-OS primitive suite, milestone 21+).
//!
//! The kernel-side benchmarks in `kernel/src/bench.rs` measure path length *inside* the kernel: the
//! bench threads are kernel threads calling `sched::` directly, so they never pay the EL0->EL1 trap.
//! lmbench measures from userspace, trap included, so to compare cricker-os to lmbench we have to
//! measure from **here**, at EL0, self-timing a loop of real `svc` syscalls. That is this program.
//!
//! It is spawned by the bench boot (`kernel/src/bench.rs`), self-times each primitive with
//! `user_rt::now` (the virtual counter, EL0-readable since milestone 19e), and SENDs `[ticks, iters]`
//! home on the one endpoint it was granted (slot 0). The bench boot prints it in the same
//! machine-readable line the rest of the harness uses, so the icount baseline gates it and `--real`
//! gives its true magnitude. One primitive for now: the null syscall. Context switch and IPC (which
//! need a second EL0 thread) follow.

#![no_std]
#![no_main]

use user_rt::{exit, now, send};

/// The endpoint the bench boot grants us (slot 0): we SEND `[ticks, iters, 0]` here per primitive.
const REPORT: u64 = 0;

/// Iterations for the null-syscall loop. Big enough that the two `now()` reads are noise and the
/// per-call cost averages cleanly; small enough that the loop is bearable under TCG (each `svc` is a
/// full emulated exception there). Self-timed, so the count is deterministic under icount and real
/// under HVF either way.
const NULL_ITERS: u64 = 20_000;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    // **Null syscall latency.** The cheapest possible boundary crossing: an unrecognized syscall
    // number, which the kernel rejects immediately (`Err(BadSyscall)`, no scheduler, no object
    // lookup). That isolates trap + dispatch + return, exactly what "null syscall" is meant to
    // measure, the way lmbench's `lat_syscall null` uses a syscall that does almost nothing.
    let start = now();
    for _ in 0..NULL_ITERS {
        null_syscall();
    }
    let ticks = now().wrapping_sub(start);
    send(REPORT, ticks, NULL_ITERS, 0);

    exit();
}

/// One null syscall: `svc` with an unrecognized number. The kernel returns an error in `x0`, which
/// we discard; we are timing the crossing, not the result.
#[inline(never)]
fn null_syscall() {
    // SAFETY: `svc` traps to EL1; an unknown number is rejected with an error and no side effect.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") 0xFFFFu64, // no defined syscall has this number
            lateout("x0") _,    // the kernel writes an error here; discard it
            options(nostack, nomem),
        );
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unsafe { core::arch::asm!("brk #0", options(nostack, nomem)) };
    loop {
        core::hint::spin_loop();
    }
}
