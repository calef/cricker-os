use super::*;

/// Reassemble a std program's stdout off its endpoint until the writer says the stream is over,
/// and compare byte for byte.
///
/// The framing is the sink contract's (`crates/sink_proto`, milestone 50), which for bytes is
/// bit-for-bit the framing the PAL used before that contract existed: `w0` is the count, `w1`
/// and `w2` are the bytes, little-endian. `SEND` blocks until a receiver takes it, so the
/// program is somewhere between its last `println!` and `SYS_EXIT` when the bytes land.
///
/// **It reads to end of stream rather than to a byte count, and that is a strengthening.**
/// Stopping at `want.len()` proved the right bytes came out and said nothing about whether more
/// were coming; draining to `OP_EOF` proves the program printed exactly this and then finished,
/// and it exercises the end-of-stream announcement std's `cleanup` now makes, which is what a
/// pipe's reader will depend on.
///
/// Shared by every std test on both ISAs (the arch-gated test modules reach it here), so all of
/// them assert the same way and a drift in one is a diff in one place.
pub(super) fn assert_std_transcript(report: crate::sched::EpId, want: &[u8], what: &str) {
    let mut got = [0u8; 512];
    let len = drain_sink(report, &mut got, what);
    assert_eq!(
        &got[..len],
        want,
        "{what}: stdout did not match the transcript"
    );
}

/// Reassemble a sink-contract stream into `out` until end of stream; returns its length.
///
/// The one decoder for every sink in the suite, on purpose. The indifference test's whole claim
/// is that two destinations produce the same bytes, and it would be a much weaker claim if each
/// arm were decoded by its own code.
pub(super) fn drain_sink(ep: crate::sched::EpId, out: &mut [u8], what: &str) -> usize {
    let mut len = 0usize;
    loop {
        let words = crate::sched::ipc_recv(ep);
        let mut chunk = [0u8; sink_proto::INLINE_MAX];
        match sink_proto::unpack(words[0], words[1], words[2], &mut chunk) {
            sink_proto::Msg::Bytes(n) => {
                for &b in &chunk[..n] {
                    assert!(len < out.len(), "{what}: wrote more than the buffer holds");
                    out[len] = b;
                    len += 1;
                }
            }
            sink_proto::Msg::Eof => return len,
            sink_proto::Msg::Malformed => {
                panic!(
                    "{what}: a sink message the contract does not define: {:#x}",
                    words[0]
                )
            }
        }
    }
}

