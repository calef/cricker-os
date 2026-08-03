use super::*;
use crate::cap::{Rights, endpoint_cap, frame_cap};
use crate::sched::EpId;

/// Where `date` expects the clock page, read-only. Must match user/src/date.rs's `CLOCK_VA`.
const CLOCK_VA: u64 = 0x00c0_0000;

// The `a0` format selector, and the `a1`/`a2` conventions. Must match user/src/date.rs.
const FMT_HUMAN: u64 = 0;
const FMT_RFC3339: u64 = 1;
const FMT_UNIX: u64 = 4;
const PROVENANCE: u64 = 1;

/// Spawn `date` and return the endpoint its output arrives on.
///
/// `page` is the whole of its clock authority: `Some(phys)` grants the frame with **`READ`**
/// and maps it **read-only**, which is the read rung of §43's ladder and is what makes a
/// `date -s` unbuildable rather than merely absent. `None` grants no clock at all, which is the
/// other unknown-clock cause and a different message.
fn spawn_date(page: Option<u64>, fmt: u64, offset_minutes: i64, provenance: u64) -> EpId {
    let image = program("date").expect("no date program in the initrd archive");
    let out = crate::sched::create_endpoint();
    crate::sched::spawn(move || match page {
        Some(phys) => run(
            image,
            Spawn {
                arg0: fmt,
                arg1: offset_minutes as u64,
                arg2: provenance,
                grants: &[
                    endpoint_cap(out, Rights::WRITE), // slot 0: stdout
                    frame_cap(phys, Rights::READ),    // slot 1: a READER, and nothing more
                ],
                maps: &[Mapping {
                    va: CLOCK_VA,
                    phys,
                    flags: Flags::user_rodata(),
                }],
            },
        ),
        None => run(
            image,
            Spawn {
                arg0: fmt,
                arg1: offset_minutes as u64,
                arg2: provenance,
                grants: &[endpoint_cap(out, Rights::WRITE)],
                maps: &[],
            },
        ),
    })
    .expect("could not spawn date");
    out
}

/// One line of `date`'s output, without its newline.
///
/// The framing is the std PAL's stdout framing (`w0` = the byte count, `w1`|`w2` = the bytes,
/// little-endian), deliberately, so there is one convention for "a program printed something"
/// rather than two. `SEND` blocks until a receiver takes it, so stopping at the newline
/// consumes exactly the messages that line was made of and leaves any following line queued.
fn line(out: EpId, buf: &mut [u8; 128]) -> usize {
    let mut len = 0usize;
    loop {
        let words = crate::sched::ipc_recv(out);
        let count = words[0] as usize;
        assert!(
            (1..=16).contains(&count),
            "date: a stdout message with a bad byte count: {count}"
        );
        let mut chunk = [0u8; 16];
        chunk[..8].copy_from_slice(&words[1].to_le_bytes());
        chunk[8..].copy_from_slice(&words[2].to_le_bytes());
        for &b in &chunk[..count] {
            if b == b'\n' {
                return len;
            }
            assert!(len < buf.len(), "date printed a line longer than a line");
            buf[len] = b;
            len += 1;
        }
    }
}

/// Start the clock service and take its startup report, so the page has been published to
/// before anything reads it.
fn clock() -> clock_service::Wiring {
    let image = program("clock").expect("no clock program in the initrd archive");
    let w = clock_service::start(image);
    let _ = crate::sched::ipc_recv(w.report);
    w
}

