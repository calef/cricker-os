#![cfg_attr(not(test), no_std)]
//! The socket contract wire format, shared by the net server (`net_stack`) and its clients (milestone
//! 30, piece 3 phase B; DECISIONS §25).
//!
//! A process holds a `Stack` endpoint capability and a per-connection **shared frame**. A socket is
//! a small integer **socket id** carried in the request word; the frame is the real granted
//! resource, delegated once. Every operation is one message on the endpoint:
//!
//! - `ATTACH_FRAME` is a `SEND_CAP` (it carries the frame capability, no reply).
//! - every other op is a `CALL` (two words out, a reply word back), the socket id packed into the
//!   request word beside the opcode.
//!
//! **Frame layout, pinned.** One data region, reused per operation, NOT a split TX/RX ring. The
//! phase-one contract is one *synchronous* exchange per `CALL` (the client blocks in the CALL while
//! the server drives the network), so a request's payload and its reply never coexist in the frame
//! and a single region is sufficient and simpler. A split TX/RX ring becomes necessary only with
//! asynchronous or streaming sockets, which the concurrency model defers (notes/net.md).
//!
//! ```text
//!   +0x000  u8[4]  dst_ip      destination address, octets (SENDTO / CONNECT)
//!   +0x004  u16    dst_port    destination port, little-endian (SENDTO / CONNECT)
//!   +0x006  u16    len         payload length, in for SEND*/out for RECV
//!   +0x008  ...    payload     up to DATA_MAX bytes
//! ```
//!
//! # The inbound half: a listener is not a connection (milestone 107)
//!
//! `LISTEN` and `ACCEPT` add the direction the contract did not have. The shape is **not** POSIX's,
//! where a listening socket and an accepted one are both file descriptors and the only difference
//! is which calls happen to work on them. Here they are two objects because they are two
//! authorities:
//!
//! - A **listener** is the authority to *receive connections on a port*. It is port-scoped, it is
//!   exclusive (two programs cannot both hold port 80), and **it carries no shared frame at all**,
//!   because no bytes ever cross on it. That last part is not a rule anyone imposed; it falls out
//!   of §25's decision that the frame is the real granted resource. A listener has nothing to grant.
//! - A **connection** is the authority to *speak with one peer*. It is 4-tuple-scoped, and it is
//!   exactly the object `CONNECT` already produced, reached the other way round.
//!
//! So `ACCEPT` takes the socket id to install the connection at, and refuses the listener's own id.
//! The client picks that id and must have attached a frame there first, which is the same thing an
//! outbound `OPEN_TCP` requires. This is the tree's existing habit of splitting authority by what a
//! holder can *do* rather than by what it names: `Frame` versus `DeviceFrame`, `WRITE` versus
//! `GRANT` on the same object.
//!
//! # Who binds the port: the listen grant
//!
//! Outbound needs no permission beyond the `Stack` capability, because an ephemeral local port is
//! allocated by the server and contended by nobody. **Inbound is different in kind:** a listening
//! port is a claim on an exclusive name in a shared namespace, which is the same thing that makes a
//! directory a capability rather than a path. A shell that could hand out "listen on 80" would be
//! handing out something categorically larger than "here is a connection".
//!
//! So the port is not the client's to choose freely. A net server is spawned with a **listen
//! grant**, an inclusive port range packed by [`listen_grant`], and refuses `LISTEN` outside it with
//! [`LISTEN_DENIED`]. [`NO_LISTEN_GRANT`] (the default, and what every outbound-only test spawns
//! with) means the server accepts nothing from anywhere: **inbound authority is granted, never
//! assumed.**
//!
//! **BUGS / limits, named here because this is where a reader meets the feature.** The grant's
//! granularity is the *`Stack` endpoint*, not the client, because an endpoint carries no sender
//! identity: two clients sharing one endpoint share its grant, and the server cannot tell them
//! apart. In this tree each stack endpoint has exactly one client today, so the distinction has no
//! bite yet, but a real multi-client net server needs the per-client minted endpoint that §25
//! already defers, and the grant then rides on that with no change to this wire format. The
//! backlog is **one connection deep** per listener (see `net_stack`'s `OP_ACCEPT`, which re-arms
//! immediately), so a second connection arriving while a first is un-accepted is refused by TCP
//! rather than queued.

