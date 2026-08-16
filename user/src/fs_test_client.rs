//! **The FS-service client** (milestone 32 phase 2): opens a file through a granted directory
//! capability and proves the whole stack end to end.
//!
//! This is the program a milestone-31 shell will one day be. It holds an endpoint to the FS server
//! and nothing that names the server, the block server, or the disk. That endpoint IS its directory
//! capability: it can open names the server resolves under the bound directory, and it can open
//! nothing else, because there is no global namespace to reach into. It reads the `motd` the image
//! ships with, then writes a pattern to `scratch` and reads it back, and reports the `motd` head
//! plus a success sentinel. Any failed check panics, which faults, which fails the waiting test.
//!
//! # Capability contract (notes/fs-server.md, notes/abi.md §4)
//! - **slot 0**: the file-service endpoint, `WRITE` (this is the directory capability; the client
//!   `CALL`s here).
//! - **slot 1**: the report endpoint, `WRITE`.
//! - **[`FILE_VA`]**: the page shared with the FS server (a name out, file bytes both ways).
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `fsclient`. Refused `fsclient`
//! (squished) and `fs_client` (any program that wants files is an FS client, so that name belongs
//! to the real thing; it also predicts "a client of the FS service" rather than "the program the
//! kernel spawns to prove the FS contract holds").

#![no_std]
#![no_main]

use fs_proto::{dir, fixture, fs, grant, xattr};
use user_rt::{call, exit, now, send};

/// The file-service endpoint: the client's whole authority to the filesystem. Naming a file over it
/// is a request the server resolves under the one directory this endpoint is bound to.
const FILE: u64 = 0;
/// Where the client reports success (WRITE).
const REPORT: u64 = 1;
/// The client's mapping of the page it shares with the FS server.
const FILE_VA: u64 = 0x0000_0000_0060_0000;

/// A failed check is a fault, not a wrong answer: panic, and the handler traps.
fn check(ok: bool) {
    if !ok {
        panic!();
    }
}

/// Copy `bytes` into the shared page (a name to open, or data to write).
fn put_page(bytes: &[u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        // SAFETY: FILE_VA is a mapped, writable 4096-byte page; `bytes` is far shorter.
        unsafe { core::ptr::write_volatile((FILE_VA + i as u64) as *mut u8, b) };
    }
}

/// Copy `bytes` into the shared page at `off`, for the one request that carries two payloads: a
/// rename's source and destination names sit back to back.
fn put_page_at(off: usize, bytes: &[u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        // SAFETY: FILE_VA is a mapped, writable 4096-byte page; two names are far shorter.
        unsafe { core::ptr::write_volatile((FILE_VA + (off + i) as u64) as *mut u8, b) };
    }
}

/// Read `n` bytes out of the shared page (a completed read landed there).
fn get_page(n: usize, out: &mut [u8]) {
    for (i, b) in out.iter_mut().take(n).enumerate() {
        // SAFETY: as above; `n` is bounded by the page and by `out`.
        *b = unsafe { core::ptr::read_volatile((FILE_VA + i as u64) as *const u8) };
    }
}

/// Open a name under the bound directory; returns the handle. A negative reply (no such file, or a
/// forged request) is REPORTED, not trapped, so the server's reason survives the failure.
fn open(name: &str) -> u64 {
    put_page(name.as_bytes());
    let (r0, _) = call(FILE, fs::req(fs::OPEN, 0, name.len() as u64), 0);
    if (r0 as i64) < 0 {
        fail(STAGE_OPEN, r0);
    }
    r0
}

/// Stage tags for [`fail`], so a reported failure says *which* request the server refused.
const STAGE_OPEN: u64 = 1;
const STAGE_READ: u64 = 2;
const STAGE_WRITE: u64 = 3;

/// **Report a refused request instead of faulting.** A `check` failure traps, and a trapped client
/// tells the waiting test only that something went wrong; the server's *reason* dies with it. This
/// sends it instead, so the reason reaches the kernel test's assertion, which prints the value it
/// compared against `SUCCESS`.
///
/// `w0` is the raw reply word, `w1` is `0xBADD_0000 | stage << 12 | errno`. The errno is recovered
/// with `fs_proto::reply_errno`'s rule (a negative reply is a negated errno). Note the known
/// reply-space overlap (notes/std.md): the kernel's own `invoke` errors are -1..-8, so a small value
/// here is ambiguous between "the server returned this errno" and "the IPC itself failed". The raw
/// word travels in `w0` precisely so that ambiguity is visible rather than hidden.
fn fail(stage: u64, r0: u64) -> ! {
    let errno = if (r0 as i64) < 0 {
        (-(r0 as i64)) as u64
    } else {
        0
    };
    send(REPORT, r0, 0xBADD_0000 | (stage << 12) | errno, 0);
    exit();
}

/// Write `data` to `handle` at `offset`; checks the server accepted the whole slice.
fn write(handle: u64, offset: u64, data: &[u8]) {
    put_page(data);
    let (r0, _) = call(FILE, fs::req(fs::WRITE, handle, data.len() as u64), offset);
    if (r0 as i64) < 0 {
        fail(STAGE_WRITE, r0);
    }
    check(r0 as usize == data.len());
}

/// How long each repeat-write payload is: exactly the fixture pattern's length, so every write in
/// this test (including the final one, which restores the fixture) replaces the file's contents
/// completely. There is no truncate verb, so a shorter write would leave the previous tail behind and
/// the gate's post-run check would see a mixture.
const REPEAT_LEN: usize = fixture::WRITE_PATTERN.len();

/// The payload for repeat-write pass `n`: tagged with the pass, position-dependent after that, and
/// [`REPEAT_LEN`] bytes like every other write here, so a stale or shifted read cannot match. The
/// twin of `fs_server`'s host-side `repeat_write_payload`.
fn repeat_payload(pass: u8) -> [u8; REPEAT_LEN] {
    let mut p = [0u8; REPEAT_LEN];
    p[..8].copy_from_slice(b"CRKRPT__");
    p[7] = b'0' + pass;
    let mut i = 8;
    while i < REPEAT_LEN {
        p[i] = (i as u8).wrapping_mul(31) ^ pass;
        i += 1;
    }
    p
}

/// Read up to `n` bytes from `handle` at `offset` into the shared page; returns the count.
fn read(handle: u64, offset: u64, n: usize) -> usize {
    let (r0, _) = call(FILE, fs::req(fs::READ, handle, n as u64), offset);
    if (r0 as i64) < 0 {
        fail(STAGE_READ, r0);
    }
    r0 as usize
}

/// Iterations for the timed read loop (`ROLE_BENCH`). Warmup pays the OPEN and the first cold read;
/// the timed loop then reads the same block over and over, so it measures the FS-server file-IPC
/// contract (client CALL, server dispatch, handle validation, engine read, copy into the shared page,
/// reply), warm, not disk latency. Self-timed, reported home like `os_primitives_benchmarker`'s primitives.
const BENCH_ITERS: u64 = 2000;
const BENCH_WARMUP: u64 = 64;

