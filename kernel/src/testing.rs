//! The QEMU test harness.
//!
//! `cargo test` builds the kernel with `cfg(test)`, the runner in
//! `.cargo/config.toml` boots it under QEMU, and we report pass/fail by asking QEMU
//! to exit with a status code via semihosting. Cargo reads that status and calls it
//! a pass or a failure.
//!
//! Set up on day one on purpose. The alternative is debugging by `println!` for a
//! year (DECISIONS.md §7).

use crate::arch::semihosting;
use crate::{print, println};
use core::sync::atomic::{AtomicU64, Ordering};

// A hang watchdog. A lost IPC wakeup leaves a test blocked forever; without this the whole run
// hangs silently (and a CI or a `script/test` never returns). Instead, the timer IRQ watches a
// heartbeat the runner bumps before each test, and if a single test makes no progress for ~60 s
// (no test is remotely that slow; a hang is infinite), it dumps the thread table and fails. Driven
// from the timer so it costs nothing and cannot itself perturb the scheduling it is watching.
static HEARTBEAT: AtomicU64 = AtomicU64::new(0);
static WATCH_LAST_HB: AtomicU64 = AtomicU64::new(0);
static WATCH_STALL_TICKS: AtomicU64 = AtomicU64::new(0);

/// Called from the timer IRQ each tick (test builds only; see `timer::tick`). Only the boot core
/// watches, so any dump happens once.
pub fn watchdog_tick() {
    if crate::cpu::id() != 0 {
        return;
    }
    const STALL_LIMIT: u64 = 6000; // ticks at 100 Hz = 60 s
    let hb = HEARTBEAT.load(Ordering::Relaxed);
    if hb != WATCH_LAST_HB.load(Ordering::Relaxed) {
        WATCH_LAST_HB.store(hb, Ordering::Relaxed);
        WATCH_STALL_TICKS.store(0, Ordering::Relaxed);
        return;
    }
    if WATCH_STALL_TICKS.fetch_add(1, Ordering::Relaxed) + 1 == STALL_LIMIT {
        println!();
        println!("WATCHDOG: a test made no progress for ~60 s — likely a lost-wakeup hang.");
        // SCHED is free on a blocked-thread hang (a lock deadlock is a different failure); this is
        // best-effort either way. It names the thread that is stuck and its wake flags.
        crate::sched::dump_threads();
        semihosting::exit(semihosting::EXIT_FAILURE);
    }
}

/// Lets us print a test's name before running it. `core::any::type_name` gives us
/// the full path of the function, which is close enough to a test name.
pub trait Testable {
    fn run(&self);
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        print!("test {} ... ", core::any::type_name::<T>());
        HEARTBEAT.fetch_add(1, Ordering::Relaxed); // tell the watchdog this test started
        self();

        // A test that overflows the stack corrupts the kernel and then fails somewhere
        // else entirely, often in a *later* test, or by hanging with no output at all.
        // Checking here pins the blame on the test that actually did it.
        //
        // This is not hypothetical. It is how milestone 3 went. See notes/stack.md.
        assert!(
            crate::stack::intact(),
            "this test smashed the stack (headroom: {})",
            crate::stack::headroom()
        );

        println!("ok");
    }
}

/// Runs every `#[test_case]` in the crate, then exits QEMU.
///
/// A panic anywhere in here lands in the panic handler, which exits with a failure
/// status instead. So there is no "count the failures" logic: the first failing
/// assertion terminates the run. Crude, but a kernel with a failed invariant has no
/// business continuing anyway.
pub fn runner(tests: &[&dyn Testable]) {
    println!();
    println!("running {} tests", tests.len());
    println!();

    for test in tests {
        test.run();
    }

    println!();
    println!("test result: ok. {} passed", tests.len());

    semihosting::exit(semihosting::EXIT_SUCCESS)
}
