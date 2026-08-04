//! **The block-device roster and the disk surveyor** (milestone 57, notes/block-devices.md).
//!
//! The kernel's whole part in "what drives are attached" is here, and it is deliberately two
//! separate acts:
//!
//! 1. **Write the roster.** One page, laid out by `block_roster`, listing every block device the
//!    kernel's own scans found, and mapped **read-only** into whichever program was granted it. It
//!    carries no capacity and no address, because a listing is not a handle.
//! 2. **Hand one device to one process.** The block server owns the DMA and the transport for
//!    exactly one disk (`fs_service::spawn_block_server`, unchanged); the surveyor holds an
//!    endpoint to it and a page they share.
//!
//! Those two authorities are what milestone 57 is about. `parted /dev/sda` as root holds both at
//! once and can reach any disk in the machine; here the program with the listing cannot open
//! anything from it, and the program with the disk was handed exactly one.
//!
//! The kernel never reads a partition table. It finds a device, confines it, and hands over
//! endpoints; every byte of GPT judgement happens in userspace, in a crate whose tests run on the
//! host against tables `sgdisk` and macOS `diskutil` wrote (notes/gpt.md).

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::*;
use crate::cap::{Rights, endpoint_cap, untyped_cap};
use crate::sched::EpId;

/// Which mmio block device the surveyor is given. The runners attach the GPT-partitioned image as
/// the FOURTH mmio disk, after crickerfs (0), RedoxFS (1) and the crash image (2); see
/// `scripts/qemu-runner-*.sh`, which explain why command-line order is reversed.
const GPT_DISK: usize = 3;

/// Which mmio block device the **write half** is given: the FIFTH, a 64 MiB image of zeros that the
/// runner regenerates every boot. Its own disk for milestone 37's reason (DECISIONS §27): a test
/// that writes a partition table and then a filesystem cannot be pointed at an image other tests
/// read, or every one of their results becomes a function of whether this one ran first.
const BLANK_DISK: usize = 4;

/// Where the surveyor maps the page it shares with its block server. Must match
/// `user/src/disk_surveyor.rs`.
const BLK_PAGE: u64 = 0x5000_0000;

/// Where the surveyor maps the roster, read-only. Must match `user/src/disk_surveyor.rs`.
///
/// Public because the negative-control probe reports this address back and then writes to it, and
/// the test asserts the fault landed exactly here. An attack on an address nobody uses would prove
/// nothing.
pub const ROSTER_VA: u64 = 0x5001_0000;

// The roles, in `a0`. Must match user/src/disk_surveyor.rs.
const ROLE_SURVEY: u64 = 0;
const ROLE_PROBE: u64 = 1;

/// Stack pages **below** the single one `run` maps, for the survey role.
///
/// A measurement rather than a guess, and the same lesson `spawn_fs_client` records: with none the
/// surveyor overflowed by about 200 bytes, which presented as a data abort on its own `sp` and then
/// as the 60-second lost-wakeup watchdog, because the test was still waiting for a report from a
/// process that had died. `Gpt::parse` walks 128 entries and a debug-build `Entry` is 128 bytes by
/// value; `check_backup` decodes a second header beside the first. Four pages is comfortably over
/// what it used and still small.
const SURVEY_EXTRA_STACK: usize = 4;

/// What the surveyor was wired with, so a test can take its reports.
pub struct Wiring {
    /// The surveyor's report endpoint: two messages, the roster summary then the table verdict.
    pub report: EpId,
    /// The block server's readiness endpoint, so a hang in device bring-up is distinguishable from
    /// a hang in the first read.
    pub ready: EpId,
    /// The roster's physical frame, so the kernel's own tests can read the same bytes the surveyor
    /// sees without holding a mapping of their own.
    pub roster_phys: u64,
    /// How many devices the kernel put in the roster.
    pub devices: usize,
}

/// Every block device the kernel can see, in the order the roster lists them.
///
/// mmio first, in slot order, then PCIe. That ordering is a promise to the roster's reader: an
/// mmio device's `ordinal` is the same number `virtio::find_block_device_n` counts by, so a listing
/// and a wiring cannot disagree about which disk is which.
fn devices() -> (
    [block_roster::Device; block_roster::capacity_of(FRAME_SIZE as usize)],
    usize,
) {
    let mut out = [block_roster::Device {
        ordinal: 0,
        transport: block_roster::TRANSPORT_MMIO,
    }; block_roster::capacity_of(FRAME_SIZE as usize)];
    let mut n = 0;

    for i in 0..crate::virtio::count_block_devices() {
        if n == out.len() {
            break;
        }
        out[n] = block_roster::Device {
            ordinal: i as u32,
            transport: block_roster::TRANSPORT_MMIO,
        };
        n += 1;
    }
    for i in 0..crate::pci::count_block_devices() {
        if n == out.len() {
            break;
        }
        out[n] = block_roster::Device {
            ordinal: i as u32,
            transport: block_roster::TRANSPORT_PCI,
        };
        n += 1;
    }
    (out, n)
}