/// Consume the FS service's two readiness sentinels, if this caller is the one that wired it.
///
/// One boot has one FS service (the block server owns the device), so the hand-written client's
/// test and the `std::fs` test share it, and only the first of them to run gets the sentinels.
/// Asserting on them where they exist is what separates a hang in the mount from one in the
/// serve path.
/// **Kill the filesystem mid-transaction on a real device, then mount what is left of it**
/// (milestone 37, DECISIONS §34 condition 1). Shared by both ISA test modules, so the property
/// and its wording are asserted identically on each (rule 5, §19).
///
/// Six steps, each of which has to happen for the next one to mean anything:
///
/// 1. a block server brings up the crash test's own disk (nothing else in the boot touches it);
/// 2. an FS server mounts it, so the image was sound before this test damaged it;
/// 3. the driver writes payload A, reads it straight back, and reports: **acknowledged**;
/// 4. the driver's second write walks into the injector, which tears a block at 2048 bytes and
///    traps with the transaction's commit unwritten. The `CUT` word is how we know the kill was
///    the injector's rather than something else having gone wrong;
/// 5. a **different FS-server process** mounts the same disk through the same block server. Its
///    readiness sentinel is the consistency result: `Server::open` refuses an image it cannot
///    make sense of, so arriving at all means the filesystem is intact;
/// 6. a fresh client reads the file and says which payload is in it.
///
/// **The assertion is the property, not an outcome.** Either payload is a pass; what fails is a
/// mixture, a length nobody wrote, or the pre-boot contents (which would mean an acknowledged
/// write had vanished). Pinning the answer to "A" would be pinning a detail of when RedoxFS
/// happens to write its commit, and the claim is not about that.
pub(super) fn assert_a_kill_mid_transaction_recovers(
    blk_image: &'static [u8],
    fs_server_image: &'static [u8],
    client_image: &'static [u8],
) {
    use fs_proto::fixture::{READY, SUCCESS, crash};
    let Some(run) = fs_service::start_crash(blk_image, fs_server_image, client_image) else {
        crate::println!("    (no crash disk attached; skipping)");
        return;
    };
    assert_eq!(
        crate::sched::ipc_recv(run.blk_ready)[0],
        READY,
        "the block server did not bring the crash-test disk up",
    );
    assert_eq!(
        crate::sched::ipc_recv(run.fs_ready)[0],
        READY,
        "the FS server did not open the crash-test image, so there was nothing to crash",
    );

    let [w0, w1, ..] = crate::sched::ipc_recv(run.driver_report);
    assert_eq!(
        (w0, w1),
        (SUCCESS, crash::SAW_A),
        "the driver's first write was not acknowledged and read back, so nothing that follows \
         is a statement about an acknowledged write",
    );

    assert_eq!(
        crate::sched::ipc_recv(run.fs_ready)[0],
        crash::CUT,
        "the FS server did not die inside the injector: whatever killed it, it was not this \
         test, and the recovery below would be measuring the wrong thing",
    );

    let (ready, report) = fs_service::recover_crash(fs_server_image, client_image);
    assert_eq!(
        crate::sched::ipc_recv(ready)[0],
        READY,
        "the recovery mount FAILED: a fresh FS server could not open the image the killed one \
         left behind, so a torn write mid-transaction cost the whole filesystem",
    );

    let [len, saw, ..] = crate::sched::ipc_recv(report);
    assert!(
        saw == crash::SAW_A || saw == crash::SAW_B,
        "after a kill mid-transaction the file held {len} bytes that were neither payload \
         whole: a write is either wholly present or wholly absent, and this was neither",
    );
    crate::println!(
        "    (crash recovery: the file holds payload {}, {len} bytes, whole)",
        if saw == crash::SAW_A { "A" } else { "B" },
    );
}

/// **The extended-attribute witness's verdict** (milestone 57), asserted as an exact set so that
/// a client which could do nothing and one which could do everything both fail.
///
/// One copy, here beside the other shared FS assertions, because the two ISA legs assert the
/// same thing and the useful part is naming *which* claim broke rather than printing a number
/// the reader has to decode. Each missing bit is one sentence about the layer.
pub(super) fn assert_attrs(attrs: u64) {
    use fs_proto::fixture::attrs as a;
    if attrs == a::EXPECTED {
        return;
    }
    for (bit, what) in [
        (
            a::SET_AND_READ_BACK,
            "an attribute set and read back with its type code",
        ),
        (a::LISTED, "the listing named it and nothing else"),
        (a::SURVIVED_RENAME, "it followed its file across a rename"),
        (a::GONE_AFTER_REMOVE, "removing it made it ENODATA"),
        (
            a::GONE_AFTER_UNLINK,
            "a file remade at the same name inherited nothing",
        ),
        (
            a::OVERSIZE_REFUSED,
            "an over-long value was refused with E2BIG",
        ),
        (
            a::STORE_UNNAMEABLE,
            "the attribute store could not be named",
        ),
        (a::STORE_UNLISTED, "and did not appear in an enumeration"),
    ] {
        if attrs & bit == 0 {
            crate::println!("    MISSING: {what}");
        }
    }
    if attrs & a::ATTRS_FAILED != 0 {
        crate::println!("    the witness reported that something it should do failed");
    }
    panic!(
        "the extended-attribute witness reported {attrs:#x}, expected {:#x}",
        a::EXPECTED,
    );
}

pub(super) fn assert_fs_service_ready(readiness: Option<(crate::sched::EpId, crate::sched::EpId)>) {
    // One copy, in `fs_service`, because draining these is **sequencing** and not only an
    // assertion: each server is parked inside its own blocking announcement until somebody
    // receives it, so nothing it serves can be answered first. The caretakers depend on that.
    fs_service::wait_for_service(readiness);
}

