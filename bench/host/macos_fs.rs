//! **The APFS side of milestone 38's filesystem-throughput comparison**, on this laptop's own
//! filesystem, natively.
//!
//! # This one is NOT at a matched tier, and that is the first thing to know about it
//!
//! `bench/host/linux_fs.rs` boots under QEMU-HVF on the same machine model, the same `-cpu host`,
//! the same `virtio-blk-device` and the same raw host image file that nife's bench boot uses, so
//! nife and ext4 differ in the filesystem and in nothing below it. This program has none of that.
//! It runs on macOS directly, on APFS, on the real NVMe device, with no virtio and no host image
//! file in the way. It is therefore **a reference point, not a competitor**: it says what the
//! hardware and a production filesystem can do when nothing is virtualized, which is the ceiling
//! the other two are working under, and any ratio taken against it is measuring the tier as much as
//! the filesystem. milestone 25 made the same call for the primitive suite and said so the same
//! way.
//!
//! # Three variants, for the reason `linux_fs.rs` gives
//!
//! - **buffered**: ordinary reads and writes, served by the unified buffer cache.
//! - **nocache**: `fcntl(F_NOCACHE)`, macOS's nearest thing to `O_DIRECT`. The bytes still go to
//!   the device's own cache; nothing is flushed.
//! - **fullsync**: `nocache` plus `fcntl(F_FULLFSYNC)` after every write, which is macOS's real
//!   barrier (`fsync` on macOS does *not* flush the drive cache, which is the whole reason
//!   `F_FULLFSYNC` exists). Strictly stronger than what nife's FS server does per write, and
//!   printed so the strongest and weakest readings are both on the page.
//!
//! Native numbers are statistical (a shared desktop underneath), so take a median of a few runs.
//!
//! Build and run natively (no Cargo, so it does not fight the aarch64 workspace target):
//!   rustc -O --edition 2021 bench/host/macos_fs.rs -o /tmp/macos_fs && /tmp/macos_fs

use std::time::Instant;

unsafe extern "C" {
    fn open(path: *const u8, flags: i32, ...) -> i32;
    fn close(fd: i32) -> i32;
    fn pread(fd: i32, buf: *mut u8, n: usize, off: i64) -> isize;
    fn pwrite(fd: i32, buf: *const u8, n: usize, off: i64) -> isize;
    fn unlink(path: *const u8) -> i32;
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn __error() -> *mut i32;
}

const O_RDONLY: i32 = 0;
const O_WRONLY: i32 = 1;
const O_RDWR: i32 = 2;
const O_CREAT: i32 = 0x0200;
const O_TRUNC: i32 = 0x0400;
/// macOS `<sys/fcntl.h>`: turn data caching off for this descriptor.
const F_NOCACHE: i32 = 48;
/// macOS `<sys/fcntl.h>`: flush the drive's own write cache, which `fsync` does not do.
const F_FULLFSYNC: i32 = 51;

/// The measurement's shape, identical to `fs_proto::fixture::throughput` and to `linux_fs.rs`.
const UNIT: usize = 4096;
const BLOCKS: u64 = 256;
const WARMUP: u64 = 8;

fn errno() -> i32 {
    unsafe { *__error() }
}

/// The same xorshift64 both other sides use, so all three do the same work per write.
fn paint(buf: &mut [u8], state: &mut u64) {
    for chunk in buf.chunks_exact_mut(8) {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        chunk.copy_from_slice(&x.to_ne_bytes());
    }
}

fn next_offset(state: &mut u64) -> i64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    ((x % BLOCKS) * UNIT as u64) as i64
}

fn set_flags(fd: i32, nocache: bool) {
    if nocache {
        let r = unsafe { fcntl(fd, F_NOCACHE, 1) };
        assert!(r != -1, "F_NOCACHE failed, errno {}", errno());
    }
}

