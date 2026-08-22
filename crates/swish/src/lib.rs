//! **The capability shell's logic, with the IO taken out** (milestone 70).
//!
//! `user/src/swish.rs` is the program: it holds a terminal endpoint, a spawn channel, a result
//! channel, a budget, and (in the wiring that has one) a directory capability. This crate is
//! everything that shell decides or renders with **no capability in hand**, so it compiles for the
//! host and its tests run in milliseconds instead of under QEMU.
//!
//! The split follows the line CLAUDE.md draws for `coremark`, `line_editor` and `compositor`: a
//! crate and a program share a name when the crate is that program's logic. Reading it the other
//! way is the useful direction. If a function here needed a capability, it would not be here.
//!
//! # What is in here
//!
//! - **The routing of a typed line.** [`route`] answers "what kind of thing did the user type",
//!   including the one ordering that has a bug behind it: the prefix words (`caps`, `time`) are
//!   answered *before* the line is split on its operators, or `caps date | wc` would lose everything
//!   after the pipe.
//! - **What a duration reads as.** [`write_duration`] and [`write_timing`] (milestone 86), plus
//!   [`write_untimed`] for the three reasons there is no duration to print. The arithmetic is here
//!   rather than in the program because it is arithmetic, and the program's half is two reads of a
//!   page it holds a capability to.
//! - **Pattern versus text.** [`is_pattern`] and [`expansion`], which decide whether a word is a
//!   designation to resolve or arbitrary text to print, and which word on a line gets expanded.
//! - **Every sentence the user reads.** [`write_say`], [`write_refusal`], [`write_outcome`],
//!   [`write_preview`], [`write_holdings`], [`write_help`], [`write_pwd`], [`write_num`].
//!
//! # How a renderer is written here
//!
//! Every rendering function takes `out: &mut dyn FnMut(&[u8])` rather than printing. The program
//! passes its terminal `print`; a test passes a collector; the guest witnesses (which have no
//! terminal at all) pass a buffer. That was already the shape `ls` and `echo` used inside the shell,
//! so nothing about the program's IO had to be restructured to lift the rest.
//!
//! Anything that needs a directory listing takes a second callback, `expand`, of the same shape:
//! the *matching* is pure and lives in `grant_plan::expand`, and only reading the directory needs a
//! capability. So [`echo`] and [`expansion`] are testable end to end against a fake directory.
//!
//! ```
//! use grant_plan::expand::NameSet;
//! use swish::{Say, echo};
//!
//! // A directory holding two files, standing in for the one the shell would have to ask a server
//! // for. Everything else on this path is the real thing.
//! let mut listing = |_pattern: &[u8]| -> Result<NameSet, Say> {
//!     let mut set = NameSet::empty();
//!     set.push(b"notes.txt", false);
//!     set.push(b"report.txt", false);
//!     Ok(set)
//! };
//!
//! let mut printed = Vec::new();
//! let said = echo(
//!     b"the set: *.txt",
//!     swish::Status::Ran,
//!     &mut listing,
//!     &mut |b| printed.extend_from_slice(b),
//! );
//!
//! assert!(matches!(said, Say::Nothing));
//! // The words with no magic in them came through byte for byte; the pattern became what it
//! // designates, which is exactly what `rm *.txt` would have been granted.
//! assert_eq!(printed, b"the set: notes.txt report.txt");
//! ```
//!
//! # BUGS
//!
//! Three of the shell's functions are **not** here and could not be lifted without restructuring
//! its IO, which milestone 70 was scoped not to do:
//!
//! - `builtin` and `dispatch_one`, because every arm is a capability invocation (`cd`, `ls` and
//!   `mkdir` are requests to the filesystem server) or a print. [`route`] lifts the decision they
//!   sit under; the arms themselves stay with the wiring.
//! - `run`, whose body is two calls into `grant_plan` (both already host-tested there) wrapped
//!   around a choice between two spawn paths. Lifting it would have moved the spawn decision away
//!   from the code that can act on it and bought no new coverage.
//! - `spawn`, `pipeline` and everything below them, which is capability movement and nothing else.
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `shell`. Refused `shell` (a category
//! rather than a name: `bash`, `zsh`, `fish` and `rc` are identities), `capsh` (Linux's libcap
//! ships `capsh(1)`) and `sheesh` (it carries a 2020-21 timestamp where `bash` and `fish` are
//! era-neutral, and it is an interjection of exasperation, while this shell's most characteristic
//! behaviour is refusing things by design). A swish is the shot that goes through the net touching
//! nothing, which is least authority in one word. The crate takes the program's name because the
//! crate is that program's logic (DECISIONS §63); it was lifted out on 2026-08-02 by milestone 70.

#![no_std]

pub mod sequence;

use fs_proto::dir;
use grant_plan::expand::{Expansion, NameSet};
use grant_plan::line::{self, Line};
use grant_plan::nav::{self, Cwd, Refused};
use grant_plan::{
    ArgSpec, Command, Endowment, Holdings, Prog, Refusal, RunSpec, Streams, spawnproto,
};

/// What a builtin has to say. A value rather than a print, because the printing half belongs to the
/// interactive prompt and the navigating witness (which has no terminal) runs the same builtins.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Say {
    /// It worked and there is nothing to add.
    Nothing,
    /// The name could not be navigated, and nothing was sent.
    Refused(Refused),
    /// The filesystem refused, with this errno. Rendered by `fs_proto::dir::explain`, which keeps
    /// the sentence next to the decision that chose the number.
    Failed(i32),
    /// This shell holds no directory capability, so there is nothing to name.
    NoDirectory,
    /// The verb needs an operand and got none.
    NeedsAName,
    /// **A designation this shell cannot back**, in `grant_plan`'s vocabulary. It arrives here
    /// because expansion happens *before* planning, so a pattern that matched nothing (or too much)
    /// is refused before there is a program to attribute it to, and [`echo`] can reach the same
    /// refusals with no program on the line at all.
    Cannot(Refusal),
}

/// **What a command did**, which is what `$?` reports and what `&&` reads (milestone 67,
/// notes/swish-language.md).
///
/// # The fork this milestone had to settle: a refusal is not an error
///
/// On Unix `127` (no such command) and a program's own `exit(1)` are the same kind of integer, and
/// `&&` cannot tell them apart because the shell has nothing better to say. Here the two are
/// genuinely different events and the shell knows which:
///
/// - [`Refused`](Status::Refused) is **the shell declining to run the line**, decided at the prompt
///   from what this shell *holds* and what a manifest says, with nothing spawned, nothing opened
///   and no authority moved. It is reproducible: the same line refuses again. "You hold no such
///   capability", `Refusal::TooManyNames`, a pattern that matched nothing, a name that is not a
///   name.
/// - [`Failed`](Status::Failed) is **something was attempted and did not work**: the filesystem
///   answered with an errno, init had no memory to spawn with, a job was interrupted.
///
/// Separating them is the answer to "what does a status mean when the thing that failed was a
/// refusal", and it is worth a number because the two answer different questions. "Did my command
/// fail?" and "was I able to ask?" are not the same question, and a shell that refuses constantly
/// and by design should be able to say which one happened.
///
/// # What it is *not*, stated because the gap is real
///
/// **No program in this system reports an exit status**, and this value does not pretend one did. A
/// spawned program answers with a *value* (`worker 7` answers 49), with bytes, or through a job
/// frame, and none of those is a status. So `$?` is the shell's own reading of what happened to the
/// line, which today is all there is; inventing a per-program status would mean a `spawnproto`
/// change and an edit to every program, which is a milestone and not a field.
///
/// # And what it cannot carry
///
/// One small integer, which designates nothing. There is no capability in it, no name, no handle:
/// a `&&` chain hands the next segment a **bit**, and that segment is planned from scratch against
/// what the shell holds, exactly as if it had been typed alone. See [`sequence`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Status {
    /// The command ran and the shell has nothing to report. `$?` is `0`.
    #[default]
    Ran,
    /// Something was attempted and did not work. `$?` is `1`.
    Failed,
    /// The shell would not run it, so nothing did. `$?` is `2`.
    Refused,
}

impl Status {
    /// Whether `&&` should carry on. Only [`Ran`](Status::Ran) is a yes.
    pub fn ok(self) -> bool {
        matches!(self, Status::Ran)
    }

    /// The number `$?` shows.
    pub fn code(self) -> u64 {
        match self {
            Status::Ran => 0,
            Status::Failed => 1,
            Status::Refused => 2,
        }
    }

    /// Read a status back out of [`code`](Status::code). The shell keeps it in an atomic cell,
    /// because every printer in the program can reach it and none of them holds a `&mut` to
    /// anything shared.
    pub fn from_code(code: u64) -> Status {
        match code {
            0 => Status::Ran,
            1 => Status::Failed,
            _ => Status::Refused,
        }
    }

    /// The number as bytes, which is `'static` because there are three of them.
    ///
    /// That is not a micro-optimisation, it is what makes `$?` expressible at all in a shell with
    /// no allocator: a substituted word has to be a slice with the line's lifetime, and a `'static`
    /// slice unifies with any of them. A status with an unbounded range would need a buffer, and
    /// there would be nowhere to put one.
    pub fn digits(self) -> &'static [u8] {
        match self {
            Status::Ran => b"0",
            Status::Failed => b"1",
            Status::Refused => b"2",
        }
    }
}

/// **The word that reads the last command's status.** Recognised where words are expanded, which
/// today is [`echo`].
///
/// It is spelled `$?` because that is the spelling every shell user already knows, and this project
/// does not respell a name a reader arrives with. It is **not** a variable: there is no variable
/// mechanism here at all (milestone 47 owns that, and studies it as "the same question wearing a
/// string costume"), so this is one word the expander knows, in the same category as a pattern.
pub const STATUS_WORD: &[u8] = b"$?";

