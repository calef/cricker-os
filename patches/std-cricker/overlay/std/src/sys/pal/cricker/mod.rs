//! Platform Abstraction Layer for cricker-os (milestone 27): std over the native capability ABI.
//!
//! This is the Hermit shape (a `sys` backend implemented directly on a non-POSIX ABI), not the
//! Redox shape (a libc first): there is no errno, no fd table, no `open`, no `fork` under here,
//! because the OS does not have them and std does not actually need them. What a cricker-os
//! process *does* have is a cspace populated by its parent, and this PAL binds std to the two
//! slots the std runtime contract names (see `rt`): an untyped budget that pays for the heap,
//! and an endpoint that stdout/stderr SEND to.
//!
//! `net` and `fs` are bound to their capability-granted servers (phase two: netstack's socket contract
//! and the FS service), and each answers `Unsupported` when the program was not granted the
//! capability that reaches its server. Everything the OS cannot honestly do yet (`process`,
//! `thread::spawn`, ...) keeps the shared dispatch fallback rather than pretending; those PALs bind
//! here when the servers behind them exist.
//!
//! This file lives in the cricker-os repo (patches/std-cricker) and is materialized into a
//! patched rust-src by `cargo xtask std-src`; see notes/std.md for how the sysroot is built.

#![allow(unsafe_op_in_unsafe_fn)]

pub(crate) mod abi;
// The netstack socket-contract wire format (opcodes, frame offsets), generated verbatim from
// `user/src/netproto.rs` by `cargo xtask std-src` so the numbers cannot drift from the server.
// The net PAL (`sys/net`) is a client of it.
pub(crate) mod netproto;
// The FS-service wire format (opcodes, request packing, the errno convention), generated verbatim
// from `crates/fs_proto/src/lib.rs` by the same xtask step. The fs PAL (`sys/fs`) is a client of
// it. `blk`/`fixture` in there belong to the other two parties, hence the allow.
#[allow(dead_code)]
pub(crate) mod fsproto;
pub(crate) mod rt;

use crate::io;

/// The process entry point. The kernel (or a userspace loader) enters the ELF here per the
/// native ABI (notes/abi.md §3): three free argument registers, a mapped stack, a populated
/// cspace, no libc and no argv. rustc's generated C `main` wraps the user's `fn main` in
/// `lang_start`, which runs std's rt init and catches the exit code; all this shim does is call
/// it and turn the return into `SYS_EXIT`.
///
/// The linker finds this symbol in std's rlib because the link script's `ENTRY(_start)` makes it
/// an undefined reference, the same way a libc's crt0.o gets pulled in.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, _a1: u64, _a2: u64) -> ! {
    unsafe extern "C" {
        fn main(argc: isize, argv: *const *const u8, sigpipe: u8) -> i32;
    }
    // No argv on this ABI (arguments are three registers, unused by std programs for now), and
    // sigpipe is a Unix concept passed as 0, the same as every non-Unix port.
    let code = unsafe { main(0, core::ptr::null(), 0) };
    rt::exit(code as i64)
}

// SAFETY: must be called only once during runtime initialization.
pub unsafe fn init(_argc: isize, _argv: *const *const u8, _sigpipe: u8) {
    // Nothing to do: the heap wires itself lazily to the untyped in slot 0 on first allocation,
    // and there is no signal machinery, no env, no TLS to set up.
}

// SAFETY: must be called only once during runtime cleanup.
// NOTE: this is not guaranteed to run, for example when the program aborts.
pub unsafe fn cleanup() {}

pub fn unsupported<T>() -> io::Result<T> {
    Err(unsupported_err())
}

pub fn unsupported_err() -> io::Error {
    io::Error::UNSUPPORTED_PLATFORM
}

/// Abort: take a breakpoint fault. The kernel kills the process and reports the fault on the
/// console, which is this ABI's honest version of `abort()`: loud, attributable, and no
/// unwinding (the target is panic=abort; unwinding machinery is never even linked).
pub fn abort_internal() -> ! {
    rt::abort()
}
