//! **`timetable`: a cron whose every entry is a grant** (milestone 129).
//!
//! Unix cron is ambient authority with a timestamp on it. A crontab line runs as a user and can do
//! whatever that user can do, so the crontab *is* the attack surface: the line `0 3 * * * rm -rf
//! /var/tmp/*` is dangerous not because of what it says but because of what the account behind it
//! holds. Nothing in the line bounds it, and nothing can, because on Unix a process's authority is
//! a property of who it runs as rather than of what it was handed.
//!
//! This crate inverts that. An entry is **a schedule plus a grant expression**, and the grant
//! expression is checked at registration by [`grant_plan::plan`], which is the same function the
//! shell checks a prompt line with. What comes out is an [`Endowment`]: the complete authority the
//! scheduled child will hold, computable and printable **before anything ever fires**. An entry
//! cannot name what its program's manifest does not declare, and it cannot be backed by authority
//! the scheduler itself was not given.
//!
//! # The four answers registration can give
//!
//! This is the demonstration, and it is the negative controls rather than the happy path. Unix cron
//! has one answer to every line (it runs, and whether it should have is not a question anything
//! asks); [`Registry::register`] has four, and three of them are refusals:
//!
//! | answer | what it means | example entry |
//! |---|---|---|
//! | [`Admission::Fires`] | planned, backed, and armed | `every 5s worker 7` |
//! | [`Admission::Refused`] | the program's own manifest refuses the line, exactly as at the prompt | `every 5s budgeter` (it exists to spend a budget and none was named) |
//! | [`Admission::Unbacked`] | the line is legal and **this scheduler holds nothing to back it** | `every 5s date` (a clock is a capability, and the scheduler has none) |
//! | [`Error`] | the document itself does not parse, with the line number | `every fortnight worker 7` |
//!
//! **[`Admission::Unbacked`] is the one worth reading twice.** `date` at a Unix cron line always
//! works, because reading the clock there is ambient. Here the wall clock is a capability
//! (DECISIONS §43), the scheduler was granted none, and so a scheduled `date` is refused *at
//! registration, in writing*, rather than firing five hundred times and printing 1970. The refusal
//! is a fact about the scheduler's own endowment, which is why it cannot be fixed by editing the
//! entry: it is fixed by granting the scheduler a clock, which is a decision somebody makes on
//! purpose at the spawn site.
//!
//! # What the document looks like
//!
//! ```text
//! # a comment
//! every 200ms  worker 7
//! at-boot      worker 3
//! ```
//!
//! Two schedule words, deliberately. `every <interval>` and `at-boot` are what milestone 129's
//! block scopes; calendar syntax (minute/hour/day fields and their daylight-saving ambiguities) is
//! its own later decision and not a default to drift into. See notes/scheduled-execution.md.
//!
//! # What this crate is not
//!
//! It performs no IO and makes no syscalls, which is CLAUDE.md rule 7 and the reason the four
//! answers above are host-tested in milliseconds rather than under QEMU. The program that holds the
//! budget, reads the counter and builds the children is `user/src/timetable.rs`; everything here is
//! the decision it makes, lifted out so it can be tested and Kani-reached.
//!
//! Name: provisional. `timetable` is a noun naming the thing this crate holds (a table of scheduled
//! entries) and avoids `scheduler`, which in this tree already means `kernel/src/sched.rs` and would
//! make two unrelated things share a word. Milestone 129's own block declines to propose a name and
//! says the eventual one is calef's (AGENTS.md); this is the placeholder, and it is expected to
//! change. Refused `cron` (the roadmap block calls it "a placeholder in the oldest tradition", and
//! the whole point of the milestone is that this is *not* cron's model), `almanac` (a book of
//! astronomical dates, which promises calendar scheduling this deliberately does not do), and
//! `metronome` (names the tick and says nothing about the grant, which is the half that matters).

#![no_std]

use grant_plan::expand::Expansion;
use grant_plan::{Endowment, Holdings, Refusal};

/// Nanoseconds in a second. Spelled here rather than taken from `clock_proto`, because this crate
/// decides *when* rather than *what time it is*: it never touches the wall clock, and depending on
/// the wall-clock contract to name a unit would claim otherwise.
pub const NANOS_PER_SEC: u64 = 1_000_000_000;

/// The most entries one document may carry.
///
/// A ceiling rather than a limit anybody meets, and past it the document is
/// [`Error::TooManyEntries`] rather than quietly short of an entry somebody wrote down. Eight is
/// what a fixed array costs nothing to hold; the reason there is an array at all is that this crate
/// allocates nothing, so a document borrows its own bytes and the table is inline.
pub const MAX_ENTRIES: usize = 8;

// ===============================================================================================
// The document.
// ===============================================================================================

/// **When an entry fires.** Two shapes, and the smallness is the decision rather than a stage.
///
/// Milestone 129's block scopes the vocabulary to exactly these because the housekeeping its first
/// customer needs (snapshot thinning, scrub passes, log rotation) is interval-shaped, and because
/// calendar syntax is a decision with real content in it: what a `0 2 * * *` entry should do when
/// the wall clock steps an hour is a question this system has vocabulary for (`ntp_proto`'s era
/// pivot) and no answer to yet. Adding a field to this enum later is cheap; shipping an
/// ambiguous one now is not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Schedule {
    /// Once, as soon as the scheduler is armed, and never again.
    AtBoot,
    /// Every interval, in **nanoseconds**, from the moment the scheduler is armed.
    ///
    /// Nanoseconds because that is what `user_rt::monotonic_nanos` reports and what the arithmetic
    /// in [`next_after`] runs in; the document is written in `ms`, `s` and `m`, and [`parse`]
    /// converts once, where the conversion can be tested.
    Every(u64),
}

/// One line of the document: when, and what.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry<'a> {
    /// The 1-based line it was written on, so every refusal can point at it.
    pub line: usize,
    /// When it fires.
    pub schedule: Schedule,
    /// **The grant expression, exactly as a person would type it at the prompt.** Bytes rather than
    /// a parsed structure, because the thing that parses it is `grant_plan` and this crate must not
    /// grow a second opinion about what a command line means.
    pub command: &'a [u8],
}

/// A parsed document: the entries, in the order written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Document<'a> {
    entries: [Entry<'a>; MAX_ENTRIES],
    n: usize,
}

/// Why a document does not parse. Every variant carries the **1-based line number** it went wrong
/// on, because a configuration error a person cannot find is a configuration error they will not
/// fix. `mdns_config` reached the same shape for the same reason.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// A line that is neither blank, a comment, nor a schedule word followed by a command.
    Malformed(usize),
    /// The first word is not `every` or `at-boot`.
    UnknownSchedule(usize),
    /// `every` with no interval after it.
    MissingInterval(usize),
    /// An interval that is not `<digits><unit>`, with the unit one of `ms`, `s`, `m`.
    BadInterval(usize),
    /// `every 0s`. A zero interval is not a fast schedule, it is a spin, and refusing it here is
    /// cheaper than discovering it as a hung machine.
    ZeroInterval(usize),
    /// A schedule word with nothing after it. There is nothing to run.
    MissingCommand(usize),
    /// More entries than [`MAX_ENTRIES`], reported at the line that overflowed rather than dropped.
    TooManyEntries(usize),
}