/// **`date` prints the time the machine actually knows, in a form that reads back.**
///
/// Three formats over the one clock, and the assertion is not "it printed something shaped
/// like a date". The kernel computes the wall clock itself, straight from the page's offset
/// plus the ambient counter, and requires `date`'s output to name the same instant. So a wrong
/// epoch, a nanoseconds/seconds confusion, a timezone applied twice, or a calendar that is
/// simply wrong all fail here, and none of them would fail a regex.
///
/// The window is ten seconds because the two readings are genuinely at different times (a
/// process spawn sits between them) and the clock has one-second resolution. It is tight
/// enough that the failures above miss it by decades.
#[test_case]
fn date_prints_the_wall_clock_it_was_granted() {
    let w = clock();
    let mut buf = [0u8; 128];

    // `Unix`, the format with nothing between the clock and the text: the kernel's own reading
    // of the same page, in seconds, must be within a few seconds of what date printed.
    let n = line(spawn_date(Some(w.page_phys), FMT_UNIX, 0, 0), &mut buf);
    let printed: i64 = core::str::from_utf8(&buf[..n])
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("date printed {:?}, which is not a number", &buf[..n]));
    let ours = (w.wall_nanos() / clock_proto::NANOS_PER_SEC) as i64;
    assert!(
        (printed - ours).abs() < 10,
        "date printed {printed} seconds; the kernel reads {ours} from the same page",
    );

    // `Rfc3339`, the interchange form, parsed back by the same crate that printed it. The
    // round trip is the weak half of this; the strong half is that the instant it names must
    // still be the one above, so the text carries the time rather than merely being well-formed.
    let n = line(spawn_date(Some(w.page_phys), FMT_RFC3339, 0, 0), &mut buf);
    let s = core::str::from_utf8(&buf[..n]).expect("date printed non-UTF-8");
    let dt = calendar::DateTime::parse_rfc3339(s)
        .unwrap_or_else(|e| panic!("date printed {s:?}, which is not RFC 3339: {}", e.as_str()));
    assert_eq!(dt.offset().minutes(), 0, "no offset was asked for");
    assert!(
        (dt.to_unix() - ours).abs() < 10,
        "date printed {s}, which is not the {ours} the kernel reads",
    );
    assert!(
        s.ends_with('Z') && s.len() == 20,
        "{s:?} is not the 20-byte Z-terminated form RFC 3339 asks for at zero offset",
    );

    // `Human`, the default a person gets, at a non-zero offset so the offset is not merely
    // accepted and dropped. `Thu 2026-07-30 18:04:56 +05:30`: the weekday is the crate's whole
    // natural-language surface, and the trailing field is what says which clock this is.
    let n = line(spawn_date(Some(w.page_phys), FMT_HUMAN, 330, 0), &mut buf);
    let s = core::str::from_utf8(&buf[..n]).expect("date printed non-UTF-8");
    assert!(
        s.ends_with(" +05:30"),
        "{s:?} should carry the +05:30 offset it was asked for",
    );
    let day = &s[..3];
    assert!(
        ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].contains(&day),
        "{s:?} should start with a weekday abbreviation, not {day:?}",
    );
    // Same instant again, seen through a different rendering. `Human` is
    // `Fri 2026-07-31 14:59:43 +05:30`: drop the weekday and close the space before the offset,
    // and what is left is RFC 3339 (which permits the space in place of `T`, but not the one
    // before the offset). Doing it this way rather than eyeballing the digits is what makes the
    // check an assertion about the *instant* rather than about the layout.
    assert_eq!(s.len(), 30, "{s:?} is not the Human rendering at an offset");
    let mut iso = [0u8; 32];
    iso[..19].copy_from_slice(&s.as_bytes()[4..23]); // the date and time
    iso[19..25].copy_from_slice(&s.as_bytes()[24..]); // the offset, its space removed
    let iso = calendar::DateTime::parse_rfc3339_bytes(&iso[..25])
        .unwrap_or_else(|e| panic!("{s:?} does not reduce to RFC 3339: {}", e.as_str()));
    assert_eq!(iso.offset().minutes(), 330);
    assert!(
        (iso.to_unix() - ours).abs() < 10,
        "{s:?} names a different instant from the {ours} the kernel reads",
    );
}

