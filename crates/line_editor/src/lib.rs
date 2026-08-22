//! **The line discipline engine: a sans-IO editor for a serial terminal** (milestone 28).
//!
//! This crate is the pure logic of the layer Unix calls the tty line discipline: byte in,
//! echo bytes out, completed lines out. It performs no IO and knows nothing about IPC, UARTs,
//! or endpoints; the `line_editor` userspace component feeds it one byte at a time and forwards
//! whatever it emits. That split is DECISIONS §7 applied: the editing rules are host-tested in
//! milliseconds (with a small terminal model, see the tests), and only the wiring needs QEMU.
//!
//! # Examples
//!
//! Typing a line, editing it, and getting it back. The [`Sink`] is where echo goes; here it is a
//! `Vec`, and in the running system it is an endpoint.
//!
//! ```
//! use line_editor::{Event, LineDisc, Sink};
//!
//! struct Echo(Vec<u8>);
//! impl Sink for Echo {
//!     fn put(&mut self, bytes: &[u8]) {
//!         self.0.extend_from_slice(bytes);
//!     }
//! }
//!
//! let mut out = Echo(Vec::new());
//! let mut ld = LineDisc::new();
//! ld.start_line(b"$ ", &mut out);
//!
//! // Type "13", then fix it: ^B moves the cursor left one, onto the 3, and typing 2 **inserts**
//! // before it rather than overwriting it.
//! for b in b"13" {
//!     assert_eq!(ld.feed(*b, &mut out), Event::None);
//! }
//! ld.feed(0x02, &mut out); // ^B
//! ld.feed(b'2', &mut out);
//!
//! // Carriage return ends the line. The caller reads it before feeding the next byte.
//! assert_eq!(ld.feed(b'\r', &mut out), Event::Line);
//! assert_eq!(ld.line(), b"123");
//!
//! // The prompt was echoed, and so was everything typed.
//! assert!(out.0.starts_with(b"$ "));
//! ```
//!
//! Three control characters mean things a caller has to act on rather than echo, and `^D`'s double
//! duty is the one worth demonstrating: on an empty line it ends input, and on a non-empty line it is
//! an ordinary delete. That is Unix's behaviour, and it is a decision rather than an accident.
//!
//! ```
//! # use line_editor::{Event, LineDisc, Sink};
//! # struct Echo(Vec<u8>);
//! # impl Sink for Echo {
//! #     fn put(&mut self, bytes: &[u8]) { self.0.extend_from_slice(bytes); }
//! # }
//! let mut out = Echo(Vec::new());
//! let mut ld = LineDisc::new();
//! ld.start_line(b"$ ", &mut out);
//!
//! // ^D on an empty line: end of input.
//! assert_eq!(ld.feed(0x04, &mut out), Event::Eof);
//!
//! // ^C discards whatever was in progress.
//! ld.start_line(b"$ ", &mut out);
//! for b in b"rm -rf" {
//!     ld.feed(*b, &mut out);
//! }
//! assert_eq!(ld.feed(0x03, &mut out), Event::Interrupt);
//!
//! // On a non-empty line, ^D deletes at the cursor instead. ^A goes to the start first.
//! ld.start_line(b"$ ", &mut out);
//! for b in b"xls" {
//!     ld.feed(*b, &mut out);
//! }
//! ld.feed(0x01, &mut out); // ^A: home
//! assert_eq!(ld.feed(0x04, &mut out), Event::None); // deletes the 'x', does NOT end input
//! assert_eq!(ld.feed(b'\r', &mut out), Event::Line);
//! assert_eq!(ld.line(), b"ls");
//! ```
//!
//! An LF straight after a CR is swallowed, which is what makes one engine serve both an interactive
//! terminal (CR) and piped CRLF text without the caller knowing which it has:
//!
//! ```
//! # use line_editor::{Event, LineDisc, Sink};
//! # struct Echo(Vec<u8>);
//! # impl Sink for Echo {
//! #     fn put(&mut self, bytes: &[u8]) { self.0.extend_from_slice(bytes); }
//! # }
//! let mut out = Echo(Vec::new());
//! let mut ld = LineDisc::new();
//! ld.start_line(b"", &mut out);
//!
//! ld.feed(b'a', &mut out);
//! assert_eq!(ld.feed(b'\r', &mut out), Event::Line);
//! assert_eq!(ld.line(), b"a");
//!
//! // The LF of the CRLF pair is not a second, empty line.
//! assert_eq!(ld.feed(b'\n', &mut out), Event::None);
//! ```
//!
//! # What it implements
//!
//! - Insertion and deletion at a cursor, with minimal echo for the common case (typing at the
//!   end of the line) and a full input-region redraw for everything else.
//! - Cursor movement: left/right/home/end, as control characters (^B ^F ^A ^E) and as ANSI
//!   sequences (CSI D/C/H/F, SS3 variants, and the vt220 `~` forms).
//! - Kill and yank: ^K (to end), ^U (to start), ^W (word back), ^Y (insert the kill buffer).
//! - History: an in-ring of the last [`HIST`] non-empty lines, browsed with up/down arrows;
//!   the in-progress line is stashed and restored when you browse back past the newest entry.
//! - Line endings: CR or LF ends a line; an LF immediately after a CR is swallowed, so both
//!   interactive terminals (CR) and piped text (LF, CRLF) work.
//! - ^C discards the line in progress ([`Event::Interrupt`]); ^D on an empty line is
//!   end-of-input ([`Event::Eof`]), on a non-empty line it deletes at the cursor, exactly the
//!   Unix double duty.
//! - ^L clears the screen and repaints the prompt and the line.
//!
//! # What it deliberately does not do
//!
//! - **No terminal size tracking.** The redraw math assumes the line fits one terminal row.
//!   `noline` probes the terminal with a cursor-position query at startup; a line discipline is
//!   always on and cannot block on a terminal that may not answer (a piped boot script never
//!   would). A wrapped long line therefore redraws incorrectly past the margin. Recorded in
//!   notes/terminal-contract.md as an honest limit; [`LINE_MAX`] keeps it rare.
//! - **No tab completion.** Completion needs the command namespace, which is the application's
//!   knowledge, not the terminal's. Tab is ignored.
//! - **No output-side state.** Application output passes through the component untouched except
//!   for newline translation, which is [`expand_output`], a free function on purpose: output
//!   processing shares no state with input editing (Unix tangles these; see notes/tcb.md's
//!   mistake-catalog reading of the tty layer).
//!
//! The echo this engine emits and the sequences it understands are the terminal contract's
//! wire half; the IPC framing is `proto`. Both documented in notes/terminal-contract.md.
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39, landed by milestone 46) for the word and again
//! 2026-08-01 (milestone 63) for the spelling, replacing `linedisc` and then `lineedit`. Refused
//! `linedisc` (the correct Unix term of art, which the person who built this system did not
//! recognise, and which imports vocabulary from exactly the model this OS rejects),
//! `line_discipline` (overclaims: that term covers echo, canonical mode, signals and flow control,
//! and this crate is narrower) and `line_edit` (a verb phrase where its sibling `video_terminal` is
//! a noun).