/// Roles, chosen by `arg0` at START (mirrors the kernel side). 0 is the end-to-end proof the test
/// spawns; 1 is the benchmark loop the `--real --smp` bench spawns; 2 is the attacker that a
/// milestone-31 per-file grant is measured against. One binary, six entries.
const ROLE_PROOF: u64 = 0;
const ROLE_BENCH: u64 = 1;
const ROLE_ATTACKER: u64 = 2;
/// Milestone 37: drive the writes the FS server is killed in the middle of.
const ROLE_CRASH_DRIVER: u64 = 3;
/// Milestone 37: read the file back through the FS server that mounted the crashed disk.
const ROLE_CRASH_VERIFY: u64 = 4;
/// Milestone 47: the attacker against a per-*directory* grant. `a1` is a run index, which is the
/// only thing it is told: everything else it has to find out by trying.
const ROLE_DIR_ATTACKER: u64 = 5;
/// Milestone 61: the attribute witness behind a **name-set** grant, the third caretaker. Told
/// nothing; it tries a name the set carries and a name it does not, and reports what got through.
const ROLE_SET_ATTRS: u64 = 6;
/// Milestone 54: seed the file the SMB gate reads ([`fixture::SMB_SEED_NAME`]), so the bytes the
/// host's SMB prober asserts were put on the filesystem through `fs_proto` by a different process
/// than the one that serves them.
const ROLE_SMB_SEED: u64 = 7;
/// Milestone 54's write path: read back, through the FS server, the file the **host's** SMB
/// prober wrote over the wire ([`fixture::SMB_WROTE_NAME`]). The seed role's mirror, and the leg
/// that makes the write gate a gate: without it the only witness to a write is the client that
/// performed it, which is the thing a protocol test must never be allowed to be.
const ROLE_SMB_VERIFY: u64 = 8;

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, a1: u64, _a2: u64) -> ! {
    match role {
        ROLE_BENCH => bench(),
        ROLE_ATTACKER => attacker(a1 != 0),
        ROLE_CRASH_DRIVER => crash_driver(),
        ROLE_CRASH_VERIFY => crash_verify(),
        ROLE_DIR_ATTACKER => dir_attacker(a1),
        ROLE_SET_ATTRS => set_attrs(),
        ROLE_SMB_SEED => smb_seed(),
        ROLE_SMB_VERIFY => smb_verify(),
        ROLE_PROOF => proof(),
        _ => proof(),
    }
}

/// **Read back what came in over SMB** (milestone 54's write path): open
/// [`fixture::SMB_WROTE_NAME`] through the FS server and check it holds exactly
/// [`fixture::SMB_WROTE`].
///
/// This runs *after* the SMB adapter has finished serving, in a process that holds a directory
/// capability and nothing that names the network. That separation is the whole point: the bytes
/// were put there by a host process over TCP, and are read here by a different process through
/// `fs_proto`, so "the write crossed SMB2 -> the `Share` seam -> fs_proto -> RedoxFS" is
/// something the machine checks rather than something the writer asserts about itself.
///
/// It classifies rather than merely failing, in [`crash_verify`]'s shape: the second report word
/// says what was found, because "the file is not there at all" and "the file is there and wrong"
/// are different bugs and a bare failure would not say which.
fn smb_verify() -> ! {
    let name = fixture::SMB_WROTE_NAME;
    put_page(name.as_bytes());
    let (h, _) = call(FILE, fs::req(fs::OPEN, 0, name.len() as u64), 0);
    if (h as i64) < 0 {
        send(REPORT, fixture::SUCCESS, fixture::smb_wrote::ABSENT, 0);
        exit();
    }
    let (size, _) = call(FILE, fs::req(fs::FSTAT, h, 0), 0);
    let mut buf = [0u8; 256];
    let want = fixture::SMB_WROTE;
    let n = read(h, 0, want.len().min(buf.len()));
    get_page(n, &mut buf);
    let verdict = if size as usize != want.len() {
        // A size mismatch is the truncate leg failing, which is worth its own word: the prober
        // writes a long payload and then shortens it with SET_INFO, so a file of the wrong
        // length means the shortening never reached the filesystem.
        fixture::smb_wrote::WRONG_SIZE
    } else if &buf[..n] != want {
        fixture::smb_wrote::WRONG_BYTES
    } else {
        fixture::smb_wrote::EXACT
    };
    let (c, _) = call(FILE, fs::req(fs::CLOSE, h, 0), 0);
    check((c as i64) >= 0);
    send(REPORT, fixture::SUCCESS, verdict, 0);
    exit();
}

/// **Seed the SMB gate's file** (milestone 54): put [`fixture::SMB_SEED`] at
/// [`fixture::SMB_SEED_NAME`] under the granted directory, whole and exact, then report.
///
/// CREATE first, and on any refusal fall back to OPEN + TRUNCATE: create is create, not
/// create-or-open (`fs_proto::fs::CREATE`), and this role must be idempotent because the HVF leg
/// reboots the suite on the same disk image the first run already seeded. The truncate is what
/// makes the fallback equal to the create: without it a shorter payload would leave the old tail
/// behind, which is exactly the half-working write §27 warns about.
fn smb_seed() -> ! {
    let name = fixture::SMB_SEED_NAME;
    put_page(name.as_bytes());
    let (r0, _) = call(FILE, fs::req(fs::CREATE, 0, name.len() as u64), 0);
    let h = if (r0 as i64) >= 0 {
        r0
    } else {
        let h = open(name);
        let (t, _) = call(FILE, fs::req(fs::TRUNCATE, h, 0), 0);
        check((t as i64) >= 0);
        h
    };
    write(h, 0, fixture::SMB_SEED);
    let (size, _) = call(FILE, fs::req(fs::FSTAT, h, 0), 0);
    check(size as usize == fixture::SMB_SEED.len());
    let (c, _) = call(FILE, fs::req(fs::CLOSE, h, 0), 0);
    check((c as i64) >= 0);
    send(REPORT, fixture::SUCCESS, 0, 0);
    exit();
}

/// **The crash driver** (milestone 37): write one payload and get an acknowledgement, then write the
/// next one into a server that has been told to die partway through it.
///
/// It reports after the acknowledged write, because that report is the thing the property depends
/// on: "payload A was acknowledged" is what makes losing it a failure rather than a permitted
/// outcome. Then it issues the second write and **never returns from it**, because the server dies
/// inside the request and blocking IPC has no "your server is gone" reply.
///
/// That permanent block is not an oversight, it is the open item DECISIONS §27 and notes/fs-server.md
/// already record: a client of a dead server waits forever, and §26's fault endpoint wired into a
/// supervision tree (milestone 23) is the mechanism that would turn it into a message. This test
/// exhibits it rather than working around it; the thread is `Blocked`, so it costs no CPU, and the
/// kernel test does not wait on this client for anything after the first report.
fn crash_driver() -> ! {
    use fixture::crash;
    let h = open(crash::NAME);

    // Write A and read it straight back. "The server accepted my write" and "my write landed" are
    // different claims, and the second is the one that has to be true before the kill means anything.
    write(h, 0, crash::A);
    let mut buf = [0u8; 128];
    let n = read(h, 0, crash::A.len());
    check(n == crash::A.len());
    get_page(n, &mut buf);
    check(&buf[..n] == crash::A);
    send(REPORT, fixture::SUCCESS, crash::SAW_A, 0);

    // And now the one the server dies inside. This call never returns.
    write(h, 0, crash::B);
    // Unreachable in the test's configuration; if the injector failed to fire, say so rather than
    // exiting quietly and leaving the kernel test to time out with no explanation.
    send(REPORT, fixture::SUCCESS, crash::SAW_B, 0);
    exit();
}

