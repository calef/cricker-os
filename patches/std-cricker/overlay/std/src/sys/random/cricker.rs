//! Randomness for cricker-os: **a granted capability, and a loud refusal when it is absent**
//! (milestone 56; DECISIONS §44, notes/entropy.md).
//!
//! This file used to open with "not cryptographic, and saying so is the point": a splitmix64 stream
//! seeded from the virtual counter, predictable to anyone who could guess boot-relative time. That
//! caveat tainted anything security-adjacent anywhere in the tree, so it was replaced rather than
//! patched, exactly as its own last paragraph said it should be. What replaced it is a **virtio-rng
//! device held by a userspace entropy service**, reached through one endpoint in slot
//! [`rt::ENTROPY_SLOT`]. This program cannot touch the device; it can only ask.
//!
//! # The fork this file settles: transparent, and split on std's own seam
//!
//! `std::random` improves **transparently**: a program that calls [`crate::random::SystemRng`] gets
//! real entropy the moment it is granted the capability, with no cricker-specific API to learn and
//! no code to change. The honesty that "explicit" would have bought is preserved a different way,
//! by refusing rather than by degrading, and the two callers are split where std already splits
//! them:
//!
//! - [`fill_bytes`] backs `SystemRng`, which std documents as "random data suitable for
//!   cryptographic purposes such as key generation". That is a promise, so a program that holds no
//!   entropy capability gets a **panic** naming the reason. `fill_bytes` has no error channel (the
//!   same wall `SystemTime::now()` hit, DECISIONS §43), and the only refusal available is a loud
//!   one. Quietly substituting a counter-seeded stream here is precisely the failure this milestone
//!   exists to remove.
//! - [`hashmap_random_keys`] backs `RandomState`, whose promise is DoS resistance for a hash table
//!   and nothing more. std's own `unsupported` backend seeds it from allocation addresses rather
//!   than panicking, so the platform is allowed to be best-effort here. A program with the
//!   capability gets device bytes; a program without gets the counter-seeded splitmix64 stream
//!   below, which is what this whole file used to be. A `HashMap` in a program nobody granted
//!   entropy still works, and no key is ever minted from it.
//!
//! So the rule is one sentence: **the caller that promises cryptographic strength refuses when it
//! cannot keep the promise, and the caller that promises nothing degrades and says so.**
//!
//! # What the bytes are
//!
//! Whatever the device produced, unmodified. The service pools nothing and whitens nothing (there
//! is no cryptographic primitive in this tree to whiten with, and a reversible permutation would
//! only obscure the claim). Under QEMU the device is backed by the host's `/dev/urandom`; on real
//! silicon it is the board's TRNG, and notes/entropy.md carries what that does and does not
//! promise.

use crate::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use crate::sys::pal::cricker::entropyproto as proto;
use crate::sys::pal::cricker::rt;

/// The entropy service's request endpoint: this process's entire authority over randomness. It
/// names no device.
const ENTROPY: u64 = rt::ENTROPY_SLOT;

// --- Is an entropy service reachable at all? ---------------------------------------------------
//
// The probe is a real request, which is what this contract buys over `fs`'s and `time`'s: a reply's
// first word is a byte count in `0..=MAX_BYTES`, and every failure the kernel can return from a
// `CALL` is one of its small negatives, which read as enormous `u64`s. So `proto::delivered`
// separates "the service answered" from "there is no service" with no extra syscall and no
// collision to reason about (contrast the errno/kernel-error overlap notes/std.md records for fs).
//
// Cached, because the answer cannot change: a cspace slot's contents are fixed at spawn on this
// ABI (0 = not yet asked, 1 = granted, 2 = not granted). Same shape as the fs and time PALs.

static GRANTED: AtomicU8 = AtomicU8::new(0);

/// Ask the service for up to eight bytes. `Some((count, word))` when a service answered, and the
/// count may be zero if it has no entropy to give. `None` when this process holds no entropy
/// capability at all.
fn ask(n: usize) -> Option<(usize, u64)> {
    if GRANTED.load(Ordering::Relaxed) == 2 {
        return None;
    }
    let (r0, r1) = rt::call(ENTROPY, proto::req(proto::GET, n as u64), 0);
    match proto::delivered(r0) {
        Some(count) => {
            GRANTED.store(1, Ordering::Relaxed);
            Some((count, r1))
        }
        None => {
            GRANTED.store(2, Ordering::Relaxed);
            None
        }
    }
}

/// **Random bytes suitable for keys**, or a panic naming exactly why there are none.
///
/// The two failing cases are deliberately distinguished, because they call for different fixes:
/// nobody granted this process an entropy capability, or the machine's random-number generator
/// produced nothing when asked.
pub fn fill_bytes(bytes: &mut [u8]) {
    let mut filled = 0;
    while filled < bytes.len() {
        let want = (bytes.len() - filled).min(proto::MAX_BYTES as usize);
        let Some((count, word)) = ask(want) else {
            panic!(
                "std::random on cricker-os: this process holds no entropy capability, so it \
                 cannot produce random bytes. Returning predictable ones is the lie this refusal \
                 exists to replace (DECISIONS §44, notes/entropy.md)."
            )
        };
        if count == 0 {
            panic!(
                "std::random on cricker-os: the entropy service is reachable but its device \
                 produced no bytes. This is a hardware fact, not a missing grant (DECISIONS §42, \
                 §44)."
            );
        }
        filled += proto::take(count, word, &mut bytes[filled..]);
    }
}

/// **The `HashMap` seed**, which is allowed to be best-effort and says so.
///
/// std calls this exactly once per `RandomState::new` fast path, to make hash iteration order and
/// collision behaviour unpredictable enough to spoil a trivial denial-of-service. It is not a key,
/// and no key is ever derived from it. With the capability these are device bytes; without it they
/// are [`weak_seed`], and the difference is invisible from here on purpose: std's own `unsupported`
/// backend degrades this same function to allocation addresses rather than failing, so a platform
/// is permitted to.
pub fn hashmap_random_keys() -> (u64, u64) {
    match (ask(8), ask(8)) {
        (Some((8, a)), Some((8, b))) => (a, b),
        _ => (weak_seed(), weak_seed()),
    }
}

/// **splitmix64 from the virtual counter: the old whole file, kept for the one caller that may
/// honestly use it.**
///
/// The increment is odd, so the state walks the full 2^64 cycle, and the mixer is the standard
/// finalizer. Statistically fine for a hash seed, and **utterly predictable to anyone who can guess
/// boot-relative time**, because the counter is the ABI's one ambient readable. Nothing in this
/// file lets it reach [`fill_bytes`], and that separation is the milestone's point.
fn weak_seed() -> u64 {
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut s = STATE.load(Ordering::Relaxed);
    if s == 0 {
        s = rt::now() | 1;
    }
    s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    STATE.store(s, Ordering::Relaxed);
    let mut z = s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