impl Error {
    /// The 1-based line this error is about.
    pub fn line(self) -> usize {
        match self {
            Error::Malformed(l)
            | Error::UnknownSchedule(l)
            | Error::MissingInterval(l)
            | Error::BadInterval(l)
            | Error::ZeroInterval(l)
            | Error::MissingCommand(l)
            | Error::TooManyEntries(l) => l,
        }
    }

    /// The fixed half of the sentence a reader gets. Host-tested, so the wording cannot drift.
    pub fn message(self) -> &'static str {
        match self {
            Error::Malformed(_) => "not a schedule and a command",
            Error::UnknownSchedule(_) => "the schedule must be `every <interval>` or `at-boot`",
            Error::MissingInterval(_) => "`every` needs an interval, like `every 30s`",
            Error::BadInterval(_) => "an interval is digits and one of ms, s, m",
            Error::ZeroInterval(_) => "a zero interval is a spin, not a schedule",
            Error::MissingCommand(_) => "a schedule with nothing to run",
            Error::TooManyEntries(_) => "more entries than this timetable holds",
        }
    }
}

/// Parse a timetable document.
///
/// Fails on the **first** problem, with its line, which is `mdns_config`'s posture and the right one
/// for a document a person edits: reporting six errors when the first one is a typo that shifted
/// everything after it is noise, and the second error is often the first one wearing a hat.
///
/// # Examples
///
/// ```
/// let doc = timetable::parse("# housekeeping\nevery 30s  worker 7\nat-boot  worker 3\n")
///     .expect("that document is well formed");
/// assert_eq!(doc.entries().len(), 2);
/// assert_eq!(doc.entries()[0].schedule, timetable::Schedule::Every(30 * timetable::NANOS_PER_SEC));
/// assert_eq!(doc.entries()[1].schedule, timetable::Schedule::AtBoot);
/// assert_eq!(doc.entries()[1].command, b"worker 3");
///
/// // A zero interval is refused rather than run, and the refusal names the line.
/// let bad = timetable::parse("every 0s worker 7\n").unwrap_err();
/// assert_eq!(bad.line(), 1);
/// assert_eq!(bad.message(), "a zero interval is a spin, not a schedule");
/// ```
pub fn parse(doc: &str) -> Result<Document<'_>, Error> {
    let mut entries = [Entry {
        line: 0,
        schedule: Schedule::AtBoot,
        command: b"",
    }; MAX_ENTRIES];
    let mut n = 0usize;

    for (i, raw) in doc.lines().enumerate() {
        let no = i + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let (word, rest) = split_word(line).ok_or(Error::Malformed(no))?;
        let (schedule, command) = match word {
            "at-boot" => (Schedule::AtBoot, rest),
            "every" => {
                let (interval, command) = split_word(rest).ok_or(Error::MissingInterval(no))?;
                let nanos = interval_nanos(interval).ok_or(Error::BadInterval(no))?;
                if nanos == 0 {
                    return Err(Error::ZeroInterval(no));
                }
                (Schedule::Every(nanos), command)
            }
            _ => return Err(Error::UnknownSchedule(no)),
        };
        let command = command.trim();
        if command.is_empty() {
            return Err(Error::MissingCommand(no));
        }
        if n == MAX_ENTRIES {
            return Err(Error::TooManyEntries(no));
        }
        entries[n] = Entry {
            line: no,
            schedule,
            command: command.as_bytes(),
        };
        n += 1;
    }
    Ok(Document { entries, n })
}

impl<'a> Document<'a> {
    /// The entries, in the order the document wrote them.
    pub fn entries(&self) -> &[Entry<'a>] {
        &self.entries[..self.n]
    }
}

/// Everything up to a `#`. No escape, because a schedule document has no use for a literal `#` and
/// an escape nobody needs is a rule everybody has to know.
fn strip_comment(line: &str) -> &str {
    match line.split_once('#') {
        Some((before, _)) => before,
        None => line,
    }
}

/// The first whitespace-delimited word and everything after it, or `None` for an empty string.
fn split_word(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    match s.find(char::is_whitespace) {
        Some(i) => Some((&s[..i], &s[i..])),
        None => Some((s, "")),
    }
}

/// `<digits><unit>` in nanoseconds, where the unit is `ms`, `s` or `m`.
///
/// Three units and no more. Anything shorter than a millisecond is below the resolution a
/// yield-polled scheduler can honour (see `user/src/timetable.rs`'s `BUGS`), and anything longer
/// than a minute wants the calendar syntax this deliberately does not have: `every 86400s` is a
/// daily job written in a way that is wrong across a leap second and silent about it.
fn interval_nanos(s: &str) -> Option<u64> {
    let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let value: u64 = s[..digits].parse().ok()?;
    let per = match &s[digits..] {
        "ms" => NANOS_PER_SEC / 1000,
        "s" => NANOS_PER_SEC,
        "m" => 60 * NANOS_PER_SEC,
        _ => return None,
    };
    value.checked_mul(per)
}

// ===============================================================================================
// Registration: the security boundary.
// ===============================================================================================

/// **What the scheduler itself holds**, which is what decides whether an entry can be backed at all.
///
/// This is `grant_plan::Holdings` one level out, and it is a parameter for exactly the same reason:
/// "this scheduler holds nothing to back that" must be a statement about a particular scheduler's
/// cspace rather than a hardcoded era. The same document is four refusals in a scheduler granted
/// nothing but a budget and four running jobs in one granted a clock, a directory and a terminal,
/// and neither the document nor the program manifests can tell them apart.
///
/// Every field is `false` or zero today, because the scheduler `user/src/timetable.rs` really does
/// hold nothing but memory and its own children's channels. Adding a capability to it is then a
/// visible edit here and a visible change in what the printed plan says, which is the property
/// worth having: **widening a scheduler is a decision somebody makes on purpose**, not something an
/// entry can talk it into.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Held {
    /// Pages of untyped this scheduler may split off for an entry's `--mem` grant. Zero means it
    /// can back no memory grant at all.
    pub mem_pages: u64,
    /// A read-only mapping of the clock page it could endow a child with (DECISIONS §43).
    pub clock: bool,
    /// A supervision endpoint it could hand a child as a process view (milestone 126).
    pub domain: bool,
    /// A directory capability it could narrow into a per-file or per-directory grant.
    pub dir: bool,
    /// A terminal whose `^C` could reach a child that declares itself interruptible (DECISIONS
    /// §24). A scheduled job has nobody at a keyboard, so this is the field least likely ever to be
    /// true, and it is here rather than assumed so that the refusal has a reason a reader can find.
    pub interrupt: bool,
}

