use core::sync::atomic::Ordering;

use block_roster::{Roster, TRANSPORT_MMIO, TRANSPORT_PCI};

use super::*;
use crate::arch::exceptions::USER_FAULTS;

/// Bounded by wall clock rather than by a yield count, for the reason `ntp_tests` records: work a
/// test spawns runs on other cores, so a yield on an idle core elapses no real time.
fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
    while crate::arch::timer::now() < deadline {
        if cond() {
            return true;
        }
        crate::sched::yield_now();
    }
    cond()
}

fn surveyor_image() -> &'static [u8] {
    program("disk_surveyor").expect("no disk_surveyor program in the initrd archive")
}

// The report's flag words. Must match user/src/disk_surveyor.rs.
const F_ROSTER: u64 = 1 << 0;
const F_SIZE: u64 = 1 << 1;
const F_MBR: u64 = 1 << 2;
const F_PRIMARY: u64 = 1 << 3;
const F_BACKUP: u64 = 1 << 4;
const F_CRICKER: u64 = 1 << 5;
const F_NAMES: u64 = 1 << 6;
const R_PROBING: u64 = 0x50_0B_11_46;

/// The layout `sgdisk` 1.0.10 wrote into the fixture the test image is built from
/// (`crates/gpt/tests/real_disks.rs` has the exact commands). Three partitions on a 64 MiB disk,
/// the third of them a cricker-os data partition (DECISIONS §45) starting at block 30720.
const PARTITIONS: u64 = 3;
const CRICKER_FIRST_LBA: u64 = 30720;

/// **The machine finds a partition table on a disk it did not write, and says what is on it.**
///
/// This is the half of milestone 57 that is not optional. A block device hands a kernel an
/// undifferentiated run of blocks, and which of them is a filesystem is written in the partition
/// table and nowhere else, so an OS that cannot read a GPT cannot find a filesystem on a disk it did
/// not create. `crates/gpt` has been able to parse one since 2026-07-30 and was wired to nothing;
/// this is the wire.
///
/// What makes the assertion worth something is where the bytes came from: the image is built from
/// the committed `sgdisk` fixture, so the table under test was written by gptfdisk, in C++, by
/// people who have never heard of this project. A parser that only reads its own output proves
/// nothing, and neither does a driver that only reads a disk its own `mkfs` laid out.
///
/// The backup check is the one that would have been easy to skip and is the reason this reads a
/// second, differently-aligned run of blocks at the far end of the disk: 33 logical blocks ending on
/// the last block, which begins partway into a filesystem block at an offset that depends on the
/// disk's size (`gpt::span`).
#[test_case]
fn the_disk_surveyor_reads_a_table_gptfdisk_wrote() {
    let Some(w) = disk_service::start(fs_service::blk_server_image(), surveyor_image()) else {
        // No fourth mmio block device: this boot did not build the GPT image. A fact about the
        // machine, not a failure.
        return;
    };
    // Past device bring-up, so a hang below is a hang in a read rather than in the driver.
    let [ready, ..] = crate::sched::ipc_recv(w.ready);
    assert_eq!(ready, fs_proto::fixture::READY);

    // Message one: the roster, which is the authority that does NOT involve the disk.
    let [total, mmio, pci, ..] = crate::sched::ipc_recv(w.report);
    assert_eq!(
        total as usize, w.devices,
        "the surveyor counted {total} devices; the kernel put {} in the page",
        w.devices,
    );
    assert!(
        mmio >= 4,
        "four mmio disks are attached when this test runs (crickerfs, RedoxFS, the crash image, \
         and the GPT image); the roster names {mmio}",
    );
    assert_eq!(pci, 1, "one virtio-blk-pci disk is attached (§18's transport)");
    assert_eq!(total, mmio + pci, "every device is on one bus or the other");

    // And the kernel's own read of the same frame agrees, computed through the same contract crate
    // rather than by trusting the number the program sent back.
    let here = Roster::read(w.roster()).expect("the kernel wrote a roster it cannot read");
    assert_eq!(here.len(), w.devices);
    assert_eq!(here.count_on(TRANSPORT_MMIO) as u64, mmio);
    assert_eq!(here.count_on(TRANSPORT_PCI) as u64, pci);

    // Message two: the table, which is the authority that does.
    let [flags, partitions, cricker_first_lba, ..] = crate::sched::ipc_recv(w.report);
    for (bit, what) in [
        (F_ROSTER, "the roster page read as a roster"),
        (F_SIZE, "the block service reported a whole number of blocks"),
        (F_MBR, "LBA 0 holds a protective MBR covering the disk"),
        (F_PRIMARY, "the primary header and entry array parsed"),
        (F_BACKUP, "the backup table agrees with the primary"),
        (F_CRICKER, "a cricker-os data partition is on the disk"),
        (F_NAMES, "every partition name decoded"),
    ] {
        assert!(flags & bit != 0, "{what} (flags {flags:#x})");
    }
    assert_eq!(partitions, PARTITIONS);
    assert_eq!(
        cricker_first_lba, CRICKER_FIRST_LBA,
        "the cricker-os partition starts where sgdisk put it",
    );
}

/// **A listing is not a lever.**
///
/// The roster answers "what drives are attached" and answers nothing else. The same binary, the
/// same mapping, the exact address the surveyor reads its roster from, and no disk at all: it
/// announces the address and writes there, and the write is refused because the mapping is
/// read-only. There is no address at which it succeeds, because the boundary is a page permission
/// rather than a check this program could be argued out of.
///
/// This is the claim `lsblk` plus `parted` cannot make. On Linux the listing is world-readable and
/// naming a device to a destructive tool is a matter of typing the right path; the warning in
/// Chris's own router instructions ("confirm the target device path before proceeding") exists
/// because nothing enforces it. Here the enumeration is a read-only mapping and the disk is an
/// endpoint somebody else was handed.
#[test_case]
fn the_roster_is_a_listing_and_not_a_lever() {
    let faults = USER_FAULTS.load(Ordering::Relaxed);
    let report = disk_service::start_probe(surveyor_image());

    let [tag, va, ..] = crate::sched::ipc_recv(report);
    assert_eq!(tag, R_PROBING, "the probe never reached its write");
    assert_eq!(va, disk_service::ROSTER_VA);

    assert!(
        wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
        "a program wrote the block-device roster at {va:#x} and was NOT stopped",
    );
    // The exact address, on both ISAs (milestone 19's portable fault record). The *kind* is not
    // asserted: what is being claimed here is where the write was aimed and that it did not land.
    assert_eq!(
        crate::arch::exceptions::last_user_fault().map(|(_, addr)| addr),
        Some(disk_service::ROSTER_VA),
    );
}
