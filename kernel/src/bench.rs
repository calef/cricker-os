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

use crate::println;
use crate::sched;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Iterations per benchmark. Fixed and part of the output, so a baseline is self-describing.
const YIELD_ITERS: u64 = 2000;
const IPC_ITERS: u64 = 1000;
const RELAY_ITERS: u64 = 1000;
const CALL_ITERS: u64 = 1000;
const SPAWN_ITERS: u64 = 64;
const MAP_ITERS: u64 = 64;
const COREMARK_ITERS: u64 = 256;

/// Untimed shakeout before each measured loop: thread startup, first rendezvous, cold paths.
const WARMUP: u64 = 32;

fn timed(name: &str, iters: u64, f: impl FnOnce()) {
    // The monotonic counter, through the arch abstraction: CNTVCT_EL0 on aarch64, `rdtime` on
    // RISC-V. `frequency()` (printed by `run` as `cntfrq`) turns ticks into seconds.
    let t0 = crate::arch::timer::now();
    f();
    let t1 = crate::arch::timer::now();
    println!("bench: {name} {} {iters}", t1 - t0);
}

/// Run every benchmark and halt. Never returns, never semihosts (see the module doc).
pub fn run() -> ! {
    println!();
    println!("bench: cntfrq {}", crate::arch::timer::frequency());

    yield_switch();
    ipc_rtt();
    relay_rtt();
    call_reply();
    spawn_reap();
    map_new();
    coremark_compute();
    null_syscall_el0();
    ctx_switch_el0();
    ipc_rtt_el0();
    map_el0();
    spawn_el0();
    smp_throughput();
    fs_read();

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

/// **The confined-server tax: a request routed through a server that fans out to a backend.** This
/// is the microkernel architecture's per-request cost that a monolith does not pay, and the topology
/// both real userspace servers use: the FS server CALLs the block server (`client -> fs -> blk -> fs
/// -> client`), netd CALLs the NIC driver (`client -> netd -> driver -> netd -> client`). Each
/// iteration here is that two-hop shape: the client sends to a relay, the relay forwards to a backend
/// and waits, the backend replies, the relay replies to the client. Two rendezvous become four, two
/// context switches become four.
///
/// Read it against `ipc_rtt` above (the one-hop client<->server round trip): the **difference** is
/// what one confined intermediary that delegates to a backend costs, the "server tax" a skeptic asks
/// about, isolated and deterministic. It is on the icount baseline for exactly that reason. The real
/// servers' end-to-end numbers are elsewhere and cannot be gated: `fs_read` (below) is device-latency
/// dominated (~200 us/block under HVF swamps this few-hundred-tick tax), and netd's path is DHCP- and
/// timer-driven, neither deterministic under `-icount`. So this kernel-side topology bench is how the
/// server tax gets a gated regression number; see notes/benchmarks.md.
fn relay_rtt() {
    let cl_req = sched::create_endpoint(); // client -> relay
    let cl_reply = sched::create_endpoint(); // relay -> client
    let bk_req = sched::create_endpoint(); // relay -> backend
    let bk_reply = sched::create_endpoint(); // backend -> relay

    // The backend: the leaf service. Recv a request, send a reply, until the sentinel.
    sched::spawn(move || {
        loop {
            let m = sched::ipc_recv(bk_req);
            if m[0] == u64::MAX {
                break;
            }
            sched::ipc_send(bk_reply, [m[0], 0, 0]);
        }
    })
    .expect("bench: no relay backend");

    // The relay: the confined intermediary. For each client request it does a full round trip to the
    // backend, then answers the client. On the sentinel it releases the backend and exits too.
    sched::spawn(move || {
        loop {
            let m = sched::ipc_recv(cl_req);
            if m[0] == u64::MAX {
                sched::ipc_send(bk_req, [u64::MAX, 0, 0]);
                break;
            }
            sched::ipc_send(bk_req, [m[0], 0, 0]);
            let r = sched::ipc_recv(bk_reply);
            sched::ipc_send(cl_reply, [r[0], 0, 0]);
        }
    })
    .expect("bench: no relay");

    for _ in 0..WARMUP {
        sched::ipc_send(cl_req, [1, 0, 0]);
        sched::ipc_recv(cl_reply);
    }
    timed("relay_rtt", RELAY_ITERS, || {
        for _ in 0..RELAY_ITERS {
            sched::ipc_send(cl_req, [1, 0, 0]);
            sched::ipc_recv(cl_reply);
        }
    });
    sched::ipc_send(cl_req, [u64::MAX, 0, 0]); // release the relay, which releases the backend
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
            let crate::cap::Object::Reply(caller) = sched::current_cap(slot)
                .expect("bench: no reply cap")
                .object
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

    let mut space = crate::user::AddressSpace::new(MAP_ITERS + 8).expect("bench: no address space");
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
const EL_MAP: u64 = 5;
const EL_SPAWN: u64 = 6;

/// The spawner's untyped budget, in pages. **Small on purpose:** each child is split, run, and
/// destroyed strictly LIFO, so its pages return to this budget (DECISIONS §16), and only one child
/// lives at a time. The budget need only fund one child (`CHILD_PAGES`) plus the shared code frame
/// and the scratch page tables, not one per iteration. That 64 funds a 100-iteration loop (which
/// would need >1000 pages without return-to-parent) is itself the proof LIFO reclaim works.
const SPAWN_EL0_BUDGET: u64 = 64;

/// Warmup + timed map counts the bench boot must provision the target region for. `MAP_EL0_ITERS`
/// **must equal** elbench's `MAP_ITERS` and `MAP_EL0_WARMUP` its `MAP_WARMUP`: the region is sized for
/// their sum plus page-table and record overhead, and if elbench asks for more maps than the region
/// funds, the surplus fail (a cheap error return, not a real map) and skew the number. Kept here
/// rather than shared because the two crates have no common header, the way EL_* mirror ROLE_*.
const MAP_EL0_ITERS: u64 = 500;
const MAP_EL0_WARMUP: u64 = 8;
const MAP_EL0_OVERHEAD: u64 = 32;

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
    let [ticks, iters, ..] = sched::ipc_recv(report);
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
    let [ticks, iters, ..] = sched::ipc_recv(report);
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

    let [ticks, iters, ..] = sched::ipc_recv(report);
    // Distinct from the kernel-side `ipc_rtt` above: this one crosses the EL0<->EL1 boundary on every
    // send and recv, which is the whole point (comparable to lmbench). The gap between them is roughly
    // the trap cost of the four svcs per round trip.
    println!("bench: ipc_rtt_el0 {ticks} {iters}");
}

/// **Map latency, measured from EL0 (the primitive suite).** lmbench's `lat_mmap`. Unlike the three
/// above, the map path *consumes* resources per call, so the bench boot builds the target here rather
/// than granting an endpoint: a fresh registry address space (its own untyped region as budget) and a
/// single frame. `elbench` (EL_MAP) is granted a WRITE cap on that space and a READ cap on the frame,
/// then times a loop of `invoke(aspace, MAP_INTO, va_i, frame, MAP_RO)`, aliasing the one frame at a
/// fresh VA each iteration. The target is a separate space, not `elbench`'s own (a run()-adopted space
/// is not in the registry MAP_INTO resolves), which is immaterial: the map path's cost is the same
/// whoever owns the space. See user/src/elbench.rs.
fn map_el0() {
    let Some(image) = crate::user::program("elbench") else {
        println!("bench: map_el0 skipped (no elbench in the initrd)");
        return;
    };
    // The target space, backed by its own region. The region pays for the root, the intermediate
    // tables, and the mapping-record log pages; the leaves are aliases of one frame, so they cost it
    // nothing. Sized for the warmup plus timed maps plus that overhead.
    let Some(region) = crate::untyped::create(MAP_EL0_WARMUP + MAP_EL0_ITERS + MAP_EL0_OVERHEAD)
    else {
        println!("bench: map_el0 skipped (no region)");
        return;
    };
    let Some(name) = crate::user::user_aspace_create(region) else {
        println!("bench: map_el0 skipped (no address space)");
        return;
    };
    // One frame to alias-map, from its own one-page region so the aspace region stays pure overhead.
    let Some(frame_region) = crate::untyped::create(1) else {
        println!("bench: map_el0 skipped (no frame region)");
        return;
    };
    let Some(phys) = crate::untyped::retype_page(frame_region) else {
        println!("bench: map_el0 skipped (no frame)");
        return;
    };

    let report = sched::create_endpoint();
    use crate::cap::{Rights, aspace_cap, endpoint_cap, frame_cap};
    sched::spawn(move || {
        crate::user::run(
            image,
            crate::user::Spawn {
                arg0: EL_MAP,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(report, Rights::WRITE), // slot 0: report the result
                    aspace_cap(name, Rights::WRITE),     // slot 1: the space we map into
                    frame_cap(phys, Rights::READ),       // slot 2: the frame we alias-map
                ],
                maps: &[],
            },
        )
    })
    .expect("bench: could not spawn the map bencher");

    let [ticks, iters, ..] = sched::ipc_recv(report);
    println!("bench: map_el0 {ticks} {iters}");
}

