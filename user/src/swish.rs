//! The capability shell, at EL0: designation is authorization (milestone 31, phase 1).
//!
//! Milestone 10 gave this program a command line; milestone 31 gives that command line a meaning it
//! does not have on Unix. A Unix child inherits your whole authority and then `open()`s whatever its
//! uid allows. Here the command line is a **grant expression**: naming a resource in a command is
//! how you grant it, and a program that names nothing gets nothing beyond the report channel every
//! spawn carries. There is no ambient authority to fall back on, so when a program needs something
//! the command did not grant, the refusal reads "you hold no such capability", not a Unix-flavored
//! EPERM. The parsing and the manifest checking are the host-tested `grant_plan` crate; this file is the
//! wiring that turns a checked grant into real, delegated capabilities.
//!
//! What the shell can grant, it grants from what it *holds*. The headline is `prog --mem N`, which
//! endows a program N pages of untyped **split from the shell's own budget** (slot 3) and delegated
//! to it. A bare name in a file position is the second, and whether it can be backed is a statement
//! about this shell's cspace rather than about the calendar: a shell whose init granted it a
//! directory (slot [`DIR_TERMINAL`]) resolves the name, and one granted none says "you hold no such
//! capability". Milestone 50 made the first case real at the interactive prompt; see [`holdings`],
//! notes/pipes.md and notes/grant-expression.md. `caps` prints the shell's whole endowment, and
//! `caps <command>` previews exactly what that command would grant, making DECISIONS §14's "reading
//! one literal tells you a process's whole authority" interactively true.
//!
//! **The clock is the one authority in a preview that no token designates** (milestone 51's wiring).
//! `date` declares it in its manifest and *init* endows it, read-only; `caps date` prints it anyway,
//! because a preview that showed only what the line designates would be off by exactly one
//! capability.
//!
//! **Since milestone 86 this shell holds a clock of its own**, at the slot init names in `x2`, and it
//! is `READ` **without** `GRANT`. That one bit is the whole design of `time <command>`: the shell can
//! read the wall clock to measure a command, and it cannot hand a clock to anything it spawns, so the
//! set of processes that can read the time is still decided by the manifests init reads (DECISIONS
//! §43). Timing is something an observer does with its own authority; the thing being timed needs
//! nothing and is not told. See notes/time-command.md.
//!
//! # The grammar lost two words in milestone 47
//!
//! There is no `run` verb: a bare program name spawns it, so builtins and programs are typed the
//! same way and nobody has to know which class a command is in. And there is no `file:` prefix: a
//! bare token in a file position designates the file, because the direction (the half that matters)
//! was always the manifest's to declare and the prefix only marked the half already on the screen.
//! `caps` is the visibility surface that remains, which is an argument for keeping it good.
//!
//! # The shell's world
//!
//! It holds, by convention (init granted them in this order):
//!
//! - slot 0: the terminal endpoint (CALL: `OP_WRITE` / `OP_READLINE`).
//! - slot 1: a spawn endpoint (direct init to start a program; `grant_plan::spawnproto`).
//! - slot 2: a result endpoint (receive a spawned program's answer).
//! - slot 3: an untyped budget, the memory it grants with `--mem`.
//!
//! and two pages shared with the terminal: `OUT_VA` (we write text and prompts) and `LINE_VA`
//! (completed lines arrive). No role selector; the syscall runtime comes from `user_rt`.
//!
//! Two more slots are **wiring-dependent** and their numbers are therefore not constants here: the
//! directory ([`DIR_TERMINAL`], slot 4, when this boot has a filesystem) and the clock page, whose
//! slot arrives in `x2` because it sits after the filesystem pair and a boot with no disk has one
//! fewer capability under it. See [`CLOCK_SLOT`].
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `shell`. Refused `shell` (a category
//! rather than a name: `bash`, `zsh`, `fish` and `rc` are identities), `capsh` (Linux's libcap
//! ships `capsh(1)`, a capability shell wrapper, so a reader from Linux would assume ours is that
//! tool) and `sheesh` (it carries a 2020-21 timestamp where `bash` and `fish` are era-neutral, and
//! it is an interjection of exasperation, while refusing things is this shell's most characteristic
//! behaviour by design). A swish is the shot that goes through the net touching nothing, which is
//! least authority in one word.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use fs_proto::{dirent, fs};
use grant_plan::expand::{Expander, NameSet, Resume};
use grant_plan::line::{self, Line, Source};
use grant_plan::nav::{self, Cwd, Refused, Step};
use grant_plan::{
    Action, Command, DirGrant as GrantDir, Endowment, Escalation, Refusal, RunSpec, Streams,
    jobframe, spawnproto,
};
use line_editor::proto;
use swish::{Route, Say, Status, Untimed, sequence};
use user_rt::{call, cap_delete, exit, invoke, monotonic_nanos, recv, send, yield_now};

// Pages shared with the terminal (must match the wiring in init).
//
// `OUT_VA` used to be 0x0060_0000, which is also [`FS_VA`]. That was sound while a shell wired to a
// terminal could not also hold a filesystem, and milestone 50's `>` is exactly the wiring where it
// holds both, so the terminal page moved rather than the FS one: `FS_VA` is `fs_service`'s
// `FILE_VA_CLIENT`, which six other programs map, and this is the one address only the shell and its
// init know.
const OUT_VA: u64 = 0x0000_0000_00c0_0000; // we write; the terminal reads
const LINE_VA: u64 = 0x0000_0000_00b0_0000; // the terminal writes; we read

// Capability slots.
const TERM: u64 = 0; // CALL requests on the terminal
const SPAWN: u64 = 1; // SEND a spawn request to init
const RESULT: u64 = 2; // RECV a spawned program's answer
const BUDGET: u64 = 3; // our own untyped; SPLIT a grant off it for `--mem`

/// The budget init granted us at boot (must match `crates/system_initializer`'s `SH_BUDGET_PAGES`).
/// We cannot query how much remains (there is no such syscall), so `caps` prints the initial grant.
const SH_BUDGET_PAGES: u64 = 128;

// ---- the shell's own clock (milestone 86, notes/time-command.md) ----

/// **The slot holding the clock page**, or [`NO_CLOCK`] when this wiring granted none. Told to us in
/// `x2` at [`_start`] rather than fixed, because the clock is granted after the filesystem pair and a
/// boot with no disk attached has one fewer capability under it.
///
/// Being *told* is the same shape as `arg1` carrying the directory's rights, and for the same reason
/// recorded there: nothing in this system reports what a process holds, so a shell that guessed would
/// be guessing about its own cspace. A wrong guess here is worse than the directory's, because
/// probing the wrong slot would find some *other* object and map a page that is not a clock.
static CLOCK_SLOT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(NO_CLOCK);

/// The `x2` value meaning "this shell was granted no clock". Zero rather than a sentinel, because
/// slot 0 is the terminal in every wiring, so no clock can ever legitimately be there.
const NO_CLOCK: u64 = 0;

/// **What this shell holds, which is what decides whether a named file can be backed.**
///
/// A per-file grant is a directory capability narrowed to one name (milestone 31 phase 2,
/// `user/src/fs_file_caretaker.rs`), and a redirection is a file opened through one. Both need a
/// directory to narrow, so both answer to this: a shell whose init granted it a directory
/// capability (slot [`DIR_TERMINAL`]) can back `>` and `<`, and a shell whose init granted it none
/// gets the milestone's headline refusal, which is **true** rather than a placeholder.
///
/// The same binary is in both positions, which is the point: `date > report.txt` is refused in the
/// wiring with no filesystem and runs in the wiring with one, and nothing in this file branches on
/// which. See notes/pipes.md.
fn holdings(nav: &Nav) -> grant_plan::Holdings {
    grant_plan::Holdings {
        dir: nav.dir.is_some(),
        cwd: nav.cwd,
    }
}

// ---- the navigation builtins (milestone 47) ----

/// Where the page this shell shares with the FS server is mapped, **in the wiring that has one**.
///
/// It is `fs_service`'s `FILE_VA_CLIENT`, the address every FS client in this system maps its half
/// of the contract at, which is why [`OUT_VA`] moved out of the way rather than this.
const FS_VA: u64 = 0x0000_0000_0060_0000;

/// The directory capability's slot in the navigating wiring (`fs_service::start_granted_dir` grants
/// it at 0). A shell that was granted none has no such slot at all, which is why [`Nav::dir`] is an
/// `Option` rather than a constant: "this shell holds no directory" is a fact about a cspace.
const DIR: u64 = 0;

/// The directory capability's slot in the **terminal** wiring, where slots 0..3 are already the
/// terminal, the spawn channel, the result channel and the budget. A shell wired this way holds a
/// filesystem *and* a way to spawn, which is the whole of what `>` and `<` needed (notes/pipes.md).
const DIR_TERMINAL: u64 = 4;

/// **The shell's position inside the one directory capability it holds**, and the capabilities it
/// walked through to get there.
///
/// A working directory, in capability terms, is a directory capability used as the default base for
/// resolving names. This is that, made concrete: [`Nav::cwd`] is the position as a value (what `pwd`
/// prints, and what a grant records at plan time), and [`Nav::handles`] is the stack of directory
/// capabilities that back it, one per level. **`..` is a pop of that stack**, which is why it cannot
/// climb out: at the root there is nothing to pop, and no request is sent for the FS server to have
/// to refuse. Chroot's shape, arrived at from the other direction.
struct Nav {
    /// The slot holding the directory capability, or `None` for a shell that was granted none.
    dir: Option<u64>,
    /// **The rights the root capability carries**, as this shell was told at spawn.
    ///
    /// It is told rather than asking, and that is a gap in the contract rather than a shortcut:
    /// `fs_proto` has no verb that reports what a handle carries. It matters because `OPENDIR`
    /// refuses (`EPERM`) when the intersection is smaller than the request, so a shell that asked
    /// for `dir::ALL` from a narrower capability could not `cd` at all. See notes/shell-navigation.md.
    rights: u64,
    /// Where we are, as a value.
    cwd: Cwd,
    /// `handles[i]` is the directory capability for level `i + 1`; level 0 is [`fs::ROOT`], the
    /// capability the endpoint itself designates.
    handles: [u64; nav::MAX_DEPTH],
    /// The sweep in progress, when the line is under `xargs` (milestone 109). Zeroed outside one.
    batching: Batching,
}

/// **The state of a batched sweep**, which lives on [`Nav`] because the expander is the one place a
/// pattern becomes a set and a batch is a *kind of expansion* rather than a second mechanism.
///
/// Putting it here is what makes every command path inherit batching for free: `echo`, an
/// invocation, a `caps` preview and a pipeline stage all reach a pattern through [`Nav::expand`],
/// so none of them needs to know that a sweep is running.
#[derive(Clone, Copy, Default)]
struct Batching {
    /// Where this batch resumes, or `None` outside a sweep.
    ///
    /// **Cleared once a pattern has been expanded**, so only the *first* pattern on a line is
    /// batched. That is not a new limitation: only the first pattern on a line is expanded at all
    /// (notes/glob-grant.md), because no manifest declares a second name slot.
    resume: Option<Resume>,
    /// Which batch this is, counting from one, for the header printed before it runs.
    index: u64,
    /// Where the batch after this one resumes, or `None` when this was the last.
    next: Option<Resume>,
    /// How many names this batch designated, which is the authority that moved.
    names: u64,
}

/// **What the command now running has done so far** (milestone 67), as a [`Status`] code.
///
/// A global for the reason [`DIAG_EP`] is one: the refusal and outcome printers are free functions
/// reached from every command path, and threading a `&mut Nav` through all of them to carry one
/// small fact would be a wide diff. Single-threaded EL0, so `Relaxed` is the whole ordering story.
///
/// This was an `AtomicBool` called `TROUBLE` until milestone 67, set by whichever printer had bad
/// news, and read by `xargs` to stop a sweep. Widening it from a bit to a status is the whole of
/// what `$?` needed: the shell already knew that something had gone wrong, and what it did not
/// record was **which kind**.
static CURRENT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// **What the command before this one did**, which is what `$?` reports and what `&&` reads.
///
/// Two cells rather than one, because a segment has to be able to read the previous segment's
/// answer *while* accumulating its own: `false || echo $?` prints the status of the thing that
/// failed, which is exactly the case one cell could not serve.
static LAST: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Record what just happened, **first one wins**: the first thing that went wrong is what stopped
/// the line, and a later printer describing a consequence should not overwrite the cause.
fn record(s: Status) {
    if CURRENT.load(core::sync::atomic::Ordering::Relaxed) == Status::Ran.code() {
        CURRENT.store(s.code(), core::sync::atomic::Ordering::Relaxed);
    }
}

/// **This shell declined to run it**, decided from what this shell holds with nothing spawned.
fn refused() {
    record(Status::Refused);
}

/// **Something was attempted and did not work**: a filesystem errno, a spawn init could not back,
/// a job torn down. See [`Status`] for why this is a different number from [`refused`].
fn failed() {
    record(Status::Failed);
}

/// What the command now running has done so far.
fn current() -> Status {
    Status::from_code(CURRENT.load(core::sync::atomic::Ordering::Relaxed))
}

/// What the command before this one did.
fn last() -> Status {
    Status::from_code(LAST.load(core::sync::atomic::Ordering::Relaxed))
}

/// A resolved path lead: the directory handle it designates, plus the temporary capabilities opened
/// to reach it, which the caller either adopts (`cd`) or closes.
struct Walk {
    /// The directory the lead designates.
    handle: u64,
    /// How far up the shell's own stack the lead started, after any `..`s.
    base: usize,
    /// The capabilities opened on the way down, in order.
    tmp: [u64; nav::MAX_DEPTH],
    n: usize,
}

impl Nav {
    /// A shell holding nothing that names a filesystem. Everything below then answers
    /// [`Say::NoDirectory`], which is a statement about this shell's cspace and not a placeholder.
    fn empty() -> Self {
        Nav {
            dir: None,
            rights: 0,
            cwd: Cwd::root(),
            handles: [0; nav::MAX_DEPTH],
            batching: Batching::default(),
        }
    }

    /// A shell rooted at the directory capability in [`DIR`], carrying `rights`.
    fn rooted(rights: u64) -> Self {
        Nav::rooted_at(DIR, rights)
    }

    /// A shell rooted at the directory capability in `slot`, carrying `rights`. The slot is a
    /// parameter because the two wirings put the directory in different places: a witness holds
    /// nothing else and gets slot 0, and a shell with a terminal already owes 0..3 to it.
    fn rooted_at(slot: u64, rights: u64) -> Self {
        Nav {
            dir: Some(slot),
            rights,
            cwd: Cwd::root(),
            handles: [0; nav::MAX_DEPTH],
            batching: Batching::default(),
        }
    }

    /// The handle for the level we are standing on.
    fn here(&self) -> u64 {
        self.at(self.cwd.depth())
    }

    /// The handle for `level` levels below the root.
    fn at(&self, level: usize) -> u64 {
        match level.checked_sub(1) {
            None => fs::ROOT,
            Some(i) => self.handles[i],
        }
    }

    /// One request that names something: stage the name in the shared page and call.
    fn name_call(&self, verb: u64, handle: u64, name: &[u8], w1: u64) -> i64 {
        put_page(name);
        call(
            self.dir.unwrap_or(DIR),
            fs::req(verb, handle, name.len() as u64),
            w1,
        )
        .0 as i64
    }

    /// Close a handle we opened. A failure here is not reportable and not recoverable: the reply is
    /// dropped deliberately rather than turned into a refusal for something the user did not ask.
    fn close(&self, handle: u64) {
        call(self.dir.unwrap_or(DIR), fs::req(fs::CLOSE, handle, 0), 0);
    }

    /// **Resolve a path without moving**, from this shell's root when the token began with `/` and
    /// from where it stands otherwise.
    ///
    /// `..` is answered from the shell's own stack (a level up is a handle it already holds), and
    /// each `Down` is one `OPENDIR`, because the FS contract takes a single component per request
    /// and nothing here walks a path. The steps are validated by [`Cwd::apply`] *before* this runs,
    /// so an `Up` past the root and a path deeper than the shell tracks are already refused with
    /// nothing sent; the only failure left is the server's.
    fn walk(&mut self, p: &nav::Path<'_>) -> Result<Walk, Say> {
        self.walk_from(p.from_root(), p.steps())
    }

    /// [`Nav::walk`] with the lead of a path rather than the whole of it: the same walk, stopping
    /// one component short, which is what every verb that acts on a *name* needs.
    ///
    /// `from_root` is carried separately because a lead is a bare step sequence and cannot say
    /// where it started. **A walk from the root starts at [`fs::ROOT`]**, the capability the
    /// endpoint itself designates, which is the only root this process has and the reason an
    /// absolute path can reach nothing a `cd` could not.
    fn walk_from(&mut self, from_root: bool, steps: &[Step<'_>]) -> Result<Walk, Say> {
        let mut w = if from_root {
            Walk {
                handle: fs::ROOT,
                base: 0,
                tmp: [0; nav::MAX_DEPTH],
                n: 0,
            }
        } else {
            Walk {
                handle: self.here(),
                base: self.cwd.depth(),
                tmp: [0; nav::MAX_DEPTH],
                n: 0,
            }
        };
        for step in steps {
            match step {
                Step::Up => {
                    if w.n > 0 {
                        w.n -= 1;
                        self.close(w.tmp[w.n]);
                    } else if w.base > 0 {
                        w.base -= 1;
                    } else {
                        self.unwind(&w);
                        return Err(Say::Refused(Refused::AtYourRoot));
                    }
                    w.handle = if w.n > 0 {
                        w.tmp[w.n - 1]
                    } else {
                        self.at(w.base)
                    };
                }
                Step::Down(name) => {
                    let r = self.name_call(fs::OPENDIR, w.handle, name, self.rights);
                    if r < 0 {
                        self.unwind(&w);
                        return Err(Say::Failed(-r as i32));
                    }
                    w.tmp[w.n] = r as u64;
                    w.n += 1;
                    w.handle = r as u64;
                }
            }
        }
        Ok(w)
    }

    /// Give back every capability a walk opened. Called on failure, and by every verb that resolved
    /// a path only to act on it: a handle nobody closes pins a node in the server for the rest of
    /// the boot, exactly as a leaked fd does.
    fn unwind(&self, w: &Walk) {
        for i in 0..w.n {
            self.close(w.tmp[i]);
        }
    }

    /// Parse and validate a path operand: the steps, and where they would leave us.
    fn plan_path<'a>(&self, token: &'a [u8]) -> Result<(nav::Path<'a>, Cwd), Say> {
        let p = nav::path(token).map_err(Say::Refused)?;
        let target = self.cwd.resolve(&p).map_err(Say::Refused)?;
        Ok((p, target))
    }

    /// **`cd`**: rebind where names resolve. An empty operand is your root, because there is no
    /// `HOME` here and the root is the one distinguished place you have: it is what you were
    /// granted. `cd /` is the same place said out loud, which is what `pwd` has always printed.
    ///
    /// The move is all or nothing. A `cd a/b` that fails at `b` leaves the shell in the directory it
    /// started in, because the next command would otherwise act somewhere the user does not think
    /// they are.
    fn cd(&mut self, token: &[u8]) -> Say {
        if self.dir.is_none() {
            return Say::NoDirectory;
        }
        if token.is_empty() {
            return self.go_home();
        }
        let (p, target) = match self.plan_path(token) {
            Ok(v) => v,
            Err(s) => return s,
        };
        let w = match self.walk(&p) {
            Ok(w) => w,
            Err(s) => return s,
        };
        // Adopt the walk: the capabilities we walked out of are no longer reachable from where we
        // now stand, so they are given back, and the ones we opened become the new stack.
        for level in w.base..self.cwd.depth() {
            self.close(self.handles[level]);
        }
        for i in 0..w.n {
            self.handles[w.base + i] = w.tmp[i];
        }
        self.cwd = target;
        Say::Nothing
    }

    /// Back to the root: close everything we descended through and forget it.
    fn go_home(&mut self) -> Say {
        for level in 0..self.cwd.depth() {
            self.close(self.handles[level]);
        }
        while self.cwd.ascend() {}
        Say::Nothing
    }

    /// **`ls`**: enumerate a directory, the cwd by default. One `READDIR` per page of entries, with
    /// the cursor advanced by what was decoded, so a directory larger than the local buffer is read
    /// in rounds rather than truncated.
    ///
    /// `each` is called with every entry: the interactive prompt prints them, and the navigating
    /// witness checks them. A listing is a rendering of authority, so what is in it is a claim worth
    /// checking rather than just printing.
    fn ls(&mut self, token: &[u8], each: &mut dyn FnMut(&[u8], bool)) -> Say {
        if self.dir.is_none() {
            return Say::NoDirectory;
        }
        let (handle, walked) = if token.is_empty() {
            (self.here(), None)
        } else {
            let (p, _) = match self.plan_path(token) {
                Ok(v) => v,
                Err(s) => return s,
            };
            match self.walk(&p) {
                Ok(w) => (w.handle, Some(w)),
                Err(s) => return s,
            }
        };

        let said = self.each_entry(handle, each);
        if let Some(w) = walked {
            self.unwind(&w);
        }
        said
    }

    /// **`mkdir`**: make a directory and, in the same verb, obtain a capability to it. It needs
    /// `CREATE` **and** `DESCEND`, because a directory you could not have walked into would be a way
    /// to mint a capability out of a right that was withheld.
    ///
    /// The capability it mints is given straight back. `mkdir` makes a directory; `cd` is how you go
    /// there, and a shell that silently moved you would be doing two things under one word.
    fn mkdir(&mut self, token: &[u8]) -> Say {
        self.act(token, |nav, handle, name| {
            let r = nav.name_call(fs::MKDIR, handle, name, nav.rights);
            if r < 0 {
                Say::Failed(-r as i32)
            } else {
                nav.close(r as u64);
                Say::Nothing
            }
        })
    }

    /// The shape `mkdir` has, and the witness's removals share: resolve everything but the last
    /// component, act on that component in the directory it named, and give back whatever the
    /// resolution opened.
    fn act(&mut self, token: &[u8], f: impl Fn(&mut Nav, u64, &[u8]) -> Say) -> Say {
        if self.dir.is_none() {
            return Say::NoDirectory;
        }
        if token.is_empty() {
            return Say::NeedsAName;
        }
        let (p, _) = match self.plan_path(token) {
            Ok(v) => v,
            Err(s) => return s,
        };
        let Some((lead, name)) = p.split_last_component() else {
            // The token ends in `..`, so it designates a directory rather than a name in one.
            return Say::Refused(Refused::NotAName);
        };
        let w = match self.walk_from(p.from_root(), lead) {
            Ok(w) => w,
            Err(s) => return s,
        };
        let said = f(self, w.handle, name);
        self.unwind(&w);
        said
    }
}

// ---- expansion: the one place a pattern becomes a set (milestone 47's globbing lane) ----

impl Nav {
    /// **Expand one pattern token against the directory its leading path names.**
    ///
    /// This is the only function in the shell that turns a pattern into names, and both callers go
    /// through it: `echo` (which prints the set) and the grant planner (which grants it). That is
    /// what makes "the expansion you see is the grant" a property of the code rather than a promise
    /// two call sites are trusted to keep.
    ///
    /// It needs `ENUMERATE` on the directory and nothing more, which is the rung the ladder already
    /// separates out (DECISIONS §47): expanding a pattern is listing a directory, so it costs the
    /// authority to list a directory. A shell that may not enumerate gets the server's `EPERM` here
    /// rather than a quietly empty set, which would be `§42`'s silent degradation exactly.
    fn expand(&mut self, token: &[u8]) -> Result<NameSet, Say> {
        if self.dir.is_none() {
            return Err(Say::NoDirectory);
        }
        let (p, _) = self.plan_path(token)?;
        let Some((lead, pattern)) = p.split_last_component() else {
            return Err(Say::Refused(Refused::NotAName));
        };
        let w = self.walk_from(p.from_root(), lead)?;

        // **The batched and unbatched expanders decide membership identically**, which is why the
        // sweep is a policy on this one function rather than a second path: `xargs rm *.txt` and
        // `rm *.txt` cannot disagree about what the pattern designates, only about how much of it
        // one invocation is handed.
        let resume = self.batching.resume;
        let mut e = match resume {
            Some(r) => Expander::batch(pattern, r),
            None => Expander::new(pattern),
        };
        let said = self.each_entry(w.handle, &mut |name, is_dir| e.offer(name, is_dir));
        self.unwind(&w);
        if said != Say::Nothing {
            return Err(said);
        }
        let Some(_) = resume else {
            return e.finish().map_err(Say::Cannot);
        };
        let batch = e.finish_batch().map_err(Say::Cannot)?;
        // Only the first pattern of a line is batched, and only the first is expanded at all.
        self.batching.resume = None;
        self.batching.next = batch.next();
        self.batching.names = batch.names.len() as u64;
        // **Printed before anything runs, once per batch**, which is the per-batch form of the
        // property the globbing lane exists for: what is shown is what *this* invocation is handed.
        // The union is never shown because the union is exactly what nobody can hold.
        swish::write_batch(self.batching.index, &batch.names, &mut print);
        Ok(batch.names)
    }

    /// Read a directory in rounds, calling `each` with every entry. The body `ls` had, lifted out so
    /// the listing loop is written once: a listing is a rendering of authority whether it is being
    /// printed or matched against.
    fn each_entry(&mut self, handle: u64, each: &mut dyn FnMut(&[u8], bool)) -> Say {
        let mut cursor = 0u64;
        let mut buf = [0u8; LISTING];
        // Bounded so a server whose cursor does not advance costs a short listing rather than a
        // prompt that never comes back.
        for _ in 0..ROUNDS {
            let n = call(
                self.dir.unwrap_or(DIR),
                fs::req(fs::READDIR, handle, 0),
                cursor,
            )
            .0 as i64;
            if n < 0 {
                return Say::Failed(-n as i32);
            }
            if n == 0 {
                break;
            }
            let n = (n as usize).min(buf.len());
            get_page(n, &mut buf);
            let mut seen = 0u64;
            for (name, is_dir) in dirent::iter(&buf[..n]) {
                each(name, is_dir);
                seen += 1;
            }
            if seen == 0 {
                break;
            }
            cursor += seen;
        }
        Say::Nothing
    }
}

// `is_pattern`, `expansion`, `write_set` and `echo` moved to the `swish` crate in milestone 70:
// each is a decision or a rendering with no capability in it, and each is now host-tested there.
// What is left below is the shell's IO: reading a directory, and putting bytes on a terminal.
//
// The two wrappers here are the seam. The crate's versions take the directory read as a callback,
// because matching a pattern is pure and only *reading* the directory needs a capability; these
// bind that callback to this shell's [`Nav`] so every call site reads exactly as it did before.

/// [`swish::expansion`], against the directory this shell holds.
fn expansion(nav: &mut Nav, spec: &RunSpec) -> Result<grant_plan::expand::Expansion, Say> {
    swish::expansion(spec, &mut |token| nav.expand(token))
}

/// [`swish::echo`], against the directory this shell holds.
/// `$?` reads [`LAST`], which is the segment *before* this one: during `false || echo $?` the
/// shell is running the second segment and the first one's answer is what the word is about.
fn echo(nav: &mut Nav, text: &[u8], out: &mut dyn FnMut(&[u8])) -> Say {
    swish::echo(text, last(), &mut |token| nav.expand(token), out)
}

/// The local buffer one `READDIR` round is decoded from. The shared page is sixteen times larger, so
/// a listing is read in rounds; this program has one stack page and cannot hold the page itself.
const LISTING: usize = 256;
/// The most `READDIR` rounds one `ls` will make. A ceiling, not a limit on directories: it is here
/// so a cursor that fails to advance costs a short listing instead of a prompt that never returns.
const ROUNDS: usize = 16;

/// Copy a name into the page shared with the FS server.
fn put_page(bytes: &[u8]) {
    for (i, &b) in bytes.iter().take(fs_proto::PAGE).enumerate() {
        // SAFETY: FS_VA is a mapped, writable page of fs_proto::PAGE bytes in the navigating wiring,
        // and every caller is behind a `dir.is_some()` check, which is only true in that wiring.
        unsafe { core::ptr::write_volatile((FS_VA + i as u64) as *mut u8, b) };
    }
}

/// Copy `n` bytes out of that page (a listing landed there).
fn get_page(n: usize, out: &mut [u8]) {
    for (i, b) in out.iter_mut().take(n).enumerate() {
        // SAFETY: as above; `n` is bounded by the page and by `out`.
        *b = unsafe { core::ptr::read_volatile((FS_VA + i as u64) as *const u8) };
    }
}

// ---- `apropos`: searching the documentation store (milestone 40 phase 2) ----

/// **One index shard, as pages read one at a time through the file contract.**
///
/// This is the entire guest half of the search, and it is small on purpose: the layout was designed
/// so that a reader holds one 4 KiB page and never more, which is exactly what a client of the file
/// contract already shares with the FS server. So there is no buffering, no memory grant, and no
/// second copy of the index anywhere.
struct Shard {
    /// The slot the directory capability lives in, which is what a request is sent on.
    dir: u64,
    /// The open file handle for `<bundle>/index`.
    handle: u64,
}

impl manual::index::Pages for Shard {
    fn page(&mut self, index: u64, out: &mut [u8; manual::index::PAGE]) -> bool {
        let got = call(
            self.dir,
            fs::req(fs::READ, self.handle, manual::index::PAGE as u64),
            index * manual::index::PAGE as u64,
        )
        .0 as i64;
        if got <= 0 {
            return false;
        }
        let got = (got as usize).min(manual::index::PAGE);
        get_page(got, &mut out[..]);
        // A short read is a page at the end of the file. The layout guarantees a record never
        // straddles a page, so the tail is padding and zeroing it is the truth rather than a
        // convenience: leaving the previous page's bytes there would let a stale record be read.
        out[got..].fill(0);
        true
    }
}

/// The most of the bundle manifest one search reads. It is one short name per line and the shipped
/// store has four, so this is two orders of magnitude of headroom in a buffer that costs a quarter
/// of a page.
const MANIFEST_MAX: usize = 256;

/// **The merge across shards**, in `.bss` rather than on the stack: it is about 2.4 KiB and this
/// program's stack is twelve pages that several deeper paths already spend.
static mut RANKED: manual::index::Ranked = manual::index::Ranked::new();

/// **`apropos <word>`: name the installed pages that mention it.**
///
/// A builtin, and `grant_plan::Command::Apropos` carries the argument for why. What is here is only
/// the IO: open the store, read the manifest, and hand each shard to `manual::index::search`, which
/// is the same function `cargo xtask manual <word>` runs on the host over the same bytes.
///
/// **The store is opened from the root, not from the cwd**, because it is installed at the root of
/// whatever this shell was granted. A `cd` does not move the manual, and a search that answered
/// differently depending on where you were standing would be a worse tool.
///
/// **It grants nothing and spawns nothing.** What comes back is a list of names; opening one is a
/// separate line the reader types, and *that* is where a capability moves.
fn apropos(nav: &mut Nav, term: &[u8]) -> Say {
    use manual::index;

    if nav.dir.is_none() {
        return Say::NoDirectory;
    }
    if term.is_empty() {
        return Say::NeedsAName;
    }
    let dir = nav.dir.unwrap_or(DIR);

    let store = nav.name_call(
        fs::OPENDIR,
        fs::ROOT,
        index::STORE_DIR.as_bytes(),
        nav.rights,
    );
    if store < 0 {
        return Say::Failed(-store as i32);
    }
    let store = store as u64;

    // The manifest, copied out of the shared page before anything else touches it: every `OPENDIR`
    // below stages a name in that same page.
    let mut names = [0u8; MANIFEST_MAX];
    let handle = nav.name_call(fs::OPEN, store, index::MANIFEST.as_bytes(), 0);
    if handle < 0 {
        nav.close(store);
        return Say::Failed(-handle as i32);
    }
    let got = call(
        dir,
        fs::req(fs::READ, handle as u64, MANIFEST_MAX as u64),
        0,
    )
    .0 as i64;
    nav.close(handle as u64);
    if got < 0 {
        nav.close(store);
        return Say::Failed(-got as i32);
    }
    let got = (got as usize).min(MANIFEST_MAX);
    get_page(got, &mut names);

    // SAFETY: this process is single-threaded, so there is exactly one live reference to this
    // static for the whole run. It is taken once, here, and passed down by reference.
    let ranked = unsafe { &mut *core::ptr::addr_of_mut!(RANKED) };
    *ranked = index::Ranked::new();
    let mut trouble = 0i32;
    let mut unreadable = false;
    index::bundles(&names[..got], |bundle| {
        let at = nav.name_call(fs::OPENDIR, store, bundle, nav.rights);
        if at < 0 {
            trouble = -at as i32;
            return;
        }
        let shard = nav.name_call(fs::OPEN, at as u64, index::SHARD.as_bytes(), 0);
        if shard < 0 {
            trouble = -shard as i32;
        } else {
            let mut src = Shard {
                dir,
                handle: shard as u64,
            };
            // A shard this reader cannot read is reported rather than skipped: a search that
            // quietly left a bundle out would answer "no page says that" about pages that do.
            unreadable |= index::search(bundle, term, &mut src, ranked).is_err();
            nav.close(shard as u64);
        }
        nav.close(at as u64);
    });
    nav.close(store);

    // **A manifest that filled the buffer is a search that did not cover the store**, and saying
    // nothing would make "no page says that" a lie about bundles nobody looked in. Found by review
    // rather than by running: the shipped manifest is 25 bytes and this will not fire until the
    // store is ten times its size.
    if got == MANIFEST_MAX {
        print(b"  the bundle manifest filled this shell's buffer; bundles past it were not searched\n");
    }
    if unreadable {
        print(b"  a shard in the store is not an index this reader knows\n");
    }
    swish::write_apropos(term, ranked, &mut print);
    // A bundle that would not open is the filesystem's answer and belongs in the status, but only
    // after the results: the pages that *were* found are still the answer to the question.
    if trouble != 0 {
        return Say::Failed(trouble);
    }
    Say::Nothing
}

/// Print through the terminal: write the text into the shared page, CALL `OP_WRITE`. The reply
/// means the bytes are on the wire and the page is ours again.
fn print(s: &[u8]) {
    let n = s.len().min(4096);
    stage(s, n);
    call(TERM, proto::req(proto::OP_WRITE, n as u64), 0);
}

/// Copy `n` bytes into the outgoing shared page.
fn stage(s: &[u8], n: usize) {
    let out = OUT_VA as *mut u8;
    for (i, &b) in s[..n].iter().enumerate() {
        // SAFETY: the terminal's output page is mapped read/write at OUT_VA.
        unsafe { core::ptr::write_volatile(out.add(i), b) };
    }
}

/// Print a small unsigned number in base 10.
fn print_num(v: u64) {
    swish::write_num(v, &mut print);
}

/// Read a command line: stage the prompt, CALL `OP_READLINE`, and block until the terminal has a
/// line for us. The editing (cursor keys, history, backspace) happens entirely on the far side;
/// we get the finished line in `LINE_VA` and its length and flags in the reply.
fn read_line(prompt: &[u8], out: &mut [u8]) -> (usize, u64) {
    stage(prompt, prompt.len());
    let (len, flags) = call(TERM, proto::req(proto::OP_READLINE, prompt.len() as u64), 0);
    let len = (len as usize).min(out.len());
    let src = LINE_VA as *const u8;
    for (i, b) in out[..len].iter_mut().enumerate() {
        // SAFETY: the line page is mapped read-only and holds at least `len` bytes.
        *b = unsafe { core::ptr::read_volatile(src.add(i)) };
    }
    (len, flags)
}

/// The interactive shell: a terminal at slot 0, a spawn channel, a result channel, a budget. It is
/// role **0** because `crates/system_initializer` starts this program with
/// `(0, fs_rights, clock_slot)`, and it is also what any unrecognized role falls through to: this
/// program's failure mode should be a prompt.
/// **The navigating witness** (milestone 47): no terminal, a directory capability at slot 0, and a
/// report endpoint at slot 1. It runs the same builtins this file's prompt runs and reports a
/// bitmap; see [`navigate`].
const ROLE_NAVIGATE: u64 = 1;
/// **The globbing witness** (milestone 47's globbing lane): the same wiring as [`ROLE_NAVIGATE`],
/// pointed at the fixture's `globset`, running `echo` and the grant planner over one pattern and
/// reporting whether they agree. See [`globbing`].
const ROLE_GLOB: u64 = 2;
/// **The pipeline witness** (milestone 50): the interactive wiring minus the keyboard. It holds a
/// terminal it writes to, a spawn channel, a result channel and a budget, and it types a fixed
/// script of command lines through the same [`dispatch`] the prompt uses. See [`piping`].
const ROLE_PIPELINE: u64 = 3;
/// **The redirection witness** (milestone 50): [`ROLE_PIPELINE`]'s wiring plus a directory
/// capability at [`DIR_TERMINAL`], which is the endowment `>` and `<` need and the one the
/// interactive boot grants. It types a fixed script through the same [`dispatch`] the prompt uses.
/// See [`redirecting`].
const ROLE_REDIRECT: u64 = 4;
/// **The timing witness** (milestone 86): [`ROLE_PIPELINE`]'s wiring plus, in the wiring that has
/// one, a clock page at the slot `x2` names. It types a fixed script through the same [`dispatch`]
/// the prompt uses, and the same script is run three times against three clock states, which is what
/// makes the refusals assertions rather than unreachable branches. See [`timing`].
const ROLE_TIMING: u64 = 5;

/// **What `arg1` carries into every role that holds a directory**: the `fs_proto::dir` rights that
/// capability was granted, with 0 meaning "you were granted no directory at all".
///
/// The shell is **told** rather than asking, and that is a gap in `fs_proto` rather than a shortcut:
/// nothing on that wire reports what a handle carries, and `OPENDIR` refuses a request wider than
/// the parent instead of narrowing it, so a shell that guessed `dir::ALL` from a narrower capability
/// could not `cd` at all. See notes/shell-navigation.md.
///
/// **`x2` carries the clock slot** (milestone 86), or [`NO_CLOCK`] for a wiring that granted none.
/// Told for the same reason and a sharper one: the slot moves with what else the boot granted, and a
/// shell that probed the wrong number would find some other object and map a page that is not a
/// clock. See [`CLOCK_SLOT`].
#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, arg: u64, clock: u64) -> ! {
    CLOCK_SLOT.store(clock, core::sync::atomic::Ordering::Relaxed);
    match role {
        ROLE_NAVIGATE => navigate(arg),
        ROLE_GLOB => globbing(arg),
        ROLE_PIPELINE => piping(),
        ROLE_REDIRECT => redirecting(arg),
        ROLE_TIMING => timing(),
        _ => interactive(arg),
    }
}

