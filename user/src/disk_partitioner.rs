//! **`disk_partitioner`**: writes the map `disk_surveyor` reads (milestone 57, notes/gpt.md).
//!
//! The destructive half of the pair, and the one whose endowment is the point. It holds exactly
//! two things:
//!
//! - a **block-service endpoint** for **one** disk (`fs_proto::blk`), which is the authority to
//!   destroy that disk and no other; and
//! - an **entropy endpoint** (`entropy_proto`), which is the authority to obtain random bytes and
//!   nothing else: it cannot reach the virtio-rng device, program its queue, or see the page the
//!   device writes into.
//!
//! **Both are required and neither is sufficient**, which is a claim the wiring can make and
//! `parted` cannot. A GPT gives every partition a globally unique id, and an id that is not random
//! is not unique; `crates/gpt` therefore refuses to invent one (notes/gpt.md), so this program
//! **without the entropy endpoint writes nothing at all** rather than falling back to a counter.
//! The kernel-side test spawns it both ways and reads the disk afterwards to see which happened.
//!
//! Compare `parted /dev/sda` as root: one authority, obtained by typing a path, reaching every disk
//! in the machine, with `/dev/urandom` always there for the asking. calef's own router instructions
//! carry a "confirm the target device path before proceeding" warning precisely because nothing in
//! that arrangement can enforce it. **Here the warning is structural**: this program was handed one
//! disk and there is no path to type.
//!
//! # The two roles
//!
//! | `a0` | |
//! |---|---|
//! | [`ROLE_PARTITION`] | draw four GUIDs, lay out a table, write both copies |
//! | [`ROLE_VERIFY`] | read the disk back and report what is on it |
//!
//! `ROLE_VERIFY` is a **separate process with no entropy endpoint**, deliberately: a writer that
//! checks its own work proves the least interesting thing available, and the verify role is also
//! what turns "the refusal wrote nothing" from an assertion into a reading.
//!
//! # What it writes
//!
//! The layout is `fs_proto::fixture::blank`, three partitions on a 64 MiB disk in the 2048-block
//! alignment every real tool uses. Placement is this program's decision and not the crate's:
//! `Gpt::create` validates a layout and refuses to move one, because alignment is policy and a
//! format library that quietly relocated a partition would be doing policy behind its caller's
//! back.
//!
//! Every block written is **read first** and the table bytes laid over it. That matters on a disk
//! that is not blank: the primary table is 34 logical blocks and the transfer unit is eight of
//! them, so the tail of the last transfer block is somebody's data, and a partitioner that wrote
//! whole transfer blocks would zero the first 3 KiB of a partition it was only supposed to be
//! describing.
//!
//! # EXAMPLES
//!
//! ```text
//!   ROLE_PARTITION, wired with a disk AND an entropy endpoint:
//!     report: R_PARTITIONED, 3 partitions
//!
//!   ROLE_PARTITION, wired with the disk and NO entropy endpoint:
//!     report: R_NO_ENTROPY, 0 partitions          (and the disk is untouched)
//!
//!   ROLE_VERIFY, afterwards:
//!     report: F_MBR|F_PRIMARY|F_BACKUP|F_NIFE|F_NAMES, 3 partitions, data at LBA 30720
//! ```
//!
//! # BUGS
//!
//! - **The logical block size is assumed to be 512**, the same assumption and the same reason as
//!   `disk_surveyor`: nothing in `fs_proto::blk` carries the device's, and every disk this project
//!   has met reports 512. Writing a table with the wrong unit produces a table no other OS can
//!   read, which is worse than a wrong answer on the read side. The fix is a field on the wire.
//! - **It writes one hard-coded layout.** There is no `mkpart` command line, no partition sizing,
//!   and no way to ask for a different type GUID; the layout is the fixture's. What this
//!   demonstrates is the *authority*, and a command line is the smaller half of the work left.
//! - **Nothing here is crash-atomic.** A kill partway through leaves a disk with one copy of the
//!   table written and the other stale, which `Gpt::check_backup` reports as a mismatch. Real
//!   partitioners have the same property; the difference is that milestone 37 measured the
//!   filesystem's crash behaviour and nothing has measured this one's.
//! - **The unique GUIDs are not checked for collision against the disk's existing table**, because
//!   the table is being replaced wholesale. A future `mkpart` that adds an entry to a table it did
//!   not write must check.
//!
//! Name: provisional (§89's state, adopted 2026-08-16; this block wrote the workaround that
//! argued for it). The clearest ratification owed on this surface. The introducing commit (4db2fd7, 2026-08-03, milestone 57) says
//! "`disk_partitioner` (provisional name) writes the map `disk_surveyor` reads" and calls its
//! sibling "`fs_maker` (provisional name)" in the same breath. So the record does not explain the
//! word; it records that nobody decided it. That sibling is `fs_server/src/bin/mkfs.rs` now, which
//! is a second provisional name resolved by somebody mid-task on a surface this record does not
//! cover (see notes/naming.md's BUGS). `disk_surveyor` calef approved by name the same day,
//! and the pairing of the two (survey, partition) is visible in the tree without being written
//! down anywhere.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use entropy_proto as entropy;
use fs_proto::fixture::blank;
use fs_proto::{blk, req};
use gpt::entry::Entry;
use gpt::guid::{Guid, types};
use gpt::{ENTRY_ARRAY_BYTES, Gpt};
use user_rt::{call, send};

