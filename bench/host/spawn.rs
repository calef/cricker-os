//! Process-creation latency on the host OS, lmbench's `lat_proc` (the fork+exit variant), and the
//! host side of nife's EL0 `spawn_el0` bench (notes/benchmarks.md, milestone 25).
//!
//! `fork()` a child that immediately `_exit()`s, and `wait()` for it. This is the lightest Unix
//! "make a process and reap it", and it is deliberately the *fork+exit* variant, not fork+exec: no
//! binary is loaded, so it is as close as a Unix number gets to nife's minimal spawn.
//!
//! It is still not the same operation, and the note says so plainly: `fork` duplicates the parent
//! (its address space, copy-on-write; its descriptor table; its signal state), where nife
//! builds a fresh minimal process from nothing (an address space, a TCB, an aliased code page). A
//! capability-microkernel process is a far lighter object than a Unix one, so this compares two
//! different amounts of work, and the gap is mostly that difference.
//!
//! Build and run natively:
//!   rustc -O bench/host/spawn.rs -o /tmp/spawn && /tmp/spawn

use std::time::Instant;

unsafe extern "C" {
    fn fork() -> i32;
    fn _exit(code: i32) -> !;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

/// `fork` a child that exits at once, and reap it. Returns ns per full create-and-reap cycle.
fn fork_exit_ns(iters: u64) -> f64 {
    let t = Instant::now();
    for _ in 0..iters {
        // SAFETY: fork has no arguments; the child branch calls _exit and never returns.
        let pid = unsafe { fork() };
        if pid == 0 {
            unsafe { _exit(0) };
        }
        let mut status = 0i32;
        // SAFETY: reaping the child we just forked; `status` is a valid out-pointer.
        unsafe { waitpid(pid, &mut status, 0) };
    }
    t.elapsed().as_nanos() as f64 / iters as f64
}

fn main() {
    let _ = fork_exit_ns(200); // warm the fork/exec path and the allocator
    let ns = fork_exit_ns(2_000);
    let os = if cfg!(target_os = "macos") { "macos" } else { "linux" };
    println!("{os} spawn (fork+exit+wait): {ns:.0} ns/iter");
}
