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
const RPT_FAILED: u64 = 99;

/// The operator's steps.
const STEP_BUILT: u64 = 1;
const STEP_DRAINED: u64 = 2;
const STEP_REVOKED: u64 = 3;
const STEP_STARTED: u64 = 4;

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

/// The roles, and the two versions.
const ROLE_DIRECT: u64 = 0;
const ROLE_QUEUED: u64 = 1;
const V1: u64 = 1;
const V2: u64 = 2;
const REQUESTS: u64 = 64;

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
/// never gets there.
const MAX_REPORTS: usize = 24;

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
             and the client, 20-27 the swap itself, 30-33 the attacker, 40-51 the queued rung.",
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