/// **An authority the entry's plan needs and this scheduler does not hold.**
///
/// The distinction from [`Refusal`] is the whole point and it is not a shade of the same thing.
/// A `Refusal` is a fact about **the line**: `budgeter` with no `--mem` is wrong wherever it is
/// typed, and the fix is to edit the entry. An `Unbacked` is a fact about **the scheduler**: the
/// line is exactly right and would run at a prompt, and the fix is to grant the scheduler something
/// it was not granted. Collapsing the two would tell a person to edit a line that has nothing wrong
/// with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unbacked {
    /// The entry designates a file, and this scheduler holds no directory to narrow.
    File,
    /// The entry designates a directory, and this scheduler holds none.
    Directory,
    /// The program's manifest declares a clock, and this scheduler holds none.
    ///
    /// **The entry Unix cannot refuse.** `date` in a crontab always runs, because there the clock is
    /// ambient; here it is a capability, so a scheduler that was not granted one cannot endow one,
    /// and saying so at registration beats firing a program that can only print that it does not
    /// know what time it is.
    Clock,
    /// The program's manifest declares a process view, and this scheduler holds none to hand over.
    Domain,
    /// The program declares itself interruptible, and there is no terminal behind a scheduled job
    /// for a `^C` to come from.
    Interrupt,
    /// The entry's `--mem` grant is larger than this scheduler's whole budget. An entry cannot ask
    /// for more than the scheduler was given, and that ceiling is the scheduler's rather than the
    /// program manifest's, which is why the manifest's own `MemSpec` range did not catch it.
    Memory,
}

impl Unbacked {
    /// The fixed half of the sentence a reader gets, host-tested so the wording cannot drift.
    ///
    /// Every one of these reads as a statement about **this scheduler**, deliberately: "holds no
    /// clock" rather than "clock not permitted", because the second sounds like a policy somebody
    /// could relax and the first names the thing that would have to change.
    pub fn message(self) -> &'static str {
        match self {
            Unbacked::File => "this timetable holds no directory, so it cannot grant a file",
            Unbacked::Directory => "this timetable holds no directory to grant",
            Unbacked::Clock => "this timetable holds no clock, so it cannot grant one",
            Unbacked::Domain => "this timetable holds no process view to grant",
            Unbacked::Interrupt => "nobody is at a keyboard behind a scheduled job",
            Unbacked::Memory => "more memory than this timetable's whole budget",
        }
    }
}

/// What registration decided about one entry. See the table in the crate docs.
///
/// **The large variant is the point rather than a defect, so clippy is overruled here on purpose.**
/// `Endowment` is about a kilobyte, mostly the name set a directory grant can carry, and carrying it
/// whole is what makes "the plan *is* the authority" true instead of a summary of it. The usual fix
/// is a `Box`, and this crate has no allocator to box into: it is `no_std` with no `alloc`, by the
/// same rule that keeps it host-testable and Kani-reachable. So the alternative is not a smaller
/// enum, it is a `Row` that stores an outcome beside a separate endowment and lets the two disagree.
/// The size is paid for where it lands, which is a fixed inline table sized for exactly this
/// (`kernel/src/user/timetable_tests.rs` states the stack the program needs, and why).
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admission {
    /// Planned, backed, armed. The [`Endowment`] is the complete authority the child will hold, and
    /// printing it is what "`caps` before anything fires" means.
    Fires(Endowment),
    /// The program's own manifest refuses the line, exactly as it would at the prompt.
    Refused(Refusal),
    /// The line is legal and this scheduler holds nothing to back it.
    Unbacked(Unbacked),
}

/// One registered entry: what was written, what registration decided, and when it next fires.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Row<'a> {
    /// The document line this came from.
    pub entry: Entry<'a>,
    /// The decision.
    pub admission: Admission,
    /// The monotonic nanosecond reading at which this next fires. `u64::MAX` for anything that
    /// never will, which is every refusal and every [`Schedule::AtBoot`] that already went.
    next: u64,
}

impl<'a> Row<'a> {
    /// The endowment this row's child will be granted, or `None` if it never fires.
    pub fn endowment(&self) -> Option<Endowment> {
        match self.admission {
            Admission::Fires(e) => Some(e),
            _ => None,
        }
    }

    /// The monotonic reading at which this next fires, or `None` if it never will.
    pub fn next_fire(&self) -> Option<u64> {
        (self.next != NEVER).then_some(self.next)
    }
}

/// The "never fires again" deadline. `u64::MAX` rather than an `Option` inside the hot loop,
/// because [`Registry::due`] compares it against a reading every pass and a branch that is always
/// taken is cheaper to read than one that is sometimes there.
const NEVER: u64 = u64::MAX;

/// The registered timetable: every entry with its decision, and the firing state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Registry<'a> {
    rows: [Row<'a>; MAX_ENTRIES],
    n: usize,
}

impl<'a> Registry<'a> {
    /// **Check every entry against its program's manifest and against what this scheduler holds.**
    ///
    /// This is the security boundary milestone 129's block names. Whoever can put a line in this
    /// document can make that line's grants periodic and **nothing more**: the endowment is planned
    /// here, by `grant_plan`, out of the program's own published manifest, and an authority the
    /// scheduler does not hold cannot appear in it however the line is written.
    ///
    /// It fires nothing and touches no clock. [`arm`](Registry::arm) is a separate call so that a
    /// caller can print the whole plan, decide it is wrong, and never arm anything.
    ///
    /// # Examples
    ///
    /// ```
    /// # use timetable::{Admission, Held, Registry, Unbacked};
    /// let doc = timetable::parse(
    ///     "every 5s  worker 7\n\
    ///      every 5s  budgeter\n\
    ///      every 5s  date\n",
    /// ).unwrap();
    /// let reg = Registry::register(&doc, Held::default());
    ///
    /// // Planned, and the endowment is the whole of what the child will hold.
    /// let e = reg.rows()[0].endowment().expect("worker 7 is a legal line");
    /// assert_eq!(e.arg, 7);
    /// assert_eq!(e.mem_pages, 0);
    ///
    /// // The manifest refuses the line, exactly as the prompt would.
    /// assert!(matches!(reg.rows()[1].admission, Admission::Refused(_)));
    ///
    /// // And the line Unix cannot refuse: legal, and unbacked by this scheduler.
    /// assert_eq!(
    ///     reg.rows()[2].admission,
    ///     Admission::Unbacked(Unbacked::Clock),
    /// );
    /// ```
    pub fn register(doc: &Document<'a>, held: Held) -> Self {
        let mut rows = [Row {
            entry: Entry {
                line: 0,
                schedule: Schedule::AtBoot,
                command: b"",
            },
            admission: Admission::Unbacked(Unbacked::Clock),
            next: NEVER,
        }; MAX_ENTRIES];
        let mut n = 0usize;
        for &entry in doc.entries() {
            rows[n] = Row {
                entry,
                admission: admit(entry.command, held),
                next: NEVER,
            };
            n += 1;
        }
        Registry { rows, n }
    }

    /// The registered rows, in document order.
    pub fn rows(&self) -> &[Row<'a>] {
        &self.rows[..self.n]
    }

    /// How many entries will actually fire.
    pub fn admitted(&self) -> usize {
        self.rows()
            .iter()
            .filter(|r| r.endowment().is_some())
            .count()
    }