/// **Spawn latency, measured from EL0 (the primitive suite).** lmbench's `lat_proc`, and the payoff
/// of object revocation: a userspace spawner builds a whole child from EL0 through the granular verbs
/// and, crucially, `DESTROY`s the child's region afterward, so the loop repeats. The bench boot hands
/// the spawner three things: a big untyped budget (slot 1), a report endpoint to answer on (slot 0),
/// and a child-done endpoint (slot 2, READ|WRITE|GRANT) it delegates a WRITE view of to each child.
/// See user/src/elbench.rs.
fn spawn_el0() {
    let Some(image) = crate::user::program("elbench") else {
        println!("bench: spawn_el0 skipped (no elbench in the initrd)");
        return;
    };
    let Some(region) = crate::untyped::create(SPAWN_EL0_BUDGET) else {
        println!("bench: spawn_el0 skipped (no budget)");
        return;
    };
    let report = sched::create_endpoint();
    let child_done = sched::create_endpoint();
    use crate::cap::{Rights, endpoint_cap, untyped_cap};
    sched::spawn(move || {
        crate::user::run(
            image,
            crate::user::Spawn {
                arg0: EL_SPAWN,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(report, Rights::WRITE), // slot 0: report the result home
                    untyped_cap(region),                 // slot 1: the spawner's whole budget
                    endpoint_cap(
                        child_done,
                        Rights::READ.union(Rights::WRITE).union(Rights::GRANT),
                    ), // slot 2: children signal done; spawner recvs and delegates WRITE
                ],
                maps: &[],
            },
        )
    })
    .expect("bench: could not spawn the spawner");
    let [ticks, iters, ..] = sched::ipc_recv(report);
    println!("bench: spawn_el0 {ticks} {iters}");
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

