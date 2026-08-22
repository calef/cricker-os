use core::sync::atomic::Ordering;

use clock_proto::{NANOS_PER_SEC, policy, state, status};
use ntp_service::{ATTEMPTS, reject, rpt, srv};

use super::*;
use crate::arch::exceptions::USER_FAULTS;

/// Bounded by wall clock rather than by a yield count: since DECISIONS §28 the work a test
/// spawns runs on other cores, so a yield on an idle core elapses no real time. Two seconds is
/// far under the 60 s hang watchdog, so a genuine lost wakeup still fails loudly.
fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
    while crate::arch::timer::now() < deadline {
        if cond() {
            return true;
        }
        crate::sched::yield_now();
    }
    cond()
}

fn ntp_image() -> &'static [u8] {
    program("ntp").expect("no ntp program in the initrd archive")
}

/// **A fresh clock service per test.** Each one allocates its own page and reads the RTC again,
/// so a step taken in one test cannot bias the next one's bounds, and each test's "before" is
/// genuinely before.
fn clock() -> clock_service::Wiring {
    let image = program("clock").expect("no clock program in the initrd archive");
    let w = clock_service::start(image);
    let report = crate::sched::ipc_recv(w.report);
    assert!(
        state::known(report[1]),
        "the clock service does not know the time, so there is nothing for NTP to correct",
    );
    w
}

/// The entropy service's request endpoint: the client's whole authority over randomness, and it
/// names no device. `ensure` wires the service once per boot; whichever test asks first pays.
fn entropy() -> Option<crate::sched::EpId> {
    let image = program("entropy").expect("no entropy program in the initrd archive");
    let w = entropy_service::ensure(image, entropy_service::Bus::Mmio)?;
    if let Some(report) = w.wait_for_ready() {
        assert_eq!(
            report[0],
            entropy_proto::READY,
            "the entropy service did not come up (it reported {:#x})",
            report[0],
        );
    }
    Some(w.request)
}

/// Where the client is told to send. Nothing listens there; the test server is behind the
/// endpoint, not behind the address. The address is asserted anyway, because a client that
/// invented its own destination would be one that ignores its wiring.
const SERVER_IP: u32 = 0x0a00_0202; // 10.0.2.2, slirp's gateway
const SERVER_PORT: u16 = ntp_proto::PORT;

/// Run one exchange: a fresh server at `claimed_nanos`, a fresh client, both reports drained in
/// the order the blocking `send` requires. Returns `(the server's report, the client's report)`.
fn exchange(
    clock: &clock_service::Wiring,
    variant: u64,
    claimed_nanos: u64,
) -> ([u64; 5], [u64; 5]) {
    let server = ntp_service::start_server(ntp_image(), variant, claimed_nanos);
    let client = ntp_service::start_client(
        ntp_image(),
        server.stack,
        clock.propose,
        entropy(),
        SERVER_IP,
        SERVER_PORT,
    );
    // The server blocks in its one `send` until this returns, and the client's RECV is queued
    // behind it, so the order here is load-bearing rather than stylistic. The bounded wait is
    // what turns "the client never sent a request" into a failure that names itself instead of
    // a sixty-second watchdog hang: `ipc_recv` on an endpoint nobody will ever send on does not
    // come back.
    assert!(
        wait_for(|| crate::sched::endpoint_waiting_senders(server.report) > 0),
        "the test server never saw a request: the client failed before it reached the network",
    );
    let served = crate::sched::ipc_recv(server.report);
    assert!(
        wait_for(|| crate::sched::endpoint_waiting_senders(client) > 0),
        "the client never reported: it is still blocked somewhere in the exchange",
    );
    let reported = crate::sched::ipc_recv(client);
    (served, reported)
}

