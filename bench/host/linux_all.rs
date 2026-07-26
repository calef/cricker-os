//! Same-hardware Linux side of the primitive suite (milestone 25). Built as a static aarch64 binary
//! and booted as PID 1 in a one-file initramfs under QEMU-HVF, so it runs on the *same* M-series core
//! at the *same* virtualization tier as cricker-os. It measures null syscall and IPC round trip (the
//! same metrics as bench/host/null_syscall.rs and ipc_rtt.rs), prints them, and powers the machine
//! off (it is PID 1: exiting would panic the kernel).
//!
//! Build static (rust-lld, no C toolchain needed) and run under QEMU-HVF:
//!   rustc -O --target aarch64-unknown-linux-musl -C target-feature=+crt-static \
//!         -C linker=rust-lld -C link-self-contained=yes bench/host/linux_all.rs -o /tmp/linux_init
//!   (then pack /tmp/linux_init as /init in a newc cpio and boot it; see the driver in the session)

use std::time::Instant;

unsafe extern "C" {
    fn syscall(num: std::os::raw::c_long, ...) -> std::os::raw::c_long;
    fn read(fd: i32, buf: *mut u8, n: usize) -> isize;
    fn write(fd: i32, buf: *const u8, n: usize) -> isize;
    fn pipe(fds: *mut i32) -> i32;
    fn fork() -> i32;
    fn close(fd: i32) -> i32;
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn reboot(cmd: i32) -> i32;
    fn _exit(code: i32) -> !;
}

/// A self-pipe pass (write a byte, read it back, no context switch), to subtract out of the round
/// trip so what's left is the context switch. See bench/host/ctx_switch.rs for the method.
fn self_pipe_pass() -> f64 {
    let iters: u64 = 1_000_000;
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

const SYS_GETPID: std::os::raw::c_long = 172; // aarch64 Linux
const RB_POWER_OFF: i32 = 0x4321fedc_u32 as i32;

fn null_syscall() -> f64 {
    let iters: u64 = 5_000_000;
    for _ in 0..100_000 {
        unsafe { syscall(SYS_GETPID) };
    }
    let t = Instant::now();
    let mut acc: i64 = 0;
    for _ in 0..iters {
        acc = acc.wrapping_add(unsafe { syscall(SYS_GETPID) } as i64);
    }
    std::hint::black_box(acc);
    t.elapsed().as_nanos() as f64 / iters as f64
}

/// Page-map (fault-in) latency, lmbench's `lat_mmap`: the host side of cricker-os's `map_el0`. `mmap`
/// a large anonymous region, then touch one byte per page; the first touch faults and the kernel
/// installs the PTE. See bench/host/mmap.rs for the method and the two asymmetries to our `MAP_INTO`
/// (fault vs syscall trigger; the host also allocates and zeroes a page where we alias one frame).
fn map_fault() -> f64 {
    const PAGE: usize = 4096;
    const PROT_READ: i32 = 0x1;
    const PROT_WRITE: i32 = 0x2;
    const MAP_PRIVATE: i32 = 0x0002;
    const MAP_ANONYMOUS: i32 = 0x20; // aarch64 Linux
    let go = |pages: usize| -> f64 {
        let len = pages * PAGE;
        let p = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert!(p as isize != -1, "mmap failed");
        let t = Instant::now();
        for i in 0..pages {
            // SAFETY: `p` covers `pages` pages; write the first byte of page i to fault it in.
            unsafe { p.add(i * PAGE).write_volatile(1) };
        }
        let ns = t.elapsed().as_nanos() as f64 / pages as f64;
        unsafe { munmap(p, len) };
        ns
    };
    let _ = go(2_000); // warm the mmap/fault paths on a throwaway region; timed run gets a fresh one
    go(50_000)
}

/// Process-creation latency, lmbench's `lat_proc` (fork+exit): the host side of `spawn_el0`. `fork`
/// a child that exits at once, reap it. See bench/host/spawn.rs for the method and the caveat that
/// this duplicates a Unix process where cricker-os builds a minimal one.
fn spawn_fork_exit() -> f64 {
    let iters: u64 = 2_000;
    let go = |n: u64| -> f64 {
        let t = Instant::now();
        for _ in 0..n {
            // SAFETY: fork takes no args; the child branch calls _exit and never returns.
            let pid = unsafe { fork() };
            if pid == 0 {
                unsafe { _exit(0) };
            }
            let mut status = 0i32;
            unsafe { waitpid(pid, &mut status, 0) };
        }
        t.elapsed().as_nanos() as f64 / n as f64
    };
    let _ = go(200); // warm
    go(iters)
}

fn ipc_rtt() -> f64 {
    let iters: u64 = 200_000;
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
    // devtmpfs is auto-mounted by the kernel (CONFIG_DEVTMPFS_MOUNT), so /dev/console exists and PID
    // 1's stdout reaches the serial line: a plain println works.
    println!("linux null_syscall (raw getpid): {:.1} ns/iter", null_syscall());
    let rt = ipc_rtt();
    println!("linux ipc_rtt (pipe round trip, 2 processes): {rt:.1} ns/iter");
    let p = self_pipe_pass();
    println!(
        "linux ctx_switch (derived): {:.1} ns  (round_trip {rt:.1}, self_pipe {p:.1})",
        rt / 2.0 - p
    );
    println!("linux map (first-touch fault-in): {:.1} ns/page", map_fault());
    println!(
        "linux spawn (fork+exit+wait): {:.0} ns/iter",
        spawn_fork_exit()
    );
    println!("linux: bench done, powering off");
    unsafe { reboot(RB_POWER_OFF) };
    loop {
        std::hint::spin_loop();
    }
}
