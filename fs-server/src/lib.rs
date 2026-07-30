//! **The FS-server core: RedoxFS behind a capability-shaped file service** (milestone 32 phase 2).
//!
//! This is the sans-IO heart of the FS server, in the `linedisc` spirit: the filesystem logic
//! with no IPC and no device in it, so it is host-tested in milliseconds against a real RedoxFS
//! image and only the wiring needs QEMU. The EL0 binary (this crate's `el0` build) wraps a
//! [`Server`] in the block-IPC [`redoxfs::Disk`] and the file-service serve loop; everything the
//! filesystem actually *does* lives here.
//!
//! # The contract, capability-shaped from birth
//!
//! A [`Server`] is opened **bound to one directory** ([`Server::open`] binds the RedoxFS root; a
//! future per-directory grant binds a subtree). Every name a client presents is resolved *under that
//! bound directory* ([`Server::open_file`]): there is no absolute path, no `..` escape, no global
//! namespace. In the running system the client reaches this server only by holding an endpoint the
//! server reads on, and that endpoint IS the directory capability; a client without it can open
//! nothing. A successful open returns a **handle**, a small integer this server minted and will
//! validate against its own table, which is itself a capability: forging one is meaningless because
//! the server honors only the handles it issued. See notes/fs-server.md.
//!
//! # The error boundary
//!
//! Every method here returns `syscall::error::Result`, RedoxFS's own error type, unmapped. The
//! translation to the wire (a negated errno, `fs_proto::reply_err`) happens once, in the serve loop
//! at the process boundary, and nowhere else. Keeping the core in RedoxFS's error vocabulary is what
//! makes that rule enforceable: there is no ABI type in here to leak.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

use redoxfs::{Disk, FileSystem, Node, TreePtr};
use syscall::error::{EBADF, EEXIST, EINVAL, Error, Result};

/// A file service over one RedoxFS image on one [`Disk`]. Generic over the disk so the host tests
/// drive a `DiskMemory`/`DiskFile` and the EL0 binary drives the block-IPC client, with identical
/// filesystem code between them.
pub struct Server<D: Disk> {
    fs: FileSystem<D>,
    /// The directory every name resolves under. The root for phase 2; the seam where milestone 31's
    /// per-directory grant will bind a subtree instead.
    dir: TreePtr<Node>,
    /// The open-handle table. `handles[h]` is the node a handle names, or `None` for a freed slot;
    /// the index is the handle. A capability the server issues and checks, never trusts.
    handles: Vec<Option<TreePtr<Node>>>,
    /// A monotonically increasing stand-in for wall-clock seconds, so a write advances the node's
    /// mtime deterministically. The server has no RTC; the value only needs to move forward, and the
    /// host tests assert on file *contents*, not timestamps.
    clock: u64,
}

impl<D: Disk> Server<D> {
    /// Open an existing RedoxFS image and bind the service to its root directory.
    ///
    /// `cleanup: true` matches the mount path: it replays the header ring to the newest *consistent*
    /// generation and tidies allocations. That is exactly the recovery that makes a kill-mid-write
    /// safe, so the server always opens this way, and never with the creation APIs (those are
    /// std-gated and stay host-side: the server opens, it never makes).
    pub fn open(disk: D) -> Result<Self> {
        let fs = FileSystem::open(disk, None, None, true)?;
        Ok(Self {
            fs,
            dir: TreePtr::root(),
            handles: Vec::new(),
            clock: 1,
        })
    }

    /// Resolve `name` under the bound directory and return a handle for it. `ENOENT` if there is no
    /// such entry. The name is a single component resolved in the bound directory; it is not a path,
    /// which is the whole point (no walk, no escape).
    pub fn open_file(&mut self, name: &str) -> Result<u32> {
        check_component(name)?;
        let dir = self.dir;
        let node = self.fs.tx(|tx| tx.find_node(dir, name))?;
        let ptr = node.ptr();
        Ok(self.install(ptr))
    }

    /// Read up to `buf.len()` bytes from `handle` at `offset`, returning the count (0 at EOF). Passes
    /// atime 0 so a read never advances the node's access time (never *newer* than what is stored),
    /// which keeps reads from triggering a copy-on-write of the node on every call.
    pub fn read(&mut self, handle: u32, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let ptr = self.node(handle)?;
        self.fs.tx(|tx| tx.read_node(ptr, offset, buf, 0, 0))
    }

    /// Write `data` to `handle` at `offset`, returning the count. Advances the internal clock so the
    /// node's mtime moves forward; the data persists regardless of the timestamp.
    pub fn write(&mut self, handle: u32, offset: u64, data: &[u8]) -> Result<usize> {
        let ptr = self.node(handle)?;
        self.clock += 1;
        let now = self.clock;
        self.fs.tx(|tx| tx.write_node(ptr, offset, data, now, 0))
    }

    /// The current size, in bytes, of the file a handle names.
    pub fn fstat(&mut self, handle: u32) -> Result<u64> {
        let ptr = self.node(handle)?;
        self.fs.tx(|tx| Ok(tx.read_tree(ptr)?.data().size()))
    }