/// **The crash verifier** (milestone 37): a fresh client of a fresh FS server that mounted the disk
/// the killed one left behind. It reports which of the two payloads the file holds.
///
/// It classifies rather than asserts, for the reason milestone 31's attacker reports a bitmap: the
/// interesting failure is not "it did not read back", it is "it read back something that was never
/// written", and a client that panicked on a mismatch would tell the kernel test nothing about
/// which.
fn crash_verify() -> ! {
    use fixture::crash;
    let h = open(crash::NAME);
    let mut buf = [0u8; 256];
    let n = read(h, 0, crash::A.len().max(crash::B.len()) + 8);
    get_page(n, &mut buf);
    let saw = if n == crash::A.len() && &buf[..n] == crash::A {
        crash::SAW_A
    } else if n == crash::B.len() && &buf[..n] == crash::B {
        crash::SAW_B
    } else {
        crash::SAW_NEITHER
    };
    // The length rides home in the first word: a partial write shows up as a number rather than as
    // a bare verdict, which is the difference between "this failed" and "this is what it did".
    send(REPORT, n as u64, saw, 0);
    exit();
}

/// **The attacker against a per-file grant** (milestone 31 phase 2). Spawned holding a *narrowed*
/// endpoint: not the directory capability, but the file caretaker's, which designates exactly one
/// file read-only. Its job is to try everything that would make that sentence false, and to report
/// which attempts got through as a bitmap (`fixture::escape`).
///
/// **It is its own negative control, which is why it reports a bitmap and not a pass.** Run against
/// a read-only grant, every bit must be clear. Run against a read/write grant of the same shape,
/// `WROTE` and `TRUNCATED` must be **set** and everything else clear. Without that second run the
/// first proves very little: a caretaker that refused every request would pass it, and so would a
/// grant that reached nothing at all.
///
/// **What makes the attempts real.** Each one is against something that exists and that the process
/// one hop up the chain can genuinely reach: the neighbour file is on the image, one directory
/// entry away, and the caretaker could open it on any request it liked. Milestone 33's attacker was
/// handed a real neighbouring client's address rather than a fictional one for exactly this reason,
/// and milestone 36 used two witnesses for the reason the paragraph above gives.
///
/// `writable` also selects *which* file is granted, and that is a fixture constraint rather than a
/// design one: the writable run damages what it is given, so it is given `scratch` (whose contents
/// the gate's post-run host check pins, and which this restores as its last write) rather than
/// `motd`, which two other tests compare byte for byte.
fn attacker(writable: bool) -> ! {
    use fixture::escape;
    let mut verdict = 0u64;
    let mut buf = [0u8; 128];

    let (granted, neighbour) = if writable {
        (fixture::SCRATCH_NAME, fixture::MOTD_NAME)
    } else {
        (fixture::MOTD_NAME, fixture::SCRATCH_NAME)
    };

    // The control: the one file this capability designates must open, and read back what is on it.
    //
    // The read-only run compares against `MOTD`, which nothing ever writes. The writable run cannot
    // and must not: the suite runs its tests in **alphabetical order**, so this program runs before
    // the two clients that put a known pattern in `scratch`, and asserting on those bytes here would
    // be asserting on a history this test did not write. That is the exact failure DECISIONS §27 was
    // corrected four times over, met again from the other side. So the writable run's control is that
    // the file reads back the number of bytes `FSTAT` says it has, and, further down, that the bytes
    // it writes are the bytes it reads back.
    put_page(granted.as_bytes());
    let (h, _) = call(FILE, fs::req(fs::OPEN, 0, granted.len() as u64), 0);
    if (h as i64) < 0 {
        verdict |= escape::GRANTED_READ_FAILED;
    } else {
        let (size, _) = call(FILE, fs::req(fs::FSTAT, h, 0), 0);
        let want = if writable {
            size as usize
        } else {
            fixture::MOTD.len()
        };
        let (n, _) = call(FILE, fs::req(fs::READ, h, want as u64), 0);
        if (size as i64) < 0 || (n as i64) < 0 || n as usize != want || want > buf.len() {
            verdict |= escape::GRANTED_READ_FAILED;
        } else {
            get_page(n as usize, &mut buf);
            if !writable && &buf[..n as usize] != fixture::MOTD {
                verdict |= escape::GRANTED_READ_FAILED;
            }
        }
    }

    // 1. A second file, by name. It exists, it sits in the same directory, and the caretaker could
    //    open it. This capability names one file, so this must find nothing.
    put_page(neighbour.as_bytes());
    let (r, _) = call(FILE, fs::req(fs::OPEN, 0, neighbour.len() as u64), 0);
    if (r as i64) >= 0 {
        verdict |= escape::SECOND_FILE;
    }

    // 2. A write, against the file it IS allowed to read. Refusing a write to a file it cannot even
    //    name would prove nothing; this is the sharp case. When it is *supposed* to succeed, the
    //    bytes are read straight back: "the server accepted my write" and "my write landed" are
    //    different claims, and only the second one makes this a control for the refusal above.
    let probe = probe_payload();
    put_page(&probe[..PROBE_LEN]);
    let (w, _) = call(FILE, fs::req(fs::WRITE, grant::HANDLE, PROBE_LEN as u64), 0);
    if (w as i64) >= 0 {
        verdict |= escape::WROTE;
        let (rb, _) = call(FILE, fs::req(fs::READ, grant::HANDLE, PROBE_LEN as u64), 0);
        get_page(PROBE_LEN, &mut buf);
        if rb as usize != PROBE_LEN || buf[..PROBE_LEN] != probe[..PROBE_LEN] {
            verdict |= escape::GRANTED_READ_FAILED;
        }
    }

    // 3. Truncation is a write that carries no bytes, so a guard that only covered WRITE would miss
    //    it, and truncating to zero destroys a file just as thoroughly.
    let (t, _) = call(FILE, fs::req(fs::TRUNCATE, grant::HANDLE, 0), 0);
    if (t as i64) >= 0 {
        verdict |= escape::TRUNCATED;
    }

    // 4. Creating a new name: the way to get a second file without opening one that exists. Refused
    //    in both directions, because a file capability is not a directory.
    put_page(b"made-by-attacker");
    let (c, _) = call(FILE, fs::req(fs::CREATE, 0, 16), 0);
    if (c as i64) >= 0 {
        verdict |= escape::CREATED;
    }

    // 5. Handle guessing. The caretaker minted one handle and the FS server's own handle for the
    //    file is a different number this process never saw, so spraying numbers is probing a table
    //    it is not addressing. Every miss must be refused by the same check.
    let mut guess = 1u64;
    while guess < 8 {
        let (g, _) = call(FILE, fs::req(fs::READ, guess, 8), 0);
        if (g as i64) >= 0 {
            verdict |= escape::FORGED_HANDLE;
        }
        guess += 1;
    }

    // 6. **Extended attributes** (milestone 61). Before it, all three caretakers answered
    //    `EOPNOTSUPP` to all four verbs, so a program behind a per-file grant could not reach its
    //    own file's attributes. That was uniform and honest and still a capability the confined
    //    program should have had: an attribute is part of what a file *is*, so a capability that may
    //    read the file may read what is attached to it, and one that may not write it may not change
    //    them.
    //
    //    Three probes, and the middle one is the sharp one.
    //
    //    A listing works through **either** direction, because it is a read. Zero attributes is a
    //    successful listing (`r0` = 0), so this distinguishes "forwarded and there were none" from
    //    "refused", which the old behaviour could not.
    let (l, _) = call(FILE, fs::req(fs::LISTXATTR, grant::HANDLE, 0), 0);
    if (l as i64) < 0 {
        verdict |= escape::GRANTED_ATTRS_FAILED;
    }

    //    A get of an attribute that is not set must answer `ENODATA`, **not** `EOPNOTSUPP`. That is
    //    the difference between a request that reached the store and one the caretaker refused, and
    //    it is the only probe here that a read-only grant can make about a verb that reads.
    put_page(fixture::attrs::NAME);
    let (g0, _) = call(
        FILE,
        fs::req(
            fs::GETXATTR,
            grant::HANDLE,
            fixture::attrs::NAME.len() as u64,
        ),
        0,
    );
    if fs_proto::reply_errno(g0 as i64) == Some(xattr::ENOTSUP) {
        verdict |= escape::GRANTED_ATTRS_FAILED;
    }

    //    A set: accepted only with the write direction, which is `WROTE`'s and `TRUNCATED`'s rule
    //    applied to the third way of changing a file. When it is accepted it is read straight back
    //    with its **type code**, because "the server took my attribute" and "my attribute is on the
    //    file" are different claims, and then removed, so the run leaves the image as it found it.
    put_page(fixture::attrs::NAME);
    put_page_at(fixture::attrs::NAME.len(), fixture::attrs::VALUE);
    let (s, _) = call(
        FILE,
        fs::req(
            fs::SETXATTR,
            grant::HANDLE,
            fixture::attrs::NAME.len() as u64,
        ),
        xattr::spec(fixture::attrs::KIND, fixture::attrs::VALUE.len() as u64),
    );
    if (s as i64) >= 0 {
        verdict |= escape::WROTE_ATTR;
        put_page(fixture::attrs::NAME);
        let (g, _) = call(
            FILE,
            fs::req(
                fs::GETXATTR,
                grant::HANDLE,
                fixture::attrs::NAME.len() as u64,
            ),
            0,
        );
        get_page(xattr::reply_value_len(g as i64), &mut buf);
        if (g as i64) < 0
            || xattr::reply_kind(g as i64) != fixture::attrs::KIND
            || xattr::reply_value_len(g as i64) != fixture::attrs::VALUE.len()
            || &buf[..fixture::attrs::VALUE.len()] != fixture::attrs::VALUE
        {
            verdict |= escape::GRANTED_ATTRS_FAILED;
        }
        put_page(fixture::attrs::NAME);
        let (rm, _) = call(
            FILE,
            fs::req(
                fs::REMOVEXATTR,
                grant::HANDLE,
                fixture::attrs::NAME.len() as u64,
            ),
            0,
        );
        if (rm as i64) < 0 {
            verdict |= escape::GRANTED_ATTRS_FAILED;
        }
    } else if writable {
        // A writable grant that cannot set one is the control for the refusal: without it, a
        // caretaker that refused every attribute request would pass the read-only run.
        verdict |= escape::GRANTED_ATTRS_FAILED;
    }

    // Leave the fixture behind, as the LAST write, on the run that was allowed to damage it. The gate
    // reopens the image with the host tool afterwards and compares `scratch` byte for byte, so an
    // attacker that walked away from a truncated file would fail a check three steps downstream and
    // look like a filesystem bug. That exact coupling is what DECISIONS §27 was corrected four times
    // over, so it is paid off here rather than left to be rediscovered.
    if writable {
        let pat = fixture::WRITE_PATTERN;
        put_page(pat);
        let (rw, _) = call(FILE, fs::req(fs::WRITE, grant::HANDLE, pat.len() as u64), 0);
        let (rr, _) = call(FILE, fs::req(fs::READ, grant::HANDLE, pat.len() as u64), 0);
        get_page(pat.len(), &mut buf);
        if (rw as i64) < 0 || rr as usize != pat.len() || &buf[..pat.len()] != pat {
            verdict |= escape::GRANTED_READ_FAILED;
        }
    }

    send(REPORT, fixture::VERDICT, verdict, 0);
    exit();
}

