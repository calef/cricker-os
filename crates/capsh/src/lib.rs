//! **capsh: the grant-expression logic of the capability shell** (milestone 31, phase 1).
//!
//! This crate is the pure logic behind the shell's one idea: on a cricker-os command line,
//! *designation is authorization* (Mark Miller's principle). Naming a resource in a command is
//! how you grant it; a program that names nothing gets nothing beyond the report channel every
//! spawn carries. There is no ambient authority to fall back on, so the failure when a program
//! needs something the command did not grant is legible ("you hold no such capability"), not a
//! Unix-flavored EPERM.
//!
//! It performs no IO and makes no syscalls. It turns a line of bytes into a decision: a
//! [`Command`], and for a `run` command an [`Endowment`] (exactly what to grant the child) or a
//! typed [`Refusal`] the shell prints at the prompt. That split is DECISIONS §7 applied, the same
//! shape as `lineedit`: the parsing and the manifest checking are host-tested in milliseconds, and
//! only the wiring (the shell and init that carry the caps) needs QEMU. See
//! notes/grant-expression.md and notes/program-manifest.md.
//!
//! # The three moving parts
//!
//! - [`parse`] tokenizes a command line into a [`Command`]. For `run`, it separates the program
//!   name, an optional integer argument, the `--mem N` budget grant, and forward-looking file
//!   designators (`file:PATH`), which phase 1 parses but cannot honor yet.
//! - A [`Manifest`] is a program's declared endowment: does it take an argument, does it require
//!   (or forbid) a memory grant, does it report back. [`Prog::manifest`] is the static table.
//! - [`plan`] checks a parsed `run` against the named program's manifest and yields an
//!   [`Endowment`] or a [`Refusal`]. A mismatch is caught here, at the prompt, before anything is
//!   spawned, which is milestone 23's component contract in embryo.
//!
//! # The wire half
//!
//! [`spawnproto`] is the word layout for the shell-to-init spawn protocol, the capability-shell
//! analogue of `lineedit::proto`. It is a userspace protocol (DECISIONS §21's shape): the kernel
//! routes the words and never reads them.

#![no_std]

pub mod jobframe;
pub mod spawnproto;

/// A program the shell can spawn. The set is small and closed in phase 1; each variant carries a
/// static [`Manifest`] and a stable wire id for [`spawnproto`].
///
/// This is deliberately an enum and not a string lookup at the grant boundary: the shell resolves
/// a typed program once, and everything downstream (the manifest check, the wire id init decodes)
/// speaks the type, not the name. A name that does not resolve is [`Refusal::NoSuchProgram`], the
/// "there is nothing there to name" shape of no-ambient-authority applied to programs themselves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prog {
    /// Squares its integer argument and reports the answer. Needs no memory grant.
    Worker,
    /// Spends a granted untyped budget: maps pages until the budget is exhausted and reports how
    /// many it got. The program that makes `--mem` *real* rather than parsed-and-ignored: the
    /// number it reports is the authority the command line handed it.
    Budgeter,
    /// A long-running job that *heeds* the cooperative interrupt: it works forever, polling its
    /// interrupt flag between work units, and on `^C` cleans up and exits (milestone 24). The
    /// cooperative tier made visible: the first `^C` stops it gracefully.
    Heeder,
    /// A runaway that ignores the interrupt entirely: a tight loop that never checks its flag. Only
    /// the forcible tier (the shell tearing its region down) ends it. The case the cooperative tier
    /// cannot reach, and the reason the second `^C` exists.
    Spinner,
}

impl Prog {
    /// Resolve a program by the name typed on the command line.
    pub fn from_name(name: &[u8]) -> Option<Prog> {
        match name {
            b"worker" => Some(Prog::Worker),
            b"budgeter" => Some(Prog::Budgeter),
            b"heeder" => Some(Prog::Heeder),
            b"spinner" => Some(Prog::Spinner),
            _ => None,
        }
    }