/// Slot 0: where the verdict goes. An endpoint with `WRITE`.
const REPORT: u64 = 0;
/// Slot 1: the block service for the one disk this program was handed, `WRITE` (it `CALL`s).
const BLK: u64 = 1;
/// Slot 2: the entropy service, `WRITE` (it `CALL`s). **Empty in the negative control.**
const ENTROPY: u64 = 2;
/// Slot 3: the untyped this program spends on the page tables its own mapping needs.
const BUDGET: u64 = 3;
/// Slot 4: the page shared with the block server, `READ|WRITE` (a transfer goes both ways).
const BLK_FRAME: u64 = 4;

/// Where this program puts the page it shares with the block server. **Its choice**: it holds the
/// frame and maps it (milestone 108), so nothing on the kernel side names this address.
const BLK_PAGE: u64 = 0x5000_0000;

/// The transfer unit of the block service: one filesystem block per request.
const TRANSFER: u64 = blk::BLOCK_SIZE as u64;

/// The logical block size a GPT counts in. See BUGS.
const LBA: u64 = blank::LBA;

// The roles, in `a0`. Must match kernel/src/user/disk_service.rs.
const ROLE_PARTITION: u64 = 0;
const ROLE_VERIFY: u64 = 1;

// The partition role's verdict, the report's first word. Must match kernel/src/user/disk_tests.rs.
/// The table was laid out and both copies were written. ASCII `PARTD`, so a hex dump reads.
pub const R_PARTITIONED: u64 = 0x_50_41_52_54_44;
/// **The entropy endpoint was not there, so nothing was written.** The refusal this program exists
/// to make: a unique id that is not random is not unique, and a made-up one would be worse than no
/// partition at all because it would look right (DECISIONS §42).
pub const R_NO_ENTROPY: u64 = 0x_4E_4F_52_4E_47;
/// The disk refused a read or a write, or the layout did not fit it. The second word says where.
pub const R_DISK_FAILED: u64 = 0x_44_49_53_4B_45;

// The verify role's flags, the report's first word. Must match kernel/src/user/disk_tests.rs.
/// LBA 0 holds a well-formed protective MBR covering the whole disk.
pub const F_MBR: u64 = 1 << 0;
/// The primary header and entry array parsed: four CRCs, the geometry, no overlaps.
pub const F_PRIMARY: u64 = 1 << 1;
/// The backup table at the far end of the disk agrees with the primary, field by field.
pub const F_BACKUP: u64 = 1 << 2;
/// A partition of type `NIFE_DATA` is on the disk (DECISIONS §45).
pub const F_NIFE: u64 = 1 << 3;
/// Every partition's name decoded as UTF-8 and matched the layout's.
pub const F_NAMES: u64 = 1 << 4;
/// Every partition's unique GUID is distinct, non-zero, and stamped version 4.
pub const F_UNIQUE: u64 = 1 << 5;