/// Allocate and fill the roster page. Returns its physical frame and how many devices it names.
fn roster_page() -> (u64, usize) {
    let phys = crate::memory::alloc()
        .expect("no frame for the block-device roster")
        .addr();
    // SAFETY: a freshly allocated frame, reachable through the direct map, owned by nobody yet.
    let page = unsafe {
        core::slice::from_raw_parts_mut(mmu::phys_to_virt(phys) as *mut u8, FRAME_SIZE as usize)
    };
    page.fill(0);
    let (devs, n) = devices();
    block_roster::write(page, &devs[..n]).expect("the roster page sizes its own array");
    (phys, n)
}

/// Wire and spawn the surveyor over the GPT-partitioned test disk.
///
/// `None` when the machine has no fourth mmio block device, which is every boot the runner did not
/// build the GPT image for. That is a fact about the machine and not a failure: the caller skips.
pub fn start(blk_image: &'static [u8], surveyor_image: &'static [u8]) -> Option<Wiring> {
    let dev = crate::virtio::find_block_device_n(GPT_DISK)?;
    let (blk_ep, ready, blk_shared) = fs_service::spawn_block_server(blk_image, dev);
    let (roster_phys, devices) = roster_page();
    let report = crate::sched::create_endpoint();

    crate::sched::spawn(move || {
        let mut maps = [Mapping {
            va: BLK_PAGE,
            phys: blk_shared,
            flags: Flags::user_data(),
        }; 2 + SURVEY_EXTRA_STACK];
        // **Read-only, and that is the enumeration authority.** The surveyor may see what devices
        // exist and may not change the list, add itself a device, or turn an entry into a handle.
        // `start_probe` is the proof.
        maps[1] = Mapping {
            va: ROSTER_VA,
            phys: roster_phys,
            flags: Flags::user_rodata(),
        };
        for (k, m) in maps[2..].iter_mut().enumerate() {
            m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
            m.phys = crate::memory::alloc()
                .expect("no frame for the surveyor's stack")
                .addr();
        }
        run(
            surveyor_image,
            Spawn {
                arg0: ROLE_SURVEY,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(report, Rights::WRITE), // slot 0: the verdict
                    endpoint_cap(blk_ep, Rights::WRITE), // slot 1: ONE disk, and no way to name another
                ],
                maps: &maps,
            },
        )
    })
    .expect("could not spawn the disk surveyor");

    Some(Wiring {
        report,
        ready,
        roster_phys,
        devices,
    })
}

/// The negative control: the same binary, the same roster mapping, and no disk at all. It announces
/// the roster's address and writes to it.
///
/// It gets no block endpoint on purpose. The claim under test is about the *page*, and a process
/// holding a disk as well would leave open the reading that something about the disk mattered.
pub fn start_probe(surveyor_image: &'static [u8]) -> EpId {
    let (roster_phys, _) = roster_page();
    let report = crate::sched::create_endpoint();

    crate::sched::spawn(move || {
        run(
            surveyor_image,
            Spawn {
                arg0: ROLE_PROBE,
                arg1: 0,
                arg2: 0,
                grants: &[endpoint_cap(report, Rights::WRITE)],
                maps: &[Mapping {
                    va: ROSTER_VA,
                    phys: roster_phys,
                    flags: Flags::user_rodata(),
                }],
            },
        )
    })
    .expect("could not spawn the roster probe");

    report
}

// =============================================================================================
// Milestone 57's write half: partitioning and formatting the blank disk
// =============================================================================================

/// Stack pages **below** the single one `run` maps, for the partitioner.
///
/// The same four the surveyor needs and for the same measured reason: `Gpt::parse` walks 128
/// entries and a debug-build `Entry` is 128 bytes by value. The write path adds a 512-byte block of
/// scratch on top of that.
const PARTITION_EXTRA_STACK: usize = 4;

/// Stack pages for `fs_maker`. **The engine's number, not this program's**: RedoxFS recurses
/// through its tree and htree and commits transactions on the stack, and one page overflows on the
/// first `open`.
///
/// Half the FS server's 96, and taken from the same measurement rather than trimmed until it fit:
/// the instrumented high-water of a *full* server workload (mount, reads, writes, a create, two
/// truncates) is 135,824 bytes on the deeper of the two ISAs, which is 34 pages
/// (`fs_service::fs_stack_used`, notes/fs-server.md). 48 gives 40% headroom over that, and creating
/// one filesystem with one file in it walks strictly less of the tree than the workload measured.
///
/// It matters that this is not simply 96: a boot runs five of these processes and their stacks are
/// not freed when they die, so the difference is 1.5 MiB on a 128 MiB machine, and the first symptom
/// of spending it is an unrelated test failing to get a budget several tests later. That happened
/// with 96 (2026-08-03) and is why the number is here rather than borrowed.
const MAKER_STACK_PAGES: u64 = 48;

