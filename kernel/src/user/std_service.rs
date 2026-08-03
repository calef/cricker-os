use super::*;
use crate::cap::{Rights, endpoint_cap, frame_cap, untyped_cap};
use crate::sched::EpId;

/// Where the loader maps the clock page for a std program. Must match the std PAL's
/// `rt::CLOCK_PAGE`, and the slot must match its `rt::CLOCK_SLOT`.
const CLOCK_PAGE_STD: u64 = 0x1200_0000;
const CLOCK_SLOT: u64 = 5;

/// The entropy service's request endpoint (milestone 56). Must match the std PAL's
/// `rt::ENTROPY_SLOT`. **An endpoint, and no mapping**: unlike the clock, whose read authority
/// IS a page, randomness is obtained by asking, so the whole grant is one endpoint that names
/// no device.
const ENTROPY_SLOT: u64 = 6;

/// The heap high-water for the demo's Vec/String/HashMap workout plus std's own runtime
/// allocations and the heap's page tables is well under 1 MiB; 256 pages is comfortable, and
/// the initial region only needs to be contiguous at spawn, when memory is unfragmented.
pub const BUDGET_PAGES: u64 = 256;

/// std's startup, formatting machinery, and collection code use far more stack than a
/// hand-written `no_std` worker. `load` maps one stack page; map 32 more below it (128 KiB
/// total), generous so a stack-depth surprise is not what a first std bring-up debugs.
const EXTRA_STACK_PAGES: u64 = 32;

pub fn start(
    image: &'static [u8],
    clock_image: &'static [u8],
    entropy_image: &'static [u8],
) -> EpId {
    let report = crate::sched::create_endpoint();
    start_on(image, clock_image, entropy_image, report);
    report
}

/// The same spawn, with **the output sink chosen by the caller** (milestone 50).
///
/// This split is the milestone's finding expressed as a function signature: everything about a
/// std program's wiring is fixed except one endpoint capability, and putting a different one in
/// slot 1 is the whole of redirection. The program is not told, cannot ask, and the two callers
/// of this function hand it an endpoint the kernel receives on and an endpoint a file sink
/// receives on. See `sink_tests`.
pub fn start_on(
    image: &'static [u8],
    clock_image: &'static [u8],
    entropy_image: &'static [u8],
    report: EpId,
) {
    let budget = crate::untyped::create(BUDGET_PAGES).expect("no untyped for std_exerciser");

    // The entropy service, wired once per boot and shared with the milestone-56 tests. Its
    // request endpoint is the whole of a std program's randomness authority: `SystemRng` is a
    // `CALL` on it, and nothing about it reaches the device (DECISIONS §44).
    let entropy = entropy_service::ensure(entropy_image, entropy_service::Bus::Mmio)
        .expect("no virtio-rng device for the std program (is CRICKER_RNG set on this leg?)");
    if let Some(ready) = entropy.ready {
        let report = crate::sched::ipc_recv(ready);
        assert_eq!(
            report[0],
            entropy_proto::READY,
            "the entropy service did not come up for the std program (it reported {:#x})",
            report[0],
        );
    }

    // The clock first, and its startup report taken before the program starts, so the offset is
    // published by the time std reads the page. Waiting is not a synchronisation trick, it is
    // the honest order: a std program that started first would see `state::UNKNOWN` and be
    // correct to say so.
    let clock = clock_service::start(clock_image);
    let _ = crate::sched::ipc_recv(clock.report);

    // The clock page read-only, then the deep stack std needs.
    let mut maps = [Mapping {
        va: 0,
        phys: 0,
        flags: Flags::user_data(),
    }; 1 + EXTRA_STACK_PAGES as usize];
    maps[0] = Mapping {
        va: CLOCK_PAGE_STD,
        phys: clock.page_phys,
        flags: Flags::user_rodata(), // a READER, and the mapping is what says so
    };
    for (k, m) in maps[1..].iter_mut().enumerate() {
        let phys = crate::memory::alloc()
            .expect("no frame for std_exerciser stack")
            .addr();
        // SAFETY: fresh frame via the direct map; zero it so the new process starts clean.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
        }
        m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
        m.phys = phys;
    }

    crate::sched::spawn(move || {
        // The clock and entropy capabilities go in at their named slots BEFORE `run` grants in
        // order, so `run`'s two grants land at 0 and 1 and slots 2 to 4 stay empty. The clock
        // is `READ` only: the whole point is that a reader cannot write the offset. See
        // `grant_at`.
        crate::sched::grant_at(CLOCK_SLOT, frame_cap(clock.page_phys, Rights::READ))
            .expect("the std clock slot was already occupied");
        crate::sched::grant_at(ENTROPY_SLOT, endpoint_cap(entropy.request, Rights::WRITE))
            .expect("the std entropy slot was already occupied");
        run(
            image,
            Spawn {
                arg0: 0,
                arg1: 0,
                arg2: 0,
                grants: &[
                    untyped_cap(budget),                 // slot 0: the heap's budget
                    endpoint_cap(report, Rights::WRITE), // slot 1: stdout/stderr
                ],
                maps: &maps,
            },
        )
    })
    .expect("could not spawn std_exerciser");
}