/// The entry array under construction, and the buffer `Gpt::create` writes into. 16 KiB, so it goes
/// in `.bss` rather than on the one-page stack.
static mut ARRAY: [u8; ENTRY_ARRAY_BYTES] = [0; ENTRY_ARRAY_BYTES];
/// The primary table as read back: LBA 0..33 is 34 logical blocks, which is five transfer blocks.
static mut PRIMARY: [u8; 5 * blk::BLOCK_SIZE] = [0; 5 * blk::BLOCK_SIZE];
/// The backup table as read back. 33 logical blocks ending on the last block of the disk; where
/// that begins inside a transfer block depends on the disk's size, so six is the worst case.
static mut BACKUP: [u8; 6 * blk::BLOCK_SIZE] = [0; 6 * blk::BLOCK_SIZE];

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, _a1: u64, _a2: u64) -> ! {
    // The shared page is a `Frame` we hold, mapped here out of our own budget (milestone 108).
    // Before either role, because every `blk` call goes through it.
    if !user_rt::map_frame(BLK_FRAME, BLK_PAGE, true, BUDGET) {
        user_rt::exit()
    }
    match role {
        ROLE_PARTITION => partition(),
        ROLE_VERIFY => verify(),
        _ => panic!(),
    }
}

/// Draw a table's worth of GUIDs, lay the table out, write it. **In that order**, so that a program
/// with no entropy endpoint reports and exits with the disk untouched.
fn partition() -> ! {
    // Four unique ids: the disk's, and one per partition. Drawn first and all together, because the
    // only refusal worth making is one made before anything is written.
    let mut guids = [Guid::ZERO; 4];
    for g in guids.iter_mut() {
        let Some(bytes) = random16() else {
            send(REPORT, R_NO_ENTROPY, 0, 0);
            user_rt::exit()
        };
        *g = Guid::v4_from_random(bytes);
    }

    let layout = [
        (types::EFI_SYSTEM, blank::ESP, blank::NAMES[0]),
        (types::LINUX_FILESYSTEM, blank::LINUX, blank::NAMES[1]),
        (types::NIFE_DATA, blank::DATA, blank::NAMES[2]),
    ];
    let mut parts = [Entry::UNUSED; blank::PARTITIONS];
    for (i, (type_guid, (first, last), name)) in layout.iter().enumerate() {
        let Ok(e) = Entry::new(*type_guid, guids[i + 1], *first, *last).with_name(name) else {
            send(REPORT, R_DISK_FAILED, 1, i as u64);
            user_rt::exit()
        };
        parts[i] = e;
    }

    // The disk's own size, asked of the disk rather than assumed. A layout that does not fit is a
    // refusal from `Gpt::create`, not a truncated table.
    let size = call(BLK, req(blk::SIZE), 0).0 as i64;
    if size <= 0 || !(size as u64).is_multiple_of(LBA) {
        send(REPORT, R_DISK_FAILED, 2, size as u64);
        user_rt::exit()
    }
    let block_count = size as u64 / LBA;

    let array = array();
    let Ok(table) = Gpt::create(guids[0], LBA as usize, block_count, &parts, array) else {
        send(REPORT, R_DISK_FAILED, 3, block_count);
        user_rt::exit()
    };

    // One logical block of scratch, built and written one at a time: the MBR, the primary header,
    // and the backup header are each 512 bytes at a known LBA, and the entry array is written
    // straight out of the buffer `Gpt::create` filled.
    let mut block = [0u8; LBA as usize];
    let entries: &[u8] = table.entry_array();

    let steps: [(u64, Option<&[u8]>); 5] = [
        (0, None),                       // the protective MBR
        (gpt::PRIMARY_HEADER_LBA, None), // the primary header
        (table.primary_entry_lba(), Some(entries)),
        (table.backup_entry_lba(), Some(entries)),
        (table.backup_header_lba(), None), // the backup header, on the last block
    ];
    for (n, (lba, bytes)) in steps.iter().enumerate() {
        let data: &[u8] = match bytes {
            Some(b) => b,
            None => {
                block.fill(0);
                let built = match n {
                    0 => table.write_protective_mbr(&mut block),
                    1 => table.write_primary_header(&mut block),
                    _ => table.write_backup_header(&mut block),
                };
                if built.is_err() {
                    send(REPORT, R_DISK_FAILED, 4, n as u64);
                    user_rt::exit()
                }
                &block
            }
        };
        if !write_at(*lba, data) {
            send(REPORT, R_DISK_FAILED, 5, *lba);
            user_rt::exit()
        }
    }

    send(REPORT, R_PARTITIONED, blank::PARTITIONS as u64, 0);
    user_rt::exit()
}

