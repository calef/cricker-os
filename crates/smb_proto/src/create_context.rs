//! **SMB2 create contexts** ([MS-SMB2] §2.2.13.2), the extension chain a CREATE request and its
//! response may carry.
//!
//! A create context is a tagged blob hung off a CREATE. It is how every SMB2 extension that needs
//! to say something *at open time* says it, and it is the only door Apple's extensions come
//! through: macOS negotiates them by attaching an `AAPL` context to the first CREATE of a tree
//! connect and reading the context the server attaches to its answer (see [`crate::apple`]).
//!
//! This module is the **chain**, not any one context's meaning. That split is worth having even
//! with one consumer, because the chain is generic and the contexts are not: a real macOS client
//! also sends `DHnQ` (durable handle request), `MxAc` (maximal access), `QFid` (query on-disk id)
//! and `RqLs` (lease request) in the same chain, and this server has to walk past them to find the
//! one it answers. Walking past is all it does with them today; see BUGS.
//!
//! # The layout
//!
//! One context, with every offset relative to **the start of that context**:
//!
//! | offset | field | |
//! |---|---|---|
//! | 0 | `Next` (u32) | bytes to the next context, or 0 for the last |
//! | 4 | `NameOffset` (u16) | |
//! | 6 | `NameLength` (u16) | four ASCII bytes for every tag in the wild |
//! | 8 | `Reserved` (u16) | |
//! | 10 | `DataOffset` (u16) | |
//! | 12 | `DataLength` (u32) | |
//! | 16 | `Buffer` | the name, padded to an 8-byte boundary, then the data |
//!
//! The chain itself is located by `CreateContextsOffset`/`CreateContextsLength` in the CREATE
//! request or response, and **those** two are relative to the start of the SMB2 header rather than
//! to the body, which is the offset convention this protocol uses everywhere and the one every
//! off-by-64 in it comes from.
//!
//! # EXAMPLES
//!
//! Emit a chain and read it back, which is exactly the round trip a client and this server make
//! across the wire:
//!
//! ```
//! use smb_proto::create_context;
//!
//! let mut buf = [0u8; 64];
//! let n = create_context::write_one(&mut buf, b"AAPL", &[1, 0, 0, 0]).unwrap();
//!
//! // The data comes back byte-identical, found by its tag.
//! assert_eq!(create_context::find(&buf[..n], b"AAPL"), Some(&[1u8, 0, 0, 0][..]));
//! // A tag that is not there is absent rather than an error: a server answers the contexts it
//! // knows and ignores the rest, which is what makes the extension mechanism extensible.
//! assert_eq!(create_context::find(&buf[..n], b"MxAc"), None);
//!
//! // The chain walks as an iterator too, for a caller that wants all of them.
//! let tags: Vec<&[u8]> = create_context::contexts(&buf[..n]).map(|c| c.name).collect();
//! assert_eq!(tags, vec![&b"AAPL"[..]]);
//! ```
//!
//! # BUGS
//!
//! - **Only one context is ever emitted.** [`write_one`] writes a chain of length one, because the
//!   only context this server answers is `AAPL`. A `Next` pointer is written (as zero) rather than
//!   assumed away, so growing to a real chain is a second function and not a format change.
//! - **Every context this server does not answer is ignored, silently.** A real macOS CREATE
//!   carries a durable-handle request and a lease request, and neither gets an answering context,
//!   which the protocol permits and clients handle by falling back. It does mean a client that
//!   *requires* one would fail here with no diagnosis on the wire.
//! - **A malformed context ends the walk rather than failing it.** An entry whose name or data
//!   runs off the end of the blob, or whose `Next` does not advance, stops the iterator; contexts
//!   after it are not seen. That is deliberate (a server that refused a CREATE over an extension
//!   it does not implement would be worse), and it means a client cannot tell "you ignored my
//!   context" from "your parse of my chain gave up".
//!
//! Name: unrecorded. `create_context` is provisional, minted by milestone 55's lane on 2026-08-17.
//! It is [MS-SMB2]'s own term for the structure, which is the naming tenet's best case: a name a
//! reader already knows from outside this project costs them nothing.

use crate::{r16, r32, w16, w32};

/// The fixed part of one context, before its name.
pub const HEADER_LEN: usize = 16;

