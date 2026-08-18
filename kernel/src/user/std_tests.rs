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
    let mut got = [0u8; 768];
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

/// **The SMB write path's independent reader** (milestone 54's second half), shared by both ISAs
/// for the same reason [`assert_fs_service_ready`] is: the two combined-inbound tests are twins
/// and this is the half of each that must not drift.
///
/// The host's SMB prober wrote a file over TCP and believed its own write. This spawns a
/// *different* process, holding a directory capability and nothing that names the network, to
/// read the same file back through the FS server. Bytes crossing in both directions with a
/// different process witnessing each is what makes this a gate rather than a client agreeing with
/// itself, and it is exactly the discipline the seed role applies to the read direction.
///
/// **It checks the subdirectory too** (milestone 54's third act), and that check is the one the
/// prober could not make for itself: the prober asked for a directory and got a success, but only
/// a reader on this side can say whether a *directory* is what reached RedoxFS or a file whose
/// name happens to contain a separator, which is what a flat share produces and what looks
/// identical on the wire.
///
/// It runs after both verdicts, because the adapter has to have finished serving before there is
/// anything settled to read.
pub(super) fn assert_smb_write_landed(had_fs: bool) {
    use fs_proto::fixture::smb_wrote;
    if !had_fs {
        crate::println!("    (no RedoxFS disk attached; nothing to verify the SMB write against)");
        return;
    }
    let (_, report) = fs_service::start(
        fs_service::blk_server_image(),
        program("fs_server").expect("no fs_server program in the initrd archive"),
        program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
        8, // ROLE_SMB_VERIFY, matching user/src/fs_test_client.rs
    )
    .expect("the FS service was wired for the SMB share a moment ago");
    let words = crate::sched::ipc_recv(report);
    assert_eq!(
        words[0],
        fs_proto::fixture::SUCCESS,
        "the SMB write verifier did not report",
    );
    assert_eq!(
        words[1],
        smb_wrote::EXACT,
        "what the host wrote over SMB2 is not what the filesystem holds (verdict {}). {} means \
         the name is not there, so the write never reached RedoxFS; {} means the length is \
         wrong, which is the SET_INFO truncate leg; {} means the bytes are wrong, which is an \
         offset or a chunking bug in the write path. The subdirectory verdicts: {} means the \
         directory was never made; {} means a FILE was made with a separator in its name, which \
         is a share that is still flat; {} means the directory is there and the file in it is \
         not; {} means the nested bytes are wrong",
        words[1],
        smb_wrote::ABSENT,
        smb_wrote::WRONG_SIZE,
        smb_wrote::WRONG_BYTES,
        smb_wrote::DIR_ABSENT,
        smb_wrote::DIR_IS_A_FILE,
        smb_wrote::NESTED_ABSENT,
        smb_wrote::NESTED_WRONG,
    );
    assert_smb_flush_reached_the_device(words[2]);
}

