//! The allocator workload (milestone 27): `alloc` collections on the untyped-backed heap.
//!
//! The first program in the tree that links `extern crate alloc`. It wires
//! `user_rt::heap::UntypedHeap` as its global allocator, then does the things a heap exists for
//! and a bump pointer cannot do: interleaved allocation and free in arbitrary order (`Vec`,
//! `String`, `BTreeMap`), drop-and-reuse, and a final large allocation that must fit in the pages
//! already committed (proving freed memory is actually reusable, not leaked).
//!
//! Every step asserts its own result; a wrong value panics, which faults (`brk`/`ebreak`), which
//! the kernel reports and the waiting test's watchdog turns into a failure. On success it SENDs a
//! magic word plus the committed byte count, so the kernel test can also assert the heap grew and
//! stayed inside its cap.
//!
//! # Capability contract (notes/abi.md §4)
//! - slot 0: the untyped budget the heap draws from
//! - slot 1: the report endpoint (WRITE)
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `allocdemo`. Refused `allocdemo`
//! (`alloc` is the crate it uses; the allocator is what it proves) and `demo` as a suffix anywhere:
//! "exercise" is this tree's own verb, it is real systems vocabulary (memory, bus and disk
//! exercisers have meant this for decades), and it is more precise, since a demo shows something
//! off where an exerciser puts a subsystem under load and sees whether it holds.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use user_rt::{exit, send};

const UNTYPED: u64 = 0;
const REPORT: u64 = 1;

/// "The heap held": sent as w0 on success. Any other value (or silence) fails the test.
const MAGIC: u64 = 0xA110_C0DE;

/// Cap the heap well under the granted budget, so the test can tell "grew as needed" from "ate
/// everything it was handed".
const HEAP_MAX: u64 = 64 * 4096;

#[global_allocator]
static HEAP: user_rt::heap::UntypedHeap = user_rt::heap::UntypedHeap::new();

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    HEAP.init(UNTYPED, user_rt::heap::DEFAULT_BASE, HEAP_MAX);

    // 1. A Vec big enough to force several growth reallocations (and each grow frees the old
    //    buffer, so this also churns the free list).
    let mut v: Vec<u64> = Vec::new();
    for i in 0..10_000u64 {
        v.push(i.wrapping_mul(2_654_435_761));
    }
    let sum: u64 = v.iter().fold(0u64, |a, &x| a.wrapping_add(x));
    let expected: u64 =
        (0..10_000u64).fold(0u64, |a, i| a.wrapping_add(i.wrapping_mul(2_654_435_761)));
    assert_eq!(sum, expected);

    // 2. Strings: heap bytes with content that must survive verbatim.
    let mut s = String::new();
    for _ in 0..100 {
        s.push_str("nife ");
    }
    assert_eq!(s.len(), 500);
    assert!(s.ends_with("nife "));

    // 3. A BTreeMap: many small node allocations, then removal of every other key, which frees
    //    mid-heap blocks in an order no stack could produce.
    let mut m: BTreeMap<u64, u64> = BTreeMap::new();
    for k in 0..500u64 {
        m.insert(k, k * k);
    }
    for k in (0..500u64).step_by(2) {
        assert_eq!(m.remove(&k), Some(k * k));
    }
    assert_eq!(m.len(), 250);
    assert_eq!(m.get(&251), Some(&(251 * 251)));

    // 4. Reuse: drop everything, note the committed pages, and allocate one block bigger than
    //    any single earlier allocation. It must fit without growing the heap; if freed memory
    //    leaked or failed to coalesce, this either grows or dies.
    drop(v);
    drop(s);
    drop(m);
    let committed_before = HEAP.committed_bytes();
    let big: Vec<u8> = alloc::vec![0xA5; 128 * 1024];
    assert_eq!(big.len(), 128 * 1024);
    assert_eq!(
        HEAP.committed_bytes(),
        committed_before,
        "reuse must not grow the heap"
    );
    drop(big);

    send(REPORT, MAGIC, HEAP.committed_bytes(), 0);
    exit();
}

user_rt::panic_handler!();
