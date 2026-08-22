//! **`disk_surveyor`**: what drives are attached, and how the one we hold is divided up
//! (milestone 57, notes/block-devices.md).
//!
//! A surveyor maps land they do not own, and that is exactly the shape of this program's
//! endowment. It holds two things that look like one thing on every other operating system:
//!
//! - a **`READ`-only capability on the roster page**, which says what block devices exist
//!   (`block_roster`); and
//! - a **block-service endpoint**, which lets it read and write the blocks of **one** device
//!   (`fs_proto::blk`).
//!
//! Since milestone 108 the roster is a `Frame` this program holds and maps itself, rather than a
//! page the kernel wired into its address space before it started. The practical difference is at
//! the top of [`probe`]: `READ` on the capability means a writable mapping cannot be *obtained*,
//! where a read-only mapping only meant a write could not *land*.
//!
//! On Linux those are the same authority in practice: `/sys/block` is world-readable, `/dev/sda` is
//! a path, and a tool that can see a disk can usually be pointed at it. `parted /dev/sda` as root
//! reaches every disk in the machine, which is why calef's own router instructions carry a "confirm
//! the target device path before proceeding" warning: the tool cannot enforce what the warning
//! asks. **Here the warning is structural.** This program was handed one disk. There is no path to
//! type, no second device to typo into, and nothing in the roster that can be turned into a handle.
//!
//! # What it does
//!
//! Reads LBA 1 of the disk it holds and parses the GUID Partition Table there, then checks the
//! backup table at the far end of the disk against it, then reports. `crates/gpt` does every byte
//! of the judging; this file is the I/O and nothing else, which is why the parser is host-tested
//! against tables written by `sgdisk` and macOS `diskutil` and this program is tested by being run.
//!
//! Reading the table is the part that is not optional. A block device hands you an undifferentiated
//! run of blocks, and **which of them is a filesystem is written in the table and nowhere else**, so
//! a machine that cannot read a GPT cannot find a partition on a disk it did not create.
//!
//! # The two roles
//!
//! | `a0` | |
//! |---|---|
//! | [`ROLE_SURVEY`] | enumerate, read the table, report |
//! | [`ROLE_PROBE`] | announce the roster's address, write to it, and die |
//!
//! The second is the negative control, and it is a claim rather than a formality: **the roster is a
//! listing, not a lever.** The page is mapped read-only, so a program that knows exactly where it
//! is still cannot add a device to it, remove one, or point itself at a disk it was not given. The
//! kernel-side test watches the fault land at that address.
//!
//! # EXAMPLES
//!
//! ```text
//!   ROLE_SURVEY on the milestone-57 test disk (a 64 MiB image sgdisk partitioned):
//!     roster : 4 devices, 3 virtio-mmio, 1 virtio-pci
//!     table  : 3 partitions, disk GUID 1A2B3C4D-5E6F-4718-9A0B-C1D2E3F40506
//!              1  EFI system    2048..10239
//!              2  linux root   10240..30719
//!              3  nife data 30720..131038
//!     backup : agrees with the primary
//! ```
//!
//! # BUGS
//!
//! - **The logical block size is assumed to be 512.** Every disk this project has met reports 512,
//!   `crates/gpt` handles 4096 and is tested at it, and the block service does not carry the
//!   device's logical block size on the wire, so there is nothing here to read it from. A 4Kn disk
//!   would be read as though its LBA 1 were at byte 512, which is a wrong answer rather than an
//!   error. The fix is a field in `fs_proto::blk`, not a change here.
//! - **It does not write.** Partitioning a disk from nife needs a unique GUID per partition,
//!   which needs randomness, which this program is not endowed with. `crates/gpt` refuses to invent
//!   one and notes/gpt.md says why. That is milestone 57's remaining half and it is a decision
//!   rather than a task; see design/roadmap/57-partitioning-and-xattrs.md.
//! - **No hot plug.** The roster is written once at wiring time and never again.
//!
//! Name: ratified 2026-08-03 (calef, milestone 57). The lane shipped it provisionally and calef
//! approved it by name. A surveyor maps land they do not own, which is this program's endowment
//! exactly: a read-only mapping of the roster page that says what devices exist, plus a
//! block-service endpoint for one of them.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use block_roster::{Roster, TRANSPORT_MMIO, TRANSPORT_PCI};
use fs_proto::{blk, req};
use gpt::Gpt;
use gpt::guid::types;
use gpt::span::Span;
use user_rt::{call, send};

