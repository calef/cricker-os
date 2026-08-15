//! Machine-checked proofs (Kani), run by `script/verify`. The property that earns its solver time
//! here is the one a fuzzer can only gesture at: the name decoder is fed by anyone on the local
//! network, and it must terminate and stay in bounds on **every** byte string, not just on the
//! packets a working stack produces. The compression-pointer loop is the classic DNS-parser hang;
//! the decoder's strictly-backwards fence turns it into an error, and the first harness quantifies
//! that claim over every message of the proved size.
//!
//! The bound is small (the harnesses quantify over short buffers) and that is the same trade
//! `gpt`'s CRC harnesses record: what is being proved is the *structure* (termination, bounds,
//! round-tripping), which does not grow new behaviour with length; the realistic lengths are
//! covered by the tests over real captured packets.

use super::*;

/// Message size the decoder harness quantifies over. Every compression behaviour the decoder has
/// (a pointer, a chain of pointers, a pointer into a label, every malformed variant) is
/// expressible in 8 bytes.
const MSG: usize = 8;

/// **The name decoder never panics, never hangs and never strays, on any input.** For every
/// 8-byte message and every starting offset, `decode_name` returns; on success the resume offset
/// lies inside (or exactly at the end of) the message and the name is well-formed uncompressed
/// wire form: length-prefixed labels, root-terminated, no pointer bytes.
///
/// The unwind bound is the termination argument made concrete: each iteration either consumes a
/// label (advancing `pos` within a segment of at most `MSG` bytes) or follows a pointer (and the
/// fence strictly decreases, so at most `MSG` follows). `MSG = 8` gives at most `8 + 9 * 4`
/// iterations; 64 covers it with room, and Kani fails loudly if it does not.
#[kani::proof]
#[kani::unwind(64)]
fn the_name_decoder_is_total_and_in_bounds() {
    let msg: [u8; MSG] = kani::any();
    let off: usize = kani::any();
    kani::assume(off <= MSG + 1);

    if let Ok((name, resume)) = decode_name(&msg, off) {
        assert!(resume <= MSG, "resume offset escaped the message");
        assert!(resume > off, "the decoder consumed nothing");
        let bytes = name.as_bytes();
        assert!(!bytes.is_empty() && bytes.len() <= MAX_NAME_LEN);
        // Well-formed: walking the label lengths lands exactly on a root byte at the end.
        let mut i = 0;
        while i < bytes.len() - 1 {
            let l = bytes[i] as usize;
            assert!(
                l != 0 && l <= MAX_LABEL_LEN,
                "an interior label is malformed"
            );
            i += 1 + l;
        }
        assert!(i == bytes.len() - 1 && bytes[i] == 0, "not root-terminated");
    }
}

/// **The header codec round-trips on every field value.** Six `u16`s in, the same six out; a
/// byte-order slip or an offset collision cannot survive this and the counts are what every parse
/// decision hangs off.
#[kani::proof]
#[kani::unwind(8)]
fn the_header_round_trips_on_every_value() {
    let h = Header {
        id: kani::any(),
        flags: kani::any(),
        qdcount: kani::any(),
        ancount: kani::any(),
        nscount: kani::any(),
        arcount: kani::any(),
    };
    let mut buf = [0u8; HEADER_LEN];
    h.write(&mut buf).unwrap();
    assert_eq!(Header::parse(&buf).unwrap(), h);
}

/// **The TXT iterator terminates and stays in bounds on any RDATA.** TXT is the other
/// length-prefixed structure an attacker feeds; each step consumes at least one byte, so the
/// iterator ends, and every yielded string lies inside the input (the slice arithmetic would
/// panic otherwise, which is exactly what this harness rules out).
#[kani::proof]
#[kani::unwind(12)]
fn the_txt_iterator_is_total() {
    let rdata: [u8; MSG] = kani::any();
    let mut count = 0;
    for s in txt_strings(&rdata) {
        if let Ok(s) = s {
            assert!(s.len() <= MSG);
        }
        count += 1;
    }
    assert!(count <= MSG, "more strings than bytes");
}
