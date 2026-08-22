//! **`ps`: what a process listing is when there is no `/proc`** (milestone 126,
//! notes/process-view.md).
//!
//! This is the program's logic, lifted out so it runs on the host in milliseconds; `user/src/ps.rs`
//! is the syscall and the two output streams and nothing else. The crate and the program share a
//! name because they are one thing split at the IO boundary, which is the convention `coremark`,
//! `line_editor` and `compositor` already follow.
//!
//! # What it does not do, which is the whole point
//!
//! `ps aux` on Linux reads `/proc`, which is **ambient**: any process gets it with no grant from
//! anyone, so the listing is every process on the machine, including the command lines with
//! secrets in `argv`. Nobody defends that; `hidepid` exists because enough people stopped wanting
//! it.
//!
//! Here the listing is a **capability**. What this crate is handed is a function that reads one
//! entry of a domain, and that function is backed by `abi::endpoint::SURVEY` on a supervision
//! endpoint the program was endowed. It cannot widen the domain, cannot ask about a tid it was not
//! shown, and cannot discover that any other domain exists. There is no path here that reaches a
//! process this program's caller did not already have authority over, because the only input is the
//! reader itself.
//!
//! # Three answers, and telling them apart is the deliverable
//!
//! A monitor that reports nothing because it **could not look** is the worst failure this tool has
//! available: it reads exactly like a quiet machine. So the three cases are kept distinct all the
//! way to the terminal:
//!
//! - **A domain with processes in it.** A table on the output stream.
//! - **A domain that is empty.** No rows, and a line on the *diagnostics* stream saying the domain
//!   is empty. Not an error: the caller's authority was never in question.
//! - **A refusal.** No rows at all, and a line on diagnostics naming the reason. `fs_proto` chose
//!   `EPERM` over an empty listing for this exact reason, and so does this.
//!
//! # Collect first, complain second, print third
//!
//! DECISIONS §67 gives a program a second output stream and one rule with it: everything it has to
//! complain about is said, and that stream closed, **before** it writes a byte of output. Its reader
//! is single-threaded and drains diagnostics to end-of-stream first, so a program that interleaved
//! the two would block in a rendezvous nobody is listening for.
//!
//! A survey cannot know its complaints up front: an endpoint can be destroyed halfway through a
//! walk. So [`collect`] takes the whole domain into a buffer first, and the caller then emits
//! [`Survey::write_diagnostics`] and [`Survey::write_report`] in that order.
//!
//! **The buffer is the caller's**, which is not ceremony. It was a `[Row; MAX_ROWS]` local until
//! `script/stack-frame-check` failed the build: two kilobytes of rows made `collect`'s frame 4,336
//! bytes, larger than the 4,096-byte guard page under every kernel thread stack, so one call could
//! move `sp` past the guard in a single step and write into a neighbouring thread's stack without
//! ever faulting. A caller-provided slice is the fix that gate recommends, and it is better anyway:
//! a program that sizes its own listing knows where the memory came from.
//!
//! Sizing it at [`MAX_ROWS`] makes truncation unreachable, because that is the kernel's whole thread
//! table. `ps` does exactly that. A shorter buffer is allowed and is **not silent**: the survey says
//! it was truncated, on diagnostics, for the same reason a refusal is not an empty list.
//!
//! # EXAMPLES
//!
//! At the prompt, in a domain holding two jobs and `ps` itself:
//!
//! ```text
//! $ ps
//!          TID  STATE
//!            5  blocked
//!            9  running
//! 4294967302  dead
//! ```
//!
//! The empty and the refused cases, which differ in a way `ps` on Linux has no way to express:
//!
//! ```text
//! $ ps                 ps: this domain holds no processes
//! $ ps                 ps: this endpoint does not carry the right to look
//! ```
//!
//! Driving it from a host test is the whole contract, and it needs no kernel:
//!
//! ```
//! use ps::{MAX_ROWS, Row, collect};
//!
//! // A domain of one running thread: cursor 0 yields it, cursor 1 says done.
//! let mut reader = |cursor: u64| match cursor {
//!     0 => (1, 7, abi::survey::RUNNING),
//!     _ => (abi::survey::DONE as i64, 0, 0),
//! };
//! let mut rows = [Row::default(); MAX_ROWS];
//! let survey = collect(&mut rows, &mut reader);
//! assert_eq!(survey.rows().len(), 1);
//! assert_eq!(survey.rows()[0].tid, 7);
//!
//! let mut text = Vec::new();
//! survey.write_report(&mut |b| text.extend_from_slice(b));
//! assert!(String::from_utf8(text).unwrap().contains("running"));
//! ```
//!
//! # BUGS
//!
//! - **A process has no name here, so there is no `CMD` column.** This system has `arg0` in `Spawn`
//!   and no display name at all, so the columns are the tid and the run state and that is
//!   everything. A name is information rather than authority, but a confined viewer may still not
//!   be entitled to it and there is no design for that today; a `CMD` column that appeared without
//!   one would be a leak wearing a familiar heading.
//! - **The tid is a generational name, so it is a large and ugly number** after any slot reuse
//!   (`(generation << 32) | slot`, `crates/slots`). It is printed as the one integer
//!   `abi::endpoint::REAP` would accept, because splitting it into `gen:slot` would publish the
//!   shape of the kernel's thread table to every program that can run `ps`.
//! - **There is no sort order to choose.** Rows come out in the kernel's slot order, which is
//!   neither creation order nor tid order once a slot has been reused. A `--sort` would be honest
//!   only once there is something to sort by, which means the accounting `top` needs.
//! - **The state is a snapshot per row, not per table.** See the `BUGS` section of
//!   notes/process-view.md: a row read early in the walk may be stale by the time the table prints.
//!
//! Name: recorded (milestone 126, and notes/naming.md's crate section). `ps` is the name every
//! reader already knows from outside this project, which the naming tenet calls the best name
//! available and not one to spend a rename on. Sharing it with `user/src/ps.rs` is the crate-and-
//! program pair the same tenet describes: splitting them would hide the relationship, so `coremark`,
//! `line_editor` and `compositor` all keep one name across the two. That rule plus the program's
//! name produces this crate name, which is the whole of what `recorded` claims: calef ruled on the
//! rules, and never on this crate.

