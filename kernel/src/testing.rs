//! The QEMU test harness.
//!
//! `cargo test` builds the kernel with `cfg(test)`, the runner in
//! `.cargo/config.toml` boots it under QEMU, and we report pass/fail by asking QEMU
//! to exit with a status code via semihosting. Cargo reads that status and calls it
//! a pass or a failure.
//!
//! Set up on day one on purpose. The alternative is debugging by `println!` for a
//! year (DECISIONS.md §7).

use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use crate::arch::semihosting;
use crate::{print, println};

// The hang watchdog: **two independent mechanisms, because there are two ways a test never
// finishes.** Both run off the timer IRQ, so they cost a couple of atomic loads per tick and cannot
// perturb the scheduling they watch. Either one firing fails the run with a thread dump.
//
// **1. The no-progress heartbeat (a lost wakeup).** [`note_progress`] bumps a counter on every
// observable kernel step: a completed IPC rendezvous or device-IRQ wake (`sched::wake` /
// `wake_load_aware`) and every line of console output (`console::_print`, which covers each test's
// "ok"). Progress is also credited when any online core is running a real, non-idle thread
// ([`any_core_running_real_work`]). If none of that happens for ~60 s the run fails. That second
// signal is what lets `std_net` pass honestly: it spends its ~300 s in net_stack's *userspace* smoltcp
// poll, CPU-bound, making no wake and no output for stretches over a minute, yet a real thread runs
// the whole time. A genuine lost wakeup is the opposite: every thread `Blocked`, every core parked on
// its idle thread.
//
// **2. The per-test wall-clock ceiling (a livelock).** The heartbeat above cannot see a livelock that
// *makes progress*, and that is not hypothetical: the RedoxFS repeat-write loop spins in an allocator
// commit while still doing blk IPC, so every rendezvous reset the heartbeat and a failure that used to
// be a loud 60 s trip became an infinite silent hang at ~400% CPU. A progress-only instrument cannot
// distinguish that from healthy work, so it needs a second question: not "is anything happening?" but
// "has this test taken longer than it is allowed to?" [`Testable::run`] stamps a start time and a
// budget per test; this fails the run when the budget is exceeded **even though progress is being
// made**. Turning a loud failure into a silent hang is strictly worse than the flake the heartbeat
// fixed, which is why both mechanisms are live.
//
// What each can and cannot see, stated plainly:
//
//   - The heartbeat catches a total stall (deadlock, lost wakeup) fast, and catches it wherever it
//     happens, including before any test starts. It is blind to any loop that keeps doing IPC.
//   - The ceiling catches any test that does not terminate, livelock included, regardless of how busy
//     it looks. It cannot fire faster than the budget, so it is a backstop, not a diagnostic.
//   - Neither can tell a livelock from slow-but-correct work *while it is running*; only the budget,
//     which is a human declaration of expected cost, separates them. That is the honest limit.
//   - **The heartbeat credits work by ANY thread, including leftovers from earlier tests, and that
//     blinded it once for real** (milestone 31 phase 2). The FS server died of a stack overflow, its
//     client blocked on a `CALL` nobody would ever answer, and nothing in that test made progress
//     again; but processes left spinning by earlier tests kept `any_core_running_real_work` true, so
//     the 60 s stall never registered and only the ceiling fired. Attributing a thread to the running
//     test would fix it and the kernel cannot: a test's processes are ordinary processes. The defence
//     is the ceiling, plus reading the thread dump's **address-space roots** rather than its program
//     counters, which is what tells a leftover spinner from the process under test.
//   - **A ceiling failure reports the ceiling, not the cost.** "ran 900 s" against a 900 s budget is
//     the budget being spent and says nothing about how long the work would take. The same incident
//     had that number read as evidence of honest slowness, which sent an investigation looking for a
//     slow path in a test whose server was already dead. Raising a budget to "measure" something
//     returns only the new budget.
//   - `scripts/qemu-bounded.sh` remains the outermost backstop, for a kernel that wedges so hard the
//     timer IRQ itself stops. It did NOT fire in the reported case because that run invoked `cargo`
//     directly instead of going through the wrapper: **a bypassable backstop is not a backstop**,
//     which is precisely why the ceiling belongs in the kernel, where nothing can route around it.
static HEARTBEAT: AtomicU64 = AtomicU64::new(0);
static WATCH_LAST_HB: AtomicU64 = AtomicU64::new(0);
static WATCH_STALL_TICKS: AtomicU64 = AtomicU64::new(0);

