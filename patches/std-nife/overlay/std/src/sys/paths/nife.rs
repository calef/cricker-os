//! `std::env`'s *path* half on nife (milestone 64): the four functions that name places, and the
//! two that take a list of them apart.
//!
//! # Why this file exists, and it is the same reason `sys/env/nife.rs` exists
//!
//! nife had no `paths` backend at all, so it fell through to `sys/paths/unsupported.rs`, and two of
//! that file's functions are not refusals:
//!
//! ```text
//! pub fn temp_dir() -> PathBuf   { panic!("no filesystem on this platform") }
//! pub fn split_paths(..)         { panic!("unsupported") }
//! ```
//!
//! So **`std::env::temp_dir()` aborted the process**, and it compiled perfectly. That is exactly the
//! shape milestone 64's second pass found in `env::vars()`, one module over, and it is the shape a
//! gap list built from `Unsupported` counts cannot see: neither call ever returns an error, so
//! neither appears as a refusal anywhere.
//!
//! It is not academic. `tempfile::NamedTempFile::new()` calls `tempfile::env::temp_dir()`, which
//! delegates straight to `std::env::temp_dir`, so every `tempfile` operation on nife died here
//! **before** reaching the `other.rs` arm that returns "operation not supported". The measurement
//! note said tempfile "builds, links, and returns an error"; it built, linked, and aborted. Eleven
//! of the fifty probe closures reach `env::temp_dir` and eight reach `split_paths`.
//!
//! # What is answered, what is refused, and the line between them
//!
//! **Refused, honestly:** `chdir` and `current_exe` return [`io::ErrorKind::Unsupported`], and
//! `home_dir` is `None`. Each of those *can* say no in its own signature, and each of them needs
//! something this process has not been given.
//!
//! **`getcwd` stopped being one of them on 2026-08-18** (milestone 47's namespace half). It
//! answers `/`, and that is not a new fiction: `sys/fs/nife.rs` accepts a leading `/` as the root
//! of *this process's* namespace, which is the directory it was granted, so the path `getcwd`
//! returns is one the filesystem half resolves. A process holding no directory capability still
//! gets `Unsupported`, from the same `reachable()` probe every `std::fs` entry point uses: naming
//! a place you hold no capability for would be this file's own recurring failure, an answer where
//! a refusal was the truth.
//!
//! **Answered, because the signature leaves no way to refuse:** `temp_dir` returns a `PathBuf` and
//! `split_paths` returns an iterator. There is no error channel in either, so "this platform has
//! none" is not expressible and something has to be named. The rule this file follows, and it is
//! the one milestone 64 keeps rediscovering: **fix the ones that abort, leave the ones that
//! refuse.**
//!
//! # `temp_dir`, and why it was not the namespace fork in disguise
//!
//! `TMPDIR` first, exactly as `sys/paths/unix.rs` does, then `/` as the fallback.
//!
//! The variable comes first for a reason beyond parity: it is the seam milestone 47's namespace
//! arrives through. Nothing seeds a nife process's environment today (see `sys/env/nife.rs`), so
//! the lookup misses; the day a program can be *given* a variable, `TMPDIR` steers this with no
//! change to this file.
//!
//! The fallback is `/`, which is where `getcwd` says you are, and the two are the same place:
//! `sys/fs/nife.rs`'s `one_name` decided that *"./motd is motd: the current directory IS the
//! granted one"*, and the namespace half added that `/motd` is `motd` too. A process holds one
//! directory capability and that is the whole of its authority over files, so the only place a
//! temporary file can go is the place every other file goes. It was `.` until 2026-08-18, which
//! named the same directory in the only spelling that then existed.
//!
//! `/tmp` was the other candidate and it still loses, though the reason has narrowed. It no longer
//! *refuses*: since the namespace half, `/tmp/x` is `tmp/x` under the granted directory. That is
//! the problem. It names a subdirectory nothing creates, so every temporary file fails with
//! `NotFound` instead of landing somewhere, and it puts a Unix fiction in a string a program may
//! print. `/` names the one directory that is certainly there.
//!
//! # BUGS
//!
//! - **A temporary file lands in the program's granted directory, beside its real files.** There is
//!   no separate scratch space, because there is no second directory to grant one from. A caller
//!   that needs isolation should be given a directory of its own and use the `_in` variants
//!   (`tempfile_in`, `tempdir_in`), which take the directory rather than asking for one.
//! - **`tempfile` still does not work**, and removing this panic did not make it. Its platform
//!   ladder has no nife arm, so it selects `other.rs`, whose six functions all return "operation
//!   not supported on this platform". What changed is that the failure is now that crate's error
//!   instead of this platform's abort.
//! - **`chdir` refuses, so a `std` program cannot move.** `current_dir` answers `/` and always
//!   will: a process holds one directory capability, fixed at spawn, and there is no second place
//!   for it to be. Moving would mean this module holding a descent handle as mutable process state
//!   and resolving every relative name against it, which is buildable (it is the shell's stack, one
//!   level down) and is not built. A program that wants to work in a subdirectory should open it
//!   and use the `_in`/`Dir` forms, exactly as one that wants an isolated scratch space should.
//!   The asymmetry the previous version of this entry recorded, `getcwd` refusing while `temp_dir`
//!   answered, is gone: both name `/` and both come from the same place.
//! - **`split_paths` splits on `:` with no escaping**, so a path containing a colon cannot be
//!   carried in a list. Every Unix has the same hole and `join_paths` reports it as an error rather
//!   than producing a list that reads back wrong.