/// **A shell driven by a script instead of a keyboard, running real pipelines** (milestone 50).
///
/// Every line below goes through [`dispatch`], which is the function the prompt calls, so what is
/// under test is the shell rather than a reimplementation of it. Everything it prints goes out over
/// the terminal contract to whoever holds the other end, which in the guest test is the kernel; the
/// transcript it produces is the assertion.
///
/// The lines are chosen so that the answers are checkable against each other rather than against
/// magic numbers. `date` and `date | wc` are the same binary sent to two destinations, and the
/// second one's byte count has to be the first one's length or the indifference claim is false.
fn piping() -> ! {
    let mut nav = Nav::empty();
    for line in [
        // The shell as the producer: a builtin's bytes into a pipe, counted by a real process.
        &b"echo hello world | wc"[..],
        // Three stages, so a middle stage is a real middle: `wc`'s own output is a byte stream, so
        // it composes, which is what `OutputSpec::Bytes` on `wc` is for.
        b"echo hello world | wc | wc",
        // The same unmodified `date`, twice, to two different destinations.
        b"date",
        b"date | wc",
        // And the refusals, at the prompt, with nothing spawned.
        b"wc",
        b"worker 9 | wc",
        b"date | date",
        b"date > report.txt",
    ] {
        print(b"$ ");
        print(line);
        print(b"\n");
        dispatch_line(&mut nav, line);
    }
    print(PIPELINE_DONE);
    exit();
}

