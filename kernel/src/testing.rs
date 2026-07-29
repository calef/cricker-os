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
// hangs silently (and a CI or a `script/test` never returns). The timer IRQ watches a heartbeat and
// fails the run if it does not move for ~60 s.
//
// **The heartbeat tracks PROGRESS, not test starts** (the honest instrument). An earlier version
// bumped only when a test *began*, so it could not tell a genuinely deadlocked test from one that is
// simply slow, and it tripped on `std_net`. Progress is now credited two ways:
//
//   1. [`note_progress`] bumps a heartbeat on every observable kernel step: a completed IPC
//      rendezvous or device-IRQ wake (`sched::wake` / `wake_load_aware`) and every line of console
//      output (`console::_print`, which covers each test's "ok").
//   2. Any online core actively running a real (non-idle) thread ([`any_core_running_real_work`]).
//
// The second signal is what makes `std_net` pass honestly. Measured, it completes in about 300 s,
// but its time is spent in netd's *userspace* smoltcp poll: a CPU-bound loop that, for stretches
// well over a minute, makes no wake and no output, so signal 1 alone still tripped it. It is plainly
// not a lost-wakeup hang, though: a real thread is running the whole time. A lost wakeup is the
// opposite, every thread `Blocked` and every core parked on its idle thread, which is exactly what
// signal 2 detects the absence of. (A busy-spin *livelock* is indistinguishable from a live
// CPU-bound test at runtime; catching that is not this watchdog's job, which is the lost wakeup.)
// Driven from the timer so it costs nothing and cannot perturb the scheduling it watches.
static HEARTBEAT: AtomicU64 = AtomicU64::new(0);
static WATCH_LAST_HB: AtomicU64 = AtomicU64::new(0);
static WATCH_STALL_TICKS: AtomicU64 = AtomicU64::new(0);

/// Record one step of forward progress for the hang watchdog (test builds only). Cheap enough to sit
/// on the wake and console paths: one relaxed increment. See the module note above.
#[inline]
pub fn note_progress() {
    HEARTBEAT.fetch_add(1, Ordering::Relaxed);
}

/// Is any online core running a real thread (not its idle fallback)? A lost-wakeup hang leaves every
/// core parked on idle; a slow-but-live test (a userspace CPU-bound loop like std_net's smoltcp
/// poll) always has one running. Read-only across the per-CPU blocks; racy by nature, which a
/// heartbeat sampled once per tick tolerates.
fn any_core_running_real_work() -> bool {
    (0..crate::smp::online_count()).any(|c| {
        let pc = crate::cpu::of(c);
        let cur = pc.current.load(Ordering::Relaxed);
        cur != crate::cpu::NO_TID && cur != pc.idle.load(Ordering::Relaxed)
    })
}

/// Called from the timer IRQ each tick (test builds only; see `timer::tick`). Only the boot core
/// watches, so any dump happens once. The boot core is `arch::boot_cpu_id()` (0 on aarch64, but on
/// RISC-V whichever hart QEMU booted), which is also the one hart that ticks in a single-hart test.
pub fn watchdog_tick() {
    if crate::cpu::id() != crate::arch::boot_cpu_id() {
        return;
    }
    const STALL_LIMIT: u64 = 6000; // ticks at 100 Hz = 60 s with no progress at all
    let hb = HEARTBEAT.load(Ordering::Relaxed);
    let progress = hb != WATCH_LAST_HB.load(Ordering::Relaxed) || any_core_running_real_work();
    if progress {
        WATCH_LAST_HB.store(hb, Ordering::Relaxed);
        WATCH_STALL_TICKS.store(0, Ordering::Relaxed);
        return;
    }
    if WATCH_STALL_TICKS.fetch_add(1, Ordering::Relaxed) + 1 == STALL_LIMIT {
        println!();
        println!("WATCHDOG: no progress for ~60 s — every core idle, every thread blocked: a lost-wakeup hang.");
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
