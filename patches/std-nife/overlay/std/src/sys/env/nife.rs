//! `std::env`'s variables on nife (milestone 64, rank 4 of the measured gap list).
//!
//! # There is no ambient environment, and that is the design
//!
//! A Unix process inherits `environ` from whoever spawned it, and every program on the machine can
//! read it. This system has no such thing: what a process holds is what it was granted, and an
//! environment variable is not a capability. So a nife process starts with an **empty** environment
//! and nothing seeds it.
//!
//! What that buys, stated plainly so nobody reads it as an omission: `env::var("HOME")` is `None`
//! because nobody gave this program a home, not because the lookup failed. A crate reading
//! `RUST_LOG`, `TZ` or `NO_COLOR` gets the same `None` it would get on a Unix box where the variable
//! is unset, which is a case every one of them already handles.
//!
//! # Why this exists at all, given that `getenv` could just answer `None`
//!
//! Because [`env()`] could not. Without a backend, nife fell through to `sys::env::unsupported`,
//! whose `env()` is `panic!("not supported on this platform")`. So `std::env::vars()` **aborted the
//! process**, and so did anything built on it: `Command::envs`, a logger dumping its configuration,
//! `dotenvy`, any crate that filters the environment rather than asking for one name. Nothing in a
//! build failure list showed that, which is the measurement's own sting (notes/crates-io-on-nife.md)
//! in a second place: `env::vars()` compiled fine and killed the program.
//!
//! An empty iterator is the truthful answer to "what variables does this process have", and it is
//! the one answer that is never a lie here.
//!
//! # The table is real, because `set_var` is
//!
//! `set_var` and `remove_var` operate on a process-local table, and `var` reads it back. That is not
//! a courtesy: it is what `set_var` means on every platform (it changes *this* process), and a
//! program that sets a variable for a library it is about to call is doing something entirely
//! ordinary that this system has no reason to refuse. Nothing leaves the process, because there is
//! nowhere for it to go: nife has no `execve` carrying an environment, and spawn is by capability.
//!
//! # BUGS
//!
//! - **Nothing seeds the table**, so a program cannot be *given* a variable, only set its own.
//!   Milestone 47's program namespace is where an endowment would arrive from, and when it does it
//!   seeds this table at startup; the shape here does not change.
//! - **The table is per-process and dies with it.** There is no `/etc/environment`, no shell export
//!   that survives, and no way for a parent to hand one down.
//! - **`env::temp_dir` and `env::current_dir` are not this module**, and they are still refused.
//!   Both are namespace questions rather than variable ones (ranks 16 and 18), and they wait on the
//!   same `File::open` resolution fork milestone 47 owns.

use crate::ffi::{OsStr, OsString};
use crate::sync::Mutex;
use crate::{fmt, io, vec};

/// The process's variables. `Vec` rather than `HashMap` on purpose: an environment here holds what
/// the program itself put in it, which is a handful of entries, and a map would drag `RandomState`
/// (and therefore the entropy PAL) into the first `env::var` any program makes.
static ENV: Mutex<Vec<(OsString, OsString)>> = Mutex::new(Vec::new());

/// A snapshot of the process's variables, taken while the lock was held.
///
/// Snapshot rather than a live view, because std's `Env` outlives the borrow and a program is free
/// to `set_var` while iterating. Unix has the same hazard and answers it the same way.
pub struct Env {
    iter: vec::IntoIter<(OsString, OsString)>,
}

impl fmt::Debug for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter.as_slice()).finish()
    }
}

impl Iterator for Env {
    type Item = (OsString, OsString);

    fn next(&mut self) -> Option<(OsString, OsString)> {
        self.iter.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

/// Every variable this process holds. **Empty until the program sets one**, and never a panic; see
/// the module docs for why that distinction is the whole reason this file exists.
pub fn env() -> Env {
    let snapshot = ENV.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Env { iter: snapshot.into_iter() }
}

pub fn getenv(k: &OsStr) -> Option<OsString> {
    let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    guard.iter().find(|(name, _)| name == k).map(|(_, value)| value.clone())
}

/// # Safety
///
/// Same contract as every other platform's: the caller must not be racing another thread that is
/// reading the environment. Trivially satisfied today (nife programs are single-threaded, see
/// `sys/thread/nife.rs`) and stated anyway, because the day `thread::spawn` lands is the day a
/// silent assumption here would become a data race.
pub unsafe fn setenv(k: &OsStr, v: &OsStr) -> io::Result<()> {
    let mut guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    match guard.iter_mut().find(|(name, _)| name == k) {
        Some(entry) => entry.1 = v.to_owned(),
        None => guard.push((k.to_owned(), v.to_owned())),
    }
    Ok(())
}

/// # Safety
///
/// See [`setenv`].
pub unsafe fn unsetenv(k: &OsStr) -> io::Result<()> {
    let mut guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    guard.retain(|(name, _)| name != k);
    Ok(())
}
