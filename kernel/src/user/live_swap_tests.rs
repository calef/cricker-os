use super::*;
use crate::sched;

/// The report protocol, matching user/src/swap.rs. Userspace owns the definition; the test
/// mirrors it, the same convention `authority_tests` and `c_seam_tests` follow.
const RPT_UP: u64 = 1;
const RPT_QUIESCED: u64 = 2;
const RPT_PROBE_SURVIVED: u64 = 3;
const RPT_STEP: u64 = 4;
const RPT_LOG: u64 = 5;
const RPT_CLIENT: u64 = 6;
const RPT_ATTACK: u64 = 7;
const RPT_DEATH: u64 = 8;
const RPT_SITE: u64 = 9;
const RPT_DRAINED: u64 = 10;
const RPT_REFUSED: u64 = 11;
const RPT_SURVEY: u64 = 12;
const RPT_UNCOLLECTABLE: u64 = 13;
const RPT_WEDGED: u64 = 14;
const RPT_FAILED: u64 = 99;

/// `component_plan::Refusal::Unprovided`'s wire code: the supervisor routes nothing to a role the
/// component's manifest declares. Mirrored here for the same reason every other constant in this
/// block is: userspace owns the definition.
const REFUSAL_UNPROVIDED: u64 = 1;

/// The operator's steps.
const STEP_BUILT: u64 = 1;
const STEP_DRAINED: u64 = 2;
const STEP_REVOKED: u64 = 3;
const STEP_STARTED: u64 = 4;
const STEP_REAPED: u64 = 5;

/// The operator's verdict bits (`swap::log_checks`).
const LOG_NO_GAP: u64 = 1 << 0;
const LOG_MONOTONE: u64 = 1 << 1;
const LOG_BOTH_VERSIONS: u64 = 1 << 2;
const LOG_REVOKE_ENFORCED: u64 = 1 << 3;

/// The client's verdict bits (`swap::client_checks`).
const CL_ALL_REPLIED: u64 = 1 << 0;
const CL_SEQ_ECHOED: u64 = 1 << 1;
const CL_DIGEST_CORRECT: u64 = 1 << 2;
const CL_ONE_TRANSITION: u64 = 1 << 3;
const CL_SPANNED_SWAP: u64 = 1 << 4;
const CL_WAS_BUFFERED: u64 = 1 << 5;
const CL_NONE_REFUSED: u64 = 1 << 6;
const CL_WAS_RELEASED: u64 = 1 << 7;

/// The roles, and the two versions.
const ROLE_DIRECT: u64 = 0;
const ROLE_QUEUED: u64 = 1;
const ROLE_HUNG: u64 = 2;
const V1: u64 = 1;
const V2: u64 = 2;
const REQUESTS: u64 = 64;

/// The request a wedging instance swallows (`swap_proto::WEDGE_SEQ`). The test asserts the swap
/// landed here, so both sides name one constant.
const WEDGE_SEQ: u64 = 24;

/// The device's virtual address in every component, matching `swap::DEV_VA`. The test asserts
/// the kernel's reported fault address against this, which is why both sides name one constant.
const DEV_VA: u64 = 0x0310_0000;

/// The console UART's physical address, matching `crate::console`. This is the device the
/// operator lends, takes back, and lends again.
#[cfg(target_arch = "aarch64")]
const UART_PHYS: u64 = 0x0900_0000; // PL011
#[cfg(target_arch = "riscv64")]
const UART_PHYS: u64 = 0x1000_0000; // NS16550

/// The operator's budget: five instance regions of forty pages plus its own scratch mappings and
/// their page tables.
///
/// Kept tight on purpose, and it is not merely tidiness. `untyped::create` takes a **contiguous**
/// run of frames and the suite runs three of these systems, on top of a dozen earlier tests that
/// each park an init holding an eight-megabyte region. An over-generous budget here fragments
/// the frame allocator enough that a *later, unrelated* test cannot get init's region, which is
/// how both of this milestone's memory failures surfaced: nowhere near their cause.
const SWAPPER_BUDGET_PAGES: u64 = 224;

/// How many reports one run can make before the test gives up waiting for the operator's final
/// verdict. Generous: the loop stops at `RPT_LOG`, and this is only the tripwire for a run that
/// never gets there. Raised from 24 with milestone 23's manifest, which adds one `RPT_REFUSED` per
/// run, and from 28 with the hung-component role, which adds three; a run that overflows this loses
/// the operator's verdict and fails for the wrong reason.
const MAX_REPORTS: usize = 40;