    /// The name init loads it by in the initrd (crickerfs), and the shell prints.
    pub fn name(self) -> &'static str {
        match self {
            Prog::Worker => "worker",
            Prog::Budgeter => "budgeter",
            Prog::Heeder => "heeder",
            Prog::Spinner => "spinner",
        }
    }

    /// The stable wire id the shell sends and init decodes ([`spawnproto`]).
    pub fn id(self) -> u64 {
        match self {
            Prog::Worker => 0,
            Prog::Budgeter => 1,
            Prog::Heeder => 2,
            Prog::Spinner => 3,
        }
    }

    /// The inverse of [`id`](Prog::id): init turns the wire id back into a program.
    pub fn from_id(id: u64) -> Option<Prog> {
        match id {
            0 => Some(Prog::Worker),
            1 => Some(Prog::Budgeter),
            2 => Some(Prog::Heeder),
            3 => Some(Prog::Spinner),
            _ => None,
        }
    }

    /// The program's declared endowment: what the shell must (and must not) grant it.
    pub fn manifest(self) -> Manifest {
        match self {
            Prog::Worker => Manifest {
                arg: ArgSpec::Required,
                mem: MemSpec::Forbidden,
                file: FileSpec::Forbidden,
                reports: true,
                // A worker finishes in one step; there is no long computation to interrupt, so it
                // is granted no interrupt channel. The shell waits for its result and no ^C tier
                // applies (milestone 24).
                interruptible: false,
            },
            Prog::Budgeter => Manifest {
                arg: ArgSpec::Forbidden,
                // A budget between 1 and 64 pages. The lower bound makes "budgeter with no --mem"
                // a refusal (it exists to spend memory); the upper bound is a sanity ceiling the
                // shell's own budget can actually back.
                mem: MemSpec::Required { min: 1, max: 64 },
                file: FileSpec::Forbidden,
                reports: true,
                interruptible: false,
            },
            // The two interrupt demonstrators. Both run until interrupted, take no argument and no
            // memory grant, and report through the shared job frame rather than the result endpoint
            // (so `reports` is false: they hold no result cap). `interruptible` is what makes the
            // shell wire the two-tier ^C path and hold the region for a forcible teardown.
            Prog::Heeder => Manifest {
                arg: ArgSpec::Forbidden,
                mem: MemSpec::Forbidden,
                file: FileSpec::Forbidden,
                reports: false,
                interruptible: true,
            },
            Prog::Spinner => Manifest {
                arg: ArgSpec::Forbidden,
                mem: MemSpec::Forbidden,
                file: FileSpec::Forbidden,
                reports: false,
                interruptible: true,
            },
        }
    }
}

/// A program's expectation about the integer argument (`run worker 9`'s `9`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgSpec {
    /// The program consumes an argument; omitting it is [`Refusal::ArgRequired`].
    Required,
    /// The program takes no argument; supplying one is [`Refusal::ArgForbidden`].
    Forbidden,
}

/// A program's expectation about a memory grant (`--mem N`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemSpec {
    /// The program takes no memory grant; supplying one is [`Refusal::MemForbidden`].
    Forbidden,
    /// The program must be granted a budget in `[min, max]` pages. Omitting it is
    /// [`Refusal::MemRequired`]; out of range is [`Refusal::MemOutOfRange`].
    Required { min: u64, max: u64 },
}

/// A program's expectation about a **file grant** (`file:PATH`), milestone 31 phase 2.
///
/// **The manifest declares the direction; the command line designates the file.** That split is the
/// SHILL shape and it is deliberate: a program knows whether it needs to write (that is a property of
/// what it does), while *which* file is the human's business and belongs on the command line. So
/// `run wc file:report.txt` reads and `run tee report.txt` writes, with no flag either way, and the
/// authority is still exactly what the line says because the program's half is fixed and published.
///
/// One file, not a list. A program that needs two files needs a manifest that says so, and that is a
/// later widening (`Required { count }`) rather than something to leave ambiguous now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileSpec {
    /// The program takes no file; naming one is [`Refusal::FileForbidden`].
    Forbidden,
    /// The program is granted exactly one file, `writable` or not. Omitting it is
    /// [`Refusal::FileRequired`].
    Required { writable: bool },
}

/// A program's SHILL-style manifest: the endowment it declares it expects. The shell checks the
/// command's grants against this at spawn, so a mismatch is a refusal at the prompt rather than a
/// mystery hang inside a program that did not get what it needed. See notes/program-manifest.md.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Manifest {
    pub arg: ArgSpec,
    pub mem: MemSpec,
    /// A per-file grant (`file:PATH`), milestone 31 phase 2. See [`FileSpec`] for why the direction
    /// lives here and the name lives on the command line.
    pub file: FileSpec,
    /// Endowed with the shared result endpoint (so it can report back). Every phase-1 program
    /// reports; the field exists so a program that does not can drop the channel it never uses.
    pub reports: bool,
    /// Granted a per-job interrupt channel so `^C` can reach it (milestone 24, DECISIONS §24). A
    /// long-running or interactive program declares this and the shell wires the two-tier interrupt
    /// path for it; a program that finishes in one step (worker) declares `false` and is simply
    /// waited on. "Granted by default to interactive programs" is expressed here, per program.
    pub interruptible: bool,
}

