//! **The operators: `>`, `<` and `|`** (milestone 50, notes/pipes.md).
//!
//! This module is the *shape* of a command line, one level above [`crate::parse`]. It takes the
//! whole line and splits it into a [`Line`]: a sequence of stages, plus the file names on either
//! end. Each stage is then parsed exactly as a whole line was before this existed, which is why
//! adding three operators changed nothing about how a command is read.
//!
//! # There is one mechanism here, not three
//!
//! `>` and `|` do the same thing: they put a different capability in a program's **output slot**.
//! The program cannot tell which, and that indifference is the milestone's claim
//! (notes/sink-protocol.md). `<` is the same substitution on the **input slot**, which is the sink
//! contract read from the other end: an endpoint the program holds with `READ`, on which sink
//! messages arrive until [`sink_proto::OP_EOF`](../../sink_proto/constant.OP_EOF.html).
//!
//! So this module produces [`Sink`] and [`Source`] values and nothing else. Whether the thing on
//! the other end is a file, another program, or the shell itself is a wiring question, and no part
//! of it reaches the program.
//!
//! # The grammar
//!
//! ```text
//! line  := stage ('|' stage)*
//! stage := <command words> ('<' name | '>' name)*
//! ```
//!
//! Three rules, each of which exists to keep a line from meaning something other than it looks
//! like:
//!
//! - **A redirection goes last in its stage.** `wc < report.txt` is the spelling; `< report.txt wc`
//!   is [`Refusal::WordAfterRedirect`]. Bash accepts the second and nobody writes it, and refusing
//!   it is what lets a stage's command text be a slice of the line rather than something
//!   reassembled (this shell has no allocator).
//! - **Only the first stage takes input and only the last is redirected.** `a > f | b` names a
//!   destination for `a` and then pipes `a` somewhere else, which is two answers to one question;
//!   it is [`Refusal::RedirectMidPipeline`] rather than a silent precedence rule.
//! - **A redirection names one file.** A pattern there is [`Refusal::PatternInRedirect`]: a
//!   redirection designates a single destination, and a set does not designate one.
//!
//! # EXAMPLES
//!
//! ```
//! # use capsh::line::{split, Line};
//! let l = split(b"date | wc").unwrap();
//! assert_eq!(l.stages(), [&b"date"[..], b"wc"]);
//! assert!(l.input.is_none() && l.output.is_none());
//!
//! let l = split(b"date > report.txt").unwrap();
//! assert_eq!(l.stages(), [&b"date"[..]]);
//! assert_eq!(l.output, Some(&b"report.txt"[..]));
//!
//! // Whitespace around an operator is optional, as it is in every shell.
//! assert_eq!(split(b"date>report.txt").unwrap().output, Some(&b"report.txt"[..]));
//! ```
//!
//! # BUGS
//!
//! - **No `>>`.** Every sink this contract can build appends already (a sink has no seek), so `>`
//!   and `>>` would differ only in whether the file is emptied first. That choice belongs to the
//!   sink's wiring rather than to the writer, and until there is a second spelling worth having,
//!   one operator that truncates is the honest surface. notes/pipes.md records the fork.
//! - **No `2>`.** There is one output slot. A second stream is a second capability and a second
//!   convention, and inventing one before a program has two things to say would be inventing the
//!   convention rather than discovering it.
//! - **A redirection cannot be quoted away.** There is no quoting in this shell at all, so a file
//!   whose name contains `>` cannot be named. That is a gap in the tokenizer, not in this module.

use crate::Refusal;

/// The most stages one pipeline may carry. Four is past what anybody types interactively and it is
/// a real ceiling rather than a buffer size: each stage costs a process and an endpoint, and a line
/// that wants more is better written as two lines than silently truncated.
pub const MAX_STAGES: usize = 4;

/// **Where a stage's bytes go**, which is the whole of what `>` and `|` decide.
///
/// The three variants are three different capabilities in one slot. The program holds an endpoint
/// with `WRITE` in every case and has no message it could send to ask which.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Sink {
    /// The shell's own result endpoint: what a program is given when nothing on the line redirects
    /// it. Not "the terminal": the shell reads the bytes and decides what to do with them, which is
    /// what makes the default destination a capability like any other.
    #[default]
    Report,
    /// The next stage of a pipeline. An endpoint the shell minted, held `WRITE` here and `READ` by
    /// the stage downstream, and nothing else exists: **the rendezvous is the pipe**.
    Pipe,
    /// A file, named on the line. Delivered as an endpoint served by a sink process
    /// (`user/src/sink.rs`), so what the program holds cannot seek, truncate, re-read or stat.
    File(crate::FileGrant),
}

