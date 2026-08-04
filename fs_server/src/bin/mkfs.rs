//! **`mkfs`**: `mkfs` on the target (milestone 57's write half, notes/crickerfs.md).
//!
//! The program that creates a filesystem, and the whole of its authority is two capabilities:
//!
//! - a **block-service endpoint** for **one** disk (`fs_proto::blk`), which is the power to destroy
//!   that disk and nothing else; and
//! - an **entropy endpoint** (`entropy_proto`), which is the power to obtain random bytes and
//!   nothing else. It cannot reach the virtio-rng device, program its queue, or see the DMA page.
//!
//! **That pair is sufficient and neither half is**, which is what this program exists to
//! demonstrate. A filesystem carries a unique id, and an id that is not random is not unique; the
//! vendored RedoxFS has no source of randomness in a `no_std` build, which is exactly why
//! `Header::new` is `std`-gated (it calls `getrandom` through `Uuid::new_v4`). Rather than give the
//! engine a random number generator, the engine now **takes the uuid as an argument**, the same way
//! `create` already takes `ctime` because it has no clock. See `vendor/README.md` divergence 4 and
//! `patches/redoxfs-no-std-create-uuid.patch`.
//!
//! So the capability boundary is where it should be: **the library computes, the program holds the
//! authority, and no randomness enters vendored code.**
//!
//! Compare `mkfs.ext4 /dev/sda3` as root, which reaches every disk in the machine and opens
//! `/dev/urandom` without asking anybody. Here a process with the disk and no entropy writes
//! nothing at all, and a process with entropy and no disk has nothing to write to. The kernel-side
//! test runs all three cases against the same binary.
//!
//! # What it does
//!
//! 1. Reads the partition table off the disk it holds and finds the **cricker-os data partition**
//!    by type GUID (DECISIONS §45). Nothing here trusts a partition *name*: on any disk a Mac
//!    touched there is not one (notes/gpt.md).
//! 2. Draws sixteen random bytes, and stops if it cannot.
//! 3. Creates the filesystem **inside that partition**, through a [`PartitionDisk`] whose block
//!    zero is the partition's first block and whose `size` is the partition's length, so the engine
//!    cannot address a byte outside it even by arithmetic error.
//! 4. Reopens it through [`fs_server::Server`] (the same code the FS server serves with) and writes
//!    one file, so that what lands on the platter is a filesystem with a tree in it rather than
//!    only a header.
//!
//! The order matters: **the entropy draw comes before the first write**, so a refusal leaves a
//! partition that still holds whatever it held.
//!
//! # EXAMPLES
//!
//! ```text
//!   wired with a disk AND an entropy endpoint:
//!     report: R_MADE, 12288 blocks, ctime 0
//!
//!   wired with the disk and NO entropy endpoint:
//!     report: R_NO_ENTROPY                      (and the partition is untouched)
//!
//!   wired with entropy and NO disk:
//!     report: R_NO_DISK                         (there is nothing to write to)
//! ```
//!
//! # BUGS
//!
//! - **Every timestamp on the new filesystem is zero.** `create_reserved_with_uuid` takes `ctime`
//!   because the engine has no clock, and this program holds no clock capability either: it has a
//!   disk and an entropy endpoint, and nothing else. The honest thing is to pass zero rather than
//!   invent a plausible number, so a filesystem made on the target is dated 1970 until the wiring
//!   grows a `clock_proto` endpoint. A one-slot change, deliberately not taken here, because adding
//!   it would blur the two-capability claim this program is making.
//! - **It refuses a partition that is not 4096-aligned at both ends**, rather than rounding one in.
//!   A filesystem whose blocks straddle the partition's first byte reads back correctly through the
//!   offset disk that made it and is unreadable by everything else, which is the worst failure
//!   available.
//! - **No progress, no resume, and no confirmation.** It formats what it was handed the moment it
//!   runs. The confirmation a `mkfs` normally prints exists because the tool could have been aimed
//!   anywhere; this one was handed one partition of one disk.
//! - **The filesystem is unencrypted**, deliberately (roadmap milestone 57). RedoxFS encryption
//!   needs a salt and two keys, which are `getrandom` upstream; the `no_std` creation path refuses
//!   a password rather than silently making an unencrypted filesystem, so wiring one in later is a
//!   second entropy draw and not a surprise.

