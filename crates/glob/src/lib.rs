//! **glob: does this pattern match this name?** (milestone 47, the navigation-and-naming lane).
//!
//! A pattern and a name go in, a `bool` comes out. That is the whole crate. It performs no IO,
//! makes no syscalls, reads no directory, allocates nothing, and holds no capability. It does not
//! know what a file is.
//!
//! That narrowness is the point rather than an accident of scheduling. Milestone 47's finding is
//! that the interesting question about globbing here is not *how to match*, which is a solved
//! problem with fifty years of prior art, but **what a match grants**: `rm *.txt` with five hundred
//! hits must convey exactly those five hundred files and nothing else, which the roadmap answers
//! with an `fs_file_caretaker` attenuated to a **name set** rather than a directory capability.
//! That wiring needs the directory-capability verb and the shell's expansion path, and it is a
//! different lane. What is left over is exactly this: a total function on two byte strings, which
//! is the part that can be machine-checked.
//!
//! The property the rest of the milestone wants from this crate is small and worth stating: **the
//! expansion you see is the grant.** `echo *.txt` prints literally the authority `rm *.txt` would
//! transfer, because the matched set *is* the namespace the caretaker will serve. That only means
//! anything if the matcher is the same matcher in both places and cannot be talked into a different
//! answer, so it lives here, once, with proofs.
//!
//! DECISIONS §46 rule 1 is why it is written rather than depended on: it is on the verification
//! path, and you cannot restructure someone else's crate to make a model checker tractable.
//!
//! # What a pattern is
//!
//! | syntax | meaning |
//! |---|---|
//! | `*` | any run of bytes, including none |
//! | `?` | exactly one byte |
//! | `[abc]` | one byte from the set |
//! | `[a-z]` | one byte from the inclusive range |
//! | `[!abc]`, `[^abc]` | one byte *not* in the set |
//! | `\x` | the byte `x`, with no special meaning |
//! | anything else | itself |
//!
//! Everything is **bytes**, not characters. A name on this system is a byte string
//! (`fs_proto::grant::MAX_NAME` is sixteen of them), and a matcher that decoded UTF-8 would have to
//! decide what to do with a name that is not valid UTF-8, which is a question the filesystem does
//! not ask. `?` therefore matches one *byte*, so it matches half of a two-byte UTF-8 character. That
//! is the same behaviour as `fnmatch(3)` in the C locale and as every shell running under it.
//!
//! # There is no error case
//!
//! [`matches()`] is total and infallible. There is no `Result`, because there is no malformed
//! pattern: a `[` with no closing `]` is a literal `[`, and a trailing `\` is a literal `\`. Both
//! follow `fnmatch(3)` and bash, and both matter here for a reason beyond compatibility. A pattern
//! is untrusted input, typed by a person or produced by a program, and the alternative to "it means
//! itself" is an error path in the shell's grant planner for every typo. `ls [foo` should list the
//! file called `[foo` if there is one, which is what it does.
//!
//! # A leading dot, and why the default is the smaller grant
//!
//! By default (`Dot::Special`, what [`matches()`] uses) a name beginning with `.` is matched only by a
//! pattern beginning with a literal `.`. So `*` does not match `.config`, and neither does `?` match
//! `.`, and neither does `[.]config`, which is glibc's rule for `FNM_PERIOD` exactly. Pass
//! [`Dot::Ordinary`] to [`matches_with`] to turn the rule off.
//!
//! This is a *policy*, not a matching fact, and in fnmatch it is a flag for that reason. It is the
//! default here because of what a match means in this system: `rm *` hands the child authority over
//! everything it matched, so the default should be the interpretation that grants less. A user who
//! wants the dotfiles asks for them by name, and gets the larger grant deliberately.
//!
//! # `**` is not here, and that is a decision
//!
//! Recursive descent (`**/*.rs`) is **out of scope for this crate, permanently, not "not yet".** Two
//! reasons, and the second is the real one.
//!
//! - **It needs a path separator, and the matcher still has no business owning one.** Milestone 47
//!   settled the syntax on 2026-08-18 and it settled it Plan 9's way: a path is resolved in the
//!   client, `/` is the root of the holder's own namespace, and the FS server still sees one
//!   component per request. That answers the question this bullet was waiting on and does not
//!   change the answer, because the resolution lives in `grant_plan::nav` and in the `std` PAL,
//!   both of which walk components against capabilities. A matcher that also split paths would be
//!   a second, unauthorised resolver.
//! - **`**` is not a matching feature. It is a traversal feature.** `*` says "consider these bytes";
//!   `**` says "and also descend into that directory, and the one below it". Descending means
//!   opening a subdirectory, which in this system means holding a capability for it, which is
//!   enumeration and granting. Putting `**` in a string matcher hides an authority question inside
//!   a pure function, which is the exact mistake this OS exists to not make.
//!
//! So this crate matches **one name**, a single path component, the thing `fs_proto` actually
//! carries. When path syntax is settled, recursive descent lands as a traversal layer *above* this
//! crate, walking directory capabilities and calling [`matches()`] per component. That layer is where
//! `**` belongs, because that is where the authority to descend is.
//!
//! **The honest cost, stated where you meet it:** nothing here treats `/` as special. Hand
//! [`matches()`] a whole path and `*` will happily match across separators, because to this crate a
//! `/` is a byte like any other. That is a caller error, not a mode. The type system cannot catch it
//! while a name is `&[u8]`, so it is written down instead.
//!
//! # Glob qualifiers are out of scope, and the reason is authority
//!
//! zsh's qualifiers are the best thing in its glob engine (`*(.)` for regular files, `*(om[1])` for
//! the newest, `*(Lm+1)` for over a megabyte) and none of them are here. The roadmap says to settle
//! this **before** building the matcher around them, so: settled, out, and this crate is not built
//! around them.
//!
//! It is not squeamishness about scope. A qualifier needs type, mtime and size **per candidate**, so
//! one `enumerate` becomes N `FSTAT` calls and needs a **read right beyond enumerate**. That turns
//! `echo *(.)`, which reads like a display, into an operation that requires more authority than
//! listing the directory. In a capability system that is a change to what the command is, not a
//! feature flag. If qualifiers ever arrive they arrive as a separate, visible step over an already
//! enumerated set, with the extra right named, and the matcher stays a function of two byte strings.
//!
//! POSIX character classes (`[[:alpha:]]`) are out for a smaller reason: the inner `[:` and `:]` are
//! not syntax here, so `[[:alpha:]]` parses as the class `[[:alpha:]` (members `[`, `:`, `a`, `l`,
//! `p`, `h`) followed by a literal `]`, and matches the two-byte names `[]`, `:]`, `a]`, `l]`, `p]`
//! and `h]`. Locale-dependent classes have no meaning on a system with no locale, and a wrong answer
//! that is quiet is worse than a missing feature, so it is written down here and pinned by a test.
//!
//! # Why it cannot be made to hang
//!
//! A pattern is untrusted input, so "pathological pattern makes the shell stop responding" is a
//! denial of service, not a performance note. The classic way to earn one is the recursive matcher
//! everybody writes first: `a*a*a*a*b` against a long run of `a` makes it try every way of splitting
//! the run between the stars, which is exponential.
//!
//! This matcher does not backtrack that way. It keeps **one** backtrack point, the most recent `*`,
//! and on a mismatch retries with that `*` absorbing one more byte. A later `*` overwrites the
//! saved one, because anything an earlier `*` could have absorbed a later one can absorb instead.
//! The consequence is that the retry position advances monotonically and never resets: at most
//! `name.len() + 1` retries, each costing at most one pass over the pattern.
//!
//! That bound is not left as prose. [`cost_bound`] computes it from the two lengths alone, before
//! any matching happens, and [`match_steps`] reports what a match actually cost. Kani proves
//! `match_steps(..).1 <= cost_bound(..)` over arbitrary short inputs, which is what says the
//! accounting is sound; a host test then runs the exponential-shaped pattern against a hundred
//! thousand bytes and checks the same inequality, which is what says the matcher is. A caller that
//! wants to refuse an expensive pattern before running it can ask [`cost_bound`] and never start.
//!
//! # EXAMPLES
//!
//! ```
//! use glob::{matches, matches_with, has_magic, literal, Dot};
//!
//! assert!(matches(b"*.txt", b"notes.txt"));
//! assert!(!matches(b"*.txt", b"notes.md"));
//!
//! // One byte, not one character, and not a leading dot.
//! assert!(matches(b"?ar", b"bar"));
//! assert!(!matches(b"*", b".hidden"));
//! assert!(matches_with(b"*", b".hidden", Dot::Ordinary));
//!
//! // Classes, ranges, negation.
//! assert!(matches(b"[abc]at", b"cat"));
//! assert!(matches(b"log[0-9].txt", b"log7.txt"));
//! assert!(matches(b"[!a]at", b"cat"));
//! assert!(!matches(b"[!c]at", b"cat"));
//!
//! // A malformed pattern is a literal, not an error.
//! assert!(matches(b"[foo", b"[foo"));
//!
//! // The shell asks two questions of a word: is it a pattern at all, and if not, what name is it?
//! assert!(has_magic(b"*.txt"));
//! assert!(!has_magic(br"a\*b"));
//! let mut buf = [0u8; 16];
//! assert_eq!(literal(br"a\*b", &mut buf), Some(&b"a*b"[..]));
//! assert_eq!(literal(b"*.txt", &mut buf), None); // it has magic; expand it instead
//! ```
//!
//! # Examples
//!
//! ```
//! use glob::{matches, has_magic};
//!
//! assert!(matches(b"*.rs", b"main.rs"));
//! assert!(!matches(b"*.rs", b"main.c"));
//! assert!(matches(b"src/?????.rs", b"src/hello.rs"));
//! assert!(matches(b"[abc]at", b"cat"));
//!
//! // A pattern with no metacharacters is just a name, which callers use to skip the walk.
//! assert!(has_magic(b"*.rs"));
//! assert!(!has_magic(b"Cargo.toml"));
//! ```
//!
//! A leading dot is special, the way a shell user expects, and [`matches_with`] is how you opt out:
//!
//! ```
//! use glob::{matches, matches_with, Dot};
//!
//! assert!(!matches(b"*", b".config"));                       // `*` does not list dotfiles
//! assert!(matches(b".*", b".config"));                       // an explicit dot does
//! assert!(matches_with(b"*", b".config", Dot::Ordinary));    // ... unless you ask otherwise
//! ```
//!
//! Name: ratified 2026-08-01 (calef, the naming tenet in CLAUDE.md). Named in the group of standard
//! terms that are already right and must not be touched, because a name a reader knows from outside
//! this project costs nothing to learn and renaming it would destroy the recognition the tenet
//! exists to buy.

