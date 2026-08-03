use sink_proto::fixture;

use super::*;
use crate::cap::{Rights, endpoint_cap};
use crate::sched::EpId;

/// `user/src/sink.rs`'s writer role. Its `arg1` is how many times to write the transcript, with
/// 0 meaning "until the sink stops taking it".
const ROLE_WRITER: u64 = 0;

/// Spawn the writer against `sink`, and return the endpoint it reports its classification on.
///
/// `None` is the case that has no sink, and the wiring says so by **leaving slot 0 empty**
/// rather than by passing a flag: an empty cspace slot is how this kernel spells "you were
/// never given one", and it is the refusal an ungranted display client meets too (§29). The
/// report still goes in at slot 1, placed rather than appended, so the program that holds no
/// sink is otherwise wired identically to the one that does.
fn spawn_writer(image: &'static [u8], sink: Option<EpId>, repeat: u64) -> EpId {
    let report = crate::sched::create_endpoint();
    crate::sched::spawn(move || match sink {
        Some(ep) => run(
            image,
            Spawn {
                arg0: ROLE_WRITER,
                arg1: repeat,
                arg2: 0,
                grants: &[
                    endpoint_cap(ep, Rights::WRITE),     // slot 0: the byte sink
                    endpoint_cap(report, Rights::WRITE), // slot 1: the classification
                ],
                maps: &[],
            },
        ),
        None => {
            crate::sched::grant_at(1, endpoint_cap(report, Rights::WRITE))
                .expect("the writer's report slot was already occupied");
            run(
                image,
                Spawn {
                    arg0: ROLE_WRITER,
                    arg1: repeat,
                    arg2: 0,
                    grants: &[],
                    maps: &[],
                },
            )
        }
    })
    .expect("could not spawn the sink writer");
    report
}

/// Spawn `wc` with `source` in its input slot, and return the endpoint its answer arrives on.
///
/// Two capabilities and nothing else, which is what makes the test below mean something: `wc`
/// holds no file, no directory, no page and nothing that names the FS server. Whatever is
/// behind its input slot, it is handed the bytes.
fn spawn_wc(source: EpId) -> EpId {
    let image = program("wc").expect("no wc program in the initrd archive");
    let out = crate::sched::create_endpoint();
    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: 0,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(out, Rights::WRITE),   // slot 0: where its answer goes
                    endpoint_cap(source, Rights::READ), // slot 1: where its bytes come from
                ],
                maps: &[],
            },
        )
    })
    .expect("could not spawn wc");
    out
}

/// Drain `wc`'s answer and parse the three numbers out of it.
fn wc_counts(out: EpId, what: &str) -> (u64, u64, u64) {
    let mut buf = [0u8; 64];
    let n = super::std_tests::drain_sink(out, &mut buf, what);
    let text = core::str::from_utf8(&buf[..n]).expect("wc printed non-UTF-8");
    let mut it = text.split_ascii_whitespace().map(|w| {
        w.parse::<u64>()
            .unwrap_or_else(|_| panic!("{what}: wc printed {text:?}"))
    });
    (
        it.next().expect("no line count"),
        it.next().expect("no word count"),
        it.next().expect("no byte count"),
    )
}

