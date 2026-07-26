//! Context-switch latency on the host OS, lmbench's `lat_ctx`, and the host side of cricker-os's EL0
//! `ctx_switch` bench (notes/benchmarks.md, milestone 25).
//!
//! A context switch cannot be timed directly on a stock OS: there is no "yield to that specific
//! process" call. lmbench isolates it by subtraction. A two-process pipe round trip is two context
//! switches plus two pipe passes (a `write` and a `read` each). A self-pipe pass, one process writing
//! a byte and reading it straight back, is a pipe pass with **no** switch. So:
//!
//!   one context switch  =  round_trip / 2  -  self_pipe_pass
//!
//! The subtraction is approximate (the blocking `read` in the round trip is a touch heavier than the
//! non-blocking one in the self-pipe, which slightly inflates the result), which is why lmbench and
//! this note treat it as a derived number, not a measured one.
//!
//! Build and run natively (no Cargo, so it does not fight the aarch64 workspace target):
//!   rustc -O bench/host/ctx_switch.rs -o /tmp/ctx_switch && /tmp/ctx_switch

use std::time::Instant;

unsafe extern "C" {
    fn pipe(fds: *mut i32) -> i32;
    fn fork() -> i32;
    fn read(fd: i32, buf: *mut u8, n: usize) -> isize;
    fn write(fd: i32, buf: *const u8, n: usize) -> isize;
    fn close(fd: i32) -> i32;
    fn _exit(code: i32) -> !;
}

/// One process, one pipe: `write` a byte then `read` it back. No context switch; measures the pipe
/// pass overhead (two syscalls plus two buffer copies) to subtract out.
fn self_pipe_pass_ns(iters: u64) -> f64 {
    let mut p = [0i32; 2];
    unsafe { pipe(p.as_mut_ptr()) };
    let mut b = [0u8; 1];
    for _ in 0..10_000 {
        unsafe {
            write(p[1], b.as_ptr(), 1);
            read(p[0], b.as_mut_ptr(), 1);
        }
    }
    let t = Instant::now();
    for _ in 0..iters {
        unsafe {
            write(p[1], b.as_ptr(), 1);
            read(p[0], b.as_mut_ptr(), 1);
        }
    }
    let ns = t.elapsed().as_nanos() as f64 / iters as f64;
    unsafe {
        close(p[0]);
        close(p[1]);
    }
    ns
}

/// Two processes, a pair of pipes: one round trip is two context switches plus two pipe passes.
fn round_trip_ns(iters: u64) -> f64 {
    let mut p2c = [0i32; 2];
    let mut c2p = [0i32; 2];
    unsafe {
        pipe(p2c.as_mut_ptr());
        pipe(c2p.as_mut_ptr());
    }
    if unsafe { fork() } == 0 {
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
    unsafe {
        close(p2c[0]);
        close(c2p[1]);
    }
    let mut b = [0u8; 1];
    let rt = |b: &mut [u8; 1]| unsafe {
        write(p2c[1], b.as_ptr(), 1);
        read(c2p[0], b.as_mut_ptr(), 1);
    };
    for _ in 0..10_000 {
        rt(&mut b);
    }
    let t = Instant::now();
    for _ in 0..iters {
        rt(&mut b);
    }
    let ns = t.elapsed().as_nanos() as f64 / iters as f64;
    unsafe { close(p2c[1]) };
    ns
}

fn main() {
    let p = self_pipe_pass_ns(1_000_000);
    let r = round_trip_ns(200_000);
    let ctx = r / 2.0 - p;
    let os = if cfg!(target_os = "macos") { "macOS" } else { "linux" };
    println!("{os} self_pipe_pass: {p:.1} ns  round_trip: {r:.1} ns  ctx_switch (derived): {ctx:.1} ns");
}