/// Slot 0: where the verdict goes. An endpoint with `WRITE`.
const REPORT: u64 = 0;
/// Slot 1: the block service for the one disk this program was handed, `WRITE` (it `CALL`s).
/// Empty in [`ROLE_PROBE`], which holds no disk.
const BLK: u64 = 1;
/// Slot 2: the untyped this program spends on the page tables its own mappings need.
const BUDGET: u64 = 2;
/// Slot 3: the page shared with the block server, `READ|WRITE` (a transfer goes both ways). Empty
/// in [`ROLE_PROBE`].
const BLK_FRAME: u64 = 3;
/// Slot 4: the roster page, **`READ` and nothing else**. See [`ROLE_PROBE`].
const ROSTER_FRAME: u64 = 4;
/// Slot 5: the go-ahead endpoint, `READ`. Only [`ROLE_HOLDER`] has one.
const RESUME: u64 = 5;

/// Where this program puts the page it shares with the block server. **Its choice, not the
/// kernel's**: it holds the frame and maps it, so the address is a local decision and nothing on
/// the kernel side names it.
const BLK_PAGE: u64 = 0x5000_0000;
/// Where this program puts the roster. `kernel/src/user/disk_service.rs` knows this one, because
/// the probe's fault has to be asserted at an address the test can name.
const ROSTER_VA: u64 = 0x5001_0000;

/// The transfer unit of the block service: one filesystem block per request.
const TRANSFER: u64 = blk::BLOCK_SIZE as u64;

/// The logical block size a GPT counts in. See BUGS: nothing on the wire reports the device's, and
/// every disk this project has met uses this one.
const LBA: u64 = 512;

/// The primary table: LBA 0 through LBA 33, which is the protective MBR, the header and the whole
/// 16 KiB entry array. Five transfer blocks cover it.
const PRIMARY_BYTES: usize = 5 * blk::BLOCK_SIZE;
/// The backup table: 33 logical blocks ending on the last block of the disk. Where that starts
/// inside a transfer block depends on the disk's size, so six blocks is the worst case.
const BACKUP_BYTES: usize = 6 * blk::BLOCK_SIZE;

/// The buffers live in `.bss`. A user process gets one 4 KiB page of stack (`USER_STACK_VA`), and
/// these are forty-four kilobytes between them.
static mut PRIMARY: [u8; PRIMARY_BYTES] = [0; PRIMARY_BYTES];
static mut BACKUP: [u8; BACKUP_BYTES] = [0; BACKUP_BYTES];

// The roles, in `a0`. Must match kernel/src/user/disk_service.rs.
const ROLE_SURVEY: u64 = 0;
const ROLE_PROBE: u64 = 1;
const ROLE_HOLDER: u64 = 2;

// The report's first word: what this program managed to establish. Must match
// kernel/src/user/disk_tests.rs.
/// The roster page read as a roster (right magic, believable count).
pub const F_ROSTER: u64 = 1 << 0;
/// The block service answered `SIZE` with a size that is a whole number of logical blocks.
pub const F_SIZE: u64 = 1 << 1;
/// LBA 0 holds a well-formed protective MBR covering the whole disk.
pub const F_MBR: u64 = 1 << 2;
/// The primary header and entry array parsed, CRCs and geometry included.
pub const F_PRIMARY: u64 = 1 << 3;
/// The backup table at the far end of the disk agrees with the primary.
pub const F_BACKUP: u64 = 1 << 4;
/// A partition of type `NIFE_DATA` is on the disk (DECISIONS §45).
pub const F_NIFE: u64 = 1 << 5;
/// Every partition's name decoded as UTF-8 (`sgdisk` writes them; macOS does not, notes/gpt.md).
pub const F_NAMES: u64 = 1 << 6;

/// The probe's announcement, so a fault that never reached the write is distinguishable from the
/// write being refused. Must match `kernel/src/user/disk_tests.rs`.
pub const R_PROBING: u64 = 0x50_0B_11_46;

/// The probe's second word: `Frame::MAP` refused it a writable window on the roster, one rung
/// before the page permissions would have. Must match `kernel/src/user/disk_tests.rs`.
pub const P_RW_REFUSED: u64 = 1 << 0;

/// [`ROLE_HOLDER`]'s announcement: it has the roster mapped and here is the first word of it.
/// ASCII `HOLDG`. Must match `kernel/src/user/disk_tests.rs`.
pub const R_HOLDING: u64 = 0x_48_4F_4C_44_47;

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, _a1: u64, _a2: u64) -> ! {
    match role {
        ROLE_SURVEY => survey(),
        ROLE_PROBE => probe(),
        ROLE_HOLDER => holder(),
        _ => panic!(),
    }
}

