use super::*;
use crate::sched;

const CODE_VA: u64 = 0x40_0000;
const STACK_VA: u64 = 0x50_0000;

/// A one-instruction runaway: branch (aarch64) or jump (riscv) to self, forever. It never
/// yields, never syscalls, never touches an endpoint, so nothing cooperative can end it and the
/// forcible tier is the only thing that can.
#[cfg(target_arch = "aarch64")]
const SPIN_STUB: &[u32] = &[0x1400_0000]; // b .
#[cfg(target_arch = "riscv64")]
const SPIN_STUB: &[u32] = &[0x0000_006F]; // j .  (jal x0, 0)

/// Build a runaway from parts (aspace, code, stack, TCB all in one region), start it, then
/// reclaim its region while it still spins, and assert the region comes back whole.
#[test_case]
fn destroy_force_kills_a_runaway_and_reclaims_its_region() {
    let frames_before = crate::memory::free_frames();
    let threads_before = sched::thread_count();

    // The runaway's whole world in one region: the address space's root and tables, its code
    // page, its stack, and its TCB, so a single `DESTROY` reclaims all of it.
    let region = crate::untyped::create(16).expect("no region for the runaway");
    let aspace = user_aspace_create(region).expect("no aspace");

    let code_phys = crate::untyped::retype_page(region).expect("no code frame");
    // SAFETY: a fresh frame we own, direct-mapped; write the spin loop and make it fetchable.
    unsafe {
        let dst = mmu::phys_to_virt(code_phys) as *mut u32;
        for (i, &insn) in SPIN_STUB.iter().enumerate() {
            dst.add(i).write(insn);
        }
    }
    sync_icache(
        mmu::phys_to_virt(code_phys),
        core::mem::size_of_val(SPIN_STUB),
    );
    user_aspace_map(aspace, CODE_VA, code_phys, Flags::user_code()).expect("map code");

    let stack_phys = crate::untyped::retype_page(region).expect("no stack frame");
    user_aspace_map(aspace, STACK_VA, stack_phys, Flags::user_data()).expect("map stack");

    let tid = sched::create_tcb(region).expect("no tcb");
    sched::configure_tcb(tid, CODE_VA, STACK_VA + frames::FRAME_SIZE, aspace).expect("configure");
    sched::start_tcb(tid, [0; 3]).expect("start");

    // Let the runaway actually reach EL0 and start spinning, so we tear down a running thread,
    // not an embryo. A few yields is plenty; it is preemptible the instant it lands.
    for _ in 0..8 {
        sched::yield_now();
    }

    // The forcible tier: reclaim the region while the runaway is still live. The first pass arms
    // the kill and refuses; the runaway is converted to a corpse at its next preemption; the
    // retry reclaims. The wait is time-based, not a fixed spin count, because since DECISIONS §28
    // the runaway may be placed on another core, where only that core's own timer tick converts
    // it (the kill is bounded by the tick, §28.3 / §16). A tight yield loop on this core would
    // finish inside one 10 ms tick and never give the remote core a chance; a one-second deadline
    // spans ~100 ticks, ample, while still failing a real bug rather than hanging the emulator.
    let deadline = crate::arch::timer::now() + crate::arch::timer::frequency();
    let mut reclaimed = false;
    while crate::arch::timer::now() < deadline {
        if sched::reclaim_region(region).is_ok() {
            reclaimed = true;
            break;
        }
        sched::yield_now();
    }
    assert!(
        reclaimed,
        "DESTROY never tore down a runaway: the killed flag did not convert it to a corpse",
    );

    assert!(
        sched::thread_count() <= threads_before,
        "the force-killed runaway was reclaimed but never actually reaped",
    );
    assert_eq!(
        crate::memory::free_frames(),
        frames_before,
        "reclaiming a force-killed runaway did not return its frames to baseline",
    );
}

/// A child that blocks in `RECV` on the endpoint in slot 0 and never comes back on its own. Nine
/// instructions, the [`super::supervision_tests::REPORT_STUB`] shape with `RECV` where the `SEND`
/// was: the tail `EXIT` is reached only if the receive returns, which happens exactly when the
/// endpoint is revoked out from under it.
#[cfg(target_arch = "aarch64")]
const RECV_STUB: &[u32] = &[
    0xD280_0000,                                           // movz x0, #0            (slot 0)
    0xD280_0000 | ((abi::endpoint::RECV as u32) << 5) | 1, // movz x1, #RECV
    0xD280_0002,                                           // movz x2, #0
    0xD280_0003,                                           // movz x3, #0
    0xD280_0004,                                           // movz x4, #0
    0xD280_0000 | ((abi::SYS_INVOKE as u32) << 5) | 8,     // movz x8, #SYS_INVOKE
    0xD400_0001,                                           // svc #0                 (RECV: blocks)
    0xD280_0008,                                           // movz x8, #SYS_EXIT
    0xD400_0001,                                           // svc #0                 (exit)
];
#[cfg(target_arch = "riscv64")]
const RECV_STUB: &[u32] = &[
    0x0000_0513,                                        // li a0, 0            (slot 0)
    0x0000_0593 | ((abi::endpoint::RECV as u32) << 20), // li a1, RECV
    0x0000_0613,                                        // li a2, 0
    0x0000_0693,                                        // li a3, 0
    0x0000_0713,                                        // li a4, 0
    0x0000_0893 | ((abi::SYS_INVOKE as u32) << 20),     // li a7, SYS_INVOKE
    0x0000_0073,                                        // ecall               (RECV: blocks)
    0x0000_0893 | ((abi::SYS_EXIT as u32) << 20),       // li a7, SYS_EXIT
    0x0000_0073,                                        // ecall               (exit)
];