/// **The attacker against a per-directory grant** (milestone 47). Spawned holding an endpoint to a
/// `fs_subtree_caretaker`, which is a capability to one subtree with one rights set. Its job is to
/// make "it cannot reach its parent or a sibling, and it carries no right it was not given" false.
///
/// **It is told nothing about its own grant** beyond a run index (which only keeps the names it
/// creates distinct across the runs that share one image). That is deliberate: it attempts every
/// verb and reports a bitmap of what got through, and the kernel test asserts the *exact* expected
/// set for each configuration. So the specification lives in the test rather than in the program
/// being tested, and three runs of the same code are each other's controls: a caretaker that
/// refused everything fails the wide run, and a caretaker that allowed everything fails the narrow
/// one.
///
/// **What makes the attempts real.** `motd` is in the parent and `other/secret` is in a sibling, and
/// both are on the image, one directory entry from the caretaker, which could open either on any
/// request it liked. Milestone 33's attacker was handed a real neighbour's address for exactly this
/// reason.
///
/// It never writes to `sub/inner` or anything outside `sub`, so the post-run host check can pin
/// those bytes from outside the guest and catch an escape this program failed to report.
fn dir_attacker(run: u64) -> ! {
    use fixture::{dirscape as esc, tree};
    let mut v = 0u64;
    let mut buf = [0u8; 256];

    // 1. The parent. `motd` is in the granted directory's parent and is not in the granted
    //    directory, so this must find nothing. Not a refusal: in this scope there is no such name.
    if dir_open(fs::ROOT, fixture::MOTD_NAME) >= 0 {
        v |= esc::REACHED_PARENT;
    }
    // 2. The sibling, by both routes: descending into it and naming the file inside it.
    if dir_descend(fs::ROOT, tree::OTHER, dir::ALL) >= 0 || dir_open(fs::ROOT, tree::SECRET) >= 0 {
        v |= esc::REACHED_SIBLING;
    }
    // 3. `..`, which is not a component this contract accepts, at any rights.
    if dir_descend(fs::ROOT, "..", dir::ALL) >= 0 || dir_open(fs::ROOT, "..") >= 0 {
        v |= esc::WALKED_UP;
    }

    // 4. The control: the file inside the grant. A capability that reaches nothing is trivially
    //    unescapable, so without this bit every refusal above proves nothing.
    let own = dir_open(fs::ROOT, tree::INNER);
    if own >= 0 {
        v |= esc::OPENED_ITS_OWN;
        let (n, _) = call(
            FILE,
            fs::req(fs::READ, own as u64, tree::INNER_BODY.len() as u64),
            0,
        );
        get_page(n as usize, &mut buf);
        if (n as i64) < 0
            || n as usize != tree::INNER_BODY.len()
            || &buf[..n as usize] != tree::INNER_BODY
        {
            v |= esc::GRANTED_ACCESS_FAILED;
        }

        // 4b. **Its attributes** (milestone 61). Reading them needs what reading the file needs, so
        //     every configuration that got here should manage it; writing one needs `dir::WRITE`,
        //     and the caretaker checks nothing, so the refusal comes from the FS server acting on
        //     the rights the caretaker's one `OPENDIR` left on this handle.
        //
        //     The attribute is set on `inner` and then removed, so the post-run host check still
        //     sees the file it pins, byte for byte, with nothing attached.
        let (l, _) = call(FILE, fs::req(fs::LISTXATTR, own as u64, 0), 0);
        if (l as i64) < 0 {
            v |= esc::GRANTED_ACCESS_FAILED;
        } else {
            v |= esc::READ_ATTRS;
        }

        put_page(fixture::attrs::NAME);
        put_page_at(fixture::attrs::NAME.len(), fixture::attrs::VALUE);
        let (s, _) = call(
            FILE,
            fs::req(fs::SETXATTR, own as u64, fixture::attrs::NAME.len() as u64),
            xattr::spec(fixture::attrs::KIND, fixture::attrs::VALUE.len() as u64),
        );
        if (s as i64) >= 0 {
            put_page(fixture::attrs::NAME);
            let (g, _) = call(
                FILE,
                fs::req(fs::GETXATTR, own as u64, fixture::attrs::NAME.len() as u64),
                0,
            );
            get_page(xattr::reply_value_len(g as i64), &mut buf);
            if (g as i64) >= 0
                && xattr::reply_kind(g as i64) == fixture::attrs::KIND
                && xattr::reply_value_len(g as i64) == fixture::attrs::VALUE.len()
                && buf[..fixture::attrs::VALUE.len()] == *fixture::attrs::VALUE
            {
                v |= esc::SET_AN_ATTR;
            } else {
                v |= esc::GRANTED_ACCESS_FAILED;
            }
            put_page(fixture::attrs::NAME);
            let (rm, _) = call(
                FILE,
                fs::req(
                    fs::REMOVEXATTR,
                    own as u64,
                    fixture::attrs::NAME.len() as u64,
                ),
                0,
            );
            if (rm as i64) < 0 {
                v |= esc::GRANTED_ACCESS_FAILED;
            }
        }
    }

    // 5. Enumeration, and what it is allowed to contain. A listing is a rendering of authority, so
    //    a name from outside the grant appearing in one is an escape even though nothing was opened.
    let (n, _) = call(FILE, fs::req(fs::READDIR, fs::ROOT, 0), 0);
    if (n as i64) >= 0 {
        v |= esc::ENUMERATED;
        // Clamped to the local buffer: the page is sixteen times larger, and indexing past the
        // buffer with a length the server chose would be a panic this program cannot report.
        let n = (n as usize).min(buf.len());
        get_page(n, &mut buf);
        for (name, _) in fs_proto::dirent::iter(&buf[..n]) {
            for stranger in [
                fixture::MOTD_NAME,
                fixture::SCRATCH_NAME,
                tree::OTHER,
                tree::SECRET,
            ] {
                if name == stranger.as_bytes() {
                    v |= esc::ENUMERATED_A_STRANGER;
                }
            }
        }
    }

    // 6. Making a name inside the grant, and 7. making a directory inside it. Both need CREATE;
    //    `mkdir` needs DESCEND too, because a directory you could not have walked into would be a
    //    way to mint a capability out of a right that was withheld.
    let made = run_name(tree::MADE, run);
    let created = dir_create(fs::ROOT, made_str(&made));
    if created >= 0 {
        v |= esc::CREATED;
        // 8. A write, to what it just made, read straight back: "the server accepted my write" and
        //    "my write landed" are different claims. Never to `inner`, which the host check pins.
        put_page(tree::MADE_BODY);
        let (w, _) = call(
            FILE,
            fs::req(fs::WRITE, created as u64, tree::MADE_BODY.len() as u64),
            0,
        );
        if (w as i64) >= 0 {
            v |= esc::WROTE;
            let (rb, _) = call(
                FILE,
                fs::req(fs::READ, created as u64, tree::MADE_BODY.len() as u64),
                0,
            );
            get_page(tree::MADE_BODY.len(), &mut buf);
            if rb as usize != tree::MADE_BODY.len()
                || &buf[..tree::MADE_BODY.len()] != tree::MADE_BODY
            {
                v |= esc::GRANTED_ACCESS_FAILED;
            }
        }
    }
    let made_dir = run_name(tree::MADE_DIR, run);
    if dir_mkdir(fs::ROOT, made_str(&made_dir), dir::READ) >= 0 {
        v |= esc::MADE_A_DIR;
    }

    // 9. Descending inside the grant, which is allowed exactly when DESCEND was granted. Asking for
    //    DESCEND alone, so this probe is about reaching the child rather than about its rights.
    let child = dir_descend(fs::ROOT, tree::DEEPER, dir::DESCEND);
    if child >= 0 {
        v |= esc::DESCENDED;
    }

    // 10. **Widening, the question this milestone exists to answer**, probed two ways.
    //
    //   (a) A child asked for NOTHING must be able to do nothing. If a zero-rights directory
    //       capability opens, lists or creates, rights are not being carried on the handle at all.
    let nothing = dir_descend(fs::ROOT, tree::DEEPER, 0);
    if nothing >= 0 {
        let listed = call(FILE, fs::req(fs::READDIR, nothing as u64, 0), 0).0 as i64;
        if dir_open(nothing as u64, tree::LEAF) >= 0
            || dir_create(nothing as u64, "smuggled") >= 0
            || listed >= 0
        {
            v |= esc::WIDENED;
        }
    }
    //   (b) A child cannot carry what the parent did not. If creating in the grant was refused, the
    //       grant has no CREATE, so a descent that asks for CREATE must be refused too.
    if created < 0 && dir_descend(fs::ROOT, tree::DEEPER, dir::ALL) >= 0 {
        v |= esc::WIDENED;
    }

    // 11. **Renaming, which is the only verb on this wire that consults `REMOVE`.** It moves the
    //     name it made itself, never `inner`, so a run that is allowed to do this damages only its
    //     own leavings and the post-run host check can still pin the fixture. The attempt is made
    //     whatever the grant is: the server checks the rights before it resolves anything, so a
    //     capability with no `REMOVE` gets the same refusal whether or not the source is there,
    //     and this program is not told which case it is in.
    let moved = run_name(tree::MOVED, run);
    if dir_rename(fs::ROOT, made_str(&made), fs::ROOT, made_str(&moved)) >= 0 {
        v |= esc::RENAMED;
    }

    // 12. Handle guessing, past anything the caretaker could have minted for it. The caretaker
    //     numbers its client's handles in its own space, so a number it never issued is refused by
    //     one check.
    let mut guess = 9u64;
    while guess < 16 {
        if call(FILE, fs::req(fs::READ, guess, 8), 0).0 as i64 >= 0
            || call(FILE, fs::req(fs::READDIR, guess, 0), 0).0 as i64 >= 0
        {
            v |= esc::FORGED_HANDLE;
        }
        guess += 1;
    }

    // Nothing at all worked: report it, so a capability that reaches nothing cannot pass as a
    // capability that is perfectly confined.
    if v & (esc::OPENED_ITS_OWN | esc::ENUMERATED | esc::DESCENDED | esc::CREATED) == 0 {
        v |= esc::GRANTED_ACCESS_FAILED;
    }

    send(REPORT, fixture::VERDICT, v, 0);
    exit();
}