    /// **The programs this timetable will ever build**, as a bitmask over `Prog::id`.
    ///
    /// The point of it being computable at registration: reading the printed plan tells you the
    /// complete set of programs this process will ever start, and a Kani-free host test can hold
    /// that against the document. It is what makes the honest caveat in
    /// `user/src/timetable.rs`'s `BUGS` (the program is handed the whole initrd archive, which is
    /// wider than its plan) a *stated* residual rather than an unbounded one.
    pub fn programs(&self) -> u64 {
        let mut set = 0u64;
        for r in self.rows() {
            if let Some(e) = r.endowment() {
                set |= 1 << e.prog.id();
            }
        }
        set
    }

    /// Arm every admitted row against a monotonic reading. `at-boot` entries become due
    /// immediately; interval entries become due one interval later.
    ///
    /// **One interval later, not immediately**, and that is a decision rather than an
    /// implementation detail: `every 30s` said at a prompt means "thirty seconds from now", and a
    /// scheduler that also ran everything once at startup would make a boot the busiest moment the
    /// machine ever has. An entry that wants both says both, on two lines, which is legible.
    pub fn arm(&mut self, now: u64) {
        for r in self.rows.iter_mut().take(self.n) {
            if r.endowment().is_none() {
                continue;
            }
            r.next = match r.entry.schedule {
                Schedule::AtBoot => now,
                Schedule::Every(p) => now.saturating_add(p),
            };
        }
    }

    /// **Take one row that is due at `now`, advancing it past `now`.** `None` when nothing is due.
    ///
    /// Call it in a loop until it answers `None`: several entries can come due in one pass, and
    /// answering one at a time is what lets the caller fire each one (which takes real time, and
    /// can block waiting for memory) without holding a borrow of the whole table.
    ///
    /// The row it returns has **already been advanced**, so a caller that fails to build the child
    /// has missed that occurrence rather than retrying it forever. That is the same skip-rather-
    /// than-catch-up rule [`next_after`] states, applied to the failure path, and for the same
    /// reason: an entry that cannot be built now will not be buildable a microsecond from now, and
    /// a retry loop against a full budget is a spin with extra steps.
    pub fn due(&mut self, now: u64) -> Option<usize> {
        for i in 0..self.n {
            let r = &mut self.rows[i];
            if r.next > now {
                continue;
            }
            r.next = match r.entry.schedule {
                Schedule::AtBoot => NEVER,
                Schedule::Every(p) => next_after(r.next, p, now),
            };
            return Some(i);
        }
        None
    }

    /// The earliest reading at which anything fires, or `None` if nothing ever will.
    ///
    /// What a timed wait would block until, if this kernel had one. It does not (milestone 106 is
    /// `NOT-STARTED` and gated on a decision), so `user/src/timetable.rs` yield-polls instead and
    /// this is the number that says how long it will spin. Computing it anyway is deliberate: when
    /// the timed wait lands, the scheduler's loop changes by one line and this function is already
    /// the argument to it.
    pub fn next_deadline(&self) -> Option<u64> {
        self.rows().iter().filter_map(|r| r.next_fire()).min()
    }
}

/// Plan one entry's command against its program's manifest and against what the scheduler holds.
fn admit(command: &[u8], held: Held) -> Admission {
    let spec = grant_plan::parse_run(command);
    // `Holdings::default()` is a shell that holds no directory and stands at its root, which is
    // exactly this scheduler when `held.dir` is false. When it is true the position is still the
    // root, because a scheduler has no `cd`: there is no prompt to have typed one at, and a
    // working directory that could move between registration and a fire would make an already
    // planned grant mean something different later.
    let holdings = Holdings {
        dir: held.dir,
        ..Holdings::default()
    };
    match grant_plan::plan(&spec, holdings, Expansion::none()) {
        Err(r) => Admission::Refused(r),
        Ok(e) => match unbacked(&e, held) {
            Some(u) => Admission::Unbacked(u),
            None => Admission::Fires(e),
        },
    }
}

/// The first authority `e` needs that `held` does not have.
///
/// **Order is meaning here.** The designations a person typed come first (a file, a directory),
/// because those are the ones an editor can fix by changing the line. The manifest-declared
/// endowments come after, because those are facts about the program that no line can change and the
/// fix is at the scheduler's spawn site. A reader who gets `Clock` back knows not to go looking for
/// a typo.
fn unbacked(e: &Endowment, held: Held) -> Option<Unbacked> {
    if e.file.is_some() && !held.dir {
        return Some(Unbacked::File);
    }
    if e.dir.is_some() && !held.dir {
        return Some(Unbacked::Directory);
    }
    if e.mem_pages > held.mem_pages {
        return Some(Unbacked::Memory);
    }
    let m = e.prog.manifest();
    if m.clock && !held.clock {
        return Some(Unbacked::Clock);
    }
    if m.domain && !held.domain {
        return Some(Unbacked::Domain);
    }
    if m.interruptible && !held.interrupt {
        return Some(Unbacked::Interrupt);
    }
    None
}

// ===============================================================================================
// The arithmetic.
// ===============================================================================================

/// **The next fire after `now`, given the one that was scheduled at `prev` and a period.**
///
/// Three properties, and the third is the decision:
///
/// - it is **strictly after `now`**, so a fire never repeats within one pass of a polling loop;
/// - it keeps the **phase**: the result is congruent to `prev` modulo `period`, so an entry that
///   drifted late because the machine was busy comes back onto its original beat instead of
///   inheriting the delay forever;
/// - it **skips rather than catches up**. If the scheduler was away for an hour, an entry that
///   should have fired two hundred times fires **once** and then resumes. Vixie cron does the same
///   thing for the same reason, and the reason is worth writing down: catching up turns a stall
///   into a stampede, and a housekeeping job that runs two hundred times back to back on a machine
///   that was already struggling is how a slow morning becomes an outage. The cost is stated rather
///   than hidden: occurrences are genuinely lost, and an entry whose work must not be skipped
///   wants a durable queue rather than a scheduler.
///
/// `period == 0` answers [`u64::MAX`] (never), which no caller reaches because [`parse`] refuses a
/// zero interval; it is here so the function is total rather than a panic waiting for a caller that
/// did not read this line.
///
/// Three of those four are machine-checked; the phase one is host-tested, and `src/proofs.rs` says
/// why in the place a reader of the proofs would look for it.
///
/// # Examples
///
/// ```
/// # use timetable::next_after;
/// // The ordinary case: one period on.
/// assert_eq!(next_after(100, 10, 100), 110);
///
/// // Late by eight periods, and it does not fire eight times to make up.
/// assert_eq!(next_after(100, 10, 185), 190);
/// assert_eq!(next_after(100, 10, 190), 200);
///
/// // The phase survives the skip: every result is congruent to `prev` mod `period`.
/// assert_eq!(next_after(103, 10, 1_000) % 10, 3);
/// ```
pub const fn next_after(prev: u64, period: u64, now: u64) -> u64 {
    if period == 0 {
        return NEVER;
    }
    let first = prev.saturating_add(period);
    if first > now {
        return first;
    }
    // Past the first candidate, so snap `now` back onto the beat and add one period. Reaching the
    // answer this way rather than by counting the periods that went by (`first + (gap / period + 1)
    // * period`) is what makes it provable in seconds instead of hours: **there is no
    // multiplication here**, and a 64-by-64 multiplier is an enormous thing to hand a SAT solver.
    // The counting form was written first, and Kani was still grinding on one harness after ten
    // minutes; this form proves in well under one. Same answer, and the tests that pin the answer
    // did not change.
    //
    // `now >= first > prev`, so the subtraction cannot wrap, and `r < period`, so `now - r` cannot
    // either.
    let r = (now - prev) % period;
    (now - r).saturating_add(period)
}

