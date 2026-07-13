//! ARM semihosting.
//!
//! A debug channel that lets the guest ask the *host* to do things it has no
//! hardware for. We use exactly one call, `SYS_EXIT`, which asks QEMU to terminate
//! with a given process exit status. That is how `cargo test` learns whether the
//! kernel's tests passed (DECISIONS.md §7).
//!
//! It is worth seeing what this actually is: a trap instruction, an operation number
//! in a register, arguments pointed to by another register, a result returned in a
//! register. **That is a syscall**, and the kernel answering it is QEMU. Milestone 7
//! builds the other side of exactly this shape. See notes/semihosting.md.
//!
//! Requires QEMU's `-semihosting` flag.

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

    // SAFETY: `hlt #0xf000` is the aarch64 semihosting trap. With a host attached it
    // never returns.
    //
    // Without one it raises a real exception, and since VBAR_EL1 is not yet set up we
    // would jump to garbage and die silently. The halt() below would NOT be reached.
    // That is a genuine hole; milestone 2 (exception vectors) closes it. It doesn't
    // bite today only because we always run under QEMU with -semihosting.
    unsafe {
        core::arch::asm!(
            "hlt #0xf000",
            in("x0") SYS_EXIT,
            in("x1") block.as_ptr(),
        );
    }

    super::halt()
}