/// The last thing [`piping`] prints, so a reader knows the transcript is complete rather than
/// having to guess from a quiet endpoint. Must match `kernel::user::pipeline_tests`.
const PIPELINE_DONE: &[u8] = b"== pipelines done\n";

/// **A scripted shell that holds a filesystem as well as a spawn channel** (milestone 50).
///
/// The same wiring as [`piping`] plus one capability, and that one capability is the whole
/// difference between a `>` that is refused and a `>` that writes a file. Every line goes through
/// [`dispatch`], so what is under test is the prompt.
///
/// The lines are ordered so the answers check each other rather than constants, which is the shape
/// [`piping`]'s script has and for the same reason:
///
/// - `ls > out.txt` writes a listing into a file; the `ls` after it prints **the same listing**
///   (nothing between them changes the directory), and `wc < out.txt` has to agree with what was
///   printed. One builtin, two destinations.
/// - `wc out.txt` is the line above it with the `<` left out, and it has to give the same three
///   numbers: naming a file to a program that declares an input is the operator, unwritten.
/// - `date` and `date > date.txt` are one unmodified program sent two places, and `wc < date.txt`
///   has to report the length of what `date` printed.
/// - The last two files are **the same two commands with one operator changed**, so what `>>` does
///   is measured against what `>` does rather than against a constant.
fn redirecting(rights: u64) -> ! {
    let mut nav = Nav::rooted_at(DIR_TERMINAL, rights);
    for line in [
        // The shell as producer *and* as sink: no process is spawned by this line at all.
        &b"ls > out.txt"[..],
        b"ls",
        b"wc < out.txt",
        // The same designation with the operator left out (milestone 31's input operand). It has
        // to answer what the line above it answered, or one of the two opened something else.
        b"wc out.txt",
        // **And the same designation inside a pipeline**, which is the line that used to answer
        // nothing at all: the operand is resolved by the planner, and `pipeline` used to wire the
        // head's input off the line instead of off the plan, so the stage was spawned with an empty
        // input slot, where a `recv` answers `NoSuchSlot` rather than blocking: the stage counted
        // an empty stream and said so. Its answer has to be the length of the line above it.
        b"wc out.txt | wc",
        // The same unmodified `date`, twice, to two destinations.
        b"date",
        b"date > date.txt",
        b"wc < date.txt",
        // `>` twice keeps the second line only; `>>` keeps both. Same commands, same order, one
        // character different, and the two answers are each other's control.
        b"echo one > trunc.txt",
        b"echo two > trunc.txt",
        b"wc < trunc.txt",
        b"echo one > app.txt",
        b"echo two >> app.txt",
        b"wc < app.txt",
        // And `>>` creates a name that is not there, so it is not "open then seek": there is
        // nothing to open. Nothing between this and the count writes to it.
        b"echo one >> fresh.txt",
        b"wc < fresh.txt",
        // And the refusals a directory does not rescue.
        b"wc < nosuch.txt",
        b"worker 9 > out.txt",
        // **`2>` binds to a declaration** (DECISIONS §67). `wc` writes one stream and its
        // diagnostics ride it, so the operator names nothing and the line does not run. The
        // sentence is the assertion; what it is *not* is a permission.
        b"wc out.txt 2> err.txt",
        // ---- milestone 67's grammar, on this same shell (notes/swish-language.md) ----
        //
        // **One witness rather than a seventh**, and that is a memory decision rather than a
        // filing one: every scripted shell in this suite is a live process whose frames are never
        // reclaimed, and adding one more put `time_tests` over the frame pool intermittently
        // (`refused to load a user program: Unmappable(OutOfFrames)`). The wiring these lines need
        // is this wiring exactly, so a second copy of it bought nothing but the failure.
        //
        // The correctness gap quoting closed: a name with a space in it, written and read back.
        // Before milestone 67 the `>` would have written to a file called `"my`.
        b"echo hello world > \"my notes.txt\"",
        b"wc < \"my notes.txt\"",
        // The same designation with the operator left out, which has to agree with the line above
        // it or one of the two opened something else.
        b"wc \"my notes.txt\"",
        // Quoted and unquoted, side by side: the same four characters are one name and a set.
        b"echo \"*.txt\"",
        b"echo *.txt",
        // A quoted operator is text, so this line has no redirection on it and writes no file.
        b"echo 'a > b'",
        // Sequencing. `worker 3` succeeds and `worker` alone is refused at the prompt, so these
        // lines cover every arm of the condition table with real commands.
        b"worker 3 && echo yes",
        b"worker && echo yes",
        b"worker 3 || echo no",
        b"worker || echo no",
        // `;` runs the second whatever the first did, which is the arm the four above cannot show.
        b"worker && echo yes ; echo always",
        // The status, after a command that ran and after one this shell refused. The second is the
        // decision this milestone settled: a refusal is 2 rather than 1, because nothing ran.
        b"worker 3",
        b"echo $?",
        b"worker",
        b"echo $?",
        // And the refusals the grammar itself makes, which run nothing at all.
        b"echo 'unclosed",
        b"date &&",
    ] {
        print(b"$ ");
        print(line);
        print(b"\n");
        dispatch_line(&mut nav, line);
    }
    print(REDIRECT_DONE);
    exit();
}

/// [`PIPELINE_DONE`]'s twin for [`redirecting`]. Must match `kernel::user::redirection_tests`.
const REDIRECT_DONE: &[u8] = b"== redirections done\n";

/// **A scripted shell that times what it runs** (milestone 86).
///
/// [`piping`]'s wiring plus at most one capability, and that one capability is the whole difference
/// between a `time` that reports a duration and a `time` that refuses. The same script runs against
/// three clock states (a published page, a blank page, no page at all), so the refusals are reached
/// by changing a cspace rather than by a branch anybody wrote for the test.
///
/// The lines check each other rather than constants, which is [`redirecting`]'s shape:
///
/// - `worker 3` and `time worker 3` are the same command run twice, so the answer has to be the same
///   both times. That is the "what you time is what you run" claim, made where it can fail.
/// - `time echo hello` times a **builtin**, which spawns nothing: the duration is real and there is
///   no process anywhere in it.
/// - `time` alone names the third refusal, which is about the line and not about a clock.
fn timing() -> ! {
    let mut nav = Nav::empty();
    for line in [
        // The control: the untimed command, so the timed one below has something to agree with.
        &b"worker 3"[..],
        // A timed spawn. `worker` declares no clock and is handed none, which is the whole point of
        // the milestone: the thing being timed needs no authority to be timed.
        b"time worker 3",
        // A timed builtin. No process, no spawn, no grant, and still a duration.
        b"time echo hello",
        // And the prefix with nothing after it.
        b"time",
    ] {
        print(b"$ ");
        print(line);
        print(b"\n");
        dispatch_line(&mut nav, line);
    }
    print(TIMING_DONE);
    exit();
}

/// [`PIPELINE_DONE`]'s twin for [`timing`]. Must match `kernel::user::time_tests`.
const TIMING_DONE: &[u8] = b"== timings done\n";

/// The interactive prompt. `rights` is the [`_start`] convention: the `fs_proto::dir` rights of the
/// directory capability at [`DIR_TERMINAL`], or 0 for a boot that wired no filesystem.
fn interactive(rights: u64) -> ! {
    print(b"\nnife capability shell. naming a resource in a command IS granting it.\n");
    print(b"commands: help, echo <text>, caps [command], time <command>, xargs <command>,\n");
    print(b"          cd, pwd, ls, mkdir, apropos <word>, rm, wc, doc, <prog> [--mem N] [arg]\n");
    print(b"          and the operators  >  >>  <  |\n");
    print(b"          'quote a whole word'   and   ;  &&  ||   with  echo $?  for the status\n");

    // Whether the navigation builtins and the redirection operators have anything to name is
    // decided here, by one capability. A boot that wired no FS service starts this program with
    // `rights = 0` and every one of them answers "this shell holds no directory capability".
    let mut nav = if rights == 0 {
        Nav::empty()
    } else {
        Nav::rooted_at(DIR_TERMINAL, rights)
    };
    let mut line = [0u8; 128];
    loop {
        let (n, flags) = read_line(b"$ ", &mut line);
        if flags & proto::FLAG_INTERRUPTED != 0 {
            // ^C at the prompt: the terminal discarded the line. Account for this interrupt so it
            // does not leak into the next job's watch, then come back for the next line.
            CONSUMED.store(intr_count(), core::sync::atomic::Ordering::Relaxed);
            continue;
        }
        if flags & proto::FLAG_EOF != 0 {
            // ^D on an empty line. This shell is the session; there is nowhere to exit to.
            print(b"  (end of input; this shell has nowhere to exit to)\n");
            continue;
        }
        dispatch_line(&mut nav, &line[..n]);
    }
}

/// **Run one line if it is a navigation builtin**, calling `each` for every entry a listing
/// produces. `None` means the line was not one of them.
///
/// Split out from [`dispatch`] because the navigating witness ([`navigate`]) runs the *same*
/// builtins from the same command lines with no terminal to print to. One implementation, two
/// callers, and the guest test therefore exercises what the prompt exercises rather than a
/// reimplementation of it.
fn builtin(nav: &mut Nav, cmd: &[u8], each: &mut dyn FnMut(&[u8], bool)) -> Option<Say> {
    match grant_plan::parse(cmd) {
        // They spawn nothing and grant nothing: the shell is rebinding and reading a capability it
        // already holds, which is why these are builtins, and why the worry about an over-granted
        // listing *program* (which would hold the power to read everything it lists) does not arise.
        Command::Cd(path) => Some(nav.cd(path)),
        Command::Ls(path) => Some(nav.ls(path, each)),
        Command::Mkdir(path) => Some(nav.mkdir(path)),
        _ => None,
    }
}

/// **Read one line and act on it**, which since milestone 50 means asking first whether it has any
/// operators on it.
///
/// A line with no `>`, `<` or `|` takes exactly the path it took before those existed, which is
/// deliberate: the operators must not make an ordinary command more expensive or more complicated,
/// because an ordinary command is nearly every command.
/// The routing itself is [`swish::route`], host-tested there, including the one ordering with a bug
/// behind it: `caps` is answered **before** the line is split, or `caps date | wc` would lose the
/// pipe. What is left here is which capability each answer reaches for.
/// **Read one typed line and run the commands on it** (milestone 67, notes/swish-language.md).
///
/// This is the outermost thing the prompt calls. It cuts the line on `;`, `&&` and `||` and hands
/// each segment to [`dispatch`], which is the function every path in this file already went
/// through, so **a segment is a whole command line and nothing under here learned a new grammar**.
///
/// # A connector carries one bit and no capability
///
/// Each segment is planned from scratch against what this shell holds, exactly as if it had been
/// typed alone. That is not a restriction adopted here, it is what the shell already was, and
/// sequencing is the first construct that could have broken it. The concrete hazard is the pipeline
/// region: a line splits one off this shell's budget, mints its endpoints in it, and `DESTROY`s it
/// when the line is over, and that destroy is what turns a stalled writer's next `SEND` into
/// `abi::Error::Gone` (notes/pipes.md). Reusing one region across a chain would keep every earlier
/// segment's endpoints alive for the whole chain. It does not, because the split is **here**, above
/// [`pipeline`], rather than inside it.
///
/// # A skipped segment leaves `$?` alone
///
/// [`LAST`] is written only by a segment that ran, which is bash's rule: `false && echo hi` leaves
/// `$?` at the status of `false`. There is no "skipped" status, because nothing happened.
fn dispatch_line(nav: &mut Nav, cmd: &[u8]) {
    let seq = match sequence::split(cmd) {
        Ok(s) => s,
        Err(r) => {
            // A line that cannot be cut runs nothing, so it is one refusal and one status.
            CURRENT.store(Status::Ran.code(), core::sync::atomic::Ordering::Relaxed);
            say(Say::Cannot(r));
            LAST.store(
                CURRENT.load(core::sync::atomic::Ordering::Relaxed),
                core::sync::atomic::Ordering::Relaxed,
            );
            return;
        }
    };
    for &(joint, segment) in seq.segments() {
        if !joint.runs_after(last()) {
            continue;
        }
        CURRENT.store(Status::Ran.code(), core::sync::atomic::Ordering::Relaxed);
        dispatch(nav, segment);
        LAST.store(
            CURRENT.load(core::sync::atomic::Ordering::Relaxed),
            core::sync::atomic::Ordering::Relaxed,
        );
    }
}

