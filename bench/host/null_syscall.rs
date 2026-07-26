//! Null-syscall latency on the host OS (macOS or Linux), the lmbench `lat_syscall null` metric, and
//! the host side of cricker-os's EL0 `null_syscall` bench (notes/benchmarks.md, milestone 25).
//!
//! A **raw** syscall through the syscall gate, not libc's `getpid`, because libc caches the pid and
//! would turn the call into a userspace read that never traps. This is the real EL0->kernel crossing.
//!
//! Build and run natively (no Cargo, so it does not fight the aarch64 workspace target):
//!   rustc -O bench/host/null_syscall.rs -o /tmp/null_syscall && /tmp/null_syscall
//!
//! The rest of the host harness (context switch and IPC round trip, over two *processes* to match
//! our measurement, plus lmbench and sel4bench) is the remaining milestone-25 work.

use std::time::Instant;

unsafe extern "C" {
    fn syscall(num: std::os::raw::c_long, ...) -> std::os::raw::c_long;
}

fn main() {
    // getpid: BSD number 20 on macOS/Darwin, Linux aarch64 number 172. Both are near-no-op syscalls.
    let sys_getpid: std::os::raw::c_long = if cfg!(target_os = "macos") { 20 } else { 172 };
    let iters: u64 = 5_000_000;

    for _ in 0..100_000 {
        unsafe { syscall(sys_getpid) };
    }
    let t = Instant::now();
    let mut acc: i64 = 0;
    for _ in 0..iters {
        acc = acc.wrapping_add(unsafe { syscall(sys_getpid) } as i64);
    }
    let ns = t.elapsed().as_nanos() as f64 / iters as f64;
    let os = if cfg!(target_os = "macos") { "macOS" } else { "linux" };
    println!("{os} null_syscall (raw getpid): {ns:.1} ns/iter  (acc={acc}, iters={iters})");
}