/// **The `VOLUME_FULL_SYNC` claim, checked rather than asserted** (milestone 55).
///
/// The SMB server tells macOS the share honours a full sync, which is the single bit that makes it
/// willing to hold a backup. Until this milestone that claim outran the stack: every `fs_proto`
/// write committed to the RedoxFS header ring before the reply, so SMB2's `FLUSH` had nothing left
/// to do *above* the block device, and below it the block server issued no `VIRTIO_BLK_T_FLUSH` at
/// all. The last acknowledged write was durable on the device's word rather than on ours.
///
/// What the third report word carries is a witness's verdict, and what makes it a witness is the
/// arrangement rather than the check. The host prober sent `FLUSH` over TCP and was told yes; a yes
/// is what the old server said too. The in-guest process reading this has synced nothing itself, so
/// the block server's flush count being **already past one** when it first asks is a fact only the
/// SMB server can have caused. Then it syncs twice and requires the count to move, which is what
/// separates a device round trip from a remembered number.
///
/// **The control was run, which is what makes the rest of this count.** With `FsShare::sync`
/// reverted to the old behaviour (a bare `Ok(())` that asks the FS server nothing), and everything else
/// unchanged including the host prober's `FLUSH` and its success, this test fails with
/// [`fs_proto::fixture::durability::NEVER_FLUSHED`]. So the witness is watching the flush rather
/// than the mount, and a regression to a polite success cannot pass it.
///
/// [`fs_proto::fixture::durability::NO_DEVICE_FLUSH`] is not a failure of this tree and does not
/// pretend to be: it means the device offers no `VIRTIO_BLK_F_FLUSH`, so nothing here could back
/// the claim on this machine. It still fails the suite, because a boot that cannot demonstrate
/// durability must not pass a test named for it; what it must not do is send the reader looking for
/// a bug in the flush path.
fn assert_smb_flush_reached_the_device(verdict: u64) {
    use fs_proto::fixture::durability;
    if verdict == durability::DURABLE {
        return;
    }
    match verdict {
        durability::NEVER_FLUSHED => crate::println!(
            "    nothing had flushed the device before the witness ran, so the host's SMB2 FLUSH              did not reach fs_proto::fs::SYNC"
        ),
        durability::NO_DEVICE_FLUSH => crate::println!(
            "    the device offers no VIRTIO_BLK_F_FLUSH, so this machine cannot back              VOLUME_FULL_SYNC at all. Not a bug in the flush path"
        ),
        durability::REFUSED => crate::println!(
            "    fs_proto::fs::SYNC was refused for a reason other than an unsupported device;              the verb is wired wrong"
        ),
        durability::NOT_ADVANCING => crate::println!(
            "    two syncs returned the same flush count, so the second one never reached the              device: the server is answering a constant"
        ),
        _ => crate::println!("    the witness reported a verdict this test does not know"),
    }
    panic!(
        "the durability witness reported {verdict}, expected {} (DURABLE). The SMB server claims          apple::VOLUME_FULL_SYNC to macOS, and this is the check that the claim is backed by a          real VIRTIO_BLK_T_FLUSH rather than by a polite success",
        durability::DURABLE,
    );
}

/// **The kernel looks at the frame the SMB adapter and the credential service share**, after two
/// authenticated SMB sessions have completed through it.
///
/// This is the check the adapter could not make about itself, and it is the same one milestone 65's
/// `no_ntlm_key_material_survives_in_the_shared_frame` makes one layer down: a program can only see
/// what it was given, and the claim here is about what the service did *not* put in a page two
/// processes map, and about what the adapter did not keep.
///
/// **It is what makes `an_smb_server_authenticates_a_session_without_ever_holding_the_key` a claim
/// about the real SMB server rather than about a stand-in.** That test spawns
/// `credentialer_test_client` in a role shaped like an SMB server; this one has the actual adapter,
/// which has just authenticated a real macOS-shaped NTLMv2 exchange from a host process, and looks
/// for the three byte strings an attacker would want. The `NTOWFv2` is the one that must never
/// exist outside the service at all; the `SessionBaseKey` is allowed to cross but only for the
/// exchange it belongs to, and this adapter never asks for it, so finding it would mean the service
/// published it into a page that then outlived the session.
///
/// The values are [MS-NLMP] §4.2.4's, for `cred_proto::fixture`'s account, and they are the same
/// literals the milestone-65 test carries. Duplicated rather than shared because they are what a
/// published specification prints: a constant naming them would let one edit move both, and the
/// point of a pinned vector is that it cannot be edited into agreement with the code.
pub(super) fn assert_smb_held_no_key(had_fs: bool) {
    if !had_fs {
        return; // no filesystem means no authenticated share and no credential service wired
    }
    let mut page = [0u8; 4096];
    crate::user::credential_service::peek(
        crate::user::credential_service::verify_page_va(),
        &mut page,
    );
    for (what, bytes) in [
        // NTOWFv2, §4.2.4.1.1: the stored key, which must never leave the credential service.
        (
            "the stored NTLM key",
            [
                0x0c, 0x86, 0x8a, 0x40, 0x3b, 0xfd, 0x7a, 0x93, 0xa3, 0x00, 0x1e, 0xf2, 0x2e, 0xf0,
                0x2e, 0x3f,
            ],
        ),
        // SessionBaseKey, §4.2.4.1.2. **This is the one the service deliberately publishes** on a
        // match, so its absence is a fact about the *adapter* rather than about the service: the
        // adapter wipes the page the moment the reply lands, because it does not sign and has no use
        // for it. Note that a real run's session key is not this literal (the challenge is the
        // connection's, not §4.2.1's), so the all-zero check below is what actually catches it; this
        // literal stays because "does not contain the published key" is the property that must hold
        // if the wipe is ever narrowed, and a test that only checked for zero would go green on a
        // change that broke it. Same argument as `the_shared_frame_holds_nothing_after_an_answer`.
        (
            "the session key",
            [
                0x8d, 0xe4, 0x0c, 0xca, 0xdb, 0xc1, 0x4a, 0x82, 0xf1, 0x5c, 0xb0, 0xad, 0x0d, 0xe9,
                0x5c, 0xa3,
            ],
        ),
    ] {
        assert!(
            !page.windows(bytes.len()).any(|s| s == bytes),
            "{what} is still in the frame the SMB adapter and the credential service share, after \
             the adapter authenticated a session through it",
        );
    }
    if let Some(i) = page.iter().position(|&b| b != 0) {
        panic!(
            "byte {i} of the SMB adapter's credential frame is {:#04x} after its last exchange: \
             something the adapter presented, or something of the store's, is still sitting in a \
             page two processes map",
            page[i],
        );
    }
}

