//! **Sequencing: `;`, `&&` and `||`, and the one bit that passes between commands** (milestone 67,
//! notes/swish-language.md).
//!
//! This is the outermost split of a typed line, one level above [`grant_plan::line::split`]. It
//! takes what a person typed and cuts it into **segments**, each of which is a whole command line
//! in the sense everything below this already understood: operators, prefix words, redirections,
//! the lot. So nothing under here learned a new grammar.
//!
//! ```text
//! line      := segment (connector segment)*
//! connector := ';' | '&&' | '||'
//! segment   := <a whole command line: stages, operators, prefix words>
//! ```
//!
//! # Why the split is outermost, which is a decision and not a layering accident
//!
//! `caps`, `time` and `xargs` are prefix words whose operand is **a whole command line**, which is
//! why [`crate::route`] answers them before the line is split on `|` (a `caps` that saw only `date`
//! would preview a line the user did not type). Sequencing has the opposite need: `time a && b`
//! must time `a` and then run `b`, not hand `time` the string `a && b`.
//!
//! Splitting here first gives that for free, and it is also bash's binding: a connector joins
//! pipelines, and `time` applies to one pipeline. The cost is named in BUGS.
//!
//! # A connector carries one bit and no capability
//!
//! This is where a shell usually leaks. On Unix a `&&` chain runs inside one process holding one
//! ambient authority, so "what the second command may touch" is a question the connector never has
//! to answer. Here it does, and the answer is that **each segment is planned from scratch against
//! what the shell holds**, exactly as if it had been typed alone:
//!
//! - No file the first segment opened is in scope for the second. The shell backs a `>` itself
//!   (DECISIONS §55) and closes it when the segment is over.
//! - **No pipeline region outlives its segment.** Each pipeline splits a region off the shell's
//!   budget, mints its endpoints in it, and `DESTROY`s it when the line is over, and that destroy is
//!   what turns a stalled writer's next `SEND` into `abi::Error::Gone` (notes/pipes.md). A chain
//!   that reused one region across its segments would keep every earlier segment's endpoints alive
//!   for the whole chain, and a writer parked on the first would never be told. So teardown happens
//!   at the end of a **segment**, not at the end of a line, which falls out of the split being here
//!   rather than inside [`crate::route`].
//! - Nothing a segment was granted is named by [`Status`], which is one small
//!   integer about the shell's own reading of what happened. It designates nothing, so nothing
//!   downstream can be widened by it.
//!
//! # EXAMPLES
//!
//! ```
//! use grant_plan::Refusal;
//! use swish::sequence::{split, Joint};
//! use swish::Status;
//!
//! let s = split(b"mkdir logs && date > logs/when.txt").unwrap();
//! assert_eq!(s.len(), 2);
//! assert_eq!(s.segments()[0], (Joint::First, &b"mkdir logs"[..]));
//! assert_eq!(s.segments()[1].0, Joint::OnSuccess);
//!
//! // The second only runs if the first did.
//! assert!(Joint::OnSuccess.runs_after(Status::Ran));
//! assert!(!Joint::OnSuccess.runs_after(Status::Refused));
//! // ... and `||` is the mirror. A refusal is a "did not succeed" here, the same as a failure:
//! // `&&` asks one question and both answers are no. Which of the two it was is `$?`'s to say.
//! assert!(Joint::OnFailure.runs_after(Status::Refused));
//!
//! // A connector inside quotes is text, so `echo` can print one.
//! assert_eq!(split(b"echo 'a && b'").unwrap().len(), 1);
//! // And a connector with nothing on one side of it is refused rather than skipped.
//! assert_eq!(split(b"date &&"), Err(Refusal::EmptySegment));
//! ```
//!
//! # BUGS
//!
//! - **There is no grouping.** `a && b || c` is left to right with no precedence between `&&` and
//!   `||`, which is bash's rule, and there is no `{ }` or `( )` to override it. Subshells are
//!   milestone 52's and grouping should arrive with them.
//! - **`time a && b` times only `a`.** That is the binding above and it matches bash, but a person
//!   who wanted the chain timed has no spelling for it until grouping exists.
//! - **A single `&` is an ordinary byte**, so `date &` runs `date` with a word `&` on the line
//!   rather than backgrounding it. Job control is milestone 48's; this module deliberately reads
//!   only the doubled form, so that milestone can give `&` its meaning without taking one away.
//! - **A skipped segment leaves `$?` alone**, which is bash's rule (`false && echo hi` leaves `$?`
//!   at the `false`'s status). A reader who expected "skipped" to be its own status will not find
//!   one, because nothing ran.

use grant_plan::word::Cursor;
use grant_plan::{Refusal, trim};

use crate::Status;

/// The most commands one line will sequence. A ceiling in the same spirit as
/// [`grant_plan::line::MAX_STAGES`]: past it the line is refused rather than silently truncated,
/// and a line that wants more is better written as several lines. Must match the count in
/// [`Refusal::TooManySegments`]'s message.
pub const MAX_SEGMENTS: usize = 8;