/// **`<` proven the way `>` is: the same `wc`, two sources, the same answer.**
///
/// This is the indifference claim on the **input** side, which milestone 50's protocol lane
/// explicitly left open ("both `< file` and a pipe's read end need an input-slot convention
/// that does not exist"). The convention chosen is the smallest one available: a source is the
/// sink contract *received* rather than sent. So this test is what says that choice was real.
///
/// One `wc` ELF, spawned twice with identical grants except for what is behind slot 1:
///
/// - **a pipe**: this test sends the transcript on an endpoint itself, sixteen bytes at a time,
///   then `OP_EOF`. That is exactly what a program on the left of a `|` does.
/// - **a file**: the transcript is written into a real file on the real RedoxFS image by
///   `sink`'s file role, and read back out by its source role, which streams it over the same
///   contract. That is `wc < report.txt`, minus the shell that would name the file.
///
/// The two arms share nothing but the framing. The second crosses two userspace processes, an
/// FS server, a block server and a virtio disk; the first does not leave this address space.
/// `wc` was not told which and holds nothing that could tell it.
///
/// The answers must be equal **and** must equal what the transcript actually is, because two
/// arms broken the same way would satisfy equality on its own.
#[test_case]
fn one_reader_two_sources_and_the_same_answer() {
    let sink_image = program("sink").expect("no sink program in the initrd archive");
    let blk = program("init").expect("no init program in the initrd archive");
    let Some(fs_server) = program("fs_server") else {
        crate::println!("    (no FS server in this archive; skipping)");
        return;
    };

    // Arm one: a pipe. The kernel is the producer, which is the same position the shell is in
    // when a builtin leads a pipeline (`kernel::user::pipeline_tests`).
    let pipe = crate::sched::create_endpoint();
    let out = spawn_wc(pipe);
    let mut off = 0usize;
    while off < fixture::TRANSCRIPT.len() {
        let (w0, w1, w2, n) = sink_proto::pack(&fixture::TRANSCRIPT[off..]);
        crate::sched::ipc_send(pipe, [w0, w1, w2]);
        off += n;
    }
    crate::sched::ipc_send(pipe, [sink_proto::eof(), 0, 0]);
    let piped = wc_counts(out, "wc reading a pipe");

    // The transcript's own numbers, so the pipe arm is anchored to something rather than only
    // to the file arm.
    let text = core::str::from_utf8(fixture::TRANSCRIPT).expect("the fixture is not UTF-8");
    assert_eq!(
        piped,
        (
            text.lines().count() as u64,
            text.split_ascii_whitespace().count() as u64,
            fixture::TRANSCRIPT.len() as u64,
        ),
        "wc miscounted the transcript it was handed down a pipe",
    );

    // Arm two: the same bytes, through a real filesystem. Write them first.
    let Some(file_sink) = fs_service::start_file_sink(blk, fs_server, sink_image) else {
        crate::println!("    (no RedoxFS disk attached; the pipe arm stands alone)");
        return;
    };
    fs_service::wait_for_service(file_sink.readiness);
    assert_eq!(
        crate::sched::ipc_recv(file_sink.report)[0],
        fixture::READY,
        "the file sink could not open its file",
    );
    let wrote = spawn_writer(sink_image, Some(file_sink.sink), 1);
    let [code, total, ..] = crate::sched::ipc_recv(wrote);
    assert_eq!(
        code,
        fixture::code(sink_proto::Sent::Ok),
        "the writer did not deliver the transcript to the file sink",
    );
    let [done, wrote_total, ..] = crate::sched::ipc_recv(file_sink.report);
    assert_eq!(done, fixture::DONE, "the file sink did not finish cleanly");
    assert_eq!(
        wrote_total, total,
        "the file sink recorded a different byte count from the one the writer sent",
    );

    // Then read them back into `wc`, which is `<`.
    let Some((source, verify_report)) = fs_service::start_sink_verify(blk, fs_server, sink_image)
    else {
        panic!("the FS service vanished between the file sink and its source");
    };
    let out = spawn_wc(source);
    let filed = wc_counts(out, "wc reading a file");
    let [vdone, size, ..] = crate::sched::ipc_recv(verify_report);
    assert_eq!(
        vdone,
        fixture::DONE,
        "the source adapter could not read the file back ({vdone:#x})",
    );
    assert_eq!(
        size, total,
        "the file is not the size that was written into it"
    );

    assert_eq!(
        piped, filed,
        "the same wc answered differently for a pipe and for a file, so its input slot is not \
         opaque after all",
    );
}

