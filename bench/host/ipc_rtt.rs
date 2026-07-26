//! IPC round-trip latency on the host OS, lmbench's `lat_pipe`, and the host side of cricker-os's
//! EL0 `ipc_rtt_el0` bench (notes/benchmarks.md, milestone 25).
//!
//! Two **processes** (fork, so the round trip crosses an address-space boundary, exactly as ours
//! does over two EL0 processes) connected by a pair of pipes. One iteration is: parent writes a
//! byte, child reads it and writes a byte back, parent reads it, four syscalls and two context
//! switches, the same shape as our client SEND / server RECV+SEND / client RECV.
//!
//! Uses the direct libc `read`/`write` (what a C program and lmbench use), not the generic `syscall`
//! gate, which is ~200 ns slower here and would under-count. `rustc` emits a benign "suspicious
//! runtime symbol" warning for declaring `read`/`write`; it is expected and the declarations match
//! libc exactly. Native numbers are statistical (a shared desktop underneath), so take a median.
//!
//! Build and run natively (no Cargo, so it does not fight the aarch64 workspace target):
//!   rustc -O bench/host/ipc_rtt.rs -o /tmp/ipc_rtt && /tmp/ipc_rtt

use std::time::Instant;

unsafe extern "C" {
    fn pipe(fds: *mut i32) -> i32;
    fn fork() -> i32;
    fn read(fd: i32, buf: *mut u8, n: usize) -> isize;
    fn write(fd: i32, buf: *const u8, n: usize) -> isize;
    fn close(fd: i32) -> i32;
    fn _exit(code: i32) -> !;
}

fn main() {
    let iters: u64 = 200_000;
    let mut p2c = [0i32; 2]; // parent -> child
    let mut c2p = [0i32; 2]; // child -> parent
    unsafe {
        assert!(pipe(p2c.as_mut_ptr()) == 0, "pipe");
        assert!(pipe(c2p.as_mut_ptr()) == 0, "pipe");
    }

    if unsafe { fork() } == 0 {
        // Child: close the ends it does not use, then echo forever until the pipe closes.
        unsafe {
            close(p2c[1]);
            close(c2p[0]);
        }
        let mut b = [0u8; 1];
        loop {
            if unsafe { read(p2c[0], b.as_mut_ptr(), 1) } != 1 {
                unsafe { _exit(0) };
            }
            unsafe { write(c2p[1], b.as_ptr(), 1) };
        }
    }

    // Parent: close the ends it does not use, warm up, then time.
    unsafe {
        close(p2c[0]);
        close(c2p[1]);
    }
    let mut b = [0u8; 1];
    let round_trip = |b: &mut [u8; 1]| unsafe {
        write(p2c[1], b.as_ptr(), 1);
        read(c2p[0], b.as_mut_ptr(), 1);
    };
    for _ in 0..10_000 {
        round_trip(&mut b);
    }
    let t = Instant::now();
    for _ in 0..iters {
        round_trip(&mut b);
    }
    let ns = t.elapsed().as_nanos() as f64 / iters as f64;
    unsafe { close(p2c[1]) }; // let the child see EOF and exit
    let os = if cfg!(target_os = "macos") { "macOS" } else { "linux" };
    println!("{os} ipc_rtt (pipe round trip, 2 processes): {ns:.1} ns/iter  (iters={iters})");
}