/// **The exchange lands as a proposal, and the service is what decides.**
///
/// The whole path in one test: eight bytes of real entropy become a nonce, a well-formed NTPv4
/// client packet goes out to port 123, a server reply comes back, `Query::accept` produces a
/// sample, and the correction reaches the clock **through the propose endpoint**. The proof that
/// it went that way rather than by a write is the state word: an accepted proposal publishes
/// `SYNCED`, which nothing but the service can produce.
#[test_case]
fn an_ntp_exchange_reaches_the_clock_as_a_proposal() {
    let clock = clock();
    let before = clock.page().read();
    let step = NANOS_PER_SEC / 2;
    let wall_before = clock.wall_nanos();
    let claimed = wall_before + step;

    let (served, reported) = exchange(&clock, srv::GOOD, claimed);

    // What the server saw on the wire: the packet is ours and it is addressed where it was told.
    assert_eq!(served[0], rpt::SERVED);
    assert_eq!(
        served[2] >> 32,
        ntp_proto::PORT as u64,
        "the client did not address UDP 123",
    );
    assert_eq!(
        (served[2] >> 8) & 0xff,
        ntp_proto::VERSION as u64,
        "not an NTPv4 request",
    );
    assert_eq!(
        served[2] & 0xff,
        ntp_proto::mode::CLIENT as u64,
        "not a client-mode request: a server would take this for an unsolicited packet",
    );

    assert_eq!(
        reported[0],
        rpt::SYNCED,
        "the client did not synchronise (report {:?})",
        reported,
    );
    assert_eq!(
        reported[1],
        status::ACCEPTED,
        "the clock service refused a half-second correction",
    );

    let after = clock.page().read();
    assert_eq!(
        after.state,
        state::SYNCED,
        "the clock is {} rather than SYNCED: an accepted proposal is not a SET, and only the \
         service can publish this",
        after.state,
    );
    assert_eq!(
        after.generation,
        before.generation + 1,
        "exactly one publish: the proposal",
    );

    // And the clock really moved, by about what the server claimed. The lower bound is what
    // fails if the proposal was dropped on the floor; the upper bound is what fails if the
    // client believed a server it should have bounded.
    let moved = clock.wall_nanos() - wall_before;
    assert!(
        moved >= step * 4 / 5,
        "the wall clock moved {moved} ns, less than the {step} ns the server claimed",
    );
    assert!(
        moved < step + 3 * NANOS_PER_SEC,
        "the wall clock moved {moved} ns, far past the {step} ns the server claimed",
    );
}

/// **A reply that fails validation moves nothing.**
///
/// Three replies a real client meets, and each is a different situation rather than three ways
/// of saying "bad packet": an origin that is not our nonce is the off-path spoof the check
/// exists for, a kiss-o'-death is an instruction, and twenty bytes is something that is not NTP
/// arriving on our socket. `ntp_proto` proves the checks themselves over 2^384 packets; what is
/// proved here is that the client **honours the verdict**, which is a property of this component.
///
/// The attempt count is the second half of it. A rejected reply is retried, because it may have
/// been a spoof that beat the real server; a kiss-o'-death is **not**, because retrying into one
/// is the abusive behaviour the packet exists to stop.
#[test_case]
fn a_reply_that_fails_validation_never_becomes_a_proposal() {
    let clock = clock();
    let before = clock.page().read();
    let claimed = clock.wall_nanos() + NANOS_PER_SEC / 2;

    for (variant, code, attempts, what) in [
        (
            srv::BAD_ORIGIN,
            reject::ORIGIN_MISMATCH,
            ATTEMPTS,
            "an origin that is not the nonce we sent",
        ),
        (
            srv::KISS_OF_DEATH,
            reject::KISS_OF_DEATH,
            1,
            "a kiss-o'-death, which must not be retried",
        ),
        (
            srv::SHORT,
            reject::LENGTH,
            ATTEMPTS,
            "twenty bytes, which is not an NTP packet",
        ),
    ] {
        let (_, reported) = exchange(&clock, variant, claimed);
        assert_eq!(
            reported[0],
            rpt::REJECTED,
            "the client accepted {what} (report {reported:?})",
        );
        assert_eq!(reported[1], code, "the wrong check refused {what}");
        assert_eq!(
            reported[2], attempts,
            "the client made {} requests against {what}, not {attempts}",
            reported[2],
        );
        assert_eq!(
            clock.page().read(),
            before,
            "the clock page changed after {what}: a refused reply reached the offset",
        );
    }
}