/// **How a segment is joined to the one before it**, which is the whole of what a connector says.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Joint {
    /// The first segment of a line. Nothing came before it, so nothing conditions it.
    #[default]
    First,
    /// `;`: run it whatever the one before did. Two commands on one line, and no more than that.
    Always,
    /// `&&`: run it only if the one before succeeded.
    OnSuccess,
    /// `||`: run it only if the one before did not.
    OnFailure,
}

impl Joint {
    /// **Whether this segment runs, given what the one before it did.**
    ///
    /// `&&` and `||` ask one question ("did it succeed") and a refusal and a failure are both
    /// answers of "no". That is deliberate: a third connector that distinguished them would be
    /// inventing a control-flow word nobody has asked for, and the distinction is still there for
    /// the person, in `$?`. See [`Status`].
    pub fn runs_after(self, prev: Status) -> bool {
        match self {
            Joint::First | Joint::Always => true,
            Joint::OnSuccess => prev.ok(),
            Joint::OnFailure => !prev.ok(),
        }
    }
}

/// A typed line cut into segments. The segments are **slices of the line**, so this borrows and
/// allocates nothing, which is the same discipline [`grant_plan::line::Line`] keeps and for the
/// same reason: this shell has no allocator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sequence<'a> {
    segments: [(Joint, &'a [u8]); MAX_SEGMENTS],
    n: usize,
}

impl<'a> Sequence<'a> {
    /// The segments, in order, each with the connector that introduced it.
    pub fn segments(&self) -> &[(Joint, &'a [u8])] {
        &self.segments[..self.n]
    }

    /// How many segments the line has. Always at least one: an empty line is one empty segment,
    /// which [`grant_plan::parse`] reads as [`grant_plan::Command::Empty`], exactly as it did
    /// before this module existed.
    pub fn len(&self) -> usize {
        self.n
    }

    /// Whether the line is one segment with no connector on it, which is every line that could be
    /// typed before this module existed.
    pub fn is_plain(&self) -> bool {
        self.n == 1
    }

    /// Never true: see [`len`](Sequence::len). Present because clippy asks for it beside `len`, and
    /// because a reader who wonders is better served by a sentence than by its absence.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
}

/// **Cut a typed line into segments.** Pure and allocation-free.
///
/// A connector inside quotes is text, which is what lets `echo 'a && b'` print one. An unclosed
/// quote is refused here rather than further down, because this is the first scanner to see the
/// whole line and a line missing its end designates something nobody typed.
pub fn split(line: &[u8]) -> Result<Sequence<'_>, Refusal> {
    let mut segments = [(Joint::First, &b""[..]); MAX_SEGMENTS];
    let mut n = 0usize;
    let mut c = Cursor::new();
    let mut start = 0usize;
    let mut joint = Joint::First;
    let mut i = 0usize;

    while i < line.len() {
        // A single `|` is the pipe operator and belongs to `line::split`; only the doubled form is
        // a connector. A single `&` is left alone for milestone 48's job control.
        let (found, width) = match (c.open(), line[i]) {
            (false, b';') => (Joint::Always, 1),
            (false, b'&') if line.get(i + 1) == Some(&b'&') => (Joint::OnSuccess, 2),
            (false, b'|') if line.get(i + 1) == Some(&b'|') => (Joint::OnFailure, 2),
            _ => {
                c.step(line[i]);
                i += 1;
                continue;
            }
        };
        let seg = trim(&line[start..i]);
        if seg.is_empty() {
            return Err(Refusal::EmptySegment);
        }
        if n == MAX_SEGMENTS {
            return Err(Refusal::TooManySegments);
        }
        segments[n] = (joint, seg);
        n += 1;
        i += width;
        start = i;
        joint = found;
    }
    if c.open() {
        return Err(Refusal::UnclosedQuote);
    }

    let tail = trim(&line[start..]);
    if tail.is_empty() {
        // An empty line is one empty segment, which is a prompt press rather than a mistake. A
        // trailing `;` is the other legal emptiness: it separates rather than joins, so there is
        // nothing after it to condition. A trailing `&&` or `||` is a question with no command to
        // ask it about.
        return match (n, joint) {
            (0, _) => Ok(Sequence {
                segments,
                n: 1, // `segments[0]` is already `(Joint::First, b"")`.
            }),
            (_, Joint::Always) => Ok(Sequence { segments, n }),
            _ => Err(Refusal::EmptySegment),
        };
    }
    if n == MAX_SEGMENTS {
        return Err(Refusal::TooManySegments);
    }
    segments[n] = (joint, tail);
    Ok(Sequence { segments, n: n + 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(line: &[u8]) -> Sequence<'_> {
        split(line).unwrap_or_else(|r| panic!("{line:?} was refused: {r:?}"))
    }

    /// Every line that could be typed before this module existed is one segment, handed on
    /// untouched. The connectors must not make an ordinary command more expensive or more
    /// complicated, which is the same promise milestone 50's operators made.
    #[test]
    fn a_line_with_no_connector_is_one_segment() {
        for line in [
            &b"worker 9"[..],
            b"echo hello  world",
            b"date | wc",
            b"wc < a | wc > b",
            b"",
            b"   ",
        ] {
            let s = segs(line);
            assert_eq!(s.len(), 1, "{}", core::str::from_utf8(line).unwrap());
            assert!(s.is_plain());
            assert_eq!(s.segments()[0], (Joint::First, trim(line)));
        }
    }

    #[test]
    fn the_three_connectors_cut_the_line_and_keep_what_joined_it() {
        let s = segs(b"date ; worker 3 && echo ok || echo no");
        assert_eq!(
            s.segments(),
            [
                (Joint::First, &b"date"[..]),
                (Joint::Always, &b"worker 3"[..]),
                (Joint::OnSuccess, &b"echo ok"[..]),
                (Joint::OnFailure, &b"echo no"[..]),
            ],
        );
        // Whitespace around a connector is optional, as it is around every other operator here.
        assert_eq!(segs(b"date;wc").len(), 2);
        assert_eq!(segs(b"date&&wc").len(), 2);
    }

    /// **A single `|` is the pipe and only the doubled form is a connector.** Getting this wrong
    /// would turn every pipeline in the system into two commands.
    #[test]
    fn one_pipe_is_a_pipeline_and_two_are_a_connector() {
        let s = segs(b"echo a | wc");
        assert_eq!(s.len(), 1);
        assert_eq!(s.segments()[0].1, b"echo a | wc");

        let s = segs(b"echo a || wc");
        assert_eq!(s.len(), 2);
        assert_eq!(s.segments()[1], (Joint::OnFailure, &b"wc"[..]));

        // Three stages either side of a connector, so the two splits do not interfere.
        let s = segs(b"echo a | wc | wc && date | wc");
        assert_eq!(s.segments()[0].1, b"echo a | wc | wc");
        assert_eq!(s.segments()[1].1, b"date | wc");
    }

    /// A single `&` stays an ordinary byte, so milestone 48 can give it a meaning without this
    /// module having taken one away first.
    #[test]
    fn a_single_ampersand_is_not_a_connector() {
        let s = segs(b"echo a & b");
        assert_eq!(s.len(), 1);
        assert_eq!(s.segments()[0].1, b"echo a & b");
    }

    #[test]
    fn a_connector_inside_quotes_is_text() {
        for line in [&b"echo 'a && b'"[..], b"echo \"a ; b\"", b"echo 'a || b'"] {
            let s = segs(line);
            assert_eq!(s.len(), 1, "{}", core::str::from_utf8(line).unwrap());
        }
    }

    #[test]
    fn a_connector_needs_a_command_on_both_sides() {
        for line in [
            &b"&& date"[..],
            b"|| date",
            b"; date",
            b"date &&",
            b"date ||",
            b"date ;; wc",
            b"date && && wc",
        ] {
            assert_eq!(
                split(line),
                Err(Refusal::EmptySegment),
                "{}",
                core::str::from_utf8(line).unwrap(),
            );
        }
    }

    /// A trailing `;` is the one legal emptiness, because `;` separates rather than joins: there is
    /// nothing after it for the connector to be about. Every other shell accepts it and nobody
    /// means anything by it.
    #[test]
    fn a_trailing_semicolon_is_a_separator_with_nothing_after_it() {
        let s = segs(b"date ;");
        assert_eq!(s.segments(), [(Joint::First, &b"date"[..])]);
        assert_eq!(segs(b"date ; wc ;").len(), 2);
    }

    #[test]
    fn an_unclosed_quote_takes_the_whole_line_with_it() {
        assert_eq!(split(b"echo 'a ; b"), Err(Refusal::UnclosedQuote));
        assert_eq!(split(b"date && echo \"x"), Err(Refusal::UnclosedQuote));
    }

    #[test]
    fn there_is_a_ceiling_and_it_refuses_rather_than_truncates() {
        assert_eq!(segs(b"a;b;c;d;e;f;g;h").len(), MAX_SEGMENTS);
        assert_eq!(split(b"a;b;c;d;e;f;g;h;i"), Err(Refusal::TooManySegments));
    }

    /// **The condition table**, which is the whole semantics of `&&` and `||` in four rows. Both
    /// non-zero statuses answer "no" to the same question; which one it was is `$?`'s to report.
    #[test]
    fn a_connector_reads_one_bit_out_of_a_status_that_has_more_in_it() {
        for prev in [Status::Ran, Status::Failed, Status::Refused] {
            assert!(Joint::First.runs_after(prev));
            assert!(Joint::Always.runs_after(prev));
            assert_eq!(Joint::OnSuccess.runs_after(prev), prev == Status::Ran);
            assert_eq!(Joint::OnFailure.runs_after(prev), prev != Status::Ran);
        }
    }
}