/// **Spawn the operator the way the kernel spawns init**, and return the report endpoint every
/// process in the run holds a WRITE view of.
///
/// Deliberately the same endowment `spawn_init` gives (the archive read-only at `INITRD_VA`, an
/// untyped in slot 0, a report endpoint in slot 1), **plus** the one thing this milestone is
/// about: a device capability in slot 2, `WRITE|GRANT`, exactly as init gets one at boot. So
/// what is under test is the operator's choices, not a privileged shortcut.
fn spawn_swapper(role: u64) -> (sched::EpId, u64, u64) {
    let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd region");
    let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);
    let bytes = program("swapper").expect("no swapper program in the initrd archive");
    let elf = Elf::parse(bytes).expect("swapper is not loadable");

    let content: u64 = elf
        .segments()
        .map(|seg| {
            let (s, e) = seg.page_range(FRAME_SIZE);
            (e - s) / FRAME_SIZE
        })
        .sum::<u64>()
        + 1
        + initrd_pages / 512
        + INIT_STACK_PAGES
        + 8;
    let mut space = AddressSpace::new(content).expect("no memory for swapper");
    map_segments(&mut space, &elf).expect("could not lay out swapper");
    for k in 0..INIT_STACK_PAGES {
        space
            .map_new(USER_STACK_VA - k * FRAME_SIZE, Flags::user_data())
            .expect("could not map swapper's stack");
    }
    for i in 0..initrd_pages {
        space
            .map_physical(
                INITRD_VA + i * FRAME_SIZE,
                initrd_start + i * FRAME_SIZE,
                Flags::user_rodata(),
            )
            .expect("could not map the initrd");
    }
    let aspace = readopt_user_aspace(space).expect("register the swapper aspace");

    let report = sched::create_endpoint();
    let budget = crate::untyped::create(SWAPPER_BUDGET_PAGES).expect("no budget for swapper");
    let tcb_region = crate::untyped::create(2).expect("no tcb region");
    let tid = sched::create_tcb(tcb_region).expect("no tcb");
    let s0 = sched::tcb_insert_cap(tid, crate::cap::untyped_root_cap(budget), None)
        .expect("insert budget");
    assert_eq!(s0, 0, "swapper's budget must land in slot 0");
    let s1 = sched::tcb_insert_cap(
        tid,
        crate::cap::endpoint_cap(
            report,
            crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
        ),
        None,
    )
    .expect("insert report");
    assert_eq!(s1, 1, "swapper's report endpoint must land in slot 1");
    let s2 = sched::tcb_insert_cap(
        tid,
        crate::cap::device_frame_cap(
            UART_PHYS,
            crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
        ),
        None,
    )
    .expect("insert device");
    assert_eq!(s2, 2, "swapper's device capability must land in slot 2");
    sched::configure_tcb(tid, elf.entry(), USER_STACK_TOP, aspace).expect("configure");
    sched::start_tcb(tid, [role, initrd_len, 0]).expect("start");
    (report, budget, tcb_region)
}

