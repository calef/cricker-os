use abi::Error;
use abi::fault::EVENT_EXIT;

use super::supervision_tests::{REPORT_STUB, REPORT_WORD, build_child_in};
use super::*;
use crate::arch::exceptions::TrapFrame;
use crate::cap::Rights;
use crate::sched;
use crate::syscall::invoke;

/// Pages per job region: what `build_child_in` lays down (an address space and its tables, a code
/// page, a stack page, a TCB). Sixteen is the number every region-reclaim test here uses.
const JOB_REGION_PAGES: u64 = 16;

/// How many live jobs the pool holds. Small on purpose: a pool nobody could exhaust would make the
/// renewal claim below unfalsifiable, which is the whole reason the first test exists.
const ROOM: u64 = 3;
const POOL_PAGES: u64 = JOB_REGION_PAGES * ROOM;

/// How many jobs the reaped run puts through that pool. Four times its capacity, so the pass cannot
/// be explained by anything except the regions coming back.
const JOBS: u64 = 12;

/// How long to let the collector get to a corpse before calling it a failure. It is an ordinary
/// unprivileged thread on the run queue, so this is a scheduling wait and not a timing assertion:
/// the *claim* is about which budget the pages land in, and that is asserted exactly.
const YIELDS: u32 = 4000;

/// `invoke(cap, REAP, tid, _, _)` through the real dispatcher, for the tidy-up in the control.
fn reap(slot: u64, tid: u64) -> Result<i64, Error> {
    let mut frame = TrapFrame::for_user_entry(0, 0, [0, 0, 0]);
    invoke(&mut frame, slot, abi::endpoint::REAP, tid, 0, 0)
}

/// Receive one five-word death message through the ABI.
fn recv_death(slot: u64) -> [u64; 5] {
    let mut frame = TrapFrame::for_user_entry(0, 0, [0, 0, 0]);
    let w0 = invoke(&mut frame, slot, abi::endpoint::RECV, 0, 0, 0).expect("RECV refused");
    [
        w0 as u64,
        frame.arg(1),
        frame.arg(2),
        frame.arg(3),
        frame.arg(4),
    ]
}

/// The pages the pool has spent and not got back.
fn spent(pool: u64) -> u64 {
    crate::untyped::usage(pool).expect("the pool exists").0
}

/// Wait for the collector to bring the pool all the way back. `false` if it never does.
fn pool_came_back(pool: u64) -> bool {
    for _ in 0..YIELDS {
        if spent(pool) == 0 {
            return true;
        }
        sched::yield_now();
    }
    spent(pool) == 0
}

/// **Start the real `job_reaper` binary** the way the interactive init starts it: an ordinary user
/// process whose entire capability space is one endpoint with `READ`.
///
/// Deliberately the real program out of the initrd rather than a stub, because what is under test is
/// a *program's* behaviour, and a stub would be this test's opinion of it.
fn spawn_job_reaper(deaths: sched::EpId) -> u64 {
    let bytes = program("job_reaper").expect("no job_reaper program in the initrd archive");
    let (space, entry) = load(bytes).expect("job_reaper is not loadable");
    let aspace = readopt_user_aspace(space).expect("register the job_reaper aspace");

    let tcb_region = crate::untyped::create(2).expect("no tcb region for job_reaper");
    let tid = sched::create_tcb(tcb_region).expect("no tcb for job_reaper");
    let slot = sched::tcb_insert_cap(tid, crate::cap::endpoint_cap(deaths, Rights::READ), None)
        .expect("insert the supervision endpoint");
    assert_eq!(
        slot, 0,
        "job_reaper reads its supervision endpoint from slot 0",
    );
    sched::configure_tcb(tid, entry, USER_STACK_TOP, aspace).expect("configure job_reaper");
    sched::start_tcb(tid, [0; 3]).expect("start job_reaper");
    tid
}

