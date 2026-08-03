use abi::fault::{EVENT_EXIT, EVENT_FAULT, FAULT_EP_SLOT};

use super::*;
use crate::sched;

pub(super) const CODE_VA: u64 = 0x40_0000;
pub(super) const STACK_VA: u64 = 0x50_0000;
/// The unmapped address the fault stub loads from. Distinctive, so the delivered fault address
/// proves the message carries real fault-time state and not a zero placeholder.
const BAD_ADDR: u64 = 0x00A5_0000;
/// The word the report stub SENDs, so a test can tell "the child ran" from "the child faulted."
pub(super) const REPORT_WORD: u64 = 0x42;

/// A child that faults on its very first memory access: load from [`BAD_ADDR`], which nothing
/// maps. Two instructions; the faulting one is the second, so the reported pc is `CODE_VA + 4`.
#[cfg(target_arch = "aarch64")]
pub(super) const FAULT_STUB: &[u32] = &[
    0xD2A0_14A0, // movz x0, #0xA5, lsl #16   (x0 = 0x00A5_0000)
    0xF940_0001, // ldr  x1, [x0]             (data abort: nothing maps BAD_ADDR)
];
#[cfg(target_arch = "riscv64")]
pub(super) const FAULT_STUB: &[u32] = &[
    0x00A5_0537, // lui a0, 0xA50             (a0 = 0x00A5_0000)
    0x0005_3583, // ld  a1, 0(a0)             (load page fault: nothing maps BAD_ADDR)
];

/// A child that SENDs [`REPORT_WORD`] on the endpoint in slot 0, then exits cleanly. The same
/// nine-instruction shape the region-reclaim tests use, so "it ran" is the SEND arriving.
#[cfg(target_arch = "aarch64")]
pub(super) const REPORT_STUB: &[u32] = &[
    0xD280_0000,                                       // movz x0, #0            (slot 0)
    0xD280_0001,                                       // movz x1, #0            (endpoint::SEND)
    0xD280_0000 | ((REPORT_WORD as u32) << 5) | 2,     // movz x2, #REPORT_WORD
    0xD280_0003,                                       // movz x3, #0
    0xD280_0004,                                       // movz x4, #0
    0xD280_0000 | ((abi::SYS_INVOKE as u32) << 5) | 8, // movz x8, #SYS_INVOKE
    0xD400_0001,                                       // svc #0                 (SEND)
    0xD280_0008,                                       // movz x8, #0            (SYS_EXIT)
    0xD400_0001,                                       // svc #0                 (exit)
];
#[cfg(target_arch = "riscv64")]
pub(super) const REPORT_STUB: &[u32] = &[
    0x0000_0513,                                    // li a0, 0            (slot 0)
    0x0000_0593,                                    // li a1, 0            (endpoint::SEND)
    0x0000_0613 | ((REPORT_WORD as u32) << 20),     // li a2, REPORT_WORD
    0x0000_0693,                                    // li a3, 0
    0x0000_0713,                                    // li a4, 0
    0x0000_0893 | ((abi::SYS_INVOKE as u32) << 20), // li a7, SYS_INVOKE
    0x0000_0073,                                    // ecall               (SEND)
    0x0000_0893 | ((abi::SYS_EXIT as u32) << 20),   // li a7, SYS_EXIT
    0x0000_0073,                                    // ecall               (exit)
];

/// Build a child from `stub` with its whole world in one region (aspace, code, stack, TCB), so a
/// single `DESTROY` reclaims it. `report` goes in slot 0 (what the report stub SENDs on);
/// `fault_ep`, if given, goes in the reserved fault slot, so `START` records it as the child's
/// supervision endpoint. Returns `(child_tid, region)`.
fn build_child(
    stub: &[u32],
    report: Option<sched::EpId>,
    fault_ep: Option<sched::EpId>,
) -> (u64, u64) {
    let region = crate::untyped::create(16).expect("no region for the child");
    (build_child_in(region, stub, report, fault_ep), region)
}