use crate::ffi::{OsStr, OsString};
use crate::path::{self, PathBuf};
use crate::sys::pal::unsupported;
use crate::{fmt, io};

/// The byte that separates entries in a path list. `:`, because nife's path separator is `/`
/// (`sys/path/mod.rs` routes everything that is not Windows-shaped to its `unix` module) and a
/// list separator has to be a character a path cannot contain.
const PATH_SEPARATOR: u8 = b':';

/// `/`: the root of this process's own namespace, which is the directory it was granted.
///
/// Not an ambient namespace and not a lie: `sys/fs/nife.rs` resolves a leading `/` against exactly
/// that directory, so a program that joins onto this path opens a file it holds a capability for,
/// and one that joins its way upwards gets `InvalidFilename` rather than somebody else's file.
///
/// A process that holds no directory capability is `Unsupported`, the same answer all of
/// `std::fs` gives it. There is a difference between "you are at your root" and "you have no
/// root", and a `PathBuf` cannot carry it.
pub fn getcwd() -> io::Result<PathBuf> {
    if crate::sys::fs::nife::reachable() {
        Ok(PathBuf::from("/"))
    } else {
        unsupported()
    }
}

/// Refused: a process's directory capability is fixed at spawn, so `/` is the only place it can
/// be. See the `BUGS` note on what moving would take.
pub fn chdir(_p: &path::Path) -> io::Result<()> {
    unsupported()
}

/// Refused: nothing tells a nife process the path it was loaded from. The loader maps an ELF out
/// of the initrd and the program never learns a name for itself.
pub fn current_exe() -> io::Result<PathBuf> {
    unsupported()
}

/// `None`: nobody gave this program a home. Not a failure, an absence, and the same answer a Unix
/// box gives when `HOME` is unset.
pub fn home_dir() -> Option<PathBuf> {
    None
}

/// Where a temporary file goes. `TMPDIR` if the program set one, otherwise the directory it holds,
/// spelled the way [`getcwd`] spells it.
///
/// **This used to be `panic!("no filesystem on this platform")`**, which killed any program that
/// asked. See the module docs for why the answer is `/` and not `/tmp`.
pub fn temp_dir() -> PathBuf {
    crate::env::var_os("TMPDIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The iterator over a `PATH`-shaped list. A borrowed byte slice and a cursor: nothing about
/// splitting a list on a separator is platform-specific, which is why the old `panic!` was
/// indefensible rather than merely unimplemented.
pub struct SplitPaths<'a> {
    /// The bytes not yet yielded, or `None` once the last entry has been. `Some(b"")` and `None`
    /// are different states on purpose: `"a:"` has a trailing empty entry and `""` has one entry,
    /// which is what every Unix does and what `join_paths` round-trips against.
    rest: Option<&'a [u8]>,
}

pub fn split_paths(unparsed: &OsStr) -> SplitPaths<'_> {
    SplitPaths { rest: Some(unparsed.as_encoded_bytes()) }
}

impl<'a> Iterator for SplitPaths<'a> {
    type Item = PathBuf;

    fn next(&mut self) -> Option<PathBuf> {
        let rest = self.rest?;
        let (head, tail) = match rest.iter().position(|&b| b == PATH_SEPARATOR) {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        self.rest = tail;
        // SAFETY: `OsStr::from_encoded_bytes_unchecked` requires the bytes to have come from
        // `as_encoded_bytes` and to be split on a boundary its encoding permits. Both hold: the
        // slice is a subslice of one such buffer, and the only place it is cut is at an ASCII
        // `:`, which is a character boundary in every encoding `OsStr` uses.
        let head = unsafe { OsStr::from_encoded_bytes_unchecked(head) };
        Some(PathBuf::from(head.to_os_string()))
    }
}

/// An entry contained the separator, so joining it would produce a list that reads back as two
/// entries. The same failure Unix reports, for the same reason.
#[derive(Debug)]
pub struct JoinPathsError;

pub fn join_paths<I, T>(paths: I) -> Result<OsString, JoinPathsError>
where
    I: Iterator<Item = T>,
    T: AsRef<OsStr>,
{
    let mut joined = Vec::new();
    for (i, path) in paths.enumerate() {
        let path = path.as_ref().as_encoded_bytes();
        if i > 0 {
            joined.push(PATH_SEPARATOR);
        }
        if path.contains(&PATH_SEPARATOR) {
            return Err(JoinPathsError);
        }
        joined.extend_from_slice(path);
    }
    // SAFETY: every byte came from `as_encoded_bytes`, and the only byte inserted between them is
    // an ASCII `:`, which no encoding `OsStr` uses treats as part of a multi-byte sequence.
    Ok(unsafe { OsString::from_encoded_bytes_unchecked(joined) })
}

impl fmt::Display for JoinPathsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "path segment contains separator `{}`", char::from(PATH_SEPARATOR))
    }
}

impl crate::error::Error for JoinPathsError {}
