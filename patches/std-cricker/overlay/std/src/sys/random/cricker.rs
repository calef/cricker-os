//! Randomness for cricker-os: **not cryptographic, and saying so is the point.**
//!
//! The platform has no entropy source (no RNG device, no timing pool, no hardware RDRAND
//! analog on either ISA's base profile), and the ABI's one ambient readable is the virtual
//! counter. So this is a splitmix64 stream seeded from that counter: statistically fine for
//! `HashMap`'s DoS-resistant-ish hashing seeds and `sort_unstable`'s pivots, and utterly
//! predictable to anyone who can guess boot-relative time. `std::random` on this target must
//! not be used for keys, tokens, or anything an adversary cares about; notes/std.md carries
//! the caveat in the milestone record. A real entropy story (a virtio-rng driver feeding a
//! capability-granted service) is future work and would replace this file, not patch it.

use crate::sync::atomic::{AtomicU64, Ordering};
use crate::sys::pal::cricker::rt;

static STATE: AtomicU64 = AtomicU64::new(0);

fn next() -> u64 {
    // splitmix64: the increment is odd, so the state walks the full 2^64 cycle; the mixer is
    // the standard finalizer. Seed lazily from the counter on first use.
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

pub fn fill_bytes(bytes: &mut [u8]) {
    for chunk in bytes.chunks_mut(8) {
        let word = next().to_le_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
    }
}
