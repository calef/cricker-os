//! **The entropy contract** (milestone 56; DECISIONS §44, notes/entropy.md).
//!
//! One definition of the request and the reply that carry random bytes out of the entropy service,
//! so the service, its clients, the std PAL and the kernel-side tests cannot drift. The same split
//! `fs_proto` makes for the filesystem and `clock_proto` for the wall clock.
//!
//! # The shape, and why it is this small
//!
//! ```text
//!   the virtio-rng device (a Virtio capability, one holder)
//!             │
//!   ┌─────────▼────────┐
//!   │ entropy service  │◄──an endpoint──── a client that may OBTAIN randomness
//!   └──────────────────┘   (CALL, bytes    and may not reach the device
//!                           in the reply)
//! ```
//!
//! A client `CALL`s with a byte count and gets the bytes back **in the reply words**. There is no
//! shared page, which is the one design choice in here worth arguing about, so it is argued about
//! in DECISIONS §44: bulk normally rides in a page (§10), but a page shared with a client is a
//! place the bytes *persist* and a second party can read, and random bytes are the payload whose
//! whole value is that nobody else has seen them. Registers and the client's own stack are a
//! smaller footprint than a page both parties map. The cost is one round trip per
//! [`MAX_BYTES`] bytes, which is what the `entropy_rtt` benchmark prices.
//!
//! # Examples
//!
//! The one thing a client of this contract must get right is that **a reply is eight bytes, so
//! filling anything larger is a loop**. That is the cost DECISIONS §44 accepted in exchange for
//! never putting random bytes in a page a second party maps, so it is what an example should show:
//! a twenty-byte key takes three round trips, and the last one is short.
//!
//! ```
//! use entropy_proto::{GET, MAX_BYTES, delivered, op, req, take, want};
//!
//! // Stands in for the `CALL`: the real service reads the same two accessors off the request word
//! // and answers with a count and a word of bytes.
//! fn service(w0: u64) -> (u64, u64) {
//!     assert_eq!(op(w0), GET);
//!     (want(w0), 0x0807_0605_0403_0201)
//! }
//!
//! let mut key = [0u8; 20];
//! let mut filled = 0;
//! let mut round_trips = 0;
//! while filled < key.len() {
//!     let (r0, r1) = service(req(GET, (key.len() - filled) as u64));
//!     round_trips += 1;
//!     // `None` would mean the word is a kernel error rather than a count. See below.
//!     let n = delivered(r0).expect("the service answered");
//!     assert_ne!(n, 0, "a zero count is NO_ENTROPY: fail, do not retry");
//!     filled += take(n, r1, &mut key[filled..]);
//! }
//! assert_eq!(round_trips, 3); // 8 + 8 + 4, because MAX_BYTES is 8
//! assert_eq!(MAX_BYTES, 8);
//! assert_eq!(&key[16..], &[0x01, 0x02, 0x03, 0x04]);
//! ```
//!
//! The other property worth stating as code is the one [`delivered`] exists for, because it is a
//! claim about two number spaces *not* overlapping. A program holding no entropy capability at all
//! gets a kernel error in the register a count would have arrived in, and it does not have to probe
//! to find out which it is holding:
//!
//! ```
//! use entropy_proto::{MAX_BYTES, NO_ENTROPY, delivered};
//!
//! // Every count the service can send.
//! for n in 0..=MAX_BYTES {
//!     assert_eq!(delivered(n), Some(n as usize));
//! }
//! // `abi::Error` is -1 to -8; as a u64 each is enormous, so none can be read as a count.
//! assert_eq!(delivered(-4i64 as u64), None); // an empty slot: no entropy capability
//!
//! // And "the service has none to give" is a third answer, distinct from both.
//! assert_eq!(delivered(NO_ENTROPY), Some(0));
//! ```
//!
//! # Nothing here transforms a byte
//!
//! The service passes the device's bytes through. It does not hash, mix, whiten, or stretch them,
//! and this crate has no arithmetic in it at all for exactly that reason: with no cryptographic
//! primitive in the tree (that is the other half of milestone 56), any "whitening" would be a
//! reversible permutation that changes the bytes without adding an unpredictability an attacker
//! could not undo. The security property is therefore statable in one sentence: **these are the
//! device's bytes**, and the device is named in the notes.
//!
//! Name: recorded (milestone 46, and notes/naming.md's crate section). The wire contract was
//! spelled four ways (`fs_proto`, `gfx_proto`, `netproto`, `line_editor::proto`) for one concept;
//! `*_proto` won on 2026-07-30 under DECISIONS §39, and `script/lint` has checked it since. That
//! rule plus the service the stem names produces this name, which is the whole of what `recorded`
//! claims: calef ruled on the rule, and never on this crate.
//! The stem is the service's own word (`entropy`, DECISIONS §44), which is itself unrecorded.

#![cfg_attr(not(test), no_std)]

/// The most bytes one request can carry: the reply's second word.
///
/// A `CALL` replies with two words. The first is the count, so the bytes get exactly one word.
/// Asking for more than this is not an error, it is answered with this many, and the caller loops;
/// see [`want`].
pub const MAX_BYTES: u64 = 8;

/// Where a request packs its opcode: bits 63:56 of the first `CALL` word, the same position
/// `fs_proto` and `line_editor::proto` use, so the contracts read alike.
pub const OP_SHIFT: u32 = 56;

/// **Give me `n` random bytes.** The only operation. `n` is clamped to [`MAX_BYTES`] by [`want`],
/// so a client that asks for more is answered rather than refused.
pub const GET: u64 = 1;

