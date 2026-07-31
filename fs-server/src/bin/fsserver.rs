//! **The FS-server EL0 binary** (milestone 32 phase 2): RedoxFS, confined, served over IPC.
//!
//! The sans-IO core is [`fs_server::Server`]; this file is only the two IO edges it needs and the
//! runtime a dedicated binary carries. Below it, the [`IpcDisk`] turns the RedoxFS `Disk` trait into
//! a **blk-IPC client**: a `read_at`/`write_at`/`size` is a `CALL` to the block server, one
//! filesystem block per shared page. Above it, [`serve`] turns file-service requests from clients
//! into `Server` calls and answers through the one-shot Reply the kernel mints. The allocator is the
//! untyped-backed heap, so every byte RedoxFS allocates is paid from this process's own budget,
//! which is the whole reason phase 2 waited on milestone 27's `GlobalAlloc`.
//!
//! # Capability contract (notes/fs-server.md, notes/abi.md §4)
//! - **slot 0**: an untyped budget, the heap's (RedoxFS is alloc-heavy; nothing runs without it).
//! - **slot 1**: the block-service endpoint, `WRITE`. The FS server is the block server's client.
//! - **slot 2**: the file-service endpoint, `READ`. Clients `CALL` here; this is the directory
//!   capability, bound in the server to the image's root (phase 2). A client without it opens
//!   nothing.
//! - **[`BLK_PAGE`]**: a page shared with the block server (the block buffer).
//! - **[`FILE_PAGE`]**: a page shared with the client (a name on open, file bytes on read/write).
//!
//! The server only ever OPENS the image (never creates: creation is std-gated and host-side), and
//! it maps RedoxFS's error type to the wire exactly once, in [`serve`], via `fs_proto::reply_err`.

#![no_std]
#![no_main]

extern crate alloc;

use fs_proto::{blk, fs, op, reply_err, req};
use fs_server::Server;
use redoxfs::Disk;
use syscall::error::{EINVAL, EIO, Error, Result};
use user_rt::{call, invoke, recv_cap, send};

/// Cspace slots, by convention with the kernel-side wiring (kernel/src/user.rs `fs_service`).
const UNTYPED: u64 = 0;
const BLK: u64 = 1;
const FILE: u64 = 2;
/// A readiness endpoint: the server SENDs one word here once the image is open, before it serves.
const READY: u64 = 3;

/// Where the kernel maps the two shared pages. Above the program image (0x40_0000) and the heap
/// (0x4000_0000 + a few MiB), so nothing collides.
const BLK_PAGE: u64 = 0x5000_0000;
const FILE_PAGE: u64 = 0x5000_1000;

/// One filesystem block / one shared page, in bytes. The transfer unit both directions.
const BLOCK: usize = blk::BLOCK_SIZE;

/// The heap cap. RedoxFS keeps a record-sized compress buffer (128 KiB), block buffers, and small
/// tree structures; a few MiB is comfortable for the small images phase 2 serves. The untyped the
/// kernel grants is the real ceiling.
const HEAP_MAX: u64 = 8 * 1024 * 1024;

#[global_allocator]
static HEAP: user_rt::heap::UntypedHeap = user_rt::heap::UntypedHeap::new();

/// Read every written block straight back and compare (a `fix/redoxfs-repeat-write` diagnostic). Off
/// by default: it doubles the write cost, and its scratch block is 4 KiB of stack inside a call
/// RedoxFS makes from deep recursion, which is enough to overflow this server's stack and produce a
/// *different* failure than the one being chased. Turn it on deliberately, and raise the FS server's
/// stack grant if you do.
const VERIFY_WRITES: bool = false;

/// **The crash injector** (milestone 37, DECISIONS §34 condition 1), armed by this process's START
/// arguments and inert otherwise.
///
/// The seam is [`IpcDisk`]: it sits between the engine and the device, so it is where a block write
/// can be torn in half and where the process can be killed with a transaction half on the platter.
/// A dedicated binary would have been the alternative and was rejected: the whole point is that the
/// thing which crashes is the FS server the gate otherwise runs, not a lookalike. What arms it is
/// the `Spawn` literal, which is this process's entire authority, so a boot that does not ask for a
/// crash cannot get one.
///
/// - `arg0` (`CRASH_AT_WRITE`): which file-service `WRITE` request to die inside, counted from 1.
///   Zero disables the injector completely, which is every boot but the crash test's.
/// - `arg1` (`CRASH_AFTER_BLOCKS`): how many block writes into that request to let through before
///   dying. **One**, in the test, because one is the count that cannot miss: a write transaction
///   always issues at least one block write, and a `k` larger than the transaction is a server that
///   never dies and a test that hangs.
/// - `arg2` (`CRASH_TEAR_BYTES`): how much of that last block to actually put on the platter. The
///   block is read first and only this many bytes of the new contents are laid over it, which is a
///   real torn write at a real device: new bytes in front, the old ones still behind.
mod inject {
    use core::sync::atomic::{AtomicU64, Ordering};