/// **The attribute witness behind a name-set grant** (milestone 61, the third caretaker).
///
/// `fs_nameset_caretaker` narrows a directory capability to a designated *set* of names, and it is
/// the one caretaker that inspects a name on every request. Milestone 61 taught it four verbs whose
/// operand is an **attribute** name rather than a directory name, and that distinction is the whole
/// risk: filtering an attribute name against the set would refuse a program its own file's
/// attributes, and *not* filtering a directory name would be an escape. This asks both questions.
///
/// It is told nothing about its grant. The kernel test wires it over the `sub` fixture with a set of
/// exactly one name, so `inner` is designated and `deeper`, one entry away in the same directory, is
/// not. `rm`'s witness already asks the naming question with `REMOVE` alone; this asks it with
/// `READ`, because closing an attribute gap by opening a naming one would be a poor trade.
///
/// The attribute is removed before it reports, so the file the gate's post-run host check pins is
/// left byte for byte as it was found, with nothing attached.
fn set_attrs() -> ! {
    use fixture::{dirscape as esc, tree};
    let mut v = 0u64;
    let mut buf = [0u8; 128];

    // The name the set designates. Everything below hangs off this, so its failure is the control.
    let own = dir_open(fs::ROOT, tree::INNER);
    if own < 0 {
        v |= esc::GRANTED_ACCESS_FAILED;
    } else {
        v |= esc::OPENED_ITS_OWN;

        // A listing: a read, so it needs what reading the file needs and nothing more. Zero
        // attributes is a successful listing, which is what tells "forwarded and there were none"
        // apart from "the caretaker refused it".
        let (l, _) = call(FILE, fs::req(fs::LISTXATTR, own as u64, 0), 0);
        if (l as i64) < 0 {
            v |= esc::GRANTED_ACCESS_FAILED;
        } else {
            v |= esc::READ_ATTRS;
        }

        // A set, read straight back with its type code, then removed. The attribute's name is not a
        // name in the directory, so a caretaker that filtered it against the set would refuse this
        // and the bit would stay clear.
        put_page(fixture::attrs::NAME);
        put_page_at(fixture::attrs::NAME.len(), fixture::attrs::VALUE);
        let (s, _) = call(
            FILE,
            fs::req(fs::SETXATTR, own as u64, fixture::attrs::NAME.len() as u64),
            xattr::spec(fixture::attrs::KIND, fixture::attrs::VALUE.len() as u64),
        );
        if (s as i64) >= 0 {
            put_page(fixture::attrs::NAME);
            let (g, _) = call(
                FILE,
                fs::req(fs::GETXATTR, own as u64, fixture::attrs::NAME.len() as u64),
                0,
            );
            get_page(xattr::reply_value_len(g as i64), &mut buf);
            if (g as i64) >= 0
                && xattr::reply_kind(g as i64) == fixture::attrs::KIND
                && xattr::reply_value_len(g as i64) == fixture::attrs::VALUE.len()
                && buf[..fixture::attrs::VALUE.len()] == *fixture::attrs::VALUE
            {
                v |= esc::SET_AN_ATTR;
            } else {
                v |= esc::GRANTED_ACCESS_FAILED;
            }
            put_page(fixture::attrs::NAME);
            let (rm, _) = call(
                FILE,
                fs::req(
                    fs::REMOVEXATTR,
                    own as u64,
                    fixture::attrs::NAME.len() as u64,
                ),
                0,
            );
            if (rm as i64) < 0 {
                v |= esc::GRANTED_ACCESS_FAILED;
            }
        } else {
            v |= esc::GRANTED_ACCESS_FAILED;
        }
    }

    // The naming question, by both routes: a file the set does not carry, and a directory it does
    // not carry. Both are really in the granted directory and the caretaker one hop up could open
    // either on any request it liked, so each refusal is a fact about the set.
    if dir_open(fs::ROOT, tree::DEEPER) >= 0 || dir_descend(fs::ROOT, tree::DEEPER, dir::READ) >= 0
    {
        v |= esc::REACHED_AN_UNMATCHED_NAME;
    }

    send(REPORT, fixture::VERDICT, v, 0);
    exit();
}

