//! **The ext4 side of milestone 38's filesystem-throughput comparison.** Built as a static aarch64
//! binary and booted as PID 1 in a one-file initramfs under QEMU-HVF, on the *same* M-series core,
//! at the *same* virtualization tier, through the *same* virtio-blk-device model and the *same*
//! raw host image file as nife's own bench boot. It mounts a scratch ext4 disk, runs the four
//! phases `fs_proto::fixture::throughput` defines, prints them, and powers the machine off (it is
//! PID 1: exiting would panic the kernel).
//!
//! # What it measures, and why there are two variants of everything
//!
//! nife's FS server holds no **data** cache: a record body is always a virtio round trip (batched
//! since milestone 138 step 4, but never answered from memory), and every write is its own RedoxFS
//! transaction that commits before the reply. It does hold a small **metadata** cache since step 2
//! (`fs_server::CachedDisk`, 64 blocks, ~257 KiB): a repeated read of the same file's tree spine
//! answers from memory rather than a second round trip, which is why `fs_read` (a repeated read of
//! one small inline file) is no longer comparable to a cold read the way it was when this file's
//! comment last described it as cacheless. The phases this file runs are dominated by record
//! bodies, which the metadata cache never touches, so the comparison below is still the honest one
//! for what it measures; it would not be for a workload that mostly reopens hot metadata. Linux is
//! the opposite by default in both dimensions: the page cache absorbs data reads and writeback
//! absorbs writes, and a 1 MiB file fits in it entirely.
//!
//! So a single ext4 number would answer a question nobody asked. This runs each phase twice:
//!
//! - **buffered**: ordinary `open`/`pread`/`pwrite`. What a Linux program actually gets, and the
//!   number to quote when someone asks how fast Linux is.
//! - **direct**: `O_DIRECT` for reads, `O_DIRECT | O_DSYNC` for writes. No page cache, and a write
//!   is durable before it returns. That is the closest thing Linux has to what our path does on
//!   every single request, and it is the only one of the two that is apples to apples.
//!
//! Both still enjoy one thing we do not: the host's own page cache under QEMU, since the drive is
//! attached `cache=writeback` (QEMU's default, and what the nife runner uses). That is matched
//! rather than removed, which is the point of running at the same tier.
//!
//! Build static and boot it with `sh bench/host/run_linux_fs.sh`.

use std::time::Instant;

unsafe extern "C" {
    fn mount(
        src: *const u8,
        target: *const u8,
        fstype: *const u8,
        flags: u64,
        data: *const u8,
    ) -> i32;
    fn mkdir(path: *const u8, mode: u32) -> i32;
    fn open(path: *const u8, flags: i32, ...) -> i32;
    fn close(fd: i32) -> i32;
    fn pread(fd: i32, buf: *mut u8, n: usize, off: i64) -> isize;
    fn pwrite(fd: i32, buf: *const u8, n: usize, off: i64) -> isize;
    fn unlink(path: *const u8) -> i32;
    fn sync();
    fn umount(target: *const u8) -> i32;
    fn usleep(us: u32) -> i32;
    fn memalign(align: usize, size: usize) -> *mut u8;
    fn reboot(cmd: i32) -> i32;
    fn syscall(num: std::os::raw::c_long, ...) -> std::os::raw::c_long;
    fn __errno_location() -> *mut i32;
}

/// aarch64 Linux. `finit_module` takes an open fd, which is what an initramfs makes easy.
const SYS_FINIT_MODULE: std::os::raw::c_long = 273;

const O_RDONLY: i32 = 0;
const O_WRONLY: i32 = 1;
const O_RDWR: i32 = 2;
const O_CREAT: i32 = 0o100;
const O_TRUNC: i32 = 0o1000;
const O_DSYNC: i32 = 0o10000;
const O_DIRECT: i32 = 0o200000; // aarch64 Linux
const RB_POWER_OFF: i32 = 0x4321fedc_u32 as i32;

/// The measurement's shape, kept identical to `fs_proto::fixture::throughput` by hand. It is not
/// shared as a crate because this file is compiled by `rustc` alone, outside the workspace, exactly
/// as `linux_all.rs` is: a benchmark that needed the workspace to build could not be run on a
/// machine that is not this one.
const UNIT: usize = 4096;
const BLOCKS: u64 = 256;
const WARMUP: u64 = 8;

fn errno() -> i32 {
    unsafe { *__errno_location() }
}

/// The same xorshift64 the nife client uses, filling the buffer with incompressible bytes. ext4
/// does not compress, so this is not needed for ext4's sake; it is here so that both sides of the
/// comparison do the same work per write, and so the buffer's cost is the same on both.
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

fn next_offset(state: &mut u64, unit: usize) -> i64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    ((x % BLOCKS) * unit as u64) as i64
}