#![no_std]
// milestone 68's doc ratchet: every public item in this crate is documented, and
// `script/lint`'s -D warnings keeps it that way. See notes/doc-coverage.md for the
// crates that are not there yet.
#![warn(missing_docs)]

/// The IPC framing of the terminal contract (notes/terminal-contract.md): opcodes, flags, and
/// limits shared by the discipline server, its clients, and the kernel-side tests. This is a
/// **userspace protocol**, not kernel ABI: the kernel routes these words without reading them.
pub mod proto {
    /// Request opcode, in bits 63:56 of the first `CALL` word. The low 32 bits carry a length
    /// or count; bits 55:32 are reserved and must be zero.
    pub const OP_SHIFT: u32 = 56;
    /// Application → terminal: print `len` bytes from the client's output page. Reply when the
    /// bytes are on the wire: r0 = bytes consumed.
    pub const OP_WRITE: u64 = 1;
    /// Application → terminal: read one line. The low bits carry a prompt length (0 for none);
    /// the prompt bytes are in the client's output page. The reply comes when a line is ready:
    /// r0 = line length (the bytes are in the client's input page), r1 = [`FLAG_EOF`] /
    /// [`FLAG_INTERRUPTED`] or 0.
    pub const OP_READLINE: u64 = 2;
    /// Input driver → terminal: `count` (1..=8) raw wire bytes, packed little-endian in the
    /// second word. Replied immediately (r0 = 0); the rendezvous is the flow control.
    pub const OP_BYTES: u64 = 3;
    /// Application → terminal: how many `^C` has the terminal seen since boot? Replied immediately
    /// (r0 = the running count). This is the shell's `^C` sensor while a foreground job runs and no
    /// read is parked to fail: the shell polls this and drives its two-tier interrupt escalation
    /// from the count's advance (DECISIONS §24). Deliberately a poll, not a delivered
    /// signal: there is no non-blocking receive, so a busy-poll with `yield` is how the shell watches
    /// two things (the job and `^C`) at once until the blocking notification primitive arrives.
    pub const OP_INTRCOUNT: u64 = 4;
    /// Adapter → terminal: print `len` (1..=8) bytes carried **in registers**, packed
    /// little-endian in the second word. Replied when the bytes are on the wire: r0 = bytes
    /// consumed, exactly as [`OP_WRITE`] answers.
    ///
    /// Eight, not sixteen, and that is the request shape rather than a choice: a served request
    /// here arrives through `recv_cap`, which hands the server the reply capability and **two** data
    /// words. [`OP_BYTES`] carries eight for the same reason, from the other direction. So a
    /// sixteen-byte `sink_proto` message is two of these, which is the honest cost of a second
    /// writer that needs no page.
    ///
    /// # Why this exists when `OP_WRITE` already prints
    ///
    /// [`OP_WRITE`] reads from **the client's output page**, and there is one of those: init maps a
    /// single frame into the terminal read-only and into the shell read/write. A second printing
    /// client would need a second frame, and the terminal would have to be told which one a request
    /// meant, which is a protocol change with a page-index in it. `fs_proto` has the same shape and
    /// the same problem (DECISIONS §55: two client processes staging into one page do not compose,
    /// and that is why the file behind a `>` is the shell itself).
    ///
    /// Register-only sidesteps it entirely, and the sixteen bytes are not a compromise: they are the
    /// three-word fastpath and the exact payload of a `sink_proto` message, so an adapter that turns
    /// the sink contract into terminal output is one unpack and one `CALL` with **no page at all**.
    /// [`OP_BYTES`] already proved the shape in the other direction, for the input driver.
    ///
    /// So the terminal gets a second *writer* without a second page and without a second client
    /// convention, which is what a sink adapter needed (notes/sink-protocol.md).
    pub const OP_PRINT: u64 = 5;

    /// Pack a request's first word from an opcode and a length/count.
    pub const fn req(op: u64, len: u64) -> u64 {
        (op << OP_SHIFT) | (len & 0xffff_ffff)
    }
    /// The opcode of a request word.
    pub const fn op(w0: u64) -> u64 {
        w0 >> OP_SHIFT
    }
    /// The length/count of a request word.
    pub const fn len(w0: u64) -> usize {
        (w0 & 0xffff_ffff) as usize
    }

    /// READLINE reply flag: end of input (^D on an empty line). The line length is 0.
    pub const FLAG_EOF: u64 = 1 << 0;
    /// READLINE reply flag: the read was interrupted (^C). The line length is 0. This is the
    /// contract's hook for interrupt routing; see design/interrupt-routing.md.
    pub const FLAG_INTERRUPTED: u64 = 1 << 1;

    /// The reply to a request whose opcode the terminal does not implement: r0 = this, r1 = 0.
    /// A sentinel rather than silence, so a confused client fails fast instead of hanging.
    pub const BAD_REQUEST: u64 = u64::MAX;
}

/// Where echo bytes go. The server implements this over its console channel; the tests
/// implement it over a terminal model.
pub trait Sink {
    /// Write echo bytes out. Called synchronously from the byte-feeding methods below, so a
    /// `Sink` that blocks blocks the whole discipline.
    fn put(&mut self, bytes: &[u8]);
}

/// The longest line the discipline will assemble. Longer input is refused with a bell.
pub const LINE_MAX: usize = 256;
/// How many history entries the ring holds.
pub const HIST: usize = 8;
/// The longest prompt the discipline will repaint. Longer prompts are truncated.
pub const PROMPT_MAX: usize = 64;

const BS: u8 = 0x08;
const BEL: u8 = 0x07;
const ESC: u8 = 0x1b;