/// **The same program, two destinations, the same bytes.**
///
/// `std_exerciser` is spawned twice with **identical grants except for what is behind slot 1**: once
/// with an endpoint this test receives on, which is the pipe shape (an ordinary receiver, no
/// page, no reply), and once with an endpoint served by `sink` in its file role, which appends
/// every message into a file on the real RedoxFS image through the real FS server. The file is
/// then read back by a **third** process with its own FS session and streamed home over the
/// sink contract.
///
/// The two arms share nothing but sixteen bytes of message. One ends in the kernel's own
/// address space; the other crosses two userspace processes, an FS server, a block server and a
/// virtio disk. The binary is identical, its code never chose either, and the bytes must be
/// equal.
///
/// **What would make this vacuous, and why it is not.** Comparing each arm against a constant
/// would pass if both arms were broken the same way, so the second arm is compared against
/// *what the first arm actually received*, and the first arm against the pinned transcript. An
/// empty file would satisfy "equal" if the program printed nothing, so the byte count is
/// asserted on both sides as well.
#[test_case]
fn a_program_cannot_tell_what_its_output_slot_holds() {
    let std_exerciser =
        program("std_exerciser").expect("no std_exerciser program in the initrd archive");
    let clock = program("clock").expect("no clock program in the initrd archive");
    let entropy = program("entropy").expect("no entropy program in the initrd archive");
    let sink_image = program("sink").expect("no sink program in the initrd archive");
    let Some(fs_server) = program("fs_server") else {
        crate::println!("    (no FS server in this archive; skipping)");
        return;
    };

    // Arm one: the kernel receives. Everything here is what the existing std test does, which
    // is the point: nothing was special-cased for the sink.
    let direct = crate::sched::create_endpoint();
    std_service::start_on(std_exerciser, clock, entropy, direct);
    let mut first = [0u8; 512];
    let n1 = super::std_tests::drain_sink(direct, &mut first, "std_exerciser, direct endpoint");
    assert_eq!(
        &first[..n1],
        super::std_tests::EXPECTED,
        "the direct arm did not print the pinned transcript, so it is no reference for what \
         the other arm produces",
    );

    // Arm two: a file sink. It opens its file before the writer exists, for the reason
    // `fs_service::wait_for_caretaker` records: it stages a name in the page it shares with the
    // FS server, and a client that already existed could write over it.
    let blk = program("init").expect("no init program in the initrd archive");
    let Some(file_sink) = fs_service::start_file_sink(blk, fs_server, sink_image) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    let (sink_ep, sink_report) = (file_sink.sink, file_sink.report);
    fs_service::wait_for_service(file_sink.readiness);
    assert_eq!(
        crate::sched::ipc_recv(sink_report)[0],
        fixture::READY,
        "the file sink could not open its file, so there was nothing to redirect into",
    );

    std_service::start_on(std_exerciser, clock, entropy, sink_ep);
    let [done, total, ..] = crate::sched::ipc_recv(sink_report);
    assert_eq!(
        done,
        fixture::DONE,
        "the file sink reported {done:#x} rather than finishing: it refused a message, or the \
         FS server refused a write",
    );
    assert_eq!(
        total as usize, n1,
        "the file sink wrote {total} bytes for a program that printed {n1}",
    );

    // The read-back, in a third process with its own FS session, and only now that the sink has
    // closed the file: the two share the FS server's one file page.
    let Some((out, verify_report)) = fs_service::start_sink_verify(blk, fs_server, sink_image)
    else {
        panic!("the FS service vanished between the file sink and its verifier");
    };
    let mut second = [0u8; 512];
    let n2 = super::std_tests::drain_sink(out, &mut second, "std_exerciser, file sink");
    let [vdone, vsize, ..] = crate::sched::ipc_recv(verify_report);
    assert_eq!(
        vdone,
        fixture::DONE,
        "the verifier could not read back the file the sink wrote ({vdone:#x})",
    );
    assert_eq!(
        vsize as usize, n2,
        "the verifier streamed a length it did not find on the platter",
    );

    assert_eq!(
        &second[..n2],
        &first[..n1],
        "the same program printed different bytes into a file than into an endpoint, so its \
         output destination is something it can tell apart",
    );
    crate::println!("    ({n1} bytes, identical through an endpoint and through RedoxFS)");
}