fn dispatch(nav: &mut Nav, cmd: &[u8]) {
    match swish::route(cmd) {
        Route::Caps(tail) => caps(nav, tail),
        Route::Time(tail) => time_command(nav, tail),
        Route::Xargs(tail) => xargs(nav, tail),
        Route::One(stage) => dispatch_one(nav, stage),
        Route::Pipeline(l) => pipeline(nav, l),
        Route::Cannot(r) => say(Say::Cannot(r)),
    }
}

/// **Run a command and say how long it took** (milestone 86, notes/time-command.md).
///
/// The tail goes back through [`dispatch`], which is the function the prompt calls, so `time` adds no
/// second way to run anything: a pipeline is timed as a pipeline, a builtin as a builtin, and a
/// refusal is refused exactly as it would have been. That is what makes "what you time is what you
/// run" a property of this code rather than a claim about it.
///
/// **The clock is this shell's, and the command is told nothing.** `time wc report.txt` times a
/// program whose whole endowment is one endpoint and no clock at all, which is the Unix behaviour and
/// the capability-model answer at the same time: a duration is observable to anyone who can watch a
/// thing start and stop, so measuring it needs the observer's authority and not the subject's.
/// Delegating a clock to the child instead would change what the child can do, and that is a
/// different tool.
///
/// # What the number is, and what it is not
///
/// Wall clock, between this shell deciding to run the line and the line being over. Not CPU time:
/// nothing in this kernel is asked what a thread spent, so there is no `user` or `sys` row to print
/// and inventing one would be worse than the gap. If that arrives it is another row here.
///
/// The two readings are taken **around** the dispatch and nothing else is between them, so what is
/// measured includes this shell's own planning, spawning and draining. For a spawned program that is
/// the honest number: the shell is the thing that started it and the thing that noticed it finish,
/// and there is no other vantage point from which the question has an answer.
fn time_command(nav: &mut Nav, tail: &[u8]) {
    // Collapse a nested prefix rather than recursing through [`dispatch`]. `time time date` is a line
    // a person can type, and each level of recursion here costs a `dispatch` frame on a stack that
    // has run out four times already (notes/pipes.md). Timing something twice measures nothing twice.
    let mut tail = grant_plan::trim(tail);
    while let Command::Time(inner) = grant_plan::parse(tail) {
        tail = grant_plan::trim(inner);
    }
    if tail.is_empty() {
        refused();
        return swish::write_untimed(Untimed::NothingToTime, &mut print);
    }
    // **A duration needs no clock** (§72). Wall time is an offset plus this counter; across a
    // command the offset cancels, so the subtraction below is the whole measurement, and
    // `user_rt::monotonic_nanos` is ambient by the kernel's own choice (it opened the counter to
    // EL0). `time` therefore cannot be refused and has no clock wiring: what a capability gates
    // here is wall-clock *identity*, which is `date`'s business, not elapsed time.
    //
    // This is also strictly more correct than the clock version it replaces. A clock stepped
    // mid-command used to make the answer a difference of two readings on different clocks,
    // which the shell had to detect and disclaim; a monotonic counter cannot be stepped, so
    // there is nothing left to disclaim.
    let start = monotonic_nanos();

    dispatch(nav, tail);

    // Saturating out of habit rather than need: the counter is monotonic, so `end` cannot precede
    // `start`, and a wrap would take centuries at either board's frequency.
    swish::write_timing(monotonic_nanos().saturating_sub(start), &mut print);
}

/// The most batches one sweep will run. A ceiling so a resume rule that failed to advance costs a
/// bounded number of spawns rather than a prompt that never comes back, in exactly the spirit of
/// [`ROUNDS`]. Eight names a batch, so this is 2048 names, and a directory that large is a machine's
/// problem before it is this shell's.
const MAX_BATCHES: u64 = 256;

/// **Run a command once per batch of what its pattern matched** (milestone 109).
///
/// # Why this is a prefix word and not a program
///
/// A batching *program* would have to hold, at once, at least the union of every batch it hands
/// out. There is no carrier for that union: that is the premise of the whole milestone, since a set
/// larger than [`grant_plan::expand::MAX_NAMES`] is exactly what cannot be handed over. So such a
/// program could only be given the **directory** plus a pattern, which is milestone 47's rejected
/// "the directory plus a name list" answer wearing a different hat: it over-grants catastrophically,
/// and it hands the batcher authority over names the pattern never matched.
///
/// Unix's `xargs` can be a program precisely because it holds *nothing*. Names in a pipe are text,
/// and `rm`'s authority there comes from its uid. Here a name in a pipe is still text, so a
/// `find | xargs rm` analogue would spawn an `rm` holding no authority at all; the pipe cannot carry
/// the thing that has to be batched. What can mint a per-batch caretaker is whatever holds the
/// directory capability with the right to delegate it, and in this system that is the shell asking
/// init. So the batching goes where the authority already is.
///
/// This is **not** milestone 47's rejected "make `rm` a builtin", either. `rm` stays a program with
/// an attenuated grant; what became a builtin is the *iteration*, which is a property of how the
/// shell delegates rather than of anything any command does. `caps` and `time` are the precedent.
///
/// # It stops at the first batch that does not run
///
/// Unix carries on and reports 123 at the end, and that is its mechanism talking: its `xargs` cannot
/// know what a child did to the names it was handed. Here the shell printed each batch's set before
/// that batch ran, so it can name the boundary, and a sweep that stopped is a **prefix** of the
/// match rather than an arbitrary subset. Batch four succeeding after batch three failed is the
/// outcome worth designing against, because the user would have no way to tell which names it left.
fn xargs(nav: &mut Nav, tail: &[u8]) {
    // Collapse a nested prefix rather than recursing, [`time_command`]'s reason: `xargs xargs rm
    // *.txt` is a line a person can type, batching a batch is still one sweep, and every level of
    // recursion costs a frame on a stack that has run out four times already.
    let mut tail = grant_plan::trim(tail);
    while let Command::Xargs(inner) = grant_plan::parse(tail) {
        tail = grant_plan::trim(inner);
    }
    if tail.is_empty() {
        refused();
        print(b"  xargs: name a command to batch\n");
        return;
    }
    let mut sweep = swish::Sweep::default();
    let mut resume = Resume::Start;
    for _ in 0..MAX_BATCHES {
        nav.batching = Batching {
            resume: Some(resume),
            index: sweep.batches + 1,
            next: None,
            names: 0,
        };
        // Each batch is its own command as far as the status is concerned, and the sweep stops at
        // the first one that did not run. That is the same reading `&&` takes (see [`Status`]): a
        // refusal and a failure are both a no, and which it was stays in the status the sweep
        // leaves behind for `$?`.
        CURRENT.store(Status::Ran.code(), core::sync::atomic::Ordering::Relaxed);
        dispatch(nav, tail);
        let batching = nav.batching;
        nav.batching = Batching::default();
        if !current().ok() {
            sweep.stop();
            break;
        }
        sweep.ran(batching.names);
        match batching.next {
            Some(r) => resume = r,
            None => break,
        }
    }
    swish::write_sweep(&sweep, &mut print);
}

/// Parse one line with `grant_plan` and act on it. All parsing and the manifest check are the host-tested
/// crate; this function is only IO and capability moves.
fn dispatch_one(nav: &mut Nav, cmd: &[u8]) {
    if let Some(said) = builtin(nav, cmd, &mut |name, is_dir| {
        print(b"  ");
        print(name);
        if is_dir {
            print(b"/");
        }
        print(b"\n");
    }) {
        say(said);
        return;
    }
    match grant_plan::parse(cmd) {
        Command::Empty => {}
        Command::Help => help(),
        // `echo` expands, and that is half of the globbing demonstration: what it prints is
        // literally the authority the same words would transfer to a program. The newline goes out
        // only when the line itself did, so a refused expansion is one line and not two.
        Command::Echo(text) => match echo(nav, text, &mut print) {
            Say::Nothing => print(b"\n"),
            said => say(said),
        },
        // Unreachable through [`dispatch`], which answers the two prefix words before it splits the
        // line so they can see the operators. Kept so this function is complete on its own: it is
        // the whole of "what one command means", and a reader should not have to know about the
        // interception to find every arm.
        Command::Caps(tail) => caps(nav, tail),
        Command::Time(tail) => time_command(nav, tail),
        Command::Xargs(tail) => xargs(nav, tail),
        Command::Pwd => print_pwd(nav),
        // **Search, which grants nothing** (milestone 40 phase 2). It is here rather than in
        // [`builtin`] because its answer is columns of text rather than a listing, so the witness's
        // `each(name, is_dir)` callback is the wrong shape for it; what it shares with the
        // navigation builtins is the argument for being a builtin at all.
        Command::Apropos(term) => say(apropos(nav, term)),
        Command::Run(spec) => run(nav, cmd, spec),
        // Handled above, by the one implementation the witness also runs.
        Command::Cd(_) | Command::Ls(_) | Command::Mkdir(_) => {}
    }
}

/// Print where we are, relative to our own root.
fn print_pwd(nav: &Nav) {
    if nav.dir.is_none() {
        say(Say::NoDirectory);
        return;
    }
    swish::write_pwd(&nav.cwd, &mut print);
}

/// Print what a builtin had to say. Every line is a statement about a name or a capability, never
/// about a policy, and the wording is [`swish::write_say`]'s so a host test can hold it: this shell
/// does not get to invent a friendlier word for a refusal.
fn say(s: Say) {
    // **Which kind of no it was** (milestone 67). The filesystem answering with an errno is
    // something that was attempted and did not work; everything else here is this shell declining
    // from what it holds, with nothing sent and nothing spawned. Under `xargs` either ends the
    // sweep, which is what the old single bit recorded.
    match s {
        Say::Nothing => {}
        Say::Failed(_) => failed(),
        _ => refused(),
    }
    swish::write_say(s, &mut print);
}

fn help() {
    swish::write_help(&mut print);
}

/// Resolve an invocation, then either refuse it at the prompt (a mismatch the manifest caught) or
/// spawn it, granting exactly what the command named and nothing else.
fn run(nav: &mut Nav, cmd: &[u8], spec: RunSpec) {
    // **Expand first.** A pattern designates the names it matched, so the planner has to see the
    // set; and a pattern that matched nothing, or too much, is refused here with nothing spawned.
    let expanded = match expansion(nav, &spec) {
        Ok(e) => e,
        // A grant refusal is attributed to the program when there is one, because "rm: no name here
        // matches that pattern" reads better than the bare sentence and is just as true.
        Err(Say::Cannot(r)) => return refuse(spec, r),
        Err(said) => return say(said),
    };
    match grant_plan::plan(&spec, holdings(nav), expanded) {
        Err(refusal) => refuse(spec, refusal),
        // A supervised job runs under the two-tier ^C path (DECISIONS §24); a fast job is simply
        // spawned and waited on.
        Ok(endow) if endow.interruptible => spawn_interruptible(endow),
        // **`wc report.txt`**, which is `wc < report.txt` with the operator left out: the planner
        // put the designated name in the *source* (`grant_plan`'s input operand), so this line is a
        // one-stage pipeline the shell feeds, and it runs down exactly the path a `<` runs down.
        // There is no second mechanism here, which is the reason to do it this way: the bytes reach
        // the child the same way, and the child still cannot tell a file from a pipe.
        Ok(endow) => match endow.source {
            Source::File(g) => {
                // The line reparses into the one stage it is. It cannot fail (this command came out
                // of it), and a `Line` is what `run_pipeline` reads its stage count off.
                let Ok(l) = line::split(cmd) else { return };
                // **One element, not `MAX_STAGES`**, and that is a stack decision rather than a
                // tidiness one. An `Endowment` carries a whole `NameSet` by value, so a full-width
                // array here overflowed the shell's stack at the *first* line this arm ran, which
                // presented as a data abort one word below the lowest mapped stack page. This line
                // is a single stage by construction, and `run_pipeline` takes a slice.
                run_pipeline(nav, l, &[Some(endow)], false, None, Some(g), None);
            }
            _ => spawn(endow),
        },
    }
}

/// Print a refusal in the capability model's voice, which is [`swish::write_refusal`]'s job: the
/// fixed half is `grant_plan`'s and the program name is supplied where one helps.
fn refuse(spec: RunSpec, refusal: Refusal) {
    refused();
    swish::write_refusal(&spec, refusal, &mut print);
}

/// One process's three `START` argument words, packed by this shell and forwarded by init without
/// being read. `fs_proto::grant`'s layout: two words of name, and a spec carrying the length plus
/// either a rights mask (a caretaker's) or an option mask (a program's).
type StartWords = (u64, u64, u64);

/// **What a directory grant travels as**: one process's `START` words each, for the two processes a
/// grant is made of.
///
/// Named rather than a bare pair of tuples because the order is the wire's, and getting it backwards
/// would start `rm` with a directory's name and a caretaker with a file's, which comes up serving a
/// namespace nobody meant. `spawnproto::GRANT_WORDS` is the count on the other side.
struct DirWords {
    /// The `fs_subtree_caretaker`'s: the granted directory, and the rights to ask for.
    caretaker: StartWords,
    /// The confined program's: the operand inside that directory, and the options typed with it.
    child: StartWords,
}

/// **Turn a planned directory grant into the two triples init needs**, or into the sentence that
/// says why this one cannot be delivered (milestone 31 phase 3).
///
/// The `Ok` half is the whole of the delivery: `(the caretaker's START words, the program's)`. The
/// caretaker is told the directory and the `fs_proto::dir` rights to ask for; the program is told
/// the operand and the options that were typed. Neither triple is decoded by anything between here
/// and the process it starts, which is why `spawnproto` can carry them without linking the
/// filesystem contract.
///
/// **The rights are computed from what was typed, and that is the point of `-r` being an option
/// rather than a mode.** Without it the capability is `REMOVE` alone: the program may take a name
/// out of this one directory and cannot enumerate it, read it, or walk into it. With it the
/// capability is `REMOVE_TREE`, which adds exactly the walk. Typing one letter visibly widens the
/// grant, and `caps rm -r logs` prints the difference before anything happens.
///
/// # The two shapes this cannot deliver, and neither is a permission
///
/// Both are facts about what a `fs_subtree_caretaker` is, so both are refused here with nothing
/// spawned rather than half-granted.
///
/// **A grant on the root of this shell's namespace.** A caretaker's whole attenuation is one
/// `OPENDIR` *into* the granted directory, and the root has no name to descend into: `fs_proto`'s
/// contract resolves a single component under a handle, and there is no verb for "the directory I
/// already hold, with fewer rights". So `rm gate.txt` typed at the top prompt cannot be narrowed at
/// all, and handing the program the unnarrowed service would be granting it the whole disk. The
/// answer is a design fork rather than a line of code (a `NARROW` verb on the contract, or a boot
/// whose shell is rooted one level down), and it is recorded in the roadmap rather than guessed at
/// here.
///
/// **A set of more than one name.** That grant is a `fs_nameset_caretaker`, which is a different
/// program taking its set in a frame; init builds the subtree one today. `rm *.txt` is planned,
/// previewed by `caps`, and refused at the point of delivery.
fn dir_grant(g: &GrantDir, flags: u64) -> Result<DirWords, &'static [u8]> {
    // The directory the caretaker descends into. One component, because that is one `OPENDIR`; a
    // deeper path is a *chain* of caretakers (DECISIONS §92 names it as the case supervision was
    // chosen for) and init builds one.
    let dir = match g.dir.depth() {
        0 => {
            return Err(
                b"  that names the root of this shell's namespace, and a caretaker is built by \n  descending into a directory; there is no name here to descend into\n",
            );
        }
        1 => g.dir.component(0),
        _ => {
            return Err(
                b"  that directory is more than one level down, and init builds one caretaker \n  per grant; a deeper grant is a chain of them, which is not built\n",
            );
        }
    };
    let Some(name) = g.names.only() else {
        return Err(
            b"  a set of names is delivered by a nameset caretaker, and init builds the subtree \n  one; name a single file\n",
        );
    };
    if !fs_proto::grant::fits(dir) || !fs_proto::grant::fits(name) {
        return Err(b"  that name does not fit in a grant's two argument words\n");
    }
    // What `-r` buys, stated as the difference between two capabilities rather than as a branch in
    // the program: REMOVE alone cannot even look at what is under the directory.
    let rights = if g.subtree {
        fs_proto::dir::REMOVE_TREE
    } else {
        fs_proto::dir::REMOVE
    };
    let (dir_lo, dir_hi) = fs_proto::grant::pack_name(dir);
    let (name_lo, name_hi) = fs_proto::grant::pack_name(name);
    Ok(DirWords {
        // `fs_subtree_caretaker::_start(name_lo, name_hi, spec)`.
        caretaker: (dir_lo, dir_hi, fs_proto::grant::spec(dir.len(), rights)),
        // `rm::_start(spec, name_lo, name_hi)`, whose spec carries the options where a caretaker's
        // carries rights: both are "what this process was started with".
        child: (fs_proto::grant::spec(name.len(), flags), name_lo, name_hi),
    })
}

