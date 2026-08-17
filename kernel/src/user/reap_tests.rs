use abi::Error;
use abi::fault::{EVENT_EXIT, EVENT_FAULT};

use super::supervision_tests::{FAULT_STUB, REPORT_STUB, REPORT_WORD, build_child_in};
use crate::arch::exceptions::TrapFrame;
use crate::cap::{Object, Rights};
use crate::sched;
use crate::syscall::invoke;

/// The builder's whole budget. Room for three instances plus slack, so the LIFO return-of-pages
/// (§16) can be watched happening more than once.
const BUILDER_BUDGET_PAGES: u64 = 80;

/// Pages per instance region: the child's address space (root and tables), its code page, its
/// stack page, and its TCB. Sixteen is what `build_child` has always carved.
const INSTANCE_PAGES: u64 = 16;

/// Pages for the test's own endpoints, one per endpoint (`RETYPE_OBJ`'s one-object-per-page
/// rule). Two is the most any one test here needs; four is slack.
const ENDPOINT_PAGES: u64 = 4;

/// The slot half of a generational name (`crates/slots` packs generation in the high 32 bits,
/// slot in the low 32). The recycled-tid test needs it to assert it is genuinely replaying a
/// name that now *aliases a live thread's slot*, which is the only version of that test worth
/// running.
const SLOT_MASK: u64 = 0xffff_ffff;

/// One test's world: a budget the *builder* owns, and a small region the test's endpoints are
/// retyped from. The endpoints come out of a reclaimable region rather than `create_endpoint`'s
/// shared kernel one so that `tidy` can give them back; six tests each leaking a couple of
/// endpoints exhausted the kernel's endpoint budget for every test that ran afterwards, which is
/// the sort of failure a test should not be able to inflict on its neighbours.
fn arena() -> (u64, u64) {
    let budget = crate::untyped::create(BUILDER_BUDGET_PAGES).expect("no builder budget");
    let endpoints = crate::untyped::create(ENDPOINT_PAGES).expect("no endpoint region");
    (budget, endpoints)
}

/// An endpoint out of the test's own region (one page each).
fn endpoint(region: u64) -> sched::EpId {
    sched::create_endpoint_from(region).expect("no endpoint")
}

/// Hold a supervision endpoint the way a supervisor holds one: `READ` alone, the right to
/// receive deaths here. No `WRITE` (it is not a sender on its own children's death channel) and
/// no `GRANT`.
fn hold_endpoint(ep: sched::EpId) -> u64 {
    sched::grant(crate::cap::endpoint_cap(ep, Rights::READ)).expect("grant the endpoint")
}

/// `invoke(cap, REAP, tid, _, _)`, through the real dispatcher.
fn reap(slot: u64, tid: u64) -> Result<i64, Error> {
    let mut frame = TrapFrame::for_user_entry(0, 0, [0, 0, 0]);
    invoke(&mut frame, slot, abi::endpoint::REAP, tid, 0, 0)
}

/// **`reap`, retried while the corpse is still standing on its own kernel stack.**
///
/// The refusal these tests race is transient and one context switch wide, and every test here is
/// built to lose it. `depart` publishes a supervised thread `Dead` and parks it on its supervision
/// endpoint *before* it reaches `switch_to`, so the instant a test sees the death message or the
/// parked sender, the corpse is still executing. `reap_region_objects` refuses to unmap a stack a
/// core is standing on, and answers `NotPermitted`.
///
/// Until 2026-08-17 it did not refuse, and freed the stack instead: four CI panics over five days,
/// every one reported as `*** KERNEL STACK OVERFLOW ***` with no stack overflowing. See
/// notes/stack.md, "a kernel stack freed under its owner", and milestone 124's block.
///
/// A test that means "this reap is refused for good" keeps calling [`reap`] and asserting on the
/// answer; this is only for the ones that mean "this reap succeeds".
fn reap_when_settled(slot: u64, tid: u64) -> Result<i64, Error> {
    let mut answer = reap(slot, tid);
    super::tests::wait_for(|| {
        if answer != Err(Error::NotPermitted) {
            return true;
        }
        answer = reap(slot, tid);
        answer != Err(Error::NotPermitted)
    });
    answer
}

