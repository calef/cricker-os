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

/// Operations. The opcode is the low byte of the request word; the socket id is the next byte.
pub const OP_ATTACH_FRAME: u64 = 1; // SEND_CAP: delegate the shared frame for this socket id
pub const OP_OPEN_UDP: u64 = 2; // CALL: create a UDP socket, bind an ephemeral local port
pub const OP_OPEN_TCP: u64 = 3; // CALL: create a TCP socket
pub const OP_SENDTO: u64 = 4; // CALL: UDP send; dst in the frame header, payload in the frame
pub const OP_RECV: u64 = 5; // CALL: block until a datagram/segment arrives, write it to the frame
pub const OP_CONNECT: u64 = 6; // CALL: TCP connect to the frame's dst; reply the outcome
pub const OP_SEND: u64 = 7; // CALL: TCP send; payload in the frame
pub const OP_CLOSE: u64 = 8; // CALL: close the socket and drop its frame mapping

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

    /// **The request word round-trips.** `req` packs an opcode and a socket id into one word and
    /// the server unpacks both; if these three ever disagree, a client's `CLOSE` on socket 3
    /// becomes some other operation on some other socket, silently, with no error anywhere.
    #[test]
    fn a_request_word_round_trips_for_every_opcode_and_socket() {
        for op in [
            OP_ATTACH_FRAME,
            OP_OPEN_UDP,
            OP_OPEN_TCP,
            OP_SENDTO,
            OP_RECV,
            OP_CONNECT,
            OP_SEND,
            OP_CLOSE,
        ] {
            for sid in 0..MAX_SOCKETS as u64 {
                let w = req(op, sid);
                assert_eq!(req_op(w), op, "opcode lost for sid {sid}");
                assert_eq!(req_sid(w), sid, "socket id lost for op {op}");
            }
        }
    }

    /// Opcodes fit the byte the packing gives them. A ninth operation numbered 256 would alias
    /// `OP_ATTACH_FRAME` and shift the socket id, and the round-trip above would still pass for
    /// the eight that exist.
    #[test]
    fn every_opcode_fits_in_its_byte() {
        for op in [
            OP_ATTACH_FRAME,
            OP_OPEN_UDP,
            OP_OPEN_TCP,
            OP_SENDTO,
            OP_RECV,
            OP_CONNECT,
            OP_SEND,
            OP_CLOSE,
        ] {
            assert!(op <= 0xff, "opcode {op} does not fit the low byte");
        }
    }

    /// Opcodes are distinct. Two sharing a number is one operation silently performing another.
    #[test]
    fn opcodes_are_distinct() {
        let ops = [
            OP_ATTACH_FRAME,
            OP_OPEN_UDP,
            OP_OPEN_TCP,
            OP_SENDTO,
            OP_RECV,
            OP_CONNECT,
            OP_SEND,
            OP_CLOSE,
        ];
        for (i, a) in ops.iter().enumerate() {
            for b in &ops[i + 1..] {
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

    /// The payload plus its header fits a page, which is the frame the client was granted. If this
    /// ever exceeded it, a full-size send would write past the end of the shared frame.
    #[test]
    fn a_full_payload_fits_the_shared_frame() {
        assert!(
            OFF_PAYLOAD as usize + DATA_MAX <= 4096,
            "a full payload overruns the frame"
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
}
