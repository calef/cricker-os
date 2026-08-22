//! EL0 microbenchmarks, measured the lmbench way (the cross-OS primitive suite, milestone 21+).
//!
//! The kernel-side benchmarks in `kernel/src/bench.rs` measure path length *inside* the kernel: the
//! bench threads are kernel threads calling `sched::` directly, so they never pay the EL0->EL1 trap.
//! lmbench measures from userspace, trap included, so to compare nife to lmbench we have to
//! measure from **here**, at EL0, self-timing a loop of real `svc` syscalls. That is this program.
//!
//! It is spawned by the bench boot (`kernel/src/bench.rs`), self-times each primitive with
//! `user_rt::now` (the virtual counter, EL0-readable since milestone 19e), and SENDs `[ticks, iters]`
//! home on the one endpoint it was granted (slot 0). The bench boot prints it in the same
//! machine-readable line the rest of the harness uses, so the icount baseline gates it and `--real`
//! gives its true magnitude. Five primitives: null syscall, context switch, IPC round trip, page
//! map, and spawn. Spawn is the whole reason object revocation was built: the loop reclaims each
//! child's region (`Untyped::DESTROY`) so it can repeat.
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `elbench`, and it is the name that
//! forced `nifefs`'s `NAME_LEN` from 24 to 32. The raise was taken first and on its own merits,
//! deliberately, because choosing a worse name to fit a limit and raising a limit because a name
//! demanded it are both the wrong way round. Refused `elbench` (`el` is aarch64-only vocabulary for
//! a program that runs on both ISAs, since RISC-V has U, S and M modes and no exception levels) and
//! `os_primitives_benchmark` (this tree already uses "benchmark" for the output:
//! `bench/baseline.txt` holds the numbers, so the agent noun names the producer).

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use user_rt::{cap_delete, exit, invoke, now, recv, send, yield_now};

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
const ROLE_SPAWN: u64 = 6;
const ROLE_SINK_PRODUCER: u64 = 7;
const ROLE_SINK_CONSUMER: u64 = 8;

// Sink-throughput slots. The producer holds only the pipe it writes into; the consumer holds the
// report first, so slot 0 stays "the endpoint I report on" across every reporting role.
const SINK_PIPE_OUT: u64 = 0; // the producer SENDs sink messages here
const SINK_PIPE_IN: u64 = 1; // the consumer RECVs them here (slot 0 is REPORT)

/// Messages in the sink-throughput loop, each carrying [`sink_proto::INLINE_MAX`] bytes. 5 000 is
/// the same order as [`IPC_ITERS`] so the two are comparable under TCG, and it is 80 000 bytes, which
/// is past the 64 KiB a Unix pipe buffers: a producer that could run ahead would have had to stop at
/// least once, which is what makes the comparison about back pressure rather than about buffer size.
const SINK_MSGS: u64 = 5_000;

// Spawn-benchmark slots. slot 0 REPORT (the final result home to the bench boot), slot 1 UNTYPED
// (the spawner's whole budget), slot 2 CHILD_DONE (children SEND here, the spawner RECVs; it also
// delegates a WRITE view to each child, so it needs READ|WRITE|GRANT). The shared code frame is
// retyped at setup into whatever slot `grant` hands back.
const SP_REPORT: u64 = 0;
const SP_UNTYPED: u64 = 1;
const SP_CHILD_DONE: u64 = 2;

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
        ROLE_SPAWN => spawn_bench(),
        ROLE_SINK_PRODUCER => sink_producer(),
        ROLE_SINK_CONSUMER => sink_consumer(),
        _ => exit(),
    }
}

/// **The writing half of `a | b`**, and it is the whole of what a program on the left of a pipe
/// does: pack sixteen bytes into the sink contract's three words and `SEND`. No buffer, no flush, no
/// knowledge of who is reading.
///
/// The warmup messages are part of the contract with [`sink_consumer`], which discards exactly that
/// many before it starts timing, so the first rendezvous and the consumer's own spawn land outside
/// the window.
fn sink_producer() -> ! {
    let payload = [b'x'; sink_proto::INLINE_MAX];
    for _ in 0..(SINK_MSGS + 64) {
        let (w0, w1, w2, _) = sink_proto::pack(&payload);
        send(SINK_PIPE_OUT, w0, w1, w2);
    }
    send(SINK_PIPE_OUT, sink_proto::eof(), 0, 0);
    exit();
}

