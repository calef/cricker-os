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

use super::*;
use crate::cap::{Rights, endpoint_cap};
use crate::sched::EpId;

/// Which mmio block device the surveyor is given. The runners attach the GPT-partitioned image as
/// the FOURTH mmio disk, after crickerfs (0), RedoxFS (1) and the crash image (2); see
/// `scripts/qemu-runner-*.sh`, which explain why command-line order is reversed.
const GPT_DISK: usize = 3;

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