/// The untyped budget every `fs_maker` process draws its heap from, **including the two that are
/// meant to be refused**. Identical on purpose: the only thing that differs between the three runs
/// is the capability under test, so a refusal cannot be explained by a smaller budget.
///
/// 1.5 MiB is four times the 352 KiB high-water a real mount reaches under this allocator
/// (DECISIONS §27), and creation touches less than a mount. It is deliberately far below
/// `fs_service::FS_BUDGET_PAGES`, because an untyped is a **reservation**: three 8 MiB ones do not
/// fit beside everything else a test boot builds in 128 MiB, and the first symptom of trying is
/// init failing to get its own budget several tests later.
const MAKER_BUDGET_PAGES: u64 = 384;

/// The blank disk's block server, wired once per boot. A second server on the same device would
/// reset its queue out from under the first, so the partitioner, the verifier and `fs_maker` are
/// four processes sharing one; they run in sequence, which is what makes one shared page safe.
static BLANK_WIRED: AtomicBool = AtomicBool::new(false);
static BLANK_BLK_EP: AtomicU64 = AtomicU64::new(0);
static BLANK_SHARED: AtomicU64 = AtomicU64::new(0);

/// The untyped region the last `fs_maker` process was given, so the next one can hand it back
/// first. Zero means none yet. See [`start_maker`].
static LAST_BUDGET: AtomicU64 = AtomicU64::new(0);

/// The blank disk's block service: the endpoint clients `CALL`, the page they share with the
/// server, and (only for the caller that wired it) the server's readiness endpoint.
pub struct BlankDisk {
    pub blk_ep: EpId,
    pub blk_shared: u64,
    /// `None` for every caller after the first, because the readiness word is sent once.
    pub blk_ready: Option<EpId>,
}

/// Bring the blank disk up under a block server, or hand back the one already running.
///
/// `None` when the machine has no fifth mmio block device, which is every boot the runner did not
/// build the blank image for. A fact about the machine, so the caller skips.
pub fn blank_disk(blk_image: &'static [u8]) -> Option<BlankDisk> {
    if BLANK_WIRED.load(Ordering::Acquire) {
        return Some(BlankDisk {
            blk_ep: BLANK_BLK_EP.load(Ordering::Relaxed),
            blk_shared: BLANK_SHARED.load(Ordering::Relaxed),
            blk_ready: None,
        });
    }
    let dev = crate::virtio::find_block_device_n(BLANK_DISK)?;
    let (blk_ep, ready, blk_shared) = fs_service::spawn_block_server(blk_image, dev);
    BLANK_BLK_EP.store(blk_ep, Ordering::Relaxed);
    BLANK_SHARED.store(blk_shared, Ordering::Relaxed);
    BLANK_WIRED.store(true, Ordering::Release);
    Some(BlankDisk {
        blk_ep,
        blk_shared,
        blk_ready: Some(ready),
    })
}

/// Spawn the partitioner over the blank disk in `role`, with an entropy endpoint or **without one**.
///
/// `entropy` is the whole experiment. `Some(ep)` and the program can draw the unique ids a GPT
/// requires; `None` leaves slot 2 empty and its first `CALL` there comes back as a kernel error,
/// which `entropy_proto::delivered` reports as "no entropy" rather than as a short reply. Same
/// binary, same role, same disk, same everything else.
pub fn start_partitioner(
    image: &'static [u8],
    disk: &BlankDisk,
    role: u64,
    entropy: Option<EpId>,
) -> EpId {
    let report = crate::sched::create_endpoint();
    let blk_ep = disk.blk_ep;
    let blk_shared = disk.blk_shared;
    // Only read when `entropy` is `Some`; the endpoint id is copied out here so the closure below
    // captures a plain `u64` rather than the `Option` twice.
    let ep = entropy.unwrap_or_default();

    crate::sched::spawn(move || {
        let mut maps = [Mapping {
            va: BLK_PAGE,
            phys: blk_shared,
            flags: Flags::user_data(),
        }; 1 + PARTITION_EXTRA_STACK];
        for (k, m) in maps[1..].iter_mut().enumerate() {
            m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
            m.phys = crate::memory::alloc()
                .expect("no frame for the partitioner's stack")
                .addr();
        }
        // **The grant list IS the experiment**: three entries or two, and nothing else differs.
        // The entropy endpoint is last precisely so that withholding it is an absent slot rather
        // than a renumbering, which is what makes the two runs the same program.
        let with = [
            endpoint_cap(report, Rights::WRITE), // slot 0: the verdict
            endpoint_cap(blk_ep, Rights::WRITE), // slot 1: ONE disk, and no way to name another
            endpoint_cap(ep, Rights::WRITE),     // slot 2: randomness, and no way to reach the RNG
        ];
        let grants = match entropy {
            Some(_) => &with[..],
            None => &with[..2],
        };
        run(
            image,
            Spawn {
                arg0: role,
                arg1: 0,
                arg2: 0,
                grants,
                maps: &maps,
            },
        )
    })
    .expect("could not spawn the disk partitioner");

    report
}