/// When the running test started, in `arch::timer::now()` counter units, or 0 for "no test is
/// running" (during boot, and between tests), which disables the ceiling.
static TEST_START: AtomicU64 = AtomicU64::new(0);

/// The running test's wall-clock budget in counter units, and its name, for the failure report. The
/// name is a `&'static str` split into pointer and length so the tick path never takes a lock; it is
/// only reassembled on the failure path.
static TEST_BUDGET: AtomicU64 = AtomicU64::new(0);
static TEST_NAME_PTR: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
static TEST_NAME_LEN: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------------------------
// The frame ledger.
//
// **A boot has one pool of physical frames and no test gives an account of what it took.** That is
// how the aarch64 suite spent its way to `Unmappable(OutOfFrames)` in whatever test happened to
// spawn last, one run in three, three milestones in a row blaming the wrong code (notes/frames.md).
// The instrument that settled it in milestone 107 was four lines in `untyped::create`, thrown away
// after one run; this is the same idea kept, so the next person reads a number instead of building
// one.
//
// Two numbers per test, because they answer different questions and only the second fails a boot:
// how many frames the test never gave back, and the longest run still allocatable afterwards. A
// suite can hold a comfortable free total and refuse a 128-page request; 137 free with no run of
// 128 is the measured case.
//
// Costs one bitmap scan per test (O(total), ~32k frames), between tests, off every hot path.
// ---------------------------------------------------------------------------------------------

/// Free frames when the first test started: the ledger's opening balance.
static FRAMES_AT_START: AtomicUsize = AtomicUsize::new(0);
/// Whether [`FRAMES_AT_START`] has been stamped (0 is a legal reading, so a flag rather than a
/// sentinel).
static FRAMES_STAMPED: AtomicBool = AtomicBool::new(false);
/// The reading taken at the top of the test now running, and that test's name. The charge against a
/// test is the drop from *its* reading to the *next* one, which is why both are carried forward.
static FRAMES_AT_PREV: AtomicUsize = AtomicUsize::new(0);
static PREV_NAME_PTR: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
static PREV_NAME_LEN: AtomicUsize = AtomicUsize::new(0);
/// The worst single spender so far, and its name, for the closing summary. Same pointer/length
/// trick as the test name above: no allocation, no lock.
static WORST_SPEND: AtomicUsize = AtomicUsize::new(0);
static WORST_NAME_PTR: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
static WORST_NAME_LEN: AtomicUsize = AtomicUsize::new(0);

/// Report a test's frame cost once it reaches this many frames. Below it, silence: a test that
/// spends two pages on an endpoint is not news, and a number on every line buries the ones that
/// are. Sixteen frames is 64 KiB, about the smallest thing a service-shaped test takes.
const FRAME_REPORT_MIN: usize = 16;

/// **Charge the test that just finished, and open an account for the one about to start.**
///
/// Called at the top of every test, and once more from the ledger, so the readings **partition the
/// run**: what a test is charged is the drop between its own reading and the next one, and every
/// frame lost between the first test and the last is charged to exactly one test. That matters more
/// than it sounds. Reading free frames before and after the test body instead attributes only what
/// the test spent *while it was running*, and a test that spawns a service and returns as soon as it
/// has its report leaves the service still mapping its heap: on the first measured aarch64 boot that
/// under-attribution was **17362 of the 29091 frames**, a clear majority landing nowhere.
///
/// The cost is a one-test lag in the transcript, which is why the line is printed after the previous
/// test's `ok` rather than on it.
fn charge_previous(now: usize, next: &'static str) {
    let prev_ptr = PREV_NAME_PTR.load(Ordering::Relaxed);
    if !prev_ptr.is_null() {
        let spent = FRAMES_AT_PREV.load(Ordering::Relaxed).saturating_sub(now);
        if spent >= FRAME_REPORT_MIN {
            println!("    [that test kept {spent} frames]");
        }
        if spent > WORST_SPEND.load(Ordering::Relaxed) {
            WORST_SPEND.store(spent, Ordering::Relaxed);
            WORST_NAME_PTR.store(prev_ptr, Ordering::Relaxed);
            WORST_NAME_LEN.store(PREV_NAME_LEN.load(Ordering::Relaxed), Ordering::Relaxed);
        }
    }
    FRAMES_AT_PREV.store(now, Ordering::Relaxed);
    PREV_NAME_PTR.store(next.as_ptr() as *mut u8, Ordering::Relaxed);
    PREV_NAME_LEN.store(next.len(), Ordering::Relaxed);
}

