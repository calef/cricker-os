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