/// A parsed command line. The shell dispatches on this; only [`Command::Run`] carries a grant
/// expression that must be planned against a manifest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command<'a> {
    /// An empty line. Reprompt.
    Empty,
    /// `help`.
    Help,
    /// `echo <text>`: print the rest of the line verbatim.
    Echo(&'a [u8]),
    /// `caps`: print this shell's own endowment. `caps <run ...>` (a tail) previews what that
    /// command would grant; the tail is carried for the shell to re-parse.
    Caps(&'a [u8]),
    /// `run [--mem N] <prog> [arg] [file:PATH ...]`: the grant expression.
    Run(RunSpec<'a>),
    /// A first word that is not a known command.
    Unknown(&'a [u8]),
}

/// The parsed form of a `run` command, before the manifest check. [`plan`] turns it into an
/// [`Endowment`] or a [`Refusal`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RunSpec<'a> {
    /// The program name as typed (may not resolve).
    pub prog: &'a [u8],
    /// `--mem N`, if given.
    pub mem: Option<u64>,
    /// The single positional integer argument, if given.
    pub arg: Option<u64>,
    /// A file designator was present (`file:PATH`). Phase 1 records only that one appeared, so it
    /// can be refused with "you hold no such capability": the shell holds no directory capability
    /// until milestone 32's FS server lands. `Some(path)` is the first such path, for the message.
    pub file: Option<&'a [u8]>,
    /// A designator the parser did not understand (an unexpected extra token). Kept so the shell
    /// can refuse it rather than silently ignore authority the user thought they were granting.
    pub unexpected: Option<&'a [u8]>,
}

/// What a valid `run` resolves to: exactly the authority to hand the child, and nothing else.
/// Reading this is reading the whole endowment, which is §14's "one literal tells you a process's
/// authority" made concrete.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Endowment<'a> {
    pub prog: Prog,
    /// The integer argument to start it with (0 when the program takes none).
    pub arg: u64,
    /// Pages of untyped to split from the shell's own budget and grant (0 = none).
    pub mem_pages: u64,
    /// The one file to narrow a directory capability down to, and the direction, or `None`.
    /// Delivered as an endpoint served by a file warden (`user/src/fwarden.rs`), so what the child
    /// ends up holding designates this name and nothing else.
    pub file: Option<FileGrant<'a>>,
    /// Grant the shared result endpoint.
    pub reports: bool,
    /// Wire the two-tier `^C` path for this job (mirrors the manifest). When true the shell mints
    /// the per-job interrupt channel and runs the escalation policy while the job is foreground.
    pub interruptible: bool,
}

/// One resolved per-file grant: the name the command designated and the direction the program's
/// manifest declared. The pair is the whole authority: `fs_proto::grant` packs exactly this into the
/// file warden's three start arguments.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FileGrant<'a> {
    pub name: &'a [u8],
    pub writable: bool,
}

/// **What the shell itself holds**, which is what decides whether a designator can be backed at all.
///
/// This is why it is a parameter rather than a constant. "You hold no such capability" must be a
/// statement about the shell's actual cspace, not a hardcoded era: the same command line is a
/// refusal in a shell that was granted no directory and a real grant in one that was, and neither
/// the parser nor the manifest can tell them apart. Phase 1 hardcoded the refusal, which was true
/// then and would have quietly become a lie.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Holdings {
    /// The shell holds a directory capability it can narrow into a per-file grant.
    pub dir: bool,
}

/// Why a `run` was refused, decided at the prompt before any spawn. Each variant maps to one
/// legible line; [`Refusal::message`] is the fixed half, host-tested so the wording cannot drift.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The named program does not exist.
    NoSuchProgram,
    /// The command designated a resource this shell holds no capability for: a `file:PATH` in a
    /// shell that was never granted a directory to narrow. This is the refusal the milestone is
    /// about: not "permission denied" but "there is nothing you hold that could grant this."
    NoSuchCapability(CapKind),
    /// The program takes no file, but one was named. The milestone's inversion cuts both ways: a
    /// designator the program has no use for is authority the user did not mean to move, so it is
    /// refused rather than granted-and-ignored.
    FileForbidden,
    /// The program is endowed a file and the command named none. A `file:PATH` is the only way it
    /// can get one, so this is caught at the prompt rather than as an empty slot at runtime.
    FileRequired,
    /// The named file is not something this shell can express: empty, a path rather than a single
    /// component, or longer than the two argument words a grant's name rides in
    /// (`fs_proto::grant::MAX_NAME`).
    FileNotNameable,
    /// The program takes no memory grant, but `--mem` was given.
    MemForbidden,
    /// The program requires a memory grant, but none was given.
    MemRequired,
    /// `--mem N` was outside the program's declared range.
    MemOutOfRange { min: u64, max: u64 },
    /// The program requires an integer argument, but none was given.
    ArgRequired,
    /// The program takes no argument, but one was given.
    ArgForbidden,
    /// An extra token the parser could not place. Refused rather than ignored, so authority the
    /// user thought they were granting never silently evaporates.
    Unexpected,
}

/// The kind of capability a command designated but the shell cannot back. Phase 1 has one; the
/// enum exists so milestone 32 (files) and later device/endpoint grants slot in without changing
/// the refusal's shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CapKind {
    /// A file or directory, designated `file:PATH`. Arrives with milestone 32's FS server.
    File,
}

impl Refusal {
    /// The fixed, program-independent half of the refusal line. The shell prefixes the program
    /// name or the offending token where a variant needs it. The strings are the deliverable's
    /// voice: a refusal that reads like the capability model, not like errno.
    pub fn message(self) -> &'static str {
        match self {
            Refusal::NoSuchProgram => "no such program",
            Refusal::NoSuchCapability(CapKind::File) => {
                "you hold no such capability: this shell was granted no directory to narrow"
            }
            Refusal::FileForbidden => "takes no file; drop the file: designator",
            Refusal::FileRequired => "is granted one file; name it with file:<name>",
            Refusal::FileNotNameable => {
                "that is not a name this shell can grant: one component, at most 16 bytes"
            }
            Refusal::MemForbidden => "takes no memory grant; drop the --mem",
            Refusal::MemRequired => "needs a memory grant; add --mem <pages>",
            Refusal::MemOutOfRange { .. } => "memory grant is out of the range it declares",
            Refusal::ArgRequired => "needs an integer argument",
            Refusal::ArgForbidden => "takes no argument",
            Refusal::Unexpected => {
                "unexpected argument (this shell will not grant what it cannot name)"
            }
        }
    }
}