    /// Which `WRITE` request to die in, from 1. Zero means never.
    pub static AT_WRITE: AtomicU64 = AtomicU64::new(0);
    /// Block writes to allow inside that request before dying.
    pub static AFTER_BLOCKS: AtomicU64 = AtomicU64::new(0);
    /// Bytes of the final block that reach the platter (0 means the write never happens at all).
    pub static TEAR_BYTES: AtomicU64 = AtomicU64::new(0);
    /// How many `WRITE` requests the serve loop has begun.
    pub static WRITES_SEEN: AtomicU64 = AtomicU64::new(0);
    /// Block writes issued since the armed request began. Only counted once armed.
    pub static BLOCKS: AtomicU64 = AtomicU64::new(0);
    /// Set when the serve loop enters the request the injector names.
    pub static ARMED: AtomicU64 = AtomicU64::new(0);

    /// Called by the serve loop as it begins a `WRITE`. Arms the injector if this is the one.
    pub fn note_write() {
        let at = AT_WRITE.load(Ordering::Relaxed);
        if at != 0 && WRITES_SEEN.fetch_add(1, Ordering::Relaxed) + 1 == at {
            ARMED.store(1, Ordering::Relaxed);
        }
    }

    /// Called by [`super::IpcDisk`] before each block write. `Some(n)` means "this is the one: put
    /// only `n` bytes of it on the platter and then die".
    pub fn tear_now() -> Option<usize> {
        if ARMED.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let n = BLOCKS.fetch_add(1, Ordering::Relaxed) + 1;
        (n == AFTER_BLOCKS.load(Ordering::Relaxed))
            .then(|| TEAR_BYTES.load(Ordering::Relaxed) as usize)
    }
}

/// The RedoxFS `Disk` over blk IPC. Stateless: everything it needs (the endpoint slot, the shared
/// page) is a fixed convention, so it is a zero-sized handle the `Server` owns.
struct IpcDisk;

impl IpcDisk {
    /// One blk `CALL`: opcode, block index. Returns the reply's first word as a signed result
    /// (negative is an error, per the wire convention). The bulk rides in [`BLK_PAGE`].
    fn blk(op_code: u64, block: u64) -> i64 {
        // SAFETY: `call` traps to the kernel, which validates the endpoint in slot BLK.
        let (r0, _) = call(BLK, req(op_code), block);
        r0 as i64
    }

    /// Copy `n` bytes out of the shared block page (a completed read landed there).
    fn from_page(dst: &mut [u8]) {
        // SAFETY: BLK_PAGE is a mapped, writable page of exactly BLOCK bytes; `dst` is no larger.
        unsafe {
            core::ptr::copy_nonoverlapping(BLK_PAGE as *const u8, dst.as_mut_ptr(), dst.len())
        }
    }

    /// Copy `src` into the shared block page (to be written). `src` is at most one block.
    fn to_page(src: &[u8]) {
        // SAFETY: as above; `src` is no larger than the page.
        unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), BLK_PAGE as *mut u8, src.len()) }
    }
}

impl Disk for IpcDisk {
    unsafe fn read_at(&mut self, block: u64, buffer: &mut [u8]) -> Result<usize> {
        // RedoxFS reads whole blocks (and multi-block records), always block-aligned; we still
        // chunk defensively so a short tail is read correctly.
        for (i, chunk) in buffer.chunks_mut(BLOCK).enumerate() {
            if Self::blk(blk::READ, block + i as u64) < 0 {
                return Err(Error::new(EIO));
            }
            Self::from_page(chunk);
        }
        Ok(buffer.len())
    }

