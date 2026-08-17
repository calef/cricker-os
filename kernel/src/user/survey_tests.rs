use abi::Error;

use super::supervision_tests::{FAULT_STUB, REPORT_STUB, build_child_in};
use crate::arch::exceptions::TrapFrame;
use crate::cap::Rights;
use crate::sched;
use crate::syscall::invoke;

/// The builder's whole budget: room for three instances plus slack, so a domain can hold more than
/// one member and a second domain can exist beside it.
const BUILDER_BUDGET_PAGES: u64 = 80;

/// Pages per instance region, the same carve `build_child_in` has always made: the child's address
/// space (root and tables), its code page, its stack page, and its TCB.
const INSTANCE_PAGES: u64 = 16;

/// Pages for the test's own endpoints, one per endpoint (`RETYPE_OBJ`'s one-object-per-page rule).
/// Three is the most any test here needs; four is slack.
const ENDPOINT_PAGES: u64 = 4;

/// **`ps`'s row buffer must hold the kernel's whole thread table**, checked by the compiler rather
/// than by a test, which is the strongest rung available for a fact that is two constants in two
/// crates. A `ps` holding the widest grant this system can express sees every thread on the
/// machine; if the table outgrew the buffer, the listing would silently stop short, which is the
/// one failure a monitor must never have. Making the wrong state unrepresentable costs one line
/// here and nobody has to remember it.
const _: () = assert!(
    ps::MAX_ROWS >= sched::MAX_THREADS,
    "ps::MAX_ROWS is smaller than the kernel's thread table: a listing of the widest domain this \
     system can express would be silently truncated",
);

/// One test's world: a budget the builder owns, and a small region the endpoints come out of, so
/// `tidy` can give both back rather than spending the kernel's shared endpoint budget on every run.
fn arena() -> (u64, u64) {
    let budget = crate::untyped::create(BUILDER_BUDGET_PAGES).expect("no builder budget");
    let endpoints = crate::untyped::create(ENDPOINT_PAGES).expect("no endpoint region");
    (budget, endpoints)
}

fn endpoint(region: u64) -> sched::EpId {
    sched::create_endpoint_from(region).expect("no endpoint")
}

/// Hold a domain the way a viewer holds one: `READ`, which is what `SURVEY` takes.
fn hold_read(ep: sched::EpId) -> u64 {
    sched::grant(crate::cap::endpoint_cap(ep, Rights::READ)).expect("grant the endpoint")
}

/// Hold the same endpoint **send-only**: a peer that may report to this supervisor and is not the
/// supervisor. The negative control's whole setup is this one line.
fn hold_write(ep: sched::EpId) -> u64 {
    sched::grant(crate::cap::endpoint_cap(ep, Rights::WRITE)).expect("grant the endpoint")
}

/// `invoke(cap, SURVEY, cursor, _, _)`, through the real dispatcher, returning the three words a
/// userspace caller would read out of its registers.
fn survey(slot: u64, cursor: u64) -> (i64, u64, u64) {
    let mut frame = TrapFrame::for_user_entry(0, 0, [0, 0, 0]);
    match invoke(&mut frame, slot, abi::endpoint::SURVEY, cursor, 0, 0) {
        Ok(next) => (next, frame.arg(1), frame.arg(2)),
        Err(e) => (e as i64, 0, 0),
    }
}

/// **The whole domain, walked by the real program's real loop.**
///
/// `ps::collect` is what `user/src/ps.rs` runs; driving it here rather than reimplementing the
/// cursor walk is the same discipline the `rm`, `date` and sink tests keep, and it is why a bug in
/// the cursor protocol cannot hide between the two sides.
fn walk(slot: u64) -> ps::Survey {
    ps::collect(&mut |cursor| survey(slot, cursor))
}

/// `invoke(cap, REAP, tid, _, _)`, through the real dispatcher.
fn reap(slot: u64, tid: u64) -> Result<i64, Error> {
    let mut frame = TrapFrame::for_user_entry(0, 0, [0, 0, 0]);
    invoke(&mut frame, slot, abi::endpoint::REAP, tid, 0, 0)
}