/// Grant and spawn. The one moment authority moves: split any memory grant off our own budget,
/// direct init to load the program, delegate the grant, and read the one answer that comes back.
fn spawn(e: Endowment) {
    // A grant this path cannot deliver must stop here, loudly. `plan` already refuses a `file:` when
    // `holdings().dir` is false, so today this is unreachable; it exists because the day that flips,
    // the thing that must NOT happen is a child spawned without the file the command named while the
    // prompt says nothing. Authority the user thought they granted must never quietly evaporate,
    // which is the same rule that makes an unexpected token a refusal instead of a shrug.
    if e.file.is_some() {
        refused();
        print(
            b"  a file grant needs init to build the caretaker; this shell cannot deliver one yet\n",
        );
        return;
    }
    // **The directory grant, which init delivers** (milestone 31 phase 3). This shell's file-service
    // endpoint carries no GRANT, so it holds nothing it could hand a caretaker; what it can do is
    // say what the grant *is*, and init, which holds the service, builds a `fs_subtree_caretaker`
    // for it. [`dir_grant`] is where the shape of the grant meets the shape of what can be
    // delivered, and it returns the words rather than sending them so a refusal happens here, with
    // nothing spawned.
    let dir_words = match e.dir {
        None => None,
        Some(g) => match dir_grant(&g, e.flags) {
            Ok(words) => Some(words),
            Err(sentence) => {
                refused();
                print(sentence);
                return;
            }
        },
    };
    // A memory grant is carved from the shell's own untyped. If our budget is spent, say so plainly
    // rather than sending init a promise we cannot keep.
    let mem_slot = if e.mem_pages > 0 {
        match untyped_split(e.mem_pages) {
            Some(slot) => Some(slot),
            None => {
                failed();
                print(b"  this shell's memory budget is exhausted; nothing left to grant\n");
                return;
            }
        }
    } else {
        None
    };

    // The request: program id, argument, page count, and the operators' answer (grant_plan::spawnproto).
    //
    // **`diagnostics` is false here and always will be** (DECISIONS §67). This path runs a line with
    // no operators on it, so there is no `2>` and nothing for this shell to back; a program that
    // declares a second stream gets the terminal's own sink, which init endows from the manifest the
    // way it endows the clock. `2>` makes a line non-plain and goes down [`pipeline`].
    let (w0, w1, w2) = spawnproto::request(
        e.prog.id(),
        e.arg,
        e.mem_pages,
        spawnproto::Wiring {
            interruptible: false,
            sink: false,
            source: false,
            diagnostics: false,
            dir: dir_words.is_some(),
        },
    );
    send(SPAWN, w0, w1, w2);

    // **The grant's two data messages, before any delegation** (`spawnproto::GRANT_WORDS`): the
    // caretaker's three `START` words, then the confined program's. They go first because they are
    // data and the delegation is capabilities, and both sides read the order off the one wiring word.
    if let Some(DirWords { caretaker, child }) = dir_words {
        send(SPAWN, caretaker.0, caretaker.1, caretaker.2);
        send(SPAWN, child.0, child.1, child.2);
    }

    // If a budget rode along, delegate it now, narrowed to WRITE|GRANT so init can re-insert it into
    // the child (init narrows it again to WRITE there: the child spends it, it does not lend it).
    if let Some(slot) = mem_slot {
        delegate(slot, abi::rights::WRITE | abi::rights::GRANT);
        cap_delete(slot); // our copy is delegated; free the slot
    }

    // One reader, one word: a real program's answer, or init's spawn-failed sentinel. A program
    // whose manifest says it writes **bytes** is the exception: its answer is a stream, so it is
    // drained by a reader that knows the sink contract's framing.
    if e.prog.manifest().output.is_byte_stream() {
        drain_text();
        return;
    }
    let answer = recv(RESULT).0;
    outcome(e, answer);
}

/// **Drain a byte stream off the result endpoint and print it**, which is what the shell does when
/// a program's output slot is the shell's own (`Sink::Report`, the default).
///
/// This used to stop at the first newline, because the framing was a convention rather than a
/// contract and there was nothing else to stop at. Milestone 50 gave it an end: it drains until
/// `OP_EOF`, so a program that prints two lines prints two lines, and the endpoint is left clean for
/// the next command instead of holding a message the next `recv` would mistake for an answer.
///
/// That change is not optional. Before it, a `date` that announced end of stream would leave that
/// message queued, and the next command's single read would take it.
fn drain_text() {
    print(b"  ");
    for _ in 0..MAX_OUTPUT_CHUNKS {
        let (w0, w1, w2) = recv(RESULT);
        // Checked before decoding: init's sentinel is `u64::MAX`, whose top byte is an opcode this
        // contract does not define, so it would otherwise read as a malformed message rather than as
        // the one thing it is.
        if w0 == spawnproto::SPAWN_FAILED {
            print(b"could not spawn (init is out of memory)\n");
            return;
        }
        let mut buf = [0u8; sink_proto::INLINE_MAX];
        match sink_proto::unpack(w0, w1, w2, &mut buf) {
            sink_proto::Msg::Bytes(n) => print(&buf[..n]),
            sink_proto::Msg::Eof => return,
            sink_proto::Msg::Malformed => {
                print(b"\n  (that program sent something this shell cannot read as bytes)\n");
                return;
            }
        }
    }
    print(b"\n  (output truncated: that program never said it was finished)\n");
}

// ---- the declared second stream, and where its bytes go (`2>`, DECISIONS §67) ----

/// **This shell's diagnostic endpoint**, minted once and kept for the session. `u64::MAX` until
/// something needs it, because most sessions never spawn a program that declares a second stream.
///
/// It is retyped straight out of [`BUDGET`] rather than out of a per-line region, and that is a
/// decision rather than a shortcut: a pipeline's region is destroyed to turn a dead reader into
/// `Gone`, and a stream this shell always drains to `OP_EOF` has no dead reader to signal. One
/// endpoint for the session costs one page and saves a SPLIT and a DESTROY per `date`.
static DIAG_EP: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(u64::MAX);

/// The endpoint a declaring child's second stream arrives on, minting it if this session has not
/// needed one yet. `None` when the budget cannot back it, and the caller then spawns without one:
/// a program whose diagnostic slot is empty says what it has to say in-band, which is what every
/// program did before §67.
fn diag_endpoint() -> Option<u64> {
    let held = DIAG_EP.load(core::sync::atomic::Ordering::Relaxed);
    if held != u64::MAX {
        return Some(held);
    }
    let ep = retype_endpoint(BUDGET)?;
    DIAG_EP.store(ep, core::sync::atomic::Ordering::Relaxed);
    Some(ep)
}

/// **Drain every declaring stage's second stream into `dest`, then hand it back.**
///
/// It runs **before** the output is drained, and the order is forced rather than chosen. In
/// `date | wc` the shell is not the reader of `date`'s output (`wc` is), so a shell that drained the
/// output first would be waiting on `wc`, which is waiting on `date`, which is blocked in a
/// rendezvous `SEND` on this endpoint. Diagnostics first is the only order in which nobody waits on
/// somebody who is waiting on them.
///
/// What that costs is a rule the declaring program has to keep, and it is in notes/pipes.md's BUGS:
/// **say everything you have to say before you produce anything.** `date` does, because its
/// complaints are all reasons it has no answer.
///
/// `writers` is how many stages were handed the endpoint, and the drain ends when that many have
/// announced end of stream. Each declaring stage sends exactly one `OP_EOF`, so the count is the
/// termination condition and no stage's silence can be mistaken for the line being over.
fn drain_diagnostics(dest: &mut dyn ByteOut, writers: usize) {
    let Some(ep) = diag_endpoint() else { return };
    let mut done = 0usize;
    for _ in 0..MAX_OUTPUT_CHUNKS {
        if done == writers {
            break;
        }
        let (w0, w1, w2) = recv(ep);
        let mut buf = [0u8; sink_proto::INLINE_MAX];
        match sink_proto::unpack(w0, w1, w2, &mut buf) {
            sink_proto::Msg::Bytes(n) => dest.push(&buf[..n]),
            sink_proto::Msg::Eof => done += 1,
            // init's failure sentinel arrives here too, as an `OP_EOF` it sends on this endpoint so
            // this drain can end; anything else is a program that cannot spell the contract.
            sink_proto::Msg::Malformed => done += 1,
        }
    }
    dest.finish();
}

/// **The most 16-byte chunks one program's output may take before this shell stops reading**: 64
/// KiB, and one number for every destination.
///
/// The ceiling is not a policy about how much a program may say. It is there so a program that
/// never announces end of stream costs a truncated answer instead of a prompt that never returns,
/// which is a bound on a **bug**, and a bug is not more tolerable when the bytes are going into a
/// file. There were two numbers here until milestone 40's viewer met the smaller one: printing was
/// capped at 32 chunks (512 bytes) while a `>` allowed 1024, so the same program's output was
/// silently cut at the terminal and whole in a file, and nothing said which had happened. The
/// reason on record for the split ("a file is where output goes when there is too much of it to
/// read") is a policy about length, which the same comment disclaimed in its next sentence.
///
/// 64 KiB rather than 16, because the number's whole job is to be larger than anything a working
/// program says and smaller than forever. A rendered manual page is tens of kilobytes; a few
/// seconds of serial output is the cost of hitting it, against a prompt that never comes back.
const MAX_OUTPUT_CHUNKS: usize = 4096;

/// Report what the spawned program did, in terms of the grant it was given.
fn outcome(e: Endowment, answer: u64) {
    // **The only failure a spawned child reports back today**, which is why the sweep's stop rule
    // is honest about its reach: there is no exit status on this path, so a child that ran and did
    // the wrong thing looks to a sweep exactly like one that succeeded. See notes/glob-grant.md.
    if answer == spawnproto::SPAWN_FAILED {
        failed();
    }
    swish::write_outcome(&e, answer, &mut print);
}

/// Print the shell's whole endowment, or, with a tail, preview what that command would grant. This
/// is the introspection that makes "reading one literal tells you a process's authority" real.
///
/// The preview is [`swish::write_caps`], and it moved there because **nothing about it needs a
/// capability**: it renders what a line *would* grant, so it is the one command in this shell whose
/// entire meaning is checkable without a machine to run it on. What is left here is the directory
/// read a pattern needs and the terminal to print to.
fn caps(nav: &mut Nav, tail: &[u8]) {
    let holdings = holdings(nav);
    // The clock row is a *slot number*, and it comes from what this shell was told rather than from
    // a constant, for [`CLOCK_SLOT`]'s reason: printing a number this shell did not get would make
    // the one table that claims to be a complete reading into a story.
    let clock = match CLOCK_SLOT.load(core::sync::atomic::Ordering::Relaxed) {
        NO_CLOCK => None,
        slot => Some(slot),
    };
    swish::write_caps(
        tail,
        SH_BUDGET_PAGES,
        holdings,
        clock,
        &mut |token| nav.expand(token),
        &mut print,
    );
}

/// Carve `pages` off our own untyped budget (slot 3) into a delegatable child untyped. `None` when
/// the budget is exhausted.
fn untyped_split(pages: u64) -> Option<u64> {
    // SAFETY: `svc`/`ecall`; the kernel checks WRITE on the untyped and returns a negative error
    // (OutOfMemory) when the budget cannot back `pages`.
    let r = unsafe { invoke(BUDGET, abi::untyped::SPLIT, pages, 0, 0) };
    if r < 0 { None } else { Some(r as u64) }
}

// ---- the two-tier interrupt path (DECISIONS §24) ----

// ---- the operators: `>`, `<` and `|` (milestone 50, notes/pipes.md) ----

/// Pages the untyped region behind one pipeline holds. The endpoints themselves are page-resident
/// objects retyped out of it (one page each), and the region exists so the shell can **destroy** it
/// when the pipeline is over.
///
/// That is not tidiness. Deleting every capability naming an endpoint does not destroy the endpoint,
/// because the object lives in a page of this region; only reclaiming the region does. So a producer
/// still blocked in a `SEND` after its reader has gone would stay blocked forever, and `yes | head`
/// would hang instead of ending. Destroying the region is what turns that into `abi::Error::Gone`.
const PIPE_REGION_PAGES: u64 = 8;

/// **Run a line that has operators on it.**
///
/// The shape is: designate the redirection targets, plan every stage, and only then move anything.
/// Nothing is spawned until the whole line is known to be runnable, which matters more here than for
/// a single command: a pipeline that half-starts leaves a process blocked on a channel nobody will
/// ever write to, and this shell has no way to reach it.
fn pipeline(nav: &mut Nav, l: Line<'_>) {
    // The names on the ends, resolved against where we stand now, exactly as an operand is.
    let out = match l.output {
        Some(t) => match grant_plan::redirect_target(t, holdings(nav), true) {
            Ok(g) => Some(g),
            Err(r) => return say(Say::Cannot(r)),
        },
        None => None,
    };
    let inp = match l.input {
        Some(t) => match grant_plan::redirect_target(t, holdings(nav), false) {
            Ok(g) => Some(g),
            Err(r) => return say(Say::Cannot(r)),
        },
        None => None,
    };
    // `2>` names one destination for the **line**, because this shell mints one diagnostic endpoint
    // and hands it to every stage that declares a second stream. See notes/pipes.md.
    let diag_file = match l.diagnostics {
        Some(t) => match grant_plan::redirect_target(t, holdings(nav), true) {
            Ok(g) => Some((g, l.diagnostics_mode())),
            Err(r) => return say(Say::Cannot(r)),
        },
        None => None,
    };

    // Plan every stage. `head` is the one shape a stage may have that is not a program: a builtin
    // that produces text, at the head of the pipeline, whose bytes **the shell writes into the pipe
    // itself**. That costs no process and no new mechanism, and it is what makes `ls | wc` a thing a
    // person can type in a shell that holds a directory.
    let mut plans: [Option<Endowment>; line::MAX_STAGES] = [None; line::MAX_STAGES];
    let mut head_builtin = false;
    for (i, stage) in l.stages().iter().enumerate() {
        match grant_plan::parse(stage) {
            Command::Run(spec) => {
                let expanded = match expansion(nav, &spec) {
                    Ok(e) => e,
                    Err(Say::Cannot(r)) => return refuse(spec, r),
                    Err(said) => return say(said),
                };
                let streams = Streams {
                    sink: l.sink_for(i, out),
                    source: l.source_for(i, inp),
                    diagnostics: diag_file,
                };
                match grant_plan::plan_stage(&spec, holdings(nav), expanded, streams) {
                    Ok(e) => plans[i] = Some(e),
                    Err(r) => return refuse(spec, r),
                }
            }
            // `echo`, `ls` and `pwd` render text and grant nothing, so the shell can be their sink's
            // client. Only at the head, and only when nothing is feeding them: a builtin with an
            // input is a builtin that reads, and none of them do.
            Command::Echo(_) | Command::Ls(_) | Command::Pwd
                if i == 0 && matches!(l.source_for(0, inp), Source::None) =>
            {
                // A builtin has no manifest, so it declares no second stream, so a `2>` on it names
                // nothing. Same sentence a program that declares none gets, for the same reason.
                if diag_file.is_some() {
                    print(b"  ");
                    print(Refusal::NoDiagnosticStream.message().as_bytes());
                    print(b"\n");
                    return;
                }
                head_builtin = true;
            }
            _ => {
                print(b"  ");
                print(Refusal::NotAPipelineStage.message().as_bytes());
                print(b"\n");
                return;
            }
        }
    }

    // **The head stage's input file comes off the plan, not off the line**, and not reading it here
    // was the bug milestone 40's viewer found: `doc page.md | wc` planned correctly, spawned the
    // head with an empty input slot, where a `recv` answers `NoSuchSlot` rather than blocking, so
    // the stage counted an empty stream and reported it. A wrong answer, not a hang.
    //
    // An input operand is a trailing positional at a program that declares an input
    // (`grant_plan::plan_against_with`), which is the same rule that makes `wc report.txt` mean
    // `wc < report.txt`. Only the planner knows it, because it needs the manifest; reading it off
    // `Line` would be the shell classifying tokens the planner has already classified. `plans[0]`
    // is `None` only for a producing builtin at the head, and a builtin has no operand.
    let inp = match plans[0] {
        Some(e) => match e.source {
            Source::File(g) => Some(g),
            _ => None,
        },
        None => inp,
    };

    run_pipeline(nav, l, &plans, head_builtin, out, inp, diag_file);
}

