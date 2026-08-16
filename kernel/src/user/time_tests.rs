use pipeline_service::answer;

use super::*;

/// The last line `user/src/swish.rs`'s timing role prints. Must match `TIMING_DONE`.
const DONE: &[u8] = b"== timings done\n";

/// Run the timing script under one clock state and hand back what the shell printed.
///
/// Not cached, unlike [`pipeline_tests`]'s and [`redirection_tests`]'s transcripts, because the
/// whole point here is to run the **same script** under three different cspaces: caching would make
/// the second and third runs the first one's answer.
fn transcript(clock: Option<u64>, out: &mut [u8; 2048]) -> usize {
    let Some(w) = pipeline_service::start_timing(clock) else {
        panic!("no swish program in the initrd archive, or no memory to wire one");
    };
    pipeline_service::transcript(&w, DONE, out)
}

/// Start the clock service and take its startup report, so the page has been published to before
/// anything reads it. The same helper `date_tests` uses, and for the same reason.
fn clock() -> clock_service::Wiring {
    let image = program("clock").expect("no clock program in the initrd archive");
    let w = clock_service::start(image);
    let _ = crate::sched::ipc_recv(w.report);
    w
}

/// The nanoseconds in a `time:` line, as a number.
///
/// The line is `  time: real 4.213 ms`, and this parses it back into nanoseconds rather than
/// checking that it looks like a duration, because "it printed something with a decimal point in it"
/// is exactly the assertion that would survive every bug worth catching. A unit this does not know
/// is a failure: a renderer that grew a fourth unit and a reader that silently ignored it would let
/// a wrong number through.
fn nanos_of(said: &[u8]) -> u64 {
    let text = core::str::from_utf8(said).expect("the shell printed non-UTF-8");
    let line = text
        .lines()
        .find(|l| l.contains("time: real "))
        .unwrap_or_else(|| panic!("no timing line in {text:?}"));
    let rest = line.split("time: real ").nth(1).expect("just matched");
    let mut fields = rest.split_ascii_whitespace();
    let value = fields
        .next()
        .unwrap_or_else(|| panic!("no value in {line:?}"));
    let unit = fields
        .next()
        .unwrap_or_else(|| panic!("no unit in {line:?}"));
    let (whole, frac) = value
        .split_once('.')
        .unwrap_or_else(|| panic!("{value:?} is not a fixed-point duration"));
    assert_eq!(frac.len(), 3, "the fraction is thousandths of a unit");
    let thousandths: u64 = whole
        .parse::<u64>()
        .and_then(|w| frac.parse::<u64>().map(|f| w * 1_000 + f))
        .unwrap_or_else(|_| panic!("{value:?} is not a number"));
    match unit {
        "us" => thousandths,
        "ms" => thousandths * 1_000,
        "s" => thousandths * 1_000_000,
        other => panic!("the shell printed a unit this test does not know: {other:?}"),
    }
}

/// **A command is timed by the shell's clock, and the command is handed nothing.**
///
/// This is the milestone's whole claim, and the three assertions are what make it one rather than a
/// slogan:
///
/// - **The same command, timed and untimed, answers the same thing.** `worker 3` is run twice in the
///   script, once bare and once behind `time`, and both must report `3*3 = 9`. A prefix word that
///   re-tokenized its tail, dropped an option or spawned something else would fail here, and nothing
///   about the printed duration would have told anybody.
/// - **The duration is positive and sane.** A real spawn (an address space, an ELF copy, a thread, a
///   rendezvous and a reply) cannot take zero time, so a zero here means the two readings came from
///   the same instant, which is what a `time` that read its clock once would print. The upper bound
///   is loose on purpose: TCG under load is slow and a tight bound would be a flake, while a
///   nanoseconds-for-microseconds error misses a ten-second ceiling by four orders of magnitude.
/// - **`worker` holds no clock.** Its manifest declares none, so init endows none, and it is still
///   timed. That is the sentence the milestone exists for: measuring a thing needs the observer's
///   authority, not the subject's.
///
/// And the builtin, which is the other end of the same claim: `time echo hello` spawns **no process
/// at all** and still reports a duration, because what is being measured is an interval and not a
/// process.
#[test_case]
fn a_command_is_timed_by_the_shells_clock_and_holds_none_itself() {
    let w = clock();
    let mut buf = [0u8; 2048];
    let n = transcript(Some(w.page_phys), &mut buf);
    let t = &buf[..n];

    let bare = answer(t, b"worker 3");
    let timed = answer(t, b"time worker 3");
    assert!(
        core::str::from_utf8(bare).unwrap().contains("3*3 = 9"),
        "the untimed control did not run: {:?}",
        core::str::from_utf8(bare).unwrap(),
    );
    assert!(
        core::str::from_utf8(timed).unwrap().contains("3*3 = 9"),
        "timing a command must not change what it answers: {:?}",
        core::str::from_utf8(timed).unwrap(),
    );

    // Ten seconds, because this is not a benchmark: `bench` measures spawn latency and this proves a
    // stopwatch exists. See notes/time-command.md.
    const SANE: u64 = 10_000_000_000;
    let spawned = nanos_of(timed);
    assert!(
        spawned > 0 && spawned < SANE,
        "a spawn took {spawned} ns, which is not a duration this machine could have spent",
    );

    // The builtin: no process, no grant, and a duration all the same.
    let builtin = answer(t, b"time echo hello");
    assert!(
        core::str::from_utf8(builtin).unwrap().contains("hello"),
        "the timed builtin did not run: {:?}",
        core::str::from_utf8(builtin).unwrap(),
    );
    let echoed = nanos_of(builtin);
    assert!(
        echoed > 0 && echoed < SANE,
        "a timed builtin took {echoed} ns",
    );

    // And the prefix with nothing after it is about the line rather than about a clock, which is
    // why it is a third sentence and not one of `date`'s two.
    let empty = answer(t, b"time");
    assert!(
        core::str::from_utf8(empty)
            .unwrap()
            .contains("name a command to time"),
        "{:?}",
        core::str::from_utf8(empty).unwrap(),
    );
}