/// A name with the run index appended, so three attacker runs sharing one image do not collide on
/// `EEXIST` and read it as a refusal. Fixed-size because this program has no allocator.
fn run_name(base: &str, run: u64) -> ([u8; 16], usize) {
    let mut out = [0u8; 16];
    let n = base.len().min(15);
    out[..n].copy_from_slice(&base.as_bytes()[..n]);
    out[n] = b'0' + (run % 10) as u8;
    (out, n + 1)
}

/// The `&str` inside a [`run_name`]. Its bytes are ASCII by construction, so the conversion cannot
/// fail; `unwrap_or` keeps the panic-free discipline anyway.
fn made_str(name: &([u8; 16], usize)) -> &str {
    core::str::from_utf8(&name.0[..name.1]).unwrap_or("made")
}

/// `OPEN` a name under a directory handle; the raw reply, so a refusal is a value rather than a trap.
fn dir_open(parent: u64, name: &str) -> i64 {
    put_page(name.as_bytes());
    call(FILE, fs::req(fs::OPEN, parent, name.len() as u64), 0).0 as i64
}

/// `CREATE` a name under a directory handle.
fn dir_create(parent: u64, name: &str) -> i64 {
    put_page(name.as_bytes());
    call(FILE, fs::req(fs::CREATE, parent, name.len() as u64), 0).0 as i64
}

/// `OPENDIR`: descend, asking for `rights`. The requested rights ride in the second word.
fn dir_descend(parent: u64, name: &str, rights: u64) -> i64 {
    put_page(name.as_bytes());
    call(
        FILE,
        fs::req(fs::OPENDIR, parent, name.len() as u64),
        rights,
    )
    .0 as i64
}

