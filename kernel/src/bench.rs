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
//!   executed, so these counter deltas are *exact and reproducible*: the same kernel prints the
//!   same numbers every run. A change in a number is a change in a code path, attributable to
//!   the commit that made it. `bench/baseline.txt` pins the numbers; `script/bench --check`
//!   fails on drift. Magnitudes are fiction (TCG models no caches, no TLB); the counts are the
//!   point.
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
