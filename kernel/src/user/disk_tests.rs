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

fn partitioner_image() -> &'static [u8] {
    program("disk_partitioner").expect("no disk_partitioner program in the initrd archive")
}

// The report's flag words. Must match user/src/disk_surveyor.rs.
const F_ROSTER: u64 = 1 << 0;
const F_SIZE: u64 = 1 << 1;
const F_MBR: u64 = 1 << 2;
const F_PRIMARY: u64 = 1 << 3;
const F_BACKUP: u64 = 1 << 4;
const F_NIFE: u64 = 1 << 5;
const F_NAMES: u64 = 1 << 6;
const R_PROBING: u64 = 0x50_0B_11_46;
const P_RW_REFUSED: u64 = 1 << 0;
const R_HOLDING: u64 = 0x_48_4F_4C_44_47;

/// The layout `sgdisk` 1.0.10 wrote into the fixture the test image is built from
/// (`crates/gpt/tests/real_disks.rs` has the exact commands). Three partitions on a 64 MiB disk,
/// the third of them a nife data partition (DECISIONS §45) starting at block 30720.
const PARTITIONS: u64 = 3;
const NIFE_FIRST_LBA: u64 = 30720;

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
        "at least four mmio disks are attached when this test runs (nifefs, RedoxFS, the crash \
         image, the GPT image, and since milestone 57's write half the blank one); the roster names \
         {mmio}",
    );
    assert_eq!(
        pci, 1,
        "one virtio-blk-pci disk is attached (§18's transport)"
    );
    assert_eq!(total, mmio + pci, "every device is on one bus or the other");

    // And the kernel's own read of the same frame agrees, computed through the same contract crate
    // rather than by trusting the number the program sent back.
    let here = Roster::read(w.roster()).expect("the kernel wrote a roster it cannot read");
    assert_eq!(here.len(), w.devices);
    assert_eq!(here.count_on(TRANSPORT_MMIO) as u64, mmio);
    assert_eq!(here.count_on(TRANSPORT_PCI) as u64, pci);

    // Message two: the table, which is the authority that does.
    let [flags, partitions, nife_first_lba, ..] = crate::sched::ipc_recv(w.report);
    for (bit, what) in [
        (F_ROSTER, "the roster page read as a roster"),
        (
            F_SIZE,
            "the block service reported a whole number of blocks",
        ),
        (F_MBR, "LBA 0 holds a protective MBR covering the disk"),
        (F_PRIMARY, "the primary header and entry array parsed"),
        (F_BACKUP, "the backup table agrees with the primary"),
        (F_NIFE, "a nife data partition is on the disk"),
        (F_NAMES, "every partition name decoded"),
    ] {
        assert!(flags & bit != 0, "{what} (flags {flags:#x})");
    }
    assert_eq!(partitions, PARTITIONS);
    assert_eq!(
        nife_first_lba, NIFE_FIRST_LBA,
        "the nife partition starts where sgdisk put it",
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
/// calef's own router instructions ("confirm the target device path before proceeding") exists
/// because nothing enforces it. Here the enumeration is a read-only mapping and the disk is an
/// endpoint somebody else was handed.
#[test_case]
fn the_roster_is_a_listing_and_not_a_lever() {
    let faults = USER_FAULTS.load(Ordering::Relaxed);
    let report = disk_service::start_probe(surveyor_image());

    let [tag, va, how, ..] = crate::sched::ipc_recv(report);
    assert_eq!(tag, R_PROBING, "the probe never reached its write");
    assert_eq!(va, disk_service::ROSTER_VA);

    // **The rung the migration added** (milestone 108). The probe holds the roster as a `Frame`
    // with `READ` alone, so it asked `Frame::MAP` for a writable window first and was refused
    // before a page-table entry was written. When the roster was a spawn-time mapping there was
    // nothing to refuse: the only boundary was the permission bit the fault below trips.
    assert!(
        how & P_RW_REFUSED != 0,
        "a frame held READ-only produced a WRITABLE mapping of the roster",
    );

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

/// **A page a driver holds can be taken back; a page it was handed at birth cannot.**
///
/// This is milestone 108's claim, and it is the one assertion in the disk suite that fails on the
/// code as it stood the day before. The surveyor's roster used to arrive through `Spawn::maps`: the
/// kernel wrote it into the new address space before the first instruction, and recorded nothing.
/// `revoke::revoke_frame` works off the mapping database, so it walked straight past that page.
/// There was no `Frame` capability to delete either. **A spawn-time mapping is permanent by
/// construction**, and "the driver's buffer is revocable" was a property the system could not have.
///
/// So: the program maps the frame it holds, reads the first word and reports it, and parks. The
/// kernel checks that word against its own read of the same physical page through the direct map,
/// which is what says the mapping was real and was of *this* page. Then it revokes the frame and
/// lets the program go. The second read faults, at the address it faults at, and the program's
/// "the read landed" message never arrives.
///
/// Run it against the old wiring and the second read returns the same word as the first.
#[test_case]
fn the_roster_can_be_revoked_out_from_under_its_holder() {
    let faults = USER_FAULTS.load(Ordering::Relaxed);
    let w = disk_service::start_holder(surveyor_image());

    let [tag, word, ..] = crate::sched::ipc_recv(w.report);
    assert_eq!(tag, R_HOLDING, "the holder never mapped its roster frame");
    assert_eq!(
        word,
        w.first_word(),
        "the holder read something other than the roster frame the kernel granted it",
    );
    // The roster's first word is its magic. Asserting it (rather than "not zero") is what makes the
    // second read's absence meaningful: a page of zeroes would read the same before and after.
    assert_eq!(word, block_roster::MAGIC);

    // Take the page back. Every capability to it is deleted and it is unmapped from every address
    // space that recorded a mapping of it, the holder's included.
    crate::revoke::revoke_frame(w.roster_phys);
    crate::sched::ipc_send(w.resume, [0, 0, 0]);

    assert!(
        wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
        "a program read a frame that had been revoked out from under it, at {:#x}, and was NOT \
         stopped: the mapping outlived the capability",
        disk_service::ROSTER_VA,
    );
    assert_eq!(
        crate::arch::exceptions::last_user_fault().map(|(_, addr)| addr),
        Some(disk_service::ROSTER_VA),
        "the fault was somewhere other than the revoked page",
    );
}

// =============================================================================================
// Milestone 57's write half: partitioning and formatting on the target
// =============================================================================================

// The partitioner's roles and verdicts. Must match user/src/disk_partitioner.rs.
const ROLE_PARTITION: u64 = 0;
const ROLE_VERIFY: u64 = 1;
const R_PARTITIONED: u64 = 0x_50_41_52_54_44;
const P_NO_ENTROPY: u64 = 0x_4E_4F_52_4E_47;
const PF_MBR: u64 = 1 << 0;
const PF_PRIMARY: u64 = 1 << 1;
const PF_BACKUP: u64 = 1 << 2;
const PF_NIFE: u64 = 1 << 3;
const PF_NAMES: u64 = 1 << 4;
const PF_UNIQUE: u64 = 1 << 5;

// `mkfs`'s roles and verdicts. Must match fs_server/src/bin/mkfs.rs.
const ROLE_MAKE: u64 = 0;
const ROLE_CHECK: u64 = 1;
const R_MADE: u64 = 0x_4D_4B_46_53_44;
const M_NO_ENTROPY: u64 = 0x_4E_4F_52_4E_47;
const M_NO_DISK: u64 = 0x_4E_4F_44_53_4B;
const R_FOUND: u64 = 0x_46_4F_55_4E_44;
const R_NO_FS: u64 = 0x_4E_4F_46_53_00;

/// The entropy service's request endpoint, wiring the service if this is the first test to ask.
///
/// The mmio transport, because both `virt` machines offer it with nothing in the way and this test
/// is not about the bus; `entropy_tests` runs the same service over both.
///
/// Returns `None` if the mmio virtio-rng device is not attached (milestone 145: correct on a bare
/// board boot with no `NIFE_RNG`-equivalent).
fn entropy_endpoint() -> Option<crate::sched::EpId> {
    let image = program("entropy").expect("no entropy program in the initrd archive");
    let w = entropy_service::ensure(image, entropy_service::Bus::Mmio)?;
    if let Some(report) = w.wait_for_ready() {
        assert_eq!(
            report[0],
            entropy_proto::READY,
            "the entropy service did not come up (it reported {:#x})",
            report[0],
        );
    }
    Some(w.request)
}

/// **The machine partitions a disk and puts a filesystem on it, and can do neither without the
/// capability that makes an identifier unique.**
///
/// Milestone 57's write half in one narrative, on one disk, in order, because the claims are about
/// *sequence*: "the refused run left the disk as it found it" is a statement about what is on the
/// platter afterwards, and only a later read can make it.
///
/// ```text
///   1  partition, no entropy   -> refused          5  mkfs, no disk       -> refused
///   2  read the table          -> nothing there    6  read the partition  -> still no filesystem
///   3  partition, with entropy -> three partitions 7  mkfs, both          -> a filesystem
///   4  mkfs, no entropy        -> refused          8  read the partition  -> the file is in it
/// ```
///
/// **The pair is the claim, not the success.** A working `mkfs` proves nothing about authority;
/// every Unix has one and it reaches every disk in the machine. What is claimed here is that two
/// capabilities are jointly sufficient and separately necessary, and the way to claim it is to
/// withhold each one from the same binary, on the same disk, with the same budget and the same
/// stack, and then read the disk.
///
/// The entropy half is the one worth reading twice. A GPT partition and a RedoxFS volume each carry
/// an identifier that must be globally unique, and neither `crates/gpt` nor a `no_std` RedoxFS has
/// any randomness: the crate refuses to invent a GUID (notes/gpt.md) and the engine's `Header::new`
/// is std-gated because it calls `getrandom` (vendor/README.md divergence 4). So "no entropy
/// endpoint" is not a policy this code enforces. It is something the programs genuinely cannot do,
/// and what is under test is that they say so rather than making a value up.
#[test_case]
fn the_write_half_needs_a_disk_and_an_entropy_endpoint_and_holds_nothing_else() {
    let Some(disk) = disk_service::blank_disk(fs_service::blk_server_image()) else {
        // No fifth mmio block device: this boot did not build the blank image. A fact about the
        // machine, not a failure (milestone 145: reported as skipped rather than a silent "ok").
        crate::testing::skip!("no blank-disk fixture attached (this boot did not build one)");
    };
    if let Some(ready) = disk.blk_ready {
        // Past device bring-up, so a hang below is a hang in a write rather than in the driver.
        let [word, ..] = crate::sched::ipc_recv(ready);
        assert_eq!(word, fs_proto::fixture::READY);
    }
    let Some(entropy) = entropy_endpoint() else {
        crate::testing::skip!("no virtio-rng device on the mmio bus (NIFE_RNG not set?)");
    };
    let maker = program("mkfs").expect("no mkfs program in the initrd archive");
    use fs_proto::fixture::blank;

    // 1. The partitioner, with the disk and NO entropy endpoint. It must refuse, and it must refuse
    //    before writing anything, which is why the program draws all four ids first and lays out
    //    the table second.
    let report = disk_service::start_partitioner(partitioner_image(), &disk, ROLE_PARTITION, None);
    let [verdict, ..] = crate::sched::ipc_recv(report);
    assert_eq!(
        verdict, P_NO_ENTROPY,
        "a partitioner with no entropy endpoint reported {verdict:#x}; it must refuse rather than \
         invent a GUID, because an identifier that is not random is not unique",
    );

    // 2. And the disk still has no table on it, which is what makes step 1 mean something: "it
    //    reported a refusal" and "it wrote nothing" are different claims.
    let report = disk_service::start_partitioner(partitioner_image(), &disk, ROLE_VERIFY, None);
    let [flags, partitions, ..] = crate::sched::ipc_recv(report);
    assert_eq!(
        (flags, partitions),
        (0, 0),
        "the refused partitioner left something on the disk (flags {flags:#x})",
    );

    // 3. The same binary, the same role, the same disk, one capability more.
    let report =
        disk_service::start_partitioner(partitioner_image(), &disk, ROLE_PARTITION, Some(entropy));
    let [verdict, written, step, ..] = crate::sched::ipc_recv(report);
    assert_eq!(
        verdict, R_PARTITIONED,
        "the partitioner failed at step {written} ({step})",
    );
    assert_eq!(written, blank::PARTITIONS as u64);

    // And the table is really there, read back by a process that cannot write: both copies, their
    // CRCs, the protective MBR, the names, and three distinct version-4 GUIDs.
    let report = disk_service::start_partitioner(partitioner_image(), &disk, ROLE_VERIFY, None);
    let [flags, partitions, data_lba, ..] = crate::sched::ipc_recv(report);
    for (bit, what) in [
        (PF_MBR, "LBA 0 holds a protective MBR covering the disk"),
        (PF_PRIMARY, "the primary header and entry array parsed"),
        (PF_BACKUP, "the backup table agrees with the primary"),
        (PF_NIFE, "a nife data partition is on the disk"),
        (PF_NAMES, "every partition name decoded and matched"),
        (
            PF_UNIQUE,
            "every unique GUID is distinct, non-zero and version 4",
        ),
    ] {
        assert!(flags & bit != 0, "{what} (flags {flags:#x})");
    }
    assert_eq!(partitions, blank::PARTITIONS as u64);
    assert_eq!(
        data_lba,
        blank::DATA.0,
        "the data partition starts where it was placed",
    );

    // 4. `mkfs` with the disk and no entropy. The same refusal one layer up: a filesystem's uuid has
    //    the GPT's problem, and the engine now takes it as an argument for exactly this reason.
    let report = disk_service::start_maker(maker, &disk, ROLE_MAKE, None, true);
    let [verdict, ..] = crate::sched::ipc_recv(report);
    assert_eq!(
        verdict, M_NO_ENTROPY,
        "mkfs with no entropy endpoint reported {verdict:#x}; it must refuse",
    );

    // 5. And with entropy and no disk: nothing to read a table off, nothing to write to.
    let report = disk_service::start_maker(maker, &disk, ROLE_MAKE, Some(entropy), false);
    let [verdict, ..] = crate::sched::ipc_recv(report);
    assert_eq!(
        verdict, M_NO_DISK,
        "mkfs with no block endpoint reported {verdict:#x}; there is nothing it can write to",
    );

    // 6. Neither refusal created anything. A partition that has been *described* is not a partition
    //    that has been *formatted*, and this is where that difference is read off the platter.
    let report = disk_service::start_maker(maker, &disk, ROLE_CHECK, None, true);
    let [verdict, ..] = crate::sched::ipc_recv(report);
    assert_eq!(
        verdict, R_NO_FS,
        "something put a filesystem in the partition before any run that was allowed to",
    );

    // 7. Both capabilities, and they are the whole authority needed to create a filesystem.
    let report = disk_service::start_maker(maker, &disk, ROLE_MAKE, Some(entropy), true);
    let [verdict, blocks, first, ..] = crate::sched::ipc_recv(report);
    assert_eq!(
        verdict, R_MADE,
        "mkfs failed at step {blocks} with errno {first}",
    );
    let want_blocks =
        (blank::DATA.1 - blank::DATA.0 + 1) * blank::LBA / fs_proto::blk::BLOCK_SIZE as u64;
    assert_eq!(blocks, want_blocks, "the filesystem covers the partition");
    assert_eq!(
        first,
        blank::DATA.0 * blank::LBA / fs_proto::blk::BLOCK_SIZE as u64,
        "and starts at the partition's first block",
    );

    // 8. And it is a filesystem a *different* process can open, holding the file that run wrote.
    //    The host tool reads the same image from outside the guest after the run, which is the half
    //    a cache cannot fake (`cargo xtask test`'s blank-image check).
    let report = disk_service::start_maker(maker, &disk, ROLE_CHECK, None, true);
    let [verdict, n, ..] = crate::sched::ipc_recv(report);
    assert_eq!(
        verdict, R_FOUND,
        "the filesystem mkfs created does not hold the file it wrote",
    );
    assert_eq!(n, blank::MADE_BODY.len() as u64);
}