/// **An unknown clock is a sentence, not 1970 and not a panic.**
///
/// This is DECISIONS §42's no-silent-degradation rule applied where a person can see it, and it
/// is the reason `date` reads `clock_proto::state` before it reads the offset. `std`'s
/// `SystemTime::now()` has no error channel and must panic here; `date` has one, so it says
/// which of the two causes it was:
///
/// - **a frame nobody published to**, which is what a machine with no RTC (or one whose reading
///   the clock service refused) leaves its readers holding. §43 recorded this path as proven by
///   construction only, because both QEMU boards always have a working RTC. It is provable in
///   the guest after all: the page is the contract, so an unpublished page **is** that machine.
/// - **no clock capability at all**, which is a different fix and therefore a different
///   sentence. Note there is no mapping either, so `date` has to answer without touching
///   `CLOCK_VA`; a program that read the page to find out whether it had one would fault.
#[test_case]
fn an_unknown_clock_is_said_plainly_rather_than_printed_as_1970() {
    let mut buf = [0u8; 128];

    // A clock page nobody has published to. Zeroed, which is what the frame allocator hands
    // out and what `clock_proto`'s `a_zeroed_page_reads_as_unknown` pins as UNKNOWN.
    let blank = crate::memory::alloc()
        .expect("no frame for a blank clock page")
        .addr();
    // SAFETY: freshly allocated, named through the direct map, owned by nobody else.
    unsafe {
        core::ptr::write_bytes(mmu::phys_to_virt(blank) as *mut u8, 0, FRAME_SIZE as usize);
    };

    let out = spawn_date(Some(blank), FMT_HUMAN, 0, PROVENANCE);
    let n = line(out, &mut buf);
    let s = core::str::from_utf8(&buf[..n]).expect("date printed non-UTF-8");
    assert_eq!(
        s, "date: the time is unknown: the machine has no clock it believes",
        "an unknown clock must be reported, not guessed at",
    );
    // And the provenance line agrees rather than inventing a source for a time it does not have.
    let n = line(out, &mut buf);
    assert_eq!(
        core::str::from_utf8(&buf[..n]).unwrap(),
        "date: clock source: unknown, generation 0",
    );

    // The other cause: no capability in the slot, so no mapping either.
    let n = line(spawn_date(None, FMT_HUMAN, 0, 0), &mut buf);
    assert_eq!(
        core::str::from_utf8(&buf[..n]).unwrap(),
        "date: the time is unknown: this process holds no clock capability",
    );
}

/// **The provenance is readable, and it is the four-state model rather than a boolean.**
///
/// "A human set it" and "an external source the service bounded accepted it" are different
/// claims, and the difference is exactly what a caller weighing a certificate expiry wants
/// (DECISIONS §43). Nothing else in the tree renders that distinction for a person, so if it is
/// not asserted here it is asserted nowhere.
///
/// The generation is the load-bearing half: it counts publishes, so a reader can see that the
/// clock was stepped under it. Proposing a small correction moves `rtc, generation 1` to
/// `synced, generation 2`, and a `date` that had cached or invented either would not follow.
#[test_case]
fn date_reports_where_the_time_came_from() {
    let w = clock();
    let mut buf = [0u8; 128];

    let out = spawn_date(Some(w.page_phys), FMT_UNIX, 0, PROVENANCE);
    let _ = line(out, &mut buf); // the time itself, asserted elsewhere
    let n = line(out, &mut buf);
    assert_eq!(
        core::str::from_utf8(&buf[..n]).unwrap(),
        "date: clock source: rtc, generation 1",
        "the clock was read from the RTC once and published once",
    );

    // Step it through the propose endpoint, which is an authority `date` does not hold, and the
    // provenance follows the page rather than the process.
    let (status, _) = w.propose_nanos(w.wall_nanos() + clock_proto::NANOS_PER_SEC / 2);
    assert_eq!(status, clock_proto::status::ACCEPTED);
    let out = spawn_date(Some(w.page_phys), FMT_UNIX, 0, PROVENANCE);
    let _ = line(out, &mut buf);
    let n = line(out, &mut buf);
    assert_eq!(
        core::str::from_utf8(&buf[..n]).unwrap(),
        "date: clock source: synced, generation 2",
        "an accepted proposal is a SYNCED clock one generation on, and date should say so",
    );
}