/// `MKDIR`: the same shape as [`dir_descend`], with creation.
fn dir_mkdir(parent: u64, name: &str, rights: u64) -> i64 {
    put_page(name.as_bytes());
    call(FILE, fs::req(fs::MKDIR, parent, name.len() as u64), rights).0 as i64
}

/// `UNLINK` a name under a directory handle; the raw reply, so a name that was not there is a value
/// rather than a trap. The witness uses it to clean up after itself and to provoke a node's death.
fn dir_unlink(parent: u64, name: &str) -> i64 {
    put_page(name.as_bytes());
    call(FILE, fs::req(fs::UNLINK, parent, name.len() as u64), 0).0 as i64
}

/// `RENAME`: two directories and two names, so both names go into the page back to back (source
/// first) and the destination's handle and length ride in the second word.
fn dir_rename(src_dir: u64, src: &str, dst_dir: u64, dst: &str) -> i64 {
    put_page(src.as_bytes());
    put_page_at(src.len(), dst.as_bytes());
    call(
        FILE,
        fs::req(fs::RENAME, src_dir, src.len() as u64),
        fs::rename_dst(dst_dir, dst.len() as u64),
    )
    .0 as i64
}

// --- The extended-attribute witness (milestone 57) ---

/// Fill `len` bytes of the shared page at `off` with `b`. The oversize probe writes a value larger
/// than any local this program could afford, so it goes straight to the page.
fn fill_page(off: usize, len: usize, b: u8) {
    for i in 0..len {
        // SAFETY: FILE_VA is a mapped, writable 4096-byte page and the caller keeps within it.
        unsafe { core::ptr::write_volatile((FILE_VA + (off + i) as u64) as *mut u8, b) };
    }
}

/// `SETXATTR`: the only verb here with two payloads, so the name and the value go into the page back
/// to back and the value's length and type code ride packed in the second word.
fn xattr_set(handle: u64, name: &[u8], kind: u32, value: &[u8]) -> i64 {
    put_page(name);
    put_page_at(name.len(), value);
    call(
        FILE,
        fs::req(fs::SETXATTR, handle, name.len() as u64),
        xattr::spec(kind, value.len() as u64),
    )
    .0 as i64
}

/// `GETXATTR`: the name goes out on the page and the value comes back on it, so the reply's packed
/// kind and length are all this returns; the bytes are read with [`get_page`].
fn xattr_get(handle: u64, name: &[u8]) -> i64 {
    put_page(name);
    call(FILE, fs::req(fs::GETXATTR, handle, name.len() as u64), 0).0 as i64
}

/// `LISTXATTR`: no name, no cursor. The reply is how many bytes of records landed in the page.
fn xattr_list(handle: u64) -> i64 {
    call(FILE, fs::req(fs::LISTXATTR, handle, 0), 0).0 as i64
}

/// `REMOVEXATTR`: [`xattr_get`]'s shape, and the reply is 0 or an error.
fn xattr_remove(handle: u64, name: &[u8]) -> i64 {
    put_page(name);
    call(FILE, fs::req(fs::REMOVEXATTR, handle, name.len() as u64), 0).0 as i64
}

/// **The extended-attribute witness** (milestone 57): everything the layer claims, attempted against
/// the real FS server over the real contract, reported as a bitmap the kernel test asserts exactly.
///
/// It is a bitmap rather than a chain of `check`s for the reason every other witness in this file is
/// one: a trapped client tells the waiting test only that something went wrong, and the interesting
/// failure here is *which* claim broke. A layer that dropped type codes, a store keyed on a path
/// instead of a node, and a purge that never ran each fail a different bit.
///
/// It cleans up after itself. Its attribute is removed from `scratch` and the two files it makes are
/// unlinked, so the image it leaves behind differs from the one it found by the store directory
/// alone, which the host tool is expected to show (notes/host-recovery.md).
fn attr_witness(scratch: u64) -> u64 {
    use fixture::attrs;
    let mut v = 0u64;
    let mut buf = [0u8; 512];

    // 1. Set an attribute and read it back: its bytes AND its type code. The kind is the half that
    //    catches a layer which stored the value and threw the type away, which is the thing that
    //    would foreclose indexing later without anyone noticing.
    if xattr_set(scratch, attrs::NAME, attrs::KIND, attrs::VALUE) >= 0 {
        let r = xattr_get(scratch, attrs::NAME);
        if r >= 0
            && xattr::reply_kind(r) == attrs::KIND
            && xattr::reply_value_len(r) == attrs::VALUE.len()
        {
            get_page(attrs::VALUE.len(), &mut buf);
            if &buf[..attrs::VALUE.len()] == attrs::VALUE {
                v |= attrs::SET_AND_READ_BACK;
            }
        }
    } else {
        v |= attrs::ATTRS_FAILED;
    }

    // 2. The listing names it, and names nothing else. A store keyed on the wrong node would show up
    //    here as somebody else's attributes rather than as a missing one.
    let n = xattr_list(scratch);
    if n > 0 {
        get_page(n as usize, &mut buf);
        let mut count = 0;
        let mut saw = false;
        for name in xattr::list::iter(&buf[..(n as usize).min(buf.len())]) {
            count += 1;
            if name == attrs::NAME {
                saw = true;
            }
        }
        if saw && count == 1 {
            v |= attrs::LISTED;
        }
    }

    // 3. A value past the ceiling is refused with its own word, not stored short. The probe is wider
    //    than any local here, so it is written straight onto the page.
    let big = b"user.toobig";
    put_page(big);
    fill_page(big.len(), xattr::MAX_VALUE + 1, b'Z');
    let r = call(
        FILE,
        fs::req(fs::SETXATTR, scratch, big.len() as u64),
        xattr::spec(xattr::RAW, (xattr::MAX_VALUE + 1) as u64),
    )
    .0 as i64;
    if fs_proto::reply_errno(r) == Some(xattr::E2BIG) {
        v |= attrs::OVERSIZE_REFUSED;
    }

    // 4. Removing it takes it away, and reading it afterwards is ENODATA. Without this the round
    //    trip above is equally true of a store that never forgets anything.
    if xattr_remove(scratch, attrs::NAME) >= 0
        && fs_proto::reply_errno(xattr_get(scratch, attrs::NAME)) == Some(xattr::ENODATA)
    {
        v |= attrs::GONE_AFTER_REMOVE;
    }

    // 5. **The headline: an attribute follows its file across a rename.** The two names are unlinked
    //    first, because `NIFE_KEEP_REDOXFS` makes a second boot meet the first boot's leftovers
    //    and an `EEXIST` here would read as a broken layer.
    let _ = dir_unlink(fs::ROOT, attrs::PROBE);
    let _ = dir_unlink(fs::ROOT, attrs::PROBE_MOVED);
    let probe = dir_create(fs::ROOT, attrs::PROBE);
    if probe >= 0
        && xattr_set(probe as u64, attrs::NAME, attrs::KIND, attrs::VALUE) >= 0
        && dir_rename(fs::ROOT, attrs::PROBE, fs::ROOT, attrs::PROBE_MOVED) >= 0
    {
        // Opened at its NEW name, through a handle minted after the rename, so nothing carried over
        // from the handle that set it.
        let moved = dir_open(fs::ROOT, attrs::PROBE_MOVED);
        let r = if moved >= 0 {
            xattr_get(moved as u64, attrs::NAME)
        } else {
            -1
        };
        if r >= 0 && xattr::reply_value_len(r) == attrs::VALUE.len() {
            get_page(attrs::VALUE.len(), &mut buf);
            if &buf[..attrs::VALUE.len()] == attrs::VALUE {
                v |= attrs::SURVIVED_RENAME;
            }
        }
    } else {
        v |= attrs::ATTRS_FAILED;
    }

    // 6. And the file's death takes its attributes with it: unlink the name, make it again, and the
    //    new file has nothing on it. The node id is very likely reused, which is the case that makes
    //    a leaked blob a correctness bug rather than wasted space.
    if dir_unlink(fs::ROOT, attrs::PROBE_MOVED) >= 0 {
        let fresh = dir_create(fs::ROOT, attrs::PROBE_MOVED);
        if fresh >= 0 && xattr_list(fresh as u64) == 0 {
            v |= attrs::GONE_AFTER_UNLINK;
        }
        let _ = dir_unlink(fs::ROOT, attrs::PROBE_MOVED);
    }

    // 7. The store is not in the namespace: it cannot be opened, created, or descended into.
    if dir_open(fs::ROOT, xattr::STORE_DIR) < 0
        && dir_create(fs::ROOT, xattr::STORE_DIR) < 0
        && dir_descend(fs::ROOT, xattr::STORE_DIR, dir::ALL) < 0
    {
        v |= attrs::STORE_UNNAMEABLE;
    }

    // 8. And a full enumeration does not name it. Driven to the end with a cursor, because a listing
    //    that stopped early would be a clean report about the part it happened to read.
    let (mut cursor, mut entries, mut clean, mut finished) = (0u64, 0u64, true, false);
    for _ in 0..64 {
        let n = call(FILE, fs::req(fs::READDIR, fs::ROOT, 0), cursor).0 as i64;
        if n <= 0 {
            finished = n == 0;
            break;
        }
        get_page(n as usize, &mut buf);
        let mut seen = 0u64;
        for (name, _) in fs_proto::dirent::iter(&buf[..(n as usize).min(buf.len())]) {
            if name == xattr::STORE_DIR.as_bytes() {
                clean = false;
            }
            seen += 1;
        }
        if seen == 0 {
            break; // a cursor that cannot advance is a failure, not a clean listing
        }
        cursor += seen;
        entries += seen;
    }
    if clean && finished && entries > 0 {
        v |= attrs::STORE_UNLISTED;
    }

    v
}

