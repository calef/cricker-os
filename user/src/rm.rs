//! **`rm`: remove a name, and with `-r` the tree under it** (milestone 47, notes/rm-recursion.md).
//!
//! A **program, not a shell builtin**, and that is Unix's shape rather than a divergence from it.
//! `cd`, `pwd` and `ls` are builtins here because the shell is rebinding a capability it already
//! holds; this is a destructive loop, not a rebinding. A builtin would run with the shell's **entire
//! endowment**. A program takes an explicit attenuated grant, so `caps rm -r logs` prints the
//! subtree at risk before anything happens, and a bug in the recursion below can only reach what
//! this process was handed.
//!
//! # The recursion is in here, and that is the design
//!
//! `fs_proto` has no verb that removes a subtree. [`fs::UNLINK`] refuses a directory and
//! [`fs::RMDIR`] refuses a non-empty one, so **no single call on that contract can take a tree
//! away**. What this program does is the loop Unix does: enumerate, unlink the files, recurse into
//! the directories, remove them from the bottom up. Every step is one request the FS server runs to
//! completion, and every step needs the right for it **at that level**, so the walk stops exactly
//! where the capabilities stop rather than where a check somebody wrote remembered to look.
//!
//! An interrupted run therefore leaves a **partial tree**, with what failed reported and a non-zero
//! exit. That is what `rm(1)` does too, and it is forced here: there is no transaction spanning
//! requests, and adding one would mean the FS server holding a transaction open across receives,
//! which is exactly the property its serve loop relies on not doing.
//!
//! # Capability contract (`kernel/src/user/fs_service.rs`, `start_granted_dir`)
//!
//! - **slot 0**: the narrowed directory endpoint, `WRITE`. [`fs::ROOT`] on it is the directory that
//!   holds the operand: the shell resolves any leading path at the prompt and grants **the
//!   directory the name is in**, because taking a name away is an operation on a directory.
//! - **slot 1**: a report endpoint, `WRITE`. Diagnostics and `-v` lines as framed text, then one
//!   [`sink_proto::eof`] carrying the verdict; see [`fs_proto::fixture::rm`] and [`verdict`].
//! - **[`PAGE_VA`]**: the page shared with the FS server, where a name goes out and a listing comes
//!   back.
//!
//! **The same two slots on both wirings, and that is why the order is this way round.** The kernel's
//! `start_granted_dir` puts the narrowed endpoint at slot 0 and a report endpoint at slot 1;
//! `system_initializer`'s spawn service puts the caretaker it built at slot 0 and the shell's output
//! at slot 1, ahead of every other grant, so this program's two constants mean one thing in a guest
//! test and at the real prompt. Every other program in this tree takes its output at slot 0, and the
//! exception is recorded there rather than left for a reader to infer from two call sites.
//!
//! The name and the options ride in the three `START` argument words, packed by
//! [`fs_proto::grant`] exactly as a per-file grant's name is, so this program costs no extra frame
//! and holds nothing that names an init, a terminal, or the filesystem above its grant.
//!
//! Name: unrecorded. Introduced 2026-07-31. The Unix command's own name, which the tenet's guard
//! rail would keep anyway; the tenet also makes a short name for a typed command a choice its
//! author makes rather than a convention, so there was no rule to apply.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use fs_proto::{dir, dirent, fixture, fs, grant};
use user_rt::{call, exit, send};

/// The granted directory capability: this program's whole authority.
const DIR: u64 = 0;
/// Where the diagnostics, the `-v` lines and the verdict go.
const REPORT: u64 = 1;
/// The page shared with the FS server.
const PAGE_VA: u64 = 0x0000_0000_0060_0000;

/// `ENOENT`. Not in [`dir`]'s list because it is POSIX's oldest number and this contract answers it
/// from three different rungs; `-f` cares about exactly one of them (see [`remove`]).
const ENOENT: i32 = 2;
/// `ELOOP`, borrowed for a tree deeper than this program can walk. It is **this program's refusal
/// and not the filesystem's**, which is why it prints its own sentence for it: nothing was wrong
/// with the directory.
const ELOOP: i32 = 40;

/// How deep the walk goes. This program has no allocator, so the recursion is real stack, and each
/// level holds a listing buffer by value. Eight matches the shell's path stack (`grant_plan::nav`) and
/// is well inside the sixteen handles a `fs_subtree_caretaker` will mint. Deeper is refused rather
/// than silently leaving a tree half-removed with a zero exit status.
const MAX_DEPTH: usize = 8;