#![no_std]
#![no_main]

extern crate alloc;

use entropy_proto as entropy;
use fs_proto::fixture::blank;
use fs_proto::{blk, req};
use fs_server::Server;
use gpt::Gpt;
use gpt::guid::types;
use redoxfs::{BLOCK_SIZE, Disk, FileSystem};
use syscall::error::{EIO, Error, Result};
use user_rt::{call, send};

/// Cspace slots, by convention with `kernel/src/user/disk_service.rs`.
const UNTYPED: u64 = 0;
/// The block service for the one disk. **Empty in the no-disk control.**
const BLK: u64 = 1;
/// The entropy service. **Empty in the no-entropy control.**
const ENTROPY: u64 = 2;
/// Where the verdict goes.
const REPORT: u64 = 3;

/// Where the kernel maps the page shared with the block server. Must match
/// `kernel/src/user/disk_service.rs`.
const BLK_PAGE: u64 = 0x5000_0000;

/// One filesystem block / one shared page, in bytes. RedoxFS's `BLOCK_SIZE` and the blk wire's
/// transfer unit are the same number, which is what lets a `Disk` block be a blk block.
const BLOCK: usize = blk::BLOCK_SIZE;

/// The logical block size the partition table counts in (`disk_surveyor`'s BUGS: nothing on the
/// wire reports the device's).
const LBA: u64 = blank::LBA;

/// The heap cap. The engine keeps a record-sized compress buffer (128 KiB) plus block buffers and
/// small tree structures; creation touches fewer of them than a mount does. The untyped the kernel
/// grants is the real ceiling.
const HEAP_MAX: u64 = 8 * 1024 * 1024;

#[global_allocator]
static HEAP: user_rt::heap::UntypedHeap = user_rt::heap::UntypedHeap::new();

// The roles, in `a0`. Must match kernel/src/user/disk_service.rs.
/// Create a filesystem in the cricker-os data partition.
const ROLE_MAKE: u64 = 0;
/// **Read the partition back and say whether there is a filesystem in it.** A process with a disk
/// and no entropy, so it cannot create one however it is spawned, which is what makes it usable
/// both before and after a run that was supposed to create nothing.
const ROLE_CHECK: u64 = 1;

// The report's first word. Must match kernel/src/user/disk_tests.rs.
/// A filesystem was created and a file written into it. ASCII `MKFSD`, so a hex dump reads.
pub const R_MADE: u64 = 0x_4D_4B_46_53_44;
/// **No entropy endpoint, so nothing was created.** A filesystem id that is not unique is a
/// filesystem two disks can claim to be; refusing beats inventing one (DECISIONS §42).
pub const R_NO_ENTROPY: u64 = 0x_4E_4F_52_4E_47;
/// **No block-service endpoint**, so there is no disk to read a table off or write a filesystem to.
pub const R_NO_DISK: u64 = 0x_4E_4F_44_53_4B;
/// The disk answered, but it carries no cricker-os data partition to format.
pub const R_NO_PARTITION: u64 = 0x_4E_4F_50_54_4E;
/// The partition is there and the engine refused to make a filesystem in it. The second word is a
/// step number, the third the engine's errno.
pub const R_FAILED: u64 = 0x_4D_4B_45_52_52;