/// What one input byte amounted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Nothing the caller needs to act on (echo may still have been emitted).
    None,
    /// A line is complete; read it with [`LineDisc::line`] before feeding the next byte.
    Line,
    /// ^D on an empty line: end of input.
    Eof,
    /// ^C: the line in progress was discarded.
    Interrupt,
}

/// The ANSI parser's state. `Esc` has seen 0x1b; `Csi` is inside `ESC [`, accumulating numeric
/// parameters; `Ss3` is inside `ESC O` (application-mode cursor keys send SS3, not CSI).
enum EscState {
    Idle,
    Esc,
    Csi { params: [u16; 2], n: usize },
    Ss3,
}

/// The editing engine. One per terminal; a few KiB of fixed buffers, no allocation.
pub struct LineDisc {
    buf: [u8; LINE_MAX],
    len: usize,
    cur: usize,
    /// The completed line, snapshotted when [`Event::Line`] is returned.
    done: [u8; LINE_MAX],
    done_len: usize,
    kill: [u8; LINE_MAX],
    kill_len: usize,
    hist: [([u8; LINE_MAX], usize); HIST],
    hist_count: usize,
    hist_next: usize,
    /// `Some(k)`: showing the k-th most recent history entry (k in `1..=hist_count`).
    browse: Option<usize>,
    stash: ([u8; LINE_MAX], usize, usize),
    prompt: [u8; PROMPT_MAX],
    prompt_len: usize,
    esc: EscState,
    last_was_cr: bool,
}

impl Default for LineDisc {
    fn default() -> Self {
        Self::new()
    }
}

impl LineDisc {
    /// An empty discipline: no line in progress, no history, prompt unset.
    pub fn new() -> Self {
        LineDisc {
            buf: [0; LINE_MAX],
            len: 0,
            cur: 0,
            done: [0; LINE_MAX],
            done_len: 0,
            kill: [0; LINE_MAX],
            kill_len: 0,
            hist: [([0; LINE_MAX], 0); HIST],
            hist_count: 0,
            hist_next: 0,
            browse: None,
            stash: ([0; LINE_MAX], 0, 0),
            prompt: [0; PROMPT_MAX],
            prompt_len: 0,
            esc: EscState::Idle,
            last_was_cr: false,
        }
    }

    /// The completed line, valid after [`Event::Line`] until the next one.
    pub fn line(&self) -> &[u8] {
        &self.done[..self.done_len]
    }

    /// Begin a read: remember `prompt` (for repaints) and paint it, followed by whatever the
    /// user has already typed ahead. Called by the server when a READLINE request arrives.
    pub fn start_line(&mut self, prompt: &[u8], out: &mut impl Sink) {
        let n = prompt.len().min(PROMPT_MAX);
        self.prompt[..n].copy_from_slice(&prompt[..n]);
        self.prompt_len = n;
        out.put(&self.prompt[..n]);
        out.put(&self.buf[..self.len]);
        csi_left(out, self.len - self.cur);
    }

    /// Feed one wire byte; echo goes to `out`. See [`Event`] for what came of it.
    pub fn feed(&mut self, b: u8, out: &mut impl Sink) -> Event {
        // A bare LF right after a CR is the second half of a CRLF, not a second Enter.
        let was_cr = core::mem::replace(&mut self.last_was_cr, false);
        if b == b'\n' && was_cr {
            return Event::None;
        }

        match core::mem::replace(&mut self.esc, EscState::Idle) {
            EscState::Idle => {}
            EscState::Esc => {
                match b {
                    b'[' => {
                        self.esc = EscState::Csi {
                            params: [0; 2],
                            n: 0,
                        }
                    }
                    b'O' => self.esc = EscState::Ss3,
                    _ => {} // an escape we do not speak; drop it and the introducer
                }
                return Event::None;
            }
            EscState::Csi { mut params, mut n } => {
                match b {
                    b'0'..=b'9' => {
                        let i = n.min(1);
                        params[i] = params[i].saturating_mul(10) + (b - b'0') as u16;
                        self.esc = EscState::Csi { params, n };
                    }
                    b';' => {
                        n += 1;
                        self.esc = EscState::Csi { params, n };
                    }
                    // A final byte: dispatch. Unknown finals are dropped whole, which is what
                    // makes unrecognized keys (F-keys, mouse noise) harmless.
                    0x40..=0x7e => return self.csi_final(b, params[0], out),
                    _ => {} // malformed; abandon
                }
                return Event::None;
            }
            EscState::Ss3 => return self.csi_final(b, 0, out),
        }

        match b {
            ESC => {
                self.esc = EscState::Esc;
                Event::None
            }
            b'\r' | b'\n' => {
                self.last_was_cr = b == b'\r';
                self.finish_line(out)
            }
            0x03 => {
                // ^C: discard the line in progress. The caller routes the interrupt (or, today,
                // merely fails a pending read); see design/interrupt-routing.md.
                out.put(b"^C\r\n");
                self.len = 0;
                self.cur = 0;
                self.browse = None;
                Event::Interrupt
            }
            0x04 => {
                if self.len == 0 {
                    Event::Eof
                } else {
                    self.delete_at(out);
                    Event::None
                }
            }
            0x01 => {
                self.home(out);
                Event::None
            }
            0x05 => {
                self.end(out);
                Event::None
            }
            0x02 => {
                self.left(out);
                Event::None
            }
            0x06 => {
                self.right(out);
                Event::None
            }
            0x0b => {
                // ^K: kill to end of line.
                self.kill[..self.len - self.cur].copy_from_slice(&self.buf[self.cur..self.len]);
                self.kill_len = self.len - self.cur;
                self.len = self.cur;
                out.put(b"\x1b[K");
                Event::None
            }
            0x15 => {
                // ^U: kill to start of line.
                self.kill[..self.cur].copy_from_slice(&self.buf[..self.cur]);
                self.kill_len = self.cur;
                self.delete_range(0, self.cur, out);
                Event::None
            }
            0x17 => {
                // ^W: kill the word before the cursor (blanks, then non-blanks, like bash).
                let mut a = self.cur;
                while a > 0 && self.buf[a - 1] == b' ' {
                    a -= 1;
                }
                while a > 0 && self.buf[a - 1] != b' ' {
                    a -= 1;
                }
                self.kill[..self.cur - a].copy_from_slice(&self.buf[a..self.cur]);
                self.kill_len = self.cur - a;
                self.delete_range(a, self.cur, out);
                Event::None
            }
            0x19 => {
                self.yank(out);
                Event::None
            }
            0x0c => {
                // ^L: clear the screen, repaint prompt and line. The one op that needs the
                // prompt, which is why READLINE carries it.
                out.put(b"\x1b[2J\x1b[H");
                out.put(&self.prompt[..self.prompt_len]);
                out.put(&self.buf[..self.len]);
                csi_left(out, self.len - self.cur);
                Event::None
            }
            BS | 0x7f => {
                self.backspace(out);
                Event::None
            }
            0x20..=0x7e => {
                self.insert(b, out);
                Event::None
            }
            _ => Event::None, // other control bytes: ignored, deliberately unecho'd
        }
    }