// ===============================================================================================
// Rendering: what `caps` would have said, said before anything fires.
// ===============================================================================================

/// **Write the whole registered plan**: one block per entry, saying when it fires and the complete
/// authority its child will hold, or why it will not fire at all.
///
/// This is the milestone's claim in the form a person meets it. Milestone 31 made the shell able to
/// print a command's endowment **before** spawning anything (`caps worker 7`); this prints the same
/// decision for every entry in a schedule, at registration, before the first tick. On Unix there is
/// nothing to print: a crontab line's authority is its user's, which no tool can enumerate and no
/// line chose.
///
/// It is not `swish::write_preview`, and the reason is the prose rather than the logic. Every
/// sentence that renderer writes names *the shell* ("split from this shell's budget", "this shell's
/// result endpoint"), and a scheduler is a different principal with a different budget; reusing it
/// would print sentences that are false about the process doing the printing. What must not drift is
/// the **decision**, and that cannot, because both callers get theirs from `grant_plan::plan`.
///
/// # Examples
///
/// ```
/// # use timetable::{Held, Registry};
/// let doc = timetable::parse("every 30s worker 7\nevery 30s date\n").unwrap();
/// let reg = Registry::register(&doc, Held::default());
///
/// // A fixed buffer rather than a `String`, because this crate is `no_std` and the program that
/// // calls it writes straight down an endpoint.
/// let mut buf = [0u8; 1024];
/// let mut n = 0;
/// timetable::write_plan(&reg, &mut |b: &[u8]| {
///     buf[n..n + b.len()].copy_from_slice(b);
///     n += b.len();
/// });
/// let plan = core::str::from_utf8(&buf[..n]).unwrap();
/// assert!(plan.contains("every 30s     worker 7"));
/// assert!(plan.contains("this timetable holds no clock, so it cannot grant one"));
/// ```
pub fn write_plan(reg: &Registry<'_>, out: &mut dyn FnMut(&[u8])) {
    out(b"timetable: the plan, before anything fires\n");
    for r in reg.rows() {
        out(b"  ");
        write_schedule(r.entry.schedule, out);
        out(b"  ");
        out(r.entry.command);
        out(b"\n");
        match r.admission {
            Admission::Fires(e) => write_grant(&e, out),
            Admission::Refused(refusal) => {
                out(b"    will not fire: ");
                out(refusal.message().as_bytes());
                out(b"\n");
            }
            Admission::Unbacked(u) => {
                out(b"    will not fire: ");
                out(u.message().as_bytes());
                out(b"\n");
            }
        }
    }
}

/// How wide the schedule column is, so the commands line up under each other. A schedule longer
/// than this pushes its own command right rather than being truncated: a table that lied about a
/// number to keep its shape would be the wrong trade in a file about authority.
const SCHEDULE_COLUMN: usize = 12;

/// The schedule column, padded so the commands line up.
fn write_schedule(s: Schedule, out: &mut dyn FnMut(&[u8])) {
    let mut buf = [b' '; 32];
    let n = match s {
        Schedule::AtBoot => {
            buf[..7].copy_from_slice(b"at-boot");
            7
        }
        Schedule::Every(nanos) => {
            buf[..6].copy_from_slice(b"every ");
            6 + write_interval(nanos, &mut buf[6..])
        }
    };
    out(&buf[..n.max(SCHEDULE_COLUMN)]);
}

/// An interval back in the document's own units, so what is printed is what a person typed.
fn write_interval(nanos: u64, out: &mut [u8]) -> usize {
    let (value, unit): (u64, &[u8]) = if nanos.is_multiple_of(60 * NANOS_PER_SEC) {
        (nanos / (60 * NANOS_PER_SEC), b"m")
    } else if nanos.is_multiple_of(NANOS_PER_SEC) {
        (nanos / NANOS_PER_SEC, b"s")
    } else {
        (nanos / (NANOS_PER_SEC / 1000), b"ms")
    };
    let mut n = write_u64(value, out);
    out[n..n + unit.len()].copy_from_slice(unit);
    n += unit.len();
    n
}

/// Decimal digits into a buffer, returning how many.
fn write_u64(v: u64, out: &mut [u8]) -> usize {
    let mut digits = [0u8; 20];
    let mut n = 0;
    let mut v = v;
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
        if v == 0 {
            break;
        }
    }
    for i in 0..n {
        out[i] = digits[n - 1 - i];
    }
    n
}

/// **What the child will hold, and nothing else.** The `caps` half.
fn write_grant(e: &Endowment, out: &mut dyn FnMut(&[u8])) {
    out(b"    grants ");
    out(e.prog.name().as_bytes());
    out(b" exactly:\n");
    out(b"      cap 0  endpoint  report its answer to this timetable\n");
    if e.mem_pages > 0 {
        out(b"      cap 1  untyped   ");
        let mut buf = [0u8; 20];
        let n = write_u64(e.mem_pages, &mut buf);
        out(&buf[..n]);
        out(b" pages  split from this timetable's budget\n");
    }
    if e.arg != 0 {
        out(b"      arg      ");
        let mut buf = [0u8; 20];
        let n = write_u64(e.arg, &mut buf);
        out(&buf[..n]);
        out(b"\n");
    }
    // The line that is the whole milestone. It is not decoration: a reader of a crontab has no way
    // to write the equivalent sentence, because on Unix the answer is "whatever that user can do".
    out(b"      and nothing else: no clock, no disk, no console, no network\n");
}

// ===============================================================================================
// The archive endowment, audited against the plan.
// ===============================================================================================

/// **What the archive a timetable was handed carries, measured against what its plan will build.**
///
/// [`Registry::programs`] says which programs a document will ever start, and it says so before the
/// first tick. That number is only worth printing next to the other one: how many programs the
/// *capability* this process holds can actually reach. A scheduler handed the whole initrd has a
/// plan naming one program and an archive reaching fifty-seven, and nothing in the plan says so.
///
/// This is the sentence that says so, and it is here rather than in the program because it is a
/// comparison rather than a print: the program contributes the names it can see, this decides what
/// they mean, and a host test pins the wording.
///
/// **It measures, it does not enforce.** A wider archive is a fact about the spawn site, and a
/// scheduler cannot narrow its own endowment; what it can do is say out loud how wide it is. That
/// asymmetry is the same one [`Unbacked`] has: the fix is a decision somebody makes at the spawn
/// site, not an edit to the document.
///
/// # Examples
///
/// ```
/// # use timetable::{Audit, Held, Registry};
/// let doc = timetable::parse("every 30s worker 7\nevery 30s date\n").unwrap();
/// let reg = Registry::register(&doc, Held::default());
///
/// // Handed an archive holding exactly what the plan builds.
/// let mut narrow = Audit::of(&reg);
/// narrow.saw("worker");
/// assert!(narrow.is_exact());
/// assert_eq!(narrow.planned(), 1);
///
/// // Handed the whole initrd. `date` is refused, so the plan never names it, and holding an
/// // image of it is authority nothing in the document asked for.
/// let mut wide = Audit::of(&reg);
/// for name in ["worker", "date", "swish", "budgeter"] {
///     wide.saw(name);
/// }
/// assert!(!wide.is_exact());
/// assert_eq!(wide.beyond(), 3);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Audit {
    /// The plan's program set, as [`Registry::programs`] computes it.
    plan: u64,
    held: usize,
    beyond: usize,
}

