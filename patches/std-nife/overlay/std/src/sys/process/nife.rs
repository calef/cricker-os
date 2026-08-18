//! `std::process::id()` on nife (milestone 64). One function, and it exists to stop an abort.
//!
//! # The defect
//!
//! nife has no `process` backend, so it falls through to `sys/process/unsupported.rs`. Everything
//! in that file is an honest refusal (`Command::spawn` and friends return
//! [`io::ErrorKind::Unsupported`]) except its last function:
//!
//! ```text
//! pub fn getpid() -> u32 { panic!("no pids on this platform") }
//! ```
//!
//! So **`std::process::id()` killed the program**, and it compiled. Five of milestone 64's fifty
//! probe closures reach it, `gix-tempfile` and `gix-utils` among them, which puts it on milestone
//! 99's path: `gix-tempfile` is what `gix-lock` commits a ref through.
//!
//! Nothing about that panic showed up in the measurement's gap list, because the gap list is built
//! from functions that answer `Unsupported` and this one never answers at all. It is the third
//! instance of the same shape after `env::vars()` and `env::temp_dir()`.
//!
//! # Why the answer is `0`, which is a decision rather than a placeholder
//!
//! **There is no process identifier on this system to report.** The syscall surface has four calls
//! (`crates/abi`: `SYS_EXIT`, `SYS_YIELD`, `SYS_INVOKE`, `SYS_CAP_DELETE`) and none of them names
//! the caller; a process here is identified by what it holds, not by a number in a global table.
//! Inventing one would be a Unix fiction over a capability model, which is the failure this PAL
//! declines elsewhere (`std::os::unix`'s `geteuid`, `sys/fs/nife.rs`'s choice of
//! `InvalidFilename` over `PermissionDenied`).
//!
//! std's signature is `fn getpid() -> u32`, so "there is none" cannot be returned. Given that, `0`
//! is the one number that cannot be mistaken for a real one: every Unix reserves pid 0 for the
//! kernel and never assigns it to a user process, so a caller that logs or compares it sees an
//! obviously-not-a-process value rather than a plausible lie.
//!
//! **And the call sites make it the right answer rather than merely the least wrong one**, which
//! is worth stating because a constant pid looks dangerous. Read against the measurement's
//! closures, every reachable use is a *fork* check: `gix-tempfile`'s `forksafe.rs` records
//! `owning_process_id` and its registry compares `current_pid`, both so that cleanup runs only in
//! the process that created the file. nife has no `fork`, so the comparison must always match, and
//! a constant is what makes it match. The one use that wants entropy (`gix-utils`' `rng.rs`) mixes
//! the pid into a *fallback* seed taken only when the OS entropy source is missing, and nife's is
//! not missing (`std::random::SystemRng`, milestone 56).
//!
//! # BUGS
//!
//! - **Every nife process reports 0**, so any scheme that derives cross-process uniqueness from the
//!   pid collides. No call site in the fifty measured closures does that, and one that did would be
//!   broken here in a way no error reports. If a real per-process identity is ever wanted, it is a
//!   syscall-surface decision (DECISIONS §10, §16) and not a PAL one.
//! - **Only `getpid` is nife's**; every other item in `std::process` comes from the shared
//!   `unsupported` backend, so `Command::spawn` still refuses. Spawning on this system is by
//!   capability, which is a design fork rather than a porting task, and the measurement records
//!   `gix-command` and `gix-credentials` as the crates that will force it.

/// This process's identifier, which does not exist, reported as `0`. See the module docs.
pub fn getpid() -> u32 {
    0
}
