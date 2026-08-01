//! The capability shell, at EL0: designation is authorization (milestone 31, phase 1).
//!
//! Milestone 10 gave this program a command line; milestone 31 gives that command line a meaning it
//! does not have on Unix. A Unix child inherits your whole authority and then `open()`s whatever its
//! uid allows. Here the command line is a **grant expression**: naming a resource in a command is
//! how you grant it, and a program that names nothing gets nothing beyond the report channel every
//! spawn carries. There is no ambient authority to fall back on, so when a program needs something
//! the command did not grant, the refusal reads "you hold no such capability", not a Unix-flavored
//! EPERM. The parsing and the manifest checking are the host-tested `capsh` crate; this file is the
//! wiring that turns a checked grant into real, delegated capabilities.
//!
//! What the shell can grant, it grants from what it *holds*. The headline is `prog --mem N`, which
//! endows a program N pages of untyped **split from the shell's own budget** (slot 3) and delegated
//! to it. A bare name in a file position is the second, and it is refused here with "you hold no
//! such capability", which is a statement about this shell's cspace rather than a placeholder: the
//! boot that starts it wires no FS service, so there is no directory to narrow. The mechanism a
//! grant would use exists and is proven on both ISAs (`user/src/fwarden.rs`); see [`holdings`] and
//! notes/grant-expression.md for exactly what is left. `caps` prints the shell's whole endowment,
//! and `caps <command>` previews exactly what that command would grant, making DECISIONS §14's
//! "reading one literal tells you a process's whole authority" interactively true.
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
//! - slot 0: the terminal endpoint (CALL: OP_WRITE / OP_READLINE).
//! - slot 1: a spawn endpoint (direct init to start a program; capsh::spawnproto).
//! - slot 2: a result endpoint (receive a spawned program's answer).
//! - slot 3: an untyped budget, the memory it grants with `--mem`.
//!
//! and two pages shared with the terminal: OUT_VA (we write text and prompts) and LINE_VA
//! (completed lines arrive). No role selector; the syscall runtime comes from `user_rt`.

#![no_std]
#![no_main]

use capsh::nav::{self, Cwd, Refused, Step};
use capsh::{Action, Command, Endowment, Escalation, Prog, Refusal, RunSpec, jobframe, spawnproto};
use fs_proto::{dir, dirent, fs};
use lineedit::proto;
use user_rt::{call, cap_delete, exit, invoke, recv, send, yield_now};

// Pages shared with the terminal (must match the wiring in init).
const OUT_VA: u64 = 0x0000_0000_0060_0000; // we write; the terminal reads
const LINE_VA: u64 = 0x0000_0000_00b0_0000; // the terminal writes; we read

// Capability slots.
const TERM: u64 = 0; // CALL requests on the terminal
const SPAWN: u64 = 1; // SEND a spawn request to init
const RESULT: u64 = 2; // RECV a spawned program's answer
const BUDGET: u64 = 3; // our own untyped; SPLIT a grant off it for `--mem`

/// The budget init granted us at boot (must match sysinit / hello init_boot's SH_BUDGET_PAGES).
/// We cannot query how much remains (there is no such syscall), so `caps` prints the initial grant.
const SH_BUDGET_PAGES: u64 = 128;

/// **What this shell holds, which is what decides whether a named file can be backed.**
///
/// A per-file grant is a directory capability narrowed to one name (milestone 31 phase 2,
/// `user/src/fwarden.rs`). Narrowing needs a directory to narrow, and this shell's endowment stops
/// at slot 3: init grants it a terminal, a spawn channel, a result channel and a budget, and nothing
/// that names a filesystem, because the boot that starts this shell wires no FS service. So the
/// answer here is `false`, and a file named at a program that takes one gets the milestone's
/// headline refusal, which is **true** rather than a placeholder: this shell really does hold no
/// such capability. (No shipped program declares `FileSpec::Required` yet, so that refusal is
/// reachable from `plan_against` and not from the prompt; see notes/grant-expression.md.)
///
/// It is a function rather than a constant so that the day the boot path wires an FS service and
/// grants a directory at a slot, the one place that changes is here. The planning, the manifest
/// vocabulary, and the `caps` preview are already written against it; see notes/grant-expression.md
/// for what is left.
fn holdings(nav: &Nav) -> capsh::Holdings {
    capsh::Holdings {
        dir: nav.dir.is_some(),
        cwd: nav.cwd,
    }
}