/// **The bounds are the service's, and the client cannot argue with them.**
///
/// The client does everything right here: a valid sample, an honest proposal, a truthful report.
/// It is the *service* that says no, twice, and asymmetrically: two hours forward is refused for
/// walking past an expiry, and ten seconds backward is refused because moving backward makes
/// instants happen twice (DECISIONS §43). A compromised client can lie inside those bounds and
/// can do nothing else, which is the sentence this test is here to make true rather than
/// asserted.
#[test_case]
fn a_proposal_outside_the_policy_is_refused_by_the_service() {
    let clock = clock();
    let before = clock.page().read();

    for (claimed, want, what) in [
        (
            clock.wall_nanos() + 2 * policy::MAX_STEP_FORWARD_NANOS,
            status::REFUSED_TOO_FAR_FORWARD,
            "two hours forward",
        ),
        (
            clock.wall_nanos() - 10 * NANOS_PER_SEC,
            status::REFUSED_TOO_FAR_BACKWARD,
            "ten seconds backward",
        ),
    ] {
        let (_, reported) = exchange(&clock, srv::GOOD, claimed);
        assert_eq!(
            reported[0],
            rpt::SYNCED,
            "the client should have accepted the sample and proposed it ({what}, report \
             {reported:?}); the refusal under test is the service's, not the client's",
        );
        assert_eq!(
            reported[1], want,
            "the service answered {} to a server claiming {what}",
            reported[1],
        );
        assert_eq!(
            clock.page().read(),
            before,
            "the clock page changed on a refused proposal ({what})",
        );
    }
}

/// **The client holds no writable clock page, and knowing where one would be buys it nothing.**
///
/// The same binary, the same five slots an NTP client is given, plus the exact address at which
/// a process holding the *set* authority maps the clock page. It reports the address, writes
/// there, and dies. The boundary is the mapping and not the layout, so there is no address at
/// which this write succeeds; this one is chosen because it is the address that would matter.
///
/// This is the claim Unix cannot make. `ntpd` runs as root: there is no address in a Unix system
/// its `settimeofday` cannot reach.
#[test_case]
fn an_ntp_client_holds_no_writable_clock_page() {
    let clock = clock();
    let before = clock.page().read();
    // An endpoint nobody serves: the probe never sends a request, and giving it a real server
    // would only add a process to the boot.
    let stack = crate::sched::create_endpoint();

    let faults = USER_FAULTS.load(Ordering::Relaxed);
    let report = ntp_service::start_probe(
        ntp_image(),
        stack,
        clock.propose,
        entropy(),
        clock_service::CLOCK_VA,
    );

    let [tag, va, ..] = crate::sched::ipc_recv(report);
    assert_eq!(tag, rpt::PROBING, "the probe never reached its write");
    assert_eq!(va, clock_service::CLOCK_VA);

    assert!(
        wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
        "an NTP client wrote the clock page at {va:#x} and was NOT stopped",
    );
    // The exact address, on both ISAs. This half used to be aarch64-only, because aarch64 had a
    // last-fault record (`FAR_EL1`, stashed for tests) and RISC-V had only a fault *count*.
    // Milestone 19's portable record keeps it on both. The *kind* is deliberately not asserted:
    // the probe holds no mapping of the clock page at all, so the fault is a translation fault,
    // and the claim being made here is about the address the client aimed at.
    assert_eq!(
        crate::arch::exceptions::last_user_fault().map(|(_, addr)| addr),
        Some(va),
        "something faulted, but not at the clock page's address",
    );
    assert_eq!(
        crate::sched::endpoint_waiting_senders(report),
        0,
        "the probe reported past its write: the write did not fault, so an NTP client set the \
         clock by hand",
    );
    assert_eq!(
        clock.page().read(),
        before,
        "the clock page changed while an NTP client was writing at it",
    );
}