/// One run of the four phases against `path`. Prints four lines.
///
/// `direct` adds `O_DIRECT` (no page cache) and `dsync` adds `O_DSYNC` (the write is durable on the
/// medium before it returns). The two are separate because **nife sits between them**: the FS
/// server puts every write through a RedoxFS transaction that commits to the header ring before it
/// replies, so there is no dirty state anywhere above the device, but it issues no
/// `VIRTIO_BLK_T_FLUSH` unless a client asks for one (`fs_proto::fs::SYNC`, milestone 55). So
/// `direct` alone is the closest analogue for where the bytes are, and `direct` plus `dsync` is the
/// closest analogue for the metadata commit, and quoting only one of them would be picking the
/// answer.
///
/// `path` under `/dev` is treated as a raw block device: nothing is unlinked and nothing is
/// created. That variant is the floor both filesystems stand on, which is the number that says how
/// much of either one's cost is the filesystem and how much is virtio under HVF.
fn phases(path: &[u8], tag: &str, direct: bool, dsync: bool, unit: usize) {
    let is_dev = path.starts_with(b"/dev/");
    let extra = if direct { O_DIRECT } else { 0 } | if dsync { O_DSYNC } else { 0 };
    // A page-aligned buffer: O_DIRECT refuses anything else.
    let buf = unsafe { memalign(unit, unit) };
    assert!(!buf.is_null(), "memalign");
    let slice = unsafe { std::slice::from_raw_parts_mut(buf, unit) };
    let mut payload = 0x9E37_79B9_7F4A_7C15_u64;

    // --- Phase 1: sequential write, which is also the file's creation. ---
    if !is_dev {
        unsafe { unlink(path.as_ptr()) };
    }
    let wflags = if is_dev {
        O_WRONLY | extra
    } else {
        O_WRONLY | O_CREAT | O_TRUNC | extra
    };
    let fd = unsafe { open(path.as_ptr(), wflags, 0o644) };
    assert!(fd >= 0, "open for write failed, errno {}", errno());
    let t = Instant::now();
    for k in 0..BLOCKS {
        paint(slice, &mut payload);
        let n = unsafe { pwrite(fd, buf, unit, (k * unit as u64) as i64) };
        assert!(n == unit as isize, "short write {n}, errno {}", errno());
    }
    let seq_write = t.elapsed().as_nanos() as f64 / BLOCKS as f64;
    unsafe { close(fd) };

    // **Let the write phase finish before the read phases start**, outside anybody's window. Linux
    // has background work after a write returns even when the write was `O_DSYNC` (jbd2 checkpoints
    // the journal, and in the buffered variant there is a whole file of dirty pages), and a read
    // phase that ran into it would be timing the previous phase. nife needs no equivalent and gets
    // none: RedoxFS commits inside the request, so when the reply arrives there is nothing left
    // running. The asymmetry is in Linux's favour, which is the direction to err in.
    unsafe {
        sync();
        usleep(250_000);
    }

    // --- Phase 2: sequential read. ---
    let fd = unsafe { open(path.as_ptr(), O_RDONLY | extra, 0) };
    assert!(fd >= 0, "open for read failed, errno {}", errno());
    for _ in 0..WARMUP {
        unsafe { pread(fd, buf, unit, 0) };
    }
    let t = Instant::now();
    for k in 0..BLOCKS {
        let n = unsafe { pread(fd, buf, unit, (k * unit as u64) as i64) };
        assert!(n == unit as isize, "short read {n}, errno {}", errno());
    }
    let seq_read = t.elapsed().as_nanos() as f64 / BLOCKS as f64;

    // --- Phase 3: random read, from the same fixed seed the nife client uses. ---
    let mut rng = 0x38F5_7042_1DEA_u64;
    for _ in 0..WARMUP {
        unsafe { pread(fd, buf, unit, next_offset(&mut rng, unit)) };
    }
    let t = Instant::now();
    for _ in 0..BLOCKS {
        let n = unsafe { pread(fd, buf, unit, next_offset(&mut rng, unit)) };
        assert!(n == unit as isize, "short read {n}, errno {}", errno());
    }
    let rand_read = t.elapsed().as_nanos() as f64 / BLOCKS as f64;
    unsafe { close(fd) };

    // --- Phase 4: random write, in place. The offset stream runs on from phase 3. ---
    let fd = unsafe { open(path.as_ptr(), O_RDWR | extra, 0) };
    assert!(fd >= 0, "reopen failed, errno {}", errno());
    for _ in 0..WARMUP {
        paint(slice, &mut payload);
        unsafe { pwrite(fd, buf, unit, next_offset(&mut rng, unit)) };
    }
    let t = Instant::now();
    for _ in 0..BLOCKS {
        paint(slice, &mut payload);
        let n = unsafe { pwrite(fd, buf, unit, next_offset(&mut rng, unit)) };
        assert!(n == unit as isize, "short write {n}, errno {}", errno());
    }
    let rand_write = t.elapsed().as_nanos() as f64 / BLOCKS as f64;
    unsafe { close(fd) };

    // ns per 4 KiB transfer, and MiB/s, so neither side of the comparison has to be divided by
    // hand into the other's units.
    for (name, ns) in [
        ("seq_write", seq_write),
        ("seq_read", seq_read),
        ("rand_read", rand_read),
        ("rand_write", rand_write),
    ] {
        let mibs = (unit as f64) * 1e9 / ns / (1024.0 * 1024.0);
        println!("linuxfs {tag}_{name} {ns:.0} ns/xfer {} KiB {mibs:.1} MiB/s", unit / 1024);
    }
}

