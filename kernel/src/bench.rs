//! Microbenchmarks over the paths a microkernel lives on (milestone 21).
//!
//! Compiled in only by `--features bench` (`script/bench`); the bench boot diverges here before
//! the milestone tour, runs each benchmark in a fixed order, prints machine-readable lines, and
//! **halts**. It never semihosts: under HVF the semihosting `hlt` traps to the guest instead of
//! exiting (see xtask's `test()`), so the contract is output-based in both modes: `xtask bench`
//! owns the QEMU process, watches for `bench: done`, and terminates it. One exit mechanism,
//! accelerator-independent.
//!
//! # The two instruments (design/roadmap.md §21)
//!
//! - **icount (default):** QEMU virtual time is a deterministic function of instructions
//!   executed, so these counter deltas are *exact and reproducible per binary*: the same kernel
//!   prints the same numbers every run. But they are NOT stable across *different* binaries: adding
//!   unrelated live code shifts even untouched benchmarks by several percent, non-uniformly, because
//!   the compiler remakes whole-crate inlining and monomorphization decisions (notes/benchmarks.md
//!   has the measurement). So `bench/baseline.txt` + `--check` is a **coarse tripwire** (10%) for a
//!   gross regression, not a fine attributor. Magnitudes are fiction anyway (TCG models no caches);
//!   the `--real` medians are the fine signal.
//! - **HVF (`--real`):** the kernel runs natively on the host core; real caches, real TLBs, the
//!   hardware counter at its real frequency. Magnitudes are true, determinism is gone (a shared
//!   desktop machine underneath), so real runs report and never gate.
//!
//! # Reading the numbers
//!
//! Each line is `bench: <name> <counter_ticks> <iters>`. The counter is `CNTVCT_EL0` at
//! `CNTFRQ_EL0` Hz (printed first), so ns/iter = ticks * 1e9 / freq / iters; xtask does the
//! division. Warmup iterations run untimed before each measurement so thread spawn and first
//! rendezvous costs land outside the window.

use crate::sched;
use crate::println;
use aarch64_cpu::registers::{CNTFRQ_EL0, CNTVCT_EL0};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tock_registers::interfaces::Readable;

/// Iterations per benchmark. Fixed and part of the output, so a baseline is self-describing.
const YIELD_ITERS: u64 = 2000;
const IPC_ITERS: u64 = 1000;
const CALL_ITERS: u64 = 1000;
const SPAWN_ITERS: u64 = 64;
const MAP_ITERS: u64 = 64;
const COREMARK_ITERS: u64 = 256;

/// Untimed shakeout before each measured loop: thread startup, first rendezvous, cold paths.
const WARMUP: u64 = 32;

fn timed(name: &str, iters: u64, f: impl FnOnce()) {
    let t0 = CNTVCT_EL0.get();
    f();
    let t1 = CNTVCT_EL0.get();
    println!("bench: {name} {} {iters}", t1 - t0);
}

/// Run every benchmark and halt. Never returns, never semihosts (see the module doc).
pub fn run() -> ! {
    println!();
    println!("bench: cntfrq {}", CNTFRQ_EL0.get());

    yield_switch();
    ipc_rtt();
    call_reply();
    spawn_reap();
    map_new();
    coremark_compute();
    null_syscall_el0();
    ctx_switch_el0();
    ipc_rtt_el0();

    println!("bench: done");
    // Parked, not exited: the host side saw the marker and tears QEMU down. `wfi`, so a
    // forgotten bench QEMU costs nothing while it waits to be killed (CLAUDE.md's rule).
    crate::arch::halt();
}

/// **The context switch, round trip.** Two threads yielding to each other; each of our yields
/// is one switch out and (eventually) one switch back in. Ticks/iter ~= two switches.
fn yield_switch() {
    static DONE: AtomicBool = AtomicBool::new(false);

    sched::spawn(|| {
        while !DONE.load(Ordering::Relaxed) {
            sched::yield_now();
        }
    })
    .expect("bench: no peer thread");

    for _ in 0..WARMUP {
        sched::yield_now();
    }
    timed("yield_switch", YIELD_ITERS, || {
        for _ in 0..YIELD_ITERS {
            sched::yield_now();
        }
    });
    DONE.store(true, Ordering::Relaxed);
    sched::yield_now(); // let the peer see the flag and exit
}