/// Operations. The opcode is the low byte of the request word; the socket id is the next byte.
pub const OP_ATTACH_FRAME: u64 = 1; // SEND_CAP: delegate the shared frame for this socket id
pub const OP_OPEN_UDP: u64 = 2; // CALL: create a UDP socket, bind an ephemeral local port
pub const OP_OPEN_TCP: u64 = 3; // CALL: create a TCP socket
pub const OP_SENDTO: u64 = 4; // CALL: UDP send; dst in the frame header, payload in the frame
pub const OP_RECV: u64 = 5; // CALL: block until a datagram/segment arrives, write it to the frame
pub const OP_CONNECT: u64 = 6; // CALL: TCP connect to the frame's dst; reply the outcome
pub const OP_SEND: u64 = 7; // CALL: TCP send; payload in the frame
pub const OP_CLOSE: u64 = 8; // CALL: close the socket and drop its frame mapping
/// `CALL(req(OP_LISTEN, sid), port)`: bind `port` on socket id `sid` and start listening there.
/// Replies one of the [`LISTEN_GRANTED`] outcomes. Names provisional (milestone 107).
pub const OP_LISTEN: u64 = 9;
/// `CALL(req(OP_ACCEPT, lsid), target_sid)`: block until a connection arrives on the listener
/// `lsid`, then install it at socket id `target_sid`, which must already have a frame attached and
/// must not be `lsid`. Replies [`REP_OK`] or [`REP_ERR`]. The listener keeps listening.
pub const OP_ACCEPT: u64 = 10;

/// Pack an opcode and socket id into the request word.
pub const fn req(op: u64, sid: u64) -> u64 {
    op | (sid << 8)
}
pub const fn req_op(word: u64) -> u64 {
    word & 0xff
}
pub const fn req_sid(word: u64) -> u64 {
    (word >> 8) & 0xff
}

/// Reply words. Non-negative is success (RECV returns the length here); the connect outcomes are
/// their own small vocabulary so the client can tell "refused" from "connected".
pub const REP_OK: u64 = 0;
pub const CONNECT_ESTABLISHED: u64 = 0;
pub const CONNECT_REFUSED: u64 = 1; // the peer sent RST, or the connect otherwise failed/closed
/// A failure sentinel (an unknown socket id, a bad op, or a server-side timeout). High bit set so
/// it is never mistaken for a length or a connect outcome.
pub const REP_ERR: u64 = 1 << 32;

/// `LISTEN`'s outcomes, its own small vocabulary for the same reason `CONNECT` has one: a client
/// that cannot tell "I was never granted this port" from "somebody else holds it" cannot report
/// anything useful, and the two call for opposite responses (ask for authority, versus pick another
/// port).
pub const LISTEN_GRANTED: u64 = 0;
/// The port is outside this stack's [`listen_grant`]. **The capability refusal**: no retry, no
/// other port in range will help unless the caller was granted it.
pub const LISTEN_DENIED: u64 = 1;
/// The port is inside the grant but another socket already listens there. Exclusivity is the whole
/// reason a port is a grantable thing; this is what enforcing it looks like from the client side.
pub const LISTEN_IN_USE: u64 = 2;

/// No inbound authority at all, and the default a net server is spawned with.
pub const NO_LISTEN_GRANT: u64 = 0;

/// Pack an inclusive listen-port range into the one word a net server is spawned with. `lo == 0` is
/// [`NO_LISTEN_GRANT`] whatever `hi` says, because port 0 is not a port and a grant that starts at
/// "no port" grants nothing; that keeps the "no grant" case a single value a reader can recognise.
pub const fn listen_grant(lo: u16, hi: u16) -> u64 {
    if lo == 0 || hi < lo {
        return NO_LISTEN_GRANT;
    }
    (lo as u64) | ((hi as u64) << 16)
}