/// **Run one swap system to its own verdict**, returning every report it made.
///
/// Run to the end rather than stopping at the first interesting message, for the reason
/// `authority_tests::run_tree` records: a half-run system keeps building processes in the
/// background, and a test that leaves work running is a test that fails somebody else. The
/// operator's `RPT_LOG` is always its last word, so that is the stop condition.
fn run_swap(role: u64) -> ([[u64; 5]; MAX_REPORTS], usize) {
    let (report, budget, tcb_region) = spawn_swapper(role);
    let mut msgs = [[0u64; 5]; MAX_REPORTS];
    let mut n = 0;
    while n < MAX_REPORTS {
        let msg = sched::ipc_recv(report);
        assert_ne!(
            msg[0], RPT_FAILED,
            "the swap system could not be built: stage {}. Stages 1-4 are the archive and the \
             four program images, 5-10 the endpoints and the witness page, 11-16 the incumbent \
             and the client, 20-27 the swap itself, 30-33 the attacker, 40-51 the queued rung, \
             60-63 the component manifests (60 means an unsatisfiable declaration was WIRED), \
             70-87 the hung-component rung (81 means the incumbent did not announce its hang \
             with a CALL, so nothing held a reply capability on it).",
            msg[1],
        );
        assert_ne!(
            msg[0], RPT_PROBE_SURVIVED,
            "the outgoing instance read the device AFTER the operator revoked it and the read \
             succeeded: the revoke did not take, so there was a window with two owners of one \
             device's registers",
        );
        msgs[n] = msg;
        n += 1;
        if msg[0] == RPT_LOG {
            break;
        }
    }
    assert!(
        n < MAX_REPORTS,
        "the operator never reached its final verdict in {MAX_REPORTS} reports",
    );

    // Let the system settle, then prove it has nothing more to say. A parked sender here would
    // mean something acted after the operator had finished, which is how "the run left nothing
    // running" is proven without a blocking receive that would hang when the code is right.
    for _ in 0..400 {
        sched::yield_now();
    }
    assert_eq!(
        sched::endpoint_waiting_senders(report),
        0,
        "the swap system had more to say after the operator's final verdict",
    );

    // **The system reclaims itself**, and this is an assertion rather than housekeeping.
    //
    // The operator supervises every child it starts and collects every corpse through its
    // supervision endpoint (DECISIONS §32), which returns each instance's region to its budget.
    // So a reclaim of the budget can only *succeed* if all five of those splits are gone: §16
    // refuses a region whose children are still carved out of it. Success is the statement that
    // nothing leaked; the frame delta is the statement that the whole run came back.
    //
    // It also has to work, for a reason that has nothing to do with tidiness. `untyped::create`
    // takes a **contiguous** run of frames, these tests run three systems, and the first version
    // of this leaked all three, which fragmented the allocator badly enough that a *later* test
    // could not get init's own eight-megabyte region.
    //
    // `sched::reclaim_region` rather than `untyped::destroy` because these regions are *pinned*:
    // the operator retyped four endpoints and a frame out of its budget. Reclaiming a region
    // with objects in it is the §16 teardown, and it is the entry point the `Untyped::DESTROY`
    // syscall uses, so the test cannot succeed down a path userspace could not have taken.
    let before_reclaim = memory::free_frames();
    sched::reclaim_region(budget).expect(
        "the operator's budget would not reclaim: a child region is still carved out of it, so \
         the swap system leaked one of its components",
    );
    let recovered = memory::free_frames() - before_reclaim;
    assert_eq!(
        recovered, SWAPPER_BUDGET_PAGES as usize,
        "reclaiming the operator's budget returned {recovered} of {SWAPPER_BUDGET_PAGES} pages",
    );

    // The operator's own address space and TCB are **not** in that budget: the kernel built the
    // operator the way it builds init. They come home through the ordinary reaper, which with
    // per-CPU run queues (DECISIONS §28) runs when the core the operator died on next schedules,
    // and the boot thread yielding here cannot force that. So this is hygiene, deliberately not
    // asserted on: what this milestone is responsible for is the swap system's own memory,
    // which is what the assertion above covers. A `debug_assert!` against a free-frame count
    // sampled at the top of the run stood here until 2026-08-03 and flaked on CI, because the
    // only thing that could trip it was an *earlier* test's teardown landing mid-run, which is
    // nothing this test is responsible for. See the BUGS section of notes/live-replacement.md.
    let _ = sched::reclaim_region(tcb_region);
    (msgs, n)
}

/// Every report of one kind, in arrival order.
fn of_kind(msgs: &[[u64; 5]], kind: u64) -> impl Iterator<Item = &[u64; 5]> {
    msgs.iter().filter(move |m| m[0] == kind)
}

/// Did the operator report this step?
fn had_step(msgs: &[[u64; 5]], step: u64) -> bool {
    of_kind(msgs, RPT_STEP).any(|m| m[1] == step)
}

/// **A component this operator cannot provide for is refused before anything is built**
/// (milestone 23's component manifest; `crates/component_plan`, notes/component-manifest.md).
///
/// Asserted on both channels, because a mechanism that only worked for the one component it was
/// written against would not be a mechanism. On the direct channel the operator plans the queue
/// broker's declaration, which names `requests` and `backend`, and that channel routes neither; on
/// the queued channel it plans the console component's declaration, which names `uart`, and that
/// channel routes no device. Both are **real manifests against real routing tables**, not fixtures:
/// each is the other role's component, asked for by a supervisor that genuinely cannot satisfy it.
///
/// Two things are asserted, and the second is the one that matters. The refusal is the *typed* one
/// (a role went unrouted) rather than any old failure, and it arrives **ahead of every build step and
/// every instance that started**, which is what makes a manifest a request the supervisor may refuse
/// rather than an instruction it has to carry out half way. A supervisor that discovered the problem
/// after mapping a page would have already moved authority.
fn a_component_the_operator_cannot_provide_for_was_refused_first(msgs: &[[u64; 5]]) {
    let at = msgs.iter().position(|m| m[0] == RPT_REFUSED).expect(
        "the operator never reported a refusal, so nothing shows that an unsatisfiable \
             manifest is refused rather than wired",
    );
    assert_eq!(
        msgs[at][1], REFUSAL_UNPROVIDED,
        "the manifest was refused for the wrong reason (code {}, wanted Unprovided={}): the \
         refusal has to be \"this supervisor routes nothing to a role it declares\" or it is not \
         evidence about routing at all",
        msgs[at][1], REFUSAL_UNPROVIDED,
    );
    let first_build = msgs
        .iter()
        .position(|m| m[0] == RPT_STEP || m[0] == RPT_UP)
        .unwrap_or(usize::MAX);
    assert!(
        at < first_build,
        "the refusal arrived at report {at}, after the first build at report {first_build}: a \
         component that cannot be provided for must be refused before any authority has moved",
    );
}