/// **Hold the roster, then have it taken away** (milestone 108).
///
/// Maps the frame, reads its first word and reports it, waits for a go-ahead, and reads again. The
/// second read is the whole point: between the two, the kernel revokes the frame, and a page a
/// process mapped from a capability it holds *goes away* when that capability is revoked. So the
/// second read faults.
///
/// **This role could not have existed before the migration.** A page wired in at spawn is in no
/// mapping record, so `Frame::REVOKE` walks past it and the second read returns the same word as
/// the first. There is no way to un-share a page a program was handed at birth, which is the cost
/// the milestone was raised to pay off.
fn holder() -> ! {
    if !user_rt::map_frame(ROSTER_FRAME, ROSTER_VA, false, BUDGET) {
        user_rt::exit()
    }
    // SAFETY: ROSTER_VA is mapped read-only from the frame in slot 4, on the line above.
    let word = unsafe { core::ptr::read_volatile(ROSTER_VA as *const u64) };
    send(REPORT, R_HOLDING, word, 0);

    // The kernel revokes the frame while we are parked here.
    user_rt::recv(RESUME);

    // SAFETY: not safe any more, and that is the test. The page is gone.
    let after = unsafe { core::ptr::read_volatile(ROSTER_VA as *const u64) };
    // Unreachable in a working kernel. If the read did land, say so, so a silent pass is
    // impossible: the test asserts on the fault, and this message is what it sees instead.
    send(REPORT, R_HOLDING, after, 1);
    user_rt::exit()
}

/// Enumerate, read the table, report. Two messages: the roster, then the table.
fn survey() -> ! {
    // **Put our own pages in our own address space** (milestone 108). Both are capabilities we
    // hold; the tables to reach them come out of our own budget. A failure here is fatal and
    // silent on purpose: this program's only channel is `REPORT`, and reporting through a page we
    // failed to map is not a thing it can do.
    if !user_rt::map_frame(BLK_FRAME, BLK_PAGE, true, BUDGET)
        || !user_rt::map_frame(ROSTER_FRAME, ROSTER_VA, false, BUDGET)
    {
        user_rt::exit()
    }

    // The roster first, because it is the cheaper authority and answers a different question.
    // A page that is not a roster reports as no devices AND no F_ROSTER, so "nobody wired me a
    // roster" never reads as "this machine has no disks" (DECISIONS §42).
    let (roster_ok, total, mmio, pci) = match Roster::read(roster_page()) {
        Some(r) => (
            F_ROSTER,
            r.len() as u64,
            r.count_on(TRANSPORT_MMIO) as u64,
            r.count_on(TRANSPORT_PCI) as u64,
        ),
        None => (0, 0, 0, 0),
    };
    send(REPORT, total, mmio, pci);

    let mut flags = roster_ok;
    let mut partitions = 0u64;
    let mut nife_first_lba = 0u64;

    // How big is the disk? This is the question only a *holder* can ask: the roster deliberately
    // carries no capacity, because a size is a fact about the device rather than about the machine
    // having one (crates/block_roster).
    let size = call(BLK, req(blk::SIZE), 0).0 as i64;
    if size > 0 && (size as u64).is_multiple_of(LBA) {
        flags |= F_SIZE;
        let block_count = size as u64 / LBA;
        flags |= read_table(block_count, &mut partitions, &mut nife_first_lba);
    }

    send(REPORT, flags, partitions, nife_first_lba);
    user_rt::exit()
}

/// Read and judge both halves of the table. Returns the flags it established.
fn read_table(block_count: u64, partitions: &mut u64, nife_first_lba: &mut u64) -> u64 {
    let mut flags = 0;

    // LBA 0..=33 in one run: the protective MBR, the primary header, and the whole entry array.
    let Some(head) = Span::covering(0, 34 * LBA, TRANSFER) else {
        return flags;
    };
    let head_buf = primary();
    if !read_span(head, &mut head_buf[..]) {
        return flags;
    }
    // Reborrowed immutably from here on, so this and the backup buffer can be live at once.
    let primary: &[u8] = head_buf;

    if gpt::mbr::validate(&primary[..LBA as usize], block_count).is_ok() {
        flags |= F_MBR;
    }
    let Ok(table) = Gpt::parse(
        &primary[LBA as usize..2 * LBA as usize],
        &primary[2 * LBA as usize..],
    ) else {
        return flags;
    };
    flags |= F_PRIMARY;

    // The backup: the entry array plus the header, ending on the last block of the disk. Reading it
    // is a second I/O on purpose (`Gpt::check_backup` is separate for exactly this reason), and it
    // is the check that says whether anything has happened to this disk since it was written.
    let backup_lba = table.backup_entry_lba();
    let backup_blocks = block_count.saturating_sub(backup_lba);
    let tail_buf = backup();
    if backup_blocks > 1
        && let Some(tail) = Span::covering(backup_lba * LBA, backup_blocks * LBA, TRANSFER)
        && tail.blocks as usize * blk::BLOCK_SIZE <= BACKUP_BYTES
        && read_span(tail, &mut tail_buf[..])
    {
        // The backup array first, then the backup header on the very last logical block.
        let array = tail.offset;
        let array_bytes = (backup_blocks as usize - 1) * LBA as usize;
        let header = array + array_bytes;
        let tail_buf: &[u8] = tail_buf;
        if table
            .check_backup(
                &tail_buf[header..header + LBA as usize],
                &tail_buf[array..header],
            )
            .is_ok()
        {
            flags |= F_BACKUP;
        }
    }

    let mut named = 0;
    for (_, part) in table.partitions() {
        *partitions += 1;
        if part.type_guid == types::NIFE_DATA {
            flags |= F_NIFE;
            *nife_first_lba = part.first_lba;
        }
        let mut label = [0u8; 4 * gpt::entry::NAME_UNITS];
        if part.name_utf8(&mut label).is_ok() {
            named += 1;
        }
    }
    if named == *partitions && *partitions > 0 {
        flags |= F_NAMES;
    }
    flags
}