#![cfg_attr(not(test), no_std)]

/// The widest domain a survey can produce: the kernel's entire thread table (`MAX_THREADS` in
/// `kernel/src/sched.rs`, 128 as of milestone 126).
///
/// **A buffer this size makes truncation unreachable rather than handled.** A `ps` holding the
/// widest grant this system can express sees every thread on the machine, and that is still 128
/// rows, so there is no "and N more" the shipped program can ever print. If the kernel's table
/// grows, this is the number that moves with it, and a compile-time assertion in
/// `kernel::user::survey_tests` is what keeps the two in step rather than a reader.
pub const MAX_ROWS: usize = 128;

/// One line of the listing: a thread and what it is doing.
///
/// Two fields, because two facts is what a supervision domain honestly knows about its members.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Row {
    /// The kernel-stamped thread id, the same name `abi::endpoint::REAP` accepts.
    pub tid: u64,
    /// One of `abi::survey`'s state codes.
    pub state: u64,
}

/// A finished walk of one domain: the rows, and whatever went wrong.
///
/// Built by [`collect`], which is the only constructor, so a `Survey` always describes a walk that
/// really happened.
pub struct Survey<'a> {
    rows: &'a [Row],
    /// The negated `abi::Error` that ended the walk, if one did. `Some` here means **the listing is
    /// not the domain**: it is however far the walk got, which is why the caller prints the reason
    /// rather than the partial table.
    refused: Option<i64>,
    /// The kernel handed back a cursor that did not advance. Not reachable from the shipped kernel
    /// and checked anyway, because a `collect` that could loop forever on a bad reader would not be
    /// a total function and could not be fuzzed or property-tested.
    stalled: bool,
    /// The buffer filled and the domain had more in it. Unreachable for a caller that sized its
    /// buffer at [`MAX_ROWS`], which `ps` does; reported rather than silent for one that did not.
    truncated: bool,
}

/// **Walk a domain to its end and keep what it says.**
///
/// `read(cursor)` is one `abi::endpoint::SURVEY`: it answers `(next_cursor, tid, state)`, where a
/// negative first word is a refusal and `abi::survey::DONE` means the walk is over. Start at 0, feed
/// each `next_cursor` back; this function is that loop.
///
/// **It is deliberately a closure rather than a syscall**, which is what makes every case here
/// reachable from a host test: the empty domain, the refusal, and a domain of a hundred threads all
/// cost microseconds and no emulator.
///
/// The walk stops at the first refusal and reports it, rather than skipping the entry and carrying
/// on. A survey that silently dropped an entry it could not read would be the failure this whole
/// program exists to avoid, one row smaller.
pub fn collect<'a>(
    rows: &'a mut [Row],
    read: &mut dyn FnMut(u64) -> (i64, u64, u64),
) -> Survey<'a> {
    let mut n = 0usize;
    let mut refused = None;
    let mut stalled = false;
    let mut truncated = false;
    let mut cursor = 0u64;
    loop {
        let (next, tid, state) = read(cursor);
        if next < 0 {
            refused = Some(next);
            break;
        }
        let next = next as u64;
        if next == abi::survey::DONE {
            break;
        }
        // A cursor that did not advance would spin here forever. The kernel is inside the trusted
        // base and does not do this; the check exists so that this function terminates for *every*
        // reader, including a test's.
        if next <= cursor {
            stalled = true;
            break;
        }
        if n == rows.len() {
            truncated = true;
            break;
        }
        rows[n] = Row { tid, state };
        n += 1;
        cursor = next;
    }
    Survey {
        rows: &rows[..n],
        refused,
        stalled,
        truncated,
    }
}