impl Audit {
    /// Start an audit against `reg`'s plan, having seen nothing yet.
    pub fn of(reg: &Registry<'_>) -> Self {
        Audit {
            plan: reg.programs(),
            held: 0,
            beyond: 0,
        }
    }

    /// Record one name the endowed archive carries.
    ///
    /// A name this crate does not recognise as a program counts as **beyond the plan**, which is the
    /// safe direction: an archive holding something nobody can name is still an archive holding
    /// something, and a reader would rather be told the endowment is wide than be told nothing.
    pub fn saw(&mut self, name: &str) {
        self.held += 1;
        let planned = grant_plan::Prog::from_name(name.as_bytes())
            .is_some_and(|p| self.plan & (1 << p.id()) != 0);
        if !planned {
            self.beyond += 1;
        }
    }

    /// How many programs the archive carries.
    pub fn held(&self) -> usize {
        self.held
    }

    /// How many of them the plan will never build.
    pub fn beyond(&self) -> usize {
        self.beyond
    }

    /// How many distinct programs the plan will build.
    pub fn planned(&self) -> usize {
        self.plan.count_ones() as usize
    }

    /// Whether the archive reaches nothing beyond the plan.
    pub fn is_exact(&self) -> bool {
        self.beyond == 0
    }

    /// **The sentence a reader gets**, host-tested so the wording cannot drift.
    ///
    /// Two forms, and the second is the one that was true before the endowment was narrowed. Both
    /// are printed in the same place for the same reason the refusals are: an endowment nobody
    /// states is an endowment nobody checks.
    pub fn write(&self, out: &mut dyn FnMut(&[u8])) {
        let mut buf = [0u8; 20];
        if self.is_exact() {
            out(b"timetable: the archive it holds carries exactly the ");
            let n = write_u64(self.held as u64, &mut buf);
            out(&buf[..n]);
            out(if self.held == 1 {
                b" program its plan names\n"
            } else {
                b" programs its plan names\n"
            });
        } else {
            out(b"timetable: the archive it holds carries ");
            let n = write_u64(self.held as u64, &mut buf);
            out(&buf[..n]);
            out(b" programs, ");
            let n = write_u64(self.beyond as u64, &mut buf);
            out(&buf[..n]);
            out(b" of them beyond its plan\n");
        }
    }
}

// ===============================================================================================

#[cfg(test)]
mod tests {
    extern crate std;
    use std::string::String;

    use grant_plan::Prog;

    use super::*;

    /// **The shipped document is a specification, not a comment.** `mdns_config` reached this shape
    /// first and the reason is the same: a configuration file nothing parses in CI is a file that
    /// rots, and the first person to notice is the one whose machine will not boot.
    const REFERENCE: &str = include_str!("../../../user/timetable.conf");

    fn shown(f: impl FnOnce(&mut dyn FnMut(&[u8]))) -> String {
        let mut s = String::new();
        let mut out = |b: &[u8]| s.push_str(core::str::from_utf8(b).unwrap());
        f(&mut out);
        s
    }

    #[test]
    fn the_shipped_document_parses_and_says_what_it_claims() {
        let doc = parse(REFERENCE).expect("the shipped timetable must parse");
        let reg = Registry::register(&doc, Held::default());
        // The document is written to show every answer registration can give, so every variant
        // must actually appear in it. A document that drifted into being all-happy-path would
        // still pass a "it parses" test and would have stopped demonstrating anything.
        let mut fires = 0;
        let mut refused = 0;
        let mut unbacked = 0;
        for r in reg.rows() {
            match r.admission {
                Admission::Fires(_) => fires += 1,
                Admission::Refused(_) => refused += 1,
                Admission::Unbacked(_) => unbacked += 1,
            }
        }
        assert!(fires > 0, "the shipped document schedules nothing");
        assert!(
            refused > 0,
            "the shipped document shows no manifest refusal"
        );
        assert!(
            unbacked > 0,
            "the shipped document shows nothing this scheduler cannot back",
        );
    }

    #[test]
    fn comments_and_blank_lines_and_spacing_are_ignored() {
        let doc = parse(
            "# a heading\n\
             \n\
             every   30s     worker 7   # trailing\n\
             \n\
             at-boot worker 3\n",
        )
        .unwrap();
        assert_eq!(doc.entries().len(), 2);
        assert_eq!(doc.entries()[0].command, b"worker 7");
        assert_eq!(doc.entries()[0].line, 3);
        assert_eq!(doc.entries()[1].command, b"worker 3");
        assert_eq!(doc.entries()[1].line, 5);
    }

    #[test]
    fn an_empty_document_is_a_timetable_with_nothing_in_it() {
        let doc = parse("# nothing scheduled yet\n").unwrap();
        assert_eq!(doc.entries().len(), 0);
        let reg = Registry::register(&doc, Held::default());
        assert_eq!(reg.admitted(), 0);
        assert_eq!(reg.next_deadline(), None);
    }

    #[test]
    fn every_interval_unit_converts_once_and_correctly() {
        let doc = parse("every 250ms x\nevery 30s x\nevery 5m x\n").unwrap();
        assert_eq!(doc.entries()[0].schedule, Schedule::Every(250_000_000));
        assert_eq!(
            doc.entries()[1].schedule,
            Schedule::Every(30 * NANOS_PER_SEC)
        );
        assert_eq!(
            doc.entries()[2].schedule,
            Schedule::Every(300 * NANOS_PER_SEC)
        );
    }

    #[test]
    fn each_error_points_at_the_line_that_is_wrong() {
        let head = "# heading\nat-boot worker 3\n";
        for (bad, want) in [
            ("weekly worker 7\n", Error::UnknownSchedule(3)),
            ("every\n", Error::MissingInterval(3)),
            ("every fortnight worker 7\n", Error::BadInterval(3)),
            ("every 30 worker 7\n", Error::BadInterval(3)),
            ("every 30d worker 7\n", Error::BadInterval(3)),
            ("every 0s worker 7\n", Error::ZeroInterval(3)),
            ("every 0ms worker 7\n", Error::ZeroInterval(3)),
            ("every 30s\n", Error::MissingCommand(3)),
            ("at-boot\n", Error::MissingCommand(3)),
        ] {
            let mut doc = String::from(head);
            doc.push_str(bad);
            assert_eq!(parse(&doc), Err(want), "for {bad:?}");
        }
    }

    #[test]
    fn more_entries_than_the_table_holds_is_refused_rather_than_dropped() {
        let mut doc = String::new();
        for _ in 0..MAX_ENTRIES + 1 {
            doc.push_str("every 30s worker 7\n");
        }
        assert_eq!(parse(&doc), Err(Error::TooManyEntries(MAX_ENTRIES + 1)));
    }

