//! ARM semihosting.
//!
//! A debug channel that lets the guest ask the *host* to do things it has no
//! hardware for. We use exactly one call, `SYS_EXIT`, which asks QEMU to terminate
//! with a given process exit status. That is how `cargo test` learns whether the
//! kernel's tests passed (DECISIONS.md §7).
//!
//! Requires QEMU's `-semihosting` flag. On real hardware with no debugger attached,
//! the `hlt` traps and nothing happens, which is why `exit` falls through to a halt
//! loop rather than pretending to diverge.

// Only the test harness and the test-mode panic handler call this today, so a normal
// `cargo build` sees it as dead. It isn't; it's just conditionally reachable.
#![allow(dead_code)]

pub const EXIT_SUCCESS: u32 = 0;
pub const EXIT_FAILURE: u32 = 1;

/// Semihosting operation number for "terminate".
const SYS_EXIT: u64 = 0x18;

/// Reason code: the application exited normally (as opposed to hitting a
/// breakpoint, running out of memory, etc).
const ADP_STOPPED_APPLICATION_EXIT: u64 = 0x2_0026;

/// Ask the host to terminate the emulator with `code` as its exit status.
///
/// `cargo test` interprets a nonzero exit status as a test failure, so this is the
/// whole reporting mechanism.
pub fn exit(code: u32) -> ! {
    // On aarch64, SYS_EXIT wants x1 to point at a two-word block:
    //   [0] = reason code
    //   [1] = exit status
    let block = [ADP_STOPPED_APPLICATION_EXIT, code as u64];

    // SAFETY: `hlt #0xf000` is the aarch64 semihosting trap. If a semihosting host
    // is attached it never returns. If one isn't, it raises an exception we don't
    // yet handle, or is ignored; either way we fall through to the halt below.
    unsafe {
        core::arch::asm!(
            "hlt #0xf000",
            in("x0") SYS_EXIT,
            in("x1") block.as_ptr(),
        );
    }

    // We only reach here if semihosting wasn't enabled. Nothing left to do.
    super::halt()
}