/// Receive one five-word death message through the ABI, so the tid a test reaps with is the tid
/// a real supervisor would have read out of its registers.
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

/// How many capability slots the supervisor occupies. The "no richer than before" measurement:
/// a reap that handed authority back would have to put it somewhere, and every path that gives a
/// process a capability lands it in a slot.
fn occupied_slots() -> usize {
    (0..abi::CSPACE_SLOTS)
        .filter(|&s| sched::current_cap(s).is_ok())
        .count()
}

/// **The supervisor cannot construct anything, audited slot by slot.**
///
/// For every empty slot, the two primitives that build a process (`RETYPE_OBJ` for a TCB and for
/// an address space) are driven through the real dispatcher and must answer `NoSuchSlot`: there
/// is nothing there, which is what no-ambient-authority feels like, and it is a different fact
/// from `NotPermitted` (something there, restricted). For every occupied slot, the capability
/// must be an `Endpoint`, which cannot build by dispatch: the endpoint arm of `invoke` offers
/// send, receive, delegate, call, and reap, and no constructor at all. Untyped methods are *not*
/// invoked on those slots, because method numbers are per-object-type and `RETYPE_OBJ`'s number
/// is `SEND_CAP`'s on an endpoint.
fn assert_can_only_supervise(expected: &[u64]) {
    let mut frame = TrapFrame::for_user_entry(0, 0, [0, 0, 0]);
    for slot in 0..abi::CSPACE_SLOTS {
        match sched::current_cap(slot) {
            Err(_) => {
                for objtype in [abi::objtype::TCB, abi::objtype::ASPACE] {
                    assert_eq!(
                        invoke(&mut frame, slot, abi::untyped::RETYPE_OBJ, objtype, 0, 0),
                        Err(Error::NoSuchSlot),
                        "slot {slot} answered something other than \"you hold no such \
                         capability\" to a construction request",
                    );
                }
            }
            Ok(cap) => {
                assert!(
                    expected.contains(&slot),
                    "the supervisor holds an unexpected capability in slot {slot}: this test's \
                     confinement claim is only about a cspace it fully accounts for",
                );
                assert!(
                    matches!(cap.object, Object::Endpoint(_)),
                    "the supervisor holds a non-endpoint capability in slot {slot}",
                );
            }
        }
    }
}

/// Give everything back: the supervisor's capability slots, then the builder's budget (which
/// reclaims any instance region still under it), then the endpoint region. In that order,
/// because reclaiming the endpoints first would revoke the channels the corpses are still
/// attached to.
fn tidy(budget: u64, endpoints: u64, slots: &[u64]) {
    for &s in slots {
        let _ = sched::delete_current_cap(s);
    }
    sched::reclaim_region(budget).expect("the builder's own budget did not come back");
    sched::reclaim_region(endpoints).expect("the test's endpoint region did not come back");
}