/// **The flagship: a component is replaced under a client that is talking to it.**
///
/// The four steps all happen, in an order the operator chose, and then two independent
/// witnesses in two address spaces agree that the conversation was unbroken. The client is not
/// consulted about the swap and the operator is not consulted about the replies; each says only
/// what it saw.
#[test_case]
fn a_client_keeps_talking_while_the_server_underneath_it_is_replaced() {
    let (msgs, n) = run_swap(ROLE_DIRECT);
    let msgs = &msgs[..n];

    // Before the four steps: every component in this run was wired from its own declaration, and one
    // that this channel cannot provide for was refused with nothing built.
    a_component_the_operator_cannot_provide_for_was_refused_first(msgs);

    // The four steps, each on machinery that existed before this milestone.
    for (step, what) in [
        (STEP_BUILT, "build the replacement"),
        (STEP_DRAINED, "drain the incumbent"),
        (STEP_REVOKED, "revoke the device"),
        (STEP_STARTED, "start the replacement"),
    ] {
        assert!(
            had_step(msgs, step),
            "the operator never got as far as: {what}",
        );
    }

    // Both instances ran, and both could reach the device they were endowed with. The second
    // half matters as much as the first: an instance that answered every request while the
    // registers went unowned would look like a perfect swap.
    let mut ups = of_kind(msgs, RPT_UP);
    let first = ups.next().expect("the incumbent never started");
    let second = ups.next().expect("the replacement never started");
    assert!(ups.next().is_none(), "a third instance started");
    assert_eq!(first[1], V1, "the incumbent should be version 1");
    assert_eq!(second[1], V2, "the replacement should be version 2");
    assert!(
        first[2] == 1 && second[2] == 1,
        "an instance could not read the device it was endowed with, so the registers were not \
         where the swap thinks they were",
    );

    // The incumbent served a real share of the conversation before it was drained: a swap that
    // happened before anyone was talking would prove nothing.
    let quiesced = of_kind(msgs, RPT_QUIESCED)
        .next()
        .expect("the incumbent never acknowledged the drain");
    assert!(
        quiesced[2] > 0 && quiesced[2] < REQUESTS,
        "the incumbent served {} of {REQUESTS} requests: the swap did not land inside the \
         conversation",
        quiesced[2],
    );

    // Witness one: the client, from its own replies, in its own address space.
    let client = of_kind(msgs, RPT_CLIENT)
        .next()
        .expect("the client never reported a verdict");
    const CLIENT_UNBROKEN: u64 =
        CL_ALL_REPLIED | CL_SEQ_ECHOED | CL_DIGEST_CORRECT | CL_ONE_TRANSITION | CL_SPANNED_SWAP;
    assert_eq!(
        client[1] & CLIENT_UNBROKEN,
        CLIENT_UNBROKEN,
        "the client's stream was broken (verdict {:#x}): missing {:#x}. ALL_REPLIED={}, \
         SEQ_ECHOED={}, DIGEST_CORRECT={}, ONE_TRANSITION={}, SPANNED_SWAP={}",
        client[1],
        CLIENT_UNBROKEN & !client[1],
        client[1] & CL_ALL_REPLIED != 0,
        client[1] & CL_SEQ_ECHOED != 0,
        client[1] & CL_DIGEST_CORRECT != 0,
        client[1] & CL_ONE_TRANSITION != 0,
        client[1] & CL_SPANNED_SWAP != 0,
    );

    // Witness two: the operator, from the shared page, after every writer is dead.
    let log = of_kind(msgs, RPT_LOG)
        .next()
        .expect("the operator never reported its verdict");
    const LOG_CLEAN: u64 = LOG_NO_GAP | LOG_MONOTONE | LOG_BOTH_VERSIONS | LOG_REVOKE_ENFORCED;
    assert_eq!(
        log[1] & LOG_CLEAN,
        LOG_CLEAN,
        "the operator's log says the swap was not clean (verdict {:#x}): NO_GAP={} (a request \
         nobody served), MONOTONE={} (the old instance answered after the new one, so two \
         owners), BOTH_VERSIONS={}, REVOKE_ENFORCED={} (the post-revoke device read did not \
         fault where it should have)",
        log[1],
        log[1] & LOG_NO_GAP != 0,
        log[1] & LOG_MONOTONE != 0,
        log[1] & LOG_BOTH_VERSIONS != 0,
        log[1] & LOG_REVOKE_ENFORCED != 0,
    );
    // The two witnesses agree on *where* the swap happened, which is the cross-check that makes
    // each of them evidence rather than a self-report.
    assert_eq!(
        log[2], client[2],
        "the operator's log and the client's replies disagree about which request the \
         replacement took over at ({} vs {})",
        log[2], client[2],
    );

    // The control: the outgoing instance died faulting on the device it no longer had, at the
    // device's own virtual address. `run_swap` has already refused a run in which that read
    // succeeded.
    let death = of_kind(msgs, RPT_DEATH)
        .next()
        .expect("the outgoing instance never died");
    assert_eq!(
        death[2],
        abi::fault::EVENT_FAULT,
        "the outgoing instance should have faulted on the revoked device, not exited cleanly",
    );
    let site = of_kind(msgs, RPT_SITE)
        .next()
        .expect("no fault site was reported");
    assert_eq!(
        site[1] & !(FRAME_SIZE - 1),
        DEV_VA,
        "the outgoing instance faulted at {:#x}, which is not in the device page {DEV_VA:#x}: \
         it died of something other than the revoke, which would make the rest of this test \
         vacuous",
        site[1],
    );
}