// [`ROLE_CHECK`]'s verdicts.
/// The filesystem opened and holds the file this program writes, with the right bytes in it.
pub const R_FOUND: u64 = 0x_46_4F_55_4E_44;
/// **The partition holds no filesystem at all.** What a partition that has only been *described*
/// looks like, and what a refused run must leave behind.
pub const R_NO_FS: u64 = 0x_4E_4F_46_53_00;
/// A filesystem is there and the file is not, or its bytes are wrong.
pub const R_NO_FILE: u64 = 0x_4E_4F_46_4C_45;

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, _a1: u64, _a2: u64) -> ! {
    HEAP.init(UNTYPED, user_rt::heap::DEFAULT_BASE, HEAP_MAX);

    // The disk first, because "I was given no disk" and "I was given no entropy" must be
    // distinguishable, and the cheapest question decides it.
    let Some((first_block, blocks)) = data_partition() else {
        send(REPORT, R_NO_DISK, 0, 0);
        user_rt::exit()
    };
    if blocks == 0 {
        send(REPORT, R_NO_PARTITION, 0, 0);
        user_rt::exit()
    }

    if role == ROLE_CHECK {
        check(first_block, blocks)
    }
    debug_assert!(role == ROLE_MAKE);

    // **Before the first write.** A program that cannot be given randomness must find out while the
    // partition still holds whatever it held.
    let Some(uuid) = random16() else {
        send(REPORT, R_NO_ENTROPY, 0, 0);
        user_rt::exit()
    };

    let disk = PartitionDisk {
        first_block,
        blocks,
    };
    // ctime zero: this program holds no clock. See BUGS.
    match FileSystem::create_reserved_with_uuid(disk, None, &[], 0, 0, uuid) {
        Ok(fs) => drop(fs),
        Err(e) => {
            send(REPORT, R_FAILED, 1, e.errno as u64);
            user_rt::exit()
        }
    }

    // Reopen through the FS server's own core and put a file in it. A header that parses proves
    // less than a tree that holds a name: the host tool reads this file back after the run.
    let mut server = match Server::open(PartitionDisk {
        first_block,
        blocks,
    }) {
        Ok(s) => s,
        Err(e) => {
            send(REPORT, R_FAILED, 2, e.errno as u64);
            user_rt::exit()
        }
    };
    let made = server
        .create_file_at(fs_proto::fs::ROOT as u32, blank::MADE_NAME)
        .and_then(|h| server.write(h, 0, blank::MADE_BODY).map(|n| (h, n)));
    match made {
        Ok((h, n)) if n == blank::MADE_BODY.len() => {
            let _ = server.close(h);
        }
        Ok((_, n)) => {
            send(REPORT, R_FAILED, 3, n as u64);
            user_rt::exit()
        }
        Err(e) => {
            send(REPORT, R_FAILED, 4, e.errno as u64);
            user_rt::exit()
        }
    }
    drop(server);

    send(REPORT, R_MADE, blocks, first_block);
    user_rt::exit()
}