/// **A shell that cannot read a clock refuses to time, and says which nothing it was.**
///
/// The two causes are `date`'s and they are kept apart for `date`'s reason: nobody granted this
/// process a clock, or the machine never learned the time. They call for different fixes, so they
/// are different sentences.
///
/// **Neither of them runs the command**, and that is the assertion worth having rather than the
/// wording. `time` is a request to measure; running the line unmeasured and saying nothing would be
/// DECISIONS §42's silent degradation with a stopwatch on it, and running it while complaining would
/// leave a person unable to tell which half happened. So the transcript must contain no `3*3 = 9`
/// **A shell with no usable clock times the command anyway** (§72, 2026-08-16), which is the
/// inversion of what this test asserted when it was written.
///
/// It used to prove the opposite: that `time` refused on a machine whose wall clock was absent or
/// unbelieved, rather than running the line unmeasured, on §42's anti-silent-degradation grounds.
/// That reasoning was right and its premise was wrong. A duration is two reads of the monotonic
/// counter, which the kernel opened to EL0, so the offset a clock page carries cancels and `time`
/// can always measure. The refusal was not protecting anyone; it was a capability check that
/// gated nothing.
///
/// So both old causes are exercised here as *non*-causes: a zeroed clock page (UNKNOWN to
/// `clock_proto`) and no clock capability at all. In each, the timed line must both answer and
/// carry a duration, and neither of the two retired sentences may appear anywhere in the
/// transcript.
#[test_case]
fn a_shell_with_no_usable_clock_times_the_command_anyway() {
    let mut buf = [0u8; 2048];

    // A frame nobody has published to, which is what a reader on a machine with no believable RTC
    // holds. Zeroed, which `clock_proto`'s `a_zeroed_page_reads_as_unknown` pins as UNKNOWN.
    let blank = crate::memory::alloc()
        .expect("no frame for a blank clock page")
        .addr();
    // SAFETY: freshly allocated, named through the direct map, owned by nobody else.
    unsafe {
        core::ptr::write_bytes(mmu::phys_to_virt(blank) as *mut u8, 0, FRAME_SIZE as usize);
    };

    // Ten seconds, the sibling test's bound and for its reason: this proves a stopwatch exists,
    // it does not benchmark one.
    const SANE: u64 = 10_000_000_000;

    for clock in [Some(blank), None] {
        let n = transcript(clock, &mut buf);
        let t = &buf[..n];
        let timed = core::str::from_utf8(answer(t, b"time worker 3")).unwrap();
        assert!(
            timed.contains("3*3 = 9"),
            "the command must run: {timed:?}",
        );
        let nanos = nanos_of(answer(t, b"time worker 3"));
        assert!(nanos > 0 && nanos < SANE, "duration out of range: {nanos}");
        // The two sentences §72 retired must be gone from the whole transcript, not merely from
        // this line: a shell that printed them anywhere would still be claiming the old boundary.
        let whole = core::str::from_utf8(t).unwrap();
        assert!(!whole.contains("holds no clock capability"), "{whole:?}");
        assert!(!whole.contains("no clock it believes"), "{whole:?}");
    }
}