/// **The userspace file-server tax** (DECISIONS §32, the flagship userspace-reuse story). This is the
/// number a microkernel skeptic asks for about a filesystem in userspace: a client opens a file
/// through a granted *directory capability* and reads a block, over the real confined stack, a block
/// server driving the RedoxFS disk by DMA and an FS server (the vendored RedoxFS engine, no_std, on
/// its own heap) mounting it over blk IPC. `kernel/src/user.rs::fs_service` wires all three; the
/// client (`user/src/fsclient.rs`, `ROLE_BENCH`) times a warm read loop and reports `[ticks, iters]`.
///
/// **Why it is `--real`-only and never gates, unlike the primitives.** The FS server's mount is
/// device-driven: hundreds of block reads gated on the disk's completion interrupt, plus the engine's
/// own logic. Under `-icount shift=0` that path is not deterministic (interrupt timing is not part of
/// the instruction clock), so an icount baseline for it would enshrine exactly the non-determinism the
/// 2026-07-28 lesson warns against. So it runs only on the `--real --smp` boot (HVF, where the whole
/// stack is proven by the fs-server test), self-skipping everywhere else via the same
/// `online_count() > 1` gate as the throughput bench, so `bench/baseline.txt` never sees it.
///
/// **What the number means: whole-path cost, dominated by the device.** ~204 us/read (HVF,
/// `--release --smp`). A read is **not** served warm from a cache: it goes to the block server, which
/// does a DMA transfer and waits on the disk's completion interrupt, roughly 200 us per block under
/// HVF. That swamps the FS server's own IPC-contract tax, which `relay_rtt` puts at a few hundred
/// *nanoseconds*. So this is the honest whole-path cost of a userspace file read, not an isolated
/// server tax, which is exactly the case milestone 21's rule names: when device latency swamps the
/// isolation, measure the whole path and say so rather than report a fictional isolated number.
///
/// The isolated file-server cost was attempted and abandoned, and this comment used to describe that
/// abandoned design (claiming a warm cache and a contract-cost measurement), which contradicted
/// notes/benchmarks.md for long enough to mislead a reader. Corrected here: a few hundred ns of
/// serving layer sitting on a ~200 us block read with its own run-to-run spread puts the delta inside
/// the device noise. The per-hop tax lives in `relay_rtt`, where it is measurable and gated.
///
/// **What it is not:** a filesystem throughput number. There is no MB/s figure and no comparison
/// against ext4 or APFS, which is DECISIONS §34's unmet condition 2 and milestone 38.
fn fs_read() {
    // Same gate as the throughput bench: meaningful only on the `--real --smp` boot, which is where
    // the RedoxFS disk is attached and the whole stack is proven. Single hart (icount, default
    // `--real`) skips, so this never reaches the deterministic baseline.
    if crate::smp::online_count() <= 1 {
        return;
    }
    // The three binaries the service needs. On aarch64 the block server is a role of `init` (the
    // hello multiplexer), as in the fs-server test. Absent any of them, or the RedoxFS disk, skip.
    let (Some(blk_image), Some(fsserver), Some(fsclient)) = (
        crate::user::program("init"),
        crate::user::program("fsserver"),
        crate::user::program("fsclient"),
    ) else {
        return;
    };
    // Spawn the block server, the FS server, and the client in its ROLE_BENCH (timed) role.
    let Some((readiness, report)) =
        crate::user::fs_service::start(blk_image, fsserver, fsclient, 1)
    else {
        return; // no RedoxFS disk on this run
    };
    // Sequence on readiness, exactly as the test does: the block server brings the device up, then
    // the FS server mounts the image, then the client's timed loop reports. The bench boot is the
    // only caller, so it always gets the readiness endpoints (nothing wired the service first).
    if let Some((blk_ready, ready)) = readiness {
        let _ = sched::ipc_recv(blk_ready);
        let _ = sched::ipc_recv(ready);
    }
    let [ticks, iters, ..] = sched::ipc_recv(report);
    println!("bench: fs_read {ticks} {iters}");
}

