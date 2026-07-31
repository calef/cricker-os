//! The socket contract wire format, shared by the net server (`netstack`) and its clients (milestone
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