/// **The headline: a supervisor that cannot build anything collects its dead child.**
///
/// Its entire authority is one supervision endpoint with `READ`. It holds no untyped, no frame,
/// no TCB, and no address space, and the audit proves that from the inside, on the two
/// primitives that build a process. Then it reaps, through the endpoint, naming the tid the
/// kernel stamped on the death message it just received. Before §32 this required `WRITE` on the
/// child's region, which is the same right that builds an arbitrary process out of it.
#[test_case]
fn a_supervisor_holding_only_its_endpoint_reaps_its_dead_child() {
    let (budget, endpoints) = arena();
    let fault_ep = endpoint(endpoints);
    let instance = crate::untyped::split(budget, INSTANCE_PAGES).expect("no instance region");
    let child = build_child_in(instance, FAULT_STUB, None, Some(fault_ep));

    let cap = hold_endpoint(fault_ep);
    assert_can_only_supervise(&[cap]);

    let msg = recv_death(cap);
    assert_eq!(msg[0], EVENT_FAULT, "the child should have crashed");
    assert_eq!(msg[1], child, "the death message named the wrong thread");

    assert_eq!(
        reap_when_settled(cap, msg[1]),
        Ok(0),
        "a supervisor holding only its supervision endpoint could not collect its own corpse",
    );
    assert_eq!(
        sched::corpse_fault_msg(child),
        None,
        "the corpse survived the reap",
    );
    // The reap did not turn into authority: the same audit still holds, and the same tid is now
    // an unknown name rather than a second free reap.
    assert_can_only_supervise(&[cap]);
    assert_eq!(reap(cap, child), Err(Error::NotSupervised));

    tidy(budget, endpoints, &[cap]);
}

/// **The reclaimed region returns to the builder, not to the reaper** (§32's first consequence,
/// and the property that makes the decision worth having).
///
/// The builder splits an instance region off its own budget, which bumps its watermark; the
/// supervisor reaps; the watermark comes back down (§16's LIFO return-of-pages) and the builder
/// can spend those pages again. The supervisor, meanwhile, occupies exactly the slots it did
/// before and still holds nothing but an endpoint. A reap that had quietly credited the reaper
/// would fail the first pair of assertions; one that had merely freed the corpse without
/// returning the memory would fail the second.
#[test_case]
fn the_reaped_region_returns_to_the_builder_not_the_reaper() {
    let (budget, endpoints) = arena();
    let fault_ep = endpoint(endpoints);
    let (spent_before, _) = crate::untyped::usage(budget).expect("the budget exists");

    let instance = crate::untyped::split(budget, INSTANCE_PAGES).expect("no instance region");
    assert_eq!(
        crate::untyped::usage(budget).unwrap().0,
        spent_before + INSTANCE_PAGES,
        "the split did not come out of the builder's budget",
    );
    let child = build_child_in(instance, FAULT_STUB, None, Some(fault_ep));

    let cap = hold_endpoint(fault_ep);
    let msg = recv_death(cap);
    assert_eq!(msg[1], child);
    let slots_before = occupied_slots();

    assert_eq!(reap_when_settled(cap, msg[1]), Ok(0), "the reap failed");

    assert_eq!(
        crate::untyped::usage(budget).unwrap().0,
        spent_before,
        "the reaped pages did not go back to the builder's budget: the reaper freed the corpse \
         and stranded its memory",
    );
    assert!(
        !crate::untyped::has_children(budget),
        "the instance region outlived its corpse, so the builder's budget is still committed",
    );
    // Genuinely spendable again, not merely un-bumped bookkeeping.
    let again = crate::untyped::split(budget, INSTANCE_PAGES)
        .expect("the builder could not re-spend the pages the reap returned");
    sched::reclaim_region(again).expect("reclaim the re-split region");

    assert_eq!(
        occupied_slots(),
        slots_before,
        "the supervisor gained a capability by reaping",
    );
    assert_can_only_supervise(&[cap]);

    tidy(budget, endpoints, &[cap]);
}

