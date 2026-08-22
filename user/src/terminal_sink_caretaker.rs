//! **The terminal, as a sink** (milestone 50's last remainder; notes/sink-protocol.md,
//! DECISIONS §67).
//!
//! The caretaker shape, applied outside the filesystem for the first time: hold a broad authority,
//! hand out a narrow view of it. This one holds the terminal endpoint and hands out a **sink**, so
//! it speaks the **sink contract** to its client and the **terminal contract** to what is behind
//! it, and a program whose output slot holds an endpoint to this process is writing to the screen
//! and cannot tell. What it keeps back is the reason the name says caretaker: that endpoint also
//! carries `OP_READLINE`, and a sink capability that can read the keyboard is not a sink
//! capability.
//!
//! ```text
//!   a declaring child ──sink_proto SEND──► terminal_sink_caretaker ──OP_PRINT CALL──► line_editor ──► console
//! ```
//!
//! # Why this is a process and not a line in the line editor
//!
//! The cheap move is to have `line_editor` serve the sink contract on the endpoint it already has: a
//! `SEND` arrives there with no reply capability, so it is trivially distinguishable from the
//! `CALL`s it serves. **That would be wrong**, and the reason is the whole of why this file exists.
//! That endpoint also carries `OP_READLINE`, and `WRITE` on an endpoint is the right to `CALL`, so a
//! child holding it as its output slot would hold **the terminal's keyboard**. A sink capability
//! that can read the keyboard is not a sink capability.
//!
//! This kernel offers no receive-on-a-set, so the terminal cannot serve two endpoints itself
//! (`fs_file_caretaker`'s note records the same gap and takes the same way out). Hence a second
//! process, holding both contracts, handing out only one of them.
//!
//! # And why it needed a new opcode
//!
//! `OP_WRITE` reads from **the client's output page**, and there is exactly one of those: init maps
//! a single frame into the terminal read-only and into the shell read/write. A second page-based
//! client would need a second frame and a page index in the request, which is `fs_proto`'s
//! one-page-two-clients problem (DECISIONS §55) arriving in a second contract. `OP_PRINT` carries
//! the bytes in the request's own words, so this process needs **no page at all**: it unpacks a sink
//! message and calls, and that is the entire program.
//!
//! # BUGS
//!
//! - **Eight bytes per call, so a sixteen-byte sink message is two round trips.** That is the
//!   terminal contract's request shape (`recv_cap` hands a server two data words), not a choice
//!   here. It costs one extra `CALL` per message on a path that is a person reading text.
//! - **No back pressure of its own.** Its client's `SEND` blocks until this process receives, and
//!   this process blocks in `CALL` until the terminal has the bytes, so flow control is the chain of
//!   rendezvous and there is nothing buffered anywhere. That is the same answer the rest of the sink
//!   contract gives and it is why there is no queue in here to overflow.
//! - **`OP_EOF` ends a writer, not the terminal.** One endpoint serves every declaring child in
//!   turn, so end-of-stream is a fact about one client and this loop simply continues. There is
//!   nothing here that could notice a client that died mid-message, and nothing that needs to.
//!
//! Name: ratified 2026-08-03 (calef), replacing `terminal_sink`. It holds the terminal endpoint,
//! which also carries `OP_READLINE`, and hands out a sink that cannot read, which is the caretaker
//! shape exactly; naming it one also proves that shape generalizes beyond `fs_`, where every other
//! member lives, so a reader stops taking caretaker for a filesystem concept. The full form keeps
//! both facts a shorter name would have forced a choice between: the client learns it gets a sink,
//! the auditor learns what it holds.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use line_editor::proto;
use user_rt::{call, recv};

/// Slot 0: the sink endpoint, held `READ`. Its clients hold `WRITE` on the same object and nothing
/// else, which is the whole point: what they can reach is this process, and this process only ever
/// prints.
const SINK: u64 = 0;

/// Slot 1: the terminal, held `WRITE` (which on an endpoint is the right to `CALL`). **Never
/// delegated onward**, and that is what a caretaker is for.
const TERM: u64 = 1;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    loop {
        let (w0, w1, w2) = recv(SINK);
        let mut buf = [0u8; sink_proto::INLINE_MAX];
        match sink_proto::unpack(w0, w1, w2, &mut buf) {
            sink_proto::Msg::Bytes(n) => print(&buf[..n]),
            // A writer finished. One endpoint serves every client in turn, so this says nothing
            // about the terminal and there is nothing to do but take the next message.
            sink_proto::Msg::Eof => {}
            // Not a message this contract can read. Saying so is better than silence, because the
            // alternative is a client that thinks it printed.
            sink_proto::Msg::Malformed => print(b"terminal_sink_caretaker: unreadable message\n"),
        }
    }
}

/// Print through the terminal, eight bytes a call. The reply means the bytes are on the wire.
fn print(bytes: &[u8]) {
    for chunk in bytes.chunks(8) {
        let mut w1 = 0u64;
        for (i, &b) in chunk.iter().enumerate() {
            w1 |= (b as u64) << (8 * i);
        }
        call(TERM, proto::req(proto::OP_PRINT, chunk.len() as u64), w1);
    }
}

user_rt::panic_handler!();