/// **Synchronous IPC round trip, the classic microkernel number.** A server loops
/// recv-then-send; the client times send-then-recv. One iteration is two rendezvous, two
/// mailbox copies, two wakes, two switches.
fn ipc_rtt() {
    let request = sched::create_endpoint();
    let reply = sched::create_endpoint();

    sched::spawn(move || {
        loop {
            let m = sched::ipc_recv(request);
            if m[0] == u64::MAX {
                break; // the client is done with us
            }
            sched::ipc_send(reply, [m[0], 0, 0]);
        }
    })
    .expect("bench: no server");

    for _ in 0..WARMUP {
        sched::ipc_send(request, [1, 0, 0]);
        sched::ipc_recv(reply);
    }
    timed("ipc_rtt", IPC_ITERS, || {
        for _ in 0..IPC_ITERS {
            sched::ipc_send(request, [1, 0, 0]);
            sched::ipc_recv(reply);
        }
    });
    sched::ipc_send(request, [u64::MAX, 0, 0]); // release the server
}

/// **Call/Reply round trip** (milestone 12): the one-endpoint shape real services use. One
/// iteration mints a one-shot Reply capability, rendezvouses, replies through it, consumes it.
fn call_reply() {
    let ep = sched::create_endpoint();

    sched::spawn(move || {
        loop {
            let m = sched::ipc_recv_cap(ep); // [word, reply_slot, word2]
            if m[0] == u64::MAX {
                break;
            }
            let slot = m[1];
            let crate::cap::Object::Reply(caller) =
                sched::current_cap(slot).expect("bench: no reply cap").object
            else {
                panic!("bench: RECV_CAP of a CALL did not deliver a Reply capability");
            };
            sched::ipc_reply(caller, [m[0], 0]);
            let _ = sched::delete_current_cap(slot);
        }
    })
    .expect("bench: no call server");

    for _ in 0..WARMUP {
        sched::ipc_call(ep, [1, 0]);
    }
    timed("call_reply", CALL_ITERS, || {
        for _ in 0..CALL_ITERS {
            sched::ipc_call(ep, [1, 0]);
        }
    });
    // Release the server: it is parked in RECV_CAP, and a plain SEND rendezvouses with it all
    // the same (the cap and plain paths share the wait queues), delivering the sentinel.
    sched::ipc_send(ep, [u64::MAX, 0, 0]);
}

/// **Thread lifecycle, spawn to reap.** Each iteration creates a thread that exits immediately,
/// then yields until the reaper has returned the table to its baseline: TCB pool slot claim and
/// release, stack map and unmap, generational name mint and death.
fn spawn_reap() {
    let baseline = sched::thread_count();
    let one = || {
        sched::spawn(|| {}).expect("bench: spawn failed");
        while sched::thread_count() > baseline {
            sched::yield_now();
        }
    };

    for _ in 0..4 {
        one(); // warmup: the first spawn pays for cold stack VAs
    }
    timed("spawn_reap", SPAWN_ITERS, || {
        for _ in 0..SPAWN_ITERS {
            one();
        }
    });
}

/// **Mapping a fresh page into an address space**: retype from the region, walk, write the
/// leaf. The exec path's inner loop.
fn map_new() {
    static TOTAL: AtomicU64 = AtomicU64::new(0);

    let mut space =
        crate::user::AddressSpace::new(MAP_ITERS + 8).expect("bench: no address space");
    let base = 0x40_0000u64;

    timed("map_new", MAP_ITERS, || {
        for i in 0..MAP_ITERS {
            let page = space
                .map_new(base + i * frames::FRAME_SIZE, paging::Flags::user_data())
                .expect("bench: map failed");
            // Touch it so the compiler cannot dissolve the loop.
            TOTAL.fetch_add(page[0] as u64, Ordering::Relaxed);
        }
    });
    drop(space); // teardown outside the timed window; it is spawn_reap's kind of cost, not map's
}

// Roles for the `elbench` EL0 program (must match user/src/elbench.rs). One binary, one micro-
// measurement per role, chosen through `START`'s `arg0`.
const EL_NULL_SYSCALL: u64 = 0;
const EL_YIELDER: u64 = 1;
const EL_CTX_SWITCH: u64 = 2;
const EL_IPC_SERVER: u64 = 3;
const EL_IPC_CLIENT: u64 = 4;

/// Spawn the `elbench` EL0 program in a given role, granting it `report` (slot 0) to answer on.
/// `false` if there is no `elbench` in the initrd (the bench boot then skips that line).
fn spawn_elbench(role: u64, report: sched::EpId) -> bool {
    let Some(image) = crate::user::program("elbench") else {
        return false;
    };
    sched::spawn(move || {
        crate::user::run(
            image,
            crate::user::Spawn {
                arg0: role,
                arg1: 0,
                arg2: 0,
                grants: &[crate::cap::endpoint_cap(report, crate::cap::Rights::WRITE)],
                maps: &[],
            },
        )
    })
    .expect("bench: could not spawn elbench");
    true
}