/// Mint the pipes, open the files, spawn the stages, feed the head if the shell is the producer, and
/// deliver what comes out of the tail. Everything above this decided; this moves capabilities.
///
/// **The shell is the process behind a `>` and a `<`**, and that is milestone 50's one wiring
/// decision that is not the obvious one. See notes/pipes.md: `fs_proto` shares a single page between
/// the FS server and its client, so two client processes racing to stage bytes in it is not sound,
/// and `ls > out.txt` is precisely a line where the shell must read the filesystem *while* the
/// redirection is being written. The shell already holds the directory capability, so it is the one
/// process that can back both ends without a second session.
///
/// What the child holds is unchanged by that, which is why it costs the milestone nothing: a
/// redirected program holds an endpoint with `WRITE` and no way to ask what is behind it.
#[allow(clippy::too_many_arguments)] // one parameter per operator, and there are four of them now
fn run_pipeline(
    nav: &mut Nav,
    l: Line<'_>,
    plans: &[Option<Endowment>],
    head_builtin: bool,
    out: Option<grant_plan::FileGrant>,
    inp: Option<grant_plan::FileGrant>,
    diag_file: Option<(grant_plan::FileGrant, line::Mode)>,
) {
    let n = l.stage_count();

    // **This shell can be at one end of a chain, never both** (`grant_plan::check_chain`,
    // notes/pipes.md). It is the producer whenever the bytes going into the head come from here: a
    // `<`, an input operand, or a producing builtin. It is always the consumer, because the tail's
    // sink is this shell's result endpoint or a file this shell backs. With one wait point per
    // process and no select, that only works if some stage reads to the end before it writes.
    //
    // **Before the files are opened**, which is the whole reason this is the first thing in the
    // function: a `>` truncates, and a line that is not going to run must not have emptied a file.
    if let Err(r) = grant_plan::check_chain(inp.is_some() || head_builtin, &plans[..n]) {
        refused();
        print(b"  ");
        // Named after the first stage that answers while it reads, which is the one a person would
        // reach for `| wc` about. The sentence itself is about the *line* and says so.
        if let Some(e) = plans[..n].iter().flatten().find(|e| e.writes_while_reading) {
            print(e.prog.name().as_bytes());
            print(b": ");
        }
        print(r.message().as_bytes());
        print(b"\n");
        return;
    }

    // The files first, before any process exists. A line whose file cannot be opened must not
    // half-run, and `>` truncates here, which is where a person expects it: `date > f` empties `f`
    // even if `date` then says nothing. `>>` is the same call with the emptying left out.
    let mut sink = match out {
        Some(g) => match open_sink(nav, g, l.mode()) {
            Ok(f) => Some(f),
            Err(s) => return say(s),
        },
        None => None,
    };
    let mut source = match inp {
        Some(g) => match open_source(nav, g) {
            Ok(f) => Some(f),
            Err(s) => {
                if let Some(f) = &mut sink {
                    f.finish();
                }
                return say(s);
            }
        },
        None => None,
    };

    // **The second stream's file, opened on the same terms as the output's** (DECISIONS §67). Held
    // as an `Option` beside the sink, because a line may name both and they are two files.
    let mut diag_sink = match diag_file {
        Some((g, mode)) => match open_sink(nav, g, mode) {
            Ok(f) => Some(f),
            Err(s) => {
                if let Some(f) = &mut sink {
                    f.finish();
                }
                return say(s);
            }
        },
        None => None,
    };
    // How many stages were handed the diagnostic endpoint, which is what [`drain_diagnostics`]
    // counts end-of-stream messages against. It comes from the manifests rather than from the line:
    // a declared stream exists whether or not a `2>` named a destination for it.
    // Only the stages whose second stream this shell is backing, which is the ones a `2>` named a
    // file for. A `Diagnostics::Printed` stage writes to the terminal's sink adapter and this shell
    // never sees those bytes, which is the whole point of that component existing.
    let declarers = plans
        .iter()
        .take(n)
        .filter(|p| p.is_some_and(|e| matches!(e.diagnostics, line::Diagnostics::File(..))))
        .count();

    // **A builtin with a file on its output spawns nothing at all.** `ls > out.txt` is one process:
    // the shell reads the directory and the shell writes the file. There is no pipe to mint, no
    // stage to start, and no capability to move, because both ends are already this shell's.
    if head_builtin && n == 1 {
        if let Some(f) = &mut sink {
            feed(nav, l.stages()[0], f);
            f.report();
        }
        return;
    }

    // One region for the whole pipeline, so a line costs one SPLIT and one DESTROY however many
    // stages it has. Each joint's endpoint is a page retyped out of it.
    let Some(region) = untyped_split(PIPE_REGION_PAGES) else {
        failed();
        failed();
        print(b"  this shell's memory budget is exhausted; nothing left to grant\n");
        return;
    };
    let mut pipes = [0u64; line::MAX_STAGES];
    let mut minted = 0usize;
    // A `<` needs one more endpoint than a bare pipeline does: the head stage reads a pipe like any
    // other stage, and the shell is what writes into it. That is the input side of "the same
    // contract, read from the other end" made concrete: nothing about the head stage knows.
    let mut want = n - 1;
    if source.is_some() {
        want += 1;
    }
    while minted < want {
        match retype_endpoint(region) {
            Some(ep) => {
                pipes[minted] = ep;
                minted += 1;
            }
            None => {
                failed();
                print(b"  could not mint a pipe from this shell's budget\n");
                release_pipeline(region, &pipes[..minted]);
                return;
            }
        }
    }
    let feed_pipe = source.is_some().then(|| pipes[n - 1]);

    // Left to right, which is the order that cannot deadlock: a producer blocks in its first `SEND`
    // until its reader exists, and its reader is the next thing this loop spawns.
    for i in 0..n {
        if i == 0 && head_builtin {
            continue;
        }
        let Some(e) = plans[i] else { continue };
        let stage_sink = if i + 1 < n { Some(pipes[i]) } else { None };
        let stage_source = if i > 0 { Some(pipes[i - 1]) } else { feed_pipe };
        let stage_diag = match e.diagnostics {
            line::Diagnostics::File(..) => diag_endpoint(),
            _ => None,
        };
        if !spawn_stage(e, stage_sink, stage_source, stage_diag) {
            release_pipeline(region, &pipes[..minted]);
            return;
        }
    }

    // **Diagnostics before anything else**, because in `date | wc` this shell is not the reader of
    // `date`'s output and draining the output first would wait on `wc`, which waits on `date`, which
    // is blocked in a rendezvous send on the diagnostic endpoint. See [`drain_diagnostics`].
    if let Some(f) = &mut diag_sink {
        if declarers > 0 {
            drain_diagnostics(f, declarers);
            f.report();
        } else {
            // A `2>` on a line where nothing was spawned to write to it. Close the file rather than
            // leaving a handle open on a name the line created.
            f.finish();
        }
    }

    // The shell as the producer, in either of its two shapes. Both run after every reader exists,
    // for the same reason the loop above goes left to right.
    if head_builtin {
        let mut w = SinkWriter::new(pipes[0]);
        feed(nav, l.stages()[0], &mut w);
    }
    if let (Some(f), Some(p)) = (&mut source, feed_pipe) {
        f.stream_into(p);
    }

    // And the tail's answer, which arrives on the shell's own result endpoint whether or not the
    // line named a file: `>` is the shell putting those bytes somewhere else, not the child being
    // handed something different. A pipeline is not a special case of printing.
    match &mut sink {
        Some(f) => {
            drain_into(f);
            f.report();
        }
        None => drain_text(),
    }
    release_pipeline(region, &pipes[..minted]);
}

/// **Somewhere this shell puts bytes it produced itself.**
///
/// Two implementations, and the whole of what `>` cost: an endpoint (the next stage of a pipeline)
/// and a file. A builtin cannot tell them apart, exactly as a spawned program cannot tell what is in
/// its output slot, and for the same reason: it is handed a place to push bytes and nothing else.
trait ByteOut {
    fn push(&mut self, bytes: &[u8]);
    /// Say the stream is over. On a pipe that is `OP_EOF`; on a file it is the last write.
    fn finish(&mut self);
}

/// **Run a producing builtin with the given destination as its output**, one message at a time.
///
/// The builtins already write through a callback rather than through `print`, because the witness
/// roles have no terminal. That was done for testability and it pays off twice here: the same `echo`
/// and the same `ls` feed a pipe *and* a file with no branch in either of them.
fn feed(nav: &mut Nav, stage: &[u8], w: &mut dyn ByteOut) {
    let said = match grant_plan::parse(stage) {
        Command::Echo(text) => {
            let said = echo(nav, text, &mut |b| w.push(b));
            if matches!(said, Say::Nothing) {
                w.push(b"\n");
            }
            said
        }
        Command::Ls(path) => nav.ls(path, &mut |name, is_dir| {
            w.push(name);
            if is_dir {
                w.push(b"/");
            }
            w.push(b"\n");
        }),
        Command::Pwd => {
            if nav.dir.is_none() {
                Say::NoDirectory
            } else {
                let mut buf = [0u8; nav::RENDER_MAX];
                let k = nav.cwd.render(&mut buf);
                w.push(&buf[..k]);
                w.push(b"\n");
                Say::Nothing
            }
        }
        _ => Say::Nothing,
    };
    // End of stream whatever happened, including a refusal: the reader downstream is a process that
    // is blocked on a receive, and leaving it there because the shell had nothing to say would turn
    // a refused line into a hung prompt.
    w.finish();
    if !matches!(said, Say::Nothing) {
        say(said);
    }
}

/// A byte-at-a-time writer onto a sink endpoint, buffering into the contract's sixteen-byte
/// messages. The seam between a callback that hands over arbitrary slices and a wire that carries
/// two words at a time; `user/src/sink.rs`'s verify role is the same loop from the other side.
struct SinkWriter {
    slot: u64,
    buf: [u8; sink_proto::INLINE_MAX],
    n: usize,
    /// Set once a send has failed. Everything after is dropped rather than retried: `Gone` means the
    /// reader stopped caring, and there is nothing to report it to.
    stopped: bool,
}

impl SinkWriter {
    fn new(slot: u64) -> Self {
        SinkWriter {
            slot,
            buf: [0; sink_proto::INLINE_MAX],
            n: 0,
            stopped: false,
        }
    }

    fn flush(&mut self) {
        if self.n == 0 || self.stopped {
            return;
        }
        let (w0, w1, w2, _) = sink_proto::pack(&self.buf[..self.n]);
        self.n = 0;
        if !matches!(
            sink_proto::classify(send(self.slot, w0, w1, w2)),
            sink_proto::Sent::Ok
        ) {
            self.stopped = true;
        }
    }
}

impl ByteOut for SinkWriter {
    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.stopped {
                return;
            }
            self.buf[self.n] = b;
            self.n += 1;
            if self.n == self.buf.len() {
                self.flush();
            }
        }
    }

    fn finish(&mut self) {
        self.flush();
        if !self.stopped {
            send(self.slot, sink_proto::eof(), 0, 0);
        }
    }
}

// ---- the file behind a `>` and a `<`, which this shell serves itself (milestone 50) ----

/// How many bytes of a file this shell stages in the page it shares with the FS server before it
/// makes a request. Small on purpose: the shell runs on the four stack pages init maps it, and the
/// page itself is sixteen times this. A larger buffer would buy fewer FS round trips and cost the
/// one resource this program is actually short of.
const FILE_CHUNK: usize = 256;

/// **Walk to the directory a resolved grant records**, from the root of what this shell holds.
///
/// The grant carries a position rather than a token (`grant_plan::designate` resolved the name once, at
/// plan time), so this re-opens that position rather than re-reading the line. Nothing here consults
/// where the shell is standing *now*, which is what makes a `cd` between planning and running unable
/// to change what a redirection means.
///
/// Returns the handle plus the capabilities opened to reach it, which the caller closes.
fn open_at(nav: &Nav, at: Cwd, tmp: &mut [u64; nav::MAX_DEPTH]) -> Result<(u64, usize), Say> {
    let mut handle = fs::ROOT;
    let mut n = 0usize;
    for level in 0..at.depth() {
        let r = nav.name_call(fs::OPENDIR, handle, at.component(level), nav.rights);
        if r < 0 {
            for h in tmp.iter().take(n) {
                nav.close(*h);
            }
            return Err(Say::Failed(-r as i32));
        }
        tmp[n] = r as u64;
        n += 1;
        handle = r as u64;
    }
    Ok((handle, n))
}

/// **Open the file behind a `>` or a `>>`**, creating it if it is not there.
///
/// `CREATE` is create, not create-or-open (DECISIONS §27), so the fallback is explicit and is the
/// same one `user/src/sink.rs` makes: on any refusal, open the existing name. What happens next is
/// the whole of `>` versus `>>`, and it happens **here, in the shell**, because the shell is what
/// backs the file (DECISIONS §55). The child is wired identically either way and has no message it
/// could send to find out which.
///
/// - `>` ([`line::Mode::Truncate`]) truncates, and does it **before the command runs**, which is
///   why `ls > out.txt` reports one more name than the `ls` before it did.
/// - `>>` ([`line::Mode::Append`]) asks the file how big it is and starts writing there. `FSTAT`
///   rather than a seek flag, because [`FileOut`] already carries its own running offset: every
///   write it makes names an absolute position, so "append" is an initial value and not a mode the
///   filesystem has to hold.
fn open_sink(nav: &mut Nav, g: grant_plan::FileGrant, mode: line::Mode) -> Result<FileOut, Say> {
    let mut tmp = [0u64; nav::MAX_DEPTH];
    let (dir_handle, opened) = open_at(nav, g.dir, &mut tmp)?;
    let name = g.name.as_bytes();
    let dir = nav.dir.unwrap_or(DIR);
    let mut handle = nav.name_call(fs::CREATE, dir_handle, name, 0);
    // A file this shell just created is empty, so neither operator has anything to do to it. Only
    // the name that was already there needs a decision.
    let mut off = 0u64;
    if handle < 0 {
        handle = nav.name_call(fs::OPEN, dir_handle, name, 0);
        if handle >= 0 {
            let r = match mode {
                line::Mode::Truncate => {
                    call(dir, fs::req(fs::TRUNCATE, handle as u64, 0), 0).0 as i64
                }
                line::Mode::Append => {
                    let size = call(dir, fs::req(fs::FSTAT, handle as u64, 0), 0).0 as i64;
                    if size >= 0 {
                        off = size as u64;
                    }
                    size
                }
            };
            if r < 0 {
                nav.close(handle as u64);
                handle = r;
            }
        }
    }
    for h in tmp.iter().take(opened) {
        nav.close(*h);
    }
    if handle < 0 {
        return Err(Say::Failed(-handle as i32));
    }
    Ok(FileOut {
        dir,
        handle: handle as u64,
        off,
        buf: [0; FILE_CHUNK],
        n: 0,
        failed: 0,
        short: false,
    })
}

/// **Open the file behind a `<`.** No create and no truncate: reading a name that is not there is a
/// refusal, not an empty stream, because a `wc < typo.txt` that reported zero would be a lie a
/// person would believe.
fn open_source(nav: &mut Nav, g: grant_plan::FileGrant) -> Result<FileIn, Say> {
    let mut tmp = [0u64; nav::MAX_DEPTH];
    let (dir_handle, opened) = open_at(nav, g.dir, &mut tmp)?;
    let handle = nav.name_call(fs::OPEN, dir_handle, g.name.as_bytes(), 0);
    for h in tmp.iter().take(opened) {
        nav.close(*h);
    }
    if handle < 0 {
        return Err(Say::Failed(-handle as i32));
    }
    Ok(FileIn {
        dir: nav.dir.unwrap_or(DIR),
        handle: handle as u64,
    })
}

/// A file this shell is appending into, on behalf of a `>`.
///
/// It holds an open handle and a running offset, and buffers into [`FILE_CHUNK`] before each write,
/// because the FS contract's unit is a page and the sink contract's is sixteen bytes: writing every
/// message straight through would cost a filesystem round trip per sixteen bytes.
struct FileOut {
    /// The slot the directory capability lives in. A `u64` rather than a borrow of [`Nav`], so a
    /// builtin that borrows the shell's navigator can still write through this.
    dir: u64,
    handle: u64,
    off: u64,
    buf: [u8; FILE_CHUNK],
    n: usize,
    /// The errno of the first refused write, or 0. Kept rather than printed at once: a producer
    /// mid-listing has nothing to do with a failure, and reporting once at the end is what a person
    /// can read.
    failed: i32,
    /// A write the server accepted and did not finish. Its own field rather than an errno, because
    /// there is no errno for it: the request did not fail, it was answered with a smaller number
    /// than it asked for, and saying `EIO` would be inventing a fact.
    short: bool,
}

impl FileOut {
    fn flush(&mut self) {
        if self.n == 0 || self.failed != 0 || self.short {
            self.n = 0;
            return;
        }
        put_page(&self.buf[..self.n]);
        let r = call(
            self.dir,
            fs::req(fs::WRITE, self.handle, self.n as u64),
            self.off,
        )
        .0 as i64;
        // A lost byte, either way, and a `>` that said nothing about it would be certifying a file
        // it did not write.
        if r < 0 {
            self.failed = -r as i32;
        } else if r != self.n as i64 {
            self.short = true;
        }
        self.off += self.n as u64;
        self.n = 0;
    }

    /// Say what happened, once, after the whole stream is through. Silence on success, because that
    /// is what `>` says everywhere else and because the bytes went into a file rather than onto the
    /// screen precisely so they would not be printed.
    fn report(&self) {
        if self.failed != 0 {
            say(Say::Failed(self.failed));
        } else if self.short {
            failed();
            print(b"  the filesystem took fewer bytes than were written; that file is short\n");
        }
    }
}

impl ByteOut for FileOut {
    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.buf[self.n] = b;
            self.n += 1;
            if self.n == self.buf.len() {
                self.flush();
            }
        }
    }

    fn finish(&mut self) {
        self.flush();
        call(self.dir, fs::req(fs::CLOSE, self.handle, 0), 0);
    }
}

/// A file this shell is reading out, on behalf of a `<`.
struct FileIn {
    dir: u64,
    handle: u64,
}

impl FileIn {
    /// **Stream the file over the sink contract**, which is the whole of `<`: the head stage holds
    /// the read end of an endpoint and receives `OP_BYTES` until `OP_EOF`, exactly as it would from
    /// a program on the left of a `|`. Nothing about the file reaches it.
    fn stream_into(&mut self, pipe: u64) {
        let mut w = SinkWriter::new(pipe);
        let mut off = 0u64;
        let mut buf = [0u8; FILE_CHUNK];
        loop {
            let got = call(
                self.dir,
                fs::req(fs::READ, self.handle, FILE_CHUNK as u64),
                off,
            )
            .0 as i64;
            if got <= 0 {
                if got < 0 {
                    say(Say::Failed(-got as i32));
                }
                break;
            }
            let got = (got as usize).min(FILE_CHUNK);
            get_page(got, &mut buf);
            w.push(&buf[..got]);
            off += got as u64;
        }
        // End of stream whatever happened, for `feed`'s reason: the reader is a process blocked in a
        // receive, and a read error is not a reason to leave it there.
        w.finish();
        call(self.dir, fs::req(fs::CLOSE, self.handle, 0), 0);
    }
}