/// Split a command line into whitespace-separated tokens, writing them into `out` and returning
/// the filled prefix. A tiny `no_std` tokenizer: no allocation, bounded by `out.len()`. Tokens
/// past `out.len()` are dropped (a command line with more than a handful of tokens is a mistake,
/// not a workload).
pub fn tokenize<'a, 'b>(line: &'a [u8], out: &'b mut [&'a [u8]]) -> &'b [&'a [u8]] {
    let mut n = 0;
    let mut i = 0;
    while i < line.len() && n < out.len() {
        while i < line.len() && line[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < line.len() && !line[i].is_ascii_whitespace() {
            i += 1;
        }
        if i > start {
            out[n] = &line[start..i];
            n += 1;
        }
    }
    &out[..n]
}

/// Parse a whole command line into a [`Command`]. Pure and allocation-free.
///
/// The grammar is small: the first token selects the command. `echo` keeps the rest of the line
/// verbatim (so `echo   two  spaces` prints its spaces); everything else works on tokens.
pub fn parse(line: &[u8]) -> Command<'_> {
    let trimmed = trim(line);
    if trimmed.is_empty() {
        return Command::Empty;
    }
    let (first, rest) = split_first_word(trimmed);
    match first {
        b"help" => Command::Help,
        b"echo" => Command::Echo(rest),
        b"caps" => Command::Caps(rest),
        b"run" => Command::Run(parse_run(rest)),
        _ => Command::Unknown(first),
    }
}

/// Parse the tail of a `run` command (everything after `run`) into a [`RunSpec`]. Recognizes the
/// `--mem N` flag anywhere before the program name, then a program name, then at most one integer
/// argument and any number of `file:PATH` designators.
pub fn parse_run(tail: &[u8]) -> RunSpec<'_> {
    let mut toks: [&[u8]; 16] = [b""; 16];
    let toks = tokenize(tail, &mut toks);

    let mut mem = None;
    let mut prog: &[u8] = b"";
    let mut arg = None;
    let mut file = None;
    let mut unexpected = None;

    let mut i = 0;
    // Flags first (only --mem N today), then the program name.
    while i < toks.len() {
        let t = toks[i];
        if t == b"--mem" {
            // The next token is the page count. A missing or non-numeric value leaves mem None,
            // which the plan turns into MemRequired for a program that needs it.
            if i + 1 < toks.len() {
                mem = parse_u64(toks[i + 1]);
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        // First non-flag token is the program name; break to positional parsing.
        break;
    }
    if i < toks.len() {
        prog = toks[i];
        i += 1;
    }
    // Positional arguments after the program name.
    while i < toks.len() {
        let t = toks[i];
        if let Some(path) = strip_prefix(t, b"file:") {
            if file.is_none() {
                file = Some(path);
            }
        } else if let Some(v) = parse_u64(t) {
            if arg.is_none() {
                arg = Some(v);
            } else if unexpected.is_none() {
                unexpected = Some(t);
            }
        } else if unexpected.is_none() {
            unexpected = Some(t);
        }
        i += 1;
    }

    RunSpec {
        prog,
        mem,
        arg,
        file,
        unexpected,
    }
}

/// Check a parsed `run` against the program's manifest and produce the exact [`Endowment`] to
/// grant, or the [`Refusal`] to print. The whole authority decision lives here: after this returns
/// `Ok`, the shell grants precisely what the `Endowment` names and the child can reach nothing
/// else.
///
/// The order of checks is deliberate. A designated resource the shell cannot back
/// ([`Refusal::NoSuchCapability`]) is reported before manifest quibbles, because "you named
/// something I hold no capability for" is the milestone's headline refusal and should win over
/// "and also your --mem is out of range."
pub fn plan<'a>(run: &RunSpec<'a>, holds: Holdings) -> Result<Endowment<'a>, Refusal> {
    // A resource this shell cannot back AT ALL trumps everything: the command is asking for
    // authority nobody here holds, and no manifest detail changes that. Note that this is now a
    // question about `holds`, not about the calendar: the same line is a refusal in a shell granted
    // no directory and a real grant in one that was.
    if run.file.is_some() && !holds.dir {
        return Err(Refusal::NoSuchCapability(CapKind::File));
    }
    if run.unexpected.is_some() {
        return Err(Refusal::Unexpected);
    }

    let prog = Prog::from_name(run.prog).ok_or(Refusal::NoSuchProgram)?;
    plan_against(run, prog, prog.manifest())
}

/// [`plan`]'s second half, against an **explicit** manifest rather than the static table.
///
/// Split out for two reasons, and the first is immediate: it is the only way to exercise a manifest
/// shape no shipped program declares yet (a program endowed a file), so `FileSpec::Required` is live,
/// tested logic instead of a branch nothing reaches. The second is milestone 23, where a manifest
/// travels *with* a component rather than living in this table, and the composer checks a program it
/// did not write. That is the same call with a different source of the manifest.
pub fn plan_against<'a>(
    run: &RunSpec<'a>,
    prog: Prog,
    m: Manifest,
) -> Result<Endowment<'a>, Refusal> {
    // The file grant. Checked before the argument and the memory rules for the same reason the
    // un-backable case above wins: a designator that moves a *capability* is the milestone's
    // headline, and it should not be shadowed by "and also your --mem is out of range".
    let file = match (m.file, run.file) {
        (FileSpec::Forbidden, None) => None,
        (FileSpec::Forbidden, Some(_)) => return Err(Refusal::FileForbidden),
        (FileSpec::Required { .. }, None) => return Err(Refusal::FileRequired),
        (FileSpec::Required { writable }, Some(name)) => {
            if !file_name_fits(name) {
                return Err(Refusal::FileNotNameable);
            }
            Some(FileGrant { name, writable })
        }
    };

    // The argument.
    let arg = match (m.arg, run.arg) {
        (ArgSpec::Required, Some(v)) => v,
        (ArgSpec::Required, None) => return Err(Refusal::ArgRequired),
        (ArgSpec::Forbidden, None) => 0,
        (ArgSpec::Forbidden, Some(_)) => return Err(Refusal::ArgForbidden),
    };

    // The memory grant.
    let mem_pages = match (m.mem, run.mem) {
        (MemSpec::Forbidden, None) => 0,
        (MemSpec::Forbidden, Some(_)) => return Err(Refusal::MemForbidden),
        (MemSpec::Required { .. }, None) => return Err(Refusal::MemRequired),
        (MemSpec::Required { min, max }, Some(n)) => {
            if n < min || n > max {
                return Err(Refusal::MemOutOfRange { min, max });
            }
            n
        }
    };

    Ok(Endowment {
        prog,
        arg,
        mem_pages,
        file,
        reports: m.reports,
        interruptible: m.interruptible,
    })
}