/// **Null syscall latency, measured from EL0 (the primitive suite).** The `bench:` lines above are
/// kernel-internal, no trap. This one is what lmbench measures: the bench boot spawns the `elbench`
/// EL0 program, which self-times a loop of the cheapest `svc` and reports `[ticks, iters]`; we print
/// it in the same format. The gap between this and a hypothetical kernel-side null syscall is roughly
/// the EL0<->EL1 boundary cost, which is the whole point of measuring here. See user/src/elbench.rs.
fn null_syscall_el0() {
    let report = sched::create_endpoint();
    if !spawn_elbench(EL_NULL_SYSCALL, report) {
        println!("bench: null_syscall skipped (no elbench in the initrd)");
        return;
    }
    let [ticks, iters, _] = sched::ipc_recv(report);
    println!("bench: null_syscall {ticks} {iters}");
}

/// **Context switch latency, measured from EL0 (the primitive suite).** lmbench's `lat_ctx`. The
/// bench boot spawns a *yielder* peer and a *timer*, two separate EL0 processes; the timer self-times
/// a loop of `SYS_YIELD`, each handing the CPU to the peer and back, two switches per iteration, each
/// an address-space change. With the boot thread blocked here on the report and only those two ready,
/// the alternation is clean. See user/src/elbench.rs.
fn ctx_switch_el0() {
    let report = sched::create_endpoint();
    // The peer first, so the timer always has something to switch to. It shares the report endpoint
    // (it never sends on it); the spawn shape stays uniform.
    if !spawn_elbench(EL_YIELDER, report) {
        println!("bench: ctx_switch skipped (no elbench in the initrd)");
        return;
    }
    if !spawn_elbench(EL_CTX_SWITCH, report) {
        return;
    }
    let [ticks, iters, _] = sched::ipc_recv(report);
    println!("bench: ctx_switch {ticks} {iters}");
}

/// **IPC round-trip latency, measured from EL0 (the primitive suite).** lmbench's `lat_pipe`. Two
/// EL0 processes and two endpoints: a server (RECV request, SEND reply) and a client that self-times
/// a loop of SEND-then-RECV and reports. The server is spawned first so a request always meets a
/// waiting receiver. Grants differ per role, so the spawns are inline rather than via `spawn_elbench`.
fn ipc_rtt_el0() {
    let Some(image) = crate::user::program("elbench") else {
        println!("bench: ipc_rtt skipped (no elbench in the initrd)");
        return;
    };
    let request = sched::create_endpoint();
    let reply = sched::create_endpoint();
    let report = sched::create_endpoint();
    use crate::cap::{Rights, endpoint_cap};

    sched::spawn(move || {
        crate::user::run(
            image,
            crate::user::Spawn {
                arg0: EL_IPC_SERVER,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(request, Rights::READ), // slot 0: RECV requests
                    endpoint_cap(reply, Rights::WRITE),  // slot 1: SEND replies
                ],
                maps: &[],
            },
        )
    })
    .expect("bench: could not spawn the ipc server");

    sched::spawn(move || {
        crate::user::run(
            image,
            crate::user::Spawn {
                arg0: EL_IPC_CLIENT,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(report, Rights::WRITE),  // slot 0: report the result
                    endpoint_cap(request, Rights::WRITE), // slot 1: SEND requests
                    endpoint_cap(reply, Rights::READ),    // slot 2: RECV replies
                ],
                maps: &[],
            },
        )
    })
    .expect("bench: could not spawn the ipc client");

    let [ticks, iters, _] = sched::ipc_recv(report);
    // Distinct from the kernel-side `ipc_rtt` above: this one crosses the EL0<->EL1 boundary on every
    // send and recv, which is the whole point (comparable to lmbench). The gap between them is roughly
    // the trap cost of the four svcs per round trip.
    println!("bench: ipc_rtt_el0 {ticks} {iters}");
}

/// **The compute workload (milestone 19e), for the record.** Unlike the paths above, this touches
/// no OS primitive: it is pure computation (the CoreMark-derived kernel, `crates/coremark`), so its
/// cost is the *core's*, not cricker-os's. It is here because the same crate runs as an EL0 workload
/// and later on macOS and Linux, and this line is where the cricker-os compute number is recorded,
/// on the same two instruments as everything else. Running it in the kernel is fine: compute is
/// privilege-independent, so this number equals the EL0 workload's. `SINK` keeps it live.
fn coremark_compute() {
    static SINK: AtomicU64 = AtomicU64::new(0);
    timed("coremark", COREMARK_ITERS, || {
        let crc = coremark::run(COREMARK_ITERS as u32);
        SINK.store(crc as u64, Ordering::Relaxed);
    });
}
