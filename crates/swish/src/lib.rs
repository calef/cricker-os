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
//!   including the one ordering that has a bug behind it: `caps` is answered *before* the line is
//!   split on its operators, or `caps date | wc` would lose everything after the pipe.
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
//! let said = echo(b"the set: *.txt", &mut listing, &mut |b| printed.extend_from_slice(b));
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

#![no_std]

use fs_proto::dir;
use grant_plan::expand::{Expansion, NameSet};
use grant_plan::line::{self, Line};
use grant_plan::nav::{self, Cwd, Refused};
use grant_plan::{Command, Endowment, Holdings, Prog, Refusal, RunSpec, Streams, spawnproto};

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

/// **What kind of thing the user typed**, decided before anything is invoked.
///
/// The variants are not three grammars. They are the three shapes the shell has to act on
/// differently, and the order they are decided in is load-bearing (see [`route`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route<'a> {
    /// `caps`, with everything after it. The tail is a **whole command line**, operators included.
    Caps(&'a [u8]),
    /// One command, with no operators on it: the path every line took before milestone 50.
    One(&'a [u8]),
    /// A line with `>`, `<` or `|` on it, already split into stages.
    Pipeline(Line<'a>),
    /// The line cannot be split at all, so there is nothing to run.
    Cannot(Refusal),
}

/// **Read one line and decide what it is.**
///
/// `caps` is asked **before** the line is split, and that ordering is the whole reason this is a
/// function worth having. Its operand *is* a command line, so it has to see the operators to preview
/// what they do: `caps date | wc` must print two stages, and splitting first would hand it the word
/// `date` and silently lose the rest.
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
/// assert!(matches!(route(b"worker 7"), Route::One(b"worker 7")));
/// match route(b"date | wc") {
///     Route::Pipeline(l) => assert_eq!(l.stages().len(), 2),
///     other => panic!("expected a pipeline, got {other:?}"),
/// }
/// ```
pub fn route(cmd: &[u8]) -> Route<'_> {
    if let Command::Caps(tail) = grant_plan::parse(cmd) {
        return Route::Caps(tail);
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
pub fn echo(
    text: &[u8],
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
        while i < text.len() && !text[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == word {
            continue;
        }
        let token = &text[word..i];
        match is_pattern(token) {
            Ok(false) => out(token),
            Ok(true) => match expand(token) {
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
    out(b"  cd [path]               move inside the directory you hold ('cd' is your root)\n");
    out(b"  pwd                     where you are, relative to YOUR root\n");
    out(b"  ls [path]               list a directory you can reach\n");
    out(b"  mkdir <path>            make a directory\n");
    out(b"  rm [-rfv] <path>        a PROGRAM, granted the directory holding what you name\n");
    out(b"  worker <n>              spawn a process that returns n*n\n");
    out(b"  budgeter --mem N        grant a process N pages from this shell's budget\n");
    out(b"  date                    print the wall-clock time\n");
    out(b"  wc                      count lines, words and bytes on its INPUT\n");
    out(b"  <prog> <name>           grant a process one file, and only that file\n");
    out(b"\n  operators (milestone 50). > and | are the same mechanism: a different\n");
    out(b"  capability in a program's output slot, which it cannot look behind.\n");
    out(b"  a | b                   a's output becomes b's input; the pipe is an endpoint\n");
    out(b"  a > name                a's output becomes a file it can append to and nothing else\n");
    out(b"  a >> name               the same, without emptying the file first\n");
    out(b"  a < name                a file's bytes become a's input\n");
    out(b"  echo hello | wc         a builtin can lead a pipeline: this shell writes the bytes\n");
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
        Prog::Heeder | Prog::Spinner | Prog::Date | Prog::Rm | Prog::Wc => {}
    }
}

/// **Write the shell's whole endowment**: the introspection that makes "reading one literal tells
/// you a process's authority" real, pointed at the shell itself.
///
/// `budget_pages` is what init granted at boot rather than what is left, because there is no syscall
/// that reports the remainder. The caller passes its own constant and the row says "(initial)".
pub fn write_holdings(budget_pages: u64, holdings: Holdings, out: &mut dyn FnMut(&[u8])) {
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
    out(b"    (no clock: 'date' spawned from here holds no clock capability, and says so)\n");
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
    expand: &mut dyn FnMut(&[u8]) -> Result<NameSet, Say>,
    out: &mut dyn FnMut(&[u8]),
) {
    let tail = grant_plan::trim(tail);
    if tail.is_empty() {
        return write_holdings(budget_pages, holdings, out);
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
    // `date`'s authority is a read-only mapping of the clock page, and it is **init's to endow, not
    // the shell's**: it is not designated on the line and no token could designate it. Saying so
    // here is the point of `caps` being the sole visibility surface. The day the shell is granted a
    // clock to delegate, this line becomes a cap row, and until then the preview must not let a
    // reader believe the command line is the whole story.
    if matches!(e.prog, Prog::Date) {
        out(b"    (clock: this shell holds none to delegate, so it will report the time\n");
        out(b"     as unknown. the clock is init's to endow; no token on the line can.)\n");
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
    if matches!(e.prog, Prog::Worker) {
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
            echo(b"  two  spaces ", &mut |_| Ok(NameSet::empty()), o);
        });
        assert_eq!(out, "  two  spaces ");
    }

    #[test]
    fn echo_prints_exactly_what_the_same_pattern_would_grant() {
        let set = listing(&[b"notes.txt", b"report.txt"]);
        let printed = shown(|o| {
            echo(b"*.txt", &mut |_| Ok(set), o);
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
        let said = echo(b"one  two", &mut |_| Ok(NameSet::empty()), &mut |b| {
            writes.push(b.to_vec());
        });
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
            Say::Refused(Refused::Absolute),
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
            reports: true,
            interruptible: false,
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
        // clock is init's to endow and no token could designate it.
        let s = shown(|o| write_preview(&endowment(Prog::Date), o));
        assert!(
            s.contains("clock: this shell holds none to delegate"),
            "{s}"
        );
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
                o,
            );
        });
        let without = shown(|o| write_holdings(128, Holdings::default(), o));
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

    // ---- caps over a whole line ----

    #[test]
    fn caps_with_no_tail_is_the_shells_own_endowment() {
        let tail = shown(|o| {
            write_caps(
                b"   ",
                128,
                Holdings::default(),
                &mut |_| Ok(NameSet::empty()),
                o,
            );
        });
        let direct = shown(|o| write_holdings(128, Holdings::default(), o));
        assert_eq!(tail, direct);
    }

    #[test]
    fn caps_previews_every_stage_of_a_pipeline() {
        let s = shown(|o| {
            write_caps(
                b"date | wc",
                128,
                Holdings::default(),
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
                &mut |_| Ok(NameSet::empty()),
                o,
            );
        });
        assert!(s.contains("caps previews a command's grant"), "{s}");
    }

    // ---- help ----

    #[test]
    fn help_names_every_verb_the_parser_accepts() {
        // The drift this catches is the cheap one to introduce and the expensive one to notice: a
        // builtin added to the parser and not to the help, which is then a feature nobody at the
        // prompt can find.
        let text = shown(write_help);
        for verb in ["help", "echo", "caps", "cd", "pwd", "ls", "mkdir"] {
            assert!(text.contains(verb), "help does not mention {verb}");
            // And it really is a verb this shell answers, rather than a word in a sentence.
            assert!(
                !matches!(grant_plan::parse(verb.as_bytes()), Command::Run(_)),
                "{verb} is not a builtin any more; the help is stale"
            );
        }
        for op in ["a | b", "a > name", "a >> name", "a < name"] {
            assert!(text.contains(op), "help does not mention {op}");
        }
    }
}
