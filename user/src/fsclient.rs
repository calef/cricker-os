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

#![no_std]
#![no_main]

use fs_proto::{fixture, fs, grant};
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
/// twin of `fs-server`'s host-side `repeat_write_payload`.
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
/// reply), warm, not disk latency. Self-timed, reported home like elbench's primitives.
const BENCH_ITERS: u64 = 2000;
const BENCH_WARMUP: u64 = 64;

/// Roles, chosen by `arg0` at START (mirrors the kernel side). 0 is the end-to-end proof the test
/// spawns; 1 is the benchmark loop the `--real --smp` bench spawns; 2 is the attacker that a
/// milestone-31 per-file grant is measured against. One binary, three entries.
const ROLE_PROOF: u64 = 0;
const ROLE_BENCH: u64 = 1;
const ROLE_ATTACKER: u64 = 2;
/// Milestone 37: drive the writes the FS server is killed in the middle of.
const ROLE_CRASH_DRIVER: u64 = 3;
/// Milestone 37: read the file back through the FS server that mounted the crashed disk.
const ROLE_CRASH_VERIFY: u64 = 4;

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, a1: u64, _a2: u64) -> ! {
    match role {
        ROLE_BENCH => bench(),
        ROLE_ATTACKER => attacker(a1 != 0),
        ROLE_CRASH_DRIVER => crash_driver(),
        ROLE_CRASH_VERIFY => crash_verify(),
        ROLE_PROOF => proof(),
        _ => proof(),
    }
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
/// endpoint: not the directory capability, but the file warden's, which designates exactly one file
/// read-only. Its job is to try everything that would make that sentence false, and to report which
/// attempts got through as a bitmap (`fixture::escape`).
///
/// **It is its own negative control, which is why it reports a bitmap and not a pass.** Run against a
/// read-only grant, every bit must be clear. Run against a read/write grant of the same shape,
/// `WROTE` and `TRUNCATED` must be **set** and everything else clear. Without that second run the
/// first proves very little: a warden that refused every request would pass it, and so would a grant
/// that reached nothing at all.
///
/// **What makes the attempts real.** Each one is against something that exists and that the process
/// one hop up the chain can genuinely reach: the neighbour file is on the image, one directory entry
/// away, and the warden could open it on any request it liked. Milestone 33's attacker was handed a
/// real neighbouring client's address rather than a fictional one for exactly this reason, and
/// milestone 36 used two witnesses for the reason the paragraph above gives.
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

    // 1. A second file, by name. It exists, it sits in the same directory, and the warden could open
    //    it. This capability names one file, so this must find nothing.
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

    // 5. Handle guessing. The warden minted one handle and the FS server's own handle for the file is
    //    a different number this process never saw, so spraying numbers is probing a table it is not
    //    addressing. Every miss must be refused by the same check.
    let mut guess = 1u64;
    while guess < 8 {
        let (g, _) = call(FILE, fs::req(fs::READ, guess, 8), 0);
        if (g as i64) >= 0 {
            verdict |= escape::FORGED_HANDLE;
        }
        guess += 1;
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
/// elbench shape. The kernel's bench boot prints it as `fs_read`; against the bare `ipc_rtt_el0`
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

    // Report the motd head plus the success sentinel; the kernel asserts both.
    send(REPORT, u64::from_le_bytes(head), fixture::SUCCESS, 0);
    exit();
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // A failed check is a dead client: fault, and the kernel reports it, failing the test.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem))
    };
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