fn phases(path: &[u8], tag: &str, nocache: bool, fullsync: bool) {
    // A page-aligned buffer. `F_NOCACHE` does not demand it the way `O_DIRECT` does, but the
    // comparison should not turn on one side's buffer being better aligned than the other's.
    let mut backing = vec![0u8; UNIT * 2];
    let off = backing.as_ptr() as usize % UNIT;
    let start_at = if off == 0 { 0 } else { UNIT - off };
    let slice = &mut backing[start_at..start_at + UNIT];
    let mut payload = 0x9E37_79B9_7F4A_7C15_u64;

    // --- Phase 1: sequential write, which is also the file's creation. ---
    unsafe { unlink(path.as_ptr()) };
    let fd = unsafe { open(path.as_ptr(), O_WRONLY | O_CREAT | O_TRUNC, 0o644) };
    assert!(fd >= 0, "open for write failed, errno {}", errno());
    set_flags(fd, nocache);
    let t = Instant::now();
    for k in 0..BLOCKS {
        paint(slice, &mut payload);
        let n = unsafe { pwrite(fd, slice.as_ptr(), UNIT, (k * UNIT as u64) as i64) };
        assert!(n == UNIT as isize, "short write {n}, errno {}", errno());
        if fullsync {
            unsafe { fcntl(fd, F_FULLFSYNC, 0) };
        }
    }
    let seq_write = t.elapsed().as_nanos() as f64 / BLOCKS as f64;
    unsafe {
        fcntl(fd, F_FULLFSYNC, 0);
        close(fd);
    }
    // Let anything the write phase left running finish outside the read phases' windows, for the
    // reason `linux_fs.rs` gives at the same point.
    std::thread::sleep(std::time::Duration::from_millis(250));

    // --- Phase 2: sequential read. ---
    let fd = unsafe { open(path.as_ptr(), O_RDONLY, 0) };
    assert!(fd >= 0, "open for read failed, errno {}", errno());
    set_flags(fd, nocache);
    for _ in 0..WARMUP {
        unsafe { pread(fd, slice.as_mut_ptr(), UNIT, 0) };
    }
    let t = Instant::now();
    for k in 0..BLOCKS {
        let n = unsafe { pread(fd, slice.as_mut_ptr(), UNIT, (k * UNIT as u64) as i64) };
        assert!(n == UNIT as isize, "short read {n}, errno {}", errno());
    }
    let seq_read = t.elapsed().as_nanos() as f64 / BLOCKS as f64;

    // --- Phase 3: random read, from the same fixed seed the other two sides use. ---
    let mut rng = 0x38F5_7042_1DEA_u64;
    for _ in 0..WARMUP {
        unsafe { pread(fd, slice.as_mut_ptr(), UNIT, next_offset(&mut rng)) };
    }
    let t = Instant::now();
    for _ in 0..BLOCKS {
        let n = unsafe { pread(fd, slice.as_mut_ptr(), UNIT, next_offset(&mut rng)) };
        assert!(n == UNIT as isize, "short read {n}, errno {}", errno());
    }
    let rand_read = t.elapsed().as_nanos() as f64 / BLOCKS as f64;
    unsafe { close(fd) };

    // --- Phase 4: random write, in place. The offset stream runs on from phase 3. ---
    let fd = unsafe { open(path.as_ptr(), O_RDWR, 0) };
    assert!(fd >= 0, "reopen failed, errno {}", errno());
    set_flags(fd, nocache);
    for _ in 0..WARMUP {
        paint(slice, &mut payload);
        unsafe { pwrite(fd, slice.as_ptr(), UNIT, next_offset(&mut rng)) };
    }
    let t = Instant::now();
    for _ in 0..BLOCKS {
        paint(slice, &mut payload);
        let n = unsafe { pwrite(fd, slice.as_ptr(), UNIT, next_offset(&mut rng)) };
        assert!(n == UNIT as isize, "short write {n}, errno {}", errno());
        if fullsync {
            unsafe { fcntl(fd, F_FULLFSYNC, 0) };
        }
    }
    let rand_write = t.elapsed().as_nanos() as f64 / BLOCKS as f64;
    unsafe { close(fd) };
    unsafe { unlink(path.as_ptr()) };

    for (name, ns) in [
        ("seq_write", seq_write),
        ("seq_read", seq_read),
        ("rand_read", rand_read),
        ("rand_write", rand_write),
    ] {
        let mibs = (UNIT as f64) * 1e9 / ns / (1024.0 * 1024.0);
        println!("macosfs {tag}_{name} {ns:.0} ns/xfer {} KiB {mibs:.1} MiB/s", UNIT / 1024);
    }
}

fn main() {
    let dir = std::env::temp_dir();
    let path = format!("{}/nife-macos-fs-throughput\0", dir.display());
    phases(path.as_bytes(), "buffered", false, false);
    phases(path.as_bytes(), "nocache", true, false);
    phases(path.as_bytes(), "fullsync", true, true);
    println!("macosfs done");
}
