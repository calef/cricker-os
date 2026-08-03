use super::*;
use crate::sched;

/// The report protocol, matching `crates/c_seam`. Userspace owns the definition; the test
/// mirrors it, the same convention `authority_tests` and the net client's selectors follow.
const RPT_RAN: u64 = 1;
const RPT_DEATH: u64 = 2;
const RPT_SITE: u64 = 3;
const RPT_VERDICT: u64 = 4;
const RPT_FAILED: u64 = 9;

/// The verdict bits, matching `crates/c_seam`'s `checks` module.
const IN_GRANT_WRITE_LANDED: u64 = 1 << 0;
const WITNESS_RO_INTACT: u64 = 1 << 1;
const WITNESS_FAR_INTACT: u64 = 1 << 2;
const FAULT_ADDR_AS_EXPECTED: u64 = 1 << 3;
const OUTPUT_CORRECT: u64 = 1 << 4;

/// What a run of one faulting attempt must report: the component's own store landed, both
/// witnesses survived, and the fault was where the C code pointed.
const CONFINED: u64 =
    IN_GRANT_WRITE_LANDED | WITNESS_RO_INTACT | WITNESS_FAR_INTACT | FAULT_ADDR_AS_EXPECTED;
/// What the honest attempt must report: all of the above, plus a correct answer.
const CONFINED_AND_CORRECT: u64 = CONFINED | OUTPUT_CORRECT;

/// The attempts, matching `crates/c_seam`.
const ATTEMPTS: usize = 3;
const ATTEMPT_HONEST: u64 = 2;

/// The confiner's budget. It builds three instances of the shim, one at a time, reaping each
/// before the next, so this covers one instance region plus its own scratch mappings rather
/// than three.
const CONFINER_BUDGET_PAGES: u64 = 512;

/// Four reports per attempt (ran, death, site, verdict).
const EXPECTED_REPORTS: usize = 4 * ATTEMPTS;

/// **Spawn the confiner the way the kernel spawns init**, and return the report endpoint every
/// process in the run holds a WRITE view of. Deliberately the same endowment `spawn_init` gives
/// (the archive read-only at `INITRD_VA`, an untyped in slot 0, a report endpoint in slot 1), so
/// what is under test is the seam rather than a privileged shortcut.
fn spawn_confiner() -> sched::EpId {
    let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd region");
    let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);
    let bytes = program("c_confiner").expect("no c_confiner program in the initrd archive");
    let elf = Elf::parse(bytes).expect("c_confiner is not loadable");

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
    let mut space = AddressSpace::new(content).expect("no memory for c_confiner");
    map_segments(&mut space, &elf).expect("could not lay out c_confiner");
    for k in 0..INIT_STACK_PAGES {
        space
            .map_new(USER_STACK_VA - k * FRAME_SIZE, Flags::user_data())
            .expect("could not map c_confiner's stack");
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
    let aspace = readopt_user_aspace(space).expect("register the c_confiner aspace");

    let report = sched::create_endpoint();
    let budget = crate::untyped::create(CONFINER_BUDGET_PAGES).expect("no budget for c_confiner");
    let tcb_region = crate::untyped::create(2).expect("no tcb region");
    let tid = sched::create_tcb(tcb_region).expect("no tcb");
    let s0 = sched::tcb_insert_cap(tid, crate::cap::untyped_root_cap(budget), None)
        .expect("insert budget");
    assert_eq!(s0, 0, "c_confiner's budget must land in slot 0");
    let s1 = sched::tcb_insert_cap(
        tid,
        crate::cap::endpoint_cap(
            report,
            crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
        ),
        None,
    )
    .expect("insert report");
    assert_eq!(s1, 1, "c_confiner's report endpoint must land in slot 1");
    sched::configure_tcb(tid, elf.entry(), USER_STACK_TOP, aspace).expect("configure");
    sched::start_tcb(tid, [0, initrd_len, 0]).expect("start");
    report
}

/// **Run one confiner from spawn to quiescence**, returning every report it made.
///
/// Run to the end rather than stopping at the first interesting message, for the reason
/// `authority_tests::run_tree` records: a half-run harness keeps building processes in the
/// background, and a test that leaves work running is a test that fails somebody else.
fn run_seam() -> [[u64; 5]; EXPECTED_REPORTS] {
    let report = spawn_confiner();
    let mut msgs = [[0u64; 5]; EXPECTED_REPORTS];
    for slot in msgs.iter_mut() {
        let msg = sched::ipc_recv(report);
        assert_ne!(
            msg[0], RPT_FAILED,
            "the C seam harness could not be built: stage {}. Stages 1-2 are the archive and \
             the c_shim ELF, 3-6 the shared pages, 7 the supervision endpoint, 10-13 building, \
             starting, and reaping an instance.",
            msg[1],
        );
        *slot = msg;
    }

    // Let it settle, then prove there is nothing more to say. A parked sender here means a
    // thirteenth report exists, which is how "the supervisor did not restart the instance that
    // finished" is proven without a blocking receive that would hang when the code is right.
    for _ in 0..400 {
        sched::yield_now();
    }
    assert_eq!(
        sched::endpoint_waiting_senders(report),
        0,
        "the run made more than {EXPECTED_REPORTS} reports: the supervisor acted after the \
         honest attempt finished",
    );
    msgs
}

/// Every report of one kind, in arrival order.
fn of_kind(msgs: &[[u64; 5]; EXPECTED_REPORTS], kind: u64) -> impl Iterator<Item = &[u64; 5]> {
    msgs.iter().filter(move |m| m[0] == kind)
}