/// **Insert one kernel module by path.** Alpine's `virt` kernel keeps ext4 in `modloop-virt`
/// rather than building it in, so a one-file initramfs that just calls `mount("ext4", ...)` gets
/// ENODEV. `run_linux_fs.sh` lifts the five modules out of that squashfs and this loads them, in
/// dependency order, before anything else happens.
///
/// A failure is reported and not fatal: `crc32c_generic` is already built in on some
/// configurations and answers EEXIST, and only the ext4 load actually has to succeed, which the
/// mount that follows proves anyway.
fn insert_module(path: &[u8]) {
    let fd = unsafe { open(path.as_ptr(), O_RDONLY, 0) };
    if fd < 0 {
        println!("linuxfs note: no {}", String::from_utf8_lossy(&path[..path.len() - 1]));
        return;
    }
    let r = unsafe { syscall(SYS_FINIT_MODULE, fd as std::os::raw::c_long, c"".as_ptr(), 0) };
    if r != 0 {
        println!(
            "linuxfs note: {} not inserted, errno {}",
            String::from_utf8_lossy(&path[..path.len() - 1]),
            errno()
        );
    }
    unsafe { close(fd) };
}

fn main() {
    // /dev, so /dev/vda exists at all, then the scratch disk.
    unsafe {
        mkdir(c"/dev".as_ptr() as *const u8, 0o755);
        let r = mount(
            c"devtmpfs".as_ptr() as *const u8,
            c"/dev".as_ptr() as *const u8,
            c"devtmpfs".as_ptr() as *const u8,
            0,
            core::ptr::null(),
        );
        assert!(r == 0, "mount devtmpfs failed, errno {}", errno());
    }
    // The disk driver and ext4, with their dependencies, in the order modprobe would have chosen.
    // `virtio_blk` is a module in this kernel too, which is why the second version of this file
    // still could not find /dev/vda after ext4 loaded cleanly.
    for m in [
        b"/mods/virtio_mmio.ko\0".as_slice(),
        b"/mods/virtio_blk.ko\0".as_slice(),
        b"/mods/crc16.ko\0".as_slice(),
        b"/mods/crc32c_generic.ko\0".as_slice(),
        b"/mods/mbcache.ko\0".as_slice(),
        b"/mods/jbd2.ko\0".as_slice(),
        b"/mods/ext4.ko\0".as_slice(),
    ] {
        insert_module(m);
    }
    unsafe {
        mkdir(c"/mnt".as_ptr() as *const u8, 0o755);
        let r = mount(
            c"/dev/vda".as_ptr() as *const u8,
            c"/mnt".as_ptr() as *const u8,
            c"ext4".as_ptr() as *const u8,
            0,
            core::ptr::null(),
        );
        assert!(r == 0, "mount ext4 failed, errno {}", errno());
    }

    phases(b"/mnt/throughput\0", "buffered", false, false, UNIT);
    phases(b"/mnt/throughput\0", "direct", true, false, UNIT);
    phases(b"/mnt/throughput\0", "direct_dsync", true, true, UNIT);
    // **What our 4 KiB cap costs, priced on the other system.** A `fs_proto` request carries at
    // most one page because the payload travels through one shared page, so nife has no choice
    // about the unit; a Linux program has, and would take 64 KiB without thinking about it. This
    // row is the same ext4, the same flags and the same total bytes, with the unit a real program
    // would use, and the gap to `direct` is the size of the prize a multi-page transfer would be
    // chasing. It is the `pipe_16` / `pipe_64k` split bench/host/pipe_throughput.rs already draws
    // for the same reason.
    phases(b"/mnt/throughput\0", "direct_64k", true, false, 64 * 1024);
    // The floor: the same four phases straight at the virtio disk, no filesystem in the way, so the
    // same device, the same host image file and the same accelerator with everything above the
    // block layer removed. It goes **last and after an unmount**, because it writes over the first
    // megabyte of the disk and that is the ext4 the phases above were using. The image is rebuilt
    // by `run_linux_fs.sh` on every run, so destroying it here costs nothing.
    unsafe {
        sync();
        let r = umount(c"/mnt".as_ptr() as *const u8);
        assert!(r == 0, "umount failed, errno {}", errno());
    }
    phases(b"/dev/vda\0", "rawdev", true, false, UNIT);

    println!("linuxfs done");
    unsafe {
        reboot(RB_POWER_OFF);
        std::process::exit(0);
    }
}