/// **The nonce is unpredictable, because it comes from the entropy service.**
///
/// Two exchanges, and the server reports what was actually on the wire each time. Two things
/// follow, and the second is the one worth having: the two nonces differ, and neither of them is
/// a **time**. A client using `Query::new` would put its own clock in the transmit field, whose
/// seconds would be within a moment of now; a 64-bit draw from the device lands in a random one
/// of 2^32 seconds, so a false failure here is a 7201-in-2^32 event (about one boot in 600,000).
///
/// This is what `Query::with_nonce` is worth, and it is worth nothing if the bits are guessable:
/// before the entropy service landed, the only source here was splitmix64 seeded off the virtual
/// counter, which notes/entropy.md calls predictable to anyone who can guess boot-relative time.
#[test_case]
fn the_nonce_on_the_wire_is_random_and_is_not_the_clock() {
    let clock = clock();
    let claimed = clock.wall_nanos() + NANOS_PER_SEC / 2;

    // A time in the NTP era the transmit field would carry if it were a clock reading.
    let now_ntp_secs = clock.wall_nanos() / NANOS_PER_SEC + ntp_proto::UNIX_DELTA;

    let mut nonces = [0u64; 2];
    for slot in nonces.iter_mut() {
        let (served, reported) = exchange(&clock, srv::GOOD, claimed);
        assert_eq!(served[0], rpt::SERVED);
        assert_eq!(reported[0], rpt::SYNCED, "report {reported:?}");
        *slot = served[1];
    }

    assert_ne!(
        nonces[0], nonces[1],
        "two exchanges put the same nonce on the wire: the transmit field is not random",
    );
    for (i, &nonce) in nonces.iter().enumerate() {
        let secs = nonce >> 32;
        assert!(
            secs.abs_diff(now_ntp_secs) > 3600,
            "nonce {i} decodes to {secs}, within an hour of the clock ({now_ntp_secs}): the \
             transmit field is a timestamp, not 64 random bits",
        );
    }
}

/// **No entropy capability, no request.**
///
/// DECISIONS §42's no-silent-degradation rule where degrading quietly is worst. The client is
/// wired with slot 4 empty, which is what "no entropy service" actually is, and it stops before
/// it touches the network: the server's report endpoint has no waiting sender, so no request was
/// ever built. The alternative, a quiet fall back to the counter-seeded stream, would hand an
/// off-path attacker the twelve bits `Query::with_nonce` exists to take away from them, and
/// nothing in the report would say so. It is the call `SystemRng` makes when it panics.
#[test_case]
fn without_entropy_the_client_refuses_rather_than_guessing() {
    let clock = clock();
    let before = clock.page().read();
    let server = ntp_service::start_server(ntp_image(), srv::GOOD, clock.wall_nanos());
    let report = ntp_service::start_client(
        ntp_image(),
        server.stack,
        clock.propose,
        None, // slot 4 empty
        SERVER_IP,
        SERVER_PORT,
    );

    // Bounded, because the failure this test guards against is a client that carries on: it
    // would block in the network it should never have reached, and an unbounded `ipc_recv` here
    // would report that as a watchdog hang rather than as the refusal that did not happen.
    assert!(
        wait_for(|| crate::sched::endpoint_waiting_senders(report) > 0),
        "the client neither refused nor reported: with no nonce it can trust, it went to the \
         network anyway and is blocked there",
    );
    let reported = crate::sched::ipc_recv(report);
    assert_eq!(
        reported[0],
        rpt::NO_ENTROPY,
        "the client proceeded without a source of unguessable bits (report {reported:?})",
    );
    assert_eq!(
        reported[1] as i64,
        abi::Error::NoSuchSlot as i64,
        "an ungranted slot must answer NoSuchSlot (-1), 'there is nothing there', not {}",
        reported[1] as i64,
    );
    assert_eq!(
        crate::sched::endpoint_waiting_senders(server.report),
        0,
        "the server saw a request: the client sent one without a nonce it could trust",
    );
    assert_eq!(clock.page().read(), before, "the clock moved anyway");
}