    /// A CSI (or SS3) final byte, with its first numeric parameter.
    fn csi_final(&mut self, b: u8, p: u16, out: &mut impl Sink) -> Event {
        match (b, p) {
            (b'D', _) => self.left(out),
            (b'C', _) => self.right(out),
            (b'A', _) => self.hist_prev(out),
            (b'B', _) => self.hist_next_entry(out),
            (b'H', _) | (b'~', 1) | (b'~', 7) => self.home(out),
            (b'F', _) | (b'~', 4) | (b'~', 8) => self.end(out),
            (b'~', 3) => self.delete_at(out),
            _ => {} // unknown key; ignore
        }
        Event::None
    }

    fn finish_line(&mut self, out: &mut impl Sink) -> Event {
        out.put(b"\r\n");
        self.done[..self.len].copy_from_slice(&self.buf[..self.len]);
        self.done_len = self.len;
        // Record non-empty lines, skipping an exact repeat of the newest entry.
        if self.len > 0 && !self.repeats_newest() {
            self.hist[self.hist_next].0[..self.len].copy_from_slice(&self.buf[..self.len]);
            self.hist[self.hist_next].1 = self.len;
            self.hist_next = (self.hist_next + 1) % HIST;
            self.hist_count = (self.hist_count + 1).min(HIST);
        }
        self.len = 0;
        self.cur = 0;
        self.browse = None;
        // The prompt belonged to the read that just completed; the next read brings its own.
        self.prompt_len = 0;
        Event::Line
    }

    fn repeats_newest(&self) -> bool {
        if self.hist_count == 0 {
            return false;
        }
        let (entry, elen) = &self.hist[(self.hist_next + HIST - 1) % HIST];
        *elen == self.len && entry[..*elen] == self.buf[..self.len]
    }

    fn insert(&mut self, b: u8, out: &mut impl Sink) {
        if self.len >= LINE_MAX {
            out.put(&[BEL]);
            return;
        }
        self.buf.copy_within(self.cur..self.len, self.cur + 1);
        self.buf[self.cur] = b;
        self.len += 1;
        self.cur += 1;
        if self.cur == self.len {
            // The common case, typing at the end: echo the byte and nothing else.
            out.put(&[b]);
        } else {
            // Mid-line: echo the byte and the shifted tail, then walk back over the tail.
            out.put(&self.buf[self.cur - 1..self.len]);
            csi_left(out, self.len - self.cur);
        }
    }

    fn backspace(&mut self, out: &mut impl Sink) {
        if self.cur == 0 {
            return;
        }
        self.delete_range(self.cur - 1, self.cur, out);
    }

    fn delete_at(&mut self, out: &mut impl Sink) {
        if self.cur == self.len {
            return;
        }
        self.delete_range(self.cur, self.cur + 1, out);
    }

    /// Remove `buf[a..b]` and repaint what changed: step back to column `a`, print the shifted
    /// tail, erase the leftover columns, step back to the cursor. CSI K does the erasing, so no
    /// space-padding arithmetic. Works for both directions: a backward kill has the cursor at
    /// `b` (it moves left with the deletion), a forward delete has it at `a` (it stays put).
    fn delete_range(&mut self, a: usize, b: usize, out: &mut impl Sink) {
        debug_assert!(a <= b && b <= self.len && a <= self.cur);
        let removed = b - a;
        if removed == 0 {
            return;
        }
        let old_cur = self.cur;
        self.buf.copy_within(b..self.len, a);
        self.len -= removed;
        self.cur = if old_cur >= b {
            old_cur - removed
        } else {
            old_cur.min(a)
        };
        csi_left(out, old_cur - a);
        out.put(&self.buf[a..self.len]);
        out.put(b"\x1b[K");
        csi_left(out, self.len - self.cur);
    }

    fn yank(&mut self, out: &mut impl Sink) {
        let n = self.kill_len.min(LINE_MAX - self.len);
        if n == 0 {
            if self.kill_len > 0 {
                out.put(&[BEL]); // there was something to yank and no room for it
            }
            return;
        }
        self.buf.copy_within(self.cur..self.len, self.cur + n);
        // A yank of the kill buffer into the line it was cut from cannot overlap: kill is a
        // separate buffer, copied out at kill time.
        let kill = self.kill;
        self.buf[self.cur..self.cur + n].copy_from_slice(&kill[..n]);
        self.len += n;
        self.cur += n;
        out.put(&self.buf[self.cur - n..self.len]);
        csi_left(out, self.len - self.cur);
    }

    fn left(&mut self, out: &mut impl Sink) {
        if self.cur > 0 {
            self.cur -= 1;
            out.put(&[BS]);
        }
    }

    fn right(&mut self, out: &mut impl Sink) {
        if self.cur < self.len {
            self.cur += 1;
            out.put(b"\x1b[C");
        }
    }

    fn home(&mut self, out: &mut impl Sink) {
        csi_left(out, self.cur);
        self.cur = 0;
    }

    fn end(&mut self, out: &mut impl Sink) {
        csi_right(out, self.len - self.cur);
        self.cur = self.len;
    }

    /// Up arrow: show the previous (older) history entry. The first press stashes the line in
    /// progress so browsing is non-destructive.
    fn hist_prev(&mut self, out: &mut impl Sink) {
        let k = match self.browse {
            None => {
                if self.hist_count == 0 {
                    out.put(&[BEL]);
                    return;
                }
                self.stash.0[..self.len].copy_from_slice(&self.buf[..self.len]);
                self.stash.1 = self.len;
                self.stash.2 = self.cur;
                1
            }
            Some(k) if k < self.hist_count => k + 1,
            Some(_) => {
                out.put(&[BEL]); // already at the oldest
                return;
            }
        };
        self.browse = Some(k);
        let (entry, elen) = self.hist[(self.hist_next + HIST - k) % HIST];
        self.replace_line(&entry[..elen], out);
    }