/// **Drain a byte stream off the result endpoint into a file**, which is what `>` is.
///
/// [`drain_text`] with a different destination, and deliberately not a parameterised version of it:
/// the printing one puts a two-space prefix on the answer and this one must put nothing at all in
/// the file that the program did not write.
fn drain_into(f: &mut FileOut) {
    for _ in 0..MAX_OUTPUT_CHUNKS {
        let (w0, w1, w2) = recv(RESULT);
        if w0 == spawnproto::SPAWN_FAILED {
            failed();
            print(b"  could not spawn (init is out of memory)\n");
            return;
        }
        let mut buf = [0u8; sink_proto::INLINE_MAX];
        match sink_proto::unpack(w0, w1, w2, &mut buf) {
            sink_proto::Msg::Bytes(n) => f.push(&buf[..n]),
            sink_proto::Msg::Eof => {
                f.finish();
                return;
            }
            sink_proto::Msg::Malformed => {
                f.finish();
                print(b"  (that program sent something this shell cannot read as bytes)\n");
                return;
            }
        }
    }
    f.finish();
    print(b"  (output truncated: that program never said it was finished)\n");
}

/// **Direct init to build one stage**, delegating whatever the operators put in its slots.
///
/// The two delegations are the entire difference between a piped stage and an ordinary spawn, and
/// they are why `>` and `|` are one mechanism: init receives an endpoint and puts it where the
/// result endpoint would have gone. Nothing here knows or can find out what is on the other end.
///
/// Returns whether the stage started; a failure has already been printed.
fn spawn_stage(
    e: Endowment,
    sink: Option<u64>,
    source: Option<u64>,
    diagnostics: Option<u64>,
) -> bool {
    let mem_slot = if e.mem_pages > 0 {
        match untyped_split(e.mem_pages) {
            Some(slot) => Some(slot),
            None => {
                failed();
                print(b"  this shell's memory budget is exhausted; nothing left to grant\n");
                return false;
            }
        }
    } else {
        None
    };

    let wiring = spawnproto::Wiring {
        interruptible: false,
        sink: sink.is_some(),
        source: source.is_some(),
        diagnostics: diagnostics.is_some(),
        // **A stage of a pipeline is never directory-granted**, and it is the manifest that says so
        // rather than a rule here: the one program with `DirSpec::Required` writes a byte stream and
        // takes no input, so `rm x | wc` puts it at the head of a line this path runs. Delivering a
        // grant here would mean a second copy of `dir_grant`'s refusals, so the honest answer is
        // that it is not delivered and `spawn` is where a directory grant is met. See this file's
        // `dir_grant` and notes/dir-capability.md's BUGS.
        dir: false,
    };
    let (w0, w1, w2) = spawnproto::request(e.prog.id(), e.arg, e.mem_pages, wiring);
    send(SPAWN, w0, w1, w2);
    // In the protocol's order, which both sides read out of the same word. A `SEND_CAP` nobody
    // expects and a `RECV_CAP` nobody answers both deadlock, so the order is the contract.
    if let Some(slot) = sink {
        delegate(slot, abi::rights::WRITE | abi::rights::GRANT);
    }
    if let Some(slot) = source {
        delegate(slot, abi::rights::READ | abi::rights::GRANT);
    }
    if let Some(slot) = diagnostics {
        delegate(slot, abi::rights::WRITE | abi::rights::GRANT);
    }
    if let Some(slot) = mem_slot {
        delegate(slot, abi::rights::WRITE | abi::rights::GRANT);
        cap_delete(slot);
    }

    // A stage whose output was substituted owes this shell no answer, so init acks instead. Without
    // it a failed spawn would be invisible and the pipeline would wait on a producer that does not
    // exist.
    if wiring.sink && recv(RESULT).0 != spawnproto::SPAWN_OK {
        failed();
        print(b"  could not spawn (init is out of memory)\n");
        return false;
    }
    true
}

/// Give the pipeline's memory back.
///
/// **The `DESTROY` is what makes a dead reader visible to a live writer.** An endpoint is an object
/// in a page of this region, so deleting the capabilities that name it leaves it alive; reclaiming
/// the region is what turns a producer's next `SEND` into `abi::Error::Gone`. A pipeline that ended
/// with a stage still writing (the `yes | head` shape) ends here.
fn release_pipeline(region: u64, pipes: &[u64]) {
    for &p in pipes {
        cap_delete(p);
    }
    // Retried for `reclaim`'s reason: a child finishing its last instruction still counts as a live
    // thread, and DESTROY refuses while one is resident.
    reclaim(region);
    cap_delete(region);
}

/// RETYPE one page of `region` into an `Endpoint` we hold with full rights, which is what lets us
/// delegate a narrowed view of it to each end of a pipe.
fn retype_endpoint(region: u64) -> Option<u64> {
    // SAFETY: `svc`/`ecall`; the kernel checks WRITE on the untyped and returns a negative error
    // when the region cannot back another page.
    let r = unsafe {
        invoke(
            region,
            abi::untyped::RETYPE_OBJ,
            abi::objtype::ENDPOINT,
            0,
            0,
        )
    };
    if r < 0 { None } else { Some(r as u64) }
}

const PAGE: u64 = 4096;
/// Pages the construction untyped for a supervised child holds: its aspace, code, stack, and TCB.
/// The heeder and spinner are tiny; this is generous. DESTROY returns these pages to our budget.
const JOB_UNTYPED_PAGES: u64 = 32;
/// Where we map a supervised job's shared frame in our own space. It advances per job, because there
/// is no unmap syscall: each job gets a fresh window and the old mapping is simply left behind (one
/// page of address space, and one frame from our budget, is the honest per-job cost).
static SH_JOBFRAME_NEXT: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0x0000_0000_00c0_0000);

/// The terminal's `^C` count we have already accounted for. A watermark, not a per-job baseline, and
/// that distinction is load-bearing: a `^C` typed the instant after `run heeder` is counted by the
/// terminal *before* the shell finishes spawning and starts watching (the input driver runs first).
/// Diffing against a watermark carried across the session catches it anyway; a fresh baseline read at
/// watch-start would already include it and miss the interrupt. A prompt `^C` (a failed read) and a
/// finished job each advance the watermark, so neither leaks into the next job.
static CONSUMED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Run a supervised foreground job: mint its resources from our own budget, direct init to build it
/// from a region we hold, then watch it under the two-tier `^C` escalation until it stops or we tear
/// it down. This is the whole of DECISIONS §24 on the shell's side.
fn spawn_interruptible(e: Endowment) {
    // Mint the job's resources. RETYPE the shared frame first, then SPLIT the construction budget,
    // so the budget is the top of our watermark and DESTROY returns its pages cleanly (LIFO).
    let Some(job_fr) = retype_frame() else {
        failed();
        print(b"  this shell's memory budget is exhausted; nothing left to grant\n");
        return;
    };
    let Some(job_ut) = untyped_split(JOB_UNTYPED_PAGES) else {
        cap_delete(job_fr);
        failed();
        print(b"  this shell's memory budget is exhausted; nothing left to grant\n");
        return;
    };

    // Map the shared frame into our own space so we can signal the job and read its status.
    let va = SH_JOBFRAME_NEXT.fetch_add(PAGE, core::sync::atomic::Ordering::Relaxed);
    if !map_frame(job_fr, va) {
        cap_delete(job_fr);
        cap_delete(job_ut);
        failed();
        print(b"  could not map the job frame\n");
        return;
    }
    // A freshly retyped frame is zeroed, but make the shared contract explicit.
    jf_store(va, jobframe::INTERRUPT, 0);
    jf_store(va, jobframe::DONE, 0);
    jf_store(va, jobframe::STATUS, 0);
    jf_store(va, jobframe::HEARTBEAT, 0);

    // Direct init: an interruptible request, then the job untyped and the job frame, both delegated
    // WRITE|GRANT (init builds from the untyped and maps the frame). We keep our own copies: the
    // untyped to tear the job down, the frame to signal it and read it.
    let (w0, w1, w2) = spawnproto::request(
        e.prog.id(),
        e.arg,
        e.mem_pages,
        spawnproto::Wiring {
            interruptible: true,
            sink: false,
            source: false,
            // A supervised job reports through the shared frame, not through a stream, and neither
            // demonstrator declares a second one. `OutputSpec::Silent` and a diagnostic endpoint
            // would be a contradiction the manifest can already refuse.
            diagnostics: false,
            // Neither demonstrator declares a directory either, and a supervised job is built out of
            // *this shell's* untyped rather than init's pool, so there is no region a caretaker
            // could share with it (DECISIONS §92).
            dir: false,
        },
    );
    send(SPAWN, w0, w1, w2);
    send_cap(job_ut);
    send_cap(job_fr);

    // init acks once the child is running: that is the shell's go-ahead to start watching.
    if recv(RESULT).0 != spawnproto::SPAWN_OK {
        failed();
        print(b"  could not spawn (init is out of memory)\n");
        cap_delete(job_fr);
        cap_delete(job_ut);
        return;
    }
    print(b"  running ");
    print(e.prog.name().as_bytes());
    print(b" in the foreground. ^C interrupts it.\n");

    watch(va, job_ut);

    // Account for every ^C up to now, so the job's interrupts do not leak into the next one.
    CONSUMED.store(intr_count(), core::sync::atomic::Ordering::Relaxed);
    // Drop our caps. If the job was reclaimed, the untyped cap is already stale; cap_delete just
    // frees the slot.
    cap_delete(job_fr);
    cap_delete(job_ut);
}

/// Watch a running supervised job: poll its done flag and the terminal's `^C` count, drive the
/// escalation policy, and act. The busy-poll with `yield` is how one thread watches two things (the
/// job and `^C`) with only blocking primitives and no non-blocking receive (DECISIONS §24, wait A).
fn watch(va: u64, job_ut: u64) {
    // The baseline is the session watermark, not a fresh read: a ^C counted during the spawn is
    // already reflected in intr_count but not yet in CONSUMED, so diffing against CONSUMED sees it.
    let base = CONSUMED.load(core::sync::atomic::Ordering::Relaxed);
    let mut esc = Escalation::new();
    let mut fed: u64 = 0;
    loop {
        // Finished on its own (cooperatively or naturally)?
        if jf_load(va, jobframe::DONE) != 0 {
            let status = jf_load(va, jobframe::STATUS);
            let beats = jf_load(va, jobframe::HEARTBEAT);
            reclaim(job_ut);
            report_finished(status, beats);
            return;
        }
        // Fold in every ^C the terminal has seen since we started watching.
        let n = intr_count().wrapping_sub(base);
        let mut action = Action::None;
        while fed < n {
            fed += 1;
            let a = esc.on_interrupt();
            if !matches!(a, Action::None) {
                action = a; // Forcible (a second ^C) wins over Cooperative (the first)
            }
        }
        if matches!(action, Action::None) {
            action = esc.on_tick(); // the grace timeout: escalate a job that ignored the first ^C
        }
        match action {
            Action::Cooperative => {
                jf_store(va, jobframe::INTERRUPT, 1);
                print(b"  ^C: asked the job to stop.\n");
            }
            Action::Forcible => {
                forcible(job_ut);
                return;
            }
            Action::None => {}
        }
        yield_now();
    }
}

/// Report a job that stopped on its own.
fn report_finished(status: u64, beats: u64) {
    if status == jobframe::STATUS_INTERRUPTED {
        // Interrupted is a job that did not finish what it was asked to do, so `&&` must not carry
        // on past it. It is [`failed`] rather than [`refused`] because it ran: the shell agreed to
        // it, spawned it, and a person stopped it.
        failed();
        print(b"  the job caught the interrupt and stopped cleanly after ");
        print_num(beats);
        print(b" work units.\n");
    } else {
        print(b"  the job finished after ");
        print_num(beats);
        print(b" work units.\n");
    }
}

/// The forcible tier: tear the job's region down with the owner's `DESTROY`. The shell holds the
/// untyped the child was built from, so this reclaims its every object. Once the §16 amendment lands
/// (DESTROY force-kills a live resident thread), this ends even a runaway that ignored the first ^C.
fn forcible(job_ut: u64) {
    failed();
    print(b"  ^C again: tearing the job down.\n");
    if reclaim(job_ut) {
        print(b"  the job's process was torn down and its memory reclaimed.\n");
    } else {
        print(b"  teardown refused: DESTROY force-kills a live thread once the kernel amendment lands.\n");
    }
}

/// Reclaim the job's region, retrying because a cooperatively-exiting child may still be finishing
/// its last instruction (DESTROY refuses while a thread is live). Returns whether it succeeded.
fn reclaim(job_ut: u64) -> bool {
    for _ in 0..256 {
        // SAFETY: `svc`/`ecall`; DESTROY reclaims the region or refuses (a live thread, pre-amendment).
        if unsafe { invoke(job_ut, abi::untyped::DESTROY, 0, 0, 0) } == 0 {
            return true;
        }
        yield_now();
    }
    false
}

/// RETYPE one page of our budget into a Frame capability we hold. `None` when the budget is spent.
fn retype_frame() -> Option<u64> {
    // SAFETY: `svc`/`ecall`; the kernel checks WRITE on the untyped.
    let r = unsafe { invoke(BUDGET, abi::untyped::RETYPE, 0, 0, 0) };
    if r < 0 { None } else { Some(r as u64) }
}

/// Map the frame in `slot` read/write at `va` in our own space; page tables come from our budget.
fn map_frame(slot: u64, va: u64) -> bool {
    // SAFETY: `svc`/`ecall`; the kernel checks the frame cap and the address.
    unsafe { invoke(slot, abi::frame::MAP, va, 1, BUDGET) == 0 }
}

/// Delegate the capability in `slot` to init over the spawn endpoint, narrowed to WRITE|GRANT (init
/// builds from an untyped or maps a frame, and narrows further from there). We keep our own copy.
fn send_cap(slot: u64) {
    delegate(slot, abi::rights::WRITE | abi::rights::GRANT);
}

/// **Delegate one capability to init, with exactly the rights it should travel with.**
///
/// `GRANT` has to be in `rights` for the send itself to be legal, so what varies is the other half,
/// and that half is load-bearing for milestone 50: a pipe's write end goes over as `WRITE|GRANT` and
/// its read end as `READ|GRANT`, so the child at the far end of a `|` **cannot write back up its own
/// input**. A pipeline that granted both directions would be a two-way channel nobody asked for, and
/// the narrowing is what stops it rather than a convention the programs are trusted to keep.
fn delegate(slot: u64, rights: u64) {
    // SAFETY: `svc`/`ecall`; the kernel checks WRITE on the endpoint and GRANT on the delegated cap.
    unsafe {
        invoke(
            SPAWN,
            abi::endpoint::SEND_CAP,
            slot,
            rights,
            spawnproto::CAP_TAG,
        )
    };
}

/// Ask the terminal how many `^C` it has seen (a non-blocking poll; see `proto::OP_INTRCOUNT`).
fn intr_count() -> u64 {
    call(TERM, proto::req(proto::OP_INTRCOUNT, 0), 0).0
}

/// Read a word from the mapped job frame.
fn jf_load(va: u64, off: usize) -> u64 {
    // SAFETY: the job frame is mapped read/write at `va`; `off` is a valid word offset.
    unsafe { core::ptr::read_volatile((va as usize + off) as *const u64) }
}

/// Write a word to the mapped job frame.
fn jf_store(va: u64, off: usize, v: u64) {
    // SAFETY: as above; this word is the shell's to write (one writer per word, see jobframe).
    unsafe { core::ptr::write_volatile((va as usize + off) as *mut u64, v) }
}

// ---- the navigating witness (milestone 47) ----

/// Where the witness reports its bitmap (`fs_service::start_granted_dir` grants it at 1).
const REPORT: u64 = 1;