/// **Where a stage's bytes come from**, which is what `<` and the right-hand side of `|` decide.
/// [`Sink`]'s mirror, and deliberately the same contract read from the other end.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Source {
    /// No input slot at all. A program declaring [`InputSpec::Forbidden`](crate::InputSpec) always
    /// has this, and it is the reason such a program can never block waiting for bytes nobody is
    /// going to send.
    #[default]
    None,
    /// The previous stage of a pipeline.
    Pipe,
    /// A file, streamed out over the sink contract by an adapter process.
    File(crate::FileGrant),
}

/// A whole command line with the operators taken out of it: the stages, and the names on the ends.
///
/// The stages are **slices of the line**, so this borrows and allocates nothing. Each is parsed
/// with [`crate::parse`], which is the same function that read a whole line before this module
/// existed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Line<'a> {
    stages: [&'a [u8]; MAX_STAGES],
    nstages: usize,
    /// The name after `<`, if the line has one. It belongs to the first stage.
    pub input: Option<&'a [u8]>,
    /// The name after `>`, if the line has one. It belongs to the last stage.
    pub output: Option<&'a [u8]>,
}

impl<'a> Line<'a> {
    /// The stages, in order, left to right.
    pub fn stages(&self) -> &[&'a [u8]] {
        &self.stages[..self.nstages]
    }

    /// How many stages the line has. Always at least one; an empty line is one empty stage, which
    /// [`crate::parse`] reads as [`Command::Empty`](crate::Command::Empty).
    pub fn len(&self) -> usize {
        self.nstages
    }

    /// Whether the line is a single stage with nothing on either end, which is every line that
    /// could be typed before this module existed. The shell's old path is kept for exactly these,
    /// so nothing that worked before pays for the operators.
    pub fn is_plain(&self) -> bool {
        self.nstages == 1 && self.input.is_none() && self.output.is_none()
    }

    /// What stage `i`'s output slot holds: the pipe when something is downstream, the file when the
    /// line named one, and the shell's result endpoint otherwise.
    ///
    /// `file` is the caller's already-designated [`crate::FileGrant`], because resolving a name
    /// needs the shell's position and this module deliberately knows nothing about directories.
    pub fn sink_for(&self, i: usize, file: Option<crate::FileGrant>) -> Sink {
        if i + 1 < self.nstages {
            Sink::Pipe
        } else {
            match file {
                Some(g) => Sink::File(g),
                None => Sink::Report,
            }
        }
    }

    /// What stage `i`'s input slot holds. The mirror of [`sink_for`](Line::sink_for).
    pub fn source_for(&self, i: usize, file: Option<crate::FileGrant>) -> Source {
        if i > 0 {
            Source::Pipe
        } else {
            match file {
                Some(g) => Source::File(g),
                None => Source::None,
            }
        }
    }
}

/// Whether this byte is one of the three operators.
fn is_op(b: u8) -> bool {
    b == b'|' || b == b'<' || b == b'>'
}