impl Survey<'_> {
    /// The rows, in the order the kernel reported them.
    pub fn rows(&self) -> &[Row] {
        self.rows
    }

    /// **Was this a refusal?** True when the domain could not be read at all or not to its end. A
    /// caller must not print an empty table for this: see the crate docs.
    pub fn refused(&self) -> bool {
        self.refused.is_some() || self.stalled
    }

    /// **Is this listing the whole domain?** False when the walk was refused, stalled, or ran out of
    /// buffer. A caller that prints the table anyway is printing something it cannot vouch for.
    pub fn complete(&self) -> bool {
        !self.refused() && !self.truncated
    }

    /// **What there is to complain about, as a clause a program name prefixes**, or `None` when the
    /// walk succeeded and found processes.
    ///
    /// Split out of [`write_diagnostics`](Survey::write_diagnostics) so that a *second* program over
    /// the same survey reuses these sentences instead of writing a parallel catalogue: `crates/pgrep`
    /// filters this listing and has to describe a refusal in the same words, or the two programs
    /// drift into disagreeing about what one refusal means. The clause carries no program name and no
    /// newline for exactly that reason.
    ///
    /// The order is the order [`write_diagnostics`](Survey::write_diagnostics) has always used, and
    /// the empty domain is last because it is the only one of the four that is **not** a failure.
    pub fn complaint(&self) -> Option<&'static str> {
        if self.stalled {
            return Some("the survey did not advance; the listing is incomplete");
        }
        if let Some(code) = self.refused {
            return Some(refusal(code));
        }
        if self.truncated {
            return Some("the listing buffer filled; this domain has more in it");
        }
        if self.rows.is_empty() {
            // **Not an error, and it must not read like one.** The caller held the domain and was
            // allowed to look; there was nothing in it. This is the sentence that distinguishes an
            // empty answer from a refused one, which is the distinction Linux's `ps` cannot draw.
            return Some("this domain holds no processes");
        }
        None
    }

    /// **Everything to complain about, said before a byte of output** (DECISIONS §67).
    ///
    /// Writes nothing at all when the walk succeeded and found processes, which is the common case
    /// and the one where a second stream should stay silent.
    pub fn write_diagnostics(&self, out: &mut dyn FnMut(&[u8])) {
        if let Some(clause) = self.complaint() {
            out(b"ps: ");
            out(clause.as_bytes());
            out(b"\n");
        }
    }

    /// The table itself, on the output stream. **Nothing at all on a refusal**, so a `ps > out.txt`
    /// that was refused leaves an empty file rather than a plausible-looking listing of nothing.
    pub fn write_report(&self, out: &mut dyn FnMut(&[u8])) {
        if !self.complete() || self.rows.is_empty() {
            return;
        }
        out(b"         TID  STATE\n");
        for row in self.rows() {
            write_tid(row.tid, out);
            out(b"  ");
            out(state_name(row.state).as_bytes());
            out(b"\n");
        }
    }
}

/// What an `abi::survey` state code is called in the listing.
///
/// Lowercase, because these are states rather than headings and the tree's other programs do not
/// shout. An unknown code prints as `?` rather than panicking: this program is reading a number
/// from a kernel that may be newer than it is, and a listing with one honest `?` in it beats a
/// crash.
pub const fn state_name(state: u64) -> &'static str {
    match state {
        abi::survey::READY => "ready",
        abi::survey::RUNNING => "running",
        abi::survey::BLOCKED => "blocked",
        abi::survey::DEAD => "dead",
        _ => "?",
    }
}