/// The worst spender's name, reassembled. Only called by the closing summary.
fn worst_spender_name() -> Option<&'static str> {
    let ptr = WORST_NAME_PTR.load(Ordering::Relaxed);
    let len = WORST_NAME_LEN.load(Ordering::Relaxed);
    if ptr.is_null() || len == 0 {
        return None;
    }
    // SAFETY: the pair was stored from a `&'static str` (`core::any::type_name`), which lives for
    // the whole program, and is only ever overwritten by another such pair.
    unsafe { core::str::from_utf8(core::slice::from_raw_parts(ptr as *const u8, len)).ok() }
}

/// Print the ledger. **Reporting only, for now**: what the run may keep is a number nobody has yet,
/// and inventing one before the measurement is the mistake this instrument exists to stop. The two
/// ceilings arrive once the reclamation below has made them meaningful.
fn report_frame_ledger() {
    if !FRAMES_STAMPED.load(Ordering::Relaxed) {
        return; // no test ran; nothing was spent
    }
    let end = crate::memory::free_frames();
    // Close the last test's account, so the charges partition the whole run with nothing left over.
    charge_previous(end, "");
    let start = FRAMES_AT_START.load(Ordering::Relaxed);
    let run = crate::memory::largest_free_run();
    let spent = start.saturating_sub(end);
    println!(
        "frames: {start} free before the first test, {end} after the last ({spent} never returned); \
         longest free run {run}"
    );
    if let Some(name) = worst_spender_name() {
        println!(
            "  the biggest single spender was {name} at {} frames",
            WORST_SPEND.load(Ordering::Relaxed)
        );
    }
}

/// Report a test's duration once it reaches this many seconds. Below it, silence: most tests are
/// milliseconds and a duration on every line would bury the signal. Above it, the number is what makes
/// a [`SLOW_TESTS`] entry an evidence-based declaration rather than a guess: until this existed, the
/// only way to learn a test's real cost was to set its budget too low on purpose and read the failure.
const SLOW_REPORT_SECS: u64 = 5;

/// **The default per-test wall-clock budget.** Deliberately tight: almost every test in this suite is
/// milliseconds, and a handful of the userspace ones are a few seconds. 90 s is far above anything
/// honest while still being a real net, so a two-second unit test that starts spinning fails in a
/// minute and a half rather than never.
const DEFAULT_BUDGET_SECS: u64 = 90;

/// **The known-slow tests, each declaring its own cost.** A test whose honest runtime exceeds
/// [`DEFAULT_BUDGET_SECS`] must say so here, with the reason, which is the point: the exception is
/// visible, reviewable, and attached to an explanation instead of being absorbed into one enormous
/// global limit that protects nothing. Matched as a substring of the full test path, so the entry
/// covers a test on both architectures.
///
/// Keep budgets roughly 2x the measured time: enough headroom that host load or a debug build does
/// not produce a flaky failure, tight enough to still catch a hang.
const SLOW_TESTS: &[(&str, u64)] = &[
    // Measured ~300 to 344 s on aarch64: a serial net_stack<->std_exerciser pipeline whose time is spent in
    // net_stack's userspace smoltcp poll (DHCP, DNS, then a TCP echo). The longest honest test we have,
    // and the reason a single global ceiling would have to be uselessly large.
    ("std_net_runs_over_the_socket_contract", 700),
];

