//! **The swappable component, version 2: the replacement, and it is written in C** (milestone 23,
//! DECISIONS §41, over the seam DECISIONS §31 built).
//!
//! Identical to [`rust_swappable`](../rust_swappable/index.html) in every respect a client could
//! observe: same capability layout, same wire protocol, same serving loop (both compile `swap.rs`).
//! The one difference is the function that computes a request's answer, which here is `cx_digest`
//! in `user/c/c_swappable.c`, compiled by a bare-metal clang and linked into this binary alone.
//!
//! **Why that difference is the point.** "A client did not notice the swap" is a much weaker claim
//! if the replacement is a recompile of the same source. Here the replacement is a different
//! implementation, in a language the kernel's safety argument does not extend to, and the client
//! checks every answer against its own independent computation. What holds across the swap is the
//! *contract*, which is exactly what the milestone says a component is.
//!
//! The confinement story is unchanged from milestone 36: the C holds no capability and makes no
//! syscall, because this Rust shell holds every capability and does every syscall. The C's entire
//! interface to the system is `(uint64_t) -> uint64_t`.
//!
//! Name: ratified 2026-08-01 (calef, milestone 61), replacing `cconx`. Refused `conx` and `cconx`:
//! no expansion for `conx` is recorded anywhere, not in DECISIONS §41, not in
//! notes/live-replacement.md, not in the commit that introduced it, and that absence is one of the
//! two cases that made naming a ratified act.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

// A source file shared by several binaries through `#[path]`, and each uses a different slice of it,
// so the unused halves are expected (§38).

// The foreign component. One function, one ABI: an integer in, an integer out. No structs, no
// callbacks, no pointers, nothing that needs a header file to agree on.
unsafe extern "C" {
    fn cx_digest(seq: u64) -> u64;
}

/// The Rust side of the seam: a safe wrapper with the signature the serving loop wants. Every call
/// into the foreign component goes through here, which is why there is exactly one `unsafe` block
/// in this program.
fn digest_in_c(seq: u64) -> u64 {
    // SAFETY: `cx_digest` is pure arithmetic on a value passed by register. It dereferences
    // nothing, so there is no pointer for it to get wrong, which is what makes this the narrowest
    // possible version of the C seam.
    unsafe { cx_digest(seq) }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(device: u64, log_base: u64, wedge: u64) -> ! {
    swap_proto::serve(swap_proto::V2, digest_in_c, log_base, device != 0, wedge)
}

user_rt::panic_handler!();