/// One create context: its tag and its payload, both borrowed out of the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context<'a> {
    /// The tag. Four ASCII bytes for every context in the wild, but this parse does not insist:
    /// [MS-SMB2] gives the field a length, and a server that assumed four would mis-walk a chain
    /// rather than ignore one entry of it.
    pub name: &'a [u8],
    /// The payload, whose meaning belongs to whoever knows the tag.
    pub data: &'a [u8],
}

/// **Walk a create-context chain.** `blob` is exactly the `CreateContextsLength` bytes at
/// `CreateContextsOffset`; see the module header on where those two are measured from.
pub fn contexts(blob: &[u8]) -> Contexts<'_> {
    Contexts { blob, off: Some(0) }
}

/// **The payload of the context tagged `name`**, or `None` if the chain does not carry one.
///
/// The first match wins. A chain with two contexts of one tag is malformed and no client sends
/// one; taking the first is a decision rather than an accident, because the alternative (refusing
/// the CREATE) trades a working mount for a diagnosis nobody reads.
pub fn find<'a>(blob: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    contexts(blob).find(|c| c.name == name).map(|c| c.data)
}

/// The iterator [`contexts`] returns.
pub struct Contexts<'a> {
    blob: &'a [u8],
    /// Where the next context starts, or `None` once the chain has ended (cleanly or otherwise).
    off: Option<usize>,
}

impl<'a> Iterator for Contexts<'a> {
    type Item = Context<'a>;

    fn next(&mut self) -> Option<Context<'a>> {
        let off = self.off?;
        // Ending the chain *first* is what makes every early return below a clean stop rather than
        // a loop: nothing after this point can leave `off` pointing at the entry that just failed.
        self.off = None;
        let c = self.blob.get(off..)?;
        if c.len() < HEADER_LEN {
            return None;
        }
        let next = r32(c, 0) as usize;
        let name_off = r16(c, 4) as usize;
        let name_len = r16(c, 6) as usize;
        let data_off = r16(c, 10) as usize;
        let data_len = r32(c, 12) as usize;

        let name = c.get(name_off..name_off.checked_add(name_len)?)?;
        // A zero-length payload is legal and is not the same thing as a missing one, so it is an
        // empty slice rather than a `None`. `DataOffset` is not consulted when there is no data,
        // which is what senders that leave it zero require.
        let data: &[u8] = if data_len == 0 {
            &[]
        } else {
            c.get(data_off..data_off.checked_add(data_len)?)?
        };

        // A `Next` that does not clear this context's own header cannot be a forward step, so it
        // ends the chain. Without that check a hostile (or merely wrong) `Next` of 0 mid-chain
        // would be indistinguishable from a terminator, and one of 4 would spin.
        if next >= HEADER_LEN {
            self.off = off.checked_add(next);
        }
        Some(Context { name, data })
    }
}

/// The bytes [`write_one`] needs for a context with a `name_len`-byte tag and `data_len` bytes of
/// payload. `const` so a caller can size a buffer at compile time, which is how the server's
/// response buffer proves it has the room.
pub const fn one_len(name_len: usize, data_len: usize) -> usize {
    let after_name = HEADER_LEN + name_len;
    // The payload is 8-byte aligned within the context, which is this protocol's rule for every
    // buffer it hangs off a fixed structure.
    let data_at = after_name.next_multiple_of(8);
    data_at + data_len
}