    /// **The four answers, one document.** This is the negative-control test: the happy path alone
    /// would prove only that something can be scheduled, and what makes the milestone's claim is
    /// what gets turned away.
    #[test]
    fn registration_gives_four_distinguishable_answers() {
        let doc = parse(
            "every 5s worker 7\n\
             every 5s budgeter\n\
             every 5s date\n\
             every 5s wc\n\
             every 5s ps\n\
             every 5s heeder\n\
             every 5s nosuchprogram\n",
        )
        .unwrap();
        let reg = Registry::register(&doc, Held::default());
        let rows = reg.rows();

        // Fires, with the argument the line named and no memory it did not.
        let e = rows[0].endowment().unwrap();
        assert_eq!(e.prog, Prog::Worker);
        assert_eq!(e.arg, 7);
        assert_eq!(e.mem_pages, 0);

        // The manifest refuses the line, and it is the same refusal the prompt gives.
        assert_eq!(rows[1].admission, Admission::Refused(Refusal::MemRequired));

        // Legal lines this scheduler holds nothing to back. All four are entries a Unix cron would
        // simply have run.
        assert_eq!(rows[2].admission, Admission::Unbacked(Unbacked::Clock));
        // `wc` reads a stream, and `grant_plan` refuses that at the prompt already
        // (`InputSpec::Required` with nothing feeding it). It arrives here as the shell's own
        // refusal rather than as an `Unbacked`, which is the right shape: one authority decides
        // whether a line is runnable, and this crate only adds the question the shell cannot ask,
        // which is what the *scheduler* holds.
        assert_eq!(
            rows[3].admission,
            Admission::Refused(Refusal::InputRequired)
        );
        assert_eq!(rows[4].admission, Admission::Unbacked(Unbacked::Domain));
        assert_eq!(rows[5].admission, Admission::Unbacked(Unbacked::Interrupt));

        // And a name that resolves to nothing is a fact about the system, not about a program.
        assert_eq!(
            rows[6].admission,
            Admission::Refused(Refusal::NoSuchProgram)
        );

        assert_eq!(reg.admitted(), 1);
        assert_eq!(reg.programs(), 1 << Prog::Worker.id());
    }

    /// **Widening the scheduler changes the answer, and that is the point of [`Held`].**
    ///
    /// The same document, twice. Nothing about the entries changed; what changed is what the
    /// process doing the scheduling was granted. This is the property that makes `Unbacked` a
    /// different thing from `Refusal` rather than a shade of it.
    #[test]
    fn what_the_scheduler_holds_decides_what_it_can_schedule() {
        let doc = parse("every 5s date\nevery 5s budgeter --mem 4\n").unwrap();

        let bare = Registry::register(&doc, Held::default());
        assert_eq!(
            bare.rows()[0].admission,
            Admission::Unbacked(Unbacked::Clock)
        );
        assert_eq!(
            bare.rows()[1].admission,
            Admission::Unbacked(Unbacked::Memory),
        );

        let endowed = Registry::register(
            &doc,
            Held {
                clock: true,
                mem_pages: 64,
                ..Held::default()
            },
        );
        assert!(matches!(endowed.rows()[0].admission, Admission::Fires(_)));
        let e = endowed.rows()[1].endowment().unwrap();
        assert_eq!(e.prog, Prog::Budgeter);
        assert_eq!(e.mem_pages, 4);

        // And the ceiling is the scheduler's, not the manifest's: `budgeter` declares 1..=64 pages,
        // so 8 is a legal line and still more than a scheduler holding 4 can back.
        let doc = parse("every 5s budgeter --mem 8\n").unwrap();
        let tight = Registry::register(
            &doc,
            Held {
                mem_pages: 4,
                ..Held::default()
            },
        );
        assert_eq!(
            tight.rows()[0].admission,
            Admission::Unbacked(Unbacked::Memory),
        );
    }

    #[test]
    fn arming_puts_at_boot_first_and_an_interval_one_interval_out() {
        let doc = parse("every 30s worker 7\nat-boot worker 3\n").unwrap();
        let mut reg = Registry::register(&doc, Held::default());
        reg.arm(1_000);
        assert_eq!(reg.rows()[0].next_fire(), Some(1_000 + 30 * NANOS_PER_SEC));
        assert_eq!(reg.rows()[1].next_fire(), Some(1_000));
        assert_eq!(reg.next_deadline(), Some(1_000));
    }

    #[test]
    fn an_at_boot_entry_fires_once_and_never_again() {
        let doc = parse("at-boot worker 3\n").unwrap();
        let mut reg = Registry::register(&doc, Held::default());
        reg.arm(0);
        assert_eq!(reg.due(0), Some(0));
        assert_eq!(reg.due(0), None);
        assert_eq!(reg.due(u64::MAX - 1), None);
        assert_eq!(reg.next_deadline(), None);
    }

    #[test]
    fn a_refused_entry_is_never_due_however_long_you_wait() {
        let doc = parse("every 1ms date\nevery 1ms budgeter\n").unwrap();
        let mut reg = Registry::register(&doc, Held::default());
        reg.arm(0);
        for t in 0..1_000 {
            assert_eq!(reg.due(t * NANOS_PER_SEC), None, "at t={t}");
        }
        assert_eq!(reg.next_deadline(), None);
    }

    #[test]
    fn several_entries_can_come_due_in_one_pass() {
        let doc = parse("every 10ms worker 1\nevery 10ms worker 2\nat-boot worker 3\n").unwrap();
        let mut reg = Registry::register(&doc, Held::default());
        reg.arm(0);
        let now = 50 * NANOS_PER_SEC;
        let mut seen = [false; 3];
        while let Some(i) = reg.due(now) {
            assert!(!seen[i], "row {i} came due twice in one pass");
            seen[i] = true;
        }
        assert_eq!(seen, [true, true, true]);
    }

    /// **A stall does not become a stampede.** The scheduler is away for an hour; the entry that
    /// should have fired 360,000 times fires once.
    #[test]
    fn a_long_stall_produces_one_fire_and_not_a_backlog() {
        let doc = parse("every 10ms worker 7\n").unwrap();
        let mut reg = Registry::register(&doc, Held::default());
        reg.arm(0);
        let hour = 3_600 * NANOS_PER_SEC;
        assert_eq!(reg.due(hour), Some(0));
        assert_eq!(reg.due(hour), None, "it caught up instead of skipping");
        // And it is back on its original beat rather than carrying the delay forever.
        assert_eq!(reg.rows()[0].next_fire().unwrap() % 10_000_000, 0);
    }