/// **Split a command line into stages and redirection targets.**
///
/// Pure and allocation-free: every slice it returns points into `line`. See the module note for the
/// three rules it enforces and why each one exists.
pub fn split(line: &[u8]) -> Result<Line<'_>, Refusal> {
    let mut stages = [&b""[..]; MAX_STAGES];
    let mut ins: [Option<&[u8]>; MAX_STAGES] = [None; MAX_STAGES];
    let mut outs: [Option<&[u8]>; MAX_STAGES] = [None; MAX_STAGES];
    let mut n = 0usize;

    let mut i = 0usize;
    loop {
        if n == MAX_STAGES {
            return Err(Refusal::TooManyStages);
        }
        // The command words: everything up to the first operator or the end of the line.
        let start = i;
        while i < line.len() && !is_op(line[i]) {
            i += 1;
        }
        stages[n] = crate::trim(&line[start..i]);

        // Then the redirections, which must all come after the command words.
        while i < line.len() && (line[i] == b'<' || line[i] == b'>') {
            let op = line[i];
            i += 1;
            while i < line.len() && line[i].is_ascii_whitespace() {
                i += 1;
            }
            let t = i;
            while i < line.len() && !line[i].is_ascii_whitespace() && !is_op(line[i]) {
                i += 1;
            }
            if i == t {
                return Err(Refusal::NoRedirectTarget);
            }
            let target = &line[t..i];
            let slot = if op == b'<' { &mut ins } else { &mut outs };
            if slot[n].is_some() {
                return Err(Refusal::RedirectRepeated);
            }
            slot[n] = Some(target);
            // A word after the name is the `< f wc` spelling. Refused rather than reordered: see
            // the module note.
            let mut j = i;
            while j < line.len() && line[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < line.len() && !is_op(line[j]) {
                return Err(Refusal::WordAfterRedirect);
            }
            i = j;
        }

        n += 1;
        if i >= line.len() {
            break;
        }
        // The only operator that can be here is `|`, because the loop above consumed every `<` and
        // `>` it could reach.
        debug_assert_eq!(line[i], b'|');
        i += 1;
    }

    // A pipeline stage with no command in it. A single empty stage is the empty line, which is a
    // prompt press and not a mistake.
    if n > 1 && stages[..n].iter().any(|s| s.is_empty()) {
        return Err(Refusal::EmptyStage);
    }
    // Input belongs to the head and output to the tail. Anywhere else the line is answering the
    // same question twice.
    for k in 0..n {
        if ins[k].is_some() && k != 0 {
            return Err(Refusal::RedirectMidPipeline);
        }
        if outs[k].is_some() && k + 1 != n {
            return Err(Refusal::RedirectMidPipeline);
        }
    }
    // A redirection designates one destination, so a pattern in one designates the wrong number of
    // things. Refused here, where the token is read, rather than expanded and then counted.
    for t in [ins[0], outs[n - 1]].into_iter().flatten() {
        if glob::has_magic(t) {
            return Err(Refusal::PatternInRedirect);
        }
    }

    Ok(Line {
        stages,
        nstages: n,
        input: ins[0],
        output: outs[n - 1],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line every command was before this module existed: one stage, no operators, and the text
    /// handed on untouched.
    #[test]
    fn a_line_with_no_operator_is_one_stage() {
        for line in [&b"worker 9"[..], b"echo hello  world", b"", b"   "] {
            let l = split(line).unwrap();
            assert_eq!(l.len(), 1, "{}", core::str::from_utf8(line).unwrap());
            assert!(l.is_plain());
            assert_eq!(l.stages()[0], crate::trim(line));
        }
    }

    #[test]
    fn a_pipe_splits_and_trims() {
        let l = split(b"date | wc").unwrap();
        assert_eq!(l.stages(), [&b"date"[..], b"wc"]);
        assert!(!l.is_plain());
        // No spaces at all, which is legal in every shell and must mean the same thing here.
        assert_eq!(split(b"date|wc").unwrap().stages(), [&b"date"[..], b"wc"]);
        // Three stages, because a pipeline is not a special case of two.
        assert_eq!(
            split(b"echo a | wc | wc").unwrap().stages(),
            [&b"echo a"[..], b"wc", b"wc"],
        );
    }

    #[test]
    fn redirections_come_off_the_ends() {
        let l = split(b"date > report.txt").unwrap();
        assert_eq!(l.stages(), [&b"date"[..]]);
        assert_eq!(l.output, Some(&b"report.txt"[..]));
        assert_eq!(l.input, None);

        let l = split(b"wc < report.txt").unwrap();
        assert_eq!(l.stages(), [&b"wc"[..]]);
        assert_eq!(l.input, Some(&b"report.txt"[..]));

        // Both ends of one stage, and both ends of a pipeline.
        let l = split(b"wc <in >out").unwrap();
        assert_eq!((l.input, l.output), (Some(&b"in"[..]), Some(&b"out"[..])));
        let l = split(b"wc < in | wc > out").unwrap();
        assert_eq!(l.stages(), [&b"wc"[..], b"wc"]);
        assert_eq!((l.input, l.output), (Some(&b"in"[..]), Some(&b"out"[..])));
    }

    /// The operator does not need whitespace, and the name it takes stops at the next operator.
    #[test]
    fn an_operator_needs_no_whitespace() {
        let l = split(b"date>report.txt").unwrap();
        assert_eq!(l.stages(), [&b"date"[..]]);
        assert_eq!(l.output, Some(&b"report.txt"[..]));
        let l = split(b"wc<in>out").unwrap();
        assert_eq!((l.input, l.output), (Some(&b"in"[..]), Some(&b"out"[..])));
    }

    #[test]
    fn a_redirection_needs_a_name() {
        for line in [&b"date >"[..], b"date > ", b"wc <", b"date > | wc"] {
            assert_eq!(
                split(line),
                Err(Refusal::NoRedirectTarget),
                "{}",
                core::str::from_utf8(line).unwrap(),
            );
        }
    }

    /// Two answers to one question. Neither "the last wins" nor "the first wins" is a rule anybody
    /// should have to remember, so the line is refused.
    #[test]
    fn a_stage_has_one_input_and_one_output() {
        assert_eq!(split(b"date > a > b"), Err(Refusal::RedirectRepeated));
        assert_eq!(split(b"wc < a < b"), Err(Refusal::RedirectRepeated));
        // But one of each is fine, in either order.
        assert!(split(b"wc < a > b").is_ok());
        assert!(split(b"wc > b < a").is_ok());
    }

    /// The pipeline already says where the middle stages' bytes go. A redirection there would be a
    /// second, contradictory answer.
    #[test]
    fn only_the_ends_of_a_pipeline_may_be_redirected() {
        assert_eq!(split(b"date > f | wc"), Err(Refusal::RedirectMidPipeline));
        assert_eq!(split(b"date | wc < f"), Err(Refusal::RedirectMidPipeline));
        assert_eq!(
            split(b"echo a | wc > f | wc"),
            Err(Refusal::RedirectMidPipeline),
        );
        // And the legal shape: input on the head, output on the tail.
        let l = split(b"wc < a | wc > b").unwrap();
        assert_eq!((l.input, l.output), (Some(&b"a"[..]), Some(&b"b"[..])));
    }

    #[test]
    fn a_word_may_not_follow_a_redirection() {
        assert_eq!(split(b"< f wc"), Err(Refusal::WordAfterRedirect));
        assert_eq!(split(b"date > f extra"), Err(Refusal::WordAfterRedirect));
        // A following *operator* is not a word, so this stays legal.
        assert!(split(b"wc < f | wc").is_ok());
    }

    #[test]
    fn a_pipeline_stage_must_have_a_command_in_it() {
        for line in [&b"| wc"[..], b"date |", b"date || wc", b"|"] {
            assert_eq!(
                split(line),
                Err(Refusal::EmptyStage),
                "{}",
                core::str::from_utf8(line).unwrap(),
            );
        }
        // The empty line is one empty stage and not a pipeline, so it is not this refusal.
        assert!(split(b"").unwrap().is_plain());
    }

    #[test]
    fn a_pipeline_has_a_ceiling() {
        assert!(split(b"echo a | wc | wc | wc").is_ok());
        assert_eq!(
            split(b"echo a | wc | wc | wc | wc"),
            Err(Refusal::TooManyStages)
        );
    }

    /// A redirection designates one destination; a pattern designates a set. Refused where the
    /// token is read rather than expanded and then counted, because the count is not the point:
    /// even a pattern that matched exactly one name is a set that happens to have one member, and
    /// letting it through would make what a line writes to depend on what is in the directory.
    #[test]
    fn a_pattern_may_not_name_a_redirection() {
        assert_eq!(split(b"date > *.txt"), Err(Refusal::PatternInRedirect));
        assert_eq!(split(b"wc < log?.txt"), Err(Refusal::PatternInRedirect));
        // A name with no magic in it is untouched.
        assert!(split(b"date > report.txt").is_ok());
    }

    /// [`Line::sink_for`] and [`Line::source_for`] are the whole of "which capability goes in which
    /// slot", so the table they implement is worth pinning: the head reads, the tail writes, and the
    /// joints are pipes.
    #[test]
    fn the_ends_of_a_pipeline_are_the_only_ones_that_touch_a_file() {
        let l = split(b"echo a | wc | wc").unwrap();
        assert_eq!(l.sink_for(0, None), Sink::Pipe);
        assert_eq!(l.sink_for(1, None), Sink::Pipe);
        assert_eq!(l.sink_for(2, None), Sink::Report);
        assert_eq!(l.source_for(0, None), Source::None);
        assert_eq!(l.source_for(1, None), Source::Pipe);
        assert_eq!(l.source_for(2, None), Source::Pipe);
    }
}
