//! The std proof (milestone 27): an ordinary Rust program, no `no_std`, no `unsafe`, no
//! attributes, running on the native capability ABI. Every line here exercises a PAL surface:
//! `println!` SENDs on the stdout endpoint (slot 1), collections draw from the untyped budget
//! (slot 0), `Instant` reads the virtual counter, and `fs`/`net` return honestly `Unsupported`.
//!
//! The output is deterministic on purpose: the kernel test suite reassembles the byte stream
//! from the endpoint and compares it against this exact text, on both ISAs. Timing is asserted
//! (monotonic, nonzero) but never printed, so the transcript stays byte-stable.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::time::Instant;

fn main() {
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
    match std::net::TcpStream::connect("127.0.0.1:80") {
        Err(e) if e.kind() == ErrorKind::Unsupported => println!("net honestly unsupported"),
        other => println!("net lied: {other:?}"),
    }

    // Instant: monotonic and advancing, but asserted rather than printed (a printed duration
    // would make the transcript nondeterministic).
    let t1 = Instant::now();
    assert!(t1 >= t0, "the virtual counter went backwards");
    assert!(t1.duration_since(t0).as_nanos() > 0, "no time passed across real work");
    println!("instant monotonic ok");
}
