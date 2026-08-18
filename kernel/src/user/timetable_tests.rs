use super::*;
use crate::cap::{Rights, endpoint_cap, untyped_root_cap};
use crate::sched::EpId;

/// The timetable's budget. Every scheduled instance is 48 pages of it (`INSTANCE_PAGES` in
/// `user/src/timetable.rs`) plus the loader's own scratch page tables, and the pages come home when
/// a corpse is reaped, so this covers a handful of live instances rather than one per fire.
const TIMETABLE_BUDGET_PAGES: u64 = 768;

/// How many fires to ask for before the timetable summarises and exits.
///
/// **Four is a count of fires, not a duration**, and that distinction is the whole of why this test
/// is safe on a loaded machine (notes/load-sensitive-assertions.md). Nothing here asserts that a
/// fire happened *within* any interval; the test waits for the fires to arrive and the harness's
/// per-test ceiling is the only backstop. A slow host makes this test slower and never makes it
/// red, which is the property milestone 62 spent a week putting back into this tree.
const FIRES: u64 = 4;

/// Stack pages for the timetable, four times what `INIT_STACK_PAGES` gives a boot's init.
///
/// A number a spawn site **states** rather than inherits, which is `supervision_proto`'s own rule
/// (`CHILD_STACK_PAGES`: "a builder that silently inherits somebody else's stack size finds faults
/// that builder does not have"). The timetable needs it because its working set is the plan itself:
/// a `grant_plan::Endowment` is a kilobyte, mostly the name set a directory grant can carry, and a
/// `timetable::Registry` holds one per entry. Eight pages died here with a data abort whose faulting
/// address was the stack pointer, which is what a stack overflow looks like from the kernel side and
/// is worth recognising: it reads like a wild pointer and is not one.
const TIMETABLE_STACK_PAGES: u64 = 32;

/// The line `user/src/timetable.rs` prints when the plan is complete and it is about to arm. The
/// test reads the plan up to it, which is what lets one endpoint carry the plan, the summary and
/// the end of the stream without the reader having to guess where each stops.
const ARMED: &str = "timetable: armed";

/// **Spawn the timetable the way a boot would**, and hand back the three endpoints it is wired to.
///
/// Deliberately the same endowment shape `spawn_init` and `c_seam_tests::spawn_confiner` use (the
/// archive read-only at `INITRD_VA`, capabilities in numbered slots, nothing privileged), so what is
/// under test is the scheduler rather than a shortcut. Its complete authority is the four slots
/// below, which is the same list `user/src/timetable.rs`'s header states and the reason a scheduled
/// `date` in the shipped document is refused.
fn spawn_timetable(fires: u64) -> (EpId, EpId, EpId) {
    let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd region");
    let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);
    let bytes = program("timetable").expect("no timetable program in the initrd archive");
    let elf = Elf::parse(bytes).expect("timetable is not loadable");

    let content: u64 = elf
        .segments()
        .map(|seg| {
            let (s, e) = seg.page_range(FRAME_SIZE);
            (e - s) / FRAME_SIZE
        })
        .sum::<u64>()
        + 1
        + initrd_pages / 512
        + TIMETABLE_STACK_PAGES
        + 8;
    let mut space = AddressSpace::new(content).expect("no memory for the timetable");
    map_segments(&mut space, &elf).expect("could not lay out the timetable");
    for k in 0..TIMETABLE_STACK_PAGES {
        space
            .map_new(USER_STACK_VA - k * FRAME_SIZE, Flags::user_data())
            .expect("could not map the timetable's stack");
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
    let aspace = readopt_user_aspace(space).expect("register the timetable aspace");

    let out = crate::sched::create_endpoint();
    let child_report = crate::sched::create_endpoint();
    let deaths = crate::sched::create_endpoint();
    let budget = crate::untyped::create(TIMETABLE_BUDGET_PAGES).expect("no budget");
    let tcb_region = crate::untyped::create(2).expect("no tcb region");
    let tid = crate::sched::create_tcb(tcb_region).expect("no tcb");

    // Slot 0: where its own text goes. WRITE, so it can say things and cannot listen.
    let s = crate::sched::tcb_insert_cap(tid, endpoint_cap(out, Rights::WRITE), None)
        .expect("insert out");
    assert_eq!(s, 0, "the timetable's output endpoint must land in slot 0");
    // Slot 1: what every instance is made of.
    let s =
        crate::sched::tcb_insert_cap(tid, untyped_root_cap(budget), None).expect("insert budget");
    assert_eq!(s, 1, "the timetable's budget must land in slot 1");
    // Slot 2: what each instance is handed as its own slot 0. `GRANT` because handing it on is the
    // entire purpose; `WRITE` because a scheduled child reports and never listens.
    let s = crate::sched::tcb_insert_cap(
        tid,
        endpoint_cap(child_report, Rights::WRITE.union(Rights::GRANT)),
        None,
    )
    .expect("insert child report");
    assert_eq!(s, 2, "the child report endpoint must land in slot 2");
    // Slot 3: the supervision endpoint. `READ` is what `RECV` and `Endpoint::REAP` take (§32);
    // `GRANT` is what lets it be placed in each child's reserved fault slot.
    let s = crate::sched::tcb_insert_cap(
        tid,
        endpoint_cap(deaths, Rights::READ.union(Rights::GRANT)),
        None,
    )
    .expect("insert deaths");
    assert_eq!(s, 3, "the supervision endpoint must land in slot 3");

    crate::sched::configure_tcb(tid, elf.entry(), USER_STACK_TOP, aspace).expect("configure");
    crate::sched::start_tcb(tid, [fires, initrd_len, 0]).expect("start");
    (out, child_report, deaths)
}