/// The longest file name a per-file grant can carry. Duplicated from `fs_proto::grant::MAX_NAME`
/// rather than imported, because `capsh` is the shell's parser and must not depend on the filesystem
/// contract to check a command line; the pair is pinned by a test in each crate so a change to one
/// without the other fails on the host in milliseconds.
pub const MAX_FILE_NAME: usize = 16;

/// Whether a designated name can travel as a grant at all. A name is a single component, the same
/// rule the FS server enforces (DECISIONS §27): a path is not something this shell can express,
/// because there is no namespace here to walk.
pub fn file_name_fits(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILE_NAME
        && name != b"."
        && name != b".."
        && !name.contains(&b'/')
        && !name.contains(&b'\\')
}

// ---- small byte helpers (no_std, no alloc) ----

/// Trim ASCII whitespace from both ends.
pub fn trim(s: &[u8]) -> &[u8] {
    let mut a = 0;
    let mut b = s.len();
    while a < b && s[a].is_ascii_whitespace() {
        a += 1;
    }
    while b > a && s[b - 1].is_ascii_whitespace() {
        b -= 1;
    }
    &s[a..b]
}

/// Split off the first whitespace-delimited word; return `(word, rest)` where `rest` keeps its
/// internal spacing (so `echo` can print it verbatim).
fn split_first_word(s: &[u8]) -> (&[u8], &[u8]) {
    let mut i = 0;
    while i < s.len() && !s[i].is_ascii_whitespace() {
        i += 1;
    }
    let word = &s[..i];
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    (word, &s[i..])
}

/// `s` without `prefix`, or `None` if it does not start with it.
pub fn strip_prefix<'a>(s: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if s.len() >= prefix.len() && &s[..prefix.len()] == prefix {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

/// Parse a base-10 `u64`. `None` for empty or any non-digit byte, so `--mem twelve` is a missing
/// grant rather than a silent zero.
pub fn parse_u64(s: &[u8]) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let mut v: u64 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((b - b'0') as u64)?;
    }
    Some(v)
}

// ---- the two-tier interrupt escalation policy (milestone 24, DECISIONS §24) ----

/// What the shell should do this step of watching a foreground job for `^C`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Nothing this step.
    None,
    /// Deliver the cooperative interrupt: ask the job to stop itself (the first tier).
    Cooperative,
    /// Tear the job down: it did not stop when asked (the forcible tier).
    Forcible,
}

/// Poll ticks the shell waits for a cooperative exit after the first `^C` before it escalates on a
/// timeout. This is the "shell-side timeout" DECISIONS §24 left to the shell: a job that ignores the
/// cooperative signal is still torn down without needing a second keystroke. In ticks (the shell's
/// own watch loop iterations), not wall time, so it is deterministic and testable without a clock.
pub const COOP_GRACE_TICKS: u32 = 200;

