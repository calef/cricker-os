//! **The FS-service client** (milestone 32 phase 2): opens a file through a granted directory
//! capability and proves the whole stack end to end.
//!
//! This is the program a milestone-31 shell will one day be. It holds an endpoint to the FS server
//! and nothing that names the server, the block server, or the disk. That endpoint IS its directory
//! capability: it can open names the server resolves under the bound directory, and it can open
//! nothing else, because there is no global namespace to reach into. It reads the `motd` the image
//! ships with, then writes a pattern to `scratch` and reads it back, and reports the `motd` head
//! plus a success sentinel. Any failed check panics, which faults, which fails the waiting test.
//!
//! # Capability contract (notes/fs-server.md, notes/abi.md §4)
//! - **slot 0**: the file-service endpoint, `WRITE` (this is the directory capability; the client
//!   `CALL`s here).
//! - **slot 1**: the report endpoint, `WRITE`.
//! - **[`FILE_VA`]**: the page shared with the FS server (a name out, file bytes both ways).

#![no_std]
#![no_main]

use fs_proto::{fixture, fs};
use user_rt::{call, exit, now, send};

/// The file-service endpoint: the client's whole authority to the filesystem. Naming a file over it
/// is a request the server resolves under the one directory this endpoint is bound to.
const FILE: u64 = 0;
/// Where the client reports success (WRITE).
const REPORT: u64 = 1;
/// The client's mapping of the page it shares with the FS server.
const FILE_VA: u64 = 0x0000_0000_0060_0000;

/// A failed check is a fault, not a wrong answer: panic, and the handler traps.
fn check(ok: bool) {
    if !ok {
        panic!();
    }
}

/// Copy `bytes` into the shared page (a name to open, or data to write).
fn put_page(bytes: &[u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        // SAFETY: FILE_VA is a mapped, writable 4096-byte page; `bytes` is far shorter.
        unsafe { core::ptr::write_volatile((FILE_VA + i as u64) as *mut u8, b) };
    }
}

/// Read `n` bytes out of the shared page (a completed read landed there).
fn get_page(n: usize, out: &mut [u8]) {
    for (i, b) in out.iter_mut().take(n).enumerate() {
        // SAFETY: as above; `n` is bounded by the page and by `out`.
        *b = unsafe { core::ptr::read_volatile((FILE_VA + i as u64) as *const u8) };
    }
}

/// Open a name under the bound directory; returns the handle. A negative reply (no such file, or a
/// forged request) faults the client, which is the correct end for a broken invariant.
fn open(name: &str) -> u64 {
    put_page(name.as_bytes());
    let (r0, _) = call(FILE, fs::req(fs::OPEN, 0, name.len() as u64), 0);
    check((r0 as i64) >= 0);
    r0
}

/// Read up to `n` bytes from `handle` at `offset` into the shared page; returns the count.
fn read(handle: u64, offset: u64, n: usize) -> usize {
    let (r0, _) = call(FILE, fs::req(fs::READ, handle, n as u64), offset);
    check((r0 as i64) >= 0);
    r0 as usize
}

/// Iterations for the timed read loop (`ROLE_BENCH`). Warmup pays the OPEN and the first cold read;
/// the timed loop then reads the same block over and over, so it measures the FS-server file-IPC
/// contract (client CALL, server dispatch, handle validation, engine read, copy into the shared page,
/// reply), warm, not disk latency. Self-timed, reported home like elbench's primitives.
const BENCH_ITERS: u64 = 2000;
const BENCH_WARMUP: u64 = 64;

/// Roles, chosen by `arg0` at START (mirrors the kernel side). 0 is the end-to-end proof the test
/// spawns; 1 is the benchmark loop the `--real --smp` bench spawns. One binary, two entries.
const ROLE_PROOF: u64 = 0;
const ROLE_BENCH: u64 = 1;

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, _a1: u64, _a2: u64) -> ! {
    match role {
        ROLE_BENCH => bench(),
        ROLE_PROOF => proof(),
        _ => proof(),
    }
}

/// **The FS-server read benchmark** (the userspace-server tax, DECISIONS §32). Open `motd` once, then
/// time a loop of reads of its first block and report `[ticks, iters]` on the report endpoint, the
/// elbench shape. The kernel's bench boot prints it as `fs_read`; against the bare `ipc_rtt_el0`
/// round trip, the difference is what the FS-server contract costs above a raw endpoint call. It is a
/// `--real`-only magnitude (the mount is device-driven, not deterministic under icount); see
/// notes/benchmarks.md and kernel/src/bench.rs.
fn bench() -> ! {
    let handle = open(fixture::MOTD_NAME);
    let block = fixture::MOTD.len();
    for _ in 0..BENCH_WARMUP {
        let _ = read(handle, 0, block);
    }
    let start = now();
    for _ in 0..BENCH_ITERS {
        let _ = read(handle, 0, block);
    }
    let ticks = now().wrapping_sub(start);
    // slot 1 is REPORT; carry ticks and the iteration count, the bench line's two fields.
    send(REPORT, ticks, BENCH_ITERS, 0);
    exit();
}

fn proof() -> ! {
    // Read the motd the image ships with, through a handle the server minted for us. This proves
    // the whole read path end to end: a real RedoxFS image we did not write, mounted by a confined
    // FS server over blk IPC, its files opened by name under a granted directory capability and read
    // by a client that names nothing else in the system.
    //
    // This client stays read-only, and that is now a scope choice rather than a blocker. It used to
    // say the on-device write looped inside RedoxFS's allocator commit; it does not (the machine
    // overruled the note once interrupt-driven block completion was restored). The on-device write
    // is proven end to end by the `std::fs` test instead, which drives the same contract through
    // more layers and has the host tool re-read the image afterwards. See notes/fs-server.md.
    let motd = open(fixture::MOTD_NAME);
    let n = read(motd, 0, fixture::MOTD.len());
    check(n == fixture::MOTD.len());
    let mut buf = [0u8; 128];
    get_page(n, &mut buf);
    check(&buf[..n] == fixture::MOTD);
    let mut head = [0u8; 8];
    head.copy_from_slice(&buf[..8]);

    // Report the motd head plus the success sentinel; the kernel asserts both.
    send(REPORT, u64::from_le_bytes(head), fixture::SUCCESS, 0);
    exit();
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // A failed check is a dead client: fault, and the kernel reports it, failing the test.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem))
    };
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
