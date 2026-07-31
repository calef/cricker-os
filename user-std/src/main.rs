//! The std proof (milestone 27): an ordinary Rust program, no `no_std`, running on the native
//! capability ABI. Every line exercises a PAL surface: `println!` SENDs on the stdout endpoint
//! (slot 1), collections draw from the untyped budget (slot 0), `Instant` reads the virtual
//! counter, `SystemTime` reads the clock page slot 5 grants (milestone 51), `std::random` asks the
//! entropy service slot 6 grants (milestone 56), and `fs` returns honestly `Unsupported`.
//!
//! The one `#![feature]` below is about **the API's stability upstream, not about this platform**:
//! `std::random` is still unstable in Rust (rust-lang/rust#130703), so any program on any target
//! that calls it opts in the same way. Everything else here is stable Rust.
#![feature(random)]
//!
//! **One binary, three behaviours, chosen by the authority it was granted.** A std program reaches
//! the network only if it holds the network, and the filesystem only if it holds a directory (no
//! ambient authority, DECISIONS §10, §25, §27). So it probes, and its grants decide:
//!   - **granted a directory** (the loader placed an FS-service endpoint in slot 4 and mapped the
//!     page it shares with the FS server): it opens the file the RedoxFS image ships, reads it
//!     through `std::fs`, and proves that a path trying to leave the granted directory is refused.
//!   - **granted the network** (a `Stack` endpoint and a frame untyped in slots 2 and 3): it runs a
//!     real UDP DNS query and a TCP echo round trip through `std::net` (milestone 27 phase two),
//!     the same netstack socket contract the hand-written client uses.
//!   - **granted neither** (only the heap and stdout slots): both return `Unsupported`, and the
//!     program runs the phase-one transcript, proving the collections, timing, and the honest
//!     refusals.
//!
//! One binary keeps the initrd inside its crickerfs directory limit (`MAX_FILES`, 31 entries) while
//! still proving all three. The kernel test suite spawns it three ways and checks each transcript
//! byte for byte, on both ISAs.

use std::collections::HashMap;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::random::{Rng, SystemRng};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn main() {
    // Probe for a directory capability first: an `Unsupported` open means no FS-service endpoint in
    // slot 4, i.e. this process holds no directory and there is no ambient filesystem to fall back
    // on. Anything else means the filesystem IS granted, so a failure to open the file the image
    // ships is a real failure and must not be silently swallowed by falling through to the net.
    match File::open(fs_proto::fixture::MOTD_NAME) {
        Ok(f) => return fs_demo(f),
        Err(e) if e.kind() == ErrorKind::Unsupported => {}
        Err(e) => panic!("a directory capability was granted but the motd would not open: {e:?}"),
    }

    // Probe for the network by trying to open a UDP socket. A program not granted the `Stack`
    // endpoint and a frame untyped gets `Unsupported` here (the RETYPE of a shared frame fails
    // with no untyped in slot 3), which is the signal to run the offline transcript instead.
    match UdpSocket::bind("0.0.0.0:0") {
        Ok(sock) => net_demo(sock),
        Err(_) => offline_demo(),
    }
}

