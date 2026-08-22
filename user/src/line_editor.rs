//! **`line_editor`: the line discipline as a userspace component** (milestone 28).
//!
//! The layer Unix builds into the kernel as the tty line discipline is here a process. It sits
//! between the raw input driver and the application, and between the application and the console
//! server, on plain endpoints:
//!
//! ```text
//!   input driver ──OP_BYTES──►┌──────────┐──text──► console server ──► UART
//!                             │ line_editor │
//!        application ◄─lines──└──────────┘◄──OP_WRITE / OP_READLINE── application
//! ```
//!
//! Nobody in that picture knows what they are talking to. The input driver holds "an endpoint I
//! send wire bytes to"; the application holds "an endpoint that prints and reads lines"; the
//! console server holds "an endpoint requests arrive on". Endpoint-only naming (notes/
//! ipc-naming.md) is what makes the discipline swappable: rewire the endpoints and no client can
//! tell, which is milestone 23's hot-swap claim in component form.
//!
//! The IPC protocol is the terminal contract, notes/terminal-contract.md; the framing constants
//! are `line_editor::proto`. Every request is a `CALL`, served through `RECV_CAP`, answered through
//! the kernel's one-shot Reply capability (DECISIONS §12). That choice is what makes the server
//! deadlock-free: a READLINE with no line ready is *held* (the reply capability parked in a
//! slot) while the server keeps serving, so a client blocked printing can never interlock with
//! a server blocked delivering. The editing itself lives in the host-tested `line_editor` crate;
//! this file is only wiring: words in, pages copied, words out.
//!
//! Its whole authority: the terminal endpoint (slot 0, RECV), the console server's request and
//! reply endpoints (slots 1 and 2), the console's shared page (write), the client's output page
//! (read) and input page (write). No UART, no interrupt: the discipline touches no hardware,
//! which is exactly why it did not exist until the drivers did.
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39, landed by milestone 46) for the word and again
//! 2026-08-01 (milestone 63) for the spelling, replacing `termd` and then `lineedit`. Refused
//! `termd` (the `-d` claim) and `linedisc`, the correct Unix term of art, which is the second half
//! of §39: calef did not recognise the phrase, and he built this system, which is evidence about
//! the name rather than about him.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use line_editor::{Event, LINE_MAX, LineDisc, PROMPT_MAX, Sink, proto};
use user_rt::{recv, recv_cap, reply, send};

/// The terminal endpoint (slot 0): clients CALL requests here; we serve it with `RECV_CAP`.
const TERM: u64 = 0;
/// The console server's request endpoint (slot 1): we SEND a byte count on it.
const CONREQ: u64 = 1;
/// The console server's reply endpoint (slot 2): we RECV its ack here. The console speaks the
/// pre-§12 two-endpoint protocol (it serves with plain RECV, which cannot answer a CALL), so
/// this hop is SEND+RECV, not CALL. Safe with one console client, and `line_editor` is that client.
const CONREP: u64 = 2;

/// The console's shared page, mapped read/write: we fill it, the console prints it. The same
/// frame the console server reads at its own `SHARED_VA`; must match the wiring (init).
const CONOUT_VA: u64 = 0x0060_0000;
/// The client's output page, mapped read-only: `OP_WRITE` text and `OP_READLINE` prompts arrive
/// here, written by the client at its own VA for this frame.
const APP_OUT_VA: u64 = 0x0080_0000;
/// The client's input page, mapped read/write: completed lines are delivered here for the
/// client to read at its own VA for this frame.
const APP_IN_VA: u64 = 0x0090_0000;

const PAGE: usize = 4096;