    /// Create `name` under the bound directory and return a handle for it (milestone 31 phase 2).
    ///
    /// **Semantics, stated because the absence of these verbs cost a day.** If `name` does not exist
    /// it is created empty and opened. If it *does* exist this returns `EEXIST` and creates nothing:
    /// create is create, not create-or-open, so a caller that wants either has to say which it wants.
    /// The alternative (silently opening an existing file) is what makes a partly-working write look
    /// like a working one, which is the exact failure mode §27 records.
    ///
    /// Resolved under the bound directory like every other name, so this cannot create outside the
    /// granted subtree, and [`check_component`] rejects a path rather than walking one.
    pub fn create_file(&mut self, name: &str) -> Result<u32> {
        check_component(name)?;
        let dir = self.dir;
        // Existence is checked inside the same transaction as the create, so two clients racing on
        // one name cannot both be told they created it.
        self.clock += 1;
        let now = self.clock;
        let node = self.fs.tx(|tx| {
            if tx.find_node(dir, name).is_ok() {
                return Err(Error::new(EEXIST));
            }
            tx.create_node(dir, name, Node::MODE_FILE | 0o644, now, 0)
        })?;
        let ptr = node.ptr();
        Ok(self.install(ptr))
    }

    /// Set the size of the file a handle names to exactly `size` bytes (milestone 31 phase 2).
    ///
    /// **Both directions, deliberately.** Shrinking discards the bytes past `size`; growing extends
    /// with zeroes. That is POSIX `ftruncate` semantics and it is what makes a shorter write able to
    /// *replace* a file's contents rather than leave the old tail behind. §27 records what the absence
    /// of this cost: a 61-byte write over a 64-byte file left a 64-byte file, three investigations read
    /// the resulting assertion failure as a write failure, and the engine was never at fault.
    ///
    /// **What is guaranteed if the machine dies mid-truncate: untested (milestone 37).** RedoxFS is
    /// copy-on-write and commits through a header ring, and the server always mounts with the
    /// `cleanup: true` replay that walks back to the newest *consistent* generation, so the engine's
    /// design says a torn truncate leaves either the old size or the new one. We have not injected a
    /// torn or dropped write to check, so that is a design claim and not a measurement. Truncate and
    /// create are the first verbs that change a file's *shape* rather than its bytes, which makes them
    /// the most interesting cases for milestone 37 to attack.
    pub fn truncate(&mut self, handle: u32, size: u64) -> Result<()> {
        let ptr = self.node(handle)?;
        self.clock += 1;
        let now = self.clock;
        self.fs.tx(|tx| tx.truncate_node(ptr, size, now, 0))
    }

    /// Release a handle. `EBADF` if it was not open.
    pub fn close(&mut self, handle: u32) -> Result<()> {
        let slot = self
            .handles
            .get_mut(handle as usize)
            .filter(|s| s.is_some())
            .ok_or(Error::new(EBADF))?;
        *slot = None;
        Ok(())
    }

    /// Install a node in the handle table, reusing a freed slot before growing. Returns the handle.
    fn install(&mut self, ptr: TreePtr<Node>) -> u32 {
        if let Some(i) = self.handles.iter().position(|s| s.is_none()) {
            self.handles[i] = Some(ptr);
            i as u32
        } else {
            self.handles.push(Some(ptr));
            (self.handles.len() - 1) as u32
        }
    }

    /// The node a handle names, or `EBADF`. Every operation goes through here, so a forged or stale
    /// handle is refused in exactly one place.
    fn node(&self, handle: u32) -> Result<TreePtr<Node>> {
        self.handles
            .get(handle as usize)
            .copied()
            .flatten()
            .ok_or(Error::new(EBADF))
    }
}

/// Reject anything that is not a single path component, with `EINVAL`.
///
/// **This is our rule, enforced at our boundary, and it was previously true only by accident.** The
/// contract has always said a name is a single component resolved under the bound directory, with no
/// absolute path and no `..` escape (§27, and this module's own header). RedoxFS does not enforce that:
/// its `check_name` rejects only `:`, over-long names, and duplicates, so `/`, `.` and `..` pass
/// straight through. Nothing walked paths, so nothing escaped, which made the invariant hold by the
/// absence of a walker rather than by a check.
///
/// Adding `CREATE` (milestone 31 phase 2) turned that from a latent oddity into something a client
/// could *write*: `create_file("../escape")` created a directory entry literally named `../escape` in
/// the bound directory. Still not a traversal, and still a landmine, because the moment anything walks
/// paths (a per-directory grant binding a subtree, the host tool, or the image mounted through Redox's
/// FUSE driver) that entry means something it should never have been able to mean. A found-and-fixed
/// gap, not a theoretical one: the test that caught it asserted a property the docs already claimed.
///
/// Deliberately checked here rather than patched into the vendored engine: it is a rule of *our*
/// contract, not a bug in RedoxFS, whose callers are free to name entries whatever they like.
fn check_component(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.contains('\0')
    {
        return Err(Error::new(EINVAL));
    }
    Ok(())
}

