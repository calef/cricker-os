use clock_proto::{policy, propose, state, status};

use super::*;

/// Start the service and take its startup report. `[RPT_READY, state, wall_nanos]`.
fn start() -> (clock_service::Wiring, [u64; 5]) {
    let image = program("clock").expect("no clock program in the initrd archive");
    let w = clock_service::start(image);
    let report = crate::sched::ipc_recv(w.report);
    (w, report)
}

/// **The machine finds out what time it is, from its own device tree.**
///
/// Two ISAs, two entirely different RTCs (a PL031 counting seconds at `0x9010000`, a Goldfish
/// counting nanoseconds at `0x101000`), one binary, and the choice between them made from the
/// `compatible` string rather than from `target_arch`. What the assertion actually catches:
/// a wrong base address, a wrong register offset, the Goldfish halves swapped, and the
/// seconds/nanoseconds unit confusion, all of which land outside a 74-year window.
///
/// It is a **plausibility** check, not an accuracy one, and deliberately so. Nothing in the
/// guest knows the host's clock to compare against, so the honest claim is "this is a time a
/// machine running this code could be at", which is exactly what `policy::plausible` is for and
/// exactly what the previous behaviour (1970 plus uptime) fails.
#[test_case]
fn the_clock_service_reads_a_plausible_wall_clock_from_the_rtc() {
    let (w, report) = start();

    assert_ne!(
        w.kind,
        clock_proto::rtc::NONE,
        "both QEMU virt boards have an RTC; finding none means the compatible match broke",
    );
    assert_eq!(
        report[1],
        state::RTC,
        "the clock should be set from the RTC"
    );
    assert!(
        policy::plausible(report[2]),
        "the RTC read back {} ns, which is outside the sanity window",
        report[2],
    );

    // And the page a reader would hold says the same thing, computed independently: the kernel
    // adds its own counter to the offset the service published, through the same seqlock.
    let r = w.page().read();
    assert_eq!(r.state, state::RTC);
    assert_eq!(r.generation, 1, "one publish: the RTC reading");
    let by_hand = clock_proto::wall_nanos(r.offset_nanos, clock_service::monotonic_nanos());
    assert!(
        by_hand >= report[2] && by_hand - report[2] < 10 * clock_proto::NANOS_PER_SEC,
        "a reader's own arithmetic ({by_hand}) should agree with the service's ({})",
        report[2],
    );
}

/// **A proposer can ask and cannot tell.**
///
/// The kernel plays the network time client here: it holds nothing but the propose endpoint,
/// and every attempt to move the clock somewhere the policy forbids comes back refused with the
/// page's generation unchanged. The generation is the load-bearing half of the assertion; a
/// refusal that had nevertheless written the offset would return the same status word.
///
/// This is the milestone's demonstrable claim, and it is one Unix cannot make: `ntpd` runs as
/// root and may set the clock to anything.
#[test_case]
fn a_proposer_can_ask_and_cannot_tell() {
    let (w, _) = start();
    let before = w.page().read();
    assert!(state::known(before.state), "needs a running clock to step");

    let now = w.wall_nanos();
    for (proposed, want, what) in [
        (0, status::REFUSED_IMPLAUSIBLE, "1970, the old lie itself"),
        (
            policy::NOT_AFTER_NANOS,
            status::REFUSED_IMPLAUSIBLE,
            "the far future, past every certificate expiry",
        ),
        (
            now + 2 * policy::MAX_STEP_FORWARD_NANOS,
            status::REFUSED_TOO_FAR_FORWARD,
            "plausible in the absolute, too big a step",
        ),
        (
            now - 2 * policy::MAX_STEP_BACKWARD_NANOS,
            status::REFUSED_TOO_FAR_BACKWARD,
            "backwards, where instants would happen twice",
        ),
    ] {
        let (got, _) = w.propose_nanos(proposed);
        assert_eq!(got, want, "proposing {what}");
        assert_eq!(
            w.page().read().generation,
            before.generation,
            "a refused proposal must not have written the page ({what})",
        );
    }

    // And the bounded case it exists to allow: a small correction is accepted, and the
    // provenance says it came from a proposal rather than from a human.
    let (got, after) = w.propose_nanos(now + clock_proto::NANOS_PER_SEC / 2);
    assert_eq!(got, status::ACCEPTED);
    assert!(after >= now);
    let r = w.page().read();
    assert_eq!(r.state, state::SYNCED, "an accepted proposal is not a SET");
    assert_eq!(r.generation, before.generation + 1);
}

/// **Adjusting the wall clock cannot perturb monotonic time, by construction.**
///
/// The property the counter-plus-offset design buys, and the reason `Instant` was left alone.
/// Unix reaches for `adjtime` slewing partly because stepping the clock backwards breaks things
/// that assumed it only moved forward; here the step is an offset write and the counter never
/// sees it, so there is nothing to slew *for correctness*. The assertion is deliberately blunt:
/// step the wall clock by half a second and require the monotonic counter to have advanced by
/// far less than that, which no implementation that fed the offset into the counter could pass.
#[test_case]
fn adjusting_the_wall_clock_leaves_the_monotonic_counter_alone() {
    let (w, _) = start();
    let step = clock_proto::NANOS_PER_SEC / 2;

    let mono_before = clock_service::monotonic_nanos();
    let wall_before = w.wall_nanos();
    let (got, wall_after) = w.propose_nanos(wall_before + step);
    assert_eq!(got, status::ACCEPTED);
    let mono_after = clock_service::monotonic_nanos();

    assert!(
        wall_after >= wall_before + step / 2,
        "the wall clock should have moved: {wall_before} -> {wall_after}",
    );
    let mono_moved = mono_after - mono_before;
    assert!(
        mono_moved < step / 4,
        "the monotonic counter moved {mono_moved} ns across a {step} ns wall-clock step; \
         it should have moved only by the cost of the round trip",
    );
}

/// **A malformed request is refused, and the service survives it.**
///
/// The serve loop's other exit. It matters because the propose endpoint is the one thing a
/// hostile component holds, so "an opcode nobody defined" has to be a reply rather than a fault
/// or a wedge: the second, well-formed call proves the service is still there.
#[test_case]
fn an_unknown_opcode_is_answered_rather_than_fatal() {
    let (w, _) = start();
    let r = crate::sched::ipc_call(w.propose, [propose::req(0xff), 0]);
    assert_eq!(r[0], status::BAD_REQUEST);

    let r = crate::sched::ipc_call(w.propose, [propose::req(propose::STATE), 0]);
    assert_eq!(r[0], state::RTC, "still serving, and still knows the time");
}