/// **A live child is refused, with an error of its own.** Collecting a corpse is not killing.
/// The child here is blocked in a `SEND` nobody has received, so it is provably alive at the
/// moment of the attempt, and the refusal is `StillAlive` rather than a generic `NotPermitted`:
/// a restart policy needs "wait, or escalate to the owner's `DESTROY`" to be distinguishable
/// from "there is no such child of mine". Then the child is let go, dies, and the *same* tid
/// through the *same* endpoint succeeds, which is what proves the refusal was about liveness and
/// not about authority.
#[test_case]
fn reap_refuses_a_live_child_with_a_distinct_error() {
    let (budget, endpoints) = arena();
    let report = endpoint(endpoints);
    let fault_ep = endpoint(endpoints);
    let instance = crate::untyped::split(budget, INSTANCE_PAGES).expect("no instance region");
    let child = build_child_in(instance, REPORT_STUB, Some(report), Some(fault_ep));

    let cap = hold_endpoint(fault_ep);
    assert_eq!(
        reap(cap, child),
        Err(Error::StillAlive),
        "a supervisor collected a thread that was still running: REAP authorizes collecting a \
         corpse, never killing",
    );

    // Let it finish. It SENDs, we receive, it exits, and its death arrives on the endpoint.
    assert_eq!(
        sched::ipc_recv(report)[0],
        REPORT_WORD,
        "the child never ran"
    );
    let msg = recv_death(cap);
    assert_eq!(msg[0], EVENT_EXIT, "a clean exit must report EXIT");
    assert_eq!(msg[1], child);
    assert_eq!(
        reap(cap, child),
        Ok(0),
        "the same tid through the same endpoint failed once dead: the earlier refusal was not \
         about liveness after all",
    );

    tidy(budget, endpoints, &[cap]);
}

/// **Another supervisor's child is refused, even to a holder of both endpoints.**
///
/// The sharpest form of §32's authorization rule: one process holds two supervision endpoints,
/// and learns a tid legitimately, through the endpoint that supervises it. Naming that tid on
/// the *other* endpoint is refused, because authorization is the `(tid, endpoint)` relationship
/// and not "am I a supervisor". So a tid is a name inside a relationship, never a global handle,
/// which is what lets §26's kernel-stamped tid be reused for this with no new bookkeeping.
#[test_case]
fn reap_refuses_another_supervisors_child() {
    let (budget, endpoints) = arena();
    let mine = endpoint(endpoints);
    let theirs = endpoint(endpoints);
    let instance = crate::untyped::split(budget, INSTANCE_PAGES).expect("no instance region");
    let child = build_child_in(instance, FAULT_STUB, None, Some(theirs));

    let cap_mine = hold_endpoint(mine);
    let cap_theirs = hold_endpoint(theirs);

    let msg = recv_death(cap_theirs);
    assert_eq!(msg[1], child, "the death arrived on the wrong endpoint");
    assert_eq!(
        reap(cap_mine, child),
        Err(Error::NotSupervised),
        "a corpse was collected through an endpoint that does not supervise it",
    );
    assert!(
        sched::corpse_fault_msg(child).is_some(),
        "the refused reap still tore something down",
    );

    assert_eq!(
        reap(cap_theirs, child),
        Ok(0),
        "the supervising endpoint could not collect its own corpse",
    );

    tidy(budget, endpoints, &[cap_mine, cap_theirs]);
}