/// One round of a directory listing, decoded out of the shared page. The page is sixteen times
/// larger; a directory bigger than this buffer is read in rounds, and this program cannot hold the
/// page itself on the stack it has.
const LISTING: usize = 128;

/// The buffer [`sweep`] decodes a **whole set** out of, sized by the contract rather than chosen:
/// the widest set a grant carries, each name at its longest, as directory entries. It is one frame
/// at the top of the stack and is never held across the recursion, which is why it can be larger
/// than [`LISTING`] on a program with four stack pages.
const SET_LISTING: usize = dirent::record_len(grant::MAX_NAME) * fs_proto::nameset::MAX_NAMES;

/// The most rounds one directory takes before the walk gives up on it. A ceiling rather than a limit
/// on directories: **each round removes everything it saw and starts again at cursor 0**, because a
/// removal shifts every entry after it, so a cursor carried across removals would skip names. The
/// bound is here so a directory whose entries somehow stop going away costs a diagnostic instead of
/// a process that never exits.
const MAX_ROUNDS: usize = 64;

/// Copy `bytes` into the shared page.
fn put_page(bytes: &[u8]) {
    for (i, &b) in bytes.iter().take(fs_proto::PAGE).enumerate() {
        // SAFETY: PAGE_VA is a mapped, writable page of fs_proto::PAGE bytes.
        unsafe { core::ptr::write_volatile((PAGE_VA + i as u64) as *mut u8, b) };
    }
}

/// Copy `n` bytes out of it (a listing landed there).
fn get_page(n: usize, out: &mut [u8]) {
    for (i, b) in out.iter_mut().take(n).enumerate() {
        // SAFETY: as above; `n` is bounded by the page and by `out`.
        *b = unsafe { core::ptr::read_volatile((PAGE_VA + i as u64) as *const u8) };
    }
}

/// One request that names something: stage the name in the shared page and call. The reply is the
/// contract's `i64`, so a negative value is a negated errno.
fn name_call(verb: u64, handle: u64, name: &[u8], w1: u64) -> i64 {
    put_page(name);
    call(DIR, fs::req(verb, handle, name.len() as u64), w1).0 as i64
}

/// Give a directory handle back. A failure here is not actionable and is deliberately dropped: this
/// program is exiting shortly either way, and a handle nobody closes pins a node in a server whose
/// table outlives its clients.
fn close(handle: u64) {
    call(DIR, fs::req(fs::CLOSE, handle, 0), 0);
}

/// **Remove `name` from the directory `parent` names**, recursing if it is a directory and `-r` is
/// on. Returns the errno of the first failure, or `Ok`.
///
/// The order of the two attempts is the whole of "`rm` on a directory is a refusal, never a silent
/// escalation": the [`fs::UNLINK`] goes first, and its `EISDIR` is what a plain `rm` reports. The
/// recursive path is only reached because the option said so.
fn remove(parent: u64, name: &[u8], flags: u64, depth: usize, count: &mut u64) -> Result<(), i32> {
    let r = name_call(fs::UNLINK, parent, name, 0);
    if r == 0 {
        *count += 1;
        if flags & grant_plan::rmopt::VERBOSE != 0 {
            say(name, b"");
        }
        return Ok(());
    }
    let errno = (-r) as i32;

    // **`-f`, and exactly what it means.** `rm(1)`: "If the file does not exist, do not display a
    // diagnostic message or modify the exit status." So it suppresses both, and it suppresses them
    // for *this* case only. A permission failure on a name that exists still reports, and so does an
    // `ENOENT` from anywhere else in the walk: the `OPENDIR` below answers `ENOENT` when the
    // capability may not descend (a naming right withheld is "in this scope there is no such name",
    // by design), and swallowing that would turn a walk that could not start into a silent success.
    // That is a real hazard rather than a hypothetical one, and it is why this branch is here and
    // not around the whole function.
    if errno == ENOENT && flags & grant_plan::rmopt::FORCE != 0 {
        return Ok(());
    }
    if errno != dir::EISDIR {
        diagnose(name, errno);
        return Err(errno);
    }

    // It is a directory. Without `-r` that is the answer, and it is `rm`'s answer.
    if flags & grant_plan::rmopt::RECURSIVE == 0 {
        diagnose(name, dir::EISDIR);
        return Err(dir::EISDIR);
    }
    if depth >= MAX_DEPTH {
        diagnose(name, ELOOP);
        return Err(ELOOP);
    }

    // Ask for exactly what a recursive removal needs, which is exactly what a `-r` grant carries.
    // Asking for more would be refused (`EPERM`) rather than quietly narrowed, and asking through a
    // grant that carries less is `ENOENT` from the rung above: **a `rm` handed the narrow grant
    // cannot even learn there is a subtree there.**
    let child = name_call(fs::OPENDIR, parent, name, dir::REMOVE_TREE);
    if child < 0 {
        diagnose(name, (-child) as i32);
        return Err((-child) as i32);
    }
    let emptied = empty(child as u64, flags, depth + 1, count);
    close(child as u64);
    emptied?;

    // Bottom-up: the name goes only once nothing is behind it. A `RMDIR` before this point would
    // have been `ENOTEMPTY`, which is the contract refusing to let one call take a tree away.
    let r = name_call(fs::RMDIR, parent, name, 0);
    if r < 0 {
        diagnose(name, (-r) as i32);
        return Err((-r) as i32);
    }
    *count += 1;
    if flags & grant_plan::rmopt::VERBOSE != 0 {
        say(name, b"/");
    }
    Ok(())
}

