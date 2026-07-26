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

use user_rt::{exit, now, send, yield_now};

/// The endpoint the bench boot grants us (slot 0): we SEND `[ticks, iters, 0]` here per primitive.
const REPORT: u64 = 0;

// Which primitive to measure, chosen by `x0` at `START` (the bench boot picks). A benchmark program
// is exactly the place a role selector still earns its keep: one binary, one micro-measurement each.
const ROLE_NULL_SYSCALL: u64 = 0;
const ROLE_YIELDER: u64 = 1;
const ROLE_CTX_SWITCH: u64 = 2;

/// Iterations for the null-syscall loop. Big enough that the two `now()` reads are noise and the
/// per-call cost averages cleanly; small enough that the loop is bearable under TCG (each `svc` is a
/// full emulated exception there). Self-timed, so the count is deterministic under icount and real
/// under HVF either way.
const NULL_ITERS: u64 = 20_000;

/// Iterations for the context-switch loop. Each is one `SYS_YIELD` that hands the CPU to the peer
/// and gets it back: a round trip of two switches (each an address-space change, since the peer is a
/// separate process). Fewer than the null loop: a switch is much heavier under TCG.
const CTX_ITERS: u64 = 5_000;

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, _x1: u64, _x2: u64) -> ! {
    match role {
        ROLE_NULL_SYSCALL => null_syscall_bench(),
        ROLE_YIELDER => yielder(),
        ROLE_CTX_SWITCH => ctx_switch_bench(),
        _ => exit(),
    }
}

/// **Null syscall latency.** The cheapest possible boundary crossing: an unrecognized syscall
/// number, which the kernel rejects immediately (`Err(BadSyscall)`, no scheduler, no object lookup).
/// That isolates trap + dispatch + return, exactly what "null syscall" is meant to measure, the way
/// lmbench's `lat_syscall null` uses a syscall that does almost nothing.
fn null_syscall_bench() -> ! {
    let start = now();
    for _ in 0..NULL_ITERS {
        null_syscall();
    }
    let ticks = now().wrapping_sub(start);
    send(REPORT, ticks, NULL_ITERS, 0);
    exit();
}

/// **The peer for the context-switch benchmark.** Yields so the timer process always has exactly
/// one other ready thread to switch to. Capped, not infinite: it must outlast the timer's warmup +
/// timed loop (so the timer always has a peer to switch to), then exit rather than spin, so it does
/// not burn a core after the measurement (CLAUDE.md's no-busy-halt rule). The cap is comfortably
/// larger than the timer's yield count.
fn yielder() -> ! {
    for _ in 0..(2 * CTX_ITERS + 4096) {
        yield_now();
    }
    exit();
}

/// **Context switch latency, measured from EL0.** With the yielder as the only other ready thread,
/// each `SYS_YIELD` here switches to it and back: two context switches, each including an address-
/// space change because the peer is a separate process (this is lmbench's `lat_ctx`, process flavor).
fn ctx_switch_bench() -> ! {
    // Warm up: let both processes reach steady state before the timed loop.
    for _ in 0..64 {
        yield_now();
    }
    let start = now();
    for _ in 0..CTX_ITERS {
        yield_now();
    }
    let ticks = now().wrapping_sub(start);
    send(REPORT, ticks, CTX_ITERS, 0);
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