/// **A dead reader ends the writer, and a missing one does not.** Milestone 50's one behaviour
/// change, asserted by value on all three outcomes.
///
/// Unix needs `SIGPIPE` here because an anonymous file descriptor gives the kernel no way to
/// reach a writer that is not making a syscall. This writer *is* making a syscall, on a
/// capability the kernel can see is dead, so the fact arrives as a return code.
///
/// 1. **A sink that stays**: the transcript arrives and the writer classifies `Ok`.
/// 2. **A sink destroyed under it**: this test takes a few messages, reclaims the region the
///    endpoint was retyped from (§16), and the writer's blocked `SEND` classifies `Gone`.
///    Blocked is the case that matters: a producer in `yes | head` is almost always parked in a
///    send when its reader exits.
/// 3. **No sink at all**: an empty slot classifies `NoSink` and the program **keeps running**.
///
/// Case 3 is what makes case 2 mean anything. Both used to be `NoSuchSlot`, so a writer that
/// ended on case 2 would have ended on case 3 as well, killing every program that was simply
/// never given a console.
#[test_case]
fn a_destroyed_sink_ends_the_writer_and_an_absent_one_does_not() {
    let image = program("sink").expect("no sink program in the initrd archive");

    // 1. A sink that stays.
    let live = crate::sched::create_endpoint();
    let report = spawn_writer(image, Some(live), 1);
    let mut got = [0u8; 256];
    let n = super::std_tests::drain_sink(live, &mut got, "the writer with a live sink");
    assert_eq!(
        &got[..n],
        fixture::TRANSCRIPT,
        "the writer did not deliver its transcript to a sink that was there",
    );
    let [class, total, ..] = crate::sched::ipc_recv(report);
    assert_eq!(
        (class, total as usize),
        (
            fixture::code(sink_proto::Sent::Ok),
            fixture::TRANSCRIPT.len()
        ),
        "a writer whose sink stayed put must classify Ok",
    );

    // 2. A sink destroyed under a writer blocked in a send. The endpoint is retyped out of a
    // region this test owns, because destroying the region is how an endpoint dies (§16) and
    // the kernel's own endpoints are not in one.
    let region = crate::untyped::create(4).expect("no untyped for the doomed sink");
    let doomed = crate::sched::create_endpoint_from(region).expect("no endpoint in the region");
    let report = spawn_writer(image, Some(doomed), 0);
    // Take a few messages first, so the writer is demonstrably running and parked in the next
    // send rather than having failed before it ever reached one.
    for _ in 0..3 {
        let _ = crate::sched::ipc_recv(doomed);
    }
    crate::sched::reclaim_region(region).expect("the doomed sink's region would not reclaim");
    let [class, total, ..] = crate::sched::ipc_recv(report);
    assert_eq!(
        class,
        fixture::code(sink_proto::Sent::Gone),
        "a writer whose sink was destroyed classified {class} rather than Gone. Before \
         milestone 50 this arrived as NoSuchSlot, indistinguishable from never having had a \
         sink, which is exactly why `yes | head` could not end.",
    );
    assert!(
        total >= 3,
        "the writer reported {total} bytes through, so it never reached a live send and the \
         classification above says nothing about a sink dying under one",
    );

    // 3. No sink at all: the slot is empty, and the program keeps running.
    let report = spawn_writer(image, None, 1);
    let [class, total, ..] = crate::sched::ipc_recv(report);
    assert_eq!(
        (class, total),
        (fixture::code(sink_proto::Sent::NoSink), 0),
        "a program with an empty output slot must keep running and print into the void, which \
         is what every OS does to a process whose stdout is closed",
    );
}
