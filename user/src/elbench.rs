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
//! gives its true magnitude. Four primitives so far: null syscall, context switch, IPC round trip,
//! and page map. Spawn waits on retype-from-untyped (a repeatable spawn loop needs fresh TCBs, which
//! that milestone provides), so it is not here yet.

#![no_std]
#![no_main]

use user_rt::{exit, invoke, now, recv, send, yield_now};

/// The endpoint the bench boot grants us (slot 0): we SEND `[ticks, iters, 0]` here per primitive.
const REPORT: u64 = 0;

// Which primitive to measure, chosen by `x0` at `START` (the bench boot picks). A benchmark program
// is exactly the place a role selector still earns its keep: one binary, one micro-measurement each.
const ROLE_NULL_SYSCALL: u64 = 0;
const ROLE_YIELDER: u64 = 1;
const ROLE_CTX_SWITCH: u64 = 2;
const ROLE_IPC_SERVER: u64 = 3;
const ROLE_IPC_CLIENT: u64 = 4;
const ROLE_MAP: u64 = 5;

// Map-benchmark slots (slot 0 is always REPORT). The bench boot grants a WRITE cap on a target
// address space and a READ cap on one frame; we map that frame at a fresh VA each iteration.
const MAP_ASPACE: u64 = 1;
const MAP_FRAME: u64 = 2;

// IPC round-trip slots. The server holds two endpoints; the client holds three (report first, so
// slot 0 stays "the endpoint I report on" across every reporting role).
const SRV_REQUEST: u64 = 0; // server RECVs a request here
const SRV_REPLY: u64 = 1; // server SENDs the reply here
const CLI_REQUEST: u64 = 1; // client SENDs the request here (slot 0 is REPORT)
const CLI_REPLY: u64 = 2; // client RECVs the reply here

/// Iterations for the IPC round-trip loop. One iteration is a `SEND` to the server and a `RECV` of
/// its reply: two rendezvous, four `svc`s (two on each side), two context switches. lmbench's
/// `lat_pipe` shape, over our endpoints.
const IPC_ITERS: u64 = 5_000;

/// Iterations for the null-syscall loop. Big enough that the two `now()` reads are noise and the
/// per-call cost averages cleanly; small enough that the loop is bearable under TCG (each `svc` is a
/// full emulated exception there). Self-timed, so the count is deterministic under icount and real
/// under HVF either way.
const NULL_ITERS: u64 = 20_000;

/// Iterations for the context-switch loop. Each is one `SYS_YIELD` that hands the CPU to the peer
/// and gets it back: a round trip of two switches (each an address-space change, since the peer is a
/// separate process). Fewer than the null loop: a switch is much heavier under TCG.
const CTX_ITERS: u64 = 5_000;

/// Iterations for the map loop. Unlike the loops above, each `MAP_INTO` **consumes** budget: a fresh
/// leaf VA needs a page-table entry (and, cold, an intermediate table) plus a revocation record, all
/// paid from the target space's region. So the count is bounded by that region, not free to grow, and
/// the bench boot sizes the region for `MAP_WARMUP + MAP_ITERS` (it must be kept in step with the
/// `MAP_EL0_ITERS` mirror there). 500 fits inside a single L3 table (512 entries from the timed base),
/// so the loop stays one table's worth of walks, and it is enough samples for the HVF median to
/// settle (64 was too few: the per-map counter delta was in the noise). Under icount it is exact.
const MAP_ITERS: u64 = 500;
/// Untimed warmup maps, so the first cold page-table allocation lands outside the window. They use a
/// disjoint VA range from the timed maps; both are paid from the same region (which is sized for the
/// sum). The page size is fixed (aarch64 4 KiB), and the VA bases are arbitrary aligned user pages.
const MAP_WARMUP: u64 = 8;
const PAGE: u64 = 4096;
const MAP_WARM_BASE: u64 = 0x20_0000;
const MAP_TIMED_BASE: u64 = 0x40_0000;

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, _x1: u64, _x2: u64) -> ! {
    match role {
        ROLE_NULL_SYSCALL => null_syscall_bench(),
        ROLE_YIELDER => yielder(),
        ROLE_CTX_SWITCH => ctx_switch_bench(),
        ROLE_IPC_SERVER => ipc_server(),
        ROLE_IPC_CLIENT => ipc_client(),
        ROLE_MAP => map_bench(),
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

/// **The server half of the IPC round-trip benchmark.** Loops forever: RECV a request, SEND a reply.
/// It BLOCKS on the RECV when idle (0% CPU, unlike the busy yielder), so it can loop unbounded and
/// still park cleanly once the client is done; the bench boot halts and reaps it.
fn ipc_server() -> ! {
    loop {
        let (n, _, _) = recv(SRV_REQUEST);
        send(SRV_REPLY, n, 0, 0);
    }
}

/// **IPC round-trip latency, measured from EL0.** lmbench's `lat_pipe`, over our endpoints. Each
/// iteration is a SEND to the server and a RECV of its reply: two rendezvous, four `svc`s, two
/// context switches. Self-timed; the bench boot spawns the server first so a request always meets a
/// waiting receiver.
fn ipc_client() -> ! {
    // Warm up: pay the first rendezvous and any cold paths outside the timed loop.
    for _ in 0..64 {
        send(CLI_REQUEST, 1, 0, 0);
        recv(CLI_REPLY);
    }
    let start = now();
    for _ in 0..IPC_ITERS {
        send(CLI_REQUEST, 1, 0, 0);
        recv(CLI_REPLY);
    }
    let ticks = now().wrapping_sub(start);
    send(REPORT, ticks, IPC_ITERS, 0);
    exit();
}

/// **Map latency, measured from EL0 (the primitive suite).** lmbench's `lat_mmap`, in the shape our
/// surface allows: `invoke(aspace, MAP_INTO, va, frame_slot, MAP_RO)` maps the one granted frame at a
/// fresh VA. We alias a single frame across every VA, so no data memory is consumed per iteration,
/// only the page-table entry and the revocation record the map path must write anyway, which is
/// exactly what we are timing. There is no unmap in the surface yet, so the loop is bounded (each VA
/// is used once); the bench boot provisions the target region for the warmup plus timed counts. The
/// gap between this and the kernel-side `map_new` is roughly the EL0<->EL1 trap on each `svc`.
fn map_bench() -> ! {
    // Warm the cold page-table allocations at a disjoint VA range, untimed.
    for i in 0..MAP_WARMUP {
        map_one(MAP_WARM_BASE + i * PAGE);
    }
    let start = now();
    for i in 0..MAP_ITERS {
        map_one(MAP_TIMED_BASE + i * PAGE);
    }
    let ticks = now().wrapping_sub(start);
    send(REPORT, ticks, MAP_ITERS, 0);
    exit();
}

/// One `MAP_INTO`: map the granted frame (slot `MAP_FRAME`) read-only into the granted space (slot
/// `MAP_ASPACE`) at `va`. Read-only needs only READ on the frame cap, which is all we were granted.
#[inline(never)]
fn map_one(va: u64) {
    // SAFETY: `svc`; the kernel validates the aspace and frame caps and the method before mapping.
    let _ = unsafe {
        invoke(
            MAP_ASPACE,
            abi::aspace::MAP_INTO,
            va,
            MAP_FRAME,
            abi::aspace::MAP_RO,
        )
    };
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
