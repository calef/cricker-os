//! **The swappable component, version 1: the incumbent** (milestone 23, DECISIONS §41).
//!
//! A console-shaped server. It owns a device (the UART's registers), it serves one stable endpoint,
//! and it holds nothing else it could reach the rest of the system with. What makes it interesting
//! is not what it does but that it can be taken out from under its client while the client is
//! talking to it.
//!
//! Its whole authority is four capabilities and two pages, and every one of them was placed by
//! `swapper` before this program ran: the stable service endpoint (READ), a report endpoint (WRITE),
//! the operator's coordination channel (WRITE), the operator's post-revoke poke (READ), the shared
//! log page (read/write), and the device's registers. No initrd, no budget, no way to build
//! anything. A compromised instance is a bad console and nothing else.
//!
//! The serving loop is in `swap.rs` and is shared byte for byte with
//! [`c_swappable`](../c_swappable/index.html), the replacement. The *only* difference between the
//! two programs is the function that computes a request's answer: this one is Rust, that one is C.
//! That is deliberate, and it is what makes the milestone's claim about the component rather than
//! about the harness.
//!
//! Name: ratified 2026-08-01 (calef, milestone 61), replacing `conx`. Refused `conx`, whose
//! expansion is recorded nowhere in this tree; that absence is one of the two cases the naming
//! tenet cites as evidence that it needed a rule.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

// A source file shared by several binaries through `#[path]`, and each uses a different slice of it,
// so the unused halves are expected. This is the one shape where a blanket allow is the honest one:
// the module is compiled once per binary and no single binary is meant to use all of it (§38).

/// `a0` says whether the operator endowed us with a device, and `a1` where our entries start in the
/// shared witness page. Both come from the operator, because both are facts about the wiring rather
/// than about this program: the incumbent on the direct channel owns a UART, the backend behind the
/// queue broker does not, and neither can tell by looking.
///
/// `a2` is the sequence number to stop answering at, or zero to serve honestly, which is what the
/// two healthy channels pass. It is a start argument rather than a manifest field for the reason
/// notes/component-manifest.md gives: a manifest declares authority, and this carries none. A
/// component lied to about it misbehaves rather than escaping, which is precisely what the hung
/// channel wants it to do.
#[unsafe(no_mangle)]
pub extern "C" fn _start(device: u64, log_base: u64, wedge: u64) -> ! {
    swap_proto::serve(
        swap_proto::V1,
        swap_proto::digest,
        log_base,
        device != 0,
        wedge,
    )
}

user_rt::panic_handler!();