/// Build the exact bytes `std_exerciser` prints when it is granted a directory capability, into
/// `buf`; returns the length. Not a `const` because the motd's contents are spliced in from the
/// shared fixture, and that is the load-bearing part: those bytes came off the RedoxFS image,
/// through the FS server, through `std::fs`, and out the stdout endpoint.
pub(super) fn std_fs_expected(buf: &mut [u8; 768]) -> usize {
    // The lengths spelled out below are the motd's; if the fixture changes, fail here rather
    // than in a byte comparison nobody can read.
    assert_eq!(
        fs_proto::fixture::MOTD.len(),
        70,
        "the motd fixture changed; the expected transcript's lengths must change with it",
    );
    let mut n = 0;
    for part in [
        b"std fs on nife\n".as_slice(),
        fs_proto::fixture::MOTD,
        b"read_to_string 70\nmetadata len 70\n".as_slice(),
        // Milestone 122 changed the third of these. `sub/motd` used to be refused for being
        // nested, which is no longer a category; what replaces it is `sub/../other/secret`, a real
        // file in a real sibling directory that this process CAN reach by naming `other/secret`,
        // refused through the `..` route. The refusal moved from "a path with two components" to
        // "there is no ascent", which is the property that was always the point.
        b"absolute refused\ndotdot refused\ninner dotdot refused\n".as_slice(),
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
        // Milestone 64's second pass: `File::set_len` (`TRUNCATE` with a size, both directions) and
        // `fs::copy` (no verb, and none needed). Same shape of finding as the nine lines above, one
        // milestone later: the refusal outlived the reason for it.
        b"set_len ok\ncopy ok\n".as_slice(),
        // **Milestone 122: descent.** The first three lines are a nested path resolving, a second
        // descent below it, and the pair that was actually broken: list a subdirectory, then open
        // every file the listing named through the `path()` the listing handed back. That pair is
        // what every filesystem walker is built out of, and one level down it used to fail.
        b"descend read ok\ndescend twice ok\nwalk entry ok\n".as_slice(),
        // A path may only walk *through* directories, and a held directory is bound exactly as the
        // granted one is. `dir handle ok` is `std::fs::Dir`, std's own `openat`-shaped API, running
        // on the capability it already is: nife used to get std's generic fallback, which stores a
        // `canonicalize`d path and therefore failed at its first call.
        b"through a file refused\ndir handle ok\n".as_slice(),
        // `rename` across two directories is one atomic message, because `RENAME` carries both
        // handles and both are now walked. `remove_dir_all` needed no nife code at all: std's
        // generic recursion is written in terms of `read_dir` and `remove_file` on paths it
        // composes itself, so it started working the moment those paths resolved.
        b"rename across ok\nremove_dir_all ok\n".as_slice(),
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
/// `os nife` proves `std::env::consts::OS` resolves through the patched `env_consts`; the
/// two `unsupported` lines prove `fs`/`net` refuse honestly rather than pretend.
pub(super) const EXPECTED: &[u8] = b"hello from std on nife\n\
    os nife\n\
    vec sum 149985000\n\
    string len 500\n\
    map lookup 1369\n\
    fs honestly unsupported\n\
    net honestly unsupported\n\
    instant monotonic ok\n\
    wall clock ok\n\
    entropy ok\n\
    env ok\n\
    paths ok\n\
    exiting through process::exit\n";

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
/// The `env ok` line is milestone 64's, and it is a third correction of the same shape, found by a
/// different method. `std::env::vars()` used to **abort the process**: nife had no `sys::env`
/// backend, so it fell through to the unsupported one whose `env()` is a `panic!`. Nothing in a
/// build failure list showed it, because it compiled. The line asserts three things at once: that
/// the call returns, that the listing is empty (nothing endows a nife process with variables), and
/// that `set_var` takes, since that is what `set_var` means on every other platform.
///
/// The `paths ok` line is the same finding a third time, and it is why this test grew rather than
/// a new one appearing. `std::env::temp_dir()`, `std::env::split_paths()` and `std::process::id()`
/// were all `panic!` in the fallbacks nife had no arm in, so each of them **killed the program**
/// while compiling perfectly; `tempfile::NamedTempFile::new()` reached the first of them before it
/// ever got to its own "operation not supported" arm, which is not what the measurement note said
/// it did. The line asserts the two that now answer (`temp_dir` is the granted directory and
/// `TMPDIR` steers it; a path list survives a join and a split) and, just as deliberately, the
/// three that still refuse (`current_dir`, `current_exe`, `home_dir`), so that a later lane cannot
/// quietly turn a refusal into a fabricated answer.
///
/// The `entropy ok` line is milestone 56's half, and it is the same shape of correction:
/// `std::random` reached a virtio-rng device, through one endpoint that names no device, and
/// the program asserted two draws differ. Before that milestone the same call returned
/// splitmix64 seeded from boot-relative time, which would also have passed any test that only
/// checked it did not crash.
///
/// The `exiting through process::exit` line, and the two assertions after the transcript, are
/// milestone 64's fourth pass and the same shape a fourth time. `std::process::exit` fell through
/// to `crate::intrinsics::abort()`, so a program ending the way almost every CLI-shaped `main`
/// ends executed a trap and was reported to its supervisor as a crash. It was invisible to the
/// three earlier passes for a reason worth keeping: `sys/exit.rs` is not a `sys/<module>/mod.rs`
/// backend, so "read every module the PAL falls through" does not reach it, and `_start` calls
/// the PAL's own exit directly, so the *usual* way a program ends never went near the broken one.
/// `cargo xtask std-aborts` is the mechanism that replaces that reading.
#[test_case]
fn a_whole_std_program_runs_on_the_native_abi() {
    use core::sync::atomic::Ordering;

    use crate::arch::exceptions::USER_FAULTS;

    let image = program("std_exerciser").expect("no std_exerciser program in the initrd archive");
    let clock = program("clock").expect("no clock program in the initrd archive");
    let entropy = program("entropy").expect("no entropy program in the initrd archive");
    let faults_before = USER_FAULTS.load(Ordering::Relaxed);
    let (report, tid) = std_service::start(image, clock, entropy);
    assert_std_transcript(report, EXPECTED, "std_exerciser");

    // **The exit is part of the transcript's claim, and it was not being checked** (milestone 64,
    // fourth pass). The last line the program prints is the one before `std::process::exit(0)`,
    // whose `_ =>` arm in `sys/exit.rs` was `crate::intrinsics::abort()`: a `brk`, which the kernel
    // takes as a fault. So this program used to print a perfect transcript and then die as a crash,
    // and every byte above passed while it did.
    //
    // Waiting for the thread to be *gone* rather than for a timeout is what makes this cheap and
    // race-free. The transcript ends inside `rt::cleanup`, which runs before the process leaves, so
    // reading the last byte proves nothing about how it left; both a clean exit and a fault reap an
    // unsupervised thread, so the departure is the synchronisation and the fault counter is the
    // verdict.
    assert!(
        super::tests::wait_for(|| !crate::sched::thread_present(tid)),
        "the std program never left: it is neither exited nor faulted",
    );
    assert_eq!(
        USER_FAULTS.load(Ordering::Relaxed),
        faults_before,
        "std::process::exit trapped instead of exiting: a clean exit reported as a crash",
    );
}