#![no_std]

/// Whether a leading `.` in the name is special.
///
/// This is `fnmatch(3)`'s `FNM_PERIOD` as a two-valued type rather than a flag bit, because there
/// are exactly two answers and the compiler should be able to say which one a call site chose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dot {
    /// A name starting with `.` matches only a pattern starting with a **literal** `.` (which `\.`
    /// counts as, and `[.]` does not). What [`matches()`] uses, and what a shell user expects: `*`
    /// does not list `.config`.
    Special,
    /// A leading `.` is an ordinary byte. `*` matches `.config`. For callers matching against
    /// something that is not a user-facing filename listing.
    Ordinary,
}

/// **Does `pattern` match `name`?** A leading `.` in the name is special; see [`Dot`].
///
/// Total: every pair of byte strings has an answer, and no pattern makes this panic or hang.
pub fn matches(pattern: &[u8], name: &[u8]) -> bool {
    match_steps(pattern, name, Dot::Special).0
}

/// [`matches()`], with the leading-dot rule under the caller's control.
pub fn matches_with(pattern: &[u8], name: &[u8], dot: Dot) -> bool {
    match_steps(pattern, name, dot).0
}

/// **Could this pattern match anything other than one fixed name?**
///
/// True when the pattern contains an unescaped `*` or `?`, or a bracket expression that is actually
/// closed. False means the pattern is a literal, and [`literal`] will give you the name it is.
///
/// The shell needs this before it plans a grant: a word with no magic names one file and wants a
/// single-name caretaker, while a word with magic is an enumeration and wants a caretaker over the
/// matched set. Answering "yes" too eagerly is not a correctness bug (expanding a literal yields
/// that one name), but it costs a directory enumeration for nothing, so this is precise: an
/// unterminated `[` is a literal byte and is reported as such.
pub fn has_magic(pattern: &[u8]) -> bool {
    let mut j = 0;
    while let Some((elem, _)) = decode(pattern, j, 0) {
        match elem {
            Elem::Byte { next, .. } => j = next,
            _ => return true,
        }
    }
    false
}

/// **The one name a magic-free pattern names**, with its escapes stripped, written into `out`.
///
/// `Some(name)` when the pattern has no magic and `out` was long enough; `None` when it has magic
/// (call the expander instead) or would not fit.
///
/// This exists so escape-stripping is not reimplemented by every caller that has to turn a shell
/// word into a filename. `a\*b` is the file called `a*b`, and getting that wrong means asking the
/// filesystem for a name nobody has.
pub fn literal<'a>(pattern: &[u8], out: &'a mut [u8]) -> Option<&'a [u8]> {
    let mut j = 0;
    let mut n = 0;
    while let Some((elem, _)) = decode(pattern, j, 0) {
        match elem {
            Elem::Byte { byte, next } => {
                *out.get_mut(n)? = byte;
                n += 1;
                j = next;
            }
            _ => return None,
        }
    }
    Some(&out[..n])
}