/// Build a request word: the opcode and the byte count the client wants.
pub const fn req(op: u64, n: u64) -> u64 {
    (op << OP_SHIFT) | (n & 0xff)
}

/// The opcode of a request word.
pub const fn op(w0: u64) -> u64 {
    w0 >> OP_SHIFT
}

/// How many bytes a request asks for, clamped to what one reply can carry. Clamping here rather
/// than refusing keeps the server's loop free of an error path that only a buggy client reaches:
/// a request for 500 bytes is a request for eight, answered, and the client asks again.
pub const fn want(w0: u64) -> u64 {
    let n = w0 & 0xff;
    if n > MAX_BYTES { MAX_BYTES } else { n }
}

/// The word the entropy service SENDs on its readiness endpoint once the device is up and its
/// first bytes are in hand. ASCII `RNGUP`, so a hex dump of a report reads. A bring-up failure
/// reports `0xDEAD_0000_0000_0000 | step` instead, the same shape every driver here uses.
pub const READY: u64 = 0x_52_4E_47_55_50;

/// **The reply's first word when the service has no entropy to give.** Zero bytes delivered, and
/// the second word is meaningless. The service sends this rather than padding, repeating an
/// earlier answer, or substituting a pseudo-random stand-in: a caller that cannot be given
/// randomness must find out, because the alternative is the exact silent-degradation failure
/// DECISIONS §42 forbids.
pub const NO_ENTROPY: u64 = 0;

/// **Read a reply's first word.** `Some(n)` when the entropy service answered, with `n` the number
/// of valid low bytes in the second word (0 means [`NO_ENTROPY`]). `None` when the word did not
/// come from the service at all.
///
/// The discrimination is free here, and deliberately so. A count can only ever be `0..=MAX_BYTES`,
/// while every failure the *kernel* can return from a `CALL` is one of its small negatives
/// (`abi::Error`, -1 to -8), which read as enormous `u64`s. So a caller holding no entropy
/// capability sees `None` and can say "this platform cannot do that" without a separate probe.
/// `fs_proto` could not manage this (its errno space collides with the kernel's, a wart
/// notes/std.md records), and a contract this new has no excuse to inherit the collision.
pub const fn delivered(r0: u64) -> Option<usize> {
    if r0 <= MAX_BYTES {
        Some(r0 as usize)
    } else {
        None
    }
}

/// Copy the `n` delivered bytes out of a reply's second word into `out`, returning how many landed.
///
/// Little-endian, and only the low `n` bytes are meaningful: the rest of the word is whatever the
/// service had left in the register and carries no entropy, so a caller that read them would be
/// mixing zeros into a key and believing otherwise.
pub fn take(n: usize, word: u64, out: &mut [u8]) -> usize {
    let n = n.min(MAX_BYTES as usize).min(out.len());
    out[..n].copy_from_slice(&word.to_le_bytes()[..n]);
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_round_trips_its_opcode_and_count() {
        let w = req(GET, 5);
        assert_eq!(op(w), GET);
        assert_eq!(want(w), 5);
    }

    /// Pins the wire layout as an exact word, with an opcode that is not `GET`. `GET` is 1, so an
    /// `op` that ignored its input and answered 1 would pass every round trip built from it; this
    /// is the one place the shift is checked against a number the protocol does not define.
    #[test]
    fn the_request_word_layout_is_exact() {
        let w = req(0x7f, 5);
        assert_eq!(w, 0x7f00_0000_0000_0005);
        assert_eq!(op(w), 0x7f);
        assert_eq!(want(w), 5);
    }

    /// A client asking for more than one reply can carry is answered with a full reply, not an
    /// error. The clamp lives in the contract so the server and the caller agree on it without
    /// either one enforcing it.
    #[test]
    fn an_oversized_request_is_clamped_rather_than_refused() {
        assert_eq!(want(req(GET, 200)), MAX_BYTES);
        assert_eq!(want(req(GET, MAX_BYTES)), MAX_BYTES);
    }

    /// **The property the whole error story rests on**: no count the service can send collides with
    /// any error the kernel can return, so "no entropy capability" is distinguishable from "no
    /// entropy" without a probe. `abi::Error` is -1..-8; those are the words a `CALL` on an empty
    /// slot puts in the first register.
    #[test]
    fn a_kernel_refusal_is_never_mistaken_for_a_byte_count() {
        for n in 0..=MAX_BYTES {
            assert_eq!(delivered(n), Some(n as usize));
        }
        for err in 1i64..=8 {
            assert_eq!(delivered((-err) as u64), None, "kernel error -{err}");
        }
        assert_eq!(delivered(MAX_BYTES + 1), None);
    }

    /// The high bytes of the reply word are not entropy and must not reach the caller's buffer.
    #[test]
    fn only_the_delivered_bytes_are_copied_out() {
        let mut out = [0xAAu8; 8];
        let n = take(3, 0xFFFF_FFFF_FF03_0201, &mut out);
        assert_eq!(n, 3);
        assert_eq!(out, [0x01, 0x02, 0x03, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA]);
    }

    /// A short buffer is the caller's business, not a panic: `take` fills what fits.
    #[test]
    fn a_short_buffer_takes_what_fits() {
        let mut out = [0u8; 2];
        assert_eq!(take(8, u64::MAX, &mut out), 2);
        assert_eq!(out, [0xFF, 0xFF]);
    }
}