/// **A recycled tid is refused, not resolved to the thread now in that slot.**
///
/// Generational names (`crates/slots`) exist for exactly this, and this is the test that proves
/// the reap path actually uses them: reap a child, build a replacement that lands in the freed
/// thread-table slot with a bumped generation, then replay the old tid. The replay must be
/// refused, and the replacement must be untouched, which is asserted by letting it run to
/// completion afterwards rather than by inspecting it. The slot-aliasing assertion is part of
/// the test: without it, the replay would be of a name nothing has reused and would prove
/// nothing.
#[test_case]
fn reap_refuses_a_recycled_tid_rather_than_the_wrong_thread() {
    let (budget, endpoints) = arena();
    let fault_ep = endpoint(endpoints);
    let report = endpoint(endpoints);
    let cap = hold_endpoint(fault_ep);

    let first_region = crate::untyped::split(budget, INSTANCE_PAGES).expect("no first region");
    let first = build_child_in(first_region, FAULT_STUB, None, Some(fault_ep));
    assert_eq!(recv_death(cap)[1], first);
    assert_eq!(
        reap_when_settled(cap, first),
        Ok(0),
        "the first reap failed"
    );

    let second_region = crate::untyped::split(budget, INSTANCE_PAGES).expect("no second region");
    let second = build_child_in(second_region, REPORT_STUB, Some(report), Some(fault_ep));
    assert_eq!(
        second & SLOT_MASK,
        first & SLOT_MASK,
        "the replacement did not land in the reaped thread's table slot, so replaying the old \
         tid would not be aliasing anything: this test needs the reuse to mean something",
    );
    assert_ne!(
        second, first,
        "a reused slot handed out the same name: the generation did not bump",
    );

    assert_eq!(
        reap(cap, first),
        Err(Error::NotSupervised),
        "a stale tid resolved: the reap path is not going through the generational name",
    );
    // The replacement is untouched, proven by it running to completion.
    assert_eq!(
        sched::ipc_recv(report)[0],
        REPORT_WORD,
        "the replayed tid reaped the thread that had recycled its slot",
    );
    let msg = recv_death(cap);
    assert_eq!(msg[1], second);
    assert_eq!(reap_when_settled(cap, second), Ok(0));

    tidy(budget, endpoints, &[cap]);
}

/// **Reaping a corpse whose death message was never collected leaves the endpoint clean.**
///
/// A supervised thread that dies with nobody in `RECV` parks on its supervision endpoint's
/// sender queue holding the message (§26 implementation note 2). Nothing requires a supervisor
/// to collect that message before reaping: it can be told the tid by its builder, or simply
/// choose not to read. So the reap has to unlink the corpse from that queue before freeing its
/// TCB, or the supervisor's next `RECV` follows a pointer into a recycled page.
///
/// This was reachable before §32 too, through `Untyped::DESTROY`; every existing caller happened
/// to receive first, so it never fired. `endpoint::REAP` makes it easy to reach, which is how it
/// was found. `crates/ipc`'s `remove_sender` is the fix, and the second half of this test (a
/// fresh child's death arriving normally on the same endpoint) is what proves the queue is
/// genuinely intact rather than merely counted right.
#[test_case]
fn reaping_an_uncollected_corpse_leaves_no_ghost_on_the_endpoint() {
    let (budget, endpoints) = arena();
    let fault_ep = endpoint(endpoints);
    let instance = crate::untyped::split(budget, INSTANCE_PAGES).expect("no instance region");
    let child = build_child_in(instance, FAULT_STUB, None, Some(fault_ep));
    let cap = hold_endpoint(fault_ep);

    // Nobody is receiving, so the corpse must park. Wait for it **on the clock**: 4000 yields was
    // the wait, and a yield count is not a duration. On the physical core under HVF (milestone 81)
    // this core spends 4000 yields in microseconds while the child's core has not run it yet, and
    // the assertion fails with "the corpse never parked" about a corpse that was on its way.
    assert!(
        super::tests::wait_for(|| sched::endpoint_waiting_senders(fault_ep) == 1),
        "the corpse never parked on its supervision endpoint",
    );

    assert_eq!(reap_when_settled(cap, child), Ok(0), "the reap failed");
    assert_eq!(
        sched::endpoint_waiting_senders(fault_ep),
        0,
        "a freed TCB is still linked into the supervision endpoint's sender queue: the next \
         RECV would follow a dangling pointer into a recycled page",
    );

    // The endpoint still works, which is the real assertion: a stale head or tail would show up
    // here and not in the count above.
    let next_region = crate::untyped::split(budget, INSTANCE_PAGES).expect("no second region");
    let next = build_child_in(next_region, FAULT_STUB, None, Some(fault_ep));
    let msg = recv_death(cap);
    assert_eq!(
        msg[1], next,
        "the endpoint delivered something other than the new child's death",
    );
    assert_eq!(reap_when_settled(cap, next), Ok(0));

    tidy(budget, endpoints, &[cap]);
}