/// [`build_child`], but into a region the caller already owns. The reap tests need this: §32's
/// property is that the reclaimed pages go back to **the builder's** budget, which can only be
/// observed if the builder's region is one the test still holds and can measure.
pub(super) fn build_child_in(
    region: u64,
    stub: &[u32],
    report: Option<sched::EpId>,
    fault_ep: Option<sched::EpId>,
) -> u64 {
    let aspace = user_aspace_create(region).expect("no aspace");

    let code_phys = crate::untyped::retype_page(region).expect("no code frame");
    // SAFETY: a fresh frame we own, direct-mapped; write the stub and make it fetchable.
    unsafe {
        let dst = mmu::phys_to_virt(code_phys) as *mut u32;
        for (i, &insn) in stub.iter().enumerate() {
            dst.add(i).write(insn);
        }
    }
    sync_icache(mmu::phys_to_virt(code_phys), core::mem::size_of_val(stub));
    user_aspace_map(aspace, CODE_VA, code_phys, Flags::user_code()).expect("map code");

    let stack_phys = crate::untyped::retype_page(region).expect("no stack frame");
    user_aspace_map(aspace, STACK_VA, stack_phys, Flags::user_data()).expect("map stack");

    let tid = sched::create_tcb(region).expect("no tcb");
    if let Some(rep) = report {
        let cap = crate::cap::endpoint_cap(
            rep,
            crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
        );
        let slot = sched::tcb_insert_cap(tid, cap, None).expect("insert report");
        assert_eq!(
            slot, 0,
            "the report cap must land in slot 0 (the stub assumes it)"
        );
    }
    if let Some(fe) = fault_ep {
        // The spawn-slot convention: the supervision endpoint goes in the reserved fault slot.
        // Rights do not matter here (the kernel reads only the endpoint name and consumes the
        // slot at START, so the child cannot forge fault messages on it); READ is the minimum.
        let cap = crate::cap::endpoint_cap(fe, crate::cap::Rights::READ);
        sched::tcb_insert_cap(tid, cap, Some(FAULT_EP_SLOT)).expect("insert fault ep");
    }
    sched::configure_tcb(tid, CODE_VA, STACK_VA + frames::FRAME_SIZE, aspace).expect("configure");
    sched::start_tcb(tid, [0; 3]).expect("start");
    tid
}

/// **A crash becomes a message; the corpse survives until reaped; a fresh child runs.** The whole
/// supervision cycle in one test: spawn a child holding a fault endpoint, let it crash, receive
/// the fault message with the right tid and fault address, confirm the corpse still holds its
/// fault-time state (dead until reaped), reap it with revocation, and respawn a child that runs.
#[test_case]
fn a_faulting_child_reports_to_its_supervisor_and_is_reaped_then_respawned() {
    let fault_ep = sched::create_endpoint();
    let (child, region) = build_child(FAULT_STUB, None, Some(fault_ep));

    // The child faults on its first load. Its death arrives here, kernel-stamped.
    let msg = sched::ipc_recv(fault_ep);
    assert_eq!(msg[0], EVENT_FAULT, "a crash must report as a FAULT event");
    assert_eq!(msg[1], child, "the fault message named the wrong thread");
    assert_eq!(
        msg[2],
        CODE_VA + 4,
        "the faulting pc was not the load instruction"
    );
    assert_eq!(
        msg[3], BAD_ADDR,
        "the faulting address was not carried in the message"
    );

    // Dead until reaped: the corpse is still in the table, still holding its fault-time state,
    // and it never runs again. This is what makes postmortem (and a future resume) possible.
    assert_eq!(
        sched::corpse_fault_msg(child),
        Some(msg),
        "the corpse did not retain its fault message: it was reaped too early, or lost its state",
    );

    // Reap it with §16 revocation, the supervisor's explicit act. The corpse is Dead, not live,
    // so the region reclaims without a force-kill.
    sched::reclaim_region(region).expect("reaping the corpse's region failed");
    assert_eq!(
        sched::corpse_fault_msg(child),
        None,
        "the corpse outlived its region: revocation did not reap it",
    );

    // Respawn: a fresh child, in a fresh region, runs to completion where the crashed one died.
    let report = sched::create_endpoint();
    let (_c2, region2) = build_child(REPORT_STUB, Some(report), None);
    assert_eq!(
        sched::ipc_recv(report)[0],
        REPORT_WORD,
        "the respawned child never ran: the supervision cycle did not recover",
    );
    // The respawn exits unsupervised, so it is reaped by the scheduler; reclaim once it is gone.
    for _ in 0..2000 {
        if sched::reclaim_region(region2).is_ok() {
            break;
        }
        sched::yield_now();
    }
}

/// **A clean exit flows too, distinguished by the event code.** The other half of §26's "both
/// faults and exits": a supervised child that SENDs its word and exits normally reports an EXIT
/// event (not FAULT), with no fault pc or address, so a restart policy can tell "finished" from
/// "crashed."
#[test_case]
fn a_clean_exit_reports_the_exit_event_not_a_fault() {
    let report = sched::create_endpoint();
    let fault_ep = sched::create_endpoint();
    let (child, region) = build_child(REPORT_STUB, Some(report), Some(fault_ep));

    // It runs (the SEND proves it reached EL0), then exits cleanly.
    assert_eq!(
        sched::ipc_recv(report)[0],
        REPORT_WORD,
        "the child never ran before exiting",
    );
    let msg = sched::ipc_recv(fault_ep);
    assert_eq!(
        msg[0], EVENT_EXIT,
        "a clean exit must report EXIT, not FAULT"
    );
    assert_eq!(msg[1], child, "the exit message named the wrong thread");
    assert_eq!(msg[2], 0, "a clean exit has no faulting pc");
    assert_eq!(msg[3], 0, "a clean exit has no faulting address");

    // A cleanly-exited supervised child is dead until reaped, exactly like a crashed one.
    sched::reclaim_region(region).expect("reaping the exited corpse's region failed");
}