/// Read the disk back and say what is on it. No entropy endpoint, no writes.
fn verify() -> ! {
    let mut flags = 0u64;
    let mut partitions = 0u64;
    let mut data_first_lba = 0u64;

    let size = call(BLK, req(blk::SIZE), 0).0 as i64;
    if size <= 0 || !(size as u64).is_multiple_of(LBA) {
        send(REPORT, 0, 0, 0);
        user_rt::exit()
    }
    let block_count = size as u64 / LBA;

    let head = primary();
    if !read_at(0, &mut head[..]) {
        send(REPORT, 0, 0, 0);
        user_rt::exit()
    }
    let head: &[u8] = head;

    if gpt::mbr::validate(&head[..LBA as usize], block_count).is_ok() {
        flags |= F_MBR;
    }
    let Ok(table) = Gpt::parse(
        &head[LBA as usize..2 * LBA as usize],
        &head[2 * LBA as usize..],
    ) else {
        send(REPORT, flags, 0, 0);
        user_rt::exit()
    };
    flags |= F_PRIMARY;

    // The backup, read as its own I/O at the far end of the disk, which is what says whether the
    // two copies this program wrote actually agree on the platter.
    let backup_lba = table.backup_entry_lba();
    let backup_blocks = block_count.saturating_sub(backup_lba);
    let tail = backup();
    if backup_blocks > 1
        && let Some(span) =
            gpt::span::Span::covering(backup_lba * LBA, backup_blocks * LBA, TRANSFER)
        && span.blocks as usize * blk::BLOCK_SIZE <= tail.len()
        && read_span(span, &mut tail[..])
    {
        let array = span.offset;
        let header = array + (backup_blocks as usize - 1) * LBA as usize;
        let tail: &[u8] = tail;
        if table
            .check_backup(&tail[header..header + LBA as usize], &tail[array..header])
            .is_ok()
        {
            flags |= F_BACKUP;
        }
    }

    let mut named = 0u64;
    let mut unique = 0u64;
    let mut seen = [Guid::ZERO; blank::PARTITIONS];
    for (i, part) in table.partitions() {
        if partitions as usize >= blank::PARTITIONS {
            partitions += 1; // more entries than the layout has: the count below will say so
            continue;
        }
        if part.type_guid == types::NIFE_DATA {
            flags |= F_NIFE;
            data_first_lba = part.first_lba;
        }
        let mut label = [0u8; 4 * gpt::entry::NAME_UNITS];
        if let Ok(n) = part.name_utf8(&mut label)
            && i < blank::NAMES.len()
            && &label[..n] == blank::NAMES[i].as_bytes()
        {
            named += 1;
        }
        // A unique GUID that is zero, repeated, or not stamped version 4 would mean the entropy
        // never arrived and something invented an id anyway, which is the failure this program's
        // whole shape exists to prevent.
        let g = part.unique_guid;
        let text = g.to_ascii();
        if !g.is_zero() && text[14] == b'4' && !seen[..partitions as usize].contains(&g) {
            unique += 1;
        }
        seen[partitions as usize] = g;
        partitions += 1;
    }
    if partitions == blank::PARTITIONS as u64 {
        if named == partitions {
            flags |= F_NAMES;
        }
        if unique == partitions {
            flags |= F_UNIQUE;
        }
    }

    send(REPORT, flags, partitions, data_first_lba);
    user_rt::exit()
}