/// **The attacker holds a real capability to the stable endpoint and still cannot be the
/// server.**
///
/// The milestone rests on endpoint-only naming, and the obvious worry about it is that a name
/// with no peer in it is a name anybody can answer to. It is not: `SEND` and `RECV` are gated by
/// different rights on the same object, so the same endpoint handed out two ways is a one-way
/// pipe in whichever direction each holder was trusted with. The attacker is endowed with
/// *exactly* what the honest client holds, so the refusal is about rights and not about
/// wiring.
#[test_case]
fn a_client_of_the_stable_endpoint_cannot_become_its_server() {
    let (msgs, n) = run_swap(ROLE_DIRECT);
    let attack = of_kind(&msgs[..n], RPT_ATTACK)
        .next()
        .expect("the attacker never reported");
    assert_eq!(
        attack[1],
        (-(abi::Error::NotPermitted as i64)) as u64,
        "a client of the stable endpoint received on it (or was refused for the wrong reason): \
         error {}, wanted NotPermitted. If this succeeded, any holder of a request capability \
         could impersonate the component.",
        attack[1] as i64,
    );
}

/// **The opt-in rung: a producer keeps producing while no backend exists at all.**
///
/// The direct rung's down window costs the caller a block: its request is safe (it parks on the
/// endpoint's sender queue and the next server drains it) but it is stopped until then. For a
/// channel that cannot afford that, `broker` takes custody. The price is one extra hop on
/// every request in the steady state, which is why it is chosen per channel and never by
/// default; `broker_rtt` in bench/baseline-aarch64.txt is that price.
///
/// What this proves that the direct test does not: there is a window here in which the backend
/// **does not exist** (it was quiesced, it died, and its corpse was collected before the
/// replacement was built), the producer kept calling through it, and every item it handed over
/// turns up in the new backend's log, in order.
#[test_case]
fn a_producer_never_blocks_on_an_absent_consumer_and_loses_nothing() {
    let (msgs, n) = run_swap(ROLE_QUEUED);
    let msgs = &msgs[..n];

    // The manifest mechanism's other side: on this channel the console component's declaration is
    // the one that cannot be satisfied, because no device is routed here.
    a_component_the_operator_cannot_provide_for_was_refused_first(msgs);

    let producer = of_kind(msgs, RPT_CLIENT)
        .next()
        .expect("the producer never reported a verdict");
    const PRODUCER_OK: u64 =
        CL_ALL_REPLIED | CL_SEQ_ECHOED | CL_DIGEST_CORRECT | CL_NONE_REFUSED | CL_WAS_BUFFERED;
    assert_eq!(
        producer[1] & PRODUCER_OK,
        PRODUCER_OK,
        "the queued producer's run was not clean (verdict {:#x}): NONE_REFUSED={} (the queue \
         overflowed or a request was rejected), WAS_BUFFERED={} (nothing was ever buffered, so \
         the producer never actually spanned a window with no backend)",
        producer[1],
        producer[1] & CL_NONE_REFUSED != 0,
        producer[1] & CL_WAS_BUFFERED != 0,
    );

    // The broker's own account: it drained everything it ever took custody of.
    let drained = of_kind(msgs, RPT_DRAINED)
        .next()
        .expect("the broker never reported a drain");
    assert!(drained[1] > 0, "the broker drained nothing");
    assert_eq!(
        drained[1], producer[2],
        "the broker drained {} items but the producer said it had handed over {}",
        drained[1], producer[2],
    );

    // And the backend's log, read by the operator after both backends are gone: every item, in
    // order, served by somebody, with the version changing exactly where the swap was.
    let log = of_kind(msgs, RPT_LOG)
        .next()
        .expect("the operator never reported its verdict");
    const LOG_CLEAN: u64 = LOG_NO_GAP | LOG_MONOTONE | LOG_BOTH_VERSIONS;
    assert_eq!(
        log[1] & LOG_CLEAN,
        LOG_CLEAN,
        "the queued channel lost or reordered work (verdict {:#x}): NO_GAP={}, MONOTONE={}, \
         BOTH_VERSIONS={}",
        log[1],
        log[1] & LOG_NO_GAP != 0,
        log[1] & LOG_MONOTONE != 0,
        log[1] & LOG_BOTH_VERSIONS != 0,
    );
}

