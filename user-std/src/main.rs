//! The std proof (milestone 27): an ordinary Rust program, no `no_std`, no attributes, running on
//! the native capability ABI. Every line exercises a PAL surface: `println!` SENDs on the stdout
//! endpoint (slot 1), collections draw from the untyped budget (slot 0), `Instant` reads the
//! virtual counter, and `fs` returns honestly `Unsupported`.
//!
//! **One binary, two behaviours, chosen by the authority it was granted.** A std program does
//! networking only if it holds the network (no ambient network, DECISIONS §10, §25). This program
//! probes for it with a single `UdpSocket::bind`:
//!   - **granted the network** (the loader placed a `Stack` endpoint and a frame untyped in slots 2
//!     and 3): it runs a real UDP DNS query and a TCP echo round trip through `std::net`
//!     (milestone 27 phase two), the same netd socket contract the hand-written client uses.
//!   - **not granted** (only the heap and stdout slots): `std::net` returns `Unsupported`, and the
//!     program runs the phase-one transcript, proving the collections, timing, and the honest
//!     refusal of `fs`/`net`.
//!
//! One binary keeps the initrd under its 15-file directory limit (crickerfs `MAX_FILES`) while
//! still proving both. The kernel test suite spawns it both ways and checks each transcript byte
//! for byte, on both ISAs.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::time::Instant;

fn main() {
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
}

/// slirp's built-in resolver and the guestfwd echo peer the test runners attach.
const DNS_SERVER: &str = "10.0.2.3:53";
const ECHO_PEER: &str = "10.0.2.9:7777";
const DNS_TXID: u16 = 0x1234;

/// The networked transcript (milestone 27 phase two): a real UDP DNS query and a TCP echo round
/// trip, reached only through `std::net`. The program never sees a capability, a socket id, or a
/// shared frame; it writes to a socket and reads from it, the way any Rust program does. Runs when
/// the program holds the network. `sock` is the already-bound UDP socket the probe opened.
fn net_demo(sock: UdpSocket) {
    println!("std net on cricker-os");

    // Assertions rather than printed status keep the transcript byte-stable: a failure faults (the
    // panic path), which the kernel test sees as a missing line and a timeout, not a wrong answer.
    assert!(dns_ok(&sock), "the UDP DNS query through std::net failed");
    println!("dns ok");

    // The UDP socket is held (by ref) across the TCP exchange so the two use distinct socket ids,
    // and thus distinct netd local ports: netd derives a socket's local port from its id, so a TCP
    // connect that reused a just-closed UDP socket's id would reuse its port against slirp and can
    // stall (notes/std.md, the reuse finding). Keeping both open sidesteps it cleanly.
    assert!(
        tcp_echo_ok(),
        "the TCP echo round trip through std::net failed"
    );
    println!("tcp echo ok");
    drop(sock);
}

/// A DNS A-record query for `name`, wire format, with recursion desired.
fn build_dns_query(name: &str, txid: u16) -> Vec<u8> {
    let mut q = Vec::new();
    q.extend_from_slice(&txid.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00]); // flags: recursion desired
    q.extend_from_slice(&[0x00, 0x01]); // qdcount = 1
    q.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // an/ns/ar counts = 0
    for label in name.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0x00); // root label
    q.extend_from_slice(&[0x00, 0x01]); // qtype A
    q.extend_from_slice(&[0x00, 0x01]); // qclass IN
    q
}

/// Send a DNS query over the given UDP socket and confirm the reply is a response to our query.
fn dns_ok(sock: &UdpSocket) -> bool {
    if sock.connect(DNS_SERVER).is_err() {
        return false;
    }
    let query = build_dns_query("example.com", DNS_TXID);
    if sock.send(&query).is_err() {
        return false;
    }
    let mut buf = [0u8; 512];
    let Ok(n) = sock.recv(&mut buf) else {
        return false;
    };
    if n < 12 {
        return false;
    }
    let rid = u16::from_be_bytes([buf[0], buf[1]]);
    let qr = buf[2] & 0x80; // high bit of byte 2 is the QR (query/response) flag
    rid == DNS_TXID && qr != 0
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