/// **The control: without a collector, a bounded job pool runs out and stays out.**
///
/// Three jobs fit; the fourth is refused, and it is refused *after every one of the three has
/// finished running*, which is the whole point. A finished job's region is not free memory: its
/// corpse persists (dead until reaped, DECISIONS §26) and the pool's watermark only moves forward.
/// This is what the interactive prompt did before this increment, one command at a time, until the
/// shell started answering "could not spawn (init is out of memory)".
///
/// It is here because the renewal test below is unfalsifiable without it: a pool that could not run
/// out would pass that test whether or not anything was ever collected.
#[test_case]
fn without_a_collector_a_bounded_job_pool_runs_out() {
    let pool = crate::untyped::create(POOL_PAGES).expect("no job pool");
    let deaths = sched::create_endpoint();
    let report = sched::create_endpoint();

    let mut corpses = [0u64; ROOM as usize];
    for (i, slot) in corpses.iter_mut().enumerate() {
        let region = crate::untyped::split(pool, JOB_REGION_PAGES)
            .unwrap_or_else(|| panic!("the pool had no room for job {i}, and it should have"));
        *slot = build_child_in(region, REPORT_STUB, Some(report), Some(deaths));
        assert_eq!(
            sched::ipc_recv(report)[0],
            REPORT_WORD,
            "job {i} never ran, so this test is not measuring what it thinks",
        );
    }

    assert_eq!(
        spent(pool),
        POOL_PAGES,
        "the pool should be fully committed after {ROOM} jobs",
    );
    assert!(
        crate::untyped::split(pool, JOB_REGION_PAGES).is_none(),
        "a {ROOM}-job pool built a fourth job: this control proves nothing if the pool cannot run \
         out, and the renewal test below leans on it",
    );

    // Tidy: collect the three corpses the way the collector would, then give the pool back.
    let cap = sched::grant(crate::cap::endpoint_cap(deaths, Rights::READ)).expect("hold deaths");
    for _ in 0..ROOM {
        let msg = recv_death(cap);
        assert_eq!(msg[0], EVENT_EXIT, "the report stub exits cleanly");
        assert_eq!(reap(cap, msg[1]), Ok(0), "the tidy-up reap failed");
    }
    let _ = sched::delete_current_cap(cap);
    sched::reclaim_region(pool).expect("the job pool did not come back");
}

/// **The claim: a bounded job pool is enough, because `job_reaper` gives every region back.**
///
/// Twelve jobs through a pool with room for three. Each one is built in a region split off the pool
/// and born supervised (§26's spawn-slot convention); it runs, reports, and exits; and the pool comes
/// all the way back to zero before the next one is split. Nothing in this test collects anything: the
/// collecting is done by an unprivileged process holding one endpoint capability and no memory at
/// all, exactly as it is at the interactive prompt.
///
/// **Which budget the pages land in is the assertion, not that the corpse went away.** A reap that
/// quietly credited the collector, or that freed the corpse and stranded its memory, would leave the
/// pool committed and fail at the first `pool_came_back`. The last two assertions close the other
/// half: the returned pages are genuinely spendable again rather than un-bumped bookkeeping.
#[test_case]
fn job_reaper_returns_every_finished_job_to_the_pool() {
    let pool = crate::untyped::create(POOL_PAGES).expect("no job pool");
    let deaths = sched::create_endpoint();
    let report = sched::create_endpoint();
    spawn_job_reaper(deaths);

    for i in 0..JOBS {
        assert!(
            pool_came_back(pool),
            "before job {i} the pool was still {} pages down: the collector did not return the \
             previous job's region, so a bounded budget is not enough after all",
            spent(pool),
        );
        let region = crate::untyped::split(pool, JOB_REGION_PAGES).unwrap_or_else(|| {
            panic!(
                "job {i} could not be carved out of a pool that just \
                                       reported itself empty"
            )
        });
        build_child_in(region, REPORT_STUB, Some(report), Some(deaths));
        assert_eq!(sched::ipc_recv(report)[0], REPORT_WORD, "job {i} never ran",);
    }

    assert!(
        pool_came_back(pool),
        "the last job's region never came back",
    );
    assert!(
        !crate::untyped::has_children(pool),
        "a job region outlived its corpse, so the pool is still committed to it",
    );
    // Spendable, not merely counted: the whole pool carves again in one piece.
    let again = crate::untyped::split(pool, POOL_PAGES)
        .expect("the pool would not spend the pages the collector returned");
    sched::reclaim_region(again).expect("reclaim the re-split region");
    sched::reclaim_region(pool).expect("the job pool did not come back");
}