/// **`DESTROY` reclaims a region whose resident is `Blocked`, not just one that spins.**
///
/// The companion to the test above and the property the aarch64 test boot actually needed. §16's
/// kill is *armed* by the refusal and *spent* by `schedule()`, and a thread parked in `RECV` never
/// reaches `schedule()`: before 2026-08-16 a region holding a blocked server was refused on every
/// pass, forever, and its memory was gone until the machine stopped. That is not an exotic case. A
/// server is a thing that blocks, and six `spawn_init` tests each built one out of a 2048-frame
/// budget, which is how the boot came to end with 216 free frames (notes/frames.md).
///
/// What makes the reclaim land is that the child's endpoint is **in the region being reclaimed**, so
/// the sweep that removes it aborts the child's IPC and wakes it, and the kill the same pass arms is
/// then spendable. `reap_region_objects` does that sweep before the refusal for exactly this reason.
///
/// **Verified it can fail**, and it was: moving the endpoint sweep back below the refusal in
/// `reap_region_objects` makes this test spend its whole two-second deadline and trip its own
/// assertion, "a region holding a resident blocked in RECV never reclaimed" (checked 2026-08-16).
#[test_case]
fn destroy_reclaims_a_region_whose_resident_is_blocked_in_recv() {
    let frames_before = crate::memory::free_frames();

    // The child's whole world in one region, its endpoint included: aspace, code, stack, TCB, and
    // the endpoint it will park on.
    let region = crate::untyped::create(16).expect("no region for the blocked child");
    let ep = sched::create_endpoint_from(region).expect("no endpoint in the child's region");

    let aspace = user_aspace_create(region).expect("no aspace");
    let code_phys = crate::untyped::retype_page(region).expect("no code frame");
    // SAFETY: a fresh frame we own, direct-mapped; write the stub and make it fetchable.
    unsafe {
        let dst = mmu::phys_to_virt(code_phys) as *mut u32;
        for (i, &insn) in RECV_STUB.iter().enumerate() {
            dst.add(i).write(insn);
        }
    }
    sync_icache(
        mmu::phys_to_virt(code_phys),
        core::mem::size_of_val(RECV_STUB),
    );
    user_aspace_map(aspace, CODE_VA, code_phys, Flags::user_code()).expect("map code");
    let stack_phys = crate::untyped::retype_page(region).expect("no stack frame");
    user_aspace_map(aspace, STACK_VA, stack_phys, Flags::user_data()).expect("map stack");

    let tid = sched::create_tcb(region).expect("no tcb");
    // READ, because the child receives on it. The rights are the point of not reusing
    // `supervision_tests::build_child_in`, which inserts a WRITE report cap.
    let slot = sched::tcb_insert_cap(
        tid,
        crate::cap::endpoint_cap(ep, crate::cap::Rights::READ),
        None,
    )
    .expect("insert the endpoint");
    assert_eq!(
        slot, 0,
        "the endpoint must land in slot 0 (the stub assumes it)"
    );
    sched::configure_tcb(tid, CODE_VA, STACK_VA + frames::FRAME_SIZE, aspace).expect("configure");
    sched::start_tcb(tid, [0; 3]).expect("start");

    // **Wait for it to be queued on the endpoint, not for "probably scheduled by now."** A test that
    // reclaims before the child has parked proves nothing at all: the child would still be Ready, and
    // the ordinary force-kill path above would carry it. See the same lesson in
    // `a_blocked_waiter_wakes_with_an_error_when_its_endpoint_is_revoked`.
    assert!(
        super::tests::wait_for(|| sched::endpoint_waiting_receivers(ep) == 1),
        "the child never blocked on its endpoint, so this test would prove the wrong thing",
    );

    // The retry loop, as `user::holding::Holding` runs it: the first pass sweeps the endpoint (which
    // wakes the child) and arms the kill; a later pass finds the corpse and reclaims.
    let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
    let mut reclaimed = false;
    while crate::arch::timer::now() < deadline {
        if sched::reclaim_region(region).is_ok() {
            reclaimed = true;
            break;
        }
        sched::yield_now();
    }
    assert!(
        reclaimed,
        "a region holding a resident blocked in RECV never reclaimed: the endpoint sweep no longer \
         runs before the refusal, so the armed kill can never be spent and the memory is gone for \
         the life of the boot",
    );
    assert!(
        super::tests::wait_for(|| !sched::thread_present(tid)),
        "the region reclaimed but its blocked resident was never reaped",
    );
    assert_eq!(
        crate::memory::free_frames(),
        frames_before,
        "reclaiming a blocked resident's region did not return its frames to baseline",
    );
}