/// What a negated `abi::Error` means to somebody who typed `ps`.
///
/// **Written as facts about authority, not as errno.** A refusal here must read like the capability
/// model, which is the same standard `grant_plan::Refusal::message` holds itself to: the reader
/// should learn what they do not hold, not which branch of a `match` they landed in.
pub fn refusal(code: i64) -> &'static str {
    match abi::Error::from_ret(code) {
        Some(abi::Error::NoSuchSlot) => "this process holds no process-domain capability",
        Some(abi::Error::NotPermitted) => {
            "this endpoint may be sent to, but not looked at: no READ on the domain"
        }
        Some(abi::Error::WrongObject) => {
            "the capability granted here is not a supervision endpoint"
        }
        Some(abi::Error::Gone) => "the domain this named has been destroyed",
        _ => "the domain could not be read",
    }
}

/// A tid right-aligned in twelve columns, which is wide enough for a generational name whose
/// generation has moved (`(1 << 32) | 6` is ten digits). A wider one is not truncated, it just
/// pushes the column; losing digits from a name that `REAP` has to accept would be worse than an
/// uneven table.
fn write_tid(tid: u64, out: &mut dyn FnMut(&[u8])) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    let mut v = tid;
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    let digits = buf.len() - i;
    for _ in digits..12 {
        out(b" ");
    }
    out(&buf[i..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader over a canned domain: `entries` in slot order, then done.
    fn domain(entries: &'static [(u64, u64)]) -> impl FnMut(u64) -> (i64, u64, u64) {
        move |cursor: u64| match entries.get(cursor as usize) {
            Some(&(tid, state)) => (cursor as i64 + 1, tid, state),
            None => (abi::survey::DONE as i64, 0, 0),
        }
    }

    fn shown(f: impl FnOnce(&mut dyn FnMut(&[u8]))) -> String {
        let mut v = Vec::new();
        f(&mut |b| v.extend_from_slice(b));
        String::from_utf8(v).unwrap()
    }

    #[test]
    fn a_domain_walks_to_its_end() {
        let mut rows = [Row::default(); MAX_ROWS];
        let s = collect(
            &mut rows,
            &mut domain(&[
                (3, abi::survey::RUNNING),
                (5, abi::survey::BLOCKED),
                (9, abi::survey::DEAD),
            ]),
        );
        assert!(!s.refused());
        assert_eq!(s.rows().len(), 3);
        assert_eq!(
            s.rows()[2],
            Row {
                tid: 9,
                state: abi::survey::DEAD
            }
        );
    }

    /// **The claim the whole program exists to make.** An empty domain and a refused one must not
    /// produce the same thing on any stream, because a monitor that reports nothing when it could
    /// not look reads exactly like a quiet machine.
    #[test]
    fn an_empty_domain_and_a_refusal_are_different_answers() {
        let mut rows_a = [Row::default(); MAX_ROWS];
        let empty = collect(&mut rows_a, &mut domain(&[]));
        let mut rows_b = [Row::default(); MAX_ROWS];
        let refused = collect(&mut rows_b, &mut |_| {
            (abi::Error::NotPermitted as i64, 0, 0)
        });

        assert!(!empty.refused(), "an empty domain is not a refusal");
        assert!(refused.refused());

        let empty_diag = shown(|o| empty.write_diagnostics(o));
        let refused_diag = shown(|o| refused.write_diagnostics(o));
        assert_ne!(empty_diag, refused_diag);
        assert!(empty_diag.contains("no processes"), "{empty_diag}");
        assert!(refused_diag.contains("not looked at"), "{refused_diag}");

        // And neither of them puts a single byte on the output stream, so a redirect of either
        // produces an empty file rather than a listing that says nothing happened.
        assert_eq!(shown(|o| empty.write_report(o)), "");
        assert_eq!(shown(|o| refused.write_report(o)), "");
    }

    /// A refusal arriving **mid-walk** is still a refusal, not a short table. Stopping and saying
    /// so is the point; a listing missing a row it could not read is the failure mode.
    #[test]
    fn a_refusal_halfway_through_discards_the_partial_table() {
        let mut calls = 0;
        let mut rows = [Row::default(); MAX_ROWS];
        let s = collect(&mut rows, &mut |cursor| {
            calls += 1;
            match cursor {
                0 => (1, 3, abi::survey::RUNNING),
                _ => (abi::Error::Gone as i64, 0, 0),
            }
        });
        assert!(s.refused());
        assert_eq!(
            s.rows().len(),
            1,
            "the rows it did read are kept for the record"
        );
        assert_eq!(
            shown(|o| s.write_report(o)),
            "",
            "but none of them are printed"
        );
        assert!(shown(|o| s.write_diagnostics(o)).contains("destroyed"));
    }

    /// **`collect` terminates for every reader**, including one that never advances its cursor. The
    /// shipped kernel cannot do this; the property is what makes this function safe to hand an
    /// arbitrary closure, which is what every test above does.
    #[test]
    fn a_cursor_that_does_not_advance_ends_the_walk() {
        let mut rows = [Row::default(); MAX_ROWS];
        let s = collect(&mut rows, &mut |_| (1, 4, abi::survey::READY));
        assert!(s.refused());
        assert!(shown(|o| s.write_diagnostics(o)).contains("did not advance"));
    }

    /// **A short buffer is not a short listing, it is a stated one.** The whole design refuses to
    /// let a monitor report less than it saw without saying so, and running out of room is one more
    /// way that could happen. `ps` cannot reach this, because it sizes its buffer at [`MAX_ROWS`];
    /// a future caller with a smaller one gets told rather than getting a plausible table.
    #[test]
    fn a_buffer_too_small_for_the_domain_says_so_and_prints_nothing() {
        let mut rows = [Row::default(); 2];
        let s = collect(&mut rows, &mut |cursor| {
            (cursor as i64 + 1, cursor + 100, abi::survey::READY)
        });
        assert_eq!(s.rows().len(), 2);
        assert!(
            !s.complete(),
            "a truncated listing claimed to be the whole domain"
        );
        assert!(!s.refused(), "running out of room is not a refusal");
        assert!(shown(|o| s.write_diagnostics(o)).contains("more in it"));
        assert_eq!(
            shown(|o| s.write_report(o)),
            "",
            "a partial table must not be printed"
        );
    }

    /// A domain as wide as the kernel's whole thread table fills the buffer exactly and needs no
    /// truncation case.
    #[test]
    fn the_widest_possible_domain_fits() {
        let mut rows = [Row::default(); MAX_ROWS];
        let s = collect(&mut rows, &mut |cursor| {
            if (cursor as usize) < MAX_ROWS {
                (cursor as i64 + 1, cursor + 100, abi::survey::READY)
            } else {
                (abi::survey::DONE as i64, 0, 0)
            }
        });
        assert!(!s.refused());
        assert_eq!(s.rows().len(), MAX_ROWS);
    }

    #[test]
    fn the_table_has_a_header_and_one_line_per_thread() {
        let mut rows = [Row::default(); MAX_ROWS];
        let s = collect(
            &mut rows,
            &mut domain(&[(3, abi::survey::RUNNING), (5, abi::survey::DEAD)]),
        );
        let out = shown(|o| s.write_report(o));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "header plus two rows: {out}");
        assert!(lines[0].contains("TID") && lines[0].contains("STATE"));
        assert!(lines[1].ends_with("3  running"), "{}", lines[1]);
        assert!(lines[2].ends_with("5  dead"), "{}", lines[2]);
        // Nothing to say when the answer is the table itself.
        assert_eq!(shown(|o| s.write_diagnostics(o)), "");
    }

    /// A generational tid is a ten-digit number and must survive the column intact: it is the name
    /// `REAP` accepts, so a truncated one would be a name that no longer works.
    #[test]
    fn a_wide_tid_keeps_every_digit() {
        let mut rows = [Row::default(); MAX_ROWS];
        let s = collect(
            &mut rows,
            &mut domain(&[((1u64 << 32) | 6, abi::survey::DEAD)]),
        );
        let out = shown(|o| s.write_report(o));
        assert!(out.contains("4294967302"), "{out}");
    }

    /// A state code from a kernel newer than this program prints as `?` rather than panicking or
    /// being silently dropped.
    #[test]
    fn an_unknown_state_is_shown_as_unknown() {
        assert_eq!(state_name(99), "?");
        let mut rows = [Row::default(); MAX_ROWS];
        let s = collect(&mut rows, &mut domain(&[(3, 99)]));
        assert!(shown(|o| s.write_report(o)).contains('?'));
    }

    /// Every refusal reads as a fact about authority. The catalogue is checked so the wording
    /// cannot drift into errno.
    #[test]
    fn every_refusal_names_what_is_not_held() {
        for code in [
            abi::Error::NoSuchSlot as i64,
            abi::Error::NotPermitted as i64,
            abi::Error::WrongObject as i64,
            abi::Error::Gone as i64,
            -99,
        ] {
            let m = refusal(code);
            assert!(!m.is_empty());
            assert!(
                m.chars().next().is_some_and(|c| c.is_lowercase()),
                "a refusal is a clause the program name prefixes: {m}",
            );
        }
    }
}