/// **A shell driven by a script instead of a keyboard**, so the builtins above can be gated on both
/// ISAs (DECISIONS §19) against a real subtree served by a real `fs_subtree_caretaker`.
///
/// It is **told nothing about which subtree it was rooted in**, beyond a run index that keeps the
/// names it creates distinct across runs sharing one image. It tries to name one file that exists
/// only in `sub` and one that exists only in `other`, and reports which it reached. So "two shells
/// with different roots cannot name each other's files" is read off the *pair* of reports rather
/// than claimed by either, and the two runs are each other's control.
///
/// The lines below are literally command lines, parsed by `grant_plan::parse` and executed by
/// [`builtin`], which is the same path the prompt takes.
fn navigate(spec: u64) -> ! {
    use fs_proto::fixture::{VERDICT, navscape as nb, tree};
    let run = fs_proto::grant::spec_len(spec) as u64;
    let mut nav = Nav::rooted(fs_proto::grant::spec_rights(spec));
    let mut v = 0u64;

    // 1. `pwd` at the start. The root of your namespace renders as `/` because it is the root of
    //    the only namespace you have.
    if pwd_is(&nav, b"/") {
        v |= nb::PWD_IS_ROOT;
    }

    // 2. **`..` at the root.** Nothing is sent: the shell holds a stack of the capabilities it
    //    descended through, and at the root there is nothing to pop. Both halves are checked, since
    //    a refusal that moved anyway would be the interesting failure.
    let said = run_line(&mut nav, b"cd ..");
    if !pwd_is(&nav, b"/") {
        v |= nb::WALKED_UP;
    } else if matches!(said, Some(Say::Refused(Refused::AtYourRoot))) {
        v |= nb::CLAMPED_AT_ROOT;
    }

    // 3. **An absolute path, rooted in this shell's own namespace** (milestone 47's namespace
    //    half). `/..` first, because it is the negative control: your root is the only root there
    //    is, so there is no level above it to name and the syntax meets the same wall `..` does.
    if matches!(
        run_line(&mut nav, b"cd /.."),
        Some(Say::Refused(Refused::AtYourRoot))
    ) && pwd_is(&nav, b"/")
    {
        v |= nb::ABSOLUTE_CLAMPED_AT_ROOT;
    }

    // 4. The two files, one from each root. Exactly one of these must open, and which one is the
    //    whole point of running this twice.
    if let Some(h) = opened(&nav, tree::INNER.as_bytes()) {
        v |= nb::REACHED_INNER;
        nav.close(h);
    }
    if let Some(h) = opened(&nav, tree::SECRET.as_bytes()) {
        v |= nb::REACHED_SECRET;
        nav.close(h);
    }

    // 4b. **The same two probes, asked with a leading slash.** This is what "an absolute path
    //     grants nothing" is measured as rather than asserted: each shell reaches exactly the file
    //     its own root contains, and a `/` rooted in a global namespace would make both reach both.
    if let Some(h) = opened_token(&mut nav, tree::ABS_INNER.as_bytes()) {
        v |= nb::ABSOLUTE_REACHED_INNER;
        nav.close(h);
    }
    if let Some(h) = opened_token(&mut nav, tree::ABS_SECRET.as_bytes()) {
        v |= nb::ABSOLUTE_REACHED_SECRET;
        nav.close(h);
    }

    // 5. `ls`. A listing is a rendering of authority, so what is in it is checked and not merely
    //    counted: a name from the other shell's root appearing here would be an escape even though
    //    nothing was opened.
    let mut saw = 0u64;
    let said = nav.ls(b"", &mut |name, _| {
        if name == tree::INNER.as_bytes() {
            saw |= nb::SAW_INNER;
        }
        if name == tree::SECRET.as_bytes() {
            saw |= nb::SAW_SECRET;
        }
    });
    if matches!(said, Say::Nothing) {
        v |= nb::LISTED;
    }
    v |= saw;

    // 6. Down one level and back. `pwd` has to follow, or the shell's idea of where it is and the
    //    capability it is resolving against have come apart.
    if matches!(run_line(&mut nav, b"cd deeper"), Some(Say::Nothing)) && pwd_is(&nav, b"/deeper") {
        v |= nb::DESCENDED;
        if matches!(run_line(&mut nav, b"cd .."), Some(Say::Nothing)) && pwd_is(&nav, b"/") {
            v |= nb::RETURNED;
        }
    }

    // 7. `mkdir`, which mints a capability and hands it straight back: making a directory is not
    //    going there.
    let dirname = run_name(tree::NAV_DIR, run);
    let mut cmd = [0u8; 32];
    if matches!(
        run_line(&mut nav, line(&mut cmd, b"mkdir ", &dirname)),
        Some(Say::Nothing)
    ) {
        v |= nb::MADE_DIR;
    }

    // 7b. **`cd /` is `pwd`'s output typed back**, which is the round trip the syntax exists for.
    //     It descends into the directory step 7 just made and comes back with a leading slash, and
    //     the middle check is what stops a shell that never moved from passing by accident. The
    //     directory has to be one this shell made, because the two subtrees this script runs in
    //     are not the same shape: only one of them ships a child directory, which is why an
    //     earlier draft of this step passed for one shell and silently proved nothing for the
    //     other.
    if matches!(
        run_line(&mut nav, line(&mut cmd, b"cd ", &dirname)),
        Some(Say::Nothing)
    ) && !pwd_is(&nav, b"/")
        && matches!(run_line(&mut nav, b"cd /"), Some(Say::Nothing))
        && pwd_is(&nav, b"/")
    {
        v |= nb::ABSOLUTE_IS_MY_ROOT;
    }

    // 8. Two files: one that stays, so the other's absence afterwards is a fact about `rm` rather
    //    than about a shell that created nothing.
    let kept = run_name(tree::NAV_KEPT, run);
    let doomed = run_name(tree::NAV_GONE, run);
    let (Some(k), Some(h)) = (
        created(&nav, name_of(&kept)),
        created(&nav, name_of(&doomed)),
    ) else {
        // Nothing else below can mean anything without them.
        send(REPORT, VERDICT, v | nb::NAVIGATION_FAILED, 0);
        exit();
    };
    v |= nb::CREATED;
    nav.close(k); // the name is what this one is for; a handle nobody closes pins the node
    put_page(tree::NAV_BODY);
    let w = call(DIR, fs::req(fs::WRITE, h, tree::NAV_BODY.len() as u64), 0).0 as i64;
    if w != tree::NAV_BODY.len() as i64 {
        v |= nb::NAVIGATION_FAILED;
    }

    // 9. **`UNLINK` while still holding the file.** The name goes; the object does not. This is the
    //    unlink/revoke split as a measurement rather than a claim.
    //
    //    Sent through the contract rather than typed as a command line, because **`rm` is a program
    //    now** (milestone 47's rmdir lane) and this witness holds no spawn channel: it is confined
    //    to a subtree by a `fs_subtree_caretaker` and nothing in its cspace names an init. So what
    //    is under test here is the verb `rm` sends, at the far end of the same caretaker chain the
    //    program runs behind, and `user/src/rm.rs`'s own guest test is what covers the program.
    if removed(&nav, fs::UNLINK, name_of(&doomed)) {
        v |= nb::UNLINKED;
        let mut buf = [0u8; 64];
        let n = call(DIR, fs::req(fs::READ, h, tree::NAV_BODY.len() as u64), 0).0 as i64;
        if n == tree::NAV_BODY.len() as i64 {
            get_page(n as usize, &mut buf);
            if &buf[..n as usize] == tree::NAV_BODY {
                v |= nb::HOLDER_KEPT_READING;
            }
        }
        // And the name really is gone, or the bit above is equally true of an unlink that did
        // nothing at all.
        if opened(&nav, name_of(&doomed)).is_none() {
            v |= nb::NAME_GONE_AFTER_UNLINK;
        }
    }
    nav.close(h);

    // 10. `UNLINK` of a **directory** is refused, and so is `RMDIR` of a directory with a name in
    //     it. Together they are the safety property this lane rests on: **no single call on this
    //     contract takes a subtree away.** Emptying one is a deliberate second step.
    if !removed(&nav, fs::UNLINK, name_of(&dirname)) {
        v |= nb::UNLINK_REFUSED_A_DIRECTORY;
    }
    let empty = run_name(tree::NAV_EMPTY, run);
    if matches!(
        run_line(&mut nav, line(&mut cmd, b"mkdir ", &empty)),
        Some(Say::Nothing)
    ) {
        // A directory with exactly one name in it: enough to make `RMDIR` refuse, and cheap to
        // take back out so the same call can be shown to work.
        let inside = nav.name_call(
            fs::CREATE,
            open_dir(&nav, name_of(&empty)).unwrap_or(fs::ROOT),
            tree::NAV_INSIDE.as_bytes(),
            0,
        );
        if let Some(d) = open_dir(&nav, name_of(&empty)) {
            if inside >= 0 {
                nav.close(inside as u64);
                if !removed(&nav, fs::RMDIR, name_of(&empty)) {
                    v |= nb::RMDIR_REFUSED_NON_EMPTY;
                }
                // Empty it by hand, exactly as `rm -r` does: bottom-up, one safe step at a time.
                let gone = nav.name_call(fs::UNLINK, d, tree::NAV_INSIDE.as_bytes(), 0) == 0;
                nav.close(d);
                if gone && removed(&nav, fs::RMDIR, name_of(&empty)) {
                    v |= nb::RMDIR_REMOVED_EMPTY;
                }
            } else {
                nav.close(d);
            }
        }
    }

    // 11. And neither removal can reach out of the root, for the reason `cd` cannot: `..` is a pop
    //     of a stack that has nothing above the root in it, so nothing is ever sent. The name is
    //     refused where it is parsed, which is why this goes through the path resolver rather than
    //     through `removed`.
    let reached_out = nav.act(b"../motd", |nav, handle, name| {
        if nav.name_call(fs::UNLINK, handle, name, 0) < 0 {
            Say::Failed(0)
        } else {
            Say::Nothing
        }
    });
    if matches!(reached_out, Say::Nothing) {
        v |= nb::WALKED_UP;
    }

    // Nothing worked at all: say so, rather than letting a shell that reaches nothing pass as a
    // shell that is perfectly confined.
    if v & (nb::REACHED_INNER | nb::REACHED_SECRET | nb::LISTED) == 0 {
        v |= nb::NAVIGATION_FAILED;
    }
    send(REPORT, VERDICT, v, 0);
    exit();
}

// ---- the globbing witness (milestone 47's globbing lane) ----

/// A small collector: what `echo` printed, or what a grant rendered to. Fixed size because this
/// program has no allocator, and generous enough that a truncation cannot make two renderings agree
/// by both running out at the same place.
struct Text {
    buf: [u8; 96],
    n: usize,
}

impl Text {
    fn new() -> Self {
        Text { buf: [0; 96], n: 0 }
    }

    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.n < self.buf.len() {
                self.buf[self.n] = b;
                self.n += 1;
            }
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.n]
    }

    /// Whether a name appears in what was collected. Used only for the stranger check, where the
    /// names are distinctive enough that a substring is the right question.
    fn holds(&self, needle: &[u8]) -> bool {
        self.as_bytes().windows(needle.len()).any(|w| w == needle)
    }
}

/// **The demonstration: `echo *.txt` prints exactly the authority `rm *.txt` would transfer.**
///
/// Both halves run here, in a shell that holds a real directory capability behind a real
/// `fs_subtree_caretaker`, over a real `READDIR`. They share the expander (that is the point of
/// there being one), and they do **not** share the plumbing: `echo` goes text → words → expand →
/// print, and the grant goes parse → positionals → expand → `grant_plan::plan` → the grant's names →
/// render. So a planner that narrowed, reordered or added to a set would show up here as a
/// disagreement.
///
/// The three refusals after it are what stop the agreement from being vacuous in the other
/// direction: a pattern that matched nothing, a pattern where one cannot mean anything, and a line
/// with no pattern in it at all.
fn globbing(spec: u64) -> ! {
    use fs_proto::fixture::{VERDICT, globscape as gb, tree};
    let mut nav = Nav::rooted(fs_proto::grant::spec_rights(spec));
    let mut v = 0u64;

    // 1. What `echo` shows.
    let mut shown = Text::new();
    let said = echo(&mut nav, tree::GLOB_PATTERN, &mut |b| shown.push(b));
    if matches!(said, Say::Nothing) && !shown.as_bytes().is_empty() {
        v |= gb::EXPANDED;
    }
    // And it is not simply everything in the directory. Both strangers are one entry away and the
    // caretaker a hop up could name either.
    if !shown.holds(tree::GLOB_MISS.as_bytes()) && !shown.holds(tree::GLOB_DIR.as_bytes()) {
        v |= gb::EXCLUDED_A_STRANGER;
    }

    // 2. What the same pattern would transfer, planned as `rm <pattern>` from the same text.
    let mut cmd = [0u8; 32];
    cmd[..3].copy_from_slice(b"rm ");
    cmd[3..3 + tree::GLOB_PATTERN.len()].copy_from_slice(tree::GLOB_PATTERN);
    let line = &cmd[..3 + tree::GLOB_PATTERN.len()];
    let mut granted = Text::new();
    if let Command::Run(rspec) = grant_plan::parse(line) {
        match expansion(&mut nav, &rspec) {
            Ok(e) => match grant_plan::plan(&rspec, holdings(&nav), e) {
                Ok(endow) => match endow.dir {
                    Some(g) => swish::write_set(&g.names, &mut |b| granted.push(b)),
                    None => v |= gb::GLOB_FAILED,
                },
                Err(_) => v |= gb::GLOB_FAILED,
            },
            Err(_) => v |= gb::GLOB_FAILED,
        }
    } else {
        v |= gb::GLOB_FAILED;
    }
    if !granted.as_bytes().is_empty() && granted.as_bytes() == shown.as_bytes() {
        v |= gb::AGREED;
    }

    // 3. A pattern that matches nothing. Refused, not printed as itself: the expansion is the grant,
    //    so an empty expansion is an empty grant.
    let mut nothing = Text::new();
    if matches!(
        echo(&mut nav, tree::GLOB_NOMATCH, &mut |b| nothing.push(b)),
        Say::Cannot(Refusal::NoMatch)
    ) && nothing.as_bytes().is_empty()
    {
        v |= gb::NO_MATCH_REFUSED;
    }

    // 4. A pattern in a leading component, refused where it is read.
    if let Command::Run(bad) = grant_plan::parse(b"rm */gone")
        && matches!(
            expansion(&mut nav, &bad),
            Err(Say::Cannot(Refusal::PatternInPath))
        )
    {
        v |= gb::PATTERN_IN_PATH_REFUSED;
    }

    // 5. And a line with no pattern in it is still text: byte for byte, spacing included.
    let mut plain = Text::new();
    if matches!(
        echo(&mut nav, tree::GLOB_PLAIN, &mut |b| plain.push(b)),
        Say::Nothing
    ) && plain.as_bytes() == tree::GLOB_PLAIN
    {
        v |= gb::TEXT_UNTOUCHED;
    }

    send(REPORT, VERDICT, v, 0);
    exit();
}

/// Run one command line through the builtins, discarding any listing.
fn run_line(nav: &mut Nav, cmd: &[u8]) -> Option<Say> {
    builtin(nav, cmd, &mut |_, _| {})
}

/// Whether `pwd` would print exactly this.
fn pwd_is(nav: &Nav, want: &[u8]) -> bool {
    let mut buf = [0u8; nav::RENDER_MAX];
    let n = nav.cwd.render(&mut buf);
    &buf[..n] == want
}

/// `OPEN` a name where we stand; the handle, or `None` if it did not resolve.
fn opened(nav: &Nav, name: &[u8]) -> Option<u64> {
    let r = nav.name_call(fs::OPEN, nav.here(), name, 0);
    if r < 0 { None } else { Some(r as u64) }
}

/// `OPEN` what a **token** designates, resolved the way the prompt resolves one: from this shell's
/// root when the token starts with `/`, from where it stands otherwise.
///
/// It goes through [`Nav::plan_path`] and [`Nav::walk_from`] rather than sending a name straight at
/// [`Nav::here`], because the claim being witnessed is about the shell's own resolver. A helper
/// that walked the path itself would prove that the *witness* can resolve a path.
fn opened_token(nav: &mut Nav, token: &[u8]) -> Option<u64> {
    let (p, _) = nav.plan_path(token).ok()?;
    let (lead, name) = p.split_last_component()?;
    let w = nav.walk_from(p.from_root(), lead).ok()?;
    let r = nav.name_call(fs::OPEN, w.handle, name, 0);
    nav.unwind(&w);
    if r < 0 { None } else { Some(r as u64) }
}

/// `CREATE` a name where we stand. There is no `touch` builtin, so this is the one thing the
/// witness does that the prompt cannot: the milestone's builtins do not include a way to make a
/// file, and inventing one to test the others would be the wrong trade.
fn created(nav: &Nav, name: &[u8]) -> Option<u64> {
    let r = nav.name_call(fs::CREATE, nav.here(), name, 0);
    if r < 0 { None } else { Some(r as u64) }
}

/// `OPENDIR` a name where we stand, asking for exactly the rights this shell holds; the handle, or
/// `None`. Used to reach *into* a directory the witness made, which is one step of the walk
/// `user/src/rm.rs` does for a living.
fn open_dir(nav: &Nav, name: &[u8]) -> Option<u64> {
    let r = nav.name_call(fs::OPENDIR, nav.here(), name, nav.rights);
    if r < 0 { None } else { Some(r as u64) }
}

/// Send one removal (`UNLINK` or `RMDIR`) for a name where we stand, and say whether it worked.
///
/// The two verbs are one helper because the property they hold up is one property: `UNLINK` refuses
/// a directory, `RMDIR` refuses a non-empty one, and a witness that used a different path for each
/// could not compare them. What is *not* here is a loop: the recursion is the `rm` program's, in its
/// own address space, holding its own attenuated grant.
fn removed(nav: &Nav, verb: u64, name: &[u8]) -> bool {
    nav.name_call(verb, nav.here(), name, 0) == 0
}

/// A fixture name with the run index appended, so runs sharing one image do not collide on `EEXIST`
/// and read it as a refusal. Fixed-size because this program has no allocator.
fn run_name(base: &str, run: u64) -> ([u8; 16], usize) {
    let mut out = [0u8; 16];
    let n = base.len().min(15);
    out[..n].copy_from_slice(&base.as_bytes()[..n]);
    out[n] = b'0' + (run % 10) as u8;
    (out, n + 1)
}

/// The bytes of a [`run_name`].
fn name_of(name: &([u8; 16], usize)) -> &[u8] {
    &name.0[..name.1]
}

/// Build one command line out of a verb and a generated name. The witness types real command lines,
/// so it has to be able to build them; the buffer is the caller's because this program has no
/// allocator and a static would be a needless piece of shared state.
fn line<'a>(buf: &'a mut [u8; 32], verb: &[u8], name: &([u8; 16], usize)) -> &'a [u8] {
    let n = verb.len();
    buf[..n].copy_from_slice(verb);
    buf[n..n + name.1].copy_from_slice(name_of(name));
    &buf[..n + name.1]
}

user_rt::panic_handler!();