/// [`matches_with`], plus the number of steps it took: the cost of the match, made observable.
///
/// A "step" is one byte of pattern examined, including the bytes a bracket expression is scanned
/// over and the ones re-examined after a backtrack. It is not a wall-clock measure and it is not
/// stable across changes to this crate; it exists so the claim "this cannot blow up" is an
/// inequality something checks rather than a paragraph, by way of [`cost_bound`].
pub fn match_steps(pattern: &[u8], name: &[u8], dot: Dot) -> (bool, usize) {
    let mut steps = 0usize;

    // The leading-dot rule, applied once and up front. It is a property of the two strings' first
    // bytes, so putting it in the loop would only mean asking it repeatedly.
    if dot == Dot::Special && name.first() == Some(&b'.') && !opens_with_a_literal_dot(pattern) {
        return (false, steps);
    }

    let n = name.len();
    let mut i = 0usize; // next byte of `name` to account for
    let mut j = 0usize; // next element of `pattern` to apply

    // The single backtrack point: where to resume in the pattern after the most recent `*`, and how
    // much of the name that `*` is currently absorbing. `None` until a `*` is seen, and a later `*`
    // simply replaces it. This is the whole anti-blowup argument (see the module comment).
    let mut star_next: Option<usize> = None;
    let mut star_absorbed = 0usize;

    loop {
        if i >= n {
            // The name is spent. Nothing can be gained by backtracking, since that only makes the
            // `*` absorb more of a name that has no more bytes, so the answer is fixed: whatever is
            // left of the pattern has to be able to match emptiness, and only `*` can.
            while pattern.get(j) == Some(&b'*') {
                j += 1;
                steps += 1;
            }
            return (j == pattern.len(), steps);
        }

        let advanced = match decode(pattern, j, name[i]) {
            // Pattern exhausted with name left over. Not a match here; the backtrack below decides
            // whether an earlier `*` can swallow the remainder.
            None => false,
            Some((elem, scanned)) => {
                steps += scanned;
                match elem {
                    Elem::Star => {
                        star_next = Some(j + 1);
                        star_absorbed = i;
                        j += 1;
                        true
                    }
                    Elem::Any { next } => {
                        i += 1;
                        j = next;
                        true
                    }
                    Elem::Byte { byte, next } => {
                        if byte == name[i] {
                            i += 1;
                            j = next;
                            true
                        } else {
                            false
                        }
                    }
                    Elem::Class { hit, next } => {
                        if hit {
                            i += 1;
                            j = next;
                            true
                        } else {
                            false
                        }
                    }
                }
            }
        };

        if advanced {
            continue;
        }

        match star_next {
            // Retry with the last `*` absorbing one more byte. `star_absorbed` only ever grows, and
            // it is bounded by the name's length, which is what makes the total work polynomial.
            Some(resume) => {
                star_absorbed += 1;
                i = star_absorbed;
                j = resume;
                steps += 1;
            }
            None => return (false, steps),
        }
    }
}

/// **The most steps a match of these two lengths can cost**, computable before matching starts.
///
/// The derivation, since a bound nobody can check is a number: the retry position advances on every
/// backtrack and is bounded by the name length, so there are at most `name_len + 1` backtracks;
/// between two of them `i + j` strictly increases and is bounded by `name_len + pattern_len`; and
/// each iteration examines at most one pass over the pattern to decode an element and one more to
/// test a bracket expression. Saturating, so a pair of absurd lengths gives `usize::MAX` rather than
/// a wrapped answer that would make the inequality vacuous.
///
/// It is a *bound*, deliberately loose rather than tight. What it is for is the shape: polynomial in
/// both lengths, with no term that grows with the number of `*`s in the pattern. That last part is
/// the whole difference between this and the matcher everyone writes first.
pub const fn cost_bound(pattern_len: usize, name_len: usize) -> usize {
    let iterations = name_len
        .saturating_add(2)
        .saturating_mul(name_len.saturating_add(pattern_len).saturating_add(1));
    iterations
        .saturating_mul(pattern_len.saturating_mul(2).saturating_add(2))
        .saturating_add(pattern_len)
        .saturating_add(2)
}

// ---- the pattern's elements ------------------------------------------------------------------

/// One element of a pattern, **resolved against the name byte it is being applied to**.
///
/// The `c` parameter on [`decode`] is why [`Elem::Class`] carries a `hit` rather than the bounds of
/// its body, and it is a concession to the model checker rather than to taste. The first version
/// found the closing `]` in one loop and tested membership in another; Kani has to unroll both,
/// nested inside the match loop it is already unrolling, and one harness ran twelve minutes on 3.5
/// GB before it was killed. Scanning a bracket expression **once**, deciding membership as it goes,
/// removed a whole loop from the unrolling. It is also less work at runtime, which is the usual
/// shape of these: DECISIONS §46 rule 1 says restructure the code rather than weaken the claim, and
/// the restructured code is normally better anyway.
///
/// Callers with no name byte in hand ([`has_magic`], [`literal`], [`opens_with_a_literal_dot`]) pass
/// any byte and read only the variant.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Elem {
    /// `*`
    Star,
    /// `?`, and where the pattern continues after it.
    Any { next: usize },
    /// A literal byte, which may have arrived as `\x`, as an unterminated `[`, or as itself.
    Byte { byte: u8, next: usize },
    /// A closed `[...]`, and whether the byte it was asked about is in it (negation applied).
    Class { hit: bool, next: usize },
}

/// Decode the element at `j` as applied to the name byte `c`, and say how many pattern bytes that
/// cost.
///
/// `None` at the end of the pattern. Every other input decodes to something: this is where "there
/// is no malformed pattern" is implemented, by turning an unclosed `[` and a trailing `\` into the
/// bytes they are.
fn decode(pattern: &[u8], j: usize, c: u8) -> Option<(Elem, usize)> {
    let byte = *pattern.get(j)?;
    let elem = match byte {
        b'*' => (Elem::Star, 1),
        b'?' => (Elem::Any { next: j + 1 }, 1),
        b'\\' => match pattern.get(j + 1) {
            Some(&escaped) => (
                Elem::Byte {
                    byte: escaped,
                    next: j + 2,
                },
                2,
            ),
            // Nothing to escape. bash and fnmatch(3) both read this as the backslash itself.
            None => (
                Elem::Byte {
                    byte: b'\\',
                    next: j + 1,
                },
                1,
            ),
        },
        b'[' => {
            let (closed, scanned) = class_at(pattern, j, c);
            match closed {
                Some((hit, next)) => (Elem::Class { hit, next }, scanned),
                // No closing bracket anywhere, so it was never a bracket expression. The scan that
                // established that is charged for; it is the one place a step costs more than O(1).
                None => (
                    Elem::Byte {
                        byte: b'[',
                        next: j + 1,
                    },
                    scanned,
                ),
            }
        }
        _ => (Elem::Byte { byte, next: j + 1 }, 1),
    };
    Some(elem)
}

/// Walk the bracket expression opened at `open`, deciding in one pass both where it ends and whether
/// `c` is a member of it.
///
/// Returns `(Some((hit, next)), scanned)` for a closed expression, with negation already applied, or
/// `(None, scanned)` when there is no closing `]`, in which case the caller treats the `[` as a
/// literal byte. `scanned` is charged either way.
///
/// Three rules here are older than any of us and all three are load-bearing for the patterns people
/// actually type:
///
/// - A `!` or `^` immediately after the `[` negates.
/// - A `]` immediately after that is a **member**, not the terminator. It is the only way to put a
///   `]` in a class without an escape.
/// - A `-` is a range only when a byte follows it that is not the terminator, which is why `[a-]`
///   and `[-a]` both mean "a hyphen or an a".
fn class_at(pattern: &[u8], open: usize, c: u8) -> (Option<(bool, usize)>, usize) {
    let mut k = open + 1;
    let negated = matches!(pattern.get(k), Some(b'!') | Some(b'^'));
    let mut scanned = 1usize;
    if negated {
        k += 1;
        scanned += 1;
    }

    let mut hit = false;
    let mut at_start = true;

    loop {
        let Some(&byte) = pattern.get(k) else {
            return (None, scanned);
        };
        scanned += 1;
        if byte == b']' && !at_start {
            return (Some((hit ^ negated, k + 1)), scanned);
        }
        at_start = false;

        // An escape inside a class, which is how `[\]]` names a `]`. Strict POSIX bracket
        // expressions have no escapes; glibc's fnmatch and bash both honour one, and matching them
        // is worth more than matching the standard nobody implements.
        let (lo, after_lo) = if byte == b'\\' && k + 1 < pattern.len() {
            scanned += 1;
            (pattern[k + 1], k + 2)
        } else {
            (byte, k + 1)
        };

        let ranged = pattern.get(after_lo) == Some(&b'-')
            && matches!(pattern.get(after_lo + 1), Some(&b) if b != b']');
        if ranged {
            let start = after_lo + 1;
            let (hi, after_hi) = if pattern[start] == b'\\' && start + 1 < pattern.len() {
                (pattern[start + 1], start + 2)
            } else {
                (pattern[start], start + 1)
            };
            scanned += after_hi - after_lo;
            // A reversed range (`[z-a]`) is empty. POSIX calls it undefined; glibc makes it match
            // nothing, which is the answer that cannot surprise anyone into a larger grant.
            if lo <= c && c <= hi {
                hit = true;
            }
            k = after_hi;
        } else {
            if lo == c {
                hit = true;
            }
            k = after_lo;
        }
    }
}

