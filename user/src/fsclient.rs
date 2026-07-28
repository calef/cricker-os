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
use user_rt::{call, exit, send};

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

/// Write `data` to `handle` at `offset`; checks the whole slice was accepted.
fn write(handle: u64, offset: u64, data: &[u8]) {
    put_page(data);
    let (r0, _) = call(FILE, fs::req(fs::WRITE, handle, data.len() as u64), offset);
    check((r0 as i64) >= 0);
    check(r0 as usize == data.len());
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, _a1: u64, _a2: u64) -> ! {
    // 1. Read the motd the image ships with, through a handle the server minted for us.
    let motd = open(fixture::MOTD_NAME);
    let n = read(motd, 0, fixture::MOTD.len());
    check(n == fixture::MOTD.len());
    let mut buf = [0u8; 128];
    get_page(n, &mut buf);
    check(&buf[..n] == fixture::MOTD);
    // The head word to report, captured before the shared page is reused for the write test.
    let mut head = [0u8; 8];
    head.copy_from_slice(&buf[..8]);

    // 2. Write a pattern to scratch and read it straight back: the write path, end to end.
    let scratch = open(fixture::SCRATCH_NAME);
    write(scratch, 0, fixture::WRITE_PATTERN);
    let m = read(scratch, 0, fixture::WRITE_PATTERN.len());
    check(m == fixture::WRITE_PATTERN.len());
    get_page(m, &mut buf);
    check(&buf[..m] == fixture::WRITE_PATTERN);

    // 3. Report the motd head plus the success sentinel; the kernel asserts both.
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
