use super::*;
use crate::sched;

/// The report protocol, matching user/src/suptree.rs (the same convention the net client's
/// selectors follow: userspace owns the definition, the test mirrors it).
const REPORT_INIT_DROPPED: u64 = 1;
const REPORT_SERVER_RAN: u64 = 2;
const REPORT_SUP_SAW_DEATH: u64 = 3;
const REPORT_SUP_GAVE_UP: u64 = 4;
const REPORT_FAILED: u64 = 9;

/// Pages in `root_supervisor`'s construction budget. It builds two servers out of this, splits the
/// spawner's budget from it, and then deletes it; the spawner's split is the only memory the tree
/// spends afterwards.
const ROOT_BUDGET_PAGES: u64 = 1024;

/// **Spawn the tree's root the way the kernel spawns init**, and return the report endpoint every
/// process in the tree holds a WRITE view of.
///
/// Deliberately the same endowment `spawn_init` gives (`INITRD_VA`, an untyped in slot 0, a report
/// endpoint in slot 1) so what is being tested is `root_supervisor`'s *choices*, not a privileged shortcut.
fn spawn_tree() -> sched::EpId {
    let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd region");
    let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);
    let bytes =
        program("root_supervisor").expect("no root_supervisor program in the initrd archive");
    let elf = Elf::parse(bytes).expect("root_supervisor is not loadable");

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
    let mut space = AddressSpace::new(content).expect("no memory for root_supervisor");
    map_segments(&mut space, &elf).expect("could not lay out root_supervisor");
    for k in 0..INIT_STACK_PAGES {
        space
            .map_new(USER_STACK_VA - k * FRAME_SIZE, Flags::user_data())
            .expect("could not map root_supervisor's stack");
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
    let aspace = readopt_user_aspace(space).expect("register the root_supervisor aspace");

    let report = sched::create_endpoint();
    let budget = crate::untyped::create(ROOT_BUDGET_PAGES).expect("no budget for root_supervisor");
    let tcb_region = crate::untyped::create(2).expect("no tcb region");
    let tid = sched::create_tcb(tcb_region).expect("no tcb");
    let s0 = sched::tcb_insert_cap(tid, crate::cap::untyped_root_cap(budget), None)
        .expect("insert budget");
    assert_eq!(s0, 0, "root_supervisor's budget must land in slot 0");
    let s1 = sched::tcb_insert_cap(
        tid,
        crate::cap::endpoint_cap(
            report,
            crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
        ),
        None,
    )
    .expect("insert report");
    assert_eq!(
        s1, 1,
        "root_supervisor's report endpoint must land in slot 1"
    );
    sched::configure_tcb(tid, elf.entry(), USER_STACK_TOP, aspace).expect("configure");
    sched::start_tcb(tid, [0, initrd_len, 0]).expect("start");
    report
}

/// How many reports a healthy run of the tree makes: init's drop, the first instance running, its
/// crash reaching the supervisor, the replacement running, and the replacement's clean exit
/// reaching the supervisor. Exactly five, which is itself an assertion: a sixth would mean the
/// supervisor restarted something it should have left finished, or a tier-one server died.
const EXPECTED_REPORTS: usize = 5;

/// **Run one tree from spawn to quiescence**, returning every report it made.
///
/// Runs it to the end rather than stopping at the first interesting message, for a reason worth
/// recording: a half-run tree keeps building processes in the background, and the first version of
/// these tests left one running, which broke a *later* test's thread accounting
/// (`destroy_force_kills_a_runaway` counts threads before and after). A test that leaves work
/// running is a test that fails somebody else.
///
/// The order of the five is not fixed (init's drop races the sub-server's first run), so callers
/// filter by kind; within a kind the order is causal and asserted.
fn run_tree() -> [[u64; 5]; EXPECTED_REPORTS] {
    let report = spawn_tree();
    let mut msgs = [[0u64; 5]; EXPECTED_REPORTS];
    for slot in msgs.iter_mut() {
        let msg = sched::ipc_recv(report);
        assert_ne!(
            msg[0], REPORT_FAILED,
            "the supervision tree could not be built: stage {}",
            msg[1]
        );
        assert_ne!(
            msg[0], REPORT_SUP_GAVE_UP,
            "the supervisor exhausted its retry budget ({} restarts): the replacement should \
             have survived",
            msg[1],
        );
        *slot = msg;
    }

    // Let the tree settle, then prove it has nothing more to say. A parked sender here means a
    // sixth report exists, which is how "the supervisor did not restart a finished server" is
    // proven without a blocking receive that would hang when the code is right.
    for _ in 0..400 {
        sched::yield_now();
    }
    assert_eq!(
        sched::endpoint_waiting_senders(report),
        0,
        "the tree made more than {EXPECTED_REPORTS} reports: something acted after the \
         sub-server finished",
    );
    msgs
}