/// The two-tier escalation policy, as a small state machine the shell drives while a foreground job
/// runs. It holds where `^C` routing lives (DECISIONS §24: in the shell, userspace, because job
/// control is the shell's knowledge). Pure logic, host-tested, so the counts and the grace window
/// are pinned without an emulator.
///
/// The shell feeds it two events: [`on_interrupt`](Escalation::on_interrupt) when it observes a
/// fresh `^C`, and [`on_tick`](Escalation::on_tick) each watch-loop iteration. The first `^C` asks
/// the job to stop ([`Action::Cooperative`]); a second `^C`, or the grace window elapsing with no
/// clean exit, tears it down ([`Action::Forcible`]). After a forcible decision the machine is spent
/// and returns [`Action::None`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Escalation {
    interrupts: u32,
    grace_left: u32,
    coop_sent: bool,
    done: bool,
}

impl Escalation {
    pub fn new() -> Self {
        Escalation {
            interrupts: 0,
            grace_left: 0,
            coop_sent: false,
            done: false,
        }
    }

    /// A fresh `^C` was observed. The first asks the job to stop and arms the grace window; a second
    /// (while the same job is still foreground) escalates to a forcible teardown.
    pub fn on_interrupt(&mut self) -> Action {
        if self.done {
            return Action::None;
        }
        self.interrupts += 1;
        if self.interrupts == 1 {
            self.coop_sent = true;
            self.grace_left = COOP_GRACE_TICKS;
            Action::Cooperative
        } else {
            self.done = true;
            Action::Forcible
        }
    }

    /// A watch-loop tick with no new `^C`. Once the cooperative signal is out, the grace window
    /// counts down; when it reaches zero the job is torn down even without a second `^C`, so a job
    /// that ignores the cooperative signal does not hang the prompt forever.
    pub fn on_tick(&mut self) -> Action {
        if self.done || !self.coop_sent {
            return Action::None;
        }
        self.grace_left = self.grace_left.saturating_sub(1);
        if self.grace_left == 0 {
            self.done = true;
            Action::Forcible
        } else {
            Action::None
        }
    }

    /// Whether the policy has reached a forcible teardown (the shell stops watching after this).
    pub fn spent(&self) -> bool {
        self.done
    }
}