    unsafe fn write_at(&mut self, block: u64, buffer: &[u8]) -> Result<usize> {
        for (i, chunk) in buffer.chunks(BLOCK).enumerate() {
            let b = block + i as u64;
            // The crash injection (milestone 37), and the only line the ordinary path pays for it.
            if let Some(tear) = inject::tear_now() {
                // A real torn write: read the block, lay the first `tear` bytes of the new contents
                // over it, and put THAT on the platter. New bytes in front, old bytes behind, which
                // is what a drive leaves when the rail collapses mid-block.
                if tear > 0 {
                    if Self::blk(blk::READ, b) < 0 {
                        return Err(Error::new(EIO));
                    }
                    Self::to_page(&chunk[..tear.min(chunk.len())]);
                    if Self::blk(blk::WRITE, b) < 0 {
                        return Err(Error::new(EIO));
                    }
                }
                // Then die, with the transaction's commit still unwritten. Announce it first, on the
                // readiness endpoint this process already holds, so the waiting test knows the kill
                // was the injector's and not something else going wrong.
                send(READY, fs_proto::fixture::crash::CUT, 0, 0);
                panic!();
            }
            if chunk.len() < BLOCK {
                // A partial final block would clobber the rest of the block; read-modify-write so
                // only the given bytes change. RedoxFS does not do this today, but a Disk owes it.
                if Self::blk(blk::READ, b) < 0 {
                    return Err(Error::new(EIO));
                }
            }
            Self::to_page(chunk);
            if Self::blk(blk::WRITE, b) < 0 {
                return Err(Error::new(EIO));
            }
            // WRITE-VERIFY (fix/redoxfs-repeat-write diagnostic): read the block straight back and
            // compare. If the transport is lossy (a write that does not land, or a read that returns
            // stale bytes), the engine would later walk a corrupt allocator chain and spin; catching
            // it here turns that far-away hang into an immediate, located fault.
            if VERIFY_WRITES {
                let mut echo = [0u8; BLOCK];
                if Self::blk(blk::READ, b) < 0 {
                    return Err(Error::new(EIO));
                }
                Self::from_page(&mut echo);
                if echo[..chunk.len()] != *chunk {
                    // The block did not read back as written: the transport lost or reordered it.
                    panic!();
                }
            }
        }
        Ok(buffer.len())
    }

    fn size(&mut self) -> Result<u64> {
        let n = Self::blk(blk::SIZE, 0);
        if n < 0 {
            return Err(Error::new(EIO));
        }
        Ok(n as u64)
    }
}

/// The file-service pages, as slices. The FS server reads a name (open) or file bytes (write) from
/// [`FILE_PAGE`] and writes read results back into it.
///
/// SAFETY: FILE_PAGE is a mapped, writable page of 4096 bytes shared with the one client bound to
/// this endpoint; `len` is clamped to the page below before either is used.
unsafe fn file_page(len: usize) -> &'static mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(FILE_PAGE as *mut u8, len) }
}

/// Answer a caller through its one-shot Reply capability (slot `reply`), then return to serving.
fn reply(reply_slot: u64, r0: i64) {
    // SAFETY: the kernel minted this Reply naming the blocked caller; REPLY consumes it.
    unsafe { invoke(reply_slot, abi::reply::REPLY, r0 as u64, 0, 0) };
}