/// The budget for a test, by name: its [`SLOW_TESTS`] entry if it has one, else the default.
fn budget_secs_for(name: &str) -> u64 {
    let mut secs = DEFAULT_BUDGET_SECS;
    for &(needle, allowed) in SLOW_TESTS {
        if name.contains(needle) {
            secs = allowed;
        }
    }
    secs
}

/// Record one step of forward progress for the hang watchdog (test builds only). Cheap enough to sit
/// on the wake and console paths: one relaxed increment. See the module note above.
#[inline]
pub fn note_progress() {
    HEARTBEAT.fetch_add(1, Ordering::Relaxed);
}

/// Is any online core running a real thread (not its idle fallback)? A lost-wakeup hang leaves every
/// core parked on idle; a slow-but-live test (a userspace CPU-bound loop like `std_net`'s smoltcp
/// poll) always has one running. Read-only across the per-CPU blocks; racy by nature, which a
/// heartbeat sampled once per tick tolerates.
fn any_core_running_real_work() -> bool {
    // The online set, not `0..count` (first-silicon sweep, 2026-08-14): with the VisionFive 2's
    // {1,2,3} online, the count-as-index scan read parked slot 0's statics and never looked at
    // cpu 3, so a suite whose only live work sat on cpu 3 would read as hung.
    crate::smp::online_cpus().any(|c| {
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

    // Mechanism 2 first: a test over its budget fails even while it is making progress, which is the
    // whole point (a livelock doing IPC keeps the heartbeat below perfectly healthy).
    check_test_ceiling();

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
        println!(
            "WATCHDOG: no progress for ~60 s. Every core idle, every thread blocked: a lost-wakeup hang."
        );
        crate::sched::dump_threads();
        semihosting::exit(semihosting::EXIT_FAILURE);
    }
}

/// **Has the running test exceeded its wall-clock budget?** (mechanism 2; see the module note.) Fails
/// the run with the test's name, how long it actually ran, and its budget, so a future livelock reads
/// as "this test exceeded its budget while making progress" and not as an anonymous timeout.
fn check_test_ceiling() {
    let start = TEST_START.load(Ordering::Relaxed);
    if start == 0 {
        return; // no test running: boot, or between tests
    }
    let budget = TEST_BUDGET.load(Ordering::Relaxed);
    let elapsed = crate::arch::timer::now().wrapping_sub(start);
    if budget == 0 || elapsed <= budget {
        return;
    }

    // Over budget. Disarm first, so anything that prints or wakes from here (the dump) cannot
    // re-enter this path and report twice.
    TEST_START.store(0, Ordering::Relaxed);

    let hz = crate::arch::timer::frequency().max(1);
    println!();
    println!(
        "WATCHDOG: test exceeded its {} s budget (ran {} s) WHILE MAKING PROGRESS: a livelock, not a \
         lost wakeup. The no-progress heartbeat cannot see this, which is why the per-test ceiling \
         exists. If this test is honestly this slow, give it an entry in testing.rs SLOW_TESTS.",
        budget / hz,
        elapsed / hz,
    );
    if let Some(name) = current_test_name() {
        println!("  test: {name}");
    }
    crate::sched::dump_threads();
    semihosting::exit(semihosting::EXIT_FAILURE);
}

/// The running test's name, reassembled from the pointer/length pair stamped by [`Testable::run`].
/// Only called on the failure path.
fn current_test_name() -> Option<&'static str> {
    let ptr = TEST_NAME_PTR.load(Ordering::Relaxed);
    let len = TEST_NAME_LEN.load(Ordering::Relaxed);
    if ptr.is_null() || len == 0 {
        return None;
    }
    // SAFETY: the pair was stored from a `&'static str` (`core::any::type_name`), which lives for the
    // whole program, and is only ever overwritten by another such pair.
    unsafe {
        let bytes = core::slice::from_raw_parts(ptr as *const u8, len);
        core::str::from_utf8(bytes).ok()
    }
}

/// Lets us print a test's name before running it. `core::any::type_name` gives us
/// the full path of the function, which is close enough to a test name.
pub trait Testable {
    fn run(&self);
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        let name = core::any::type_name::<T>();