    /// Down arrow: toward newer entries; past the newest, restore the stashed line.
    fn hist_next_entry(&mut self, out: &mut impl Sink) {
        match self.browse {
            None => out.put(&[BEL]),
            Some(1) => {
                self.browse = None;
                let (stash, slen, scur) = self.stash;
                self.replace_line(&stash[..slen], out);
                // replace_line leaves the cursor at the end; restore the stashed column.
                csi_left(out, slen - scur);
                self.cur = scur;
            }
            Some(k) => {
                self.browse = Some(k - 1);
                let (entry, elen) = self.hist[(self.hist_next + HIST - (k - 1)) % HIST];
                self.replace_line(&entry[..elen], out);
            }
        }
    }

    /// Replace the whole edit line with `text` and repaint the input region, cursor at the end.
    fn replace_line(&mut self, text: &[u8], out: &mut impl Sink) {
        csi_left(out, self.cur);
        let n = text.len().min(LINE_MAX);
        self.buf[..n].copy_from_slice(&text[..n]);
        self.len = n;
        self.cur = n;
        out.put(&self.buf[..n]);
        out.put(b"\x1b[K");
    }
}

/// Output-side processing: translate `\n` to `\r\n` so applications can speak Unix newlines to
/// a serial terminal that wants a carriage return. Everything else, ANSI included, passes
/// through untouched: the wire belongs to the application when it is printing.
pub fn expand_output(src: &[u8], out: &mut impl Sink) {
    let mut from = 0;
    for (i, &b) in src.iter().enumerate() {
        if b == b'\n' {
            out.put(&src[from..i]);
            out.put(b"\r\n");
            from = i + 1;
        }
    }
    out.put(&src[from..]);
}

/// Emit `CSI <n> D` (cursor left `n`), eliding the count when it is 1 and the whole sequence
/// when it is 0.
fn csi_left(out: &mut impl Sink, n: usize) {
    csi_move(out, n, b'D');
}

/// Emit `CSI <n> C` (cursor right `n`), same elisions.
fn csi_right(out: &mut impl Sink, n: usize) {
    csi_move(out, n, b'C');
}

