use super::*;
use crate::cap::{Rights, endpoint_cap};
use crate::sched::EpId;

/// Where the service expects its two mappings. Must match user/src/clock.rs.
///
/// `CLOCK_VA` is public because it is the address the **set** authority lives at, and milestone
/// 51's NTP tests aim a write there from a process that holds no such mapping. An attack on an
/// address nobody uses would prove nothing.
pub const CLOCK_VA: u64 = 0x00c0_0000;
const RTC_VA: u64 = 0x00d0_0000;

/// What the clock service was wired with, so a test (or a real init) can play its clients.
pub struct Wiring {
    /// The service's startup report.
    pub report: EpId,
    /// The propose endpoint. A holder of `WRITE` here may ask; it may not tell.
    pub propose: EpId,
    /// The clock page's **physical** frame, so a reader can be given a read-only mapping of it
    /// (and so the kernel's own tests can read it through the direct map).
    pub page_phys: u64,
    /// Which RTC the machine turned out to have, one of `clock_proto::rtc`.
    pub kind: u64,
}

/// Wire and spawn the clock service.
pub fn start(image: &'static [u8]) -> Wiring {
    let page_phys = crate::memory::alloc()
        .expect("no frame for the clock page")
        .addr();
    // Zeroed, which is also the honest starting state: a page nobody has published to reads as
    // `state::UNKNOWN` rather than as 1970 (clock_proto's `a_zeroed_page_reads_as_unknown`).
    // SAFETY: freshly allocated, reachable through the direct map, owned by nobody yet.
    unsafe {
        core::ptr::write_bytes(
            mmu::phys_to_virt(page_phys) as *mut u8,
            0,
            FRAME_SIZE as usize,
        );
    };

    let report = crate::sched::create_endpoint();
    let propose = crate::sched::create_endpoint();

    let rtc = crate::memory::rtc_region();
    let kind = rtc.map_or(clock_proto::rtc::NONE, |(_, _, k)| k);

    // Two mappings, or one on a machine with no RTC. The device page is mapped and no
    // `DeviceFrame` capability is granted, exactly as the console server's UART is: the service
    // drives the registers and never delegates them onward, and §41's take-back revocation
    // reaches an address-space mapping whether or not a cspace slot names it too.
    let mut maps = [
        Mapping {
            va: CLOCK_VA,
            phys: page_phys,
            flags: Flags::user_data(), // read/WRITE: the service is a setter
        },
        Mapping {
            va: RTC_VA,
            phys: 0,
            flags: Flags::user_device(),
        },
    ];
    let n_maps = match rtc {
        Some((phys, _, _)) => {
            maps[1].phys = phys;
            2
        }
        None => 1,
    };

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: kind, // which register layout the MACHINE has, not which ISA we are
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(propose, Rights::READ), // slot 0: serve proposals
                    endpoint_cap(report, Rights::WRITE), // slot 1: the startup verdict
                ],
                maps: &maps[..n_maps],
            },
        )
    })
    .expect("could not spawn the clock service");

    Wiring {
        report,
        propose,
        page_phys,
        kind,
    }
}

impl Wiring {
    /// The clock page as the kernel sees it, through the direct map. A reader process would
    /// hold a read-only mapping instead; the seqlock and the layout are the same either way,
    /// because they come from the one contract crate.
    pub fn page(&self) -> clock_proto::ClockPage {
        // SAFETY: a frame this module allocated and still owns, named through the direct map.
        unsafe { clock_proto::ClockPage::new(mmu::phys_to_virt(self.page_phys)) }
    }

    /// Wall-clock nanoseconds as a reader would compute them: the page's offset plus the
    /// ambient monotonic counter. 0 when the machine does not know.
    pub fn wall_nanos(&self) -> u64 {
        let r = self.page().read();
        if clock_proto::state::known(r.state) {
            clock_proto::wall_nanos(r.offset_nanos, monotonic_nanos())
        } else {
            0
        }
    }

    /// Play a proposer: `CALL` the propose endpoint. Returns `(status, wall_nanos_after)`.
    pub fn propose_nanos(&self, unix_nanos: u64) -> (u64, u64) {
        let r = crate::sched::ipc_call(
            self.propose,
            [
                clock_proto::propose::req(clock_proto::propose::PROPOSE),
                unix_nanos,
            ],
        );
        (r[0], r[1])
    }
}

/// Monotonic nanoseconds since boot, the kernel's copy of the reader's arithmetic. Split into
/// seconds and a remainder for the same reason the service's version is: the naive
/// `ticks * 1_000_000_000` overflows a `u64` a few minutes into a boot.
pub fn monotonic_nanos() -> u64 {
    let freq = crate::arch::timer::frequency();
    let ticks = crate::arch::timer::now();
    let secs = ticks / freq;
    let rem = ticks % freq;
    secs * clock_proto::NANOS_PER_SEC + rem * clock_proto::NANOS_PER_SEC / freq
}