// ===============================================================================================
// Milestone 23's third residual: a component that stops answering **without dying**.
// ===============================================================================================

/// `abi::survey`'s state codes, unpacked from `RPT_SURVEY` by `swap_proto::survey_counts`'s layout.
/// Mirrored here for the reason every other constant in this file is: userspace owns the definition.
fn survey_blocked(w: u64) -> u64 {
    (w >> 16) & 0xff
}
fn survey_dead(w: u64) -> u64 {
    (w >> 24) & 0xff
}
fn survey_awake(w: u64) -> u64 {
    (w & 0xff) + ((w >> 8) & 0xff)
}

/// **A component that stops answering without dying is invisible to its supervisor, and the service
/// can be restored anyway** (milestone 23's third residual; notes/hung-component.md).
///
/// Every failure this system handles elsewhere is a *death*: the kernel witnesses a fault or an exit,
/// stamps a five-word message onto the supervision endpoint (DECISIONS §26), `Endpoint::REAP`
/// collects the corpse (§32), and the region comes home. A component that merely stops answering
/// produces none of it, and this test is the machine saying so rather than a paragraph claiming it.
///
/// **Four results, and the first two are negative.**
///
/// 1. **The domain does not report a hang.** Every member of the survey is `BLOCKED`, none is `DEAD`,
///    and that is byte for byte what a healthy idle system reads as: `abi::survey::BLOCKED` is the
///    state of a server parked in `RECV_CAP`, which is every healthy server between requests. The
///    view milestone 126 built is the widest one a supervisor has and it cannot tell the difference.
/// 2. **The supervisor's whole vocabulary over its domain is refused.** `Endpoint::REAP` is asked
///    about every member and answers `StillAlive` every time, on purpose: §32 authorizes collecting a
///    corpse and not killing. Against a hung component there is no corpse, so there is nothing to
///    say.
/// 3. **The service is restored with no authority the operator did not already hold**, which
///    contradicts §32's sentence that a supervisor restarting a hung child "still needs the stronger
///    right". The device comes back by `Frame::REVOKE` take-back, which asks the holder for nothing;
///    the replacement parks on the stable endpoint and drains what queued behind the silence; the
///    client's stream closes over the hang. §32 is right about *reclaiming the hung component's
///    memory* and wrong about restarting its service, and those are different acts.
/// 4. **Restoring the service does not recover the caller that was mid-`CALL`.** That caller is
///    parked awaiting a reply, and a caller awaiting a reply is woken by `sched::ipc_reply` and by
///    nothing else in the kernel: `abi::Error::Gone` reaches a caller whose *endpoint* died and never
///    one whose *server is alive and silent*. The one-shot `Reply` capability naming it is `WRITE`
///    without `GRANT`, inside the hung component's cspace, so the operator cannot answer on its
///    behalf, forge one, or revoke its way to it. In this run the wedge is deliberate and lets go
///    when asked. A real one does not, and `CL_WAS_RELEASED` is the bit that marks which of the two
///    this was.
///
/// **Nothing here waits on a clock**, and that is deliberate rather than incidental: the wedge fires
/// on the identity of one request, the hang is a `CALL` whose parked state is established inside the
/// same critical section that wakes the operator, and every assertion below is about program order.
/// A watchdog that decided "hung" from elapsed time would need the timed wait milestone 106 has not
/// decided, and a *test* that did would be the next load-sensitive assertion (milestone 62, 78).
#[test_case]
fn a_component_that_stops_answering_without_dying_is_invisible_to_its_supervisor() {
    let (msgs, n) = run_swap(ROLE_HUNG);
    let msgs = &msgs[..n];

    // The hang landed inside a real conversation. A component that stopped answering before anyone
    // was talking, or after everyone had finished, would make the rest of this vacuous.
    let wedged = of_kind(msgs, RPT_WEDGED)
        .next()
        .expect("the incumbent never stopped answering, so this run tested the healthy path");
    assert_eq!(wedged[1], V1, "the wrong instance wedged");
    assert!(
        wedged[2] > 0 && wedged[2] < REQUESTS,
        "the incumbent had served {} of {REQUESTS} requests when it stopped answering: the hang \
         did not land inside the conversation",
        wedged[2],
    );

    // ---------------------------------------------------------------------------------------
    // Result 1: the domain does not report a hang.
    // ---------------------------------------------------------------------------------------

    let survey = of_kind(msgs, RPT_SURVEY)
        .next()
        .expect("the operator never surveyed its domain");
    assert_ne!(
        survey[1],
        u64::MAX,
        "the operator was refused its own domain (error {}): it retyped this endpoint out of its \
         own budget, so it holds ENUMERATE and this is a bug in the survey rather than a finding",
        survey[2],
    );
    assert_eq!(
        survey[1], 2,
        "the domain should hold exactly the incumbent and the client ({} reported). The \
         replacement is built but never started, and a thread's supervision endpoint is recorded \
         at START, so an embryo is not yet a member.",
        survey[1],
    );
    assert_eq!(
        survey_blocked(survey[2]),
        2,
        "the survey found {} blocked members and {} awake, of 2. **This assertion is the finding**: \
         the hung component and the caller it stranded both read as BLOCKED, which is exactly what \
         a healthy server waiting for work and a client waiting for its answer read as. If this \
         ever fails because a member read READY or RUNNING, the wedge stopped being a CALL and the \
         test is measuring a race instead of a state.",
        survey_blocked(survey[2]),
        survey_awake(survey[2]),
    );
    assert_eq!(
        survey_dead(survey[2]),
        0,
        "the survey reported {} dead members: a hang is not a death, and if the kernel is calling \
         it one then the rest of this test is asserting the wrong thing",
        survey_dead(survey[2]),
    );

    // No death message had arrived by then either, which is the other half of "nothing noticed":
    // the operator reports every death it collects, and the first of those must come after the
    // survey. A supervisor blocked in RECV on its supervision endpoint would simply never wake.
    let at_survey = msgs.iter().position(|m| m[0] == RPT_SURVEY).unwrap();
    let first_death = msgs
        .iter()
        .position(|m| m[0] == RPT_DEATH)
        .unwrap_or(usize::MAX);
    assert!(
        at_survey < first_death,
        "a death reached the operator (report {first_death}) before it surveyed the hang (report \
         {at_survey}): something died, so this was not the hung case",
    );

    // ---------------------------------------------------------------------------------------
    // Result 2: every member refuses to be collected.
    // ---------------------------------------------------------------------------------------

    let uncollectable = of_kind(msgs, RPT_UNCOLLECTABLE)
        .next()
        .expect("the operator never tried to collect its domain");
    assert_eq!(
        uncollectable[1], 2,
        "the operator asked about {} members, expected 2",
        uncollectable[1],
    );
    assert_eq!(
        uncollectable[2], uncollectable[1],
        "{} of {} members refused collection with StillAlive. Every one must: Endpoint::REAP \
         authorizes collecting a corpse and refuses a live thread (DECISIONS §32), so a supervisor \
         facing a hang holds a verb with nothing to apply it to. A member that was collectable \
         here would mean something had died.",
        uncollectable[2], uncollectable[1],
    );

    // ---------------------------------------------------------------------------------------
    // Result 3: the service comes back, and step 2 of the four never happened.
    // ---------------------------------------------------------------------------------------

    for (step, what) in [
        (STEP_BUILT, "build the replacement"),
        (
            STEP_REVOKED,
            "take the device back from the live, wedged holder",
        ),
        (STEP_STARTED, "start the replacement on the stable endpoint"),
        (STEP_REAPED, "collect the incumbent once it finally died"),
    ] {
        assert!(
            had_step(msgs, step),
            "the operator never got as far as: {what}"
        );
    }
    assert!(
        !had_step(msgs, STEP_DRAINED),
        "the operator reported a drain: OP_QUIESCE needs the incumbent to answer, which is the one \
         thing a hung component does not do. A run that drained did not test a hang.",
    );

    // Both instances ran and both reached the device, so the registers were where the swap thinks
    // they were on each side of a revoke that the outgoing holder never consented to.
    let mut ups = of_kind(msgs, RPT_UP);
    let first = ups.next().expect("the incumbent never started");
    let second = ups.next().expect("the replacement never started");
    assert_eq!(first[1], V1);
    assert_eq!(second[1], V2);
    assert!(
        first[2] == 1 && second[2] == 1,
        "an instance could not read the device it was endowed with",
    );

    // The client's own verdict, from its own replies, in its own address space. Every check the
    // healthy channel makes still holds: a hang cost this client nothing but a detour.
    let client = of_kind(msgs, RPT_CLIENT)
        .next()
        .expect("the client never reported a verdict, so it is still parked inside its call");
    const CLIENT_UNBROKEN: u64 =
        CL_ALL_REPLIED | CL_SEQ_ECHOED | CL_DIGEST_CORRECT | CL_ONE_TRANSITION | CL_SPANNED_SWAP;
    assert_eq!(
        client[1] & CLIENT_UNBROKEN,
        CLIENT_UNBROKEN,
        "the client's stream was broken across the hang (verdict {:#x}): missing {:#x}",
        client[1],
        CLIENT_UNBROKEN & !client[1],
    );

    // The operator's independent witness: the shared page, read after every writer is dead.
    let log = of_kind(msgs, RPT_LOG)
        .next()
        .expect("the operator never reported its verdict");
    const LOG_CLEAN: u64 = LOG_NO_GAP | LOG_MONOTONE | LOG_BOTH_VERSIONS | LOG_REVOKE_ENFORCED;
    assert_eq!(
        log[1] & LOG_CLEAN,
        LOG_CLEAN,
        "the hung channel's log is not clean (verdict {:#x}): NO_GAP={} (a request nobody ever \
         served, so the swallowed one was never re-asked), MONOTONE={} (the wedged instance \
         answered after the replacement had, so there were two owners), BOTH_VERSIONS={}, \
         REVOKE_ENFORCED={} (the wedged instance's post-revoke device read did not fault, so the \
         take-back did not reach a live holder)",
        log[1],
        log[1] & LOG_NO_GAP != 0,
        log[1] & LOG_MONOTONE != 0,
        log[1] & LOG_BOTH_VERSIONS != 0,
        log[1] & LOG_REVOKE_ENFORCED != 0,
    );

    // ---------------------------------------------------------------------------------------
    // Result 4: the caller was stranded, and only the component that stranded it could let go.
    // ---------------------------------------------------------------------------------------

    assert_ne!(
        client[1] & CL_WAS_RELEASED,
        0,
        "the client never saw WEDGE_RELEASED (verdict {:#x}), so its call was answered normally \
         and it was never stranded. Without that bit this run is indistinguishable from an \
         ordinary swap that happened between two calls, which proves nothing about a hang.",
        client[1],
    );
    // The swap landed exactly at the swallowed request, on both witnesses. Not a coincidence and
    // not a range: the request the incumbent took and never answered is the first one the
    // replacement served, because it is the one the client asked again for.
    assert_eq!(
        client[2], WEDGE_SEQ,
        "the client says the version changed at request {}, but the incumbent swallowed request \
         {WEDGE_SEQ}",
        client[2],
    );
    assert_eq!(
        log[2], client[2],
        "the operator's log and the client's replies disagree about where the replacement took \
         over ({} vs {})",
        log[2], client[2],
    );
    // And the release came after the service was already back, which is what makes the two
    // recoveries separable rather than one act: restoring a service and recovering a caller are
    // different problems, and only the second needed the hung component's cooperation.
    let at_started = msgs
        .iter()
        .position(|m| m[0] == RPT_STEP && m[1] == STEP_STARTED)
        .unwrap();
    let at_client = msgs.iter().position(|m| m[0] == RPT_CLIENT).unwrap();
    assert!(
        at_started < at_client,
        "the client finished (report {at_client}) before the replacement was started (report \
         {at_started}): the hang did not actually block it",
    );
}