/// Does the pattern begin with a literal `.`?
///
/// `.` and `\.` do; `[.]`, `*` and `?` do not. That is glibc's `FNM_PERIOD` rule, and the reason a
/// bracket expression does not count is that the rule exists to stop a *wildcard* reaching a
/// dotfile, and a class is a wildcard however few members it has.
fn opens_with_a_literal_dot(pattern: &[u8]) -> bool {
    matches!(
        decode(pattern, 0, 0),
        Some((Elem::Byte { byte: b'.', .. }, _))
    )
}

// ---- host tests ------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_literal_pattern_matches_itself_and_nothing_else() {
        assert!(matches(b"notes.txt", b"notes.txt"));
        assert!(!matches(b"notes.txt", b"notes.txtx"));
        assert!(!matches(b"notes.txt", b"notes.tx"));
        assert!(!matches(b"notes.txt", b""));
    }

    #[test]
    fn star_matches_any_run_including_none() {
        assert!(matches(b"*", b""));
        assert!(matches(b"*", b"a"));
        assert!(matches(b"*", b"anything at all"));
        assert!(matches(b"a*", b"a"));
        assert!(matches(b"*a", b"a"));
        assert!(matches(b"a*b", b"ab"));
        assert!(matches(b"a*b", b"axxxb"));
        assert!(!matches(b"a*b", b"axxx"));
        assert!(matches(b"**", b"anything")); // two stars, one meaning: see the module comment
    }

    #[test]
    fn question_matches_exactly_one_byte() {
        assert!(matches(b"?", b"a"));
        assert!(!matches(b"?", b""));
        assert!(!matches(b"?", b"ab"));
        assert!(matches(b"???", b"abc"));
        // One byte, not one character: `é` is two bytes in UTF-8 and takes two `?`.
        assert!(!matches(b"?", "\u{e9}".as_bytes()));
        assert!(matches(b"??", "\u{e9}".as_bytes()));
    }

    #[test]
    fn classes_ranges_and_negation() {
        assert!(matches(b"[abc]", b"b"));
        assert!(!matches(b"[abc]", b"d"));
        assert!(matches(b"[a-z]", b"q"));
        assert!(!matches(b"[a-z]", b"Q"));
        assert!(matches(b"[!a]", b"b"));
        assert!(!matches(b"[!a]", b"a"));
        assert!(matches(b"[^a]", b"b"));
        assert!(!matches(b"[^a]", b"a"));
        assert!(matches(b"log[0-9][0-9].txt", b"log42.txt"));
        assert!(!matches(b"log[0-9][0-9].txt", b"log4x.txt"));
        // A negated class still has to match a byte; it is not `*`.
        assert!(!matches(b"[!a]", b""));
        assert!(!matches(b"[!a]", b"bb"));
    }

    /// The corners real glob implementations get wrong. Each line is a documented decision in the
    /// module comment, pinned here so a rewrite has to argue with a test rather than with prose.
    #[test]
    fn the_corners_everyone_gets_wrong() {
        // An unterminated `[` is a literal `[`, not an error and not a wildcard.
        assert!(matches(b"[foo", b"[foo"));
        assert!(!matches(b"[foo", b"f"));
        assert!(matches(b"[", b"["));
        assert!(matches(b"[!", b"[!"));
        assert!(matches(b"[]", b"[]")); // `]` first is a member, so this class never closes

        // `]` as the first member, which is the only way to have one without an escape.
        assert!(matches(b"[]]", b"]"));
        assert!(matches(b"[!]]", b"a"));
        assert!(!matches(b"[!]]", b"]"));
        assert!(matches(br"[\]]", b"]"));

        // `-` first or last is a member, not a range.
        assert!(matches(b"[a-]", b"a"));
        assert!(matches(b"[a-]", b"-"));
        assert!(!matches(b"[a-]", b"b"));
        assert!(matches(b"[-a]", b"-"));
        assert!(matches(b"[-a]", b"a"));

        // A reversed range is empty.
        assert!(!matches(b"[z-a]", b"m"));
        assert!(!matches(b"[z-a]", b"z"));

        // The empty pattern matches the empty name and nothing else.
        assert!(matches(b"", b""));
        assert!(!matches(b"", b"a"));
        assert!(!matches(b"a", b""));

        // A trailing backslash is a backslash.
        assert!(matches(br"a\", br"a\"));
        assert!(matches(br"\", br"\"));
        assert!(matches(br"\*", b"*"));
        assert!(!matches(br"\*", b"anything"));
        assert!(matches(br"\\", br"\"));

        // POSIX character classes are not implemented, and this pins what the syntax does instead
        // rather than leaving a reader to discover it: a class of `[:alpha`, then a literal `]`.
        assert!(matches(b"[[:alpha:]]", b"a]"));
        assert!(matches(b"[[:alpha:]]", b":]"));
        assert!(!matches(b"[[:alpha:]]", b"z]"));
        assert!(!matches(b"[[:alpha:]]", b"a"));
    }

    #[test]
    fn a_leading_dot_is_special_by_default() {
        assert!(!matches(b"*", b".config"));
        assert!(!matches(b"?config", b".config"));
        assert!(!matches(b"[.]config", b".config"));
        assert!(matches(b".config", b".config"));
        assert!(matches(b".*", b".config"));
        assert!(matches(br"\.config", b".config"));
        assert!(!matches(b"*", b"."));
        assert!(!matches(b"*", b".."));
        assert!(matches(b".*", b".."));

        // A dot anywhere else is an ordinary byte.
        assert!(matches(b"*", b"a.config"));
        assert!(matches(b"a*", b"a.config"));

        // And the rule is switchable, because it is a policy.
        assert!(matches_with(b"*", b".config", Dot::Ordinary));
        assert!(matches_with(b"?", b".", Dot::Ordinary));
        assert!(matches_with(b"[.]config", b".config", Dot::Ordinary));
    }

    #[test]
    fn magic_and_the_literal_behind_it() {
        assert!(has_magic(b"*.txt"));
        assert!(has_magic(b"a?b"));
        assert!(has_magic(b"[ab]"));
        assert!(!has_magic(b"notes.txt"));
        assert!(!has_magic(br"a\*b"));
        assert!(!has_magic(b"[foo")); // never closes, so it is a literal `[foo`
        assert!(!has_magic(b""));

        let mut buf = [0u8; 32];
        assert_eq!(literal(b"notes.txt", &mut buf), Some(&b"notes.txt"[..]));
        assert_eq!(literal(br"a\*b", &mut buf), Some(&b"a*b"[..]));
        assert_eq!(literal(b"[foo", &mut buf), Some(&b"[foo"[..]));
        assert_eq!(literal(b"", &mut buf), Some(&b""[..]));
        assert_eq!(literal(b"*.txt", &mut buf), None);

        // Too small an output buffer is `None`, not a panic and not a truncation: a truncated
        // filename is a different file, and this is the path that decides what gets granted.
        let mut tiny = [0u8; 3];
        assert_eq!(literal(b"abcd", &mut tiny), None);
        assert_eq!(literal(b"abc", &mut tiny), Some(&b"abc"[..]));
    }

    /// A magic-free pattern matches its own literal and nothing else, which is what lets the shell
    /// skip enumeration for a word with no magic in it. Checked over every escape corner rather
    /// than asserted once.
    #[test]
    fn no_magic_means_the_literal_is_the_only_match() {
        for pattern in [
            &b"notes.txt"[..],
            br"a\*b",
            br"a\?b",
            br"\[ab\]",
            b"[foo",
            br"a\",
            b"",
            b".config",
        ] {
            let mut buf = [0u8; 32];
            let len = literal(pattern, &mut buf).expect("no magic").len();
            assert!(
                matches_with(pattern, &buf[..len], Dot::Ordinary),
                "{pattern:?} should match its own literal"
            );
            // And nothing else: a byte appended or removed breaks it.
            buf[len] = b'x';
            assert!(!matches_with(pattern, &buf[..len + 1], Dot::Ordinary));
            if len > 0 {
                assert!(!matches_with(pattern, &buf[..len - 1], Dot::Ordinary));
            }
        }
    }

    // ---- the exhaustive cross-check ----------------------------------------------------------

    /// The naive matcher everybody writes first: try every way a `*` could split the name, by
    /// recursion. Exponential, and correct by inspection, which is exactly the trade this is for.
    ///
    /// It shares [`decode`] with the real matcher on purpose. What the
    /// cross-check below compares is therefore the **search strategy** and nothing else: if the two
    /// disagree, the greedy single-backtrack loop is wrong, not the syntax.
    fn naive(pattern: &[u8], name: &[u8], j: usize, i: usize) -> bool {
        // Past the end of the name the byte does not matter: every arm that could read it returns
        // false first.
        let c = name.get(i).copied().unwrap_or(0);
        let Some((elem, _)) = decode(pattern, j, c) else {
            return i == name.len();
        };
        match elem {
            Elem::Star => {
                let mut absorbed = i;
                loop {
                    if naive(pattern, name, j + 1, absorbed) {
                        return true;
                    }
                    if absorbed == name.len() {
                        return false;
                    }
                    absorbed += 1;
                }
            }
            _ if i >= name.len() => false,
            Elem::Any { next } => naive(pattern, name, next, i + 1),
            Elem::Byte { byte, next } => byte == name[i] && naive(pattern, name, next, i + 1),
            Elem::Class { hit, next } => hit && naive(pattern, name, next, i + 1),
        }
    }

    /// **The greedy matcher agrees with exhaustive search, on every pattern and name in a domain
    /// small enough to enumerate completely.**
    ///
    /// This is the `ntp_proto` lesson applied (see notes/ntp.md): a model checker is the tool for
    /// domains too big to enumerate, not a better tool for domains that are not. Equivalence with a
    /// recursive reference is exactly the property a solver struggles with, because the reference is
    /// recursive and Kani has to unwind it, and it is exactly the property this loop settles
    /// completely. Every pattern of length 0..=5 over an alphabet chosen to contain every syntactic
    /// role (`a`, `b`, `*`, `?`, `[`, `]`, `!`, `-`, `\`) against every name of length 0..=3 over
    /// `a`, `b`, `-`: 66,430 patterns times 40 names, 2,657,200 comparisons.
    ///
    /// Five is the shortest pattern length that reaches a full range (`[a-b]`) and a full negated
    /// class (`[!ab]`), which is why it is not four.
    ///
    /// The alphabet is the interesting part. It is not "some characters"; it is one representative
    /// of each thing the decoder can do, so the enumeration reaches unterminated brackets, `]`-first
    /// classes, `[a-]`, `[!...]`, escaped metacharacters and trailing backslashes without anyone
    /// having thought to write them down.
    #[test]
    fn greedy_agrees_with_exhaustive_search_over_every_short_pattern() {
        const PAT: &[u8] = b"ab*?[]!-\\";
        const NAME: &[u8] = b"ab-";

        let mut pattern = [0u8; 5];
        let mut name = [0u8; 3];
        let mut checked = 0u64;
        let mut hits = 0u64;

        // Under Miri, every 61st pattern instead of every pattern: 61 is prime and not a power of
        // the 9-byte alphabet, so the sample still lands on every length and every syntactic role,
        // ~44,000 comparisons instead of 2,657,200. The complete enumeration, which is the claim
        // this test is named for, is native-only; "Miri-clean" means the sample (notes/undefined-behavior.md).
        let stride: u64 = if cfg!(miri) { 61 } else { 1 };
        for plen in 0..=5usize {
            let combos = (PAT.len() as u64).pow(plen as u32);
            for pc in (0..combos).step_by(stride as usize) {
                let mut v = pc;
                for slot in pattern.iter_mut().take(plen) {
                    *slot = PAT[(v % PAT.len() as u64) as usize];
                    v /= PAT.len() as u64;
                }
                let p = &pattern[..plen];
                for nlen in 0..=3usize {
                    let ncombos = (NAME.len() as u64).pow(nlen as u32);
                    for nc in 0..ncombos {
                        let mut v = nc;
                        for slot in name.iter_mut().take(nlen) {
                            *slot = NAME[(v % NAME.len() as u64) as usize];
                            v /= NAME.len() as u64;
                        }
                        let n = &name[..nlen];
                        let fast = matches_with(p, n, Dot::Ordinary);
                        let slow = naive(p, n, 0, 0);
                        assert_eq!(fast, slow, "pattern {p:?} name {n:?}");
                        checked += 1;
                        hits += u64::from(fast);
                    }
                }
            }
        }
        // The completeness pin: the enumeration (or, under Miri, the stride's exact sample of it)
        // actually ran. The Miri number is the same arithmetic at stride 61:
        // (1 + 1 + 2 + 12 + 108 + 969) patterns x 40 names.
        assert_eq!(checked, if cfg!(miri) { 43_720 } else { 2_657_200 });
        // Not vacuous in either direction: an agreement test between two matchers that both said
        // "no" to everything would pass, and most random patterns do not match most random names,
        // so the positive side is the one worth pinning. 15,956 of the pairs match natively; the
        // Miri sample keeps only the both-sides check, since its hit count is an accident of the
        // stride.
        if cfg!(miri) {
            assert!(hits > 0 && hits < checked, "{hits} matches");
        } else {
            assert!(hits > 10_000 && hits < checked - 10_000, "{hits} matches");
        }
    }

    /// **The worst case over the proof's own domain, measured rather than guessed.**
    ///
    /// This is where the `#[kani::unwind]` numbers on the harnesses come from. An unwind bound has to
    /// exceed the most iterations any loop can run, or Kani reports an unwinding-assertion failure;
    /// pick it too high instead and the formula grows for nothing, which is what put one harness past
    /// 3 GB before this test existed. Every outer iteration adds at least one to the step count, so
    /// the maximum step count over a domain is a safe upper bound for the iteration count over it.
    ///
    /// Enumerated over the same alphabet as the cross-check, at the lengths the harnesses use. The
    /// alphabet is nine bytes rather than all 256, and that is sound for this purpose because the
    /// step count depends on a byte's syntactic *role* rather than its value, and the alphabet holds
    /// one of each.
    /// Skipped under Miri rather than sampled, because sampling would be wrong here, not merely
    /// weaker: the assertions pin the exact argmax over the domain (`at_three == 10`), and a
    /// strided sample that misses the maximizing input fails the test against correct code. The
    /// enumeration is ~1.8M `match_steps` runs, an hour of interpreter for a property about the
    /// unwind-bound formula, not about memory; every path it walks is walked by the tests above.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "asserts the exact worst case over 1.8M runs; a sample would miss the argmax and \
                  fail against correct code"
    )]
    fn the_worst_case_over_the_proof_domain_is_what_the_unwind_bounds_are_set_from() {
        const PAT: &[u8] = b"ab*?[]!-\\";
        const NAME: &[u8] = b"ab-";

        let mut worst = [0usize; 5]; // indexed by max(pattern len, name len)
        let mut pattern = [0u8; 4];
        let mut name = [0u8; 4];

        for plen in 0..=4usize {
            for pc in 0..(PAT.len() as u64).pow(plen as u32) {
                let mut v = pc;
                for slot in pattern.iter_mut().take(plen) {
                    *slot = PAT[(v % PAT.len() as u64) as usize];
                    v /= PAT.len() as u64;
                }
                for nlen in 0..=4usize {
                    for nc in 0..(NAME.len() as u64).pow(nlen as u32) {
                        let mut v = nc;
                        for slot in name.iter_mut().take(nlen) {
                            *slot = NAME[(v % NAME.len() as u64) as usize];
                            v /= NAME.len() as u64;
                        }
                        let bucket = plen.max(nlen);
                        for dot in [Dot::Special, Dot::Ordinary] {
                            let (_, steps) = match_steps(&pattern[..plen], &name[..nlen], dot);
                            worst[bucket] = worst[bucket].max(steps);
                        }
                    }
                }
            }
        }

        // Cumulative, since a harness bounded at four bytes admits every shorter input too.
        let at_three = worst[0].max(worst[1]).max(worst[2]).max(worst[3]);
        let at_four = at_three.max(worst[4]);
        assert_eq!(at_three, 10, "the three-byte worst case moved");
        assert_eq!(at_four, 17, "the four-byte worst case moved");
    }

    // ---- the blowup that is not there --------------------------------------------------------

    /// **The classic backtracking bomb, at a scale that would settle the question.**
    ///
    /// `a*a*a*...*b` against a long run of `a` and no `b` is the input that makes a recursive
    /// matcher try every way of splitting the run between the stars. With ten stars and 100,000
    /// bytes that is astronomically many splits; the naive matcher above would not finish before the
    /// heat death of anything. This one answers in the time it takes to walk the name a few times,
    /// and the assertion is the interesting half: the measured step count is under the bound
    /// [`cost_bound`] computed from the two lengths **before the match ran**.
    #[test]
    fn the_pathological_pattern_stays_inside_its_bound() {
        // 100,000 bytes native; 2,000 under Miri. The linearity claim needs the scale (at 2,000 a
        // quadratic matcher would still look plausible); the interpreter cannot afford the scale
        // (the walk is ~4M matcher steps, most of an hour under Miri). Both assertions still run
        // at both sizes, so Miri checks the memory rules on the same paths and the native run
        // keeps the scale that settles the question. See notes/undefined-behavior.md.
        const NAME_LEN: usize = if cfg!(miri) { 2_000 } else { 100_000 };
        let name = [b'a'; NAME_LEN];
        let pattern = b"a*a*a*a*a*a*a*a*a*a*b";
        let (hit, steps) = match_steps(pattern, &name[..], Dot::Ordinary);
        assert!(!hit);
        assert!(
            steps <= cost_bound(pattern.len(), NAME_LEN),
            "{steps} steps exceeded the bound {}",
            cost_bound(pattern.len(), NAME_LEN)
        );

        // The measured cost is linear-ish in practice, far under the quadratic bound. Asserted
        // loosely, because the point is the order of magnitude: a bomb would be off by many.
        assert!(steps < 40 * NAME_LEN, "{steps} steps is not linear-ish");
    }

    /// The other shape of the same attack: many `?` after a star, and a name that nearly matches.
    /// This one genuinely is quadratic, and the test pins that it is quadratic rather than worse.
    #[test]
    fn a_star_followed_by_a_long_tail_stays_polynomial() {
        // Shrunk under Miri like the test above: this one is genuinely quadratic, so its native
        // 2,000 x 512 is ~1M matcher steps, minutes under an interpreter that add no new paths.
        const NAME_LEN: usize = if cfg!(miri) { 200 } else { 2_000 };
        const PATTERN_LEN: usize = if cfg!(miri) { 64 } else { 512 };
        let name = [b'a'; NAME_LEN];
        let mut pattern = [b'?'; PATTERN_LEN];
        pattern[0] = b'*';
        pattern[PATTERN_LEN - 1] = b'b';

        let (hit, steps) = match_steps(&pattern[..], &name[..], Dot::Ordinary);
        assert!(!hit);
        assert!(steps <= cost_bound(pattern.len(), NAME_LEN));
    }

    /// Every unterminated bracket in a long pattern rescans to the end, which is the one place the
    /// per-step cost is not constant. Bounded, and the bound knows about it.
    #[test]
    fn a_pattern_full_of_unterminated_brackets_stays_inside_its_bound() {
        let pattern = [b'['; 400];
        let name = [b'['; 400];
        let (hit, steps) = match_steps(&pattern[..], &name[..], Dot::Ordinary);
        assert!(hit);
        assert!(steps <= cost_bound(pattern.len(), name.len()));
    }

    /// The cost bound saturates instead of wrapping. A wrapped bound would be a small number, and
    /// every `steps <= bound` assertion in this file would then be checking nothing.
    #[test]
    fn the_cost_bound_saturates() {
        assert_eq!(cost_bound(usize::MAX, usize::MAX), usize::MAX);
        assert_eq!(cost_bound(0, 0), 6); // the empty pattern against the empty name
    }

    /// The step count is a real measure of work, not a constant. Without this the bound assertions
    /// above would pass on a counter that never moved.
    #[test]
    fn the_step_count_grows_with_the_work() {
        let short = match_steps(b"*b", b"aaaa", Dot::Ordinary).1;
        let long = match_steps(b"*b", b"aaaaaaaaaaaaaaaa", Dot::Ordinary).1;
        assert!(long > short);
        assert!(short > 0);
    }

    // ---- what mutation testing found (milestone 85) -------------------------------------------
    //
    // The first pass at these two wrote them inside `#[cfg(kani)] mod verification`, which
    // `cargo test` never compiles, so neither had ever run when they were reported as killing
    // anything. See notes/mutation-testing.md.

    /// **Each syntactic extra costs exactly what it scans**: a negation marker one step, an escape
    /// one step, a range's tail its two bytes, a trailing star one step. The step count is the
    /// DoS-bound contract, and Kani only proves it stays under [`cost_bound`], which a counter
    /// stuck at zero would also satisfy; these say what it actually counts.
    ///
    /// Asserted as differences between near-identical matches rather than as absolute totals, so
    /// the pins survive a deliberate change to the accounting while still catching a `+=` that
    /// rotted into `-=` or `*=`.
    ///
    /// **The range is asserted twice and the second one is the one that bites.** At the head of a
    /// one-member class every arithmetic mutation of `scanned += after_hi - after_lo` hides behind
    /// a coincidence: `scanned` is 2 there and the tail is 2 bytes, so `2 + 2` and `2 * 2` agree,
    /// and `4 - 2` and `4 / 2` agree too. One member in front of the range separates all four.
    #[test]
    fn each_class_feature_costs_its_own_scan() {
        let steps = |p: &[u8], n: &[u8]| match_steps(p, n, Dot::Special).1;
        assert_eq!(
            steps(b"[!b]", b"a"),
            steps(b"[b]", b"b") + 1,
            "negation marker"
        );
        assert_eq!(steps(b"[\\b]", b"b"), steps(b"[b]", b"b") + 1, "escape");
        assert_eq!(
            steps(b"[a-c]", b"b"),
            steps(b"[ac]", b"a") + 1,
            "range tail"
        );
        assert_eq!(
            steps(b"[xa-c]", b"b"),
            steps(b"[xac]", b"c") + 1,
            "range tail, past the coincidence at the head of the class"
        );
        assert_eq!(
            steps(b"ab**", b"ab"),
            steps(b"ab*", b"ab") + 1,
            "trailing star"
        );
    }

    /// An escaped `]` as a range's high end: `[A-\]]` is the range 0x41..=0x5D. The parse has to
    /// take the byte AFTER the backslash as the endpoint and resume AFTER the escape, and both
    /// indices were mutable without any test noticing.
    #[test]
    fn an_escaped_bracket_can_end_a_range() {
        assert!(matches(b"[A-\\]]", b"["), "0x5B is inside A..=0x5D");
        assert!(matches(b"[A-\\]]", b"]"), "the endpoint itself is inside");
        assert!(!matches(b"[A-\\]]", b"^"), "0x5E is one past the endpoint");
        // A plain range followed by a literal, so the resume index shows in what comes next.
        assert!(matches(b"[a-c]d", b"bd"));
        assert!(!matches(b"[a-c]d", b"bx"));
        // A class that ends in a bare escape is unterminated, never a read past the end.
        assert!(!matches(b"[a-\\", b"b"));
    }

    /// **A range covers its interior and nothing else**, which is the half of the parse no step
    /// count can see.
    ///
    /// `scanned += after_hi - after_lo` charges exactly the bytes a range skips, so a resume index
    /// that lands *inside* the range costs the same total: the loop re-scans one at a time what it
    /// should have stepped over, and the ledger balances. What does not balance is membership.
    /// Both ways of landing short widen the class, and a wider class is a larger grant, which is
    /// the one direction this crate is not allowed to be wrong in.
    #[test]
    fn a_range_admits_neither_its_hyphen_nor_a_reversed_endpoint() {
        // Resuming at the `-` would re-read it as an ordinary member. It is punctuation here, and
        // the `[a-]` and `[-a]` spellings above are where a hyphen *is* a member.
        assert!(!matches(b"[a-c]", b"-"));
        assert!(matches(b"[a-c]", b"b"));

        // Resuming at the high endpoint would re-read it as an ordinary member, which is invisible
        // for a range that contains it. A reversed range contains neither end, so it is the
        // spelling that tells the two apart.
        assert!(!matches(b"[z-a]", b"a"));
        assert!(!matches(b"[z-a]", b"z"));
    }
}

// ---- machine-checked proofs ------------------------------------------------------------------

/// The proofs, and the bounds they run under.
///
/// **Why the pattern and name are short.** Kani unrolls loops, so the match loop appears in the
/// formula once per possible iteration, and the number of possible iterations grows with the name
/// length rather than staying flat. Three bytes each is where a full run finishes in about a minute
/// on an M-series laptop. That is a real limit and it is stated rather than implied: these harnesses
/// quantify over **every byte value** at those lengths, which is 2^48 pattern-name pairs per harness
/// and reaches every arm of the decoder, but they do not quantify over length.
///
/// The length-independent claims are the host tests' job, and deliberately: the exhaustive
/// cross-check against a naive recursive matcher covers 2,657,200 pattern/name pairs completely, and
/// the blowup tests run at 100,000 bytes. Splitting it this way follows the rule `ntp_proto` wrote
/// down (notes/ntp.md): the solver takes the domains too big to enumerate, and enumeration takes the
/// ones that are not.
#[cfg(kani)]
mod verification {
    use super::*;

    /// **Three, measured rather than chosen.** Four bytes each was tried first and abandoned twice:
    /// the solver passed 3 GB and ten minutes without finishing, which is the `calendar` finding
    /// again (a symbolic-length slice costs far more than the logic wrapped around it, notes/
    /// verification.md). Three bytes still reaches a closed class (`[a]`), an escape (`\*a`), a
    /// trailing backslash, an unterminated bracket and two stars, so every arm of the decoder is
    /// quantified over. The forms that need four and five bytes, `[!a]` and `[a-b]`, are quantified
    /// over by `a_negated_class_is_the_exact_complement_of_the_class`, which fixes the shape and
    /// leaves the members symbolic; that harness would also catch a panic in them.
    const P: usize = 3;
    const N: usize = 3;

    /// Two symbolic lengths, within the bound above. Symbolic rather than fixed because the empty
    /// pattern, the empty name and a pattern that runs out mid-class are all distinct code paths,
    /// and they are the ones a caller reaches by typing something odd.
    ///
    /// It is also the single most expensive thing in these harnesses, which `calendar` had already
    /// recorded: a symbolic-length slice costs far more than the logic wrapped around it. The three
    /// bytes above are what that costs.
    fn any_lengths() -> (usize, usize) {
        let plen: usize = kani::any();
        let nlen: usize = kani::any();
        kani::assume(plen <= P);
        kani::assume(nlen <= N);
        (plen, nlen)
    }

    /// **Matching is total.**
    ///
    /// For every pattern of up to three arbitrary bytes and every name of up to three arbitrary
    /// bytes, under both [`Dot`] settings: no panic, no arithmetic overflow, no out-of-bounds index,
    /// and the loop terminates within the unwinding bound. Termination is the property that matters for
    /// an untrusted pattern, and it is what the unwinding assertion establishes: at these lengths
    /// the matcher provably runs out of work, rather than being observed to.
    #[kani::proof]
    #[kani::unwind(11)]
    fn matching_is_total() {
        let pattern: [u8; P] = kani::any();
        let name: [u8; N] = kani::any();
        let (plen, nlen) = any_lengths();
        let dot = if kani::any() {
            Dot::Special
        } else {
            Dot::Ordinary
        };
        let _ = match_steps(&pattern[..plen], &name[..nlen], dot);
    }

    /// **The cost of a match never exceeds the bound computed from the two lengths alone**, over
    /// every pattern and name of up to three arbitrary bytes.
    ///
    /// This is the anti-blowup claim, and it is a separate harness from totality above for a reason
    /// that cost two dead runs to find. [`cost_bound`] is three saturating 64-bit multiplies, and a
    /// bit-blasting model checker is bad at multiplication (`ntp_proto` measured about four times the
    /// work per bit of operand, notes/ntp.md). Composed with a match loop that was already being
    /// unrolled twenty times over two symbolic lengths, the harness passed 3 GB and was killed.
    /// Splitting it moved the multiply next to a smaller unrolling instead of on top of the largest
    /// one, which is the same restructure-rather-than-weaken move DECISIONS §46 rule 1 describes.
    ///
    /// **What this harness does and does not establish, because the difference is easy to overclaim
    /// and was overclaimed here first.** `cost_bound(3, 3)` is 285, and no matcher at all is slow
    /// enough to exceed that on three bytes, so this is **not** where exponential backtracking would
    /// be caught. What it establishes is that the accounting is sound: `cost_bound` really is an
    /// over-approximation of `match_steps` for every short input, including the corners where a step
    /// costs more than one byte (an unterminated `[`, an escape inside a class, a backtrack). An
    /// off-by-one in the charging shows up here, and an off-by-one in the charging is exactly what
    /// would make the 100,000-byte host test pass while measuring the wrong thing.
    ///
    /// The blowup itself is ruled out by two other things: the host test that runs a ten-star pattern
    /// against 100,000 bytes, and the structural argument in the module comment (one backtrack point,
    /// a retry position that only advances). Neither is a solver's job.
    #[kani::proof]
    #[kani::unwind(11)]
    fn the_cost_never_exceeds_the_bound() {
        const SHORT: usize = 3;
        let pattern: [u8; SHORT] = kani::any();
        let name: [u8; SHORT] = kani::any();
        let plen: usize = kani::any();
        let nlen: usize = kani::any();
        kani::assume(plen <= SHORT);
        kani::assume(nlen <= SHORT);

        let (_, steps) = match_steps(&pattern[..plen], &name[..nlen], Dot::Ordinary);
        assert!(steps <= cost_bound(plen, nlen));
    }

    /// **A pattern with no magic in it matches exactly one name: itself, unescaped.**
    ///
    /// The theorem the shell's grant planner rests on. If [`has_magic`] says no, the word names one
    /// file, so the shell can skip the enumeration and ask for a single-name caretaker; if that
    /// were wrong in either direction the grant would be wrong, either missing a file or naming one
    /// the user did not write.
    ///
    /// Quantified over every pattern of up to three arbitrary bytes, which includes the escape
    /// corners (`\*`, `\\`, a trailing `\`) and the unterminated brackets that are literals rather
    /// than classes.
    #[kani::proof]
    #[kani::unwind(11)]
    fn no_magic_means_the_pattern_is_its_own_only_match() {
        let pattern: [u8; P] = kani::any();
        let name: [u8; N] = kani::any();
        let (plen, nlen) = any_lengths();
        let p = &pattern[..plen];
        let n = &name[..nlen];

        // An assumption rather than an early `return`, which says the restriction rather than
        // implementing it. Measured expecting a solver win, and there was none: 181s with the
        // `return`, 186s with the `assume`, which is noise. Recorded because "state the restriction
        // as an assumption and the solver prunes earlier" is a plausible-sounding rule that did not
        // hold here, and a plausible-sounding rule nobody measured is how the 60-unwind guess
        // happened too.
        kani::assume(!has_magic(p));
        let mut buf = [0u8; P];
        let lit = literal(p, &mut buf).expect("a magic-free pattern always has a literal");
        assert_eq!(matches_with(p, n, Dot::Ordinary), n == lit);
    }

    /// **`*` matches every name, and `?` matches exactly the one-byte names.**
    ///
    /// The two base cases, over every name of up to three arbitrary bytes. Stated with
    /// [`Dot::Ordinary`] so they are claims about the matcher; the leading-dot policy is the next
    /// harness, separately, because it is a separate thing.
    #[kani::proof]
    #[kani::unwind(12)]
    fn star_matches_anything_and_question_matches_one_byte() {
        let name: [u8; N] = kani::any();
        let nlen: usize = kani::any();
        kani::assume(nlen <= N);
        let n = &name[..nlen];

        assert!(matches_with(b"*", n, Dot::Ordinary));
        assert!(matches_with(b"**", n, Dot::Ordinary));
        assert_eq!(matches_with(b"?", n, Dot::Ordinary), nlen == 1);
        assert_eq!(matches_with(b"??", n, Dot::Ordinary), nlen == 2);
        // `*` around anything still matches everything that thing matches.
        assert_eq!(matches_with(b"*?*", n, Dot::Ordinary), nlen >= 1);
    }

    /// **The leading-dot rule is exactly "a wildcard cannot reach a dotfile".**
    ///
    /// Under [`Dot::Special`], a name beginning with `.` matches a pattern only if the pattern begins
    /// with a literal `.`, and a name **not** beginning with `.` is matched identically under both
    /// settings. The second half is the one worth proving: it says the policy has no effect anywhere
    /// else, which is otherwise something a reader takes on trust.
    ///
    /// **The name is bounded at two bytes here rather than three, and the pattern still at three.**
    /// This is the only harness that runs the match loop twice, so its cost is double everything
    /// else's, and at three bytes it was 279 seconds, more than the rest of the crate put together.
    /// Two is enough to state the rule completely, because the rule is a predicate on the name's
    /// *first* byte: empty, `.`, `.x`, `x` and `xy` are every case there is, and a third byte adds
    /// none. The pattern keeps three so `.*`, `\.x` and `[.]` are all reachable, which is where the
    /// rule's interesting inputs actually live.
    #[kani::proof]
    #[kani::unwind(9)]
    fn the_dot_rule_only_touches_names_that_start_with_a_dot() {
        const DOT_N: usize = 2;
        let pattern: [u8; P] = kani::any();
        let name: [u8; DOT_N] = kani::any();
        let plen: usize = kani::any();
        let nlen: usize = kani::any();
        kani::assume(plen <= P);
        kani::assume(nlen <= DOT_N);
        let p = &pattern[..plen];
        let n = &name[..nlen];

        let special = matches_with(p, n, Dot::Special);
        let ordinary = matches_with(p, n, Dot::Ordinary);

        if n.first() == Some(&b'.') {
            // Special is never more permissive, and is permissive only for a pattern that says `.`.
            assert!(!special || ordinary);
            assert!(!special || opens_with_a_literal_dot(p));
        } else {
            assert_eq!(special, ordinary);
        }
    }

    /// **`[xy]` and `[!xy]` partition the byte space**, over every two-byte class body and every
    /// byte, including the range spelling `[a-b]` that a symbolic body reaches on its own.
    ///
    /// The complement is the half worth proving. An implementation that got the `]`-first or
    /// `-`-last rule wrong in only one of the two spellings shows up here as an overlap or a gap,
    /// and neither is visible from either spelling alone.
    ///
    /// This is also the harness that quantifies over the four- and five-byte class forms the
    /// three-byte domain above cannot reach, so a panic in `[!...]` or in a range would fail here.
    ///
    /// **Three assumptions, and Kani found the need for the third.** Writing this without excluding
    /// `!` and `^` at the head of the body failed in 42 seconds with a counterexample, and the
    /// counterexample is correct: `[!y]` is not "the class of `!` and `y`", it is *already* a negated
    /// class, so `[!!y]` is its complement rather than its double. The other two exclude bodies that
    /// close early (`]`) or escape (`\`), which make the two spellings different patterns rather than
    /// one pattern and its negation. All three are about what the two spellings *are*; bracket
    /// parsing itself is quantified over without restriction by the totality harness above.
    #[kani::proof]
    #[kani::unwind(10)]
    fn a_negated_class_is_the_exact_complement_of_the_class() {
        let body: [u8; 2] = kani::any();
        let c: u8 = kani::any();

        kani::assume(body[0] != b']' && body[1] != b']');
        kani::assume(body[0] != b'\\' && body[1] != b'\\');
        kani::assume(body[0] != b'!' && body[0] != b'^');

        let plain = [b'[', body[0], body[1], b']'];
        let negated = [b'[', b'!', body[0], body[1], b']'];
        let name = [c];

        assert_eq!(
            matches_with(&plain, &name, Dot::Ordinary),
            !matches_with(&negated, &name, Dot::Ordinary)
        );
        // Not vacuous: the range spelling is inside the quantifier, so this is a claim about `[a-b]`
        // as well as about `[ab]`.
        kani::cover!(body[1] == b'-');
        kani::cover!(matches_with(&plain, &name, Dot::Ordinary));
    }
}
