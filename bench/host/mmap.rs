//! Page-map (fault-in) latency on the host OS, lmbench's `lat_mmap`, and the host side of cricker-os's
//! EL0 `map_el0` bench (notes/benchmarks.md, milestone 25).
//!
//! What "map one page" costs, as apples-to-apples with our `MAP_INTO` as a stock OS allows. Neither a
//! bare `mmap` syscall (lazy on both Linux and macOS: it creates the VMA and installs no PTE) nor our
//! `MAP_INTO` (which eagerly writes one PTE) is the other's equal. The act they share is the trap that
//! installs a page-table entry, so that is what we time: `mmap` a large anonymous region, then touch
//! one byte per page. The first touch faults, and the kernel's fault handler installs the PTE, a
//! user->kernel->user round trip like our `svc`. Per page = touch-loop time / pages. This is exactly
//! lmbench's `lat_mmap` shape.
//!
//! Two honest asymmetries (see the note): the trigger differs (a page fault here, an explicit syscall
//! in `MAP_INTO`), and the host also allocates and zeroes a fresh page where our bench aliases one
//! existing frame, so the host number carries a page-zeroing cost ours does not.
//!
//! Build and run natively (no Cargo, so it does not fight the aarch64 workspace target):
//!   rustc -O bench/host/mmap.rs -o /tmp/mmap && /tmp/mmap

use std::time::Instant;

unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
}

const PROT_READ: i32 = 0x1;
const PROT_WRITE: i32 = 0x2;
const MAP_PRIVATE: i32 = 0x0002;
// MAP_ANON's value differs by OS; everything else above is shared.
#[cfg(target_os = "macos")]
const MAP_ANON: i32 = 0x1000;
#[cfg(not(target_os = "macos"))]
const MAP_ANON: i32 = 0x20; // Linux MAP_ANONYMOUS

const PAGE: usize = 4096;

/// Fault in `pages` fresh anonymous pages, one write each, and return ns per page. `mmap` is lazy, so
/// the first write to each page traps into the kernel's fault handler, which installs the PTE (and,
/// unlike our aliasing bench, allocates and zeroes a page). `write_volatile` cannot be optimized away.
fn map_fault_ns(pages: usize) -> f64 {
    let len = pages * PAGE;
    let p = unsafe {
        mmap(
            std::ptr::null_mut(),
            len,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANON,
            -1,
            0,
        )
    };
    assert!(p as isize != -1, "mmap failed");
    let t = Instant::now();
    for i in 0..pages {
        // SAFETY: `p` covers `pages` pages; this writes the first byte of page i, faulting it in.
        unsafe { p.add(i * PAGE).write_volatile(1) };
    }
    let ns = t.elapsed().as_nanos() as f64 / pages as f64;
    unsafe { munmap(p, len) };
    ns
}

fn main() {
    // Warm the mmap and fault paths on a throwaway region (allocator, loop, branch predictor). The
    // timed run uses a fresh region: a first touch cannot be warmed on the same pages, since once
    // faulted they stay resident and a second touch would measure a hit, not a map.
    let _ = map_fault_ns(2_000);
    let ns = map_fault_ns(50_000);
    let os = if cfg!(target_os = "macos") { "macos" } else { "linux" };
    println!("{os} map (first-touch fault-in): {ns:.1} ns/page");
}