/// One line of `sink_proto` bytes off `ep`, without its newline. `None` at end of stream.
fn line(ep: EpId, buf: &mut [u8; 256]) -> Option<usize> {
    let mut len = 0usize;
    loop {
        let m = crate::sched::ipc_recv(ep);
        let mut chunk = [0u8; sink_proto::INLINE_MAX];
        match sink_proto::unpack(m[0], m[1], m[2], &mut chunk) {
            sink_proto::Msg::Eof => return None,
            sink_proto::Msg::Malformed => panic!("the timetable wrote a malformed sink message"),
            sink_proto::Msg::Bytes(n) => {
                for &b in &chunk[..n] {
                    if b == b'\n' {
                        return Some(len);
                    }
                    assert!(len < buf.len(), "the timetable printed a very long line");
                    buf[len] = b;
                    len += 1;
                }
            }
        }
    }
}

/// **A cron whose every entry is a grant: the plan is printed before anything fires, the entries
/// that were admitted really run under supervision, and the entries that were refused never run at
/// all** (milestone 129).
///
/// One test, because the three halves are one claim and none of them means anything alone. A
/// scheduler that fires proves nothing about authority; a printed plan proves nothing if what fires
/// disagrees with it; and a refusal proves nothing unless something else in the same document did
/// fire, since a scheduler that fired *nothing* would satisfy it trivially.
///
/// What it walks is the real program on the real document. `user/timetable.conf` is `include_str!`d
/// by `user/src/timetable.rs` and read again by `crates/timetable`'s host tests, so there is one
/// source for what this machine schedules and editing it moves the program, the host tests and this
/// assertion together.
///
/// # The four answers, and why the refusals are the interesting ones
///
/// Upstream cron has one answer to a crontab line: it runs. The shipped document exercises four:
///
/// - `worker 7` and `worker 3` are **planned, backed and fired**, and their answers come back on the
///   endpoint the plan said they would hold.
/// - `budgeter` and `wc` are refused by **the prompt's own check**, unchanged: the same lines typed
///   at a shell get the same sentences, because it is literally the same function.
/// - `date` and `ps` are refused because **this timetable holds nothing to back them**. Those two
///   are the ones Unix cannot refuse: there a clock and a process listing are ambient, so the lines
///   run and the question of whether they should have is one nothing asks.
///
/// # What is asserted about time, and what deliberately is not
///
/// The fires are **counted, never timed**. Nothing here says an entry fired within its interval, and
/// nothing compares a wall clock to anything: the test asks the timetable for [`FIRES`] fires and
/// waits for them. A loaded host makes this slower and cannot make it red, which is
/// notes/load-sensitive-assertions.md's rule applied at the point where it is easiest to get wrong.
#[test_case]
fn a_scheduled_entry_holds_what_the_plan_said_and_a_refused_one_never_runs() {
    let (out, reports, _deaths) = spawn_timetable(FIRES);
    let mut buf = [0u8; 256];

    // ---- the plan, printed before the first tick ----
    //
    // Read it to the arming line, keeping the facts the assertions below need. Reading it at all is
    // half the claim: the timetable is blocked on these sends until this loop takes them, so
    // nothing can have fired before the plan was complete.
    let mut saw_worker_grant = false;
    let mut saw_nothing_else = false;
    let mut saw_clock_refusal = false;
    let mut saw_domain_refusal = false;
    let mut saw_mem_refusal = false;
    let mut saw_input_refusal = false;
    let mut armed = false;
    for _ in 0..256 {
        let Some(n) = line(out, &mut buf) else {
            panic!("the timetable's output ended before it armed");
        };
        let s = core::str::from_utf8(&buf[..n]).expect("the timetable printed non-UTF-8");
        if s == ARMED {
            armed = true;
            break;
        }
        saw_worker_grant |= s.contains("grants worker exactly:");
        saw_nothing_else |=
            s.contains("and nothing else: no clock, no disk, no console, no network");
        saw_clock_refusal |= s.contains("this timetable holds no clock, so it cannot grant one");
        saw_domain_refusal |= s.contains("this timetable holds no process view to grant");
        // The prompt's own refusals, arriving unchanged. `budgeter` with no `--mem` and `wc` with
        // nothing feeding it are wrong wherever they are typed.
        saw_mem_refusal |= s.contains(grant_plan::Refusal::MemRequired.message());
        saw_input_refusal |= s.contains(grant_plan::Refusal::InputRequired.message());
    }
    assert!(
        armed,
        "the timetable printed more than a plan's worth of lines"
    );
    assert!(
        saw_worker_grant && saw_nothing_else,
        "the plan did not say what a scheduled worker would hold",
    );
    assert!(
        saw_clock_refusal,
        "a scheduled `date` must be refused for want of a clock, and said so. On Unix this entry \
         runs, because there the clock is ambient; here it is a capability this process was not \
         granted, and the refusal has to be legible before anything fires.",
    );
    assert!(
        saw_domain_refusal,
        "a scheduled `ps` must be refused for want of a process view",
    );
    assert!(
        saw_mem_refusal && saw_input_refusal,
        "the prompt's own refusals must arrive here unchanged",
    );

    // ---- what actually fired ----
    //
    // `worker` squares its argument, so the shipped document's two admitted entries answer 49
    // (`worker 7`) and 9 (`worker 3`). **This is the negative control**: every refused entry in the
    // document is a program that would have written something else here (`date` and `wc` write
    // `sink_proto` bytes, `budgeter` writes a page count), so a document whose refusals had leaked
    // would fail on the value rather than on a count.
    let mut nines = 0;
    let mut forty_nines = 0;
    for _ in 0..FIRES {
        let answer = crate::sched::ipc_recv(reports)[0];
        match answer {
            9 => nines += 1,
            49 => forty_nines += 1,
            other => panic!(
                "a scheduled child reported {other}; only `worker 3` and `worker 7` were admitted, \
                 so this is an entry that fired after being refused",
            ),
        }
    }
    assert_eq!(nines, 1, "`at-boot worker 3` must fire exactly once");
    assert_eq!(
        forty_nines,
        FIRES - 1,
        "the rest must be the repeating `worker 7` heartbeat",
    );

    // ---- the summary, after every corpse has been collected ----
    //
    // The clean-exit count is the supervision half of the claim, and it is not decoration: each of
    // those children was born with the timetable's supervision endpoint in its reserved fault slot,
    // died, reported its death to the timetable, and had its region reclaimed through
    // `Endpoint::REAP`. A scheduler that leaked a region per fire would still print the fire count
    // and would run out of budget instead of finishing.
    let Some(n) = line(out, &mut buf) else {
        panic!("the timetable ended its stream without a summary");
    };
    let s = core::str::from_utf8(&buf[..n]).expect("non-UTF-8 summary");
    assert_eq!(
        s, "timetable: 4 fires, 4 clean exits, 0 faults",
        "the summary must account for every child it started",
    );

    // The stream ends rather than stopping, and the verdict says it finished cleanly.
    assert!(
        line(out, &mut buf).is_none(),
        "the timetable said something after its summary",
    );
    assert_eq!(
        crate::sched::ipc_recv(out)[0],
        0,
        "the timetable's verdict word must be a clean finish",
    );
}