impl Default for Escalation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_blank_lines_are_empty() {
        assert_eq!(parse(b""), Command::Empty);
        assert_eq!(parse(b"   "), Command::Empty);
        assert_eq!(parse(b"\t \n"), Command::Empty);
    }

    #[test]
    fn help_and_unknown() {
        assert_eq!(parse(b"help"), Command::Help);
        assert_eq!(parse(b"  help  "), Command::Help);
        assert_eq!(parse(b"frobnicate"), Command::Unknown(b"frobnicate"));
    }

    #[test]
    fn echo_keeps_internal_spacing() {
        assert_eq!(parse(b"echo hello world"), Command::Echo(b"hello world"));
        assert_eq!(parse(b"echo   two  spaces"), Command::Echo(b"two  spaces"));
        assert_eq!(parse(b"echo"), Command::Echo(b""));
    }

    #[test]
    fn run_worker_with_arg() {
        let Command::Run(r) = parse(b"run worker 9") else {
            panic!("not a run")
        };
        assert_eq!(r.prog, b"worker");
        assert_eq!(r.arg, Some(9));
        assert_eq!(r.mem, None);
        let e = plan(&r, Holdings::default()).unwrap();
        assert_eq!(e.prog, Prog::Worker);
        assert_eq!(e.arg, 9);
        assert_eq!(e.mem_pages, 0);
        assert!(e.reports);
    }

    #[test]
    fn worker_needs_an_argument() {
        let Command::Run(r) = parse(b"run worker") else {
            panic!()
        };
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::ArgRequired));
    }

    #[test]
    fn worker_refuses_a_memory_grant() {
        let Command::Run(r) = parse(b"run --mem 8 worker 3") else {
            panic!()
        };
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::MemForbidden));
    }

    #[test]
    fn budgeter_needs_a_memory_grant() {
        let Command::Run(r) = parse(b"run budgeter") else {
            panic!()
        };
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::MemRequired));
    }

    #[test]
    fn budgeter_with_mem_plans_the_grant() {
        let Command::Run(r) = parse(b"run --mem 16 budgeter") else {
            panic!()
        };
        let e = plan(&r, Holdings::default()).unwrap();
        assert_eq!(e.prog, Prog::Budgeter);
        assert_eq!(e.mem_pages, 16);
        assert_eq!(e.arg, 0);
    }

    #[test]
    fn budgeter_mem_out_of_range() {
        let Command::Run(r) = parse(b"run --mem 999 budgeter") else {
            panic!()
        };
        assert_eq!(
            plan(&r, Holdings::default()),
            Err(Refusal::MemOutOfRange { min: 1, max: 64 })
        );
        let Command::Run(r0) = parse(b"run --mem 0 budgeter") else {
            panic!()
        };
        assert_eq!(
            plan(&r0, Holdings::default()),
            Err(Refusal::MemOutOfRange { min: 1, max: 64 })
        );
    }

    #[test]
    fn budgeter_takes_no_argument() {
        let Command::Run(r) = parse(b"run --mem 8 budgeter 5") else {
            panic!()
        };
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::ArgForbidden));
    }

    #[test]
    fn unknown_program_is_refused_by_name() {
        let Command::Run(r) = parse(b"run frobnicate 1") else {
            panic!()
        };
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::NoSuchProgram));
    }

    #[test]
    fn a_file_designator_is_no_such_capability() {
        // The headline refusal, in the shell as it is actually endowed today: it holds no directory
        // capability, so there is nothing it could narrow, and the honest answer is about what it
        // holds rather than about a permission.
        let Command::Run(r) = parse(b"run worker 3 file:report.txt") else {
            panic!()
        };
        assert_eq!(r.file, Some(&b"report.txt"[..]));
        assert_eq!(
            plan(&r, Holdings::default()),
            Err(Refusal::NoSuchCapability(CapKind::File))
        );
        assert!(
            plan(&r, Holdings::default())
                .unwrap_err()
                .message()
                .contains("no such capability")
        );
    }

    #[test]
    fn file_refusal_beats_manifest_quibbles() {
        // Even though worker also has a missing arg and a forbidden --mem here, the un-grantable
        // resource wins: "you named something I cannot back" is the point.
        let Command::Run(r) = parse(b"run --mem 8 worker file:secret") else {
            panic!()
        };
        assert_eq!(
            plan(&r, Holdings::default()),
            Err(Refusal::NoSuchCapability(CapKind::File))
        );
    }

    /// A manifest no shipped program declares yet: one endowed a readable file. Checked through
    /// [`plan_against`] so the `FileSpec::Required` logic is live and tested rather than a branch
    /// nothing reaches, which is the same door milestone 23 will come through.
    const READS_A_FILE: Manifest = Manifest {
        arg: ArgSpec::Forbidden,
        mem: MemSpec::Forbidden,
        file: FileSpec::Required { writable: false },
        reports: true,
        interruptible: false,
    };

    /// The writable twin: a program that is endowed a file it may write.
    const WRITES_A_FILE: Manifest = Manifest {
        file: FileSpec::Required { writable: true },
        ..READS_A_FILE
    };

    /// A shell that WAS granted a directory to narrow.
    const WITH_DIR: Holdings = Holdings { dir: true };

    #[test]
    fn a_file_designator_plans_one_narrowed_grant() {
        // The headline: naming a file IS the grant, and the direction comes from the manifest, not
        // from a flag on the line. `run wc file:report.txt` reads; the same line against a writing
        // program writes; the human never types a mode.
        let Command::Run(r) = parse(b"run wc file:report.txt") else {
            panic!()
        };
        assert_eq!(r.file, Some(&b"report.txt"[..]));
        let e = plan_against(&r, Prog::Worker, READS_A_FILE).unwrap();
        let g = e.file.expect("the file designator did not become a grant");
        assert_eq!(g.name, b"report.txt");
        assert!(
            !g.writable,
            "the manifest declared a read, so the grant reads"
        );

        let e = plan_against(&r, Prog::Worker, WRITES_A_FILE).unwrap();
        assert!(
            e.file.unwrap().writable,
            "the same command line against a writing program grants a writable file",
        );
    }

    #[test]
    fn a_program_endowed_a_file_is_refused_when_the_command_names_none() {
        // The manifest's whole job: catch the mismatch at the prompt instead of letting the program
        // fault on an empty slot somewhere deep inside itself.
        let Command::Run(r) = parse(b"run wc") else {
            panic!()
        };
        assert_eq!(
            plan_against(&r, Prog::Worker, READS_A_FILE),
            Err(Refusal::FileRequired)
        );
    }

    #[test]
    fn a_file_named_at_a_program_that_takes_none_is_refused_not_ignored() {
        // The inversion cuts both ways. A designator the program has no use for is authority the
        // user thought they were moving, so it is refused rather than granted-and-dropped.
        let Command::Run(r) = parse(b"run worker 3 file:report.txt") else {
            panic!()
        };
        assert_eq!(
            plan_against(&r, Prog::Worker, Prog::Worker.manifest()),
            Err(Refusal::FileForbidden),
        );
    }

    #[test]
    fn a_name_that_cannot_travel_as_a_grant_is_refused_at_the_prompt() {
        // A grant's name rides in two argument words and is a single component, the same rule the FS
        // server enforces. A path is not something this shell can express: there is no namespace
        // here to walk, so it is refused where it was typed rather than turning into an ENOENT from
        // a server that was asked something meaningless.
        for line in [
            &b"run wc file:this-name-is-far-too-long.txt"[..],
            b"run wc file:sub/report.txt",
            b"run wc file:..",
        ] {
            let Command::Run(r) = parse(line) else {
                panic!()
            };
            assert_eq!(
                plan_against(&r, Prog::Worker, READS_A_FILE),
                Err(Refusal::FileNotNameable),
                "{}",
                core::str::from_utf8(line).unwrap(),
            );
        }
    }

    #[test]
    fn the_no_such_capability_refusal_is_about_what_the_shell_holds() {
        // Phase 1 hardcoded this refusal, which was true then and would have quietly become a lie.
        // The same command line must read as "you hold nothing that could grant this" in a shell
        // that was granted no directory, and as a real grant in one that was.
        let Command::Run(r) = parse(b"run wc file:report.txt") else {
            panic!()
        };
        assert_eq!(
            plan_against(&r, Prog::Worker, READS_A_FILE)
                .map(|e| e.file.map(|g| g.name))
                .unwrap(),
            Some(&b"report.txt"[..]),
            "with a directory in hand, the same line is a grant",
        );
        assert_eq!(
            plan(&r, Holdings::default()),
            Err(Refusal::NoSuchCapability(CapKind::File)),
            "with no directory in hand, it is the milestone's headline refusal",
        );
        // And the holdings only decide the un-backable case; they do not conjure a grant for a
        // program whose manifest takes no file.
        assert_eq!(plan(&r, WITH_DIR), Err(Refusal::NoSuchProgram));
    }

    #[test]
    fn a_grant_name_limit_matches_the_filesystem_contract() {
        // `capsh` deliberately does not depend on `fs_proto` (the shell's parser must not need the
        // filesystem contract to check a command line), so the constant is duplicated. That is only
        // safe if a change to one without the other fails here, on the host, in milliseconds.
        assert_eq!(MAX_FILE_NAME, fs_proto::grant::MAX_NAME);
        assert!(file_name_fits(b"sixteen-bytes!!!"));
        assert!(!file_name_fits(b"seventeen-bytes!!"));
        assert!(!file_name_fits(b""));
    }

    #[test]
    fn unexpected_token_is_refused_not_ignored() {
        let Command::Run(r) = parse(b"run worker 3 5") else {
            panic!()
        };
        assert_eq!(r.arg, Some(3));
        assert_eq!(r.unexpected, Some(&b"5"[..]));
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::Unexpected));
    }

    #[test]
    fn mem_flag_must_have_a_numeric_value() {
        // `--mem twelve` is a missing grant, not a silent zero: parse_u64 rejects it, so budgeter
        // sees "no --mem given" and refuses.
        let Command::Run(r) = parse(b"run --mem twelve budgeter") else {
            panic!()
        };
        assert_eq!(r.mem, None);
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::MemRequired));
    }

    #[test]
    fn caps_carries_its_tail() {
        assert_eq!(parse(b"caps"), Command::Caps(b""));
        assert_eq!(
            parse(b"caps run --mem 16 budgeter"),
            Command::Caps(b"run --mem 16 budgeter")
        );
    }

    #[test]
    fn prog_id_round_trips() {
        for p in [Prog::Worker, Prog::Budgeter] {
            assert_eq!(Prog::from_id(p.id()), Some(p));
        }
        assert_eq!(Prog::from_id(99), None);
    }

    #[test]
    fn parse_u64_rejects_junk() {
        assert_eq!(parse_u64(b"123"), Some(123));
        assert_eq!(parse_u64(b""), None);
        assert_eq!(parse_u64(b"1a"), None);
        assert_eq!(parse_u64(b"-1"), None);
    }

    #[test]
    fn worker_and_budgeter_are_not_interruptible() {
        // Fast jobs finish in one step; the shell just waits for them, no ^C tier.
        assert!(!Prog::Worker.manifest().interruptible);
        assert!(!Prog::Budgeter.manifest().interruptible);
        let Command::Run(r) = parse(b"run worker 9") else {
            panic!()
        };
        assert!(!plan(&r, Holdings::default()).unwrap().interruptible);
    }

    #[test]
    fn first_interrupt_is_cooperative_second_is_forcible() {
        let mut e = Escalation::new();
        assert_eq!(e.on_interrupt(), Action::Cooperative);
        assert!(!e.spent());
        assert_eq!(e.on_interrupt(), Action::Forcible);
        assert!(e.spent());
        // Spent: further events do nothing.
        assert_eq!(e.on_interrupt(), Action::None);
        assert_eq!(e.on_tick(), Action::None);
    }

    #[test]
    fn a_cooperative_signal_times_out_into_a_forcible_teardown() {
        // The "shell-side timeout": the first ^C asks nicely; if the job never exits, the grace
        // window elapsing tears it down without a second keystroke.
        let mut e = Escalation::new();
        assert_eq!(e.on_interrupt(), Action::Cooperative);
        // Ticks short of the window do nothing.
        for _ in 0..COOP_GRACE_TICKS - 1 {
            assert_eq!(e.on_tick(), Action::None);
        }
        // The last tick of the window escalates.
        assert_eq!(e.on_tick(), Action::Forcible);
        assert!(e.spent());
    }

    #[test]
    fn ticks_before_any_interrupt_do_nothing() {
        // No ^C yet: the grace window is not armed, so watching a well-behaved job never escalates.
        let mut e = Escalation::new();
        for _ in 0..COOP_GRACE_TICKS * 2 {
            assert_eq!(e.on_tick(), Action::None);
        }
        assert!(!e.spent());
        // And a first ^C after a long quiet run is still cooperative.
        assert_eq!(e.on_interrupt(), Action::Cooperative);
    }
}