/// **What kind of thing the user typed**, decided before anything is invoked.
///
/// The variants are not three grammars. They are the three shapes the shell has to act on
/// differently, and the order they are decided in is load-bearing (see [`route`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route<'a> {
    /// `caps`, with everything after it. The tail is a **whole command line**, operators included.
    Caps(&'a [u8]),
    /// `time`, with everything after it (milestone 86). The tail is a whole command line for
    /// [`Caps`](Route::Caps)'s reason, and the shell **re-dispatches** it: what gets timed is the
    /// line taking the path it would have taken untimed, so "what you time is what you run" is a
    /// property of the code rather than a claim about it.
    Time(&'a [u8]),
    /// `xargs`, with everything after it (milestone 109). The tail is a whole command line for
    /// [`Caps`](Route::Caps)'s reason, and the shell re-dispatches it **once per batch** of what its
    /// pattern matched. The name is provisional.
    Xargs(&'a [u8]),
    /// One command, with no operators on it: the path every line took before milestone 50.
    One(&'a [u8]),
    /// A line with `>`, `<` or `|` on it, already split into stages.
    Pipeline(Line<'a>),
    /// The line cannot be split at all, so there is nothing to run.
    Cannot(Refusal),
}

/// **Read one line and decide what it is.**
///
/// The two prefix words are asked about **before** the line is split, and that ordering is the whole
/// reason this is a function worth having. Their operand *is* a command line, so they have to see
/// the operators: `caps date | wc` must print two stages, and splitting first would hand it the word
/// `date` and silently lose the rest. `time date | wc` is the same shape with the same bug behind
/// it, one milestone later, and it costs one arm rather than a second mechanism.
///
/// A line with no operators takes exactly the path it took before they existed, which is deliberate.
/// The operators must not make an ordinary command more expensive or more complicated, because an
/// ordinary command is nearly every command.
///
/// ```
/// use swish::{Route, route};
///
/// // The tail keeps the pipe. `caps` will preview two stages, not one.
/// assert!(matches!(route(b"caps date | wc"), Route::Caps(b"date | wc")));
/// // And `time` keeps it for the same reason: it times the pipeline, not its first word.
/// assert!(matches!(route(b"time date | wc"), Route::Time(b"date | wc")));
/// // The third prefix word (milestone 109), which batches a whole line for the same reason.
/// assert!(matches!(route(b"xargs rm *.txt"), Route::Xargs(b"rm *.txt")));
/// assert!(matches!(route(b"worker 7"), Route::One(b"worker 7")));
/// match route(b"date | wc") {
///     Route::Pipeline(l) => assert_eq!(l.stages().len(), 2),
///     other => panic!("expected a pipeline, got {other:?}"),
/// }
/// ```
pub fn route(cmd: &[u8]) -> Route<'_> {
    match grant_plan::parse(cmd) {
        Command::Caps(tail) => return Route::Caps(tail),
        Command::Time(tail) => return Route::Time(tail),
        Command::Xargs(tail) => return Route::Xargs(tail),
        _ => {}
    }
    match line::split(cmd) {
        Ok(l) if l.is_plain() => Route::One(l.stages()[0]),
        Ok(l) => Route::Pipeline(l),
        Err(r) => Route::Cannot(r),
    }
}

/// **Whether this token is a designation to resolve rather than a word to print.**
///
/// `glob::has_magic` first, deliberately: an [`echo`] word is arbitrary text (`a:b` is not a path
/// and must not be refused as one), so nothing asks whether a token is a *nameable* path until it is
/// established that it is a pattern at all. Once it is, a pattern anywhere but the last component is
/// refused, because selecting directories to walk is an authority question (notes/glob.md).
pub fn is_pattern(token: &[u8]) -> Result<bool, Refusal> {
    if !glob::has_magic(token) {
        return Ok(false);
    }
    grant_plan::expand::magic_component(token)
}

/// **Expand the invocation's pattern operand, before anything is planned.**
///
/// The shell expands and then plans, which is what Unix does, so there is no divergence to earn.
/// What is different is the consequence: the planner must see the **set** rather than the pattern,
/// because the endowment is the set. Only the first pattern is expanded, and that is not a
/// limitation being hidden: no manifest declares two name slots, so a second operand of any kind is
/// already an unplaceable token and a refusal.
///
/// `expand` is the shell's directory read, which is the only part of this that needs a capability.
pub fn expansion(
    spec: &RunSpec,
    expand: &mut dyn FnMut(&[u8]) -> Result<NameSet, Say>,
) -> Result<Expansion, Say> {
    for (i, token) in spec.positionals().iter().enumerate() {
        // **A quoted word designates itself** (milestone 67). This is the whole of what quoting
        // does to authority, and it is a narrowing: `rm "*.txt"` hands over one name where
        // `rm *.txt` hands over the set. Asked before `is_pattern`, because the question "is this a
        // pattern" is only worth asking about a word nobody quoted.
        if spec.quoted(i) {
            continue;
        }
        match is_pattern(token) {
            Ok(false) => continue,
            Ok(true) => return Ok(Expansion::at(i, expand(token)?)),
            Err(r) => return Err(Say::Cannot(r)),
        }
    }
    Ok(Expansion::none())
}

/// Write a set as [`echo`] shows it and as [`write_preview`] previews it: the names, one space
/// apart, in the order the directory yielded them. One renderer, so what is displayed in the two
/// places cannot differ in a way that hides a difference in the authority.
pub fn write_set(set: &NameSet, out: &mut dyn FnMut(&[u8])) {
    for (i, (name, _)) in set.iter().enumerate() {
        if i > 0 {
            out(b" ");
        }
        out(name);
    }
}

/// **`echo`, which expands.** The half of milestone 47's globbing demonstration that costs no
/// authority: `echo *.txt` prints literally what `rm *.txt` would transfer.
///
/// Words with no magic in them are copied through **byte for byte, spacing included**, so `echo
/// two  spaces` still prints its two spaces. Only a word that is a pattern is replaced by what it
/// designates, which keeps `echo` a text command everywhere it was one before.
///
/// # The two words that are not text (milestone 67)
///
/// A **quoted** word prints what is between its quotes and is never expanded, so `echo "*.txt"`
/// prints a pattern and `echo *.txt` prints the set it designates. Those two lines side by side are
/// the demonstration this milestone owes: quoting is the difference between naming a name and
/// naming a set, and `echo` shows it before anything moves.
///
/// And [`STATUS_WORD`] prints `status`, which is how `$?` is read at a prompt. `echo "$?"` prints
/// the two characters, because both quote forms are literal here (see [`grant_plan::word`]).
pub fn echo(
    text: &[u8],
    status: Status,
    expand: &mut dyn FnMut(&[u8]) -> Result<NameSet, Say>,
    out: &mut dyn FnMut(&[u8]),
) -> Say {
    let mut i = 0;
    while i < text.len() {
        let space = i;
        while i < text.len() && text[i].is_ascii_whitespace() {
            i += 1;
        }
        if i > space {
            out(&text[space..i]);
        }
        let word = i;
        // A word ends at the first **bare** whitespace, so `echo "two  spaces"` is one word and
        // keeps the spacing inside it.
        i = grant_plan::word::span(text, i, &|b| b.is_ascii_whitespace());
        if i == word {
            continue;
        }
        let token = match grant_plan::word::read(&text[word..i]) {
            Ok(w) => w,
            Err(r) => return Say::Cannot(r),
        };
        if token.quoted {
            out(token.text);
            continue;
        }
        if token.text == STATUS_WORD {
            out(status.digits());
            continue;
        }
        match is_pattern(token.text) {
            Ok(false) => out(token.text),
            Ok(true) => match expand(token.text) {
                Ok(set) => write_set(&set, out),
                // A pattern that matched nothing stops the line rather than printing itself. That is
                // the same answer `rm` gets, and it has to be: if `echo` printed the pattern where
                // `rm` refuses, the two would disagree about what the line designates, which is the
                // one thing this pairing exists to rule out.
                Err(s) => return s,
            },
            Err(r) => return Say::Cannot(r),
        }
    }
    Say::Nothing
}

// ---- batching at the bound (milestone 109) ----

/// **The account of a batched line**: how many batches ran, how many names they designated, and
/// whether anything stopped the sweep.
///
/// It exists because a batched line is **one command with a partial effect**, which is a thing no
/// unbatched line here can be, and the only honest way to report one is to say where the boundary
/// fell. A sweep that stopped is a *prefix* of the match, which a person can hold in their head and
/// resume from; a sweep that carried on past a failure would be an arbitrary subset, and the user
/// could not tell which names it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Sweep {
    /// Batches that ran.
    pub batches: u64,
    /// Names those batches designated, which is the authority that actually moved.
    pub names: u64,
    /// The batch that stopped the sweep, counting from one, or `None` for a sweep that finished.
    pub stopped: Option<u64>,
}

impl Sweep {
    /// Record a batch that ran, carrying `names` names.
    pub fn ran(&mut self, names: u64) {
        self.batches += 1;
        self.names += names;
    }

    /// Record that the batch **after** the ones that ran did not, and stop.
    pub fn stop(&mut self) {
        self.stopped = Some(self.batches + 1);
    }
}

/// Write the sweep's account.
///
/// **The failing case says what was not attempted, and that is the whole point of the sentence.**
/// Unix's `xargs` carries on after a failed invocation and reports 123 at the end, which is its
/// mechanism talking: it cannot know what a child did to the names it was handed, so it may as well
/// try the rest. Here the shell printed each batch's set before that batch ran, so it can name the
/// boundary instead, and the failure worth designing against is batch four succeeding after batch
/// three failed.
///
/// ```
/// use swish::{Sweep, write_sweep};
///
/// let render = |s: &Sweep| {
///     let mut v = Vec::new();
///     write_sweep(s, &mut |b| v.extend_from_slice(b));
///     String::from_utf8(v).unwrap()
/// };
///
/// let mut done = Sweep::default();
/// done.ran(8);
/// done.ran(3);
/// assert_eq!(render(&done), "  11 names, in 2 batches\n");
///
/// // One batch is not a sweep worth reporting: the line behaved like any other line.
/// let mut one = Sweep::default();
/// one.ran(3);
/// assert_eq!(render(&one), "");
///
/// // And a stop names the boundary in both directions.
/// let mut stopped = Sweep::default();
/// stopped.ran(8);
/// stopped.stop();
/// assert_eq!(
///     render(&stopped),
///     "  batch 2 did not run: 8 names were handed over in 1 batch, and nothing after \
///      them was attempted\n",
/// );
///
/// // A sweep whose FIRST batch did not run says nothing: the refusal printed above it is the
/// // whole story, and "0 names were handed over in 0 batches" is a sentence about an event that
/// // did not happen.
/// let mut none = Sweep::default();
/// none.stop();
/// assert_eq!(render(&none), "");
/// ```
pub fn write_sweep(s: &Sweep, out: &mut dyn FnMut(&[u8])) {
    let batches = |n: u64, out: &mut dyn FnMut(&[u8])| {
        write_num(n, out);
        out(if n == 1 { b" batch" } else { b" batches" });
    };
    if s.batches == 0 {
        return;
    }
    match s.stopped {
        Some(at) => {
            out(b"  batch ");
            write_num(at, out);
            out(b" did not run: ");
            write_num(s.names, out);
            out(b" names were handed over in ");
            batches(s.batches, out);
            out(b", and nothing after them was attempted\n");
        }
        // **One batch prints nothing**, deliberately: `xargs` over a match that fits is the line the
        // user would have typed without it, and a footer under it would be noise claiming an event.
        None if s.batches == 1 => {}
        None => {
            out(b"  ");
            write_num(s.names, out);
            out(b" names, in ");
            batches(s.batches, out);
            out(b"\n");
        }
    }
}

/// Write one batch's set, before that batch runs.
///
/// **This is the per-batch form of the property the whole globbing lane exists for**: the expansion
/// you see is the grant. Under batching the set the user is shown has to be the set *this
/// invocation* is handed, not the union of the sweep, because the union is precisely the thing
/// nobody can hold. So it is printed once per batch, from the same [`write_set`] `echo` and the
/// grant preview use.
pub fn write_batch(index: u64, set: &NameSet, out: &mut dyn FnMut(&[u8])) {
    out(b"  batch ");
    write_num(index, out);
    out(b": ");
    write_set(set, out);
    out(b"\n");
}

/// Write a small unsigned number in base 10.
pub fn write_num(mut v: u64, out: &mut dyn FnMut(&[u8])) {
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    out(&digits[i..]);
}

// ---- `apropos`: what a search of the documentation store says (milestone 40 phase 2) ----

/// How wide the count column is, so the numbers line up on their right edge. Four digits, because
/// the strongest term in the shipped store occurs 32 times and a page mentioning one nine thousand
/// times is a page with a different problem.
const APROPOS_COUNT: usize = 4;

/// **The column a title starts in**, counted from the start of the line.
///
/// Two spaces of indent, the count, two spaces, then the widest location the shipped store
/// produces (`doc/swish/line-discipline.md`, twenty-eight bytes) and two more. A longer location
/// pushes its title right rather than being truncated: losing the name a reader is meant to type
/// would defeat the whole line. See `BUGS` in notes/manual.md for what that costs at eighty
/// columns.
const APROPOS_TITLE: usize = 2 + APROPOS_COUNT + 2 + 28 + 2;

/// **Render one search result**: how strongly it matched, the name a reader can type, and the
/// page's title.
///
/// The columns are the answer's argument. A person reads this to decide what to open, so the
/// **typeable name** is what has to be unmissable, and the count is what orders the list. The
/// origin ([`manual::index::Found::origin`]) is deliberately not printed: it is provenance, it is
/// nearly the location again, and a second path on the line would compete with the one the reader
/// is meant to type.
pub fn write_found(f: &manual::index::Found, out: &mut dyn FnMut(&[u8])) {
    let mut digits = 1;
    let mut v = f.count as u64 / 10;
    while v > 0 {
        digits += 1;
        v /= 10;
    }
    out(b"  ");
    pad(APROPOS_COUNT.saturating_sub(digits), out);
    write_num(f.count as u64, out);
    out(b"  ");
    out(f.location());
    let n = 2 + APROPOS_COUNT.max(digits) + 2 + f.location().len();
    // A location that ran past its column still gets one space, because a title jammed against a
    // path reads as one word.
    pad(APROPOS_TITLE.saturating_sub(n).max(1), out);
    out(f.title());
    out(b"\n");
}

/// **Render a whole search answer**, including the tail that keeps it honest.
///
/// Three things a reader needs and one of them is a refusal to overclaim: the results, the fact
/// that a store said nothing at all, and the fact that more pages matched than the table can hold.
/// [`manual::index::Ranked`] counts what it dropped precisely so this can say so; printing the
/// results alone would imply the answer was complete.
pub fn write_apropos(term: &[u8], r: &manual::index::Ranked, out: &mut dyn FnMut(&[u8])) {
    if r.offered() == 0 {
        out(b"  no page in the store says ");
        out(term);
        out(b"\n");
        return;
    }
    for f in r.results() {
        write_found(f, out);
    }
    if r.offered() > r.results().len() {
        out(b"  ");
        write_num(r.results().len() as u64, out);
        out(b" of ");
        write_num(r.offered() as u64, out);
        out(b" pages, strongest first\n");
    }
}

/// Write `n` spaces.
fn pad(n: usize, out: &mut dyn FnMut(&[u8])) {
    const SPACES: &[u8; 32] = &[b' '; 32];
    let mut left = n;
    while left > 0 {
        let take = left.min(SPACES.len());
        out(&SPACES[..take]);
        left -= take;
    }
}

// ---- `time`: what the shell may say about how long something took (milestone 86) ----

/// **The three reasons a line was not timed**, printed instead of a duration.
///
/// Two of them are `date`'s, worded for the prefix position, because they are the same two facts
/// about the same page: this process holds no clock capability, or the machine has published no time
/// it believes. The third is the one the prefix position adds, and it is a fact about the line.
///
/// **A `time` that cannot measure does not run the command**, which is the decision worth naming
/// here rather than in a note. `time wc report.txt` is a request to measure something; running it
/// unmeasured and saying nothing would be DECISIONS §42's silent degradation with a stopwatch on it,
/// and running it while printing a complaint would leave a person guessing which half happened. So
/// the refusal is at the prompt, before anything is spawned, the way every other refusal in this
/// shell is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Untimed {
    /// `time` with nothing after it. There is no command to run, so there is nothing to time.
    ///
    /// The only variant, and it is a usage error rather than a capability one. `NoClock` and
    /// `UnknownClock` lived here until §72: they refused to measure on a machine whose wall clock
    /// was absent or unbelieved, which turned out to gate nothing, because a duration is two reads
    /// of an ambient counter. A `time` that cannot be refused has no other refusals to name.
    NothingToTime,
}

/// Write why a line was not timed. The two clock sentences are `date`'s with the program name
/// changed, deliberately: they name the same two causes and call for the same two fixes, so a person
/// who has read one has read the other.
pub fn write_untimed(r: Untimed, out: &mut dyn FnMut(&[u8])) {
    out(match r {
        Untimed::NothingToTime => b"  time: name a command to time: time <command>\n".as_slice(),
    });
}

/// **An elapsed duration, in the largest unit that keeps it readable**, with three digits after the
/// point and nothing after that.
///
/// Three units and no more: seconds, milliseconds, microseconds. A spawn on this system is
/// milliseconds and a builtin is microseconds, so those are the two a person meets; seconds is there
/// because a long job exists and `1234567.891 ms` is not a number anybody reads.
///
/// **Three digits is a rendering choice, not a resolution claim.** The counter ticks at 62.5 MHz on
/// the aarch64 board (16 ns) and 10 MHz on the RISC-V one (100 ns), so the last digit of a
/// microsecond reading is real on one board and rounded on the other. What it must never do is print
/// more precision than the arithmetic has, which is why there is no nanosecond unit here at all.
///
/// ```
/// use swish::write_duration;
///
/// let shown = |nanos| {
///     let mut s = Vec::new();
///     write_duration(nanos, &mut |b| s.extend_from_slice(b));
///     String::from_utf8(s).unwrap()
/// };
///
/// assert_eq!(shown(0), "0.000 us");
/// assert_eq!(shown(1_234), "1.234 us");
/// assert_eq!(shown(4_213_000), "4.213 ms");
/// assert_eq!(shown(90_500_000_000), "90.500 s");
/// ```
pub fn write_duration(nanos: u64, out: &mut dyn FnMut(&[u8])) {
    const MICRO: u64 = 1_000;
    const MILLI: u64 = 1_000_000;
    const SEC: u64 = 1_000_000_000;
    let (whole, thousandths, unit) = if nanos >= SEC {
        (nanos / SEC, (nanos % SEC) / MILLI, b" s".as_slice())
    } else if nanos >= MILLI {
        (nanos / MILLI, (nanos % MILLI) / MICRO, b" ms".as_slice())
    } else {
        (nanos / MICRO, nanos % MICRO, b" us".as_slice())
    };
    write_num(whole, out);
    out(b".");
    // Written digit by digit rather than through [`write_num`], because the fraction is **zero
    // padded** and that function writes no leading zeros: `4.13 ms` and `4.013 ms` are different
    // numbers and only one of them is what happened. `thousandths` is below 1000 by construction
    // (every branch above divides its remainder by the next unit down), so three digits is exact.
    out(&[
        b'0' + (thousandths / 100 % 10) as u8,
        b'0' + (thousandths / 10 % 10) as u8,
        b'0' + (thousandths % 10) as u8,
    ]);
    out(unit);
}

/// **The line `time` prints when it measured something.**
///
/// `real` is Unix's word for it and it is the honest one: this is wall-clock time between the shell
/// deciding to run the line and the line being over. It is **not** CPU time, and there is no `user`
/// or `sys` row to go with it, because nothing in this kernel is asked how much processor a thread
/// consumed. That is a scheduler fact and it is unqueried today; if it arrives it is another row
/// here, not another command.
///
/// The number is a difference of two reads of the monotonic counter (§72), so there is nothing to
/// disclaim: the old signature took a `stepped` flag because a wall clock corrected mid-command
/// made the answer a difference of readings on two different clocks, and a stopwatch that is
/// sometimes wrong and never says so is worse than no stopwatch. A monotonic counter cannot be
/// stepped, so that whole failure mode, and the flag that reported it, are gone.
pub fn write_timing(nanos: u64, out: &mut dyn FnMut(&[u8])) {
    out(b"  time: real ");
    write_duration(nanos, out);
    out(b"\n");
}

/// Write what a builtin had to say. Every line is a statement about a name or a capability, never
/// about a policy: `fs_proto::dir::explain` keeps the filesystem's half next to the decision that
/// chose the errno, so this function does not get to invent a friendlier word for a refusal.
pub fn write_say(s: Say, out: &mut dyn FnMut(&[u8])) {
    match s {
        Say::Nothing => {}
        Say::Refused(r) => {
            out(b"  ");
            out(r.message().as_bytes());
            out(b"\n");
        }
        Say::Failed(errno) => {
            out(b"  ");
            out(dir::explain(errno).as_bytes());
            out(b"\n");
        }
        Say::NoDirectory => {
            out(b"  this shell holds no directory capability; there is nothing here to name\n");
        }
        Say::NeedsAName => out(b"  name what you mean: this verb takes one\n"),
        Say::Cannot(r) => {
            out(b"  ");
            out(r.message().as_bytes());
            out(b"\n");
        }
    }
}

/// Write where the shell is, relative to its own root. A shell holding no directory has no position
/// to print, and the caller says so with [`Say::NoDirectory`] instead of calling this.
pub fn write_pwd(cwd: &Cwd, out: &mut dyn FnMut(&[u8])) {
    let mut buf = [0u8; nav::RENDER_MAX];
    let n = cwd.render(&mut buf);
    out(b"  ");
    out(&buf[..n]);
    out(b"\n");
}

/// The shell's own help text.
pub fn write_help(out: &mut dyn FnMut(&[u8])) {
    out(b"  help                    this text\n");
    out(b"  echo <text>             print <text>\n");
    out(b"  caps                    print this shell's whole endowment\n");
    out(b"  caps <command>          preview what that command would grant\n");
    out(b"  time <command>          run it and say how long it took (WALL clock, not CPU)\n");
    out(b"  xargs <command>         run it once per BATCH when its pattern matches too many\n");
    out(b"  cd [path]               move inside the directory you hold ('cd' is your root)\n");
    out(b"  pwd                     where you are, relative to YOUR root\n");
    out(b"  ls [path]               list a directory you can reach\n");
    out(b"  mkdir <path>            make a directory\n");
    out(
        b"  apropos <word>          name the installed pages that mention it (it grants nothing)\n",
    );
    out(b"  rm [-rfv] <path>        a PROGRAM, granted the directory holding what you name\n");
    out(b"  worker <n>              spawn a process that returns n*n\n");
    out(b"  budgeter --mem N        grant a process N pages from this shell's budget\n");
    out(b"  date                    print the wall-clock time\n");
    out(b"  wc                      count lines, words and bytes on its INPUT\n");
    out(b"  doc <page>              render markdown from its INPUT (apropos names the pages)\n");
    out(b"  <prog> <name>           grant a process one file, and only that file\n");
    out(b"\n  operators (milestone 50). > and | are the same mechanism: a different\n");
    out(b"  capability in a program's output slot, which it cannot look behind.\n");
    out(b"  a | b                   a's output becomes b's input; the pipe is an endpoint\n");
    out(b"  a > name                a's output becomes a file it can append to and nothing else\n");
    out(b"  a >> name               the same, without emptying the file first\n");
    out(b"  a < name                a file's bytes become a's input\n");
    out(b"  a 2> name               a's DECLARED second stream, if it has one (date does)\n");
    out(b"  echo hello | wc         a builtin can lead a pipeline: this shell writes the bytes\n");
    out(b"\n  quoting (milestone 67). it decides what a WORD is, and a word is often the\n");
    out(b"  thing you are granting. it never widens what you may name.\n");
    out(b"  'text'  \"text\"          one word, spaces and operators included\n");
    out(b"  wc \"my notes.txt\"       ... which is the only way to name a file with a space\n");
    out(b"  echo \"*.txt\"            quoted, so it is ONE name and not the set it matches\n");
    out(b"\n  sequencing. a connector carries one bit and no capability: each command is\n");
    out(b"  planned from scratch against what this shell holds.\n");
    out(b"  a ; b                   run b whatever a did\n");
    out(b"  a && b                  run b only if a succeeded\n");
    out(b"  a || b                  run b only if a did not\n");
    out(b"  echo $?                 0 it ran, 1 it failed, 2 THIS SHELL REFUSED IT\n");
    out(b"\n  naming a resource grants it; a program that names nothing can touch nothing.\n");
}

/// Write a refusal in the capability model's voice. The fixed half is `grant_plan`'s (host-tested so
/// the wording cannot drift); the shell supplies the program name where one helps.
///
/// ```
/// use grant_plan::{Command, Refusal};
/// use swish::write_refusal;
///
/// let render = |line: &[u8], refusal| {
///     let Command::Run(spec) = grant_plan::parse(line) else { panic!("not an invocation") };
///     let mut s = Vec::new();
///     write_refusal(&spec, refusal, &mut |b| s.extend_from_slice(b));
///     String::from_utf8(s).unwrap()
/// };
///
/// // A name that resolved to nothing is worth repeating back. `cat` is a command a Unix hand
/// // would reach for and this shell does not have, so the line reads as a fact about the name.
/// assert!(render(b"cat 7", Refusal::NoSuchProgram).starts_with("  cat: "));
/// // A line of nothing but flags names no program, so the bare refusal is all there is to print.
/// // An empty name followed by a colon would be worse.
/// assert_eq!(
///     render(b"--mem 16", Refusal::NoSuchProgram),
///     "  no such program (try 'help' for the builtins)\n"
/// );
/// ```
pub fn write_refusal(spec: &RunSpec, refusal: Refusal, out: &mut dyn FnMut(&[u8])) {
    out(b"  ");
    // A named-but-unresolvable program, or an un-grantable resource, name the offending thing. A
    // line of nothing but flags (`--mem 16`) names no program at all, and printing an empty name
    // followed by a colon would be worse than printing the bare refusal.
    match refusal {
        Refusal::NoSuchProgram if !spec.prog.is_empty() => {
            out(spec.prog);
            out(b": ");
        }
        Refusal::NoSuchProgram => {}
        _ => {
            if let Some(p) = Prog::from_name(spec.prog) {
                out(p.name().as_bytes());
                out(b": ");
            }
        }
    }
    out(refusal.message().as_bytes());
    out(b"\n");
}

/// Report what the spawned program did, in terms of the grant it was given.
pub fn write_outcome(e: &Endowment, answer: u64, out: &mut dyn FnMut(&[u8])) {
    if answer == spawnproto::SPAWN_FAILED {
        out(b"  could not spawn (init is out of memory)\n");
        return;
    }
    match e.prog {
        Prog::Worker => {
            out(b"  a process at EL0 computed ");
            write_num(e.arg, out);
            out(b"*");
            write_num(e.arg, out);
            out(b" = ");
            write_num(answer, out);
            out(b"\n");
        }
        Prog::Budgeter => {
            out(b"  the process mapped ");
            write_num(answer, out);
            out(b" pages out of the ");
            write_num(e.mem_pages, out);
            out(b"-page budget you granted (the rest paid for its page tables)\n");
        }
        // Supervised jobs report through the job frame and the interruptible path, not here; `date`
        // answers in text and is drained by the byte-stream reader before this is reached. `rm` is
        // unreachable from the interactive prompt at all (a directory grant needs a caretaker that
        // shell cannot build, and it says so), and when it is reachable it will report the way
        // `date` does: diagnostics as text, then an exit status.
        Prog::Heeder
        | Prog::Spinner
        | Prog::Date
        | Prog::Rm
        | Prog::Wc
        | Prog::Doc
        | Prog::Ps
        | Prog::Pgrep => {}
    }
}

/// **Write the shell's whole endowment**: the introspection that makes "reading one literal tells
/// you a process's authority" real, pointed at the shell itself.
///
/// `budget_pages` is what init granted at boot rather than what is left, because there is no syscall
/// that reports the remainder. The caller passes its own constant and the row says "(initial)".
///
/// `clock` is the slot holding this shell's read-only clock page, or `None` for a wiring that
/// granted none (milestone 86). It is a slot number rather than a boolean because the number is not
/// fixed: a shell wired without a filesystem has one fewer capability below it, and printing a
/// number this shell did not actually get would make the table a story rather than a reading.
pub fn write_holdings(
    budget_pages: u64,
    holdings: Holdings,
    clock: Option<u64>,
    out: &mut dyn FnMut(&[u8]),
) {
    out(b"  this shell holds, and nothing else:\n");
    out(b"    cap 0  endpoint  terminal   read lines, write text\n");
    out(b"    cap 1  endpoint  spawn      direct init to start a program\n");
    out(b"    cap 2  endpoint  result     read a spawned program's answer\n");
    out(b"    cap 3  untyped   ");
    write_num(budget_pages, out);
    out(b" pages  the memory it grants with --mem (initial)\n");
    if holdings.dir {
        out(b"    cap 4  endpoint  directory  the files it can narrow into per-file grants\n");
    } else {
        out(b"    (no directory capability: a name on the line has nothing to narrow)\n");
    }
    // **The clock, and the rights row is the whole of it** (milestone 86). This shell reads the page
    // and holds no `GRANT` on it, so `time` can measure a command and nothing typed here can hand a
    // clock to a child: which processes can read the time is still decided by the manifests init
    // reads (DECISIONS §43), and the shell's own reading authority does not widen that set by one.
    //
    // That distinction is why the row prints the rights rather than just the object. A reader who
    // saw "clock" in this table and assumed the shell could pass it on would be wrong about the one
    // thing the table exists to answer.
    match clock {
        Some(slot) => {
            out(b"    cap ");
            write_num(slot, out);
            out(b"  frame     clock      READ only, NOT delegable: 'time' measures with\n");
            out(b"                                  it and no command can be handed it\n");
        }
        None => {
            out(b"    (no clock here: init endows 'date' a read-only clock page, this shell was\n");
            out(b"     granted none, so 'time' has nothing to measure with)\n");
        }
    }
    out(b"  it can name no devices and no other process. authority is what it holds.\n");
}

/// **Preview what a command line would grant**, which is the whole of `caps <command>`.
///
/// With an empty tail this is [`write_holdings`]. With a tail it is **a whole line, operators
/// included** (milestone 50), because what a pipeline grants is not the sum of what its stages grant
/// read one at a time: which slot holds what depends on where the stage is. Previewing the line the
/// user would run is the only preview worth having.
///
/// Nothing here moves any authority. That is the point of it being reachable from a host test at
/// all: the sentences a user reads before they decide to run something are checked without an
/// emulator, and without a shell that holds anything.
pub fn write_caps(
    tail: &[u8],
    budget_pages: u64,
    holdings: Holdings,
    clock: Option<u64>,
    expand: &mut dyn FnMut(&[u8]) -> Result<NameSet, Say>,
    out: &mut dyn FnMut(&[u8]),
) {
    let mut tail = grant_plan::trim(tail);
    if tail.is_empty() {
        return write_holdings(budget_pages, holdings, clock, out);
    }
    // **`caps time <command>` previews the command**, because that is what would run and `time`
    // moves no authority to it: the shell times with its own clock and the child is spawned with the
    // endowment the tail names, unchanged. A preview that stopped at the prefix word would answer
    // "this is not an invocation" about a line that is one, which is the drift between what you
    // inspect and what you run that `caps` exists to close.
    //
    // A loop rather than a recursive call, because `caps time time time date` is a line a person can
    // type and this function's frame carries a whole split `Line` by value. Bounded recursion on a
    // shell stack is a bug waiting for a long line (notes/pipes.md has two of those already).
    let mut timed = false;
    while let Command::Time(inner) = grant_plan::parse(tail) {
        timed = true;
        tail = grant_plan::trim(inner);
        if tail.is_empty() {
            return write_untimed(Untimed::NothingToTime, out);
        }
    }
    if timed {
        out(b"  time grants nothing; what it would run:\n");
    }
    let l = match line::split(tail) {
        Ok(l) => l,
        Err(r) => return write_say(Say::Cannot(r), out),
    };
    let sink_file = match l.output {
        Some(t) => match grant_plan::redirect_target(t, holdings, true) {
            Ok(g) => Some(g),
            Err(r) => return write_say(Say::Cannot(r), out),
        },
        None => None,
    };
    let source_file = match l.input {
        Some(t) => match grant_plan::redirect_target(t, holdings, false) {
            Ok(g) => Some(g),
            Err(r) => return write_say(Say::Cannot(r), out),
        },
        None => None,
    };
    let diag_file = match l.diagnostics {
        Some(t) => match grant_plan::redirect_target(t, holdings, true) {
            Ok(g) => Some((g, l.diagnostics_mode())),
            Err(r) => return write_say(Say::Cannot(r), out),
        },
        None => None,
    };
    for (i, stage) in l.stages().iter().enumerate() {
        // Only a program invocation carries a grant to preview; `caps help` has nothing to say.
        let Command::Run(spec) = grant_plan::parse(stage) else {
            out(b"  caps previews a command's grant; try: caps budgeter --mem 16\n");
            return;
        };
        let expanded = match expansion(&spec, expand) {
            Ok(e) => e,
            Err(Say::Cannot(r)) => return write_refusal(&spec, r, out),
            Err(said) => return write_say(said, out),
        };
        let streams = Streams {
            sink: l.sink_for(i, sink_file),
            source: l.source_for(i, source_file),
            diagnostics: diag_file,
        };
        match grant_plan::plan_stage(&spec, holdings, expanded, streams) {
            Err(refusal) => return write_refusal(&spec, refusal, out),
            Ok(e) => write_preview(&e, out),
        }
    }
}

/// Write the endowment a resolved invocation would hand the new process.
pub fn write_preview(e: &Endowment, out: &mut dyn FnMut(&[u8])) {
    out(b"  ");
    out(e.prog.name().as_bytes());
    out(b" would grant the new process, and nothing else:\n");
    out(b"    cap 0  endpoint  result   report its answer back\n");
    if e.mem_pages > 0 {
        out(b"    cap 1  untyped   ");
        write_num(e.mem_pages, out);
        out(b" pages  split from this shell's budget\n");
    }
    // A file endowment reads as one line naming the file and the direction, because that IS the
    // whole authority: an endpoint served by a file caretaker that will answer for this name and no
    // other. The direction comes from the program's manifest, not from anything typed, which is why
    // it is worth printing: the line you typed plus this table is the child's complete authority.
    if let Some(g) = e.file {
        out(b"    cap 2  endpoint  file     ");
        out(g.name.as_bytes());
        out(if g.writable {
            b"  (read+write, and nothing else on the disk)\n".as_slice()
        } else {
            b"  (read-only, and nothing else on the disk)\n".as_slice()
        });
    }
    // **A directory endowment is the subtree at risk, printed before anything happens**, which is
    // the argument for `rm` being a program rather than a builtin: a builtin would have run with
    // this shell's entire endowment and there would have been nothing to print. The `-r` line is
    // the load-bearing half, because typing that option is what widens the capability from "may
    // take a name out of this directory" to "may walk everything under it".
    if let Some(g) = e.dir {
        out(b"    cap 2  endpoint  dir      ");
        let mut buf = [0u8; nav::RENDER_MAX];
        let n = g.dir.render(&mut buf);
        out(&buf[..n]);
        out(b"  (the directory holding ");
        // **The names, all of them, and this is the point of previewing a set at all.** `caps rm
        // *.txt` prints exactly what `echo *.txt` prints, because both render the same expansion:
        // the authority about to move is on the screen before anything moves it, which is a claim
        // Unix cannot make about its own `rm`.
        write_set(&g.names, out);
        out(b")\n");
        if g.subtree {
            out(b"           ...and everything under it: -r grants the walk\n");
        } else {
            out(b"           ...and nothing under it: no -r, so it cannot even look\n");
        }
    }
    // **The clock, which no token on the line designates.** It is init's to endow rather than the
    // shell's, and it is still part of this child's complete authority, so `caps` prints it: a
    // reader who took the command line for the whole story would be wrong by exactly one capability.
    // The row says *read-only* because that is the entire reason `date` cannot set the time
    // (DECISIONS §43): there is no flag it could pass and no method it could call.
    if e.prog.manifest().clock {
        out(b"    cap 1  frame     clock    read-only. it can read the time and not set it,\n");
        out(b"                              and no token on the line could have asked for more\n");
    }
    // **The row this milestone exists to print.** On Linux there is nothing here to say: `ps` reads
    // /proc and the answer is "every process on the machine", which no command line chose and no
    // tool can narrow. Here the scope is a capability, so it is a line a person can read before
    // anything is spawned, and a wider grant would be a different line rather than an invisible one.
    if e.prog.manifest().domain {
        // `ENUMERATE`, and the word is the point of the line rather than decoration: it is the
        // right that lets this program *name* the domain's members and not the one that would let
        // it receive their deaths or collect them. Printing `READ` here would describe a wider
        // grant than the one being made, which is the failure a `caps` output has available.
        out(b"    cap 7  endpoint  domain   ENUMERATE. the processes this shell's jobs are\n");
        out(b"                              supervised by, and no others. it can name them and\n");
        out(b"                              do nothing to them: not receive their deaths, not\n");
        out(
            b"                              collect them, and not learn anything about a process\n",
        );
        // **The last line was an overclaim until 2026-08-17**, and the audit that found it
        // (design/audit-reports/) fixed the sentence rather than the mechanism, on purpose. It read
        // "and not learn that a process outside this domain exists", which is false: `SURVEY`
        // returns a cursor that is a machine-wide thread-table slot index, and a tid whose low half
        // is the same index, so a viewer with two members can subtract and count the threads
        // created between them. It still cannot name one. Narrowing the claim to what the mechanism
        // actually delivers is the honest half-step; scoping the cursor to the domain is a
        // milestone, and notes/process-view.md's `BUGS` carries the disposition.
        out(b"                              outside this domain but that it exists\n");
    }
    // **Where its output goes**, which is the demonstration milestone 50 owed: the destination is a
    // capability rather than an integer with a convention attached, so `caps` can name it. On Unix
    // the same question has no answer at this point, because fd 1 is whatever the shell's fd 1
    // happened to be and nothing records what that was.
    match e.sink {
        line::Sink::Report => {
            out(
                b"    output   this shell's result endpoint (it reads the bytes and prints them)\n",
            );
        }
        line::Sink::Pipe => {
            out(b"    output   an endpoint into the next stage. no file, no buffer, no object:\n");
            out(b"             the rendezvous IS the pipe\n");
        }
        // The row names the file, and the parenthesis names what the program actually holds, which
        // is not the file. Its slot 0 is this shell's result endpoint either way; `>` is where the
        // shell puts the bytes, not something the child was handed. So a redirected program still
        // cannot seek, truncate, re-read or stat, and this is where a reader can see why.
        line::Sink::File(g, mode) => {
            out(b"    output   ");
            out(g.name.as_bytes());
            out(b"  (this shell writes the bytes there; the program holds\n");
            out(b"             an endpoint and cannot seek, truncate, re-read or stat)\n");
            // The one line where `>` and `>>` differ, and it is about the shell rather than about
            // the child: whichever was typed, what the child holds is the same endpoint. Printed
            // because it is the only visible consequence of the operator, and because a person
            // about to overwrite a file should be able to see that from the preview.
            out(if matches!(mode, line::Mode::Append) {
                b"             this shell keeps what is already in it and writes after it\n"
                    .as_slice()
            } else {
                b"             this shell empties it first, before the command runs\n".as_slice()
            });
        }
    }
    // **The second stream, for a program that declares one** (DECISIONS §67). It is printed even
    // when no `2>` is on the line, and that is the row worth having: a reader who took `date >
    // when.txt` for "everything date says goes into when.txt" would be wrong, and this is where they
    // can see it. A program that declares none gets no row, because it has no second stream to hide.
    match e.diagnostics {
        line::Diagnostics::None => {}
        line::Diagnostics::Printed => {
            out(b"    diags    the terminal's own sink, a component this shell does not hold.\n");
            out(b"             declared by the program, so a > cannot swallow them, 2> can\n");
            out(b"             name them, and they reach the screen without passing here\n");
        }
        line::Diagnostics::File(g, mode) => {
            out(b"    diags    ");
            out(g.name.as_bytes());
            out(b"  (this shell writes them there; the program holds a\n");
            out(b"             second endpoint and still cannot seek, truncate or stat)\n");
            out(if matches!(mode, line::Mode::Append) {
                b"             this shell keeps what is already in it and writes after it\n"
                    .as_slice()
            } else {
                b"             this shell empties it first, before the command runs\n".as_slice()
            });
        }
    }
    match e.source {
        line::Source::None => {}
        line::Source::Pipe => out(b"    input    the previous stage's output\n"),
        line::Source::File(g) => {
            out(b"    input    ");
            out(g.name.as_bytes());
            out(b"  (this shell reads it and streams it in; the program\n");
            out(b"             holds an endpoint, not a file)\n");
        }
    }
    out(b"    arg    ");
    // **Read the manifest, do not keep a second list of which programs take an argument.** This
    // was `matches!(e.prog, Prog::Worker)` until 2026-08-16, so every other argument-taking program
    // previewed `arg (none)` while the shell went on to hand it the argument anyway. That is the
    // worst possible direction for this particular line to be wrong in: the next thing it prints is
    // that reading the command is reading its whole authority, and a preview that under-reports
    // authority is one a reader would trust. Found by someone adding their first program, who hit
    // it because the manifest already knew the answer.
    if e.prog.manifest().arg == ArgSpec::Required {
        write_num(e.arg, out);
        out(b"\n");
    } else {
        out(b"(none)\n");
    }
    out(b"  reading the command is reading its whole authority.\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::string::String;
    use std::vec::Vec;

    use grant_plan::PROG_COUNT;
    use grant_plan::expand::NameSet;

    /// Run a renderer and collect what it wrote. Every test below reads the shell's own output,
    /// which is the thing that used to need a booted kernel and a terminal to see at all.
    fn shown(f: impl FnOnce(&mut dyn FnMut(&[u8]))) -> String {
        let mut buf: Vec<u8> = Vec::new();
        f(&mut |b| buf.extend_from_slice(b));
        String::from_utf8(buf).expect("the shell writes ASCII")
    }

    fn spec_of(line: &[u8]) -> RunSpec<'_> {
        match grant_plan::parse(line) {
            Command::Run(spec) => spec,
            other => panic!("expected an invocation, parsed {other:?}"),
        }
    }

    /// A directory holding `names`, standing in for the FS request the shell would make.
    fn listing(names: &[&'static [u8]]) -> NameSet {
        let mut set = NameSet::empty();
        for n in names {
            assert!(set.push(n, false), "fixture too large for a NameSet");
        }
        set
    }

    // ---- `apropos`: the answer a search prints (milestone 40 phase 2) ----

    #[test]
    fn a_search_answer_names_pages_a_reader_can_type() {
        let mut r = manual::index::Ranked::new();
        r.offer(b"swish", b"notes/pipes.md", b"Pipes and redirection", 31);
        r.offer(b"kernel", b"notes/ipc-naming.md", b"Who does IPC name?", 11);
        let s = shown(|o| write_apropos(b"capability", &r, o));

        // **The typeable name is the deliverable.** A result a person cannot act on is a list of
        // titles, and `doc doc/swish/pipes.md` is the line this answer exists to produce.
        assert!(s.contains("doc/swish/pipes.md"), "{s}");
        assert!(s.contains("doc/kernel/ipc-naming.md"), "{s}");
        assert!(s.contains("Pipes and redirection"), "{s}");
        // Strongest first, and the source path is not printed: it is provenance, and a second path
        // on the line would compete with the one the reader is meant to type.
        let pipes = s.find("pipes.md").expect("the first result");
        let ipc = s.find("ipc-naming.md").expect("the second result");
        assert!(pipes < ipc, "{s}");
        assert!(!s.contains("notes/pipes.md"), "{s}");
        // The titles line up, which is what makes a list of five scannable rather than ragged.
        let cols: Vec<usize> = s
            .lines()
            .map(|l| l.find("Pipes").or_else(|| l.find("Who")).unwrap_or(0))
            .filter(|&c| c > 0)
            .collect();
        assert_eq!(cols, [APROPOS_TITLE, APROPOS_TITLE], "{s}");
    }

    #[test]
    fn a_search_that_found_nothing_says_so_in_the_words_of_the_question() {
        let r = manual::index::Ranked::new();
        let s = shown(|o| write_apropos(b"quantum", &r, o));
        assert_eq!(s, "  no page in the store says quantum\n");
    }

    #[test]
    fn a_truncated_answer_says_how_much_it_dropped() {
        // The half that keeps the answer honest: printing sixteen results out of forty without
        // saying so implies the store holds sixteen.
        let mut r = manual::index::Ranked::new();
        for i in 0..manual::index::RESULTS_MAX + 4 {
            r.offer(b"kernel", b"notes/x.md", b"X", i as u16);
        }
        let s = shown(|o| write_apropos(b"capability", &r, o));
        assert!(s.contains("16 of 20 pages, strongest first"), "{s}");
    }

    // ---- routing: the ordering with a bug behind it ----

    #[test]
    fn caps_is_answered_before_the_line_is_split() {
        // The whole reason `route` asks about `caps` first. Splitting first would hand the preview
        // the word `date` and lose the pipe, and the preview would then be a true statement about a
        // line the user did not type.
        match route(b"caps date | wc") {
            Route::Caps(tail) => assert_eq!(tail, b"date | wc"),
            other => panic!("expected caps, got {other:?}"),
        }
    }

    #[test]
    fn a_line_with_no_operators_is_one_command() {
        match route(b"  worker 7  ") {
            Route::One(stage) => assert_eq!(grant_plan::trim(stage), b"worker 7"),
            other => panic!("expected one command, got {other:?}"),
        }
    }

    #[test]
    fn operators_make_a_pipeline_of_the_stages_typed() {
        match route(b"date | wc") {
            Route::Pipeline(l) => {
                assert_eq!(l.stages().len(), 2);
                assert_eq!(grant_plan::trim(l.stages()[0]), b"date");
                assert_eq!(grant_plan::trim(l.stages()[1]), b"wc");
            }
            other => panic!("expected a pipeline, got {other:?}"),
        }
    }

    #[test]
    fn a_redirection_with_nothing_after_it_runs_nothing() {
        // A refusal at routing time is a line that never reaches a planner, so nothing is spawned
        // and nothing is opened. That it is reachable here at all is the lift: it used to need a
        // booted shell to provoke.
        match route(b"date >") {
            Route::Cannot(r) => assert_eq!(r, Refusal::NoRedirectTarget),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    // ---- pattern versus text ----

    #[test]
    fn a_word_with_no_magic_is_text_even_when_it_is_not_a_path() {
        // `has_magic` first. `a:b` carries a byte a name cannot carry, so asking "is this a nameable
        // path" first would refuse an ordinary `echo` word.
        assert_eq!(is_pattern(b"a:b"), Ok(false));
        assert_eq!(is_pattern(b"report.txt"), Ok(false));
    }

    #[test]
    fn a_pattern_off_the_last_component_is_refused() {
        assert_eq!(is_pattern(b"*.txt"), Ok(true));
        assert_eq!(is_pattern(b"docs/*.txt"), Ok(true));
        // Selecting directories to walk is an authority question, not a matching one.
        assert_eq!(is_pattern(b"*/report.txt"), Err(Refusal::PatternInPath));
    }

    #[test]
    fn only_the_first_pattern_on_a_line_is_expanded() {
        let mut asked: Vec<Vec<u8>> = Vec::new();
        let e = expansion(&spec_of(b"rm a.txt *.log *.bak"), &mut |t| {
            asked.push(t.to_vec());
            Ok(listing(&[b"one.log"]))
        })
        .expect("the fixture matches");
        // The literal was skipped, the first pattern expanded, and the second never reached the
        // directory: it is already an unplaceable token and a refusal at plan time.
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0], b"*.log");
        assert!(e.for_positional(0).is_none());
        assert_eq!(e.for_positional(1).expect("expanded here").len(), 1);
    }

    #[test]
    fn a_line_with_no_pattern_reads_no_directory() {
        let mut asked = 0;
        let e = expansion(&spec_of(b"wc report.txt"), &mut |_| {
            asked += 1;
            Ok(NameSet::empty())
        })
        .expect("nothing to expand");
        assert_eq!(asked, 0);
        assert!(e.for_positional(0).is_none());
    }

    #[test]
    fn a_pattern_the_shell_cannot_read_stops_the_line() {
        let said = expansion(&spec_of(b"rm *.txt"), &mut |_| Err(Say::NoDirectory))
            .expect_err("the shell holds nothing to expand against");
        assert_eq!(said, Say::NoDirectory);
    }

    // ---- echo, the half of globbing that costs no authority ----

    #[test]
    fn echo_copies_ordinary_words_byte_for_byte() {
        let out = shown(|o| {
            echo(
                b"  two  spaces ",
                Status::Ran,
                &mut |_| Ok(NameSet::empty()),
                o,
            );
        });
        assert_eq!(out, "  two  spaces ");
    }

    #[test]
    fn echo_prints_exactly_what_the_same_pattern_would_grant() {
        let set = listing(&[b"notes.txt", b"report.txt"]);
        let printed = shown(|o| {
            echo(b"*.txt", Status::Ran, &mut |_| Ok(set), o);
        });
        let granted = shown(|o| write_set(&set, o));
        // One renderer, so `echo *.txt` and `caps rm *.txt` cannot disagree about what the line
        // designates. That agreement is the whole demonstration.
        assert_eq!(printed, granted);
        assert_eq!(printed, "notes.txt report.txt");
    }

    #[test]
    fn echo_writes_no_empty_runs() {
        // In the program `out` is the shell's `print`, and every call is one CALL on the terminal
        // endpoint. A word with nothing before it must not cost a round trip that carries no
        // bytes, which is what emitting the zero-length whitespace run ahead of it would be.
        let mut writes: Vec<Vec<u8>> = Vec::new();
        let said = echo(
            b"one  two",
            Status::Ran,
            &mut |_| Ok(NameSet::empty()),
            &mut |b| {
                writes.push(b.to_vec());
            },
        );
        assert_eq!(said, Say::Nothing);
        assert!(writes.iter().all(|w| !w.is_empty()), "{writes:?}");
        assert_eq!(writes.concat(), b"one  two");
    }

    #[test]
    fn a_pattern_that_matched_nothing_stops_echo_rather_than_printing_itself() {
        // zsh's answer rather than bash's, and the model forces it: if `echo` printed the pattern
        // where `rm` refuses, the two would disagree about what the line means.
        let mut printed = Vec::new();
        let said = echo(
            b"before *.txt after",
            Status::Ran,
            &mut |_| Err(Say::Cannot(Refusal::NoMatch)),
            &mut |b| printed.extend_from_slice(b),
        );
        assert_eq!(said, Say::Cannot(Refusal::NoMatch));
        assert_eq!(printed, b"before ");
    }

    #[test]
    fn echo_refuses_a_pattern_it_cannot_place_without_reading_a_directory() {
        let mut asked = 0;
        let said = shown_say(|o| {
            echo(
                b"*/report.txt",
                Status::Ran,
                &mut |_| {
                    asked += 1;
                    Ok(NameSet::empty())
                },
                o,
            )
        });
        assert_eq!(said, Say::Cannot(Refusal::PatternInPath));
        assert_eq!(asked, 0);
    }

    fn shown_say(f: impl FnOnce(&mut dyn FnMut(&[u8])) -> Say) -> Say {
        f(&mut |_| {})
    }

    // ---- quoting: what a word is, and therefore what is designated (milestone 67) ----

    /// **The demonstration the milestone owes, in two lines.** The same four characters are a name
    /// when quoted and a set when not, and `echo` prints exactly what a grant would move either way.
    #[test]
    fn quoting_is_the_difference_between_naming_a_name_and_naming_a_set() {
        let set = listing(&[b"notes.txt", b"report.txt"]);
        let expanded = shown(|o| {
            echo(b"*.txt", Status::Ran, &mut |_| Ok(set), o);
        });
        assert_eq!(expanded, "notes.txt report.txt");

        let mut asked = 0;
        let literal = shown(|o| {
            echo(
                b"\"*.txt\"",
                Status::Ran,
                &mut |_| {
                    asked += 1;
                    Ok(set)
                },
                o,
            );
        });
        assert_eq!(literal, "*.txt");
        // And the directory was never read, so the quoting is a decision about the line rather than
        // a filter on what came back.
        assert_eq!(asked, 0);
    }

    #[test]
    fn a_quoted_word_keeps_the_spaces_inside_it() {
        let out = shown(|o| {
            echo(
                b"a \"two  spaces\" b",
                Status::Ran,
                &mut |_| Ok(NameSet::empty()),
                o,
            );
        });
        assert_eq!(out, "a two  spaces b");
    }

    /// The planner never sees a quoted pattern as a pattern, which is the same claim one layer up
    /// from [`quoting_is_the_difference_between_naming_a_name_and_naming_a_set`] and the one that
    /// decides what actually moves.
    #[test]
    fn a_quoted_operand_is_not_expanded_before_it_is_planned() {
        let mut asked = 0;
        let e = expansion(&spec_of(b"rm \"*.txt\""), &mut |_| {
            asked += 1;
            Ok(listing(&[b"one.txt"]))
        })
        .expect("nothing to expand");
        assert_eq!(asked, 0, "a quoted word must not reach the directory");
        assert!(e.for_positional(0).is_none());
        // The same line unquoted does read the directory, so the pair is a control rather than a
        // test of a code path nothing takes.
        let mut asked = 0;
        expansion(&spec_of(b"rm *.txt"), &mut |_| {
            asked += 1;
            Ok(listing(&[b"one.txt"]))
        })
        .expect("the fixture matches");
        assert_eq!(asked, 1);
    }

    /// A misquoted word stops `echo` where a misplaced pattern stops it, and for the same reason:
    /// the line does not designate what it looks like it designates.
    #[test]
    fn echo_refuses_a_word_it_cannot_read() {
        assert_eq!(
            shown_say(|o| echo(b"'unclosed", Status::Ran, &mut |_| Ok(NameSet::empty()), o)),
            Say::Cannot(Refusal::UnclosedQuote),
        );
        assert_eq!(
            shown_say(|o| echo(b"a\"b\"", Status::Ran, &mut |_| Ok(NameSet::empty()), o)),
            Say::Cannot(Refusal::PartlyQuoted),
        );
    }

    // ---- the status word ----

    /// `$?` is read where words are expanded, and its three values are the three things that can
    /// happen to a line here.
    #[test]
    fn the_status_word_reads_the_last_commands_status() {
        for (s, digit) in [
            (Status::Ran, "0"),
            (Status::Failed, "1"),
            (Status::Refused, "2"),
        ] {
            let out = shown(|o| {
                echo(b"$?", s, &mut |_| Ok(NameSet::empty()), o);
            });
            assert_eq!(out, digit);
        }
    }

    /// **Quoting turns it off**, which is the escape hatch and also the honest statement of where
    /// this shell is: both quote forms are literal today, so `"$?"` is two characters. When
    /// variables arrive the two forms have to stop being the same thing.
    #[test]
    fn a_quoted_status_word_is_two_characters() {
        let out = shown(|o| {
            echo(b"'$?' $?", Status::Failed, &mut |_| Ok(NameSet::empty()), o);
        });
        assert_eq!(out, "$? 1");
    }

    /// The status a line ends on is not a name, a handle or a capability. This pins the shape
    /// rather than a value: three variants, one small integer each, and nothing to designate with.
    #[test]
    fn a_status_carries_a_number_and_nothing_else() {
        assert_eq!(Status::default(), Status::Ran);
        assert!(Status::Ran.ok());
        assert!(!Status::Failed.ok() && !Status::Refused.ok());
        assert_eq!(
            (
                Status::Ran.code(),
                Status::Failed.code(),
                Status::Refused.code()
            ),
            (0, 1, 2),
        );
        for s in [Status::Ran, Status::Failed, Status::Refused] {
            let mut rendered = Vec::new();
            write_num(s.code(), &mut |b| rendered.extend_from_slice(b));
            assert_eq!(rendered, s.digits(), "the two spellings of {s:?} disagree");
        }
    }

    // ---- the sentences ----

    #[test]
    fn numbers_render_in_base_ten_including_the_ends() {
        assert_eq!(shown(|o| write_num(0, o)), "0");
        assert_eq!(shown(|o| write_num(7, o)), "7");
        assert_eq!(shown(|o| write_num(128, o)), "128");
        // Twenty digits, which is exactly the buffer. One more and this would have been a panic
        // nobody could have provoked from a prompt.
        assert_eq!(shown(|o| write_num(u64::MAX, o)), "18446744073709551615");
    }

    #[test]
    fn every_say_but_nothing_is_one_line() {
        assert_eq!(shown(|o| write_say(Say::Nothing, o)), "");
        for s in [
            Say::Refused(Refused::AtYourRoot),
            Say::Refused(Refused::NotAName),
            Say::Failed(-2),
            Say::NoDirectory,
            Say::NeedsAName,
            Say::Cannot(Refusal::NoMatch),
        ] {
            let line = shown(|o| write_say(s, o));
            assert!(line.ends_with('\n'), "{s:?} does not end its line");
            assert_eq!(line.matches('\n').count(), 1, "{s:?} printed two lines");
            assert!(line.len() > 4, "{s:?} printed nothing worth reading");
        }
    }

    #[test]
    fn a_filesystem_refusal_keeps_the_filesystems_own_words() {
        // The shell does not get to invent a friendlier sentence for an errno. This pins that the
        // rendering goes through `fs_proto`, which is where the number was chosen.
        assert_eq!(
            shown(|o| write_say(Say::Failed(-2), o)).trim(),
            fs_proto::dir::explain(-2)
        );
    }

    #[test]
    fn pwd_prints_the_position_relative_to_this_shells_own_root() {
        // The leading slash is this shell's root, not the system's. A shell holding one directory
        // capability has no name for anything above it, so `/` at depth zero is the honest answer
        // rather than a borrowed one, and `pwd` is the only place a user sees it.
        assert_eq!(shown(|o| write_pwd(&Cwd::root(), o)), "  /\n");

        let mut cwd = Cwd::root();
        assert!(cwd.descend(b"docs"));
        assert!(cwd.descend(b"drafts"));
        assert_eq!(shown(|o| write_pwd(&cwd, o)), "  /docs/drafts\n");
    }

    #[test]
    fn an_unresolvable_name_is_repeated_back_and_a_flag_only_line_is_not() {
        let named = shown(|o| write_refusal(&spec_of(b"cat 7"), Refusal::NoSuchProgram, o));
        assert!(named.starts_with("  cat: "), "{named}");

        // `--mem 16` names no program, and "  : the named program does not exist" would be worse
        // than the bare sentence.
        let flags_only = shown(|o| write_refusal(&spec_of(b"--mem 16"), Refusal::NoSuchProgram, o));
        assert!(!flags_only.contains(": "), "{flags_only}");
        assert!(flags_only.ends_with('\n'));
    }

    #[test]
    fn a_refusal_about_a_real_program_uses_the_canonical_name() {
        // The name the manifest carries, not the bytes typed, because the manifest is what refused.
        let s = shown(|o| write_refusal(&spec_of(b"worker"), Refusal::ArgRequired, o));
        assert!(s.starts_with("  worker: "), "{s}");
        // Nothing to prefix when the program did not resolve, whatever the refusal.
        let s = shown(|o| write_refusal(&spec_of(b"nope x"), Refusal::FileForbidden, o));
        assert!(!s.contains("nope"), "{s}");
    }

    // ---- the outcome of a spawn ----

    fn endowment(prog: Prog) -> Endowment {
        Endowment {
            prog,
            arg: 0,
            mem_pages: 0,
            file: None,
            dir: None,
            flags: 0,
            sink: line::Sink::Report,
            source: line::Source::None,
            diagnostics: match prog.manifest().output.diagnostics_slot() {
                Some(_) => line::Diagnostics::Printed,
                None => line::Diagnostics::None,
            },
            reports: true,
            interruptible: false,
            writes_while_reading: false,
        }
    }

    #[test]
    fn a_spawn_that_failed_says_so_whatever_the_program_was() {
        // The sentinel is checked before the program is looked at, which matters: `worker`'s arm
        // would otherwise report that a process computed `u64::MAX`.
        for prog in [Prog::Worker, Prog::Budgeter, Prog::Date] {
            let s = shown(|o| write_outcome(&endowment(prog), spawnproto::SPAWN_FAILED, o));
            assert!(s.contains("could not spawn"), "{prog:?}: {s}");
        }
    }

    #[test]
    fn the_two_programs_that_answer_with_a_number_report_it_in_their_own_terms() {
        let mut e = endowment(Prog::Worker);
        e.arg = 7;
        assert_eq!(
            shown(|o| write_outcome(&e, 49, o)),
            "  a process at EL0 computed 7*7 = 49\n"
        );

        let mut e = endowment(Prog::Budgeter);
        e.mem_pages = 16;
        let s = shown(|o| write_outcome(&e, 14, o));
        // Both numbers, because the gap between them is the page tables the grant paid for, and a
        // reader who sees only one of them cannot tell that the grant was honoured.
        assert!(s.contains("mapped 14 pages"), "{s}");
        assert!(s.contains("16-page budget"), "{s}");
    }

    #[test]
    fn a_program_that_reports_elsewhere_prints_nothing_here() {
        // Text-answering and supervised programs are drained by other readers. A line printed here
        // would be a second, empty report for the same run.
        for prog in [Prog::Date, Prog::Wc, Prog::Rm, Prog::Heeder, Prog::Spinner] {
            assert_eq!(shown(|o| write_outcome(&endowment(prog), 0, o)), "");
        }
    }

    // ---- the preview, which is the visibility surface ----

    #[test]
    fn a_memory_grant_is_a_row_and_no_grant_is_no_row() {
        let mut e = endowment(Prog::Budgeter);
        assert!(!shown(|o| write_preview(&e, o)).contains("untyped"));
        e.mem_pages = 16;
        assert!(shown(|o| write_preview(&e, o)).contains("cap 1  untyped   16 pages"));
    }

    #[test]
    fn append_and_truncate_differ_in_exactly_one_line_of_the_preview() {
        let grant = grant_plan::FileGrant {
            dir: Cwd::root(),
            name: grant_plan::expand::Name::new(b"out.txt").expect("a nameable file"),
            writable: true,
        };
        let mut e = endowment(Prog::Date);
        e.sink = line::Sink::File(grant, line::Mode::Truncate);
        let truncate: Vec<String> = shown(|o| write_preview(&e, o))
            .lines()
            .map(String::from)
            .collect();
        e.sink = line::Sink::File(grant, line::Mode::Append);
        let append: Vec<String> = shown(|o| write_preview(&e, o))
            .lines()
            .map(String::from)
            .collect();

        assert_eq!(truncate.len(), append.len());
        let differing = truncate.iter().zip(&append).filter(|(a, b)| a != b).count();
        // `>` and `>>` hand the child the identical capability, so exactly one line may differ, and
        // it has to be about the shell rather than about the child.
        assert_eq!(differing, 1);
        assert!(
            append
                .iter()
                .any(|l| l.contains("keeps what is already in it"))
        );
        assert!(truncate.iter().any(|l| l.contains("empties it first")));
    }

    #[test]
    fn a_directory_grant_prints_every_name_and_says_whether_r_was_typed() {
        let mut e = endowment(Prog::Rm);
        e.dir = Some(grant_plan::DirGrant {
            dir: Cwd::root(),
            names: listing(&[b"a.txt", b"b.txt"]),
            subtree: false,
        });
        let without = shown(|o| write_preview(&e, o));
        assert!(without.contains("a.txt b.txt"), "{without}");
        assert!(
            without.contains("no -r, so it cannot even look"),
            "{without}"
        );

        if let Some(g) = e.dir.as_mut() {
            g.subtree = true;
        }
        let with = shown(|o| write_preview(&e, o));
        assert!(with.contains("-r grants the walk"), "{with}");
    }

    #[test]
    fn date_says_the_clock_is_not_on_the_line() {
        // The preview must not let a reader believe the command line is the whole story: `date`'s
        // clock is init's to endow, no token could designate it, and it is still a capability the
        // child holds. So it is printed, and it is printed as read-only, which is the whole of why
        // there is no `date -s`.
        let s = shown(|o| write_preview(&endowment(Prog::Date), o));
        assert!(s.contains("cap 1  frame     clock"), "{s}");
        assert!(s.contains("read the time and not set it"), "{s}");
        // And a program that declares no clock is not given a row that says it has one.
        assert!(!shown(|o| write_preview(&endowment(Prog::Wc), o)).contains("clock"));
    }

    #[test]
    fn every_preview_names_where_the_output_goes() {
        // The demonstration milestone 50 owed: on Unix this question has no answer at this point,
        // because fd 1 is whatever the shell's fd 1 happened to be.
        for sink in [line::Sink::Report, line::Sink::Pipe] {
            let mut e = endowment(Prog::Date);
            e.sink = sink;
            assert!(
                shown(|o| write_preview(&e, o)).contains("    output   "),
                "{sink:?} left the destination unnamed"
            );
        }
    }

    // ---- the shell's own endowment ----

    #[test]
    fn holding_a_directory_changes_exactly_one_line_of_the_endowment() {
        let with = shown(|o| {
            write_holdings(
                128,
                Holdings {
                    dir: true,
                    cwd: Cwd::root(),
                },
                None,
                o,
            );
        });
        let without = shown(|o| write_holdings(128, Holdings::default(), None, o));
        let differing = with
            .lines()
            .zip(without.lines())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(differing, 1);
        assert!(with.contains("cap 4  endpoint  directory"));
        assert!(without.contains("no directory capability"));
        // The budget row is the one number the shell cannot query, so it must be the caller's.
        assert!(with.contains("128 pages"));
    }

    /// **A clock in the endowment is one row, and the row says it cannot be handed on** (milestone
    /// 86).
    ///
    /// The rights half is the whole point. Before this milestone `caps` said the shell held no clock
    /// and could not endow one; now it holds one and *still* cannot endow one, because what it was
    /// granted is `READ` without `GRANT`. Those are different sentences about the same guarantee, and
    /// a table that printed only the object would have lost it.
    #[test]
    fn a_clock_is_one_row_and_it_says_the_shell_cannot_pass_it_on() {
        let with = shown(|o| write_holdings(128, Holdings::default(), Some(5), o));
        assert!(with.contains("cap 5  frame     clock"), "{with}");
        assert!(with.contains("NOT delegable"), "{with}");

        // The slot is the caller's to state rather than a constant here, because it moves with the
        // wiring: a shell granted no directory has one fewer capability under it.
        let lower = shown(|o| write_holdings(128, Holdings::default(), Some(4), o));
        assert!(lower.contains("cap 4  frame     clock"), "{lower}");

        // And a shell granted none says what is missing and what it costs, rather than leaving a
        // reader to wonder why `time` refuses.
        let without = shown(|o| write_holdings(128, Holdings::default(), None, o));
        assert!(without.contains("was\n     granted none"), "{without}");
        assert!(without.contains("nothing to measure with"), "{without}");
    }

    // ---- caps over a whole line ----

    #[test]
    fn caps_with_no_tail_is_the_shells_own_endowment() {
        let tail = shown(|o| {
            write_caps(
                b"   ",
                128,
                Holdings::default(),
                None,
                &mut |_| Ok(NameSet::empty()),
                o,
            );
        });
        let direct = shown(|o| write_holdings(128, Holdings::default(), None, o));
        assert_eq!(tail, direct);
    }

    #[test]
    fn caps_previews_every_stage_of_a_pipeline() {
        let s = shown(|o| {
            write_caps(
                b"date | wc",
                128,
                Holdings::default(),
                None,
                &mut |_| Ok(NameSet::empty()),
                o,
            );
        });
        // Two stages, two tables, and the pipe named on both sides: the first stage's output is an
        // endpoint into the second, and the second's input is the first's output. Reading one stage
        // at a time could not have shown that.
        assert!(s.contains("date would grant"), "{s}");
        assert!(s.contains("wc would grant"), "{s}");
        assert!(s.contains("the rendezvous IS the pipe"), "{s}");
        assert!(s.contains("the previous stage's output"), "{s}");
    }

    /// The preview's `arg` line must follow the manifest, for **every** program rather than for the
    /// one that happens to take an argument today.
    ///
    /// This is written as a sweep over the whole enum on purpose. The line used to read
    /// `matches!(e.prog, Prog::Worker)`, which is correct for the tree as it stands (`Worker` is the
    /// only `ArgSpec::Required` program) and silently wrong for the next one added: the shell would
    /// print `arg (none)` and then hand the argument over anyway. A test naming `Worker` would have
    /// passed against the bug. A test that asks the manifest cannot.
    #[test]
    fn the_arg_line_follows_the_manifest_for_every_program() {
        for id in 0..PROG_COUNT as u64 {
            let Some(prog) = Prog::from_id(id) else {
                continue;
            };
            let takes_arg = prog.manifest().arg == ArgSpec::Required;
            let line = std::format!("{} 21", prog.name());
            let s = shown(|o| {
                write_caps(
                    line.as_bytes(),
                    128,
                    Holdings::default(),
                    None,
                    &mut |_| Ok(NameSet::empty()),
                    o,
                );
            });
            // A program that refuses an argument is refused before any table is printed, so the
            // preview only has an `arg` line to get wrong when the manifest allows one.
            if takes_arg {
                assert!(
                    s.contains("arg    21"),
                    "{} takes an argument and the preview did not show it:\n{s}",
                    prog.name(),
                );
            } else {
                assert!(
                    !s.contains("arg    21"),
                    "{} takes no argument and the preview showed one:\n{s}",
                    prog.name(),
                );
            }
        }
    }

    #[test]
    fn caps_refuses_a_pipeline_whose_stage_has_no_bytes_to_pipe() {
        // `worker` answers with a number, not a stream, so there is nothing for `|` to carry. The
        // preview refuses the same line the prompt would, which is the property that makes `caps`
        // worth typing before a command rather than after it.
        let s = shown(|o| {
            write_caps(
                b"worker 7 | wc",
                128,
                Holdings::default(),
                None,
                &mut |_| Ok(NameSet::empty()),
                o,
            );
        });
        assert!(s.starts_with("  worker: "), "{s}");
        assert!(!s.contains("would grant"), "{s}");
    }

    #[test]
    fn caps_refuses_the_line_it_would_have_refused_at_the_prompt() {
        // A shell holding no directory cannot back a name, and the preview says so rather than
        // printing a grant that would not happen.
        let s = shown(|o| {
            write_caps(
                b"wc report.txt",
                128,
                Holdings::default(),
                None,
                &mut |_| Ok(NameSet::empty()),
                o,
            );
        });
        assert!(s.contains("wc: "), "{s}");
        assert!(!s.contains("would grant"), "{s}");
    }

    #[test]
    fn caps_says_so_when_the_tail_is_not_an_invocation() {
        let s = shown(|o| {
            write_caps(
                b"help",
                128,
                Holdings::default(),
                None,
                &mut |_| Ok(NameSet::empty()),
                o,
            );
        });
        assert!(s.contains("caps previews a command's grant"), "{s}");
    }

    /// **`caps time <command>` previews the command, unchanged** (milestone 86).
    ///
    /// The assertion is equality with the preview of the same line untimed, not a substring, because
    /// the claim is that `time` moves no authority at all. One extra line of prose says the prefix
    /// was seen; every row of the table has to be identical, or the shell would be previewing one
    /// endowment and spawning another.
    #[test]
    fn caps_of_a_timed_command_previews_the_command_itself() {
        let preview = |line: &'static [u8]| {
            shown(|o| {
                write_caps(
                    line,
                    128,
                    Holdings::default(),
                    Some(5),
                    &mut |_| Ok(NameSet::empty()),
                    o,
                );
            })
        };
        let timed = preview(b"time budgeter --mem 16");
        let plain = preview(b"budgeter --mem 16");
        assert_eq!(
            timed.strip_prefix("  time grants nothing; what it would run:\n"),
            Some(plain.as_str()),
            "timing a command must not change one row of what it would be granted",
        );

        // Nested prefixes collapse rather than recursing, and a prefix with nothing after it is the
        // same complaint the prompt makes.
        assert_eq!(preview(b"time time budgeter --mem 16"), timed);
        assert!(preview(b"time").contains("name a command to time"));
    }

    // ---- time: the duration, and the three reasons there is not one ----

    /// **The unit boundaries, which are the only place this arithmetic can be wrong.**
    ///
    /// Each pair is a value just under a boundary and the one just over it, because a rendering that
    /// picks its unit with the wrong comparison is right everywhere else. The zero-padding is here
    /// for the same reason: `4.13 ms` looks like a number and is a different one from `4.013 ms`.
    #[test]
    fn a_duration_reads_in_the_largest_unit_that_keeps_it_readable() {
        let shown_ns = |nanos| shown(|o| write_duration(nanos, o));

        assert_eq!(shown_ns(0), "0.000 us");
        assert_eq!(shown_ns(999_999), "999.999 us");
        assert_eq!(shown_ns(1_000_000), "1.000 ms");
        assert_eq!(shown_ns(999_999_999), "999.999 ms");
        assert_eq!(shown_ns(1_000_000_000), "1.000 s");
        // The padding, at each width, because the digits come from a writer that omits leading zeros.
        assert_eq!(shown_ns(1_001_000), "1.001 ms");
        assert_eq!(shown_ns(1_010_000), "1.010 ms");
        assert_eq!(shown_ns(1_100_000), "1.100 ms");
        // Sub-tick values still render: the counter's resolution is a fact about the input, and a
        // renderer that refused small numbers would be lying about a different thing.
        assert_eq!(shown_ns(7), "0.007 us");
    }

    /// **A timing line is one line and says `real`**, and after §72 there is no second line it can
    /// grow: the stepped-clock disclaimer went with the clock, because a monotonic counter cannot
    /// be stepped.
    #[test]
    fn a_timing_line_reports_the_duration_and_nothing_else() {
        let line = shown(|o| write_timing(4_213_000, o));
        assert_eq!(line, "  time: real 4.213 ms\n");
        // `real` is Unix's word, and the absence of `user` and `sys` is the honest part: nothing in
        // this kernel is asked what a thread spent.
        assert!(!line.contains("user"));
        assert!(!line.contains("sys"));
        // The failure mode this used to disclaim is now unreachable rather than tolerated.
        assert!(!line.contains("stepped"));
    }

    /// **The one refusal left is a usage error**, not a capability one. `time` reads an ambient
    /// counter (§72), so there is no machine on which it must decline to measure.
    #[test]
    fn an_untimed_line_says_the_one_reason_it_can_be() {
        let said = |r| shown(|o| write_untimed(r, o));
        assert!(said(Untimed::NothingToTime).contains("name a command to time"));
        // Attributed, because a bare sentence at a prompt reads as the command's complaint rather
        // than the shell's.
        assert!(
            said(Untimed::NothingToTime).starts_with("  time: "),
            "{:?}",
            said(Untimed::NothingToTime)
        );
    }

    // ---- help ----

    #[test]
    fn help_names_every_verb_the_parser_accepts() {
        // The drift this catches is the cheap one to introduce and the expensive one to notice: a
        // builtin added to the parser and not to the help, which is then a feature nobody at the
        // prompt can find.
        let text = shown(write_help);
        for verb in [
            "help", "echo", "caps", "time", "xargs", "cd", "pwd", "ls", "mkdir",
        ] {
            assert!(text.contains(verb), "help does not mention {verb}");
            // And it really is a verb this shell answers, rather than a word in a sentence.
            assert!(
                !matches!(grant_plan::parse(verb.as_bytes()), Command::Run(_)),
                "{verb} is not a builtin any more; the help is stale"
            );
        }
        for op in ["a | b", "a > name", "a >> name", "a < name", "a 2> name"] {
            assert!(text.contains(op), "help does not mention {op}");
        }
    }

    // ---- `2>`, the declared second stream (DECISIONS §67) ----

    /// **The preview names the second stream even when the line does not**, which is the row that
    /// stops `caps date > when.txt` from being a half-truth: the answer goes into the file and the
    /// complaint does not, and a reader can see both destinations before running anything.
    #[test]
    fn a_declarer_shows_where_its_second_stream_goes() {
        let mut e = endowment(Prog::Date);
        e.sink = line::Sink::File(
            grant_plan::FileGrant {
                dir: Cwd::root(),
                name: grant_plan::expand::Name::new(b"when.txt").expect("a nameable file"),
                writable: true,
            },
            line::Mode::Truncate,
        );
        let s = shown(|o| write_preview(&e, o));
        assert!(s.contains("    output   when.txt"), "{s}");
        assert!(
            // And the row says the destination is **not** this shell, which is what the terminal's
            // own sink adapter exists for: those bytes reach the screen without passing through
            // here, so nothing the shell does to the output can reach them.
            s.contains("    diags    the terminal's own sink"),
            "{s}"
        );

        // And a program that declares none has no row at all: there is no second stream to hide,
        // so inventing a line about one would be the preview claiming more than the manifest does.
        let s = shown(|o| write_preview(&endowment(Prog::Wc), o));
        assert!(!s.contains("diags"), "{s}");
    }

    /// `caps` refuses the `2>` the prompt would refuse, which is what makes previewing worth doing
    /// before a command rather than after it.
    #[test]
    fn caps_refuses_a_second_stream_the_program_never_declared() {
        let s = shown(|o| {
            write_caps(
                b"worker 7 2> err.txt",
                128,
                Holdings {
                    dir: true,
                    cwd: Cwd::root(),
                },
                None,
                &mut |_| Ok(NameSet::empty()),
                o,
            );
        });
        assert!(s.starts_with("  worker: "), "{s}");
        assert!(s.contains("declares no second output"), "{s}");
        assert!(!s.contains("would grant"), "{s}");
    }
}