/// One filesystem block, in bytes (RedoxFS's `BLOCK_SIZE`): the unit [`BlockIo`] transfers.
pub const BLOCK: usize = 4096;

/// A block-granular transport: read or write one whole filesystem block, or report the disk size.
/// The EL0 binary implements this over blk IPC; a host test implements it over a `Vec`.
///
/// This exists so the **chunking** that bridges RedoxFS's byte-addressed, arbitrary-length `Disk`
/// calls to whole-block transfers is written once and host-tested, instead of living only in the EL0
/// binary where no host test can reach it. That gap is what let the repeat-write bug hide.
pub trait BlockIo {
    /// Read filesystem block `block` into `buf` (exactly [`BLOCK`] bytes).
    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK]) -> Result<()>;
    /// Write `buf` (exactly [`BLOCK`] bytes) to filesystem block `block`.
    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK]) -> Result<()>;
    /// The disk size, in bytes.
    fn size_bytes(&mut self) -> Result<u64>;
}

/// A RedoxFS [`Disk`] over a [`BlockIo`]. RedoxFS calls `read_at`/`write_at` with a block index and a
/// buffer that may be shorter than a block (a compressed record), exactly a block, or many blocks
/// long (an uncompressed 128 KiB record), so this splits each call into whole-block transfers.
///
/// A write whose final chunk is short is **read-modify-written**: the block is read first, the chunk
/// overwrites its front, and the whole block goes back. That preserves the bytes past the chunk,
/// which is what a byte-addressed disk (`DiskFile`, where `write_at` writes exactly the buffer) does,
/// and RedoxFS relies on it for compressed records.
pub struct BlockDisk<T>(pub T);

impl<T: BlockIo> Disk for BlockDisk<T> {
    unsafe fn read_at(&mut self, block: u64, buffer: &mut [u8]) -> Result<usize> {
        let mut scratch = [0u8; BLOCK];
        for (i, chunk) in buffer.chunks_mut(BLOCK).enumerate() {
            self.0.read_block(block + i as u64, &mut scratch)?;
            chunk.copy_from_slice(&scratch[..chunk.len()]);
        }
        Ok(buffer.len())
    }

    unsafe fn write_at(&mut self, block: u64, buffer: &[u8]) -> Result<usize> {
        let mut scratch = [0u8; BLOCK];
        for (i, chunk) in buffer.chunks(BLOCK).enumerate() {
            let b = block + i as u64;
            if chunk.len() < BLOCK {
                self.0.read_block(b, &mut scratch)?;
            }
            scratch[..chunk.len()].copy_from_slice(chunk);
            self.0.write_block(b, &scratch)?;
        }
        Ok(buffer.len())
    }