/// Every report of one kind, in arrival order.
fn of_kind(msgs: &[[u64; 5]; EXPECTED_REPORTS], kind: u64) -> impl Iterator<Item = &[u64; 5]> {
    msgs.iter().filter(move |m| m[0] == kind)
}

/// **init drops its construction authority, and the drop is real.**
///
/// `root_supervisor` builds its two servers, deletes the wiring capabilities and then the untyped budget
/// itself, and immediately tries the two primitives that build things: retype a page, and retype a
/// kernel object. Both must fail, and they must fail with `NoSuchSlot` (there is nothing there)
/// rather than `NotPermitted` (there is something there and you may not use it), because the
/// capability is *gone*, not narrowed. That distinction is the whole difference between "we asked
/// init not to" and "init cannot."
///
/// It is reported from inside the process on purpose: what matters is what the *holder* can do,
/// and only the holder can ask.
#[test_case]
fn init_drops_its_construction_authority_and_cannot_build_again() {
    let msgs = run_tree();
    let dropped = of_kind(&msgs, REPORT_INIT_DROPPED)
        .next()
        .expect("init never reported dropping its budget");
    assert_eq!(
        dropped[1], 1,
        "init still built a page or a kernel object after deleting its untyped: the authority \
         was not actually dropped",
    );
    assert_eq!(
        dropped[2], 1,
        "using the dropped budget failed with error {} (negated), not NoSuchSlot: the slot \
         should be empty, not merely restricted",
        dropped[2],
    );
}

/// **A dead sub-server is restarted by its own supervisor, in userspace, and init cannot have
/// helped.**
///
/// The sequence: the sub-server runs as attempt 0 and crashes on a load from an unmapped address;
/// its supervisor receives the kernel's fault message, reaps the corpse through the spawner (§16
/// revocation), and asks for attempt 1; attempt 1 runs and exits cleanly; the supervisor reads
/// EXIT as "finished" and does **not** restart it again. Every decision in that paragraph is code
/// in an unprivileged process that holds no memory at all, and the kernel's whole contribution is
/// one message.
///
/// **How "without init's involvement" is proven, and why it is not a timing argument.** init has
/// no construction authority by then: it deleted its untyped, and the companion test above
/// confirms it can no longer use it. A process that cannot retype a page cannot have built the
/// replacement. Authority, not scheduling order, is the evidence.
#[test_case]
fn a_dead_sub_server_is_restarted_by_its_supervisor_not_by_init() {
    let msgs = run_tree();

    let mut ran = of_kind(&msgs, REPORT_SERVER_RAN);
    let first = ran.next().expect("the sub-server never ran at all");
    assert_eq!(first[1], 0, "the first instance should be attempt 0");
    let second = ran
        .next()
        .expect("the crashed sub-server was never restarted");
    assert_eq!(
        second[1], 1,
        "the replacement was not started as attempt 1: the supervisor's restart policy did not \
         run, or ran with the wrong state",
    );
    assert!(
        ran.next().is_none(),
        "a third instance ran: the supervisor restarted a server that had finished",
    );

    let mut deaths = of_kind(&msgs, REPORT_SUP_SAW_DEATH);
    let crash = deaths.next().expect("the supervisor saw no death");
    assert_eq!(
        crash[2],
        abi::fault::EVENT_FAULT,
        "the crash should reach the supervisor as a FAULT event",
    );
    assert_ne!(
        crash[1], 0,
        "the fault message carried no tid: the supervisor cannot tell who died",
    );
    // The other half of §26's "both events flow": a clean exit must arrive as EXIT, because that
    // is what lets a userspace policy tell "finished" from "crashed" without guessing.
    let finished = deaths
        .next()
        .expect("the replacement's clean exit never reached the supervisor");
    assert_eq!(
        finished[2],
        abi::fault::EVENT_EXIT,
        "attempt 1 exited cleanly, so the supervisor must see EXIT, not FAULT",
    );
}