/// The serve loop. Blocks on the file-service endpoint, dispatches one request, replies, repeats.
/// This is the **only** place a RedoxFS error becomes a wire value ([`reply_err`]); everything
/// below it speaks `syscall::error::Result`.
fn serve(server: &mut Server<IpcDisk>) -> ! {
    loop {
        // RECV_CAP delivers (first word, the Reply cap's slot, second word). The Reply names the
        // caller; endpoint-only naming means we never learn who they are, only how to answer.
        let (w0, reply_slot, w1) = recv_cap(FILE);
        let handle = fs::req_handle(w0) as u32;
        let len = fs::req_len(w0).min(BLOCK); // never touch past the shared page
        let offset = w1;

        let result: Result<i64> = match op(w0) {
            // The handle field is the **parent directory**, not a file: `fs::ROOT` is the endpoint's
            // bound directory, which is what every client that predates directory handles sends.
            fs::OPEN => {
                // SAFETY: the name is `len` bytes the client wrote at the start of FILE_PAGE.
                let name_bytes = unsafe { file_page(len) };
                match core::str::from_utf8(name_bytes) {
                    Ok(name) => server.open_file_at(handle, name).map(|h| h as i64),
                    Err(_) => Err(Error::new(EINVAL)),
                }
            }
            fs::READ => {
                // SAFETY: read straight into the shared page, up to one page.
                let buf = unsafe { file_page(len) };
                server.read(handle, offset, buf).map(|n| n as i64)
            }
            fs::WRITE => {
                inject::note_write(); // milestone 37: arm the crash if this is the named request
                // SAFETY: the data is `len` bytes the client wrote into the shared page.
                let data = unsafe { file_page(len) };
                server.write(handle, offset, data).map(|n| n as i64)
            }
            fs::FSTAT => server.fstat(handle).map(|s| s as i64),
            fs::CLOSE => server.close(handle).map(|()| 0),
            fs::CREATE => {
                // Same shape as OPEN, deliberately: the name is `len` bytes at the start of the
                // shared page, and the reply is a handle. A client that can open can create.
                // SAFETY: the name is `len` bytes the client wrote at the start of FILE_PAGE.
                let name_bytes = unsafe { file_page(len) };
                match core::str::from_utf8(name_bytes) {
                    Ok(name) => server.create_file_at(handle, name).map(|h| h as i64),
                    Err(_) => Err(Error::new(EINVAL)),
                }
            }
            // **The verbs that hand back authority** (milestone 47). Both share OPEN's shape: the
            // name is `len` bytes at the start of the shared page and the reply is a handle. What
            // differs is the second word, which carries the rights the caller is asking the child
            // to have rather than an offset. It is `offset` here only because that is what the
            // wire's second word is called; the server intersects it with the parent's rights and
            // refuses if the answer is smaller than the request.
            fs::OPENDIR | fs::MKDIR => {
                // SAFETY: the name is `len` bytes the client wrote at the start of FILE_PAGE.
                let name_bytes = unsafe { file_page(len) };
                match core::str::from_utf8(name_bytes) {
                    Ok(name) if op(w0) == fs::OPENDIR => {
                        server.open_dir(handle, name, offset).map(|h| h as i64)
                    }
                    Ok(name) => server.make_dir(handle, name, offset).map(|h| h as i64),
                    Err(_) => Err(Error::new(EINVAL)),
                }
            }
            // The cursor rides in the second word for TRUNCATE's reason: `len` is clamped to one
            // page above, and a cursor is an index into a directory rather than a payload length.
            // The listing goes into the shared page and `r0` says how much of it was filled.
            fs::READDIR => {
                // SAFETY: the whole page is ours to fill; the encoder never writes past its slice.
                let buf = unsafe { file_page(BLOCK) };
                server.read_dir(handle, offset as u32, buf).map(|n| n as i64)
            }
            // **The only verb that names two directories**, so the second word is a packed pair
            // (handle, length) rather than a scalar and both names ride in the shared page, source
            // first. The page is the bound: a pair of names longer than it is EINVAL here rather
            // than a clamp, because clamping a name is renaming something else.
            fs::RENAME => {
                let dst_len = fs::dst_len(offset);
                if len + dst_len > BLOCK {
                    Err(Error::new(EINVAL))
                } else {
                    // SAFETY: both names are the client's bytes at the start of FILE_PAGE, and the
                    // sum is checked against the page above.
                    let (src, dst) = unsafe { file_page(len + dst_len) }.split_at(len);
                    match (core::str::from_utf8(src), core::str::from_utf8(dst)) {
                        (Ok(src), Ok(dst)) => server
                            .rename(handle, src, fs::dst_handle(offset) as u32, dst)
                            .map(|()| 0),
                        _ => Err(Error::new(EINVAL)),
                    }
                }
            }
            // The new size rides in the second word, NOT in the length field, because it is an
            // offset-shaped quantity: `len` is clamped to one page above, which would silently cap a
            // truncate at 4096 bytes. Reading it from `offset` is what lets a file be truncated to
            // any size the filesystem can hold.
            fs::TRUNCATE => server.truncate(handle, offset).map(|()| 0),
            _ => Err(Error::new(EINVAL)),
        };

        // The one error-mapping site: RedoxFS's Error -> the negated-errno wire value.
        reply(reply_slot, result.unwrap_or_else(|e| reply_err(e.errno)));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(crash_at_write: u64, crash_after_blocks: u64, crash_tear_bytes: u64) -> ! {
    // The crash injector's arming, straight out of the START arguments (milestone 37). All zero on
    // every boot but the crash test's, and `AT_WRITE == 0` is what makes the injector inert.
    {
        use core::sync::atomic::Ordering;
        inject::AT_WRITE.store(crash_at_write, Ordering::Relaxed);
        inject::AFTER_BLOCKS.store(crash_after_blocks, Ordering::Relaxed);
        inject::TEAR_BYTES.store(crash_tear_bytes, Ordering::Relaxed);
    }
    HEAP.init(UNTYPED, user_rt::heap::DEFAULT_BASE, HEAP_MAX);

    // Open the image over blk IPC and bind to its root. A bad image (or a block server that never
    // answers correctly) faults here, which the kernel reports; the server never creates.
    let mut server = match Server::open(IpcDisk) {
        Ok(s) => s,
        Err(_) => panic!(), // not a RedoxFS image, or the disk misbehaved: die legibly.
    };
    // The image is open: signal readiness (so the test can tell an open-path hang from a serve-path
    // one), then serve forever.
    send(READY, fs_proto::fixture::READY, 0, 0);
    serve(&mut server);
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // A server fault is a dead server: trap, and the kernel reaps it legibly. One arch-specific line.
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