    fn size(&mut self) -> Result<u64> {
        self.0.size_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redoxfs::DiskMemory;

    /// A [`BlockIo`] over an in-memory image, so the exact chunking the EL0 binary uses is exercised
    /// on the host. Byte for byte it must behave like `DiskMemory`.
    struct VecIo(Vec<u8>);
    impl BlockIo for VecIo {
        fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK]) -> Result<()> {
            let off = block as usize * BLOCK;
            buf.copy_from_slice(&self.0[off..off + BLOCK]);
            Ok(())
        }
        fn write_block(&mut self, block: u64, buf: &[u8; BLOCK]) -> Result<()> {
            let off = block as usize * BLOCK;
            self.0[off..off + BLOCK].copy_from_slice(buf);
            Ok(())
        }
        fn size_bytes(&mut self) -> Result<u64> {
            Ok(self.0.len() as u64)
        }
    }

    /// **Repeat writes through the EL0 binary's exact chunking.** The device's `IpcDisk` and this
    /// `BlockDisk` split `Disk` calls the same way, so if the chunking is what breaks a repeat write
    /// (the partial-block read-modify-write is the suspicious part, and compression plus a growing
    /// allocator log exercise it more on later writes), it breaks here, on the host, in milliseconds.
    #[test]
    fn repeat_writes_through_the_block_chunking_do_not_loop() {
        let mut fs = FileSystem::create(BlockDisk(VecIo(vec![0u8; 16 * 1024 * 1024])), None, 0, 0)
            .expect("mkfs through BlockDisk");
        fs.tx(|tx| {
            let ptr = tx
                .create_node(TreePtr::root(), "scratch", Node::MODE_FILE | 0o644, 0, 0)?
                .ptr();
            tx.write_node(ptr, 0, b"(placeholder)", 0, 0)?;
            Ok(())
        })
        .expect("populate");
        let image = fs.disk.0.0;

        let mut srv = Server::open(BlockDisk(VecIo(image))).expect("Server::open");
        let h = srv.open_file("scratch").expect("open scratch");
        let mut buf = [0u8; 128];
        for pass in 1..=3u8 {
            let payload = repeat_write_payload(pass);
            assert_eq!(srv.write(h, 0, &payload).unwrap(), payload.len());
            let n = srv.read(h, 0, &mut buf).unwrap();
            assert_eq!(&buf[..n], &payload[..], "pass {pass} through the chunking");
        }
    }

    /// Repeat writes of a **record-sized** payload, so the chunking's multi-block path and the
    /// compressed-record partial tail both get exercised across generations. 160 KiB is larger than
    /// RedoxFS's 128 KiB record, so a write spans records and the tail is partial.
    #[test]
    fn repeat_record_sized_writes_through_the_chunking_do_not_loop() {
        let mut fs = FileSystem::create(BlockDisk(VecIo(vec![0u8; 32 * 1024 * 1024])), None, 0, 0)
            .expect("mkfs");
        fs.tx(|tx| {
            tx.create_node(TreePtr::root(), "big", Node::MODE_FILE | 0o644, 0, 0)?;
            Ok(())
        })
        .expect("populate");
        let image = fs.disk.0.0;

        let mut srv = Server::open(BlockDisk(VecIo(image))).expect("open");
        let h = srv.open_file("big").expect("open big");
        let len = 160 * 1024;
        for pass in 1..=3u8 {
            // Position-dependent and pass-dependent, and deliberately not compressible to a tiny
            // size, so the record path does real work each pass.
            let data: Vec<u8> = (0..len)
                .map(|i| ((i as u32).wrapping_mul(2_654_435_761) >> 16) as u8 ^ pass)
                .collect();
            assert_eq!(srv.write(h, 0, &data).unwrap(), len);
            let mut back = vec![0u8; len];
            let n = srv.read(h, 0, &mut back).unwrap();
            assert_eq!(n, len, "pass {pass} short read");
            assert!(back == data, "pass {pass} record data mismatch");
        }
    }

    /// Build a 16 MiB RedoxFS image in memory with the given files in its root (using the std
    /// creation APIs the host tool uses), then reopen it through a fresh [`Server`], exactly as the
    /// running system does: the host tool makes the image, the server only ever opens it.
    fn server_with(files: &[(&str, &[u8])]) -> Server<DiskMemory> {
        Server::open(image_with(files)).expect("open")
    }

    /// The disk behind [`server_with`], as a standalone [`DiskMemory`], so a test can open it, drop
    /// the server, and reopen the same disk to prove persistence across a close.
    fn image_with(files: &[(&str, &[u8])]) -> DiskMemory {
        let disk = DiskMemory::new(16 * 1024 * 1024);
        let mut fs = FileSystem::create(disk, None, 0, 0).expect("create");
        fs.tx(|tx| {
            for (name, data) in files {
                let ptr = tx
                    .create_node(TreePtr::root(), name, Node::MODE_FILE | 0o644, 0, 0)?
                    .ptr();
                tx.write_node(ptr, 0, data, 0, 0)?;
            }
            Ok(())
        })
        .expect("populate");
        fs.disk
    }

    #[test]
    fn opens_and_reads_a_file_the_host_tool_wrote() {
        let mut srv = server_with(&[("motd", b"hello from redoxfs\n")]);
        let h = srv.open_file("motd").expect("open motd");
        let mut buf = [0u8; 64];
        let n = srv.read(h, 0, &mut buf).expect("read");
        assert_eq!(&buf[..n], b"hello from redoxfs\n");
    }

    #[test]
    fn a_missing_name_is_enoent_and_names_do_not_walk() {
        let mut srv = server_with(&[("present", b"x")]);
        assert_eq!(
            srv.open_file("absent").unwrap_err().errno,
            syscall::error::ENOENT
        );
        // A name is one component resolved in the bound directory, never a path: a slash does not
        // descend, so "present/nope" finds nothing rather than walking anywhere.
        assert!(srv.open_file("present/nope").is_err());
    }

    #[test]
    fn a_write_persists_and_reads_back() {
        let mut srv = server_with(&[("scratch", &[0u8; 32])]);
        let h = srv.open_file("scratch").unwrap();
        let payload = b"CRKWRIT1 and then some position-dependent bytes here";
        let n = srv.write(h, 0, payload).unwrap();
        assert_eq!(n, payload.len());
        let mut buf = [0u8; 128];
        let m = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(&buf[..m], payload);
        assert_eq!(srv.fstat(h).unwrap(), payload.len() as u64);
    }

    /// The payload for pass `n` of the repeat-write test: a fixed 64 bytes, tagged with the pass and
    /// position-dependent after that, so every pass has the same length (no truncation confusion) and
    /// a stale or shifted read cannot match. Shared with the on-device client through
    /// `fs_proto::fixture` where that matters.
    fn repeat_write_payload(pass: u8) -> [u8; 64] {
        let mut p = [0u8; 64];
        p[..8].copy_from_slice(b"CRKRPT__");
        p[7] = b'0' + pass;
        for (i, b) in p.iter_mut().enumerate().skip(8) {
            *b = (i as u8).wrapping_mul(31) ^ pass;
        }
        p
    }

    /// **The repeat-write gate** (fix/redoxfs-repeat-write). A first write to a pristine block
    /// worked all along; a write to a block that has ALREADY been written is what loops on device.
    /// The old gate never saw it because `mkredoxfs` rewrites the target to a placeholder before
    /// every run, so every gated write was a first write and the bug hid behind the harness.
    ///
    /// This writes the same file TWICE in one run, which is the honest reproduction: it depends on
    /// nothing left over from a previous invocation. It is also the decisive host-vs-device
    /// comparison: if this loops, the bug is reachable with no cricker runtime at all (upstream or
    /// our chunking); if it passes, the divergence is ours, in the device I/O path.
    #[test]
    fn a_second_write_to_the_same_block_does_not_loop() {
        let mut srv = server_with(&[("scratch", b"(placeholder)")]);
        let h = srv.open_file("scratch").unwrap();

        // Equal-length, position-dependent payloads: each write fully replaces the last (a shorter
        // write at offset 0 does not truncate, which is correct filesystem behaviour but would muddy
        // the comparison), and a buffer that came back shifted or stale cannot match.
        let mut buf = [0u8; 128];
        for pass in 1..=3u8 {
            let payload = repeat_write_payload(pass);
            assert_eq!(
                srv.write(h, 0, &payload).unwrap(),
                payload.len(),
                "write {pass} was short"
            );
            let n = srv.read(h, 0, &mut buf).unwrap();
            assert_eq!(&buf[..n], &payload[..], "write {pass} did not read back");
        }
    }

    /// **The real trigger** (fix/redoxfs-repeat-write): write, drop the mount WITHOUT a clean
    /// unmount, reopen, and write again. Every existing test stopped short of this. The one that
    /// looked closest, `a_write_survives_a_full_close_and_reopen`, only READS after the reopen.
    ///
    /// This is exactly what the gate does across its two ISA legs: `mkredoxfs` runs once, the aarch64
    /// leg mounts and writes (a first write, which works), the process dies without unmounting, and
    /// then the riscv leg mounts the same image and writes again. That second boot's write is what
    /// fails, which is why the bug looked like "repeat write" and why one leg passing hid it.
    #[test]
    fn a_write_after_a_reopen_of_a_previously_written_image_does_not_loop() {
        let mut disk = image_with(&[("scratch", b"(placeholder)")]);

        // Boot 1: mount, write, and drop the mount the way a dying process does (no unmount).
        {
            let mut srv = Server::open(disk).expect("open 1");
            let h = srv.open_file("scratch").expect("open scratch 1");
            let p1 = repeat_write_payload(1);
            assert_eq!(srv.write(h, 0, &p1).unwrap(), p1.len());
            disk = srv.fs.disk; // take the disk back; no unmount, as on device
        }

        // Boot 2: mount the image boot 1 wrote, and write again.
        let mut srv = Server::open(disk).expect("open 2");
        let h = srv.open_file("scratch").expect("open scratch 2");
        let p2 = repeat_write_payload(2);
        assert_eq!(srv.write(h, 0, &p2).unwrap(), p2.len(), "boot-2 write");
        let mut buf = [0u8; 128];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], &p2[..], "boot-2 write did not read back");
    }

    /// **The mechanism behind the "cross-boot second-mount write failure", and it is not a filesystem
    /// bug at all** (fix/redoxfs-second-mount). There is no TRUNCATE verb, so a write shorter than the
    /// file leaves the previous write's tail in place. That is correct behaviour, and it is what the
    /// contract offers today, but it is sharp: a test that writes N bytes and then compares a
    /// *whole-file* read against those N bytes passes only if the file was not already longer.
    ///
    /// That is exactly what happened. One boot's client left a 64-byte payload in `scratch`; the next
    /// boot's `std::fs` test wrote its 61-byte pattern and asserted the whole file equalled it, got 64
    /// bytes back, and panicked in the write block. It looked like "a second mount of a used image
    /// fails its write", and three rounds chased a filesystem bug that was never there. The write
    /// succeeded every time.
    ///
    /// The byte counts here are the real ones so the arithmetic is legible rather than abstract.
    #[test]
    fn a_shorter_write_does_not_truncate_and_that_is_what_broke_across_boots() {
        let mut srv = server_with(&[("scratch", b"(placeholder)")]);
        let h = srv.open_file("scratch").unwrap();

        // Boot 1's client wrote 64 bytes.
        let long = [b'L'; 64];
        assert_eq!(srv.write(h, 0, &long).unwrap(), 64);
        assert_eq!(srv.fstat(h).unwrap(), 64);

        // Boot 2's std::fs wrote its 61-byte pattern over it. The write itself is fine.
        let short = [b'S'; 61];
        assert_eq!(srv.write(h, 0, &short).unwrap(), 61);

        // And the file is STILL 64 bytes, because nothing truncates it.
        assert_eq!(
            srv.fstat(h).unwrap(),
            64,
            "a shorter write must not truncate; TRUNCATE is the verb that fixes this, not WRITE"
        );
        let mut buf = [0u8; 128];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(n, 64, "a whole-file read returns the OLD length");
        assert_eq!(&buf[..61], &short[..], "the new bytes landed");
        assert_eq!(
            &buf[61..64],
            b"LLL",
            "and the longer write's tail survives, which is what failed the comparison"
        );
    }

    #[test]
    fn a_write_survives_a_full_close_and_reopen() {
        // Persistence across a full close/reopen is what the on-disk image buys, and it is the
        // property the kill-mid-write test leans on: a reopen sees the committed write.
        let disk = image_with(&[("f", b"")]);
        let mut srv = Server::open(disk).unwrap();
        let h = srv.open_file("f").unwrap();
        srv.write(h, 0, b"persisted").unwrap();
        let disk = srv.fs.disk; // the FileSystem committed on each tx; take the disk back
        let mut srv = Server::open(disk).unwrap();
        let h = srv.open_file("f").unwrap();
        let mut buf = [0u8; 16];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"persisted");
    }

    #[test]
    fn a_forged_or_closed_handle_is_ebadf() {
        let mut srv = server_with(&[("f", b"data")]);
        let h = srv.open_file("f").unwrap();
        let mut buf = [0u8; 8];
        assert_eq!(srv.read(999, 0, &mut buf).unwrap_err().errno, EBADF);
        srv.close(h).unwrap();
        assert_eq!(srv.read(h, 0, &mut buf).unwrap_err().errno, EBADF);
        assert_eq!(srv.close(h).unwrap_err().errno, EBADF);
    }

    #[test]
    fn handles_are_reused_after_close() {
        let mut srv = server_with(&[("a", b"1"), ("b", b"2")]);
        let h0 = srv.open_file("a").unwrap();
        srv.close(h0).unwrap();
        let h1 = srv.open_file("b").unwrap();
        assert_eq!(h0, h1, "a freed slot is reused before growing the table");
    }
    // --- CREATE and TRUNCATE (milestone 31 phase 2) ---

    #[test]
    fn create_makes_an_empty_file_and_refuses_an_existing_name() {
        let mut srv = server_with(&[("already", b"here")]);

        let h = srv.create_file("fresh").unwrap();
        assert_eq!(srv.fstat(h).unwrap(), 0, "a created file starts empty");

        // Create is create, not create-or-open. Silently opening an existing file is how a
        // partly-working write comes to look like a working one (the §27 failure mode).
        assert_eq!(
            srv.create_file("already").err().map(|e| e.errno),
            Some(EEXIST),
            "creating over an existing name must refuse, and must not truncate it either",
        );
        let h2 = srv.open_file("already").unwrap();
        assert_eq!(
            srv.fstat(h2).unwrap(),
            4,
            "the refused create left the file alone"
        );

        // And the refusal did not half-create anything: the name still resolves to the old file.
        let mut buf = [0u8; 8];
        assert_eq!(srv.read(h2, 0, &mut buf).unwrap(), 4);
        assert_eq!(&buf[..4], b"here");
    }

    #[test]
    fn truncate_shrinks_and_discards_the_tail_which_is_the_bug_it_exists_to_fix() {
        // The exact byte counts from DECISIONS §27: a 64-byte payload from one boot, a 61-byte
        // write from the next, and the three-byte tail that made a good write look like a failure.
        let mut srv = server_with(&[("scratch", b"(placeholder)")]);
        let h = srv.open_file("scratch").unwrap();
        assert_eq!(srv.write(h, 0, &[b'L'; 64]).unwrap(), 64);

        let short = [b'S'; 61];
        assert_eq!(srv.write(h, 0, &short).unwrap(), 61);
        srv.truncate(h, short.len() as u64).unwrap();

        assert_eq!(srv.fstat(h).unwrap(), 61, "truncate sets the size exactly");
        let mut buf = [0u8; 128];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(n, 61, "and a whole-file read returns only what was written");
        assert_eq!(&buf[..61], &short[..], "with the new bytes intact");
    }

    #[test]
    fn truncate_grows_with_zeroes_and_is_idempotent() {
        let mut srv = server_with(&[("f", b"abc")]);
        let h = srv.open_file("f").unwrap();

        srv.truncate(h, 8).unwrap();
        assert_eq!(srv.fstat(h).unwrap(), 8);
        let mut buf = [0xffu8; 16];
        assert_eq!(srv.read(h, 0, &mut buf).unwrap(), 8);
        assert_eq!(&buf[..3], b"abc", "the original bytes survive a grow");
        assert_eq!(
            &buf[3..8],
            &[0, 0, 0, 0, 0],
            "and the extension reads as zeroes"
        );

        // Truncating to the current size changes nothing, which matters because std::fs::write
        // opens-creates-truncates unconditionally and would otherwise churn the tree every call.
        srv.truncate(h, 8).unwrap();
        assert_eq!(srv.fstat(h).unwrap(), 8);

        srv.truncate(h, 0).unwrap();
        assert_eq!(
            srv.fstat(h).unwrap(),
            0,
            "and shrinking to empty is allowed"
        );
        assert_eq!(srv.read(h, 0, &mut buf).unwrap(), 0);
    }

    #[test]
    fn create_write_truncate_all_survive_a_close_and_reopen() {
        // The half a cache cannot fake: drop the mount entirely and reopen the same disk. The disk is
        // taken back out of the server rather than cloned, which is also how a dying process leaves
        // it: no unmount, exactly as the cross-boot test does.
        let mut disk = image_with(&[]);
        let payload = b"created, written, and truncated to exactly this";

        {
            let mut srv = Server::open(disk).expect("open");
            let h = srv.create_file("made").unwrap();
            assert_eq!(srv.write(h, 0, payload).unwrap(), payload.len());
            assert_eq!(srv.write(h, 0, &[b'X'; 200]).unwrap(), 200);
            assert_eq!(srv.write(h, 0, payload).unwrap(), payload.len());
            srv.truncate(h, payload.len() as u64).unwrap();
            disk = srv.fs.disk;
        }

        let mut srv = Server::open(disk).expect("reopen");
        let h = srv
            .open_file("made")
            .expect("the created name survived the reopen");
        assert_eq!(srv.fstat(h).unwrap(), payload.len() as u64);
        let mut buf = [0u8; 512];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(
            &buf[..n],
            payload,
            "the truncated contents are what persisted"
        );
    }

    #[test]
    fn create_and_truncate_refuse_what_every_other_verb_refuses() {
        let mut srv = server_with(&[("f", b"x")]);

        // A forged handle is EBADF in exactly one place, and the new verb goes through it too.
        assert_eq!(
            srv.truncate(99, 0).err().map(|e| e.errno),
            Some(EBADF),
            "truncate must validate the handle like every other operation",
        );

        // A name is a single component, not a path, and both verbs enforce it. RedoxFS does NOT:
        // its check_name passes `/`, `.` and `..` through, so before this check create_file("../escape")
        // happily made an entry literally named `../escape`. Not a traversal, because nothing walks
        // paths, and a landmine for the first thing that does.
        for bad in [
            "../escape",
            "sub/deep",
            "/absolute",
            ".",
            "..",
            "",
            "with:colon",
            "back\\slash",
        ] {
            assert_eq!(
                srv.create_file(bad).err().map(|e| e.errno),
                Some(EINVAL),
                "create must refuse {bad:?} as EINVAL: a name is not a path",
            );
            assert_eq!(
                srv.open_file(bad).err().map(|e| e.errno),
                Some(EINVAL),
                "and open must refuse {bad:?} the same way, not fall through to ENOENT",
            );
        }
    }
    /// **What `std::fs::write` actually does, twice, with a shorter payload the second time.**
    ///
    /// Written to chase a livelock: on device this sequence tripped the 90 s per-test ceiling with two
    /// userspace threads spinning. If the loop is in the filesystem it shows up here in milliseconds;
    /// if this passes, the fault is above the core (the wire, the PAL, or the client's own loops) and
    /// that is worth knowing before reading any of them.
    ///
    /// `std::fs::write` is `create(true).truncate(true)`, so each call is: open, or create if absent;
    /// truncate to 0; write. The second call is the interesting one, because it truncates a file that
    /// already has bytes and then writes fewer than it had.
    #[test]
    fn the_std_fs_write_sequence_twice_does_not_loop() {
        let mut srv = server_with(&[]);
        let long = b"the first write, deliberately the longer of the two";
        let short = b"the second write, shorter";
        assert!(short.len() < long.len());

        // Call one: the name is absent, so create, then truncate an already-empty file, then write.
        let h = srv.create_file("made-by-std").unwrap();
        srv.truncate(h, 0).unwrap();
        assert_eq!(srv.write(h, 0, long).unwrap(), long.len());
        srv.close(h).unwrap();

        // Call two: the name exists, so open, truncate away 51 bytes, then write 25.
        let h = srv.open_file("made-by-std").unwrap();
        srv.truncate(h, 0).unwrap();
        assert_eq!(
            srv.fstat(h).unwrap(),
            0,
            "truncate to 0 must empty the file"
        );
        assert_eq!(srv.write(h, 0, short).unwrap(), short.len());
        assert_eq!(srv.fstat(h).unwrap(), short.len() as u64);
        srv.close(h).unwrap();

        // And the read-back `std::fs::read` does: open, fstat for the size hint, read to EOF. The
        // read loop must terminate, which means a read at or past the size returns 0.
        let h = srv.open_file("made-by-std").unwrap();
        let size = srv.fstat(h).unwrap() as usize;
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        let mut off = 0u64;
        // Bounded so a livelock fails the test instead of hanging it: EOF must arrive well inside this.
        for _ in 0..16 {
            let n = srv.read(h, off, &mut buf).unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
            off += n as u64;
        }
        assert_eq!(
            got.len(),
            size,
            "the read loop did not reach EOF in 16 reads"
        );
        assert_eq!(
            &got[..],
            short,
            "the shorter write did not replace the contents"
        );
        assert_eq!(
            srv.read(h, off, &mut buf).unwrap(),
            0,
            "a read at EOF must return 0, or every read_to_end spins",
        );
    }
    /// **Create into a directory the image already populated**, which is what the device does and what
    /// the empty-root tests above do not.
    ///
    /// The device image is built by the host tool with `motd` and `scratch` already in the root, and
    /// `std::fs::write` then adds a third name. On device that hangs: three FS-server instances were
    /// caught spinning, one in `create_node` and two in `find_node_inner`, i.e. in the DIRECTORY code
    /// rather than the allocator. Every earlier host test created into an *empty* root, so none of
    /// them shaped the directory the way the real image does. If the fault is directory-shape
    /// dependent it belongs here, in milliseconds.
    #[test]
    fn create_into_a_populated_root_and_look_it_up() {
        // The same names and contents the shipped image carries, so the root's shape matches.
        let mut srv = server_with(&[
            (
                "motd",
                b"redoxfs served this file to an EL0 client through a capability handle\n",
            ),
            (
                "scratch",
                b"(placeholder overwritten by the fs-server write test)",
            ),
        ]);

        let h = srv.create_file("made-by-std").unwrap();
        assert_eq!(srv.write(h, 0, b"first").unwrap(), 5);
        srv.close(h).unwrap();

        // The lookups that spin on device: the new name, and crucially the pre-existing ones, since a
        // corrupted directory would take every scan down with it, not just the new entry's.
        for name in ["made-by-std", "motd", "scratch"] {
            let h = srv
                .open_file(name)
                .unwrap_or_else(|e| panic!("{name} no longer resolves: {e:?}"));
            srv.close(h).unwrap();
        }
        assert!(
            srv.open_file("definitely-absent").is_err(),
            "a miss must still terminate and report, not scan forever",
        );
    }
    /// **CREATE through the EL0 binary's exact block chunking**, which is the one path none of the
    /// other create tests take.
    ///
    /// Every test above drives `DiskMemory`, whose `write_at` writes exactly the buffer it is given.
    /// The device drives `IpcDisk` behind [`BlockDisk`], which splits every call into whole-block
    /// transfers and READ-MODIFY-WRITES a short final chunk. `create_node` writes a fresh node block
    /// and extends a directory, so it is exactly the shape that would expose a chunking fault, and on
    /// device it hangs with FS servers spinning in `create_node` and `find_node_inner`.
    ///
    /// Bounded on purpose: the point is that this terminates, so a livelock fails the run rather than
    /// wedging the suite.
    #[test]
    fn create_through_the_block_chunking_does_not_loop() {
        let mut fs = FileSystem::create(BlockDisk(VecIo(vec![0u8; 16 * 1024 * 1024])), None, 0, 0)
            .expect("mkfs through BlockDisk");
        // The root the shipped image has when std::fs::write adds a third name to it.
        fs.tx(|tx| {
            for (name, body) in [
                (
                    "motd",
                    &b"redoxfs served this file to an EL0 client through a capability handle\n"[..],
                ),
                (
                    "scratch",
                    &b"(placeholder overwritten by the fs-server write test)"[..],
                ),
            ] {
                let ptr = tx
                    .create_node(TreePtr::root(), name, Node::MODE_FILE | 0o644, 0, 0)?
                    .ptr();
                tx.write_node(ptr, 0, body, 0, 0)?;
            }
            Ok(())
        })
        .expect("populate");
        let image = fs.disk.0.0;

        let mut srv = Server::open(BlockDisk(VecIo(image))).expect("Server::open");

        // What std::fs::write does: create, truncate, write; then again, shorter.
        let h = srv
            .create_file("made-by-std")
            .expect("create through chunking");
        srv.truncate(h, 0).unwrap();
        let long = b"the first write, deliberately the longer of the two";
        assert_eq!(srv.write(h, 0, long).unwrap(), long.len());
        srv.close(h).unwrap();

        let h = srv
            .open_file("made-by-std")
            .expect("reopen through chunking");
        srv.truncate(h, 0).unwrap();
        let short = b"the second write, shorter";
        assert_eq!(srv.write(h, 0, short).unwrap(), short.len());
        srv.close(h).unwrap();

        // The lookups that spin on device, including the pre-existing names: a directory damaged by
        // the create would take every scan down with it, not only the new entry's.
        for name in ["made-by-std", "motd", "scratch"] {
            let h = srv.open_file(name).unwrap_or_else(|e| {
                panic!("{name} no longer resolves after a chunked create: {e:?}")
            });
            srv.close(h).unwrap();
        }

        let h = srv.open_file("made-by-std").unwrap();
        let mut buf = [0u8; 256];
        let n = srv.read(h, 0, &mut buf).unwrap();
        assert_eq!(
            &buf[..n],
            short,
            "the chunked create/truncate/write round trip lost bytes"
        );
    }
}
