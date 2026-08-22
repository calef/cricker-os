//! **The sink contract's ends, in one binary** (milestone 50, notes/sink-protocol.md).
//!
//! Three roles, chosen by `arg0` at START. Two of them are the two sides of `sink_proto` and the
//! third reads back what the second one wrote.
//!
//! # `ROLE_FILE`: a file behind a sink
//!
//! The `fs_file_caretaker` shape, one contract further out. `fs_file_caretaker` is a caretaker
//! because it "serves the same `fs_proto` protocol its own client speaks"; this one serves a
//! *different* and much smaller protocol than it speaks, and that asymmetry is the point. It holds
//! an FS-service endpoint, which is a directory capability: it can open names, read them, write at
//! arbitrary offsets, truncate, and stat. Its client holds an endpoint to this process, over which
//! the only expressible request is *here are up to sixteen bytes, append them*.
//!
//! So `> report.txt` grants strictly less than Unix's fd 1 does, and it is structural rather than
//! policy: the confined program cannot seek, cannot truncate, cannot re-read and cannot stat,
//! because there is no message that says any of those things and nothing in its cspace that names
//! the FS server. That is milestone 50's claim, and this file is where it is either true or not.
//!
//! # `ROLE_WRITER`: a program that does not know what it is writing to
//!
//! It writes [`sink_proto::fixture::TRANSCRIPT`] to whatever is in slot 0 and reports how the last
//! `SEND` classified. Its whole job is to make the classification assertable **by value**, because
//! "gone" and "never had one" are two numbers and a test that could not tell them apart would not
//! be testing milestone 50's one behaviour change.
//!
//! # `ROLE_VERIFY`: the read-back
//!
//! Opens the file `ROLE_FILE` wrote and streams its contents out **over the sink contract**, which
//! is a small piece of evidence in itself: the same sixteen-byte framing carries a file's contents
//! as carried a program's `println!`.
//!
//! # Capability contract
//!
//! Slot 0 is the byte sink in every role, which is the convention the whole milestone is about:
//! `WRITE` when this process is the writer, `READ` when it is the sink.
//!
//! | | slot 0 | slot 1 | slot 2 | [`PAGE_VA`] |
//! |---|---|---|---|---|
//! | `ROLE_WRITER` | the sink, `WRITE` | report, `WRITE` | | |
//! | `ROLE_FILE` | the sink, `READ` | FS service, `WRITE` | report, `WRITE` | shared with the FS server |
//! | `ROLE_VERIFY` | the sink, `WRITE` | FS service, `WRITE` | report, `WRITE` | shared with the FS server |
//!
//! Name: unrecorded. Introduced 2026-07-31 with the sink contract (DECISIONS §51), taking the
//! contract's own word.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use fs_proto::fs;
use sink_proto::fixture;
use user_rt::{call, exit, recv, send};

/// The byte sink. `WRITE` in a writing role, `READ` in a sink role, and the whole of what a writer
/// holds: no page, no acknowledgement channel, nothing else.
const SINK: u64 = 0;
/// The FS-service endpoint (the sink and verify roles only): a directory capability, and the
/// authority this process holds that its client deliberately does not.
const FS: u64 = 1;
/// Where a sink role reports readiness and its byte total.
const REPORT_FS: u64 = 2;
/// Where the writer role reports; it holds no FS endpoint, so its report is one slot lower.
const REPORT_WRITER: u64 = 1;

/// The page shared with the FS server. Matches `fs_service`'s `FILE_VA_CLIENT`.
const PAGE_VA: u64 = 0x0000_0000_0060_0000;
/// Its size, the FS contract's transfer unit.
const PAGE: usize = fs_proto::PAGE;

const ROLE_WRITER: u64 = 0;
const ROLE_FILE: u64 = 1;
const ROLE_VERIFY: u64 = 2;

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, a1: u64, _a2: u64) -> ! {
    match role {
        ROLE_FILE => file_sink(),
        ROLE_VERIFY => verify(),
        ROLE_WRITER => writer(a1),
        _ => writer(a1),
    }
}

// ---------------------------------------------------------------------------------------------
// The writer

/// Write the transcript `repeat` times, or forever if `repeat` is 0, and report how the last
/// `SEND` classified plus how many bytes got through.
///
/// `repeat = 0` is what the "gone" test uses: the program cannot know when its reader will die, so
/// it writes until the sink tells it to stop, which is exactly the position a real producer in
/// `yes | head` is in. A clean finish announces end of stream; a failed one does not, because there
/// is nothing left to announce it to.
fn writer(repeat: u64) -> ! {
    let mut total = 0u64;
    let mut rounds = 0u64;
    let mut last = sink_proto::Sent::Ok;

    'outer: loop {
        let mut off = 0usize;
        while off < fixture::TRANSCRIPT.len() {
            let (w0, w1, w2, n) = sink_proto::pack(&fixture::TRANSCRIPT[off..]);
            last = sink_proto::classify(send(SINK, w0, w1, w2));
            if !matches!(last, sink_proto::Sent::Ok) {
                break 'outer;
            }
            off += n;
            total += n as u64;
        }
        rounds += 1;
        if repeat != 0 && rounds >= repeat {
            break;
        }
    }

    if matches!(last, sink_proto::Sent::Ok) {
        let _ = send(SINK, sink_proto::eof(), 0, 0);
    }
    send(REPORT_WRITER, fixture::code(last), total, rounds);
    exit();
}

// ---------------------------------------------------------------------------------------------
// The FS side, shared by the sink and the verifier