/// **Let every parked child finish.** Each `REPORT_STUB` member is blocked in a send on the shared
/// parking endpoint, so taking `n` messages off it releases `n` of them to exit.
///
/// Deliberately not per-child: an `ipc_recv` takes whichever sender is queued, so a test with
/// members in two domains must drain the whole endpoint before it collects any of them. Draining
/// only its own domain's count releases somebody else's child and leaves one of its own blocked
/// forever, which is the bug this comment exists to stop the next reader reintroducing.
fn drain(parking: sched::EpId, n: usize) {
    for _ in 0..n {
        sched::ipc_recv(parking);
    }
}

/// **Collect every member of a domain through the endpoint that showed it.**
///
/// A test that keeps children alive in order to survey them has to end them deliberately: a region
/// holding a live thread refuses to be reclaimed, and the refusal is destructive on the way past
/// (§16's amendment arms the kill), so leaving it to `tidy` would hand the next test an arena in a
/// state it did not ask for.
///
/// It doubles as an assertion worth having on its own: **every tid a survey reported is a tid the
/// same endpoint can reap**, which is `capability::survey_includes`'s scope property observed from
/// the control side rather than the view side. The wait is the clock's, not a yield count, because
/// how long a released child takes to reach its exit is a property of the host (notes/hvf-leg.md).
fn collect_all(cap: u64, tids: &[u64]) {
    for &tid in tids {
        assert!(
            super::tests::wait_for(|| reap(cap, tid) == Ok(0)),
            "a member the survey reported could not be collected through the endpoint that \
             showed it",
        );
    }
}

/// The tids a domain reports, in the order it reported them.
fn tids(s: &ps::Survey) -> impl Iterator<Item = u64> + '_ {
    s.rows().iter().map(|r| r.tid)
}

/// Give everything back: the viewer's slots, then the builder's budget (which reclaims any instance
/// region still under it), then the endpoint region. Reclaiming the endpoints first would revoke
/// the channels the corpses are still attached to.
fn tidy(budget: u64, endpoints: u64, slots: &[u64]) {
    for &s in slots {
        let _ = sched::delete_current_cap(s);
    }
    sched::reclaim_region(budget).expect("the builder's own budget did not come back");
    sched::reclaim_region(endpoints).expect("the test's endpoint region did not come back");
}

/// Build a child into its own region carved from `budget`, so one reclaim frees the instance.
fn child_in(budget: u64, stub: &[u32], report: Option<sched::EpId>, fault_ep: sched::EpId) -> u64 {
    let region = crate::untyped::split(budget, INSTANCE_PAGES).expect("no instance region");
    build_child_in(region, stub, report, Some(fault_ep))
}

/// **The headline: a domain is exactly the endpoint's own children, and nothing else on the
/// machine.**
///
/// Two children under one supervision endpoint, one under another, and the whole rest of the
/// system (this thread, the idle threads, every kernel thread and every other test's leftovers)
/// unsupervised. The survey of the first endpoint reports its two and stops.
///
/// This is the confinement claim, and it is the one Linux's `/proc` cannot make: there, a listing
/// is a fact about the machine and every process gets it for free. Here it is a fact about a
/// capability, so the answer changes with which endpoint you hold and there is nothing to hold that
/// would widen it.
#[test_case]
fn a_domain_is_exactly_the_children_of_the_endpoint_that_was_granted() {
    let (budget, endpoints) = arena();
    let mine = endpoint(endpoints);
    let theirs = endpoint(endpoints);
    // A parking endpoint nobody ever receives on, so a REPORT_STUB child blocks in its send and
    // stays a live member of the domain for the length of the test instead of exiting under us.
    let parking = endpoint(endpoints);

    let a = child_in(budget, REPORT_STUB, Some(parking), mine);
    let b = child_in(budget, REPORT_STUB, Some(parking), mine);
    let stranger = child_in(budget, REPORT_STUB, Some(parking), theirs);

    // Both of mine have to have reached their send before the states below mean anything.
    assert!(
        super::tests::wait_for(|| sched::endpoint_waiting_senders(parking) == 3),
        "the three children never reached their sends",
    );

    let cap = hold_read(mine);
    let seen = walk(cap);
    assert!(
        !seen.refused(),
        "a supervisor could not read its own domain"
    );

    let found = TidSet::of(&seen);
    assert_eq!(
        seen.rows().len(),
        2,
        "the domain reported {} members where two were built",
        seen.rows().len(),
    );
    assert!(found.has(a) && found.has(b), "a domain lost one of its own");
    assert!(
        !found.has(stranger),
        "another supervisor's child appeared in this domain: the scope is not the subtree",
    );

    // And the survey never reaches anything unsupervised, which is every other thread in the
    // system including the one running this assertion. That is the half a `/proc` cannot express.
    let me = sched::current();
    assert!(
        !found.has(me),
        "the surveying thread listed itself despite being supervised by nobody",
    );

    // The other endpoint answers about its own child and only its own, from the same kernel walk.
    let other = hold_read(theirs);
    let seen = walk(other);
    assert_eq!(seen.rows().len(), 1);
    assert_eq!(
        tids(&seen).next(),
        Some(stranger),
        "the second domain reported somebody else's child",
    );

    drain(parking, 3);
    collect_all(cap, &[a, b]);
    collect_all(other, &[stranger]);
    tidy(budget, endpoints, &[cap, other]);
}