/// **Is there a filesystem in the partition, and does it hold what a creation run puts there?**
///
/// Never creates and never writes: it opens read-only in the sense that matters here (a `Server`
/// mount does tidy allocations, which is why this runs against the disk and not against a fixture
/// somebody else needs). It is the reading that turns "the refused run reported R_NO_ENTROPY" into
/// "the refused run left an unformatted partition".
fn check(first_block: u64, blocks: u64) -> ! {
    // **The signature first, because the alternative is 12,288 IPC round trips.** `FileSystem::open`
    // scans the whole disk for a valid header (it has no way to know how big the ring is on a disk
    // it has not opened yet), and on an unformatted partition that scan runs until a read fails at
    // the end. Block 0 always holds generation zero's header, so its signature is a one-read answer
    // to "is there a filesystem here at all", and it is the question this role is asked twice.
    if blk_call(blk::READ, first_block) < 0 {
        send(REPORT, R_NO_DISK, 0, 0);
        user_rt::exit()
    }
    // SAFETY: BLK_PAGE is a mapped page of BLOCK bytes holding the block just read.
    let signature = unsafe { core::slice::from_raw_parts(BLK_PAGE as *const u8, 8) };
    if signature != redoxfs::SIGNATURE {
        send(REPORT, R_NO_FS, 0, 0);
        user_rt::exit()
    }

    let mut server = match Server::open(PartitionDisk {
        first_block,
        blocks,
    }) {
        Ok(s) => s,
        // The engine says ENOENT for a partition with no valid header anywhere in the ring, which
        // is what an unformatted one is.
        Err(_) => {
            send(REPORT, R_NO_FS, 0, 0);
            user_rt::exit()
        }
    };
    let mut buf = [0u8; 128];
    let read = server
        .open_file_at(fs_proto::fs::ROOT as u32, blank::MADE_NAME)
        .and_then(|h| server.read(h, 0, &mut buf[..blank::MADE_BODY.len()]));
    match read {
        Ok(n) if n == blank::MADE_BODY.len() && buf[..n] == *blank::MADE_BODY => {
            send(REPORT, R_FOUND, n as u64, 0)
        }
        _ => send(REPORT, R_NO_FILE, 0, 0),
    };
    user_rt::exit()
}

/// Find the cricker-os data partition on the disk this process holds: `(first block, block count)`
/// in RedoxFS blocks. `None` when there is no disk at all; `Some((_, 0))` when there is a disk and
/// no such partition.
///
/// The table is read with `crates/gpt`, the same parser `disk_surveyor` uses and the same one whose
/// host tests run against tables `sgdisk` and macOS `diskutil` wrote. **The partition is found by
/// type GUID**, never by name: macOS writes no GPT partition names at all (notes/gpt.md).
fn data_partition() -> Option<(u64, u64)> {
    let size = blk_call(blk::SIZE, 0);
    if size <= 0 {
        return None; // no endpoint in slot BLK, or a device that cannot say how big it is
    }
    let device_blocks = (size as u64) / LBA;

    // LBA 0..=33: the protective MBR, the header, and the whole 16 KiB entry array. Five transfer
    // blocks cover it. On the heap rather than in `.bss` because this program has one.
    let mut head = alloc::vec![0u8; 5 * BLOCK];
    for i in 0..5u64 {
        if blk_call(blk::READ, i) < 0 {
            return None;
        }
        // SAFETY: BLK_PAGE is a mapped page of exactly BLOCK bytes; the window is in range by
        // construction (five blocks into a five-block buffer).
        unsafe {
            core::ptr::copy_nonoverlapping(
                BLK_PAGE as *const u8,
                head.as_mut_ptr().add(i as usize * BLOCK),
                BLOCK,
            );
        }
    }

    let table = Gpt::parse(
        &head[LBA as usize..2 * LBA as usize],
        &head[2 * LBA as usize..],
    )
    .ok()?;
    for (_, part) in table.partitions() {
        if part.type_guid != types::CRICKER_DATA {
            continue;
        }
        let start = part.first_lba * LBA;
        let len = part.blocks()? * LBA;
        // Both ends on a filesystem block, or refuse. See BUGS: a filesystem whose blocks straddle
        // the partition's first byte is readable only through the offset disk that wrote it.
        if !start.is_multiple_of(BLOCK_SIZE) || !len.is_multiple_of(BLOCK_SIZE) {
            return Some((0, 0));
        }
        let first_block = start / BLOCK_SIZE;
        let blocks = len / BLOCK_SIZE;
        // And inside the device the block service is actually serving.
        if first_block + blocks > device_blocks * LBA / BLOCK_SIZE {
            return Some((0, 0));
        }
        return Some((first_block, blocks));
    }
    Some((0, 0))
}