/// Sixteen random bytes from the entropy service, or `None` if this process holds no entropy
/// endpoint (or the service has none to give).
///
/// Two round trips, because a reply carries one word. `entropy_proto::delivered` is what separates
/// "the service answered with n bytes" from "the kernel refused the call", and it can: a count is
/// always `0..=8`, while every kernel error is a small negative that reads as an enormous `u64`.
/// So a program with an empty slot 2 finds out here, before it has written anything.
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

/// Lay `data` over the disk starting at logical block `first_lba`, **preserving everything else in
/// the transfer blocks it touches**: each is read, patched in the shared page, and written back.
fn write_at(first_lba: u64, data: &[u8]) -> bool {
    let mut done = 0usize;
    while done < data.len() {
        let byte_at = first_lba * LBA + done as u64;
        let block = byte_at / TRANSFER;
        let offset = (byte_at % TRANSFER) as usize;
        let n = core::cmp::min(blk::BLOCK_SIZE - offset, data.len() - done);
        if (call(BLK, req(blk::READ), block).0 as i64) < 0 {
            return false;
        }
        // SAFETY: BLK_PAGE is a mapped, writable page of exactly one transfer block, and `offset +
        // n` is bounded by that block above.
        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr().add(done),
                (BLK_PAGE as *mut u8).add(offset),
                n,
            );
        }
        if (call(BLK, req(blk::WRITE), block).0 as i64) < 0 {
            return false;
        }
        done += n;
    }
    true
}

/// Read `into.len()` bytes starting at logical block `first_lba`. `into` must be a whole number of
/// transfer blocks and `first_lba` must land on one.
fn read_at(first_lba: u64, into: &mut [u8]) -> bool {
    match gpt::span::Span::covering(first_lba * LBA, into.len() as u64, TRANSFER) {
        Some(span) if span.offset == 0 => read_span(span, into),
        _ => false,
    }
}

/// Fetch a [`gpt::span::Span`] of transfer blocks into `into`, one `CALL` per block, copying each
/// out of the shared page before the next request lands in it.
fn read_span(span: gpt::span::Span, into: &mut [u8]) -> bool {
    for i in 0..span.blocks {
        let at = i as usize * blk::BLOCK_SIZE;
        if at + blk::BLOCK_SIZE > into.len() {
            return false;
        }
        if (call(BLK, req(blk::READ), span.first_block + i).0 as i64) < 0 {
            return false;
        }
        // SAFETY: BLK_PAGE is a mapped page of exactly one block, and the destination window was
        // bounds-checked against `into` on the line above.
        unsafe {
            core::ptr::copy_nonoverlapping(
                BLK_PAGE as *const u8,
                into.as_mut_ptr().add(at),
                blk::BLOCK_SIZE,
            );
        }
    }
    true
}

/// The entry-array buffer.
///
/// # Safety
/// One thread per address space here (DECISIONS §33), so there is no second reference. Taking the
/// raw pointer first is what `static_mut_refs` asks for.
fn array() -> &'static mut [u8] {
    let p = &raw mut ARRAY;
    // SAFETY: see above.
    unsafe { &mut *p }
}

/// The primary-table read buffer. Same reasoning as [`array()`].
fn primary() -> &'static mut [u8] {
    let p = &raw mut PRIMARY;
    // SAFETY: see above.
    unsafe { &mut *p }
}

/// The backup-table read buffer. Same reasoning as [`array()`].
fn backup() -> &'static mut [u8] {
    let p = &raw mut BACKUP;
    // SAFETY: see above.
    unsafe { &mut *p }
}

user_rt::panic_handler!();