/// The phase-one transcript: collections on the untyped heap, `Instant`, and the honest refusal of
/// `fs` and `net`. Runs when the program was not granted the network.
fn offline_demo() {
    let t0 = Instant::now();

    println!("hello from std on cricker-os");
    println!("os {}", std::env::consts::OS);

    // Vec: growth reallocations against the untyped-backed heap.
    let v: Vec<u64> = (0..10_000).map(|i| i * 3).collect();
    let sum: u64 = v.iter().sum();
    println!("vec sum {sum}");

    // String: heap bytes whose length the receiver checks.
    let mut s = String::new();
    for _ in 0..100 {
        s.push_str("cricker ");
    }
    println!("string len {}", s.len());

    // HashMap: exercises the platform RandomState seed (sys/random) plus many small allocations.
    let mut m = HashMap::new();
    for k in 0u64..100 {
        m.insert(k, k * k);
    }
    println!("map lookup {}", m[&37]);

    // The honesty checks: the platform must refuse, not pretend.
    match std::fs::File::open("/init") {
        Err(e) if e.kind() == ErrorKind::Unsupported => println!("fs honestly unsupported"),
        other => println!("fs lied: {other:?}"),
    }
    match TcpStream::connect("127.0.0.1:80") {
        Err(e) if e.kind() == ErrorKind::Unsupported => println!("net honestly unsupported"),
        other => println!("net lied: {other:?}"),
    }

    // Instant: monotonic and advancing, but asserted rather than printed (a printed duration
    // would make the transcript nondeterministic).
    let t1 = Instant::now();
    assert!(t1 >= t0, "the virtual counter went backwards");
    assert!(
        t1.duration_since(t0).as_nanos() > 0,
        "no time passed across real work"
    );
    println!("instant monotonic ok");

    // **Wall-clock time** (milestone 51). This process was granted a clock: a `Frame` capability
    // naming the clock service's page in slot 5 and a read-only mapping of it, which is all
    // `SystemTime::now()` needs (the offset from the page, the monotonic counter from the ambient
    // register). Asserted rather than printed, because a real date is not a deterministic
    // transcript; what is checked is that it is inside the same sanity window the clock service
    // applies, which 1970-plus-uptime is not.
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the wall clock is before the Unix epoch");
    assert!(
        wall >= NOT_BEFORE && wall < NOT_AFTER,
        "the wall clock reads {wall:?}, outside the plausible window",
    );
    // And the property the counter-plus-offset design buys: `SystemTime` and `Instant` are read
    // from the same counter, so the wall clock advancing does not mean the monotonic one jumped.
    let t2 = Instant::now();
    assert!(t2 >= t1, "the monotonic counter went backwards");
    println!("wall clock ok");

    // **Real entropy** (milestone 56). This process was granted one endpoint that means "you may
    // obtain randomness"; it names no device, and the entropy service on the other end is the only
    // thing that can read the virtio-rng. Asserted rather than printed, because random bytes are
    // the least deterministic transcript imaginable. Two 32-byte draws agreeing is a 2^-256 event
    // with a real source and a certainty with the counter-seeded stream this replaced, so the
    // comparison is what says the bytes came off a device.
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    SystemRng.fill_bytes(&mut a);
    SystemRng.fill_bytes(&mut b);
    assert_ne!(a, b, "two draws from std::random are identical");
    assert!(a.iter().any(|&x| x != 0), "a draw is all zeros");
    println!("entropy ok");
}

/// The same sanity window `clock_proto::policy` applies, restated here because a std program links
/// std and not the contract crate. 2026-01-01 and 2100-01-01.
const NOT_BEFORE: Duration = Duration::from_secs(1_767_225_600);
const NOT_AFTER: Duration = Duration::from_secs(4_102_444_800);