// ---- the navigation builtins (milestone 47) ----

/// Where the page this shell shares with the FS server is mapped, **in the wiring that has one**.
///
/// The same address as [`OUT_VA`], and that is not a collision: a shell wired to a terminal has no
/// filesystem and a shell wired to a filesystem has no terminal, so the two mappings never exist in
/// one address space. Both are the first spare 4 KiB window in this program's layout, which is why
/// they landed on the same number.
const FS_VA: u64 = 0x0000_0000_0060_0000;

/// The directory capability's slot in the navigating wiring (`fs_service::start_granted_dir` grants
/// it at 0). The interactive wiring has no such slot at all, which is why [`Nav::dir`] is an
/// `Option` rather than a constant: "this shell holds no directory" is a fact about a cspace.
const DIR: u64 = 0;

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
}

/// What a builtin has to say. A value rather than a print, because the printing half belongs to the
/// interactive prompt and the navigating witness (which has no terminal) runs the same builtins.
enum Say {
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
        }
    }

    /// A shell rooted at the directory capability in [`DIR`], carrying `rights`.
    fn rooted(rights: u64) -> Self {
        Nav {
            dir: Some(DIR),
            rights,
            cwd: Cwd::root(),
            handles: [0; nav::MAX_DEPTH],
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

    /// **Resolve a lead against where we stand, without moving.**
    ///
    /// `..` is answered from the shell's own stack (a level up is a handle it already holds), and
    /// each `Down` is one `OPENDIR`, because the FS contract takes a single component per request
    /// and nothing here walks a path. The steps are validated by [`Cwd::apply`] *before* this runs,
    /// so an `Up` past the root and a path deeper than the shell tracks are already refused with
    /// nothing sent; the only failure left is the server's.
    fn walk(&mut self, steps: &[Step<'_>]) -> Result<Walk, Say> {
        let mut w = Walk {
            handle: self.here(),
            base: self.cwd.depth(),
            tmp: [0; nav::MAX_DEPTH],
            n: 0,
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
        let mut target = self.cwd;
        target.apply(p.steps()).map_err(Say::Refused)?;
        Ok((p, target))
    }

    /// **`cd`**: rebind where names resolve. An empty operand is your root, because there is no
    /// `HOME` here and the root is the one distinguished place you have: it is what you were
    /// granted.
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
        let w = match self.walk(p.steps()) {
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
            match self.walk(p.steps()) {
                Ok(w) => (w.handle, Some(w)),
                Err(s) => return s,
            }
        };

        let mut said = Say::Nothing;
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
                said = Say::Failed(-n as i32);
                break;
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
        let w = match self.walk(lead) {
            Ok(w) => w,
            Err(s) => return s,
        };
        let said = f(self, w.handle, name);
        self.unwind(&w);
        said
    }
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

/// Print through the terminal: write the text into the shared page, CALL OP_WRITE. The reply
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
fn print_num(mut v: u64) {
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
    print(&digits[i..]);
}

/// Read a command line: stage the prompt, CALL OP_READLINE, and block until the terminal has a
/// line for us. The editing (cursor keys, history, backspace) happens entirely on the far side;
/// we get the finished line in LINE_VA and its length and flags in the reply.
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
/// role **0** because `sysinit` starts this program with `(0, 0, 0)`, and it is also what any
/// unrecognized role falls through to: this program's failure mode should be a prompt.
/// **The navigating witness** (milestone 47): no terminal, a directory capability at slot 0, and a
/// report endpoint at slot 1. It runs the same builtins this file's prompt runs and reports a
/// bitmap; see [`navigate`].
const ROLE_NAVIGATE: u64 = 1;

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, arg: u64, _x2: u64) -> ! {
    match role {
        ROLE_NAVIGATE => navigate(arg),
        _ => interactive(),
    }
}

fn interactive() -> ! {
    print(b"\ncricker-os capability shell. naming a resource in a command IS granting it.\n");
    print(b"commands: help, echo <text>, caps [command], cd, pwd, ls, mkdir, rm,\n");
    print(b"          <prog> [--mem N] [arg]\n");

    // This wiring grants no directory, so the navigation builtins have nothing to name and say so.
    // The day the interactive boot wires an FS service, this line is the one that changes.
    let mut nav = Nav::empty();
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
        dispatch(&mut nav, &line[..n]);
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
    match capsh::parse(cmd) {
        // They spawn nothing and grant nothing: the shell is rebinding and reading a capability it
        // already holds, which is why these are builtins, and why the worry about an over-granted
        // listing *program* (which would hold the power to read everything it lists) does not arise.
        Command::Cd(path) => Some(nav.cd(path)),
        Command::Ls(path) => Some(nav.ls(path, each)),
        Command::Mkdir(path) => Some(nav.mkdir(path)),
        _ => None,
    }
}

/// Parse one line with `capsh` and act on it. All parsing and the manifest check are the host-tested
/// crate; this function is only IO and capability moves.
fn dispatch(nav: &mut Nav, cmd: &[u8]) {
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
    match capsh::parse(cmd) {
        Command::Empty => {}
        Command::Help => help(),
        Command::Echo(text) => {
            print(text);
            print(b"\n");
        }
        Command::Caps(tail) => caps(nav, tail),
        Command::Pwd => print_pwd(nav),
        Command::Run(spec) => run(nav, spec),
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
    let mut buf = [0u8; nav::RENDER_MAX];
    let n = nav.cwd.render(&mut buf);
    print(b"  ");
    print(&buf[..n]);
    print(b"\n");
}

/// Print what a builtin had to say. Every line is a statement about a name or a capability, never
/// about a policy: `fs_proto::dir::explain` keeps the filesystem's half next to the decision that
/// chose the errno, so this function does not get to invent a friendlier word for a refusal.
fn say(s: Say) {
    match s {
        Say::Nothing => {}
        Say::Refused(r) => {
            print(b"  ");
            print(r.message().as_bytes());
            print(b"\n");
        }
        Say::Failed(errno) => {
            print(b"  ");
            print(dir::explain(errno).as_bytes());
            print(b"\n");
        }
        Say::NoDirectory => {
            print(b"  this shell holds no directory capability; there is nothing here to name\n");
        }
        Say::NeedsAName => print(b"  name what you mean: this verb takes one\n"),
    }
}

fn help() {
    print(b"  help                    this text\n");
    print(b"  echo <text>             print <text>\n");
    print(b"  caps                    print this shell's whole endowment\n");
    print(b"  caps <command>          preview what that command would grant\n");
    print(b"  cd [path]               move inside the directory you hold ('cd' is your root)\n");
    print(b"  pwd                     where you are, relative to YOUR root\n");
    print(b"  ls [path]               list a directory you can reach\n");
    print(b"  mkdir <path>            make a directory\n");
    print(b"  rm [-rfv] <path>        a PROGRAM, granted the directory holding what you name\n");
    print(b"  worker <n>              spawn a process that returns n*n\n");
    print(b"  budgeter --mem N        grant a process N pages from this shell's budget\n");
    print(b"  date                    print the wall-clock time\n");
    print(b"  <prog> <name>           grant a process one file, and only that file\n");
    print(b"\n  naming a resource grants it; a program that names nothing can touch nothing.\n");
}

/// Resolve an invocation, then either refuse it at the prompt (a mismatch the manifest caught) or
/// spawn it, granting exactly what the command named and nothing else.
fn run(nav: &Nav, spec: RunSpec) {
    match capsh::plan(&spec, holdings(nav)) {
        Err(refusal) => refuse(spec, refusal),
        // A supervised job runs under the two-tier ^C path (milestone 24); a fast job is simply
        // spawned and waited on.
        Ok(endow) if endow.interruptible => spawn_interruptible(endow),
        Ok(endow) => spawn(endow),
    }
}

/// Print a refusal in the capability model's voice. The fixed half is `capsh`'s (host-tested so the
/// wording cannot drift); the shell supplies the program name where one helps.
fn refuse(spec: RunSpec, refusal: Refusal) {
    print(b"  ");
    // A named-but-unresolvable program, or an un-grantable resource, name the offending thing. A
    // line of nothing but flags (`--mem 16`) names no program at all, and printing an empty name
    // followed by a colon would be worse than printing the bare refusal.
    match refusal {
        Refusal::NoSuchProgram if !spec.prog.is_empty() => {
            print(spec.prog);
            print(b": ");
        }
        Refusal::NoSuchProgram => {}
        _ => {
            if let Some(p) = Prog::from_name(spec.prog) {
                print(p.name().as_bytes());
                print(b": ");
            }
        }
    }
    print(refusal.message().as_bytes());
    print(b"\n");
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
        print(
            b"  a file grant needs init to build the warden; this shell cannot deliver one yet\n",
        );
        return;
    }
    // The same rule one rung up, and `rm` is the first shipped program it applies to. A directory
    // grant is delivered by a `dwarden` built from a directory this shell holds, and the boot that
    // starts this shell wires no FS service, so `plan` has already refused with "you hold no such
    // capability" and this line is what stops a future wiring from spawning `rm` with no capability
    // at all. A silently ungranted `rm` would be the worst possible failure of this model: a program
    // told to destroy something, holding nothing, saying nothing.
    if e.dir.is_some() {
        print(b"  a directory grant needs init to build the warden; this shell cannot yet\n");
        return;
    }
    // A memory grant is carved from the shell's own untyped. If our budget is spent, say so plainly
    // rather than sending init a promise we cannot keep.
    let mem_slot = if e.mem_pages > 0 {
        match untyped_split(e.mem_pages) {
            Some(slot) => Some(slot),
            None => {
                print(b"  this shell's memory budget is exhausted; nothing left to grant\n");
                return;
            }
        }
    } else {
        None
    };

    // The request: program id, argument, page count, not interruptible (capsh::spawnproto).
    let (w0, w1, w2) = spawnproto::request(e.prog.id(), e.arg, e.mem_pages, false);
    send(SPAWN, w0, w1, w2);

    // If a budget rode along, delegate it now, narrowed to WRITE|GRANT so init can re-insert it into
    // the child (init narrows it again to WRITE there: the child spends it, it does not lend it).
    if let Some(slot) = mem_slot {
        // SAFETY: `svc`/`ecall`; the kernel checks WRITE on the endpoint and GRANT on the untyped.
        unsafe {
            invoke(
                SPAWN,
                abi::endpoint::SEND_CAP,
                slot,
                abi::rights::WRITE | abi::rights::GRANT,
                spawnproto::CAP_TAG,
            )
        };
        cap_delete(slot); // our copy is delegated; free the slot
    }

    // One reader, one word: a real program's answer, or init's spawn-failed sentinel. `date` is the
    // exception: it answers in **text**, framed the way every std program's stdout is, so its
    // messages are drained by a reader that knows that framing.
    if matches!(e.prog, Prog::Date) {
        report_text();
        return;
    }
    let answer = recv(RESULT).0;
    outcome(e, answer);
}

/// Drain one framed line of text from the result endpoint and print it.
///
/// The framing is the std PAL's stdout framing (`w0` = the byte count, `w1`|`w2` = the bytes,
/// little-endian), which `date` shares deliberately so there is one convention for "a program
/// printed something". `SEND` blocks until a receiver takes it, so stopping at the newline consumes
/// exactly the messages that line was made of and leaves the endpoint clean for the next command.
///
/// It reads one line because the programs that answer this way print one, and the shell must not
/// block the prompt forever waiting for a second that is not coming. A `date` spawned with the
/// provenance selector prints two, which is why that selector is not reachable from here until the
/// manifest can declare arity (capsh's `Prog::Date`).
fn report_text() {
    print(b"  ");
    for _ in 0..MAX_TEXT_CHUNKS {
        let (n, w1, w2) = recv(RESULT);
        if n == spawnproto::SPAWN_FAILED {
            print(b"could not spawn (init is out of memory)\n");
            return;
        }
        let n = (n as usize).min(16);
        let mut buf = [0u8; 16];
        for (i, b) in buf[..n].iter_mut().enumerate() {
            *b = if i < 8 {
                (w1 >> (8 * i)) as u8
            } else {
                (w2 >> (8 * (i - 8))) as u8
            };
        }
        print(&buf[..n]);
        if buf[..n].contains(&b'\n') {
            return;
        }
    }
    print(b"\n");
}

/// The most 16-byte chunks one reported line may take before the shell stops reading. `date`'s
/// longest line is 66 bytes (five chunks); the ceiling exists so a program that never sends its
/// newline costs one truncated line instead of a hung prompt.
const MAX_TEXT_CHUNKS: usize = 8;

/// Report what the spawned program did, in terms of the grant it was given.
fn outcome(e: Endowment, answer: u64) {
    if answer == spawnproto::SPAWN_FAILED {
        print(b"  could not spawn (init is out of memory)\n");
        return;
    }
    match e.prog {
        Prog::Worker => {
            print(b"  a process at EL0 computed ");
            print_num(e.arg);
            print(b"*");
            print_num(e.arg);
            print(b" = ");
            print_num(answer);
            print(b"\n");
        }
        Prog::Budgeter => {
            print(b"  the process mapped ");
            print_num(answer);
            print(b" pages out of the ");
            print_num(e.mem_pages);
            print(b"-page budget you granted (the rest paid for its page tables)\n");
        }
        // Supervised jobs report through the job frame and the interruptible path, not here; `date`
        // answers in text and is drained by `report_text` before this is reached. `rm` is
        // unreachable from this prompt at all (a directory grant needs a warden this shell cannot
        // build, and `spawn` says so), and when it is reachable it will report the way `date` does:
        // diagnostics as text, then an exit status.
        Prog::Heeder | Prog::Spinner | Prog::Date | Prog::Rm => {}
    }
}

/// Print the shell's whole endowment, or, with a tail, preview what that command would grant. This
/// is the introspection that makes "reading one literal tells you a process's authority" real.
fn caps(nav: &Nav, tail: &[u8]) {
    let tail = capsh::trim(tail);
    if tail.is_empty() {
        print(b"  this shell holds, and nothing else:\n");
        print(b"    cap 0  endpoint  terminal   read lines, write text\n");
        print(b"    cap 1  endpoint  spawn      direct init to start a program\n");
        print(b"    cap 2  endpoint  result     read a spawned program's answer\n");
        print(b"    cap 3  untyped   ");
        print_num(SH_BUDGET_PAGES);
        print(b" pages  the memory it grants with --mem (initial)\n");
        if holdings(nav).dir {
            print(
                b"    cap 4  endpoint  directory  the files it can narrow into per-file grants\n",
            );
        } else {
            print(b"    (no directory capability: a name on the line has nothing to narrow)\n");
        }
        print(b"    (no clock: 'date' spawned from here holds no clock capability, and says so)\n");
        print(b"  it can name no devices and no other process. authority is what it holds.\n");
        return;
    }
    // Only a program invocation carries a grant to preview; `caps help` has nothing to say.
    let Command::Run(spec) = capsh::parse(tail) else {
        print(b"  caps previews a command's grant; try: caps budgeter --mem 16\n");
        return;
    };
    match capsh::plan(&spec, holdings(nav)) {
        Err(refusal) => refuse(spec, refusal),
        Ok(e) => preview(e),
    }
}

/// Print the endowment a resolved invocation would hand the new process.
fn preview(e: Endowment) {
    print(b"  ");
    print(e.prog.name().as_bytes());
    print(b" would grant the new process, and nothing else:\n");
    print(b"    cap 0  endpoint  result   report its answer back\n");
    if e.mem_pages > 0 {
        print(b"    cap 1  untyped   ");
        print_num(e.mem_pages);
        print(b" pages  split from this shell's budget\n");
    }
    // A file endowment reads as one line naming the file and the direction, because that IS the
    // whole authority: an endpoint served by a file warden that will answer for this name and no
    // other. The direction comes from the program's manifest, not from anything typed, which is why
    // it is worth printing: the line you typed plus this table is the child's complete authority.
    if let Some(g) = e.file {
        print(b"    cap 2  endpoint  file     ");
        print(g.name);
        print(if g.writable {
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
        print(b"    cap 2  endpoint  dir      ");
        let mut buf = [0u8; nav::RENDER_MAX];
        let n = g.dir.render(&mut buf);
        print(&buf[..n]);
        print(b"  (the directory holding ");
        print(g.name);
        print(b")\n");
        if g.subtree {
            print(b"           ...and everything under it: -r grants the walk\n");
        } else {
            print(b"           ...and nothing under it: no -r, so it cannot even look\n");
        }
    }
    // `date`'s authority is a read-only mapping of the clock page, and it is **init's to endow, not
    // this shell's**: it is not designated on the line and no token could designate it. Saying so
    // here is the point of `caps` being the sole visibility surface. The day the shell is granted a
    // clock to delegate, this line becomes a cap row, and until then the preview must not let a
    // reader believe the command line is the whole story.
    if matches!(e.prog, Prog::Date) {
        print(b"    (clock: this shell holds none to delegate, so it will report the time\n");
        print(b"     as unknown. the clock is init's to endow; no token on the line can.)\n");
    }
    print(b"    arg    ");
    if matches!(e.prog, Prog::Worker) {
        print_num(e.arg);
        print(b"\n");
    } else {
        print(b"(none)\n");
    }
    print(b"  reading the command is reading its whole authority.\n");
}

/// Carve `pages` off our own untyped budget (slot 3) into a delegatable child untyped. `None` when
/// the budget is exhausted.
fn untyped_split(pages: u64) -> Option<u64> {
    // SAFETY: `svc`/`ecall`; the kernel checks WRITE on the untyped and returns a negative error
    // (OutOfMemory) when the budget cannot back `pages`.
    let r = unsafe { invoke(BUDGET, abi::untyped::SPLIT, pages, 0, 0) };
    if r < 0 { None } else { Some(r as u64) }
}

// ---- the two-tier interrupt path (milestone 24, DECISIONS §24) ----

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
    let job_fr = match retype_frame() {
        Some(s) => s,
        None => {
            print(b"  this shell's memory budget is exhausted; nothing left to grant\n");
            return;
        }
    };
    let job_ut = match untyped_split(JOB_UNTYPED_PAGES) {
        Some(s) => s,
        None => {
            cap_delete(job_fr);
            print(b"  this shell's memory budget is exhausted; nothing left to grant\n");
            return;
        }
    };

    // Map the shared frame into our own space so we can signal the job and read its status.
    let va = SH_JOBFRAME_NEXT.fetch_add(PAGE, core::sync::atomic::Ordering::Relaxed);
    if !map_frame(job_fr, va) {
        cap_delete(job_fr);
        cap_delete(job_ut);
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
    let (w0, w1, w2) = spawnproto::request(e.prog.id(), e.arg, e.mem_pages, true);
    send(SPAWN, w0, w1, w2);
    send_cap(job_ut);
    send_cap(job_fr);

    // init acks once the child is running: that is the shell's go-ahead to start watching.
    if recv(RESULT).0 != spawnproto::SPAWN_OK {
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
    // SAFETY: `svc`/`ecall`; the kernel checks WRITE on the endpoint and GRANT on the delegated cap.
    unsafe {
        invoke(
            SPAWN,
            abi::endpoint::SEND_CAP,
            slot,
            abi::rights::WRITE | abi::rights::GRANT,
            spawnproto::CAP_TAG,
        )
    };
}

/// Ask the terminal how many `^C` it has seen (a non-blocking poll; see proto::OP_INTRCOUNT).
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
/// ISAs (DECISIONS §19) against a real subtree served by a real `dwarden`.
///
/// It is **told nothing about which subtree it was rooted in**, beyond a run index that keeps the
/// names it creates distinct across runs sharing one image. It tries to name one file that exists
/// only in `sub` and one that exists only in `other`, and reports which it reached. So "two shells
/// with different roots cannot name each other's files" is read off the *pair* of reports rather
/// than claimed by either, and the two runs are each other's control.
///
/// The lines below are literally command lines, parsed by `capsh::parse` and executed by
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

    // 3. An absolute path. Refused as **unnameable**, with no request made: there is no namespace
    //    to root it in, and nothing consulted a permission.
    if matches!(
        run_line(&mut nav, b"cd /other"),
        Some(Say::Refused(Refused::Absolute))
    ) && pwd_is(&nav, b"/")
    {
        v |= nb::ABSOLUTE_REFUSED;
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
    //    to a subtree by a `dwarden` and nothing in its cspace names an init. So what is under test
    //    here is the verb `rm` sends, at the far end of the same warden chain the program runs
    //    behind, and `user/src/rm.rs`'s own guest test is what covers the program.
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

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem))
    };
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