/// How many bytes the attacker's write probe carries. A fixed length rather than the file's, because
/// the file's length is history this program did not write (see the control's note).
const PROBE_LEN: usize = 32;

/// A recognisable payload for the write probe: if a read-only grant ever let one through, the bytes
/// left on the disk say who put them there.
fn probe_payload() -> [u8; PROBE_LEN] {
    let mut p = [b'!'; PROBE_LEN];
    p[..16].copy_from_slice(b"CLOBBERED-BY-ATK");
    p
}

/// **The FS-server read benchmark** (the userspace-server tax, DECISIONS §32). Open `motd` once, then
/// time a loop of reads of its first block and report `[ticks, iters]` on the report endpoint, the
/// `os_primitives_benchmarker` shape. The kernel's bench boot prints it as `fs_read`; against the bare `ipc_rtt_el0`
/// round trip, the difference is what the FS-server contract costs above a raw endpoint call. It is a
/// `--real`-only magnitude (the mount is device-driven, not deterministic under icount); see
/// notes/benchmarks.md and kernel/src/bench.rs.
fn bench() -> ! {
    let handle = open(fixture::MOTD_NAME);
    let block = fixture::MOTD.len();
    for _ in 0..BENCH_WARMUP {
        let _ = read(handle, 0, block);
    }
    let start = now();
    for _ in 0..BENCH_ITERS {
        let _ = read(handle, 0, block);
    }
    let ticks = now().wrapping_sub(start);
    // slot 1 is REPORT; carry ticks and the iteration count, the bench line's two fields.
    send(REPORT, ticks, BENCH_ITERS, 0);
    exit();
}

fn proof() -> ! {
    // Read the motd the image ships with, through a handle the server minted for us. This proves
    // the whole read path end to end: a real RedoxFS image we did not write, mounted by a confined
    // FS server over blk IPC, its files opened by name under a granted directory capability and read
    // by a client that names nothing else in the system.
    //
    let motd = open(fixture::MOTD_NAME);
    let n = read(motd, 0, fixture::MOTD.len());
    check(n == fixture::MOTD.len());
    let mut buf = [0u8; 128];
    get_page(n, &mut buf);
    check(&buf[..n] == fixture::MOTD);
    let mut head = [0u8; 8];
    head.copy_from_slice(&buf[..8]);

    // **Repeat writes, in one run.** A first write to a pristine block always worked; a write to a
    // block the image already carries is the case that loops, and the gate could not see it because
    // `mkredoxfs` rewrites the target to a placeholder before every run, making every gated write a
    // first write. Writing the same block three times here depends on nothing left over from a
    // previous invocation, so the bug cannot hide behind the harness again.
    let scratch = open(fixture::SCRATCH_NAME);
    let mut pass = 1u8;
    while pass <= 3 {
        let payload = repeat_payload(pass);
        write(scratch, 0, &payload);
        let m = read(scratch, 0, payload.len());
        check(m == payload.len());
        get_page(m, &mut buf);
        check(buf[..m] == payload[..]);
        pass += 1;
    }

    // Leave the fixture pattern behind as the LAST write, so the gate's post-run host-tool check
    // (`redoxfs_check_after_run`, which reopens the image with the pinned engine and compares
    // `scratch` against `WRITE_PATTERN`) still validates a documented value. Every write above is the
    // same length as this one, so this replaces them completely.
    write(scratch, 0, fixture::WRITE_PATTERN);
    let f = read(scratch, 0, fixture::WRITE_PATTERN.len());
    check(f == fixture::WRITE_PATTERN.len());
    get_page(f, &mut buf);
    check(&buf[..f] == fixture::WRITE_PATTERN);

    // **Extended attributes** (milestone 57), on the same server over the same endpoint, so the
    // whole three-process stack does not have to be booted a second time to prove a fourth verb
    // family. It runs after the fixture restore above because attributes do not touch a file's
    // contents, so the gate's post-run byte comparison is unaffected either way and this ordering
    // keeps the restore the last thing that writes bytes.
    let attrs = attr_witness(scratch);

    // Report the motd head, the success sentinel, and the attribute witness's bitmap; the kernel
    // asserts all three, and the third against an exact expected set.
    send(REPORT, u64::from_le_bytes(head), fixture::SUCCESS, attrs);
    exit();
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // A failed check is a dead client: fault, and the kernel reports it, failing the test.
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `brk` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem));
    };
    #[cfg(target_arch = "riscv64")]
    // SAFETY: `ebreak` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