/// Take everything out of the directory `handle` names, depth first.
///
/// Each round reads one page of entries **from cursor 0** and removes every name it decoded, then
/// starts again, because removing a name shifts the entries after it and `READDIR`'s cursor is an
/// index (notes/dir-capability.md records that caveat where the verb is defined). Restarting is the
/// cheap correct answer; carrying the cursor would skip names.
///
/// A failure does not stop the round: `rm(1)` removes what it can and reports what it cannot, so
/// the first errno is kept and the walk keeps going. What it does stop is the `RMDIR` above, which
/// would only have answered `ENOTEMPTY` anyway.
fn empty(handle: u64, flags: u64, depth: usize, count: &mut u64) -> Result<(), i32> {
    let mut first_error = None;
    for _ in 0..MAX_ROUNDS {
        let n = call(DIR, fs::req(fs::READDIR, handle, 0), 0).0 as i64;
        if n < 0 {
            // `ENUMERATE` withheld is `EPERM` here, and that is the walk stopping where the
            // capability stops: it can see the directory and not what is in it.
            let errno = (-n) as i32;
            diagnose(b"", errno);
            return Err(first_error.unwrap_or(errno));
        }
        if n == 0 {
            return match first_error {
                Some(e) => Err(e),
                None => Ok(()),
            };
        }
        // Copy the round out of the shared page before anything else uses it: every removal below
        // stages a name in that same page, so a listing read lazily out of it would be reading its
        // own successor's request.
        let n = (n as usize).min(LISTING);
        let mut buf = [0u8; LISTING];
        get_page(n, &mut buf);

        let mut progress = false;
        for (name, _) in dirent::iter(&buf[..n]) {
            match remove(handle, name, flags, depth, count) {
                Ok(()) => progress = true,
                Err(e) => first_error = first_error.or(Some(e)),
            }
        }
        if !progress {
            // Nothing in this round could be removed, so another round would read the same names.
            return Err(first_error.unwrap_or(dir::EPERM));
        }
    }
    Err(first_error.unwrap_or(ELOOP))
}