/// Spawn `fs_maker` over the blank disk, with or without either half of the pair it needs.
///
/// Three wirings, one binary: both capabilities, no entropy, no disk. The budget, the stack and the
/// shared page are identical in all three, so the only thing that can explain a refusal is the
/// capability that is missing.
pub fn start_maker(
    image: &'static [u8],
    disk: &BlankDisk,
    role: u64,
    entropy: Option<EpId>,
    with_disk: bool,
) -> EpId {
    let report = crate::sched::create_endpoint();
    // **Give the previous run's reservation back before taking another.** An untyped is a
    // reservation rather than a cap, and this test starts five of these processes; holding all five
    // at once costs 7.5 MiB of a 128 MiB machine and the symptom is an unrelated test failing to get
    // its own budget much later ("no building budget for init", 2026-08-03). The previous process
    // reported and exited a whole round trip ago, so `destroy` revokes nothing that is live; a
    // region it refuses (something still pinned) is simply left alone, which is the old behaviour.
    let previous = LAST_BUDGET.swap(0, Ordering::Relaxed);
    if previous != 0 {
        crate::untyped::destroy(previous);
    }
    let budget = crate::untyped::create(MAKER_BUDGET_PAGES).expect("no heap budget for fs_maker");
    LAST_BUDGET.store(budget, Ordering::Relaxed);
    let blk_ep = disk.blk_ep;
    let blk_shared = disk.blk_shared;
    let has_entropy = entropy.is_some();
    let ep = entropy.unwrap_or_default();
    let mut stack = [0u64; MAKER_STACK_PAGES as usize];
    for f in stack.iter_mut() {
        *f = crate::memory::alloc()
            .expect("no frame for the fs_maker stack")
            .addr();
    }

    crate::sched::spawn(move || {
        let mut maps = [Mapping {
            va: BLK_PAGE,
            phys: blk_shared,
            flags: Flags::user_data(),
        }; 1 + MAKER_STACK_PAGES as usize];
        for (i, &phys) in stack.iter().enumerate() {
            maps[1 + i] = Mapping {
                va: USER_STACK_VA - (i as u64 + 1) * FRAME_SIZE,
                phys,
                flags: Flags::user_data(),
            };
        }
        // **Every slot placed explicitly, because the HOLE is the experiment.** A missing
        // capability here is not the last entry of a shorter list (the disk is slot 1 and the
        // verdict is slot 3), so `run`'s fill-in-order grants cannot express it: they would
        // renumber everything above the gap and the program would find its report endpoint where
        // it looked for a disk. `grant_at` leaves the slot genuinely empty, which is what the
        // program's first `CALL` there meets, and it is the same move `std_service` makes for a
        // std program that holds a directory and no network (notes/abi.md §4).
        crate::sched::grant_at(0, untyped_cap(budget)).expect("fs_maker slot 0 was occupied");
        if with_disk {
            crate::sched::grant_at(1, endpoint_cap(blk_ep, Rights::WRITE))
                .expect("fs_maker slot 1 was occupied");
        }
        if has_entropy {
            crate::sched::grant_at(2, endpoint_cap(ep, Rights::WRITE))
                .expect("fs_maker slot 2 was occupied");
        }
        crate::sched::grant_at(3, endpoint_cap(report, Rights::WRITE))
            .expect("fs_maker slot 3 was occupied");
        run(
            image,
            Spawn {
                arg0: role,
                arg1: 0,
                arg2: 0,
                grants: &[], // every one of them is placed above, at its own slot
                maps: &maps,
            },
        )
    })
    .expect("could not spawn fs_maker");

    report
}

impl Wiring {
    /// The roster as the kernel sees it, through the direct map. The surveyor holds a read-only
    /// mapping of the same frame; the layout comes from the one contract crate, so both sides are
    /// reading the same bytes with the same code.
    pub fn roster(&self) -> &'static [u8] {
        // SAFETY: a frame this module allocated and still owns, named through the direct map.
        unsafe {
            core::slice::from_raw_parts(
                mmu::phys_to_virt(self.roster_phys) as *const u8,
                FRAME_SIZE as usize,
            )
        }
    }
}