fn csi_move(out: &mut impl Sink, n: usize, dir: u8) {
    match n {
        0 => {}
        1 => out.put(&[ESC, b'[', dir]),
        _ => {
            // One contiguous put per sequence: a sink must never see half an escape.
            let mut seq = [0u8; 11];
            seq[0] = ESC;
            seq[1] = b'[';
            let mut i = 10;
            let mut v = n;
            while v > 0 {
                i -= 1;
                seq[i] = b'0' + (v % 10) as u8;
                v /= 10;
            }
            seq.copy_within(i..10, 2);
            let digits = 10 - i;
            seq[2 + digits] = dir;
            out.put(&seq[..3 + digits]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::string::String;
    use std::vec::Vec;

    /// A one-row terminal model: interprets the echo stream the way a VT does, so the tests
    /// assert **what the user sees**, not which escape sequences we happened to emit. This is
    /// what lets the engine change its redraw strategy without rewriting every test.
    struct Screen {
        row: Vec<u8>,
        col: usize,
        bells: usize,
        cleared: bool,
    }

    impl Screen {
        fn new() -> Self {
            Screen {
                row: Vec::new(),
                col: 0,
                bells: 0,
                cleared: false,
            }
        }

        /// The visible row as a string (trailing blanks trimmed).
        fn text(&self) -> String {
            let end = self
                .row
                .iter()
                .rposition(|&b| b != b' ')
                .map_or(0, |p| p + 1);
            String::from_utf8_lossy(&self.row[..end]).into_owned()
        }

        fn interpret(&mut self, bytes: &[u8]) {
            let mut i = 0;
            while i < bytes.len() {
                let b = bytes[i];
                match b {
                    0x07 => self.bells += 1,
                    0x08 => self.col = self.col.saturating_sub(1),
                    b'\r' => self.col = 0,
                    b'\n' => { /* row model: a fresh line, content untouched */ }
                    0x1b => {
                        // CSI only; that is all the engine emits.
                        assert_eq!(bytes.get(i + 1), Some(&b'['), "non-CSI escape in echo");
                        let mut j = i + 2;
                        let mut n: usize = 0;
                        let mut has_n = false;
                        while j < bytes.len() && bytes[j].is_ascii_digit() {
                            n = n * 10 + (bytes[j] - b'0') as usize;
                            has_n = true;
                            j += 1;
                        }
                        let count = if has_n { n } else { 1 };
                        match bytes[j] {
                            b'D' => self.col = self.col.saturating_sub(count),
                            b'C' => self.col += count,
                            b'K' => self.row.truncate(self.col),
                            b'J' => {
                                self.row.clear();
                                self.cleared = true;
                            }
                            b'H' => self.col = 0,
                            other => panic!("echo used unmodeled CSI final {other:?}"),
                        }
                        i = j;
                    }
                    _ => {
                        if self.col == self.row.len() {
                            self.row.push(b);
                        } else {
                            self.row[self.col] = b;
                        }
                        self.col += 1;
                    }
                }
                i += 1;
            }
        }
    }

    impl Sink for Screen {
        fn put(&mut self, bytes: &[u8]) {
            self.interpret(bytes);
        }
    }

    fn feed_all(d: &mut LineDisc, s: &mut Screen, bytes: &[u8]) -> Vec<Event> {
        bytes
            .iter()
            .map(|&b| d.feed(b, s))
            .filter(|e| *e != Event::None)
            .collect()
    }

    /// Typing echoes, Enter completes, and the completed line is exactly what was typed. The
    /// baseline everything else builds on.
    #[test]
    fn plain_typing_makes_a_line() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        let events = feed_all(&mut d, &mut s, b"echo hi\r");
        assert_eq!(events, [Event::Line]);
        assert_eq!(d.line(), b"echo hi");
        assert_eq!(s.text(), "echo hi");
    }

    /// Backspace erases from the screen as well as the buffer, including mid-line, where the
    /// tail must shift left visibly.
    #[test]
    fn backspace_erases_on_screen() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"ekko");
        feed_all(&mut d, &mut s, &[0x7f, 0x7f, 0x7f]);
        assert_eq!(s.text(), "e");
        feed_all(&mut d, &mut s, b"cho\r");
        assert_eq!(d.line(), b"echo");
    }

    /// Cursor keys move the insertion point: arrow-left three times into "hllo", insert 'e',
    /// get "hello", and the screen agrees at every step.
    #[test]
    fn arrow_keys_edit_mid_line() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"hllo");
        feed_all(&mut d, &mut s, b"\x1b[D\x1b[D\x1b[D");
        assert_eq!(feed_all(&mut d, &mut s, b"e"), []);
        assert_eq!(s.text(), "hello");
        assert_eq!(feed_all(&mut d, &mut s, b"\r"), [Event::Line]);
        assert_eq!(d.line(), b"hello");
    }

    /// SS3 arrows (application cursor mode, `ESC O D`) work exactly like CSI arrows.
    #[test]
    fn ss3_arrows_are_understood() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"ab\x1bODc\r");
        assert_eq!(d.line(), b"acb");
    }

    /// Home/End in all three encodings land the cursor where they claim: type at Home and the
    /// character appears at the front.
    #[test]
    fn home_and_end_place_the_cursor() {
        for home in [&b"\x1b[H"[..], b"\x1b[1~", b"\x01"] {
            let (mut d, mut s) = (LineDisc::new(), Screen::new());
            feed_all(&mut d, &mut s, b"bc");
            feed_all(&mut d, &mut s, home);
            feed_all(&mut d, &mut s, b"a");
            feed_all(&mut d, &mut s, b"\x1b[F"); // End
            feed_all(&mut d, &mut s, b"d\r");
            assert_eq!(d.line(), b"abcd", "home encoding {home:x?}");
            assert_eq!(s.text(), "abcd");
        }
    }

    /// Delete (CSI 3~) removes under the cursor; ^D does the same on a non-empty line.
    #[test]
    fn delete_forward_both_ways() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"axbc");
        feed_all(&mut d, &mut s, b"\x1b[D\x1b[D\x1b[D"); // cursor on 'x'
        feed_all(&mut d, &mut s, b"\x1b[3~");
        assert_eq!(s.text(), "abc");
        feed_all(&mut d, &mut s, b"\x04"); // ^D on 'b'
        assert_eq!(s.text(), "ac");
        feed_all(&mut d, &mut s, b"\r");
        assert_eq!(d.line(), b"ac");
    }

    /// ^K kills to end, ^Y yanks it back: cut "ef", move home, yank, and the killed text lands
    /// at the cursor, on screen and in the line.
    #[test]
    fn kill_to_end_and_yank() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"abcdef");
        feed_all(&mut d, &mut s, b"\x02\x02"); // ^B x2, cursor before "ef"
        feed_all(&mut d, &mut s, b"\x0b"); // ^K
        assert_eq!(s.text(), "abcd");
        feed_all(&mut d, &mut s, b"\x01\x19"); // ^A then ^Y
        assert_eq!(s.text(), "efabcd");
        feed_all(&mut d, &mut s, b"\r");
        assert_eq!(d.line(), b"efabcd");
    }

    /// ^U kills to start; ^W kills the word before the cursor, blanks first.
    #[test]
    fn kill_to_start_and_kill_word() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"rm -rf /\x15"); // ^U
        assert_eq!(s.text(), "");
        feed_all(&mut d, &mut s, b"echo one two");
        feed_all(&mut d, &mut s, b"\x17"); // ^W
        assert_eq!(s.text(), "echo one");
        feed_all(&mut d, &mut s, b"\x17\x17\r");
        assert_eq!(d.line(), b"");
    }

    /// Up arrow recalls the previous line; a second up goes older; down comes back; and
    /// browsing past the newest entry restores the line that was in progress.
    #[test]
    fn history_browsing_is_non_destructive() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"first\r");
        feed_all(&mut d, &mut s, b"second\r");
        s = Screen::new();
        feed_all(&mut d, &mut s, b"in prog"); // a line in progress
        feed_all(&mut d, &mut s, b"\x1b[A");
        assert_eq!(s.text(), "second");
        feed_all(&mut d, &mut s, b"\x1b[A");
        assert_eq!(s.text(), "first");
        feed_all(&mut d, &mut s, b"\x1b[B\x1b[B");
        assert_eq!(s.text(), "in prog");
        feed_all(&mut d, &mut s, b"\r");
        assert_eq!(d.line(), b"in prog");
    }

    /// A recalled history entry can be edited and submitted, and an exact repeat of the newest
    /// entry is not recorded twice (up, up must reach the older command).
    #[test]
    fn history_recall_edits_and_dedups() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"run 9\r");
        feed_all(&mut d, &mut s, b"run 9\r"); // repeat: must not duplicate
        feed_all(&mut d, &mut s, b"help\r");
        s = Screen::new();
        feed_all(&mut d, &mut s, b"\x1b[A\x1b[A"); // help, then run 9 (not help twice)
        assert_eq!(s.text(), "run 9");
        feed_all(&mut d, &mut s, &[0x7f]);
        feed_all(&mut d, &mut s, b"7\r");
        assert_eq!(d.line(), b"run 7");
    }

    /// The history ring holds [`HIST`] entries and evicts the oldest.
    #[test]
    fn history_ring_evicts_the_oldest() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        for i in 0..=HIST {
            feed_all(&mut d, &mut s, std::format!("cmd{i}\r").as_bytes());
        }
        s = Screen::new();
        for _ in 0..HIST + 5 {
            feed_all(&mut d, &mut s, b"\x1b[A"); // walk to the oldest, then bell
        }
        assert_eq!(s.text(), "cmd1", "cmd0 should have been evicted");
        assert!(s.bells > 0, "walking past the oldest should ring the bell");
    }

    /// ^C discards the line in progress and reports Interrupt; the next line starts clean.
    #[test]
    fn ctrl_c_discards_the_line() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        let events = feed_all(&mut d, &mut s, b"rm -rf\x03");
        assert_eq!(events, [Event::Interrupt]);
        let events = feed_all(&mut d, &mut s, b"ok\r");
        assert_eq!(events, [Event::Line]);
        assert_eq!(d.line(), b"ok");
    }

    /// ^D is EOF only on an empty line.
    #[test]
    fn ctrl_d_is_eof_only_when_empty() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        assert_eq!(feed_all(&mut d, &mut s, b"\x04"), [Event::Eof]);
        assert_eq!(feed_all(&mut d, &mut s, b"x\x04"), []);
    }

    /// CR, LF, and CRLF each end exactly one line: piped scripts (LF), terminals (CR), and
    /// CRLF senders all behave.
    #[test]
    fn line_endings_cr_lf_crlf() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        let events = feed_all(&mut d, &mut s, b"a\rb\nc\r\nd\r");
        assert_eq!(events.len(), 4, "CRLF must not count as two Enters");
        assert_eq!(d.line(), b"d");
    }

    /// An overlong line refuses further input with a bell instead of truncating silently or
    /// walking off the buffer.
    #[test]
    fn overflow_rings_the_bell() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        for _ in 0..LINE_MAX + 3 {
            d.feed(b'a', &mut s);
        }
        assert_eq!(s.bells, 3);
        feed_all(&mut d, &mut s, b"\r");
        assert_eq!(d.line().len(), LINE_MAX);
    }

    /// ^L repaints the prompt and the line after clearing, with the cursor where it was:
    /// typing continues mid-line.
    #[test]
    fn ctrl_l_repaints_prompt_and_line() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        d.start_line(b"$ ", &mut s);
        feed_all(&mut d, &mut s, b"ab\x1b[D"); // cursor between a and b
        feed_all(&mut d, &mut s, b"\x0c");
        assert!(s.cleared);
        assert_eq!(s.text(), "$ ab");
        feed_all(&mut d, &mut s, b"x");
        assert_eq!(
            s.text(),
            "$ axb",
            "typing after ^L must land mid-line on screen"
        );
        feed_all(&mut d, &mut s, b"\r");
        assert_eq!(d.line(), b"axb");
    }

    /// `start_line` paints the prompt and any type-ahead, and the completed line includes the
    /// type-ahead: bytes that arrived before the read are not lost.
    #[test]
    fn start_line_shows_type_ahead() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"early");
        let mut s2 = Screen::new();
        d.start_line(b"$ ", &mut s2);
        assert_eq!(s2.text(), "$ early");
        feed_all(&mut d, &mut s2, b"\r");
        assert_eq!(d.line(), b"early");
    }

    /// Unknown CSI sequences (F-keys and friends) are swallowed whole: no stray bytes appear
    /// in the line or on the screen.
    #[test]
    fn unknown_sequences_are_swallowed() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"a\x1b[15~b\x1b[Zc\r"); // F5, shift-tab
        assert_eq!(d.line(), b"abc");
        assert_eq!(s.text(), "abc");
    }

    /// Output expansion: every bare `\n` becomes `\r\n`, existing `\r\n` is untouched (the
    /// engine never emits `\r\r\n`), and ANSI passes through.
    #[test]
    fn output_expansion_translates_newlines() {
        struct Buf(Vec<u8>);
        impl Sink for Buf {
            fn put(&mut self, b: &[u8]) {
                self.0.extend_from_slice(b);
            }
        }
        let mut out = Buf(Vec::new());
        expand_output(b"a\nb\x1b[1mc\n", &mut out);
        assert_eq!(out.0, b"a\r\nb\x1b[1mc\r\n");
    }

    /// The proto word packing round-trips, and the reserved bits stay zero.
    #[test]
    fn proto_words_round_trip() {
        let w = proto::req(proto::OP_READLINE, 42);
        assert_eq!(proto::op(w), proto::OP_READLINE);
        assert_eq!(proto::len(w), 42);
        assert_eq!(w & 0x00ff_ffff_0000_0000, 0);
        // The reply flags are wire ABI shared with every client: pin the values, because a
        // drifted FLAG_INTERRUPTED makes an interrupted read look like a normal empty line.
        assert_eq!(proto::FLAG_EOF, 1);
        assert_eq!(proto::FLAG_INTERRUPTED, 2);
    }

    /// The control-character encodings of movement (^B ^A ^E ^F), which the CSI tests do not
    /// cover, including ^B at column 0 where there is nowhere to go.
    #[test]
    fn control_key_movement() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"\x02"); // ^B on an empty line: nothing to do
        feed_all(&mut d, &mut s, b"ab");
        feed_all(&mut d, &mut s, b"\x01\x06x"); // ^A, ^F right one, insert mid-line
        assert_eq!(s.text(), "axb");
        feed_all(&mut d, &mut s, b"\x05c\r"); // ^E, append
        assert_eq!(d.line(), b"axbc");
        assert_eq!(s.text(), "axbc");
    }

    /// CSI C moves right mid-line and is a no-op at the end of the line; typing after each
    /// proves where the cursor actually is (the echo alone would not).
    #[test]
    fn right_arrow_moves_and_stops_at_end() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"ab\x1b[D\x1b[D"); // cursor to column 0
        feed_all(&mut d, &mut s, b"\x1b[Cx"); // right one, insert between a and b
        assert_eq!(s.text(), "axb");
        feed_all(&mut d, &mut s, b"\x1b[C\x1b[C"); // to the end, then once more (no-op)
        feed_all(&mut d, &mut s, b"c\r");
        assert_eq!(d.line(), b"axbc");
    }

    /// A CSI with a ';' (shift-Delete, `ESC [ 3 ; 2 ~`) still dispatches on the first
    /// parameter: the ';' advances to the second slot instead of aborting the sequence.
    #[test]
    fn csi_second_parameter_is_parsed() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"abc\x01"); // ^A, cursor on 'a'
        feed_all(&mut d, &mut s, b"\x1b[3;2~"); // delete under cursor, modifier ignored
        assert_eq!(s.text(), "bc");
        feed_all(&mut d, &mut s, b"\r");
        assert_eq!(d.line(), b"bc");
    }

    /// Type-ahead with the cursor moved off the end: `start_line` repaints and puts the
    /// cursor back where it was, so the next insertion lands mid-line on screen too.
    #[test]
    fn start_line_restores_mid_line_cursor() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"ab\x02"); // type ahead, cursor between a and b
        let mut s2 = Screen::new();
        d.start_line(b"$ ", &mut s2);
        feed_all(&mut d, &mut s2, b"X");
        assert_eq!(s2.text(), "$ aXb");
        feed_all(&mut d, &mut s2, b"\r");
        assert_eq!(d.line(), b"aXb");
    }

    /// Enter on an empty line completes an empty read but records nothing: up must recall
    /// the last real command, not a blank.
    #[test]
    fn empty_lines_stay_out_of_history() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"one\r\r");
        s = Screen::new();
        feed_all(&mut d, &mut s, b"\x1b[A");
        assert_eq!(s.text(), "one");
    }

    /// An immediate repeat is stored once: with "a" entered twice, the second up-arrow has
    /// nowhere older to go and rings the bell. The screen alone cannot tell one stored copy
    /// from two, which is why the bell count is the assertion here.
    #[test]
    fn duplicate_entry_is_stored_once() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"a\ra\r");
        s = Screen::new();
        feed_all(&mut d, &mut s, b"\x1b[A\x1b[A");
        assert_eq!(s.text(), "a");
        assert_eq!(s.bells, 1, "one stored entry: the second up must bell");
    }

    /// Walk a wrapped ring end to end in both directions, asserting every entry: ten
    /// commands in a ring of eight, plus a deduplicated repeat across the wrap. The
    /// existing browsing tests never cross the wrap point, which is where the ring index
    /// arithmetic can be wrong without them noticing.
    #[test]
    fn wrapped_ring_walks_correctly_both_ways() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        for i in 0..10 {
            feed_all(&mut d, &mut s, std::format!("cmd{i}\r").as_bytes());
        }
        feed_all(&mut d, &mut s, b"cmd9\r"); // repeat of the newest, post-wrap: not stored
        s = Screen::new();
        for i in (2..=9).rev() {
            feed_all(&mut d, &mut s, b"\x1b[A");
            assert_eq!(s.text(), std::format!("cmd{i}"), "up should reach cmd{i}");
        }
        feed_all(&mut d, &mut s, b"\x1b[A"); // past the oldest
        assert_eq!(s.bells, 1);
        assert_eq!(s.text(), "cmd2");
        for i in 3..=9 {
            feed_all(&mut d, &mut s, b"\x1b[B");
            assert_eq!(s.text(), std::format!("cmd{i}"), "down should reach cmd{i}");
        }
        feed_all(&mut d, &mut s, b"\x1b[B"); // past the newest: the (empty) line in progress
        assert_eq!(s.text(), "");
    }

    /// Browsing away and back restores not just the in-progress text but the cursor column:
    /// typing after the round trip lands where the cursor was left. The buffer restore is
    /// checked elsewhere; this pins the on-screen column, which only the echo carries.
    #[test]
    fn stash_restores_cursor_column() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"old\r");
        s = Screen::new();
        feed_all(&mut d, &mut s, b"hello\x02\x02"); // cursor after "hel"
        feed_all(&mut d, &mut s, b"\x1b[A\x1b[B"); // up to "old", back down
        feed_all(&mut d, &mut s, b"X");
        assert_eq!(s.text(), "helXlo");
        feed_all(&mut d, &mut s, b"\r");
        assert_eq!(d.line(), b"helXlo");
    }

    /// A yank that does not fit is truncated to the room left: the line ends exactly full,
    /// silently (the bell is reserved for a yank with no room at all).
    #[test]
    fn yank_truncates_to_the_room_left() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        for _ in 0..250 {
            d.feed(b'a', &mut s);
        }
        feed_all(&mut d, &mut s, b"\x15"); // ^U: kill all 250
        for _ in 0..10 {
            d.feed(b'b', &mut s);
        }
        feed_all(&mut d, &mut s, b"\x19\r"); // ^Y: only 246 of the 250 fit
        assert_eq!(s.bells, 0);
        assert_eq!(d.line().len(), LINE_MAX);
        assert_eq!(&d.line()[..10], b"bbbbbbbbbb");
        assert!(d.line()[10..].iter().all(|&b| b == b'a'));
    }

    /// ^Y with nothing ever killed is silent; ^Y with a kill buffer but a full line rings
    /// the bell (there was something to yank and nowhere to put it). Both sides matter: the
    /// distinction is the condition, not just the bell.
    #[test]
    fn yank_bell_needs_a_kill_and_a_full_line() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"\x19"); // empty kill buffer: nothing, no bell
        assert_eq!(s.bells, 0);
        feed_all(&mut d, &mut s, b"x\x15"); // kill buffer now holds "x"
        for _ in 0..LINE_MAX {
            d.feed(b'a', &mut s);
        }
        feed_all(&mut d, &mut s, b"\x19"); // no room at all
        assert_eq!(s.bells, 1);
    }

    /// Yank into the middle of a line, then keep typing: the tail stays put on screen
    /// through both the yank repaint and the next insertion.
    #[test]
    fn yank_mid_line_keeps_the_tail_in_place() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"abcdef\x02\x02\x0b"); // ^K kills "ef"
        feed_all(&mut d, &mut s, b"\x02\x02\x19"); // yank it between "ab" and "cd"
        assert_eq!(s.text(), "abefcd");
        feed_all(&mut d, &mut s, b"X");
        assert_eq!(s.text(), "abefXcd");
        feed_all(&mut d, &mut s, b"\r");
        assert_eq!(d.line(), b"abefXcd");
    }

    /// ^W with only blanks before the cursor stops cleanly at column 0 (the blank-skipping
    /// loop must not walk past the front of the buffer).
    #[test]
    fn word_kill_over_leading_blanks() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"  \x17\r");
        assert_eq!(d.line(), b"");
        assert_eq!(s.text(), "");
    }

    /// ^W then ^Y round-trips exactly the killed word: the kill length is the word's, not
    /// the cursor position's.
    #[test]
    fn word_kill_yank_round_trip() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"ab cd\x17\x19\r");
        assert_eq!(d.line(), b"ab cd");
    }

    /// Home from column 12 needs a two-digit CSI count; typing at the front proves the
    /// cursor really travelled all twelve columns. Every other test moves fewer than ten,
    /// so the multi-digit encoding path was unexercised.
    #[test]
    fn multi_digit_cursor_move() {
        let (mut d, mut s) = (LineDisc::new(), Screen::new());
        feed_all(&mut d, &mut s, b"abcdefghijkl\x01X");
        assert_eq!(s.text(), "Xabcdefghijkl");
        feed_all(&mut d, &mut s, b"\r");
        assert_eq!(d.line(), b"Xabcdefghijkl");
    }
}
