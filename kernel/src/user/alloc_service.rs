use super::*;
use crate::cap::{Rights, endpoint_cap, untyped_cap};
use crate::sched::EpId;

/// The demo caps its own heap at 64 pages; the budget must also cover the program's page
/// tables and the heap's, so 96 pages is comfortable without being unbounded.
pub const BUDGET_PAGES: u64 = 96;

/// `load` maps one stack page, which suits the hand-sized programs; `alloc` collections
/// (`BTreeMap` nodes, the fmt machinery behind `assert!`) burn more than 4 KiB of stack, so
/// map three more pages below it. The demo found this the honest way: a data abort at
/// 0x4ffff8, one word below the mapped page.
const EXTRA_STACK_PAGES: u64 = 3;

pub fn start(image: &'static [u8]) -> EpId {
    let budget = crate::untyped::create(BUDGET_PAGES).expect("no untyped for allocator_exerciser");
    let report = crate::sched::create_endpoint();

    let mut stack = [Mapping {
        va: 0,
        phys: 0,
        flags: Flags::user_data(),
    }; EXTRA_STACK_PAGES as usize];
    for (k, m) in stack.iter_mut().enumerate() {
        let phys = crate::memory::alloc()
            .expect("no frame for allocator_exerciser stack")
            .addr();
        // SAFETY: fresh frame via the direct map; zero it so the new process starts clean.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
        }
        m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
        m.phys = phys;
    }

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: 0,
                arg1: 0,
                arg2: 0,
                grants: &[
                    untyped_cap(budget),                 // slot 0: the heap's budget
                    endpoint_cap(report, Rights::WRITE), // slot 1: report the verdict
                ],
                maps: &stack,
            },
        )
    })
    .expect("could not spawn allocator_exerciser");

    report
}