/// **The reading half of `a | b`, self-timed**: how long it takes for [`SINK_MSGS`] messages to
/// cross a pipe that is an endpoint and nothing else.
///
/// It reports **bytes**, not iterations, because that is the number a Unix pipe can be compared
/// against: `bench/host/pipe_throughput.rs` moves the same bytes through a real `pipe(2)`. What the
/// comparison is *for* is the cost of full lockstep (notes/pipes.md): there is no buffer here, so
/// every sixteen bytes is a rendezvous and the producer can never run ahead.
fn sink_consumer() -> ! {
    for _ in 0..64 {
        recv(SINK_PIPE_IN);
    }
    let start = now();
    for _ in 0..SINK_MSGS {
        recv(SINK_PIPE_IN);
    }
    let ticks = now().wrapping_sub(start);
    send(REPORT, ticks, SINK_MSGS * sink_proto::INLINE_MAX as u64, 0);
    exit();
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
/// Returns the syscall result: 0 on success, a negative error otherwise.
#[inline(never)]
fn map_one(va: u64) -> i64 {
    // SAFETY: `svc`; the kernel validates the aspace and frame caps and the method before mapping.
    unsafe {
        invoke(
            MAP_ASPACE,
            abi::aspace::MAP_INTO,
            va,
            MAP_FRAME,
            abi::aspace::MAP_RO,
        )
    }
}

/// Spawn benchmark tuning. Each iteration builds a whole child from EL0 and reclaims it, so the
/// count is modest (spawn is the heaviest primitive). The child is split, run, and destroyed
/// strictly LIFO, so `DESTROY` returns its pages to the budget (DECISIONS §16): only one child lives
/// at a time, and the bench boot's untyped need only fund one child, not one per iteration.
const SPAWN_ITERS: u64 = 100;
const SPAWN_WARMUP: u64 = 8;
const CHILD_PAGES: u64 = 10; // the child's aspace root + tables + one stack page + revoke records
const CHILD_CODE_VA: u64 = 0x40_0000;
const CHILD_STACK_VA: u64 = 0x50_0000;
const SPAWN_SCRATCH_VA: u64 = 0x0100_0000; // where we map the shared code frame to write the stub
const SPAWN_PAGE: u64 = 4096;

/// The child's whole program, hand-assembled: `SEND(slot 0, endpoint::SEND, 1)` then `SYS_EXIT`.
/// Its slot 0 is the `CHILD_DONE` endpoint the spawner inserts. Nine instructions: `SEND` the done
/// word (1) on the slot-0 endpoint, then `EXIT`. Every child runs this identical code, which is why
/// the spawner writes it into one shared frame and aliases that frame into each child. Hand-assembled
/// machine code, arch-gated: aarch64 `movz`/`svc`, RISC-V `li`/`ecall`.
///
/// aarch64: x0=slot 0, x1=SEND, x2=1, `x8=SYS_INVOKE`, svc; then `x8=SYS_EXIT`, svc.
#[cfg(target_arch = "aarch64")]
const CHILD_STUB: [u32; 9] = [
    0xD280_0000,
    0xD280_0001,
    0xD280_0000 | (1u32 << 5) | 2, // movz x2, #1  (the done word)
    0xD280_0003,
    0xD280_0004,
    0xD280_0000 | ((abi::SYS_INVOKE as u32) << 5) | 8,
    0xD400_0001,
    0xD280_0008,
    0xD400_0001,
];

/// RISC-V: a0=slot 0, a1=SEND, a2=1, a3=a4=0, a7=SYS_INVOKE, ecall; then a7=SYS_EXIT, ecall.
/// Each `li aN, imm` is `addi aN, x0, imm` = `(imm << 20) | (reg << 7) | 0x13`; `ecall` is 0x73.
#[cfg(target_arch = "riscv64")]
const CHILD_STUB: [u32; 9] = [
    0x0000_0513,                                        // li a0, 0            (slot 0)
    0x0000_0593 | ((abi::endpoint::SEND as u32) << 20), // li a1, SEND         (method)
    0x0010_0613,                                        // li a2, 1            (the done word)
    0x0000_0693,                                        // li a3, 0
    0x0000_0713,                                        // li a4, 0
    0x0000_0893 | ((abi::SYS_INVOKE as u32) << 20),     // li a7, SYS_INVOKE
    0x0000_0073,                                        // ecall               (SEND)
    0x0000_0893 | ((abi::SYS_EXIT as u32) << 20),       // li a7, SYS_EXIT
    0x0000_0073,                                        // ecall               (EXIT)
];

/// **Spawn latency, measured from EL0 (the primitive suite).** lmbench's `lat_proc`: the cost to
/// build, start, run to exit, reap, and reclaim a whole child process, driven entirely from EL0
/// through the granular verbs (`SPLIT` a region, retype an address space and a TCB, map code and a
/// stack, configure, start), then `DESTROY` the child's region. This is the payoff of object
/// revocation: without reclamation the loop could not repeat.
fn spawn_bench() -> ! {
    // One shared code frame, retyped from our OWN untyped so destroying a child never frees it. Map
    // it writable in our own space, write the child stub once, and keep the cap to alias into every
    // child. The kernel makes the icache coherent when we later map it as CODE into a child.
    // SAFETY: `invoke` traps to the kernel, which validates the capability and the method
    // before acting (user_rt's contract). A caller cannot break an invariant by passing a
    // bad slot or method; it gets an error back.
    let code_frame = unsafe { invoke(SP_UNTYPED, abi::untyped::RETYPE, 0, 0, 0) } as u64;
    // SAFETY: as above: the kernel validates the capability and the method.
    unsafe { invoke(code_frame, abi::frame::MAP, SPAWN_SCRATCH_VA, 1, SP_UNTYPED) };
    // SAFETY: SPAWN_SCRATCH_VA is a page we just mapped read/write into our own address space.
    unsafe {
        let dst = SPAWN_SCRATCH_VA as *mut u32;
        for (i, &insn) in CHILD_STUB.iter().enumerate() {
            dst.add(i).write(insn);
        }
    }

    for _ in 0..SPAWN_WARMUP {
        spawn_one(code_frame);
    }
    let start = now();
    for _ in 0..SPAWN_ITERS {
        spawn_one(code_frame);
    }
    let ticks = now().wrapping_sub(start);
    send(SP_REPORT, ticks, SPAWN_ITERS, 0);
    exit();
}

/// Build one child from EL0, run it to exit, and reclaim its region. `code_frame` is the shared,
/// already-filled code page aliased into the child; everything else (address space, stack, TCB)
/// comes from the child's own region so `DESTROY` reclaims it whole.
#[inline(never)]
fn spawn_one(code_frame: u64) {
    // The child's own region, so it is independently reclaimable.
    // SAFETY: as above: the kernel validates the capability and the method.
    let child_ut = unsafe { invoke(SP_UNTYPED, abi::untyped::SPLIT, CHILD_PAGES, 0, 0) } as u64;
    // SAFETY: as above: the kernel validates the capability and the method.
    let aspace = unsafe {
        invoke(
            child_ut,
            abi::untyped::RETYPE_OBJ,
            abi::objtype::ASPACE,
            0,
            0,
        )
    } as u64;
    // Alias the shared code frame as the child's code; a stack from the child's own region.
    // SAFETY: as above: the kernel validates the capability and the method.
    unsafe {
        invoke(
            aspace,
            abi::aspace::MAP_INTO,
            CHILD_CODE_VA,
            code_frame,
            abi::aspace::MAP_CODE,
        )
    };
    // SAFETY: as above: the kernel validates the capability and the method.
    let stack = unsafe { invoke(child_ut, abi::untyped::RETYPE, 0, 0, 0) } as u64;
    // SAFETY: as above: the kernel validates the capability and the method.
    unsafe {
        invoke(
            aspace,
            abi::aspace::MAP_INTO,
            CHILD_STACK_VA,
            stack,
            abi::aspace::MAP_RW,
        )
    };
    cap_delete(stack);

    // The thread: give it CHILD_DONE (narrowed to WRITE) in its slot 0, configure (which consumes
    // the aspace cap), and start. The child drops to EL0, SENDs its done word, and exits.
    // SAFETY: as above: the kernel validates the capability and the method.
    let tcb = unsafe { invoke(child_ut, abi::untyped::RETYPE_OBJ, abi::objtype::TCB, 0, 0) } as u64;
    // SAFETY: as above: the kernel validates the capability and the method.
    unsafe {
        invoke(
            tcb,
            abi::tcb::CAP_INSERT,
            SP_CHILD_DONE,
            abi::rights::WRITE,
            0,
        )
    };
    // SAFETY: as above: the kernel validates the capability and the method.
    unsafe {
        invoke(
            tcb,
            abi::tcb::CONFIGURE,
            CHILD_CODE_VA,
            CHILD_STACK_VA + SPAWN_PAGE,
            aspace,
        )
    };
    // SAFETY: as above: the kernel validates the capability and the method.
    unsafe { invoke(tcb, abi::tcb::START, 0, 0, 0) };

    // Wait for the child to run and signal, then reclaim its region. The retry covers the sliver
    // between the child's SEND and its SYS_EXIT: DESTROY refuses a region with a still-live thread,
    // so yield until the child has finished exiting.
    let _ = recv(SP_CHILD_DONE);
    // SAFETY: as above: the kernel validates the capability and the method.
    while unsafe { invoke(child_ut, abi::untyped::DESTROY, 0, 0, 0) } != 0 {
        yield_now();
    }
    // Free the slots this child used (the aspace cap was consumed by CONFIGURE) so the fixed cspace
    // does not fill over the loop.
    cap_delete(tcb);
    cap_delete(child_ut);
}

/// One null syscall: a trap with an unrecognized number. The kernel returns an error, which we
/// discard; we are timing the crossing, not the result. aarch64 `svc` + `x8`, RISC-V `ecall` + `a7`.
#[inline(never)]
fn null_syscall() {
    // SAFETY: the trap is rejected (unknown number) with an error and no side effect.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") 0xFFFFu64, // no defined syscall has this number
            lateout("x0") _,    // the kernel writes an error here; discard it
            options(nostack, nomem),
        );
    }
    #[cfg(target_arch = "riscv64")]
    // SAFETY: `ecall` with the benchmark's null-syscall number: it traps to the kernel, which returns immediately. This is the measurement, and the options promise it touches neither memory nor the stack.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 0xFFFFu64,
            lateout("a0") _,
            options(nostack, nomem),
        );
    }
}

user_rt::panic_handler!();