/// **The negative control, and it is the point of the whole method** (milestone 126, the block's
/// own requirement): a viewer that was not granted the domain is **refused loudly**, not shown an
/// empty list.
///
/// Three answers, all distinct, all reached through the real dispatcher:
///
/// - a **send-only** holder, a peer that may report to this supervisor, gets `NotPermitted`;
/// - a holder of **nothing at all** gets `NoSuchSlot`, which is what no-ambient-authority feels
///   like: there is nothing there to name;
/// - a `READ` holder of a domain that is genuinely **empty** gets `DONE`, an answer rather than a
///   refusal, because its authority was never in question.
///
/// A monitor that reported nothing because it could not look would read exactly like a quiet
/// machine, which is the worst failure this tool has available. `fs_proto` chose `EPERM` over an
/// empty listing for the same reason.
#[test_case]
fn a_viewer_without_the_domain_is_refused_rather_than_shown_an_empty_list() {
    let (budget, endpoints) = arena();
    let ep = endpoint(endpoints);
    let parking = endpoint(endpoints);
    let child = child_in(budget, REPORT_STUB, Some(parking), ep);
    assert!(
        super::tests::wait_for(|| sched::endpoint_waiting_senders(parking) == 1),
        "the child never reached its send",
    );

    // Send-only: the endpoint is real, the child is real, and the answer is a refusal.
    let peer = hold_write(ep);
    assert_eq!(
        survey(peer, 0),
        (Error::NotPermitted as i64, 0, 0),
        "a send-only holder was shown a domain it holds no right to look at",
    );
    let refused = walk(peer);
    assert!(refused.refused());
    assert_eq!(refused.rows().len(), 0);

    // Nothing at all in the slot: a different refusal, and a louder one.
    let empty_slot = abi::CSPACE_SLOTS - 1;
    assert!(
        sched::current_cap(empty_slot).is_err(),
        "pick an empty slot"
    );
    assert_eq!(
        survey(empty_slot, 0),
        (Error::NoSuchSlot as i64, 0, 0),
        "an ungranted viewer got something other than \"you hold no such capability\"",
    );

    // A domain that really is empty is an *answer*. This is the assertion that makes the two above
    // mean something: without it, "refused" and "nothing here" could be the same code path.
    let vacant = endpoint(endpoints);
    let held = hold_read(vacant);
    assert_eq!(
        survey(held, 0),
        (abi::survey::DONE as i64, 0, 0),
        "an empty domain must answer, not refuse",
    );
    let seen = walk(held);
    assert!(
        !seen.refused(),
        "an empty domain was reported as a refusal, which is the confusion this method exists to \
         prevent",
    );
    assert_eq!(seen.rows().len(), 0);

    // And the holder that *is* the supervisor still sees its child, so the refusals above are
    // about the rights and not about a survey that never works.
    let sup = hold_read(ep);
    assert_eq!(tids(&walk(sup)).next(), Some(child));

    drain(parking, 1);
    collect_all(sup, &[child]);
    tidy(budget, endpoints, &[peer, held, sup]);
}