    #[test]
    fn next_after_is_strictly_in_the_future_and_keeps_its_phase() {
        // The ordinary case.
        assert_eq!(next_after(100, 10, 100), 110);
        // Exactly on a boundary: strictly greater, so a polling loop cannot fire twice.
        assert_eq!(next_after(100, 10, 110), 120);
        // Late, by a whole number of periods and by a fraction of one.
        assert_eq!(next_after(100, 10, 185), 190);
        assert_eq!(next_after(100, 10, 190), 200);
        // The phase survives.
        for prev in [0u64, 3, 97, 1_000_003] {
            for period in [1u64, 7, 1_000, 999_983] {
                for now in [0u64, 1, 50, 1_000_000, u64::MAX / 4] {
                    let next = next_after(prev, period, now);
                    assert!(next > now, "{prev} {period} {now} -> {next}");
                    assert!(next >= prev.saturating_add(period));
                    assert_eq!(next % period, prev % period);
                    if now >= prev {
                        assert!(next <= now.saturating_add(period));
                    }
                }
            }
        }
        // Total rather than panicking, for a period `parse` will never produce.
        assert_eq!(next_after(1, 0, 5), u64::MAX);
    }

    #[test]
    fn the_plan_is_printable_before_anything_fires() {
        let doc = parse(
            "every 30s worker 7\n\
             at-boot budgeter --mem 4\n\
             every 1m date\n",
        )
        .unwrap();
        let reg = Registry::register(
            &doc,
            Held {
                mem_pages: 64,
                ..Held::default()
            },
        );
        let s = shown(|out| write_plan(&reg, out));

        // Every entry is named with its schedule in the units it was written in.
        assert!(s.contains("every 30s"), "{s}");
        assert!(s.contains("at-boot"), "{s}");
        assert!(s.contains("every 1m"), "{s}");
        // The grant, in full, for the ones that will fire.
        assert!(s.contains("grants worker exactly:"), "{s}");
        assert!(
            s.contains("4 pages  split from this timetable's budget"),
            "{s}"
        );
        assert!(
            s.contains("and nothing else: no clock, no disk, no console, no network"),
            "{s}",
        );
        // And the reason, in full, for the one that will not.
        assert!(
            s.contains("will not fire: this timetable holds no clock, so it cannot grant one"),
            "{s}",
        );
    }

    /// Every `Unbacked` has a distinct sentence. Two variants sharing wording would make the
    /// difference between "fix the line" and "fix the scheduler" invisible to a reader.
    #[test]
    fn every_refusal_reads_differently() {
        let all = [
            Unbacked::File,
            Unbacked::Directory,
            Unbacked::Clock,
            Unbacked::Domain,
            Unbacked::Interrupt,
            Unbacked::Memory,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.message(), b.message(), "{a:?} and {b:?} read the same");
            }
        }
        let errs = [
            Error::Malformed(1),
            Error::UnknownSchedule(1),
            Error::MissingInterval(1),
            Error::BadInterval(1),
            Error::ZeroInterval(1),
            Error::MissingCommand(1),
            Error::TooManyEntries(1),
        ];
        for (i, a) in errs.iter().enumerate() {
            for b in &errs[i + 1..] {
                assert_ne!(a.message(), b.message(), "{a:?} and {b:?} read the same");
            }
        }
    }

    /// **The endowment a spawn site hands over, measured against the plan it can compute.**
    ///
    /// The whole point of [`Registry::programs`] being computable at registration is that the spawn
    /// site can narrow what it hands over to exactly that set. This is the check that says whether
    /// it did, and the two sentences are what a reader meets on the console: one for an archive that
    /// reaches nothing beyond the plan, one for the whole initrd.
    ///
    /// **This test is also the mechanism behind a written list in another file**, which is why the
    /// first assertion names `worker` rather than counting to one.
    /// `kernel/src/user/timetable_tests.rs` builds the narrowed archive from a `PLANNED_PROGRAMS`
    /// list it does not compute, because computing it there means a `Registry` on a kernel stack and
    /// a `Registry` is 21632 bytes against a 4096-byte guard page (`script/stack-frame-check`). So
    /// the plan is computed **here**, on the host, in milliseconds, and a document edited without
    /// that list being edited fails right here. Change the shipped document's admitted set and this
    /// assertion is the first thing that goes red; keep them in step and nothing else has to
    /// remember.
    #[test]
    fn the_archive_a_timetable_holds_is_measured_against_what_it_will_build() {
        let doc = parse(REFERENCE).expect("the shipped document parses");
        let reg = Registry::register(&doc, Held::default());

        // The shipped document admits `worker` twice and refuses everything else, so its plan is
        // one program however many entries name it. Asserted **by name**, because a spawn site in
        // another crate carries this same set as a written list and this is what keeps it true.
        assert_eq!(
            reg.programs(),
            1 << Prog::Worker.id(),
            "the shipped document's plan changed; \
             kernel/src/user/timetable_tests.rs's PLANNED_PROGRAMS must change with it",
        );
        assert_eq!(Audit::of(&reg).planned(), 1);

        // Narrowed to the plan: what the spawn site hands over after milestone 129's second
        // stratum. `worker` and nothing else.
        let mut narrow = Audit::of(&reg);
        narrow.saw("worker");
        assert!(narrow.is_exact());
        assert_eq!(narrow.held(), 1);
        assert_eq!(narrow.beyond(), 0);
        assert_eq!(
            shown(|out| narrow.write(out)),
            "timetable: the archive it holds carries exactly the 1 program its plan names\n",
        );

        // The whole initrd: what it used to be handed. Every program the plan refused is still an
        // image this process could load, which is authority no line in the document asked for.
        // `date` is the sharpest case, because the document names it and registration refused it.
        let mut wide = Audit::of(&reg);
        for name in ["worker", "date", "ps", "budgeter", "wc", "swish"] {
            wide.saw(name);
        }
        assert!(!wide.is_exact());
        assert_eq!(wide.held(), 6);
        assert_eq!(wide.beyond(), 5);
        assert_eq!(
            shown(|out| wide.write(out)),
            "timetable: the archive it holds carries 6 programs, 5 of them beyond its plan\n",
        );

        // A name this tree does not know is still authority, and counts against the plan rather
        // than being waved through as unrecognised.
        let mut stranger = Audit::of(&reg);
        stranger.saw("not_a_program_in_this_tree");
        assert_eq!(stranger.beyond(), 1);
    }

    /// **Widening the scheduler widens the plan, and the audit follows it.** The same pairing
    /// [`Held`] has everywhere else: what counts as authority beyond the plan is a fact about a
    /// particular scheduler, not a constant.
    #[test]
    fn a_wider_scheduler_plans_more_programs_and_the_audit_agrees() {
        let doc = parse("every 1s worker 7\nevery 1s date\n").expect("well formed");

        let bare = Registry::register(&doc, Held::default());
        let mut a = Audit::of(&bare);
        a.saw("worker");
        a.saw("date");
        assert_eq!(
            a.beyond(),
            1,
            "a refused `date` is an image nothing will run"
        );

        let with_clock = Registry::register(
            &doc,
            Held {
                clock: true,
                ..Held::default()
            },
        );
        let mut b = Audit::of(&with_clock);
        b.saw("worker");
        b.saw("date");
        assert!(
            b.is_exact(),
            "granting the scheduler a clock admits `date`, so its image is in the plan",
        );
        assert_eq!(b.planned(), 2);
    }
}

// Machine-checked proofs (Kani). Behind `#[cfg(kani)]`, so an ordinary build or test never sees
// them; `script/verify` runs them.
#[cfg(kani)]
mod proofs;