/// **Remove everything this process can see** (milestone 47's globbing lane).
///
/// `rm *.txt` grants a directory capability attenuated to the names the pattern matched, so what
/// this program was handed is a namespace whose whole content is the operand. It is told so by a
/// grant carrying no name at all ([`grant::WHOLE_NAMESPACE`]), and it learns the names by
/// **enumerating its own capability**, which reveals exactly what the command line already printed.
///
/// **One listing, no rounds**, and that is not a shortcut: a set namespace is *fixed*, so the
/// caretaker answers `READDIR` with the set whether or not a name still exists. Re-reading (which
/// is what [`empty`] must do, because removing a name shifts a real directory's entries) would hand
/// this loop the names it has already taken away and turn a finished job into a page of `ENOENT`s.
fn sweep(flags: u64, count: &mut u64) -> Result<(), i32> {
    let n = call(DIR, fs::req(fs::READDIR, fs::ROOT, 0), 0).0 as i64;
    if n < 0 {
        // A capability that may not be listed cannot say what it was granted, which is the one
        // failure this mode cannot work around.
        let errno = (-n) as i32;
        diagnose(b"", errno);
        return Err(errno);
    }
    let n = (n as usize).min(SET_LISTING);
    let mut buf = [0u8; SET_LISTING];
    get_page(n, &mut buf);

    let mut first_error = None;
    for (name, _) in dirent::iter(&buf[..n]) {
        if let Err(e) = remove(fs::ROOT, name, flags, 0, count) {
            first_error = first_error.or(Some(e));
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// **The `-v` line: the name, and nothing else.** `rm(1)`'s `-v` is "showing them as they are
/// removed", and the default is silence, which is why every call to this is behind the option.
fn say(name: &[u8], suffix: &[u8]) {
    let mut out = [0u8; 64];
    let n = pack(&mut out, &[name, suffix, b"\n"]);
    text(&out[..n]);
}

/// **A diagnostic, which is not behind an option**: a failure is a line plus a non-zero exit,
/// always. The sentence comes from [`dir::explain`], so what a human reads is chosen next to the
/// decision that chose the errno rather than reinvented here.
fn diagnose(name: &[u8], errno: i32) {
    let reason: &[u8] = if errno == ELOOP {
        // This program's own refusal, not the filesystem's: `explain` would say "the filesystem
        // refused", which would be a lie about which side gave up.
        b"deeper than rm can walk without an allocator"
    } else {
        dir::explain(errno).as_bytes()
    };
    let mut out = [0u8; 96];
    let n = if name.is_empty() {
        pack(&mut out, &[b"rm: ", reason, b"\n"])
    } else {
        pack(&mut out, &[b"rm: ", name, b": ", reason, b"\n"])
    };
    text(&out[..n]);
}

/// Concatenate `parts` into `out`, returning the length. Truncating rather than growing, because
/// this program has no allocator and a diagnostic that could not be printed is worse than one that
/// is short.
fn pack(out: &mut [u8], parts: &[&[u8]]) -> usize {
    let mut n = 0;
    for part in parts {
        for &b in *part {
            if n == out.len() {
                return n;
            }
            out[n] = b;
            n += 1;
        }
    }
    n
}

/// Send a line as framed text, 16 bytes per message: the std PAL's stdout framing, which `date`
/// shares deliberately so there is one convention for "a program printed something".
fn text(bytes: &[u8]) {
    for chunk in bytes.chunks(16) {
        let mut w1 = 0u64;
        let mut w2 = 0u64;
        for (i, &b) in chunk.iter().enumerate() {
            if i < 8 {
                w1 |= (b as u64) << (8 * i);
            } else {
                w2 |= (b as u64) << (8 * (i - 8));
            }
        }
        send(REPORT, chunk.len() as u64, w1, w2);
    }
}

/// **The last message, and it closes the stream** ([`sink_proto::eof`]) rather than inventing a
/// terminator of its own.
///
/// This program's manifest declares [`grant_plan::OutputSpec::Bytes`], which is the sink contract:
/// self-framing byte messages ending in `OP_EOF`. [`text`] always produced them, and the verdict
/// used to be `fs_proto::fixture::VERDICT`, a word that contract has no meaning for. Under the guest
/// wiring the reader was a test that knew to look for it; at the real prompt the reader is the shell,
/// which reads the same three words through `sink_proto::unpack` and would have called it malformed.
/// So `rm -rv logs | wc` had never been expressible, and the declaration and the program disagreed
/// with nothing to notice. Found while wiring milestone 31 phase 3, 2026-08-17.
///
/// The verdict is not lost: `OP_EOF` uses only the first word, so the status and the count ride in
/// the two the contract leaves free. Every reader that wants them takes them off the message that
/// ends the stream, which is also the only message that can carry a status without racing the text.
fn verdict(status: u64, count: u64) {
    send(REPORT, sink_proto::eof(), status, count);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(spec: u64, name_lo: u64, name_hi: u64) -> ! {
    let mut buf = [0u8; grant::MAX_NAME];
    let n = grant::unpack_name(name_lo, name_hi, grant::spec_len(spec), &mut buf);
    // The options ride in the spec word's mask field, the same field a caretaker's rights ride in:
    // both are "what this process was started with", and neither needs a frame. The bit order is
    // `grant_plan::rmopt`, which is the shell's own numbering, so the program and the prompt cannot
    // disagree about what `-r` means.
    let flags = grant::spec_rights(spec);

    let mut count = 0u64;
    // **A grant with no name in it means the operand is the namespace** (`grant::WHOLE_NAMESPACE`).
    // A name cannot be empty, so a length of zero cannot be one, which is what lets a set grant say
    // "everything you hold" without a second protocol. See [`sweep`].
    let removal = if n == grant::WHOLE_NAMESPACE {
        sweep(flags, &mut count)
    } else {
        remove(fs::ROOT, &buf[..n], flags, 0, &mut count)
    };
    let status = match removal {
        Ok(()) => fixture::rm::OK,
        Err(errno) => fixture::rm::status(errno),
    };
    // The verdict, last and once. Anything this run printed went out before it, so a receiver that
    // takes this as its first message knows the run printed nothing, which is `rm(1)`'s default.
    verdict(status, count);
    exit();
}

user_rt::panic_handler!();