// --- Multi-hart aggregate throughput (DECISIONS §28, the SMP placement win) ---
//
// Every primitive above is hart-pinned by design: the icount instrument boots `-smp 1` (the
// 2026-07-28 attribution finding, notes/benchmarks.md), so those numbers are per-core path length
// and cannot show §28's placement work at all. This one is different, and its methodology has to be
// different, so it is set apart here on purpose.
//
// It runs N independent ping-pong pipelines and measures the WALL-CLOCK time to complete them. A
// single pipeline is a synchronous rendezvous: only one of its two threads is runnable at a time,
// so it keeps ~one core busy and §28's local-wake rule keeps the pair co-located and warm. N
// independent pipelines are N such streams, which §28's power-of-two placement scatters across the
// cores at spawn. So the aggregate throughput of N pipelines should approach `online_count()` times
// a single pipeline's throughput: the whole machine filled, which is the property §28 exists to
// deliver and the one no hart-pinned primitive can see.
//
// **Why it is NOT on the icount baseline, and never gates.** Two reasons, both structural:
//   1. It is meaningful only with more than one hart. The icount instrument pins `-smp 1`, and so
//      does the default `--real` run (per-core magnitudes; see xtask's `bench`), so it runs ONLY on
//      the `--real --smp` boot (HVF, 4 harts), which the harness already forbids from
//      `--check`/`--save`. Everywhere else `online_count()` is 1 and it is skipped outright, so it
//      never touches `bench/baseline.txt`.
//   2. Under `-icount` all vCPUs share one virtual clock (again the 2026-07-28 finding), so a
//      wall-clock throughput number is not even defined there; and TCG serialises vCPUs onto one
//      host thread, so there is no real parallelism to measure. Only HVF gives each core its own
//      counter and real concurrent execution.
//
// So this is a statistical `--real` measurement read by a human with loose bounds, exactly like the
// other HVF magnitudes, not a deterministic tick baseline. It reports two lines, `smp_pipe_solo`
// (one pipeline) and `smp_pipe_all` (N pipelines); the scaling factor is the solo ns/iter divided
// by the all ns/iter, and it should land near `online_count()`.
//
// **A methodology note about the solo baseline, learned by getting it wrong first.** The whole
// result is only as honest as its single-core reference, and the obvious way to take it is a trap.
// The main thread must **block** (a real `RECV`) while a batch runs, exactly as `ipc_rtt` above does,
// NOT busy-yield waiting on a counter. A yield-spinning main stays runnable, so on the solo batch
// the scheduler sees main plus the pair (three runnable-ish threads) and scatters them, turning each
// local rendezvous into a cross-core wake; the solo pair then clocked ~60x slower than `ipc_rtt`'s
// identical pair and the derived scaling went *superlinear* (>cores), which is not physical. With
// main blocked, the solo pair co-locates and runs at the `ipc_rtt` rate, and scaling lands at or
// below `cores`, as it must. Each batch is also run a few times and the **minimum** ticks kept: on a
// shared desktop under HVF the host preempts guest vCPUs, and the min is the least-contended sample,
// the closest thing to the true rate. Loose bounds, read by a human.