/// Sixteen random bytes from the entropy service, or `None` if this process holds no entropy
/// endpoint (or the service has none to give).
///
/// Two round trips, because one reply carries one word. `entropy_proto::delivered` is what tells a
/// missing capability apart from a short answer: a count is always `0..=8` and every kernel error is
/// a small negative that reads as an enormous `u64`.
fn random16() -> Option<[u8; 16]> {
    let mut out = [0u8; 16];
    for half in 0..2 {
        let (r0, r1) = call(ENTROPY, entropy::req(entropy::GET, entropy::MAX_BYTES), 0);
        let n = entropy::delivered(r0)?;
        if n != entropy::MAX_BYTES as usize {
            return None;
        }
        entropy::take(n, r1, &mut out[half * 8..half * 8 + 8]);
    }
    Some(out)
}

/// One blk `CALL`: the opcode and a block index, with the bulk in [`BLK_PAGE`]. Negative is an
/// error, and "there is no endpoint in that slot" is one of them.
fn blk_call(op_code: u64, block: u64) -> i64 {
    let (r0, _) = call(BLK, req(op_code), block);
    r0 as i64
}

/// **The RedoxFS `Disk` for one partition**: block zero is the partition's first block and `size` is
/// the partition's length, so every address the engine computes is inside the partition by
/// construction rather than by the engine's good behaviour.
///
/// This is the FS server's `IpcDisk` plus an origin and a length, and it is deliberately its own
/// copy rather than a shared module: the two differ in what they enforce (this one bounds; that one
/// carries milestone 37's crash injector), and there is one consumer of each.
struct PartitionDisk {
    /// The partition's first block, in RedoxFS blocks, from the start of the device.
    first_block: u64,
    /// How many blocks the partition holds.
    blocks: u64,
}

impl PartitionDisk {
    /// The device block a filesystem block lands on, or `None` if it is past the partition's end.
    fn device_block(&self, block: u64) -> Option<u64> {
        (block < self.blocks).then_some(self.first_block + block)
    }
}

impl Disk for PartitionDisk {
    unsafe fn read_at(&mut self, block: u64, buffer: &mut [u8]) -> Result<usize> {
        for (i, chunk) in buffer.chunks_mut(BLOCK).enumerate() {
            let Some(at) = self.device_block(block + i as u64) else {
                return Err(Error::new(EIO));
            };
            if blk_call(blk::READ, at) < 0 {
                return Err(Error::new(EIO));
            }
            // SAFETY: BLK_PAGE is a mapped page of exactly BLOCK bytes and `chunk` is no larger.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    BLK_PAGE as *const u8,
                    chunk.as_mut_ptr(),
                    chunk.len(),
                )
            };
        }
        Ok(buffer.len())
    }

    unsafe fn write_at(&mut self, block: u64, buffer: &[u8]) -> Result<usize> {
        for (i, chunk) in buffer.chunks(BLOCK).enumerate() {
            let Some(at) = self.device_block(block + i as u64) else {
                return Err(Error::new(EIO));
            };
            if chunk.len() < BLOCK {
                // A partial final block would clobber the rest of the block; read-modify-write so
                // only the given bytes change. The same courtesy `IpcDisk` pays, for the same
                // reason: a `Disk` owes it even if this engine never asks.
                if blk_call(blk::READ, at) < 0 {
                    return Err(Error::new(EIO));
                }
            }
            // SAFETY: as above; `chunk` is no larger than the page.
            unsafe {
                core::ptr::copy_nonoverlapping(chunk.as_ptr(), BLK_PAGE as *mut u8, chunk.len())
            };
            if blk_call(blk::WRITE, at) < 0 {
                return Err(Error::new(EIO));
            }
        }
        Ok(buffer.len())
    }

    fn size(&mut self) -> Result<u64> {
        // **The partition's size, not the device's.** This is the line that keeps the engine inside
        // the partition: it sizes its allocator from this number.
        Ok(self.blocks * BLOCK_SIZE)
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `brk` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem));
    };
    #[cfg(target_arch = "riscv64")]
    // SAFETY: `ebreak` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