/// Copy `bytes` into the shared page.
fn put(bytes: &[u8]) {
    for (i, &b) in bytes.iter().take(PAGE).enumerate() {
        // SAFETY: PAGE_VA is a mapped, writable page of PAGE bytes; the loop is clamped to it.
        unsafe { core::ptr::write_volatile((PAGE_VA + i as u64) as *mut u8, b) };
    }
}

/// Read one byte out of the shared page.
fn get(i: usize) -> u8 {
    // SAFETY: as above; callers clamp `i` to the page.
    unsafe { core::ptr::read_volatile((PAGE_VA + i as u64) as *const u8) }
}

/// One request to the FS server. Negative replies are the caller's to interpret.
fn fs_call(w0: u64, w1: u64) -> i64 {
    call(FS, w0, w1).0 as i64
}

/// **Open the sink's file, creating it if it is not there and emptying it if it is.**
///
/// `CREATE` is create, not create-or-open (DECISIONS §27), so the fallback is explicit: on any
/// refusal, open the existing name and truncate it to nothing. Stated rather than inferred, because
/// a sink that appended to whatever a previous run left behind would pass its own test on a stale
/// file. Both ISA legs write the same image name on a fresh image per run, so in practice the
/// create succeeds; the fallback is what keeps a repeat boot honest instead of confusing.
fn open_for_writing() -> u64 {
    let name = fixture::SINK_NAME.as_bytes();
    put(name);
    let created = fs_call(fs::req(fs::CREATE, fs::ROOT, name.len() as u64), 0);
    if created >= 0 {
        return created as u64;
    }
    put(name);
    let opened = fs_call(fs::req(fs::OPEN, fs::ROOT, name.len() as u64), 0);
    if opened < 0 {
        send(REPORT_FS, fixture::BAD, opened as u64, 0);
        exit();
    }
    if fs_call(fs::req(fs::TRUNCATE, opened as u64, 0), 0) < 0 {
        send(REPORT_FS, fixture::BAD, opened as u64, 1);
        exit();
    }
    opened as u64
}

/// **The file sink.** Receive sink messages, append each one's bytes at the running offset, and
/// close on end of stream.
///
/// Each message is written straight through rather than buffered. That costs an FS round trip per
/// sixteen bytes and it is the honest starting point: buffering is a component that speaks the sink
/// contract on both sides, inserted into the chain, not a hidden behaviour of one sink. See
/// notes/sink-protocol.md.
fn file_sink() -> ! {
    let handle = open_for_writing();
    send(REPORT_FS, fixture::READY, 0, 0);

    let mut off = 0u64;
    loop {
        let (w0, w1, w2) = recv(SINK);
        let mut buf = [0u8; sink_proto::INLINE_MAX];
        match sink_proto::unpack(w0, w1, w2, &mut buf) {
            sink_proto::Msg::Bytes(0) => {}
            sink_proto::Msg::Bytes(n) => {
                put(&buf[..n]);
                let wrote = fs_call(fs::req(fs::WRITE, handle, n as u64), off);
                if wrote != n as i64 {
                    // A short write is a lost byte, and a sink that reported DONE after one would
                    // be certifying a file it did not write. Say which request failed and stop.
                    send(REPORT_FS, fixture::BAD, wrote as u64, off);
                    exit();
                }
                off += n as u64;
            }
            sink_proto::Msg::Eof => break,
            sink_proto::Msg::Malformed => {
                send(REPORT_FS, fixture::BAD, w0, off);
                exit();
            }
        }
    }

    let _ = fs_call(fs::req(fs::CLOSE, handle, 0), 0);
    send(REPORT_FS, fixture::DONE, off, 0);
    exit();
}

/// **The read-back.** Open what the file sink wrote and stream it out over the sink contract, so
/// the bytes that reach the test have been through a real filesystem and back.
fn verify() -> ! {
    let name = fixture::SINK_NAME.as_bytes();
    put(name);
    let handle = fs_call(fs::req(fs::OPEN, fs::ROOT, name.len() as u64), 0);
    if handle < 0 {
        send(REPORT_FS, fixture::BAD, handle as u64, 0);
        exit();
    }
    let handle = handle as u64;

    let size = fs_call(fs::req(fs::FSTAT, handle, 0), 0);
    if size < 0 {
        send(REPORT_FS, fixture::BAD, size as u64, 1);
        exit();
    }
    let size = size as u64;

    let mut off = 0u64;
    while off < size {
        let want = (size - off).min(PAGE as u64);
        let got = fs_call(fs::req(fs::READ, handle, want), off);
        if got <= 0 {
            send(REPORT_FS, fixture::BAD, got as u64, off);
            exit();
        }
        // Out of the page and onto the sink, sixteen bytes at a time. The page is the FS contract's
        // transfer unit and the message is the sink contract's; this loop is the seam between them,
        // and it is the whole of what a sink adapter is.
        let mut i = 0usize;
        while i < got as usize {
            let mut chunk = [0u8; sink_proto::INLINE_MAX];
            let n = (got as usize - i).min(sink_proto::INLINE_MAX);
            for (k, b) in chunk[..n].iter_mut().enumerate() {
                *b = get(i + k);
            }
            let (w0, w1, w2, _) = sink_proto::pack(&chunk[..n]);
            send(SINK, w0, w1, w2);
            i += n;
        }
        off += got as u64;
    }

    send(SINK, sink_proto::eof(), 0, 0);
    let _ = fs_call(fs::req(fs::CLOSE, handle, 0), 0);
    send(REPORT_FS, fixture::DONE, size, 0);
    exit();
}

user_rt::panic_handler!();