/// May a holder of a stack carrying `grant` listen on `port`? The whole authority check, in one
/// place both the server and its tests read, so a server-side widening cannot pass a test that
/// keeps its own copy of the range.
pub const fn grant_allows(grant: u64, port: u16) -> bool {
    let lo = (grant & 0xffff) as u16;
    let hi = ((grant >> 16) & 0xffff) as u16;
    lo != 0 && port >= lo && port <= hi
}

/// Frame header offsets.
pub const OFF_DST_IP: u64 = 0x000;
pub const OFF_DST_PORT: u64 = 0x004;
pub const OFF_LEN: u64 = 0x006;
pub const OFF_PAYLOAD: u64 = 0x008;

/// The most sockets one client may hold at once. Small and fixed; the shared frame is the real
/// per-socket resource.
pub const MAX_SOCKETS: usize = 4;

/// The largest payload the frame carries (a 4 KiB frame minus the header).
pub const DATA_MAX: usize = 4096 - OFF_PAYLOAD as usize;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every opcode the contract defines, in one place. It was written out three times before
    /// milestone 107 added two more, and a list repeated per property is a list that will one day
    /// be missing an entry in exactly the property that would have caught it.
    const OPS: &[u64] = &[
        OP_ATTACH_FRAME,
        OP_OPEN_UDP,
        OP_OPEN_TCP,
        OP_SENDTO,
        OP_RECV,
        OP_CONNECT,
        OP_SEND,
        OP_CLOSE,
        OP_LISTEN,
        OP_ACCEPT,
    ];

    /// **The request word round-trips.** `req` packs an opcode and a socket id into one word and
    /// the server unpacks both; if these three ever disagree, a client's `CLOSE` on socket 3
    /// becomes some other operation on some other socket, silently, with no error anywhere.
    #[test]
    fn a_request_word_round_trips_for_every_opcode_and_socket() {
        for &op in OPS {
            for sid in 0..MAX_SOCKETS as u64 {
                let w = req(op, sid);
                assert_eq!(req_op(w), op, "opcode lost for sid {sid}");
                assert_eq!(req_sid(w), sid, "socket id lost for op {op}");
            }
        }
    }

    /// Opcodes fit the byte the packing gives them. An operation numbered 256 would alias
    /// `OP_ATTACH_FRAME` and shift the socket id, and the round-trip above would still pass for
    /// every opcode that exists.
    #[test]
    fn every_opcode_fits_in_its_byte() {
        for &op in OPS {
            assert!(op <= 0xff, "opcode {op} does not fit the low byte");
        }
    }

    /// Opcodes are distinct. Two sharing a number is one operation silently performing another.
    #[test]
    fn opcodes_are_distinct() {
        for (i, a) in OPS.iter().enumerate() {
            for b in &OPS[i + 1..] {
                assert_ne!(a, b, "two opcodes share a number");
            }
        }
    }

    /// **A socket id must fit the byte it rides in**, or `MAX_SOCKETS` promises more sockets than
    /// the wire format can name and the highest ones alias the lowest.
    #[test]
    fn every_socket_id_fits_the_field() {
        assert!(MAX_SOCKETS as u64 <= 256, "socket ids do not fit one byte");
        let top = MAX_SOCKETS as u64 - 1;
        assert_eq!(req_sid(req(OP_SEND, top)), top);
    }

    /// The shared frame's header fields do not overlap, and the payload starts after all of them.
    /// An overlap here means a destination port written over an address, which would send bytes to
    /// the wrong host rather than failing.
    #[test]
    fn the_frame_header_fields_do_not_overlap() {
        let mut offs: [u64; 3] = [OFF_DST_IP, OFF_DST_PORT, OFF_LEN];
        offs.sort_unstable();
        for w in offs.windows(2) {
            assert_ne!(w[0], w[1], "two header fields share an offset");
        }
        for o in offs {
            assert!(o < OFF_PAYLOAD, "header field {o} runs into the payload");
        }
    }

    /// The payload is exactly the rest of the frame: the header, then bytes to the last one on the
    /// page. Larger and a full-size send writes past the end of the frame the client was granted;
    /// smaller and part of a granted page is unreachable, which nothing anywhere would report.
    #[test]
    fn a_full_payload_fills_the_shared_frame_exactly() {
        assert_eq!(
            OFF_PAYLOAD as usize + DATA_MAX,
            4096,
            "the payload is not the whole frame minus its header"
        );
    }

    /// The connect outcomes are distinguishable from each other and from the generic reply codes,
    /// which is the whole reason they have their own vocabulary.
    #[test]
    fn connect_outcomes_are_their_own_vocabulary() {
        assert_ne!(CONNECT_ESTABLISHED, CONNECT_REFUSED);
        assert_ne!(CONNECT_ESTABLISHED, REP_ERR);
        assert_ne!(CONNECT_REFUSED, REP_ERR);
    }

    /// The same for `LISTEN`'s three outcomes, and for the same reason: a client that cannot tell
    /// a refusal of *authority* from a collision on a port cannot do the right thing about either.
    #[test]
    fn listen_outcomes_are_their_own_vocabulary() {
        let outcomes = [LISTEN_GRANTED, LISTEN_DENIED, LISTEN_IN_USE, REP_ERR];
        for (i, a) in outcomes.iter().enumerate() {
            for b in &outcomes[i + 1..] {
                assert_ne!(a, b, "two listen outcomes share a value");
            }
        }
    }

    /// **A grant admits exactly its range and nothing else.** This is the authority check itself,
    /// so an off-by-one here is a server binding a port it was never granted, which no other test
    /// in the tree would see: the on-device gate asks for one allowed port and one denied one.
    #[test]
    fn a_listen_grant_admits_its_range_and_refuses_everything_outside_it() {
        let g = listen_grant(7778, 7780);
        assert!(!grant_allows(g, 7777), "one below the range was allowed");
        for p in 7778..=7780 {
            assert!(grant_allows(g, p), "port {p} inside the range was refused");
        }
        assert!(!grant_allows(g, 7781), "one above the range was allowed");
        assert!(
            !grant_allows(g, 80),
            "a well-known port outside was allowed"
        );

        // A single-port grant is the common case (one server, one port) and the tightest one.
        let one = listen_grant(80, 80);
        assert!(grant_allows(one, 80));
        assert!(!grant_allows(one, 81));
    }

    /// **No grant means no port.** The default a server is spawned with must admit nothing, or
    /// "inbound authority is granted, never assumed" is a sentence in a doc comment and not a
    /// property of the system. Port 0 is refused under every grant, including one that names it,
    /// because it is not a port anything can listen on.
    #[test]
    fn the_default_grant_admits_no_port_at_all() {
        for p in [0u16, 1, 80, 7778, 65535] {
            assert!(
                !grant_allows(NO_LISTEN_GRANT, p),
                "port {p} was allowed with no grant"
            );
        }
        assert_eq!(listen_grant(0, 65535), NO_LISTEN_GRANT);
        assert!(!grant_allows(listen_grant(0, 65535), 7778));
        assert!(!grant_allows(listen_grant(1, 65535), 0));
        // An inverted range is a mistake at the spawn site, and the safe reading of a mistake in a
        // grant is "grants nothing", never "grants the ports between them in the other order".
        assert_eq!(listen_grant(7780, 7778), NO_LISTEN_GRANT);
    }

    /// A grant round-trips through the one word a spawn argument carries, up to the top of the
    /// port space. It rides in a `u64` beside nothing else, so the only way to lose a bit is to
    /// pack it wrong.
    #[test]
    fn a_grant_survives_the_word_it_is_spawned_with() {
        for (lo, hi) in [
            (1u16, 65535u16),
            (49152, 65535),
            (65535, 65535),
            (7778, 7778),
        ] {
            let g = listen_grant(lo, hi);
            assert!(grant_allows(g, lo), "the low end was lost for {lo}..={hi}");
            assert!(grant_allows(g, hi), "the high end was lost for {lo}..={hi}");
        }
    }
}
