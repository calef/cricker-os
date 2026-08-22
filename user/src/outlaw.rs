//! **The outlaw: the two programs the privilege-boundary tests need, and nothing else wants.**
//!
//! Both of these used to be aarch64 machine code assembled inline in `kernel/src/user.rs` (the
//! `user_program!` macro, milestone 7a). That was fine while there was one instruction set. It
//! stopped being fine when there were two: porting `kernel::user::tests` to RISC-V that way meant
//! hand-assembling every one of them a second time, for no new coverage of anything portable.
//!
//! So they are a real ELF now, built by the ordinary toolchain for whichever target is being built,
//! and the kernel loads them the way it loads any other program. The tests that drive them are the
//! same tests on both ISAs.
//!
//! It is deliberately **not** a role of `hello`. hello is the system's init on aarch64 and carries
//! the roles a running system uses; these two exist only to be killed or to be counted, and a
//! process that reads a forbidden address on purpose has no business sharing an image with init.
//! Keeping it separate also keeps it tiny, which the frame-accounting test cares about: it spawns
//! this program five times and asserts every frame comes back exactly.
//!
//! Name: unrecorded. Introduced 2026-07-31 as a fixture: a program that does what it is not
//! permitted to do, so the kernel's refusal has a witness.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use user_rt::{exit, yield_now};

/// **Leave user mode and come back, twice.** The kernel counts syscalls from user mode; two of them
/// is the assertion, because the second can only happen if the return from the first genuinely put
/// us back at EL0/U-mode. One proves we left. Two prove we came back.
/// It is also the default, so nothing branches on this constant; it is here because the kernel side
/// (`user::OUTLAW_ROUND_TRIP`) has to name the same number, and a role catalogue with a hole in it
/// is how the numbers drift apart.
#[allow(dead_code)]
const ROUND_TRIP: u64 = 0;

/// **Reach for an address we are not allowed to touch.** The address arrives in the second argument
/// register rather than being baked in here, which is the whole reason this program is portable: the
/// kernel's own memory is at a different virtual address on each ISA, and the test knows that
/// address (it is the one it just asserted the kernel itself can read). See
/// `kernel::user::tests::a_user_program_cannot_read_a_kernel_address`.
const READ_KERNEL: u64 = 1;

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, addr: u64, _arg2: u64) -> ! {
    match role {
        READ_KERNEL => read_kernel(addr),
        // ROUND_TRIP, and anything else: a role this program does not know is not worth dying over.
        _ => round_trip(),
    }
}

fn round_trip() -> ! {
    yield_now();
    yield_now();
    // Exit rather than spin. A program left spinning at user mode outlives the test that spawned it
    // and starves whatever runs next, which is what `no_leaked_threads` exists to catch.
    exit();
}

fn read_kernel(addr: u64) -> ! {
    // SAFETY: there is nothing safe about this, and that is the point. The address is mapped and
    // the kernel reads it all day; the permission bits are what say we may not. The read faults, the
    // kernel kills this thread, and the loop below is never reached.
    unsafe { core::ptr::read_volatile(addr as *const u64) };

    // If we get here the privilege boundary did not hold, and the test's fault count never moves.
    loop {
        core::hint::spin_loop();
    }
}

user_rt::panic_handler!();