/// Fetch a [`Span`] of transfer blocks into `into`, one `CALL` per block. Every block lands in the
/// page shared with the block server and is copied out before the next request.
fn read_span(span: Span, into: &mut [u8]) -> bool {
    for i in 0..span.blocks {
        let at = i as usize * blk::BLOCK_SIZE;
        if at + blk::BLOCK_SIZE > into.len() {
            return false;
        }
        if (call(BLK, req(blk::READ), span.first_block + i).0 as i64) < 0 {
            return false;
        }
        // SAFETY: BLK_PAGE is a page this program mapped from a frame it holds, of exactly one
        // block, and the destination window was bounds-checked against `into` on the line above.
        unsafe {
            core::ptr::copy_nonoverlapping(
                BLK_PAGE as *const u8,
                into.as_mut_ptr().add(at),
                blk::BLOCK_SIZE,
            );
        };
    }
    true
}

/// **The negative control**, and since milestone 108 it attacks two rungs instead of one.
///
/// First it asks `Frame::MAP` for a **writable** window on the roster. It holds the frame with
/// `READ` alone, so the kernel refuses before it writes a page-table entry: there is no writable
/// mapping of the roster anywhere in this address space to be found, because the capability could
/// not produce one. Then it maps the page read-only, which it may, announces the address, and
/// writes there anyway. That faults.
///
/// Knowing the address buys nothing at either rung, which is the whole claim: enumeration is a
/// listing and not a lever, and there is no address at which this write succeeds because the
/// boundary is a capability's rights and then a page's permissions, rather than anything this
/// program could be persuaded to skip.
fn probe() -> ! {
    // Rung one: a writable mapping, refused by the rights on the capability itself.
    let rw_refused = !user_rt::map_frame(ROSTER_FRAME, ROSTER_VA, true, BUDGET);

    // Rung two: the read-only mapping we are entitled to, and a write through it.
    if !user_rt::map_frame(ROSTER_FRAME, ROSTER_VA, false, BUDGET) {
        user_rt::exit()
    }
    send(
        REPORT,
        R_PROBING,
        ROSTER_VA,
        if rw_refused { P_RW_REFUSED } else { 0 },
    );
    // SAFETY: not safe, and that is the test. The kernel refuses it.
    unsafe { core::ptr::write_volatile(ROSTER_VA as *mut u64, 0) };
    // Unreachable in a working kernel; if we get here the write was allowed and the test's
    // fault-count assertion is what says so.
    user_rt::exit()
}

/// The roster page, read-only. Its contents are checked by `block_roster::Roster::read`, which
/// refuses a page nobody wrote and a count larger than the page can hold.
fn roster_page() -> &'static [u8] {
    // SAFETY: a page this program mapped read-only from a frame it holds `READ` on, of exactly
    // `fs_proto::PAGE` bytes, and never written by anybody after that (the roster is built once at
    // wiring time).
    unsafe { core::slice::from_raw_parts(ROSTER_VA as *const u8, fs_proto::PAGE) }
}

/// The primary table buffer.
///
/// # Safety
/// One thread per address space here (DECISIONS §33), so there is no second reference. The raw
/// pointer first is what `static_mut_refs` asks for, and it asks for a real reason.
fn primary() -> &'static mut [u8] {
    let p = &raw mut PRIMARY;
    // SAFETY: see above.
    unsafe { &mut *p }
}

/// The backup table buffer. Same reasoning as [`primary`].
fn backup() -> &'static mut [u8] {
    let p = &raw mut BACKUP;
    // SAFETY: see above.
    unsafe { &mut *p }
}

user_rt::panic_handler!();