        // The frame ledger, before this test's name is printed: the reading closes the *previous*
        // test's account (and prints its charge under its own `ok` line) and opens this one's. The
        // opening balance is taken at the first test rather than at boot, because what the kernel
        // spends coming up is not a test's doing and is not what this measures.
        let frames_now = crate::memory::free_frames();
        if !FRAMES_STAMPED.swap(true, Ordering::Relaxed) {
            FRAMES_AT_START.store(frames_now, Ordering::Relaxed);
        }
        charge_previous(frames_now, name);

        print!("test {name} ... ");
        HEARTBEAT.fetch_add(1, Ordering::Relaxed); // tell the watchdog this test started

        // Arm the per-test wall-clock ceiling (mechanism 2). The name goes first and the start time
        // last, because a non-zero start is what arms the check: this way the watchdog never sees a
        // live budget with a stale name.
        TEST_NAME_PTR.store(name.as_ptr() as *mut u8, Ordering::Relaxed);
        TEST_NAME_LEN.store(name.len(), Ordering::Relaxed);
        TEST_BUDGET.store(
            budget_secs_for(name) * crate::arch::timer::frequency(),
            Ordering::Relaxed,
        );
        let start = crate::arch::timer::now();
        // `now()` could legitimately be 0 on the very first tick of a fresh counter, and 0 is the
        // disarmed sentinel, so nudge it. One counter unit of slack is nothing against a 90 s budget.
        TEST_START.store(if start == 0 { 1 } else { start }, Ordering::Relaxed);

        self();

        // Disarm: between tests there is no budget to exceed, and the next test arms its own.
        TEST_START.store(0, Ordering::Relaxed);

        // Report what a test actually cost, if it cost anything worth knowing. The ceiling already
        // needs a start time, so this is free, and it closes a real gap: a SLOW_TESTS budget is a
        // human declaration of expected cost, and until now there was no way to learn the cost except
        // by setting a budget too low on purpose and reading the failure. That is a bad way to find
        // out, and it is exactly the position I was in when std_fs tripped the 90 s ceiling with no
        // number attached. Anything under the threshold stays silent so the transcript is unchanged.
        let elapsed =
            crate::arch::timer::now().saturating_sub(start) / crate::arch::timer::frequency();
        if elapsed >= SLOW_REPORT_SECS {
            print!("[{elapsed} s] ");
        }

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

/// **The livelock probe** (feature `watchdog_probe`, EXPECTED TO FAIL the run). Proof that the
/// per-test ceiling catches what the no-progress heartbeat cannot: this loops forever while doing a
/// full IPC rendezvous every iteration, so [`note_progress`] fires constantly and the heartbeat sees
/// a perfectly healthy kernel. That is the shape of the RedoxFS repeat-write livelock (spinning in an
/// allocator commit while still serving blk IPC), which turned a loud 60 s watchdog trip into an
/// infinite silent hang at ~400% CPU.
///
/// Run it with:
///
/// ```text
/// scripts/qemu-bounded.sh 200 cargo test -p kernel \
///     --features watchdog_probe --target aarch64-unknown-none-softfloat
/// ```
///
/// Expected: the run FAILS after this test's 90 s default budget, naming the test and its runtime.
/// Without the ceiling it hangs until the outer bound kills it, which is the regression this guards.
#[cfg(all(test, feature = "watchdog_probe"))]
#[test_case]
fn a_livelock_that_keeps_doing_ipc_trips_the_per_test_ceiling() {
    let ep = crate::sched::create_endpoint();

    // A partner that answers forever. Both sides rendezvous every pass, so every pass is a wake, and
    // a wake is "progress" as far as the heartbeat is concerned.
    crate::sched::spawn(move || {
        loop {
            let _ = crate::sched::ipc_recv(ep);
        }
    })
    .expect("probe: could not spawn the partner");

    loop {
        crate::sched::ipc_send(ep, [1, 2, 3]);
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
    #[cfg(test)]
    // the runner itself is compiled in every build; the instrument only exists in test
    crate::stack::report_high_water();
    report_frame_ledger();

    println!();
    println!("test result: ok. {} passed", tests.len());

    semihosting::exit(semihosting::EXIT_SUCCESS)
}