/// **The `std::fs` transcript** (milestone 27 phase two, the FS half): ordinary `File`, `Read`,
/// `read_to_string`, and `metadata`, all served by the RedoxFS FS server over the §27 contract, and
/// all reached through the one directory capability this process was granted.
///
/// The interesting half is the refusals. `File::open` takes a path, but this system has no global
/// namespace: a name means "under the directory I hold", so `/etc/passwd`, `../motd`, and
/// `sub/motd` are not things this process can express, and each is refused *before* a byte reaches
/// the server. The refusal is `InvalidFilename` (there is no such name here), never
/// `PermissionDenied`, because nothing checked a permission. A name that IS expressible but absent
/// is an ordinary `NotFound`, which is what makes the difference legible.
///
/// `motd` is already open when this runs: opening it was the probe that chose this branch.
fn fs_demo(mut motd: File) {
    println!("std fs on cricker-os");

    // Bytes off a real RedoxFS image, through a confined FS server, reached with `Read` on an
    // ordinary `File`. Printed as well as asserted, so the kernel test compares the file's contents
    // byte for byte after they have crossed the whole stack.
    let mut bytes = Vec::new();
    motd.read_to_end(&mut bytes)
        .expect("reading the motd through std::fs failed");
    assert_eq!(
        bytes,
        fs_proto::fixture::MOTD,
        "std::fs read the wrong bytes off the image"
    );
    print!(
        "{}",
        String::from_utf8(bytes).expect("the motd is UTF-8 and ends in a newline")
    );
    drop(motd); // Drop CLOSEs the handle the server minted for us.

    // read_to_string reopens the same name and leans on the size hint the PAL answers with FSTAT.
    let text = std::fs::read_to_string(fs_proto::fixture::MOTD_NAME)
        .expect("read_to_string through std::fs failed");
    println!("read_to_string {}", text.len());

    let meta =
        std::fs::metadata(fs_proto::fixture::MOTD_NAME).expect("metadata through std::fs failed");
    assert!(meta.is_file(), "the motd is a regular file");
    println!("metadata len {}", meta.len());

    refused("/etc/passwd", "absolute");
    refused("../motd", "dotdot");
    refused("sub/motd", "nested");

    match File::open("definitely-not-here") {
        Err(e) if e.kind() == ErrorKind::NotFound => println!("missing not found"),
        other => panic!("a missing name did not read as NotFound: {other:?}"),
    }

    // **`std::fs::write` works now** (milestone 31 phase 2), and this is the assertion the CREATE and
    // TRUNCATE verbs exist for. It creates a name the image does not carry, so it exercises CREATE,
    // and it is deliberately written TWICE with a SHORTER payload the second time, which is the case
    // that used to be impossible to get right: without TRUNCATE the second write would leave the tail
    // of the first behind and the read-back would come up long. §27's four corrections all trace to
    // that one behaviour, so it is pinned here at the top level rather than only in a host test.
    let long = b"the first write, deliberately the longer of the two";
    let short = b"the second write, shorter";
    assert!(short.len() < long.len(), "the shorter write must be shorter");

    std::fs::write("made-by-std", long).expect("fs::write could not create a file");
    std::fs::write("made-by-std", short).expect("fs::write could not rewrite a file");
    let back = std::fs::read("made-by-std").expect("reading back what fs::write wrote failed");
    assert_eq!(
        back, short,
        "a shorter fs::write must REPLACE the contents, not leave the old tail",
    );
    println!("write create ok");

    // `create_new` on a name that now exists is AlreadyExists, not Unsupported and not a silent
    // overwrite. That distinction is the reason CREATE refuses rather than opening.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open("made-by-std")
    {
        Err(e) if e.kind() == ErrorKind::AlreadyExists => println!("create_new refused"),
        other => panic!("create_new over an existing name did not refuse: {other:?}"),
    }

    // And a created name is still bound by the directory capability: creating outside it is refused
    // the same way opening outside it is, so CREATE did not widen what a client can reach.
    for (path, what) in [("/tmp/escape", "absolute"), ("../escape", "dotdot")] {
        match std::fs::write(path, b"x") {
            Err(e) if e.kind() != ErrorKind::PermissionDenied => {
                println!("create refused {what}")
            }
            other => panic!("creating an un-nameable path was not refused: {other:?}"),
        }
    }

    // **The on-device write path, and a correction to the record.** notes/fs-server.md recorded
    // that an end-to-end write looped inside RedoxFS's allocator commit on bare metal, so the
    // milestone-32 client's test stayed read-only. With interrupt-driven block completion restored
    // it completes: this writes over a file the image ships (there is no create verb, so `scratch`
    // must already exist), reads it back, and the host tool re-reads the image after the run to
    // prove the bytes reached the disk rather than a cache.
    let mut scratch = std::fs::OpenOptions::new()
        .write(true)
        .open(fs_proto::fixture::SCRATCH_NAME)
        .expect("opening scratch for writing through std::fs failed");
    scratch
        .write_all(fs_proto::fixture::WRITE_PATTERN)
        .expect("writing scratch through std::fs failed");
    drop(scratch);
    let back = std::fs::read(fs_proto::fixture::SCRATCH_NAME)
        .expect("reading scratch back through std::fs failed");
    assert_eq!(
        back,
        fs_proto::fixture::WRITE_PATTERN,
        "the write did not read back"
    );
    println!("write readback ok");

    println!("fs ok");
}

/// Assert that a path is refused as un-nameable, and say which case it was.
fn refused(path: &str, label: &str) {
    match File::open(path) {
        Err(e) if e.kind() == ErrorKind::InvalidFilename => println!("{label} refused"),
        other => panic!("{path} was not refused as un-nameable: {other:?}"),
    }
}

/// The guestfwd echo peer the test runners attach. Both of this program's network fixtures now live
/// inside libslirp (this one and the TFTP server `udp_ok` uses), so the transcript is offline and
/// deterministic: nothing it depends on can be dropped by somebody else's router.
const ECHO_PEER: &str = "10.0.2.9:7777";