/// Build the exact bytes `std_exerciser` prints when it is granted a directory capability, into
/// `buf`; returns the length. Not a `const` because the motd's contents are spliced in from the
/// shared fixture, and that is the load-bearing part: those bytes came off the RedoxFS image,
/// through the FS server, through `std::fs`, and out the stdout endpoint.
pub(super) fn std_fs_expected(buf: &mut [u8; 512]) -> usize {
    // The lengths spelled out below are the motd's; if the fixture changes, fail here rather
    // than in a byte comparison nobody can read.
    assert_eq!(
        fs_proto::fixture::MOTD.len(),
        70,
        "the motd fixture changed; the expected transcript's lengths must change with it",
    );
    let mut n = 0;
    for part in [
        b"std fs on cricker-os\n".as_slice(),
        fs_proto::fixture::MOTD,
        b"read_to_string 70\nmetadata len 70\n".as_slice(),
        b"absolute refused\ndotdot refused\nnested refused\n".as_slice(),
        b"missing not found\n".as_slice(),
        // Milestone 31 phase 2: `create unsupported` became `write create ok`, plus the two
        // refusals that prove CREATE did not widen what a client can reach.
        b"write create ok\ncreate_new refused\n".as_slice(),
        b"create refused absolute\ncreate refused dotdot\n".as_slice(),
        b"write readback ok\n".as_slice(),
        // Milestone 64: the namespace verbs. Every one of these was `Unsupported` in the PAL
        // while the FS server had been dispatching the verb behind it since milestones 47 and 48,
        // so these nine lines are a binding proven rather than a contract widened.
        b"mkdir ok\nread_dir ok\nread_dir descend ok\n".as_slice(),
        b"unlink refused a directory\nrmdir refused a file\n".as_slice(),
        b"rename ok\nunlink ok\nis_dir ok\nrmdir ok\n".as_slice(),
        b"fs ok\n".as_slice(),
    ] {
        buf[n..n + part.len()].copy_from_slice(part);
        n += part.len();
    }
    n
}

/// The exact bytes `std_exerciser` prints, in order. `println!` is line-buffered and every line
/// ends in `\n`, so the whole transcript is flushed by the time the program exits. Pinned here
/// so a drift in std's behaviour, the PAL, or the demo is a loud diff rather than a mystery.
/// `os cricker` proves `std::env::consts::OS` resolves through the patched `env_consts`; the
/// two `unsupported` lines prove `fs`/`net` refuse honestly rather than pretend.
pub(super) const EXPECTED: &[u8] = b"hello from std on cricker-os\n\
    os cricker\n\
    vec sum 149985000\n\
    string len 800\n\
    map lookup 1369\n\
    fs honestly unsupported\n\
    net honestly unsupported\n\
    instant monotonic ok\n\
    wall clock ok\n\
    entropy ok\n";

/// A whole Rust `std` program runs on the native ABI and its output is exactly right.
///
/// Granted a heap, a stdout endpoint, and a clock, so both `fs` and `net` refuse: the two
/// `unsupported` lines in the transcript are "no ambient filesystem" and "no ambient network"
/// felt from inside std, on a binary that also runs both for real when it is granted them.
///
/// The `wall clock ok` line is milestone 51's half: `SystemTime::now()` reached a real time,
/// through a read-only page and the ambient counter, and the program asserted it was inside the
/// same sanity window the clock service applies. Before that milestone the same call returned
/// 1970 plus uptime and would have passed any test that only checked it did not crash.
///
/// The `entropy ok` line is milestone 56's half, and it is the same shape of correction:
/// `std::random` reached a virtio-rng device, through one endpoint that names no device, and
/// the program asserted two draws differ. Before that milestone the same call returned
/// splitmix64 seeded from boot-relative time, which would also have passed any test that only
/// checked it did not crash.
#[test_case]
fn a_whole_std_program_runs_on_the_native_abi() {
    let image = program("std_exerciser").expect("no std_exerciser program in the initrd archive");
    let clock = program("clock").expect("no clock program in the initrd archive");
    let entropy = program("entropy").expect("no entropy program in the initrd archive");
    let report = std_service::start(image, clock, entropy);
    assert_std_transcript(report, EXPECTED, "std_exerciser");
}