/// How many completed lines the type-ahead queue holds. A user typing while the application is
/// busy loses nothing until this overflows, and then the newest line is dropped with a bell,
/// which is what a real tty's flooded input queue does too.
const QUEUE: usize = 4;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    let mut disc = LineDisc::new();
    let mut con = Con { used: 0 };
    let mut queue = LineQueue::new();
    // A parked READLINE: the slot holding the caller's one-shot Reply capability. The caller
    // stays blocked (that is CALL's contract) while we serve everyone else.
    let mut pending: Option<u64> = None;
    // How many ^C we have seen since boot. The shell reads this (OP_INTRCOUNT) to sense interrupts
    // while a foreground job runs and no read is parked to fail (DECISIONS §24). A
    // monotonic counter, so the shell learns of a ^C by the count advancing, never missing one.
    let mut intr_count: u64 = 0;

    loop {
        let (w0, slot, w1) = recv_cap(TERM);
        if slot == abi::endpoint::NO_CAP {
            // A plain SEND slipped in; the contract says CALL. With no reply capability there
            // is nobody to answer, so the only honest move is to drop it.
            continue;
        }
        match proto::op(w0) {
            proto::OP_BYTES => {
                let n = proto::len(w0).min(8);
                for i in 0..n {
                    let b = (w1 >> (8 * i)) as u8;
                    match disc.feed(b, &mut con) {
                        Event::Line => queue.push(disc.line(), 0, &mut con),
                        Event::Eof => queue.push(&[], proto::FLAG_EOF, &mut con),
                        Event::Interrupt => {
                            // ^C: the discipline discarded the edit line. We discard the type-ahead
                            // and fail a pending read (case 1, the shell is at the prompt). And we
                            // bump the interrupt count, which the shell polls (OP_INTRCOUNT) when a
                            // foreground job is running and no read is parked (case 2,
                            // DECISIONS §24). One ^C, both effects; whichever the shell is watching.
                            intr_count = intr_count.wrapping_add(1);
                            queue.clear();
                            if let Some(p) = pending.take() {
                                reply(p, 0, proto::FLAG_INTERRUPTED);
                            }
                        }
                        Event::None => {}
                    }
                }
                con.flush();
                reply(slot, 0, 0);
                deliver(&mut queue, &mut pending);
            }
            proto::OP_WRITE => {
                let len = proto::len(w0).min(PAGE);
                // Stream the client's text through newline expansion into the console page, in
                // small chunks so the expansion never needs a buffer the size of its input.
                let mut done = 0;
                let mut chunk = [0u8; 256];
                while done < len {
                    let n = (len - done).min(chunk.len());
                    copy_in(APP_OUT_VA, done, &mut chunk[..n]);
                    line_editor::expand_output(&chunk[..n], &mut con);
                    done += n;
                }
                con.flush();
                reply(slot, len as u64, 0);
            }
            proto::OP_READLINE => {
                if pending.is_some() {
                    // One outstanding read per terminal (the contract). A second caller while
                    // one is parked is a protocol violation; refuse it loudly.
                    reply(slot, proto::BAD_REQUEST, 0);
                    continue;
                }
                let plen = proto::len(w0).min(PROMPT_MAX);
                let mut prompt = [0u8; PROMPT_MAX];
                copy_in(APP_OUT_VA, 0, &mut prompt[..plen]);
                disc.start_line(&prompt[..plen], &mut con);
                con.flush();
                pending = Some(slot);
                deliver(&mut queue, &mut pending);
            }
            proto::OP_PRINT => {
                // **A second writer, with no second page** (DECISIONS §67, notes/sink-protocol.md).
                // The bytes are in the request's own words, so this client needs no frame mapped
                // here and no frame of its own; `OP_WRITE` above reads the *one* page init maps in,
                // and a second page-based client would need this contract to grow a page index.
                //
                // Through the same `expand_output` as `OP_WRITE`, so a newline from a sink adapter
                // becomes a carriage return and a newline exactly as one from the shell does. Two
                // writers, one terminal, one set of manners.
                let len = proto::len(w0).min(8);
                let bytes = w1.to_le_bytes();
                line_editor::expand_output(&bytes[..len], &mut con);
                con.flush();
                reply(slot, len as u64, 0);
            }
            proto::OP_INTRCOUNT => {
                // The shell's ^C sensor: reply immediately with the running count. Never blocks, so
                // the shell can busy-poll it while watching a foreground job (DECISIONS §24).
                reply(slot, intr_count, 0);
            }
            _ => {
                reply(slot, proto::BAD_REQUEST, 0);
            }
        }
    }
}

/// If a read is parked and a line is queued, marry them: copy the line into the client's input
/// page and wake the caller through its reply capability.
fn deliver(queue: &mut LineQueue, pending: &mut Option<u64>) {
    if pending.is_none() || queue.count == 0 {
        return;
    }
    let (len, flags) = queue.pop_front_into(APP_IN_VA);
    let p = pending.take().unwrap();
    reply(p, len as u64, flags);
}

/// The console channel: a page we fill and a message that says how much. Batches everything
/// the engine emits between flushes, so one keystroke's echo (or one ^L repaint) is one IPC
/// round trip, not one per escape sequence.
struct Con {
    used: usize,
}

impl Con {
    fn flush(&mut self) {
        if self.used > 0 {
            send(CONREQ, self.used as u64, 0, 0);
            recv(CONREP); // the ack means the page is ours to refill
            self.used = 0;
        }
    }
}

impl Sink for Con {
    fn put(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let space = PAGE - self.used;
            let n = bytes.len().min(space);
            for (i, &b) in bytes[..n].iter().enumerate() {
                // SAFETY: the console page is mapped read/write at CONOUT_VA.
                unsafe { core::ptr::write_volatile((CONOUT_VA as *mut u8).add(self.used + i), b) };
            }
            self.used += n;
            bytes = &bytes[n..];
            if self.used == PAGE {
                self.flush();
            }
        }
    }
}

/// Read `dst.len()` bytes from the page at `base`, starting at `offset`.
fn copy_in(base: u64, offset: usize, dst: &mut [u8]) {
    for (i, b) in dst.iter_mut().enumerate() {
        // SAFETY: the client pages are mapped (read-only is enough to read) and offset+len is
        // bounded by PAGE by every caller.
        *b = unsafe { core::ptr::read_volatile((base as *const u8).add(offset + i)) };
    }
}

/// The type-ahead queue: completed lines waiting for a reader, FIFO, fixed storage.
struct LineQueue {
    lines: [([u8; LINE_MAX], usize, u64); QUEUE],
    head: usize,
    count: usize,
}

impl LineQueue {
    fn new() -> Self {
        LineQueue {
            lines: [([0; LINE_MAX], 0, 0); QUEUE],
            head: 0,
            count: 0,
        }
    }

    fn push(&mut self, line: &[u8], flags: u64, con: &mut Con) {
        if self.count == QUEUE {
            con.put(&[0x07]); // full: drop the newest, audibly
            return;
        }
        let slot = (self.head + self.count) % QUEUE;
        let n = line.len().min(LINE_MAX);
        self.lines[slot].0[..n].copy_from_slice(&line[..n]);
        self.lines[slot].1 = n;
        self.lines[slot].2 = flags;
        self.count += 1;
    }

    fn clear(&mut self) {
        self.count = 0;
    }

    /// Pop the oldest line into the page at `base`; returns (length, flags).
    fn pop_front_into(&mut self, base: u64) -> (usize, u64) {
        let (line, len, flags) = &self.lines[self.head];
        for (i, &b) in line[..*len].iter().enumerate() {
            // SAFETY: the client input page is mapped read/write at `base`.
            unsafe { core::ptr::write_volatile((base as *mut u8).add(i), b) };
        }
        let out = (*len, *flags);
        self.head = (self.head + 1) % QUEUE;
        self.count -= 1;
        out
    }
}

user_rt::panic_handler!();