/// The networked transcript (milestone 27 phase two): a UDP round trip and a TCP echo round
/// trip, reached only through `std::net`. The program never sees a capability, a socket id, or a
/// shared frame; it writes to a socket and reads from it, the way any Rust program does. Runs when
/// the program holds the network. `sock` is the already-bound UDP socket the probe opened.
fn net_demo(sock: UdpSocket) {
    println!("std net on cricker-os");

    // Assertions rather than printed status keep the transcript byte-stable: a failure faults (the
    // panic path), which the kernel test sees as a missing line and a timeout, not a wrong answer.
    assert!(udp_ok(&sock), "the UDP round trip through std::net failed");
    println!("udp ok");

    // The UDP socket is held (by ref) across the TCP exchange so the two use distinct socket ids,
    // and thus distinct netstack local ports: netstack derives a socket's local port from its id, so a TCP
    // connect that reused a just-closed UDP socket's id would reuse its port against slirp and can
    // stall (notes/std.md, the reuse finding). Keeping both open sidesteps it cleanly.
    assert!(
        tcp_echo_ok(),
        "the TCP echo round trip through std::net failed"
    );
    println!("tcp echo ok");
    drop(sock);
}

/// **The gating UDP round trip: slirp's own TFTP server**, the `std::net` twin of `netcli`'s
/// `udp_tftp`. libslirp implements TFTP internally (enabled by `tftp=` on the netdev), so this
/// request and its reply never leave the emulator.
///
/// This used to be a DNS A-record query for `example.com` at 10.0.2.3:53, which is *not* a resolver:
/// libslirp NATs anything sent there to the HOST's nameserver, so the test silently depended on the
/// developer's DNS answering at that instant and flaked at roughly 2.5% per query. That was fixed for
/// the hand-built `netcli` gate and **missed here**, which is why this twin went on flaking after the
/// fix landed; it cost a riscv leg on 2026-07-29. The lesson is worth the sentence: a fix applied to
/// one of two call sites of the same hazard is half a fix, and the surviving half is harder to find
/// because the record says the problem is solved.
///
/// What it proves is what the DNS version was there to prove about *our* code and nothing about the
/// host: a program holding no capability and no socket id sends a datagram through `std::net` to an
/// address it chooses and reads the reply back. Send a read request (opcode 1, `octet` mode) for the
/// fixture the runners planted, and require the first data packet: opcode 3, block 1, the fixture's
/// bytes exactly. The fixture is one short block, so the whole file arrives in that packet.
///
/// The name and body must match what `scripts/qemu-runner*.sh` writes into `target/tftp`.
fn udp_ok(sock: &UdpSocket) -> bool {
    const TFTP_SERVER: &str = "10.0.2.2:69";
    const TFTP_NAME: &[u8] = b"cricker";
    const TFTP_BODY: &[u8] = b"cricker-tftp!";

    if sock.connect(TFTP_SERVER).is_err() {
        return false;
    }

    // RRQ: { u16 opcode = 1 } filename 0 "octet" 0
    let mut rrq = vec![0x00, 0x01];
    rrq.extend_from_slice(TFTP_NAME);
    rrq.push(0x00);
    rrq.extend_from_slice(b"octet");
    rrq.push(0x00);
    if sock.send(&rrq).is_err() {
        return false;
    }

    let mut buf = [0u8; 512];
    let Ok(n) = sock.recv(&mut buf) else {
        return false;
    };
    if n < 4 + TFTP_BODY.len() {
        return false;
    }
    // An ERROR packet (opcode 5) here means the fixture is missing: see the runners.
    let opcode = u16::from_be_bytes([buf[0], buf[1]]);
    let block = u16::from_be_bytes([buf[2], buf[3]]);
    if opcode != 3 || block != 1 || &buf[4..4 + TFTP_BODY.len()] != TFTP_BODY {
        return false;
    }

    // ACK block 1, ending the transfer properly rather than leaving the server retransmitting DATA
    // at a socket we are about to drop. Failing to be acknowledged is not this test's business, so
    // the send's result is deliberately ignored.
    let _ = sock.send(&[0x00, 0x04, 0x00, 0x01]);
    true
}

/// Connect to the echo peer over TCP, send a payload, and read the echo back whole.
fn tcp_echo_ok() -> bool {
    const MSG: &[u8] = b"cricker-std-net!";
    let Ok(mut stream) = TcpStream::connect(ECHO_PEER) else {
        return false;
    };
    if stream.write_all(MSG).is_err() {
        return false;
    }
    let mut got = Vec::new();
    let mut buf = [0u8; 64];
    while got.len() < MSG.len() {
        match stream.read(&mut buf) {
            Ok(0) => break, // peer closed
            Ok(k) => got.extend_from_slice(&buf[..k]),
            Err(_) => return false,
        }
    }
    got == MSG
}