/// Independent ping-pong pipelines. 16 on a 4-hart boot is four per core, enough that placement and
/// stealing fill every core and the aggregate is not starved by too few streams.
const TP_PIPES: usize = 16;
/// Round trips per pipeline in a batch. Large enough that spawn and first-rendezvous costs (paid
/// before the clock starts, behind the GO barrier) are noise against the timed steady state.
const TP_RTT: u64 = 2000;
/// Batches per measurement; the minimum ticks is kept (least host contention under HVF).
const TP_REPEAT: usize = 4;

static TP_GO: AtomicBool = AtomicBool::new(false);

/// Run `pipes` independent ping-pong pipelines over the pre-created endpoint pairs and return the
/// wall-clock ticks to complete them all. The clock spans only steady state: threads are spawned
/// first, then released together by `TP_GO` after `now()` is read, so N-pipeline batches are not
/// charged N times the spawn cost that a 1-pipeline batch pays only once. `done` collects one signal
/// per pipeline so the main thread can **block** (not busy-yield) through the timed window.
fn tp_batch(req: &[sched::EpId], reply: &[sched::EpId], done: sched::EpId, pipes: usize) -> u64 {
    TP_GO.store(false, Ordering::SeqCst);
    let base = sched::thread_count();

    for i in 0..pipes {
        let (rq, rp) = (req[i], reply[i]);
        // The server half: exactly TP_RTT recv-then-send, then it returns and is reaped. It blocks
        // in `ipc_recv` until its client sends, so it effectively starts when the client does.
        sched::spawn(move || {
            for _ in 0..TP_RTT {
                let _ = sched::ipc_recv(rq);
                sched::ipc_send(rp, [1, 0, 0]);
            }
        })
        .expect("bench: throughput server spawn failed");

        // The client half: wait at the barrier, then TP_RTT send-then-recv, then signal done. Yield
        // (not spin) at the barrier so a waiting client does not burn its core before the clock.
        sched::spawn(move || {
            while !TP_GO.load(Ordering::Acquire) {
                sched::yield_now();
            }
            for _ in 0..TP_RTT {
                sched::ipc_send(rq, [1, 0, 0]);
                let _ = sched::ipc_recv(rp);
            }
            sched::ipc_send(done, [1, 0, 0]);
        })
        .expect("bench: throughput client spawn failed");
    }

    // Start the clock, release every client in one step, then BLOCK receiving one done per pipeline.
    // Blocking (not yield-spinning) is what keeps the solo pair co-located; see the methodology note.
    let t0 = crate::arch::timer::now();
    TP_GO.store(true, Ordering::Release);
    for _ in 0..pipes {
        let _ = sched::ipc_recv(done);
    }
    let ticks = crate::arch::timer::now() - t0;

    // Drain: let the servers finish their last iterations and every thread reap before the next
    // batch, so a batch's timing is never charged the previous batch's teardown.
    while sched::thread_count() > base {
        sched::yield_now();
    }
    ticks
}

/// Minimum ticks over `TP_REPEAT` batches of `pipes` pipelines: the least host-contended sample.
fn tp_best(req: &[sched::EpId], reply: &[sched::EpId], done: sched::EpId, pipes: usize) -> u64 {
    let mut best = u64::MAX;
    for _ in 0..TP_REPEAT {
        best = best.min(tp_batch(req, reply, done, pipes));
    }
    best
}

/// Inner iterations per compute worker. Sized so a single worker runs a few hundred microseconds
/// under HVF, long enough that the 41 ns counter grain and host jitter are noise against the batch.
const TC_WORK: u64 = 300_000;

static TC_SINK: AtomicU64 = AtomicU64::new(0);

/// A non-elidable integer grind: an LCG mixed with an xorshift, folded into a returned value the
/// caller sinks so the optimizer cannot delete the loop. Pure compute, no syscalls, so a running
/// worker touches no other core: this is the workload that isolates §28 **placement** from IPC's
/// cross-core wakes, which is exactly why the pipeline result and this one differ under HVF.
#[inline(never)]
fn busy(iters: u64) -> u64 {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for i in 0..iters {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407 ^ i);
        x ^= x >> 29;
    }
    x
}