/// **Write a one-context chain** at the start of `out`, returning its length, or `None` if `out`
/// is too small.
///
/// The caller then puts the offset of `out` (measured from the SMB2 header) in
/// `CreateContextsOffset` and this length in `CreateContextsLength`.
pub fn write_one(out: &mut [u8], name: &[u8], data: &[u8]) -> Option<usize> {
    let total = one_len(name.len(), data.len());
    let out = out.get_mut(..total)?;
    out.fill(0);
    let data_at = (HEADER_LEN + name.len()).next_multiple_of(8);
    w32(out, 0, 0); // Next: this is the only one
    w16(out, 4, HEADER_LEN as u16);
    w16(out, 6, name.len() as u16);
    w16(out, 10, data_at as u16);
    w32(out, 12, data.len() as u32);
    out[HEADER_LEN..HEADER_LEN + name.len()].copy_from_slice(name);
    out[data_at..data_at + data.len()].copy_from_slice(data);
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_context_reads_back_exactly() {
        let mut buf = [0u8; 128];
        let payload: [u8; 24] = core::array::from_fn(|i| i as u8);
        let n = write_one(&mut buf, b"AAPL", &payload).unwrap();
        // 16 header + 4 name, padded to 24, plus 24 of payload.
        assert_eq!(n, 48);
        assert_eq!(one_len(4, 24), 48);
        let all: Vec<Context<'_>> = contexts(&buf[..n]).collect();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, b"AAPL");
        assert_eq!(all[0].data, &payload[..]);
        assert_eq!(find(&buf[..n], b"AAPL"), Some(&payload[..]));
        assert_eq!(find(&buf[..n], b"DHnQ"), None);
    }

    #[test]
    fn the_payload_lands_eight_byte_aligned() {
        let mut buf = [0u8; 128];
        for name_len in 1..=8usize {
            let name = &b"ABCDEFGH"[..name_len];
            write_one(&mut buf, name, &[0xAA]).unwrap();
            let data_at = r16(&buf, 10) as usize;
            assert_eq!(data_at % 8, 0, "name length {name_len}");
            assert!(data_at >= HEADER_LEN + name_len);
        }
    }

    #[test]
    fn a_chain_of_two_walks_and_finds_the_second() {
        // Hand-built, because `write_one` cannot make one: a chain is what a real client sends.
        let mut buf = [0u8; 128];
        // First context: tag "MxAc", empty payload, Next = 24.
        w32(&mut buf, 0, 24);
        w16(&mut buf, 4, 16);
        w16(&mut buf, 6, 4);
        buf[16..20].copy_from_slice(b"MxAc");
        // Second: tag "AAPL", four bytes of payload at 24 + 24.
        let at = 24;
        w32(&mut buf, at, 0);
        w16(&mut buf, at + 4, 16);
        w16(&mut buf, at + 6, 4);
        w16(&mut buf, at + 10, 24);
        w32(&mut buf, at + 12, 4);
        buf[at + 16..at + 20].copy_from_slice(b"AAPL");
        buf[at + 24..at + 28].copy_from_slice(&[9, 8, 7, 6]);

        let tags: Vec<&[u8]> = contexts(&buf[..at + 28]).map(|c| c.name).collect();
        assert_eq!(tags, vec![&b"MxAc"[..], &b"AAPL"[..]]);
        assert_eq!(find(&buf[..at + 28], b"AAPL"), Some(&[9u8, 8, 7, 6][..]));
        // The one with no payload is an empty slice, not an absence.
        assert_eq!(find(&buf[..at + 28], b"MxAc"), Some(&[][..]));
    }

    #[test]
    fn a_chain_that_does_not_advance_terminates() {
        let mut buf = [0u8; 64];
        // `Next` of 4 points back inside this very header. A walk that trusted it would spin.
        w32(&mut buf, 0, 4);
        w16(&mut buf, 4, 16);
        w16(&mut buf, 6, 4);
        buf[16..20].copy_from_slice(b"AAPL");
        assert_eq!(contexts(&buf).count(), 1);
    }

    #[test]
    fn a_name_or_payload_off_the_end_ends_the_walk() {
        let mut buf = [0u8; 32];
        w16(&mut buf, 4, 16);
        w16(&mut buf, 6, 64); // a name longer than the blob
        assert_eq!(contexts(&buf).count(), 0);

        let mut buf = [0u8; 32];
        w16(&mut buf, 4, 16);
        w16(&mut buf, 6, 4);
        w16(&mut buf, 10, 24);
        w32(&mut buf, 12, 4096); // a payload longer than the blob
        buf[16..20].copy_from_slice(b"AAPL");
        assert_eq!(contexts(&buf).count(), 0);

        // A blob too short to hold even one header.
        assert_eq!(contexts(&[0u8; 8]).count(), 0);
        assert_eq!(contexts(&[]).count(), 0);
    }

    #[test]
    fn write_one_refuses_a_buffer_it_would_overrun() {
        let mut small = [0u8; 24];
        assert_eq!(write_one(&mut small, b"AAPL", &[0; 8]), None);
        assert_eq!(write_one(&mut small, b"AAPL", &[0; 0]), Some(24));
    }
}