/// **A C out-of-bounds write faults the process and touches nothing outside its grant.** The
/// milestone's one load-bearing test.
///
/// Two bugs, two fault paths, two witnesses. The off-by-one store lands on a page mapped
/// read-only into the component and read/write into the confiner: same physical memory, present
/// in the offender's page tables, and provably unchanged, which is the strongest form the claim
/// can take. The wild store a page further out lands on a virtual address the component has no
/// mapping for and the confiner does: a different frame, the same number, and unchanged.
///
/// The verdict bitmap is asserted for **equality**, not for containing the interesting bits,
/// because a missing bit is exactly what a broken confinement looks like and a superset would
/// mean the checker started answering a question nobody asked.
#[test_case]
fn a_c_out_of_bounds_write_faults_and_changes_nothing_outside_its_grant() {
    let msgs = run_seam();

    let mut verdicts = of_kind(&msgs, RPT_VERDICT);
    for attempt in 0..2u64 {
        let v = verdicts
            .next()
            .unwrap_or_else(|| panic!("no verdict for attempt {attempt}"));
        assert_eq!(v[1], attempt, "verdicts arrived out of order");
        assert_eq!(
            v[2],
            CONFINED,
            "attempt {attempt}: the C component was not confined. Bits that should be set and \
             are not: in-grant store landed {}, read-only witness intact {}, unmapped witness \
             intact {}, fault address as expected {}.",
            v[2] & IN_GRANT_WRITE_LANDED != 0,
            v[2] & WITNESS_RO_INTACT != 0,
            v[2] & WITNESS_FAR_INTACT != 0,
            v[2] & FAULT_ADDR_AS_EXPECTED != 0,
        );
    }

    // The deaths themselves: a real fault, not a cooperative exit, and a tid the supervisor can
    // trust because the kernel is the only sender on this path (§26.5).
    let mut deaths = of_kind(&msgs, RPT_DEATH);
    for attempt in 0..2u64 {
        let d = deaths
            .next()
            .unwrap_or_else(|| panic!("attempt {attempt} produced no death message"));
        assert_eq!(
            d[2],
            abi::fault::EVENT_FAULT,
            "attempt {attempt} did not fault: the out-of-bounds write was tolerated, or the \
             process exited some other way",
        );
        assert_ne!(
            d[1], 0,
            "attempt {attempt}'s fault message carried no tid: the supervisor cannot tell who \
             died",
        );
    }

    // And the fault sites are real addresses in real code, not zeroed placeholders. The
    // *equality* to the intended address is checked inside the confiner (which knows the
    // layout); this is the sanity check that the kernel filled both words at all.
    let mut sites = of_kind(&msgs, RPT_SITE);
    for attempt in 0..2u64 {
        let s = sites
            .next()
            .unwrap_or_else(|| panic!("attempt {attempt} produced no fault site"));
        assert_ne!(
            s[1], 0,
            "attempt {attempt}: the kernel reported no faulting pc"
        );
        assert_ne!(
            s[2], 0,
            "attempt {attempt}: the kernel reported no faulting address"
        );
    }
}

/// **The supervisor restarts the crashed C component, and the restarted component does real
/// work.**
///
/// The restart half of §26, with a foreign component on the end of it. Three instances run in
/// sequence: two crash, and the third computes a checksum and a transform in C, writes them
/// into the shared grant, and exits cleanly. The confiner checks that output against an
/// independent Rust implementation of the same definition, which is why `OUTPUT_CORRECT` is
/// worth a bit of its own: "an instance ran" is cheap, "the C produced the right answer after
/// two crashes" is the claim.
///
/// The clean exit must arrive as `EVENT_EXIT` and must **not** be restarted, which is the other
/// half of "both events flow" (§26.3) and is what ends the run.
#[test_case]
fn the_supervisor_restarts_the_faulted_c_component_and_the_restart_does_real_work() {
    let msgs = run_seam();

    // Every attempt reached its C call, in order, including the two that came after a crash.
    // (No `Vec` here: this is the kernel, and there is no allocator in a test.)
    let mut ran = [u64::MAX; ATTEMPTS];
    let mut count = 0usize;
    for m in of_kind(&msgs, RPT_RAN) {
        if count < ATTEMPTS {
            ran[count] = m[1];
        }
        count += 1;
    }
    assert_eq!(
        count, ATTEMPTS,
        "expected {ATTEMPTS} instances of the C component to run, saw {count}: the supervisor \
         did not restart a crashed child, or restarted one it should have left finished",
    );
    for (i, &attempt) in ran.iter().enumerate() {
        assert_eq!(
            attempt, i as u64,
            "instance {i} reported itself as attempt {attempt}: the supervisor's restart policy \
             ran with the wrong state",
        );
    }

    // The honest attempt: confined like the others, and correct.
    let honest = of_kind(&msgs, RPT_VERDICT)
        .find(|v| v[1] == ATTEMPT_HONEST)
        .expect("the honest attempt produced no verdict");
    assert_eq!(
        honest[2],
        CONFINED_AND_CORRECT,
        "the restarted C component was reached but did not produce the right answer (output \
         correct: {}). A restart that revives a process which computes nothing is not a restart.",
        honest[2] & OUTPUT_CORRECT != 0,
    );

    // A clean exit reads as EXIT, which is what lets a userspace policy tell "finished" from
    // "crashed" without guessing.
    let last = of_kind(&msgs, RPT_DEATH)
        .last()
        .expect("no death messages at all");
    assert_eq!(
        last[2],
        abi::fault::EVENT_EXIT,
        "the honest attempt exited cleanly, so its supervisor must see EXIT, not FAULT",
    );
}