/// Run `workers` independent CPU-bound workers behind the GO barrier and return the wall-clock ticks
/// to finish them all. Each worker does the same fixed grind, so N workers is N times the work of
/// one; §28 placement should spread them so the aggregate finishes in about `ceil(N/cores)` worker-
/// times, i.e. aggregate throughput approaches `cores` times a single worker's. No rendezvous during
/// the grind, so no cross-core wakes: the clean placement measurement. Main blocks on `done`.
fn tc_batch(done: sched::EpId, workers: usize) -> u64 {
    TP_GO.store(false, Ordering::SeqCst);
    let base = sched::thread_count();
    for _ in 0..workers {
        sched::spawn(move || {
            while !TP_GO.load(Ordering::Acquire) {
                sched::yield_now();
            }
            let v = busy(TC_WORK);
            TC_SINK.fetch_add(v, Ordering::Relaxed);
            sched::ipc_send(done, [1, 0, 0]);
        })
        .expect("bench: compute worker spawn failed");
    }
    let t0 = crate::arch::timer::now();
    TP_GO.store(true, Ordering::Release);
    for _ in 0..workers {
        let _ = sched::ipc_recv(done);
    }
    let ticks = crate::arch::timer::now() - t0;
    while sched::thread_count() > base {
        sched::yield_now();
    }
    ticks
}

/// Minimum ticks over `TP_REPEAT` compute batches: the least host-contended sample.
fn tc_best(done: sched::EpId, workers: usize) -> u64 {
    let mut best = u64::MAX;
    for _ in 0..TP_REPEAT {
        best = best.min(tc_batch(done, workers));
    }
    best
}

/// **Aggregate multi-hart throughput.** See the block comment above for the methodology and why this
/// is a `--real`-only, non-gating measurement. Skipped on a single hart (the icount boot), where it
/// would measure nothing §28 does.
///
/// Two workloads, on purpose. **Compute** (`smp_compute_*`) is N independent CPU-bound workers: no
/// syscalls, so no cross-core wakes, so the host keeps every busy vCPU on a real core and the
/// aggregate scales cleanly to `cores`. This is the direct picture of §28 placement filling the
/// machine. **Pipelines** (`smp_pipe_*`) is N synchronous IPC ping-pong pairs, and under HVF it does
/// NOT scale, it goes slightly backwards: a semi-idle guest vCPU is descheduled by the host, so the
/// cross-core wakes an IPC pipeline needs whenever placement/stealing splits a pair pay host
/// reschedule latency that a single co-located pair never does. That is a virtualization property,
/// not a scheduler defect (it is the same reason the icount primitive suite is pinned to one hart),
/// and recording both is the honest result: compute parallelises on this instrument, synchronous IPC
/// does not, and the reason is the host underneath.
fn smp_throughput() {
    let cores = crate::smp::online_count();
    if cores <= 1 {
        return; // single hart (the icount instrument): there is no placement win to show.
    }

    // Endpoint pairs, created once and reused across batches so a repeated run does not leak the
    // endpoint table down. One request and one reply endpoint per pipeline, plus a shared done EP.
    let mut req = [0u64; TP_PIPES];
    let mut reply = [0u64; TP_PIPES];
    for i in 0..TP_PIPES {
        req[i] = sched::create_endpoint();
        reply[i] = sched::create_endpoint();
    }
    let done = sched::create_endpoint();

    // Compute: the clean placement win. Warm once, then measure solo and the full machine.
    let _ = tc_batch(done, 1);
    let compute_solo = tc_best(done, 1);
    let compute_all = tc_best(done, TP_PIPES);

    // Pipelines: the IPC workload, which does not parallelise under HVF (see the doc comment).
    let _ = tp_batch(&req, &reply, done, 1);
    let pipe_solo = tp_best(&req, &reply, done, 1);
    let pipe_all = tp_best(&req, &reply, done, TP_PIPES);

    // Each `*_all` is TP_PIPES times the work of its `*_solo`. The scaling factor is solo ns/iter
    // divided by all ns/iter: near `cores` for compute (the machine filled), near or below 1 for
    // pipelines under HVF. The `smp_cores` line records the ceiling.
    println!("bench: smp_compute_solo {compute_solo} {TC_WORK}");
    println!(
        "bench: smp_compute_all {compute_all} {}",
        TP_PIPES as u64 * TC_WORK
    );
    println!("bench: smp_pipe_solo {pipe_solo} {TP_RTT}");
    println!(
        "bench: smp_pipe_all {pipe_all} {}",
        TP_PIPES as u64 * TP_RTT
    );
    println!("bench: smp_cores {cores} {cores}");
}