/// **A corpse is still in the domain, and it says so.**
///
/// The state a supervisor most needs to see and the one Unix cannot show without a parent that
/// happened to call `wait`: the child faulted, its death message is waiting, and it persists until
/// `endpoint::REAP` collects it (DECISIONS §26). A survey that dropped corpses would hide exactly
/// the thing a monitor is watching for.
///
/// Then the reap, through the same endpoint, and the row is gone. The two halves are checked in one
/// test on purpose: the domain the viewer reports and the domain the supervisor may collect from
/// are one set, which is `capability::survey_includes`'s proved property observed on a real kernel.
#[test_case]
fn a_dead_child_is_still_in_the_domain_until_it_is_reaped() {
    let (budget, endpoints) = arena();
    let ep = endpoint(endpoints);
    let child = child_in(budget, FAULT_STUB, None, ep);

    let cap = hold_read(ep);
    // The corpse parks on its supervision endpoint's sender queue with its death message when
    // nobody is in RECV, which is how a survey can see it before anyone has collected the news.
    assert!(
        super::tests::wait_for(|| sched::endpoint_waiting_senders(ep) == 1),
        "the child never died onto its supervision endpoint",
    );

    let seen = walk(cap);
    assert_eq!(seen.rows().len(), 1, "the corpse fell out of its domain");
    assert_eq!(seen.rows()[0].tid, child);
    assert_eq!(
        seen.rows()[0].state,
        abi::survey::DEAD,
        "a corpse was reported as something other than dead",
    );

    // Collect it through the endpoint the survey named it on, then the domain is empty and says so
    // rather than refusing.
    let mut frame = TrapFrame::for_user_entry(0, 0, [0, 0, 0]);
    invoke(&mut frame, cap, abi::endpoint::RECV, 0, 0, 0).expect("RECV refused");
    assert_eq!(
        invoke(&mut frame, cap, abi::endpoint::REAP, child, 0, 0),
        Ok(0),
        "the tid a survey reported was not one the same endpoint could reap",
    );
    let seen = walk(cap);
    assert!(!seen.refused());
    assert_eq!(
        seen.rows().len(),
        0,
        "a reaped corpse is still being listed"
    );

    tidy(budget, endpoints, &[cap]);
}

/// **The cursor resumes, and it never repeats or skips a member.**
///
/// The walk gives `SCHED` back between entries, so the cursor is the only thing carrying the
/// position across calls (`slots::Table::iter_from`). A cursor that resolved to a *position* rather
/// than a slot would double-report or drop a member the moment anything changed, so what is checked
/// is the invariant that makes the shape safe: distinct tids, every one of them a real member.
///
/// It also pins the sizing claim `crates/ps` makes about its row buffer, which is otherwise one
/// number written in two crates.
#[test_case]
fn a_resumed_walk_reports_every_member_exactly_once() {
    let (budget, endpoints) = arena();
    let ep = endpoint(endpoints);
    let parking = endpoint(endpoints);
    let a = child_in(budget, REPORT_STUB, Some(parking), ep);
    let b = child_in(budget, REPORT_STUB, Some(parking), ep);
    let c = child_in(budget, REPORT_STUB, Some(parking), ep);
    assert!(
        super::tests::wait_for(|| sched::endpoint_waiting_senders(parking) == 3),
        "the three children never reached their sends",
    );

    let cap = hold_read(ep);
    let seen = walk(cap);
    assert_eq!(seen.rows().len(), 3, "the walk did not reach every member");

    let mut found = [0u64; 3];
    for (i, tid) in tids(&seen).enumerate() {
        found[i] = tid;
    }
    for &want in &[a, b, c] {
        assert_eq!(
            found.iter().filter(|&&t| t == want).count(),
            1,
            "tid {want} was reported {} times by one walk",
            found.iter().filter(|&&t| t == want).count(),
        );
    }

    // A cursor past the whole table is "nothing more", not a refusal: a caller that keeps feeding
    // back what it was given must never fall off the end into an answer that reads as "you may not
    // look".
    assert_eq!(survey(cap, u64::MAX), (abi::survey::DONE as i64, 0, 0));

    drain(parking, 3);
    collect_all(cap, &[a, b, c]);
    tidy(budget, endpoints, &[cap]);
}

/// A small membership check without allocating: the kernel has no heap, and these tests hold at
/// most three tids at a time.
struct TidSet {
    tids: [u64; 3],
    n: usize,
}

impl TidSet {
    fn of(s: &ps::Survey) -> TidSet {
        let mut set = TidSet { tids: [0; 3], n: 0 };
        for r in s.rows().iter().take(3) {
            set.tids[set.n] = r.tid;
            set.n += 1;
        }
        set
    }

    fn has(&self, tid: u64) -> bool {
        self.tids[..self.n].contains(&tid)
    }
}
