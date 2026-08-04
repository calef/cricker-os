//! The ARM Generic Timer: the preemption source.
//!
//! # Why this lives in `arch/` and not `drivers/`
//!
//! It is not an MMIO device. It is **part of the CPU**, reached through system registers, and
//! every aarch64 core has one whether the board's designer wanted it or not. There is no base
//! address to discover and nothing in the device tree to look up except which interrupt it
//! raises.
//!
//! That is a real convenience and it is why aarch64 kernels can have a working clock before
//! they can enumerate a single peripheral.
//!
//! # The registers
//!
//! | | |
//! |---|---|
//! | `CNTFRQ_EL0` | how fast the counter ticks. **Set by firmware, read by us.** QEMU says 62.5 MHz. |
//! | `CNTVCT_EL0` | the counter itself: a 64-bit number that only ever goes up |
//! | `CNTV_CVAL_EL0` | an **absolute deadline**. Fire when `CNTVCT_EL0` reaches this. |
//! | `CNTV_TVAL_EL0` | a **relative countdown**, which is just `CVAL = CNTPCT + N`. A trap. |
//! | `CNTV_CTL_EL0` | enable, mask, and a read-only "did it fire" bit |
//!
//! `CNTVCT_EL0` is what `Instant` is made of. It never wraps in any timescale that matters (at
//! 62.5 MHz, 64 bits is about 9000 years) and it does not stop when interrupts are masked,
//! which makes it the only honest way to measure a critical section.
//!
//! # Re-arming is not optional
//!
//! The timer is **one-shot**. It counts down, fires, and then sits there with its "I fired" bit
//! set, raising the interrupt line forever. The handler must set a new deadline, and *that
//! write is what lowers the line*.
//!
//! Forget it and the timer fires exactly once and then the machine wedges in a permanent
//! interrupt storm, which looks nothing like "you forgot to write a register".
//!
//! # And re-arming with TVAL silently loses ticks
//!
//! We shipped this bug and then measured it. `TVAL` is a **relative** countdown: writing N
//! means "fire N ticks from *now*". So re-arming with `TVAL = interval` in the handler gives a
//! real period of
//!
//! ```text
//!     interval  +  however long it took to get into the handler and back
//! ```
//!
//! Every tick starts its countdown *late*, and the lateness is never recovered. **The clock
//! runs slow, permanently, and nothing tells you.** Measured under QEMU: 100 Hz configured,
//! ~70 Hz observed. Thirty percent of our preemptions, gone.
//!
//! `CVAL` is an **absolute** deadline: fire when the counter reaches this exact value. Set the
//! next one to `previous_deadline + interval` and the deadlines sit on a **fixed grid**. A slow
//! handler makes one tick *late*; it does not make the next one late as well.
//!
//! This is the difference between a clock that drifts and a clock that doesn't, and it is one
//! register.

// Four accessors below (`ticks`, `missed_ticks`, `uptime_ms`, `interval`) have no caller outside
// this file's own test module on aarch64: the tick handler re-arms from CVAL rather than from
// `interval()`, and the boot banner prints a tick count only on the RISC-V tour. They are each
// marked `cfg_attr(not(test), allow(dead_code))` rather than the whole module being suppressed,
// so a test that stops exercising one is a gate failure instead of silence. (The header comment
// this replaces predicted callers in "milestone 6 and 8", both long since shipped without them.)

use core::sync::atomic::{AtomicU64, Ordering};

use aarch64_cpu::registers::{CNTFRQ_EL0, CNTKCTL_EL1, CNTV_CTL_EL0, CNTV_CVAL_EL0, CNTVCT_EL0};
use tock_registers::interfaces::{Readable, Writeable};

use crate::cpu::{self, MAX_CPUS};
use crate::drivers::gic;

/// The EL1 **virtual** timer, as a GIC interrupt ID.
///
/// **A PPI, not an SPI**, and the device tree says so: `interrupts = <1 11 ...>` on the timer
/// node, where type 1 means PPI and 11 is the PPI number. PPIs start at INTID 16, so
/// `16 + 11 = 27`.
///
/// It *has* to be per-core. A timer that fired on only one core could not preempt threads
/// running on the others, so every core has its own, wearing the same number.
///
/// # Why the *virtual* timer, and not the physical one (INTID 30)
///
/// We used the physical timer (`CNTP_*`, INTID 30) through milestone 9, and it worked on QEMU's
/// software CPU and would work on bare metal. It **traps under a hypervisor**: the physical timer
/// belongs to EL2, and a guest at EL1 that writes `CNTP_CVAL_EL0` takes an "Unknown reason" trap
/// (ESR EC 0x00). We found this the first time we booted under Apple's Hypervisor.framework on an
/// M3, which is exactly the "which assumptions were secretly QEMU-shaped" moment DECISIONS.md and
/// notes/portability.md anticipate for a new target, arriving early because HVF runs the real
/// core.
///
/// The **virtual** timer (`CNTV_*`, INTID 27) is the one a guest is meant to use, and it is
/// available at EL1 both on bare metal and under any hypervisor. So this is strictly more
/// portable: it keeps working under QEMU/TCG, under HVF, and on a real board, with no
/// per-environment branching. See notes/virtualization.md.
pub const TIMER_INTID: u32 = 27;

/// 100 Hz. Ten milliseconds per tick.
///
/// The classic tradeoff, and it is a real one. Faster ticks mean finer-grained preemption (a
/// thread cannot hog the CPU for longer than one tick) but more time spent in the handler
/// doing nothing useful. Linux ships 250 Hz and can be built tickless; 100 Hz is the old Unix
/// default and it is plenty for a kernel with no threads yet.
pub const TICK_HZ: u64 = 100;

/// Every tick, forever. The heartbeat.
/// Per-CPU, because each core has its own timer (a banked PPI). A single global counter would be
/// advanced by every core's tick, which breaks the "holding a lock masks *my* timer" invariant the
/// tests check: a lock masks only the holding core's interrupts, not the others'. See DECISIONS §11.
static TICKS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Counter ticks between interrupts. Computed from `CNTFRQ_EL0`, never hardcoded: the
/// frequency is a property of the board, and a hardcoded one would make our 10 ms into
/// something else entirely on a Pi.
static INTERVAL: AtomicU64 = AtomicU64::new(0);

/// Start the heartbeat.
///
/// The GIC must already be up: we ask it to deliver INTID 30, and it has to exist to be asked.
pub fn init() {
    let freq = CNTFRQ_EL0.get();
    assert!(freq > 0, "firmware left CNTFRQ_EL0 at zero: no clock");

    // Let EL0 read the virtual counter (`CNTVCT_EL0`) and `CNTFRQ_EL0`, so a userspace program can
    // time *itself*, the way Linux exposes the counter to its vDSO. Without this the read traps.
    //
    // This is a deliberate, eyes-open exception to §10's no-ambient-authority rule (notes/abi.md).
    // A monotonic counter grants no authority to *affect* anything, only to observe the passage of
    // time; the cost is that it is a timing side channel, which every OS that offers userspace
    // timing accepts. We accept it too, and we accept it *knowingly*, because the cross-OS primitive
    // suite needs userspace self-timing to be comparable to lmbench (which measures from userspace).
    // The physical counter and the timer registers stay trapped; only the virtual counter opens.
    // `aarch64_cpu` gives CNTKCTL_EL1 no named fields, so set the bit by hand: EL0VCTEN is bit 1.
    const EL0VCTEN: u64 = 1 << 1;
    CNTKCTL_EL1.set(CNTKCTL_EL1.get() | EL0VCTEN);

    let interval = freq / TICK_HZ;
    INTERVAL.store(interval, Ordering::Relaxed);

    gic::enable(TIMER_INTID, 0); // PPI: per-core, target ignored

    start(interval);
}

/// Set the first deadline and enable.
///
/// `IMASK` clear means "and actually raise the interrupt line". The timer will happily count
/// down and set its status bit with the interrupt masked; the mask only stops the line.
fn start(interval: u64) {
    CNTV_CVAL_EL0.set(CNTVCT_EL0.get() + interval);
    CNTV_CTL_EL0.write(CNTV_CTL_EL0::ENABLE::SET + CNTV_CTL_EL0::IMASK::CLEAR);
}

/// Move the deadline forward by exactly one interval.
///
/// **`previous + interval`, not `now + interval`.** That is the entire point: the deadlines sit
/// on a fixed grid anchored at boot, so however long the handler took, the next tick is still
/// where it always was going to be. A slow handler makes *one* tick late; it does not push the
/// next one out too.
///
/// The `if` is the safety valve. If we fell so far behind that the next deadline is *already in
/// the past*, we would fire again immediately, and again, and spin in the handler forever
/// trying to catch up on a debt we cannot pay. So we give up on the missed ticks and re-anchor
/// the grid to now. Linux calls this the same thing every kernel calls it: dropping ticks.
fn rearm(interval: u64) {
    let now = CNTVCT_EL0.get();
    let mut next = CNTV_CVAL_EL0.get() + interval;

    if next <= now {
        MISSED_TICKS[cpu::id()].fetch_add(1, Ordering::Relaxed);
        // Test builds only: keep the numbers, so a failure says HOW LATE rather than only that it
        // was late. Two relaxed stores, no branch on the hot path, and nothing printed: this runs in
        // interrupt context and DECISIONS §9's rule (handlers record and defer) applies to
        // diagnostics too.
        #[cfg(test)]
        miss_detail::record(now, next);
        next = now + interval;
    }

    CNTV_CVAL_EL0.set(next);
}

/// Deadlines that had already passed by the time we re-armed. **Should be zero.** A nonzero
/// count means the handler is taking longer than a whole tick period, which is a real problem
/// and not a rounding error.
static MISSED_TICKS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// This core's missed ticks.
#[cfg_attr(not(test), allow(dead_code))] // this file's tests are the callers
pub fn missed_ticks() -> u64 {
    missed_ticks_on(cpu::id())
}

/// A named core's missed ticks. The [`ticks_on`] argument, applied to the miss count: a test that
/// reads it either side of a wait must name the core, or a migration compares two unrelated
/// counters.
#[cfg_attr(not(test), allow(dead_code))]
pub fn missed_ticks_on(core: usize) -> u64 {
    MISSED_TICKS[core].load(Ordering::Relaxed)
}

/// This core's next armed deadline: `CNTV_CVAL_EL0`, the grid cell [`rearm`] advances from.
///
/// Exposed so the drift test can assert the re-arm law directly (deadlines advance by exactly one
/// interval per delivered tick) instead of inferring it from a wall-clock tick rate, which a
/// descheduled emulator falsifies. The register is banked per core, like the counter.
#[cfg_attr(not(test), allow(dead_code))] // this file's tests are the callers
pub fn deadline() -> u64 {
    CNTV_CVAL_EL0.get()
}

/// Why a miss happened, kept only in test builds.
///
/// `missed_ticks()` says a deadline was already past when the handler re-armed. It does not say by
/// how much, and the difference matters: **a few hundred cycles late is a slow handler, and a whole
/// tick period late is the emulator having been descheduled.** Without the numbers those two look
/// identical in a panic message, which is the position milestone 78 records the suite being in.
///
/// The last miss only, plus a count. A burst records once and reports the final pair, which is
/// enough to tell the two cases apart and cheaper than a ring buffer in interrupt context.
#[cfg(test)]
pub mod miss_detail {
    use core::sync::atomic::{AtomicU64, Ordering};

    use crate::cpu::{self, MAX_CPUS};

    static NOW: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
    static NEXT: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
    static COUNT: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

    /// Record the counter and the deadline it had already passed. Called from `rearm`, in
    /// interrupt context, so this is three relaxed stores and nothing else.
    pub fn record(now: u64, next: u64) {
        let id = cpu::id();
        NOW[id].store(now, Ordering::Relaxed);
        NEXT[id].store(next, Ordering::Relaxed);
        COUNT[id].fetch_add(1, Ordering::Relaxed);
    }

    /// This core's last miss as `(now, next, count)`. `now - next` is how late the re-arm was, in
    /// counter ticks; compare it against `timer::interval()` to tell a slow handler from a
    /// descheduled emulator.
    pub fn last() -> (u64, u64, u64) {
        let id = cpu::id();
        (
            NOW[id].load(Ordering::Relaxed),
            NEXT[id].load(Ordering::Relaxed),
            COUNT[id].load(Ordering::Relaxed),
        )
    }
}

/// Called from the IRQ handler. **Must re-arm**, or the interrupt line stays high forever and
/// the machine drowns in its own timer.
///
/// This is the whole handler, and it is deliberately tiny: bump a counter, reload the
/// countdown, return. DECISIONS.md §9: **interrupt handlers record and defer; they do not do
/// work.** At milestone 6 this will also set a "reschedule wanted" flag, and the *scheduler*
/// will act on it in normal context.
pub fn tick() {
    TICKS[cpu::id()].fetch_add(1, Ordering::Relaxed);
    // Test builds only: watch for a hung test (a lost IPC wakeup) and fail fast with a diagnostic
    // instead of blocking the run forever. Costs a couple of atomic loads per tick.
    #[cfg(test)]
    crate::testing::watchdog_tick();
    rearm(INTERVAL.load(Ordering::Relaxed));
}

/// This core's ticks since it started.
#[cfg_attr(not(test), allow(dead_code))] // this file's tests are the callers
pub fn ticks() -> u64 {
    ticks_on(cpu::id())
}

/// Ticks since boot on a **named** core, which is what a test needs when the thread doing the
/// measuring can move between the two reads.
///
/// [`TICKS`] is per core, so `ticks()` before a wait and `ticks()` after it are the same counter
/// only if nothing migrated the caller in between; a kernel thread on a run queue can be stolen by
/// an idle core at any preemption point (DECISIONS §28.3). Reading by index makes the pair name one
/// core on purpose. See notes/load-sensitive-assertions.md.
#[cfg_attr(not(test), allow(dead_code))]
pub fn ticks_on(core: usize) -> u64 {
    TICKS[core].load(Ordering::Relaxed)
}

/// The raw counter. Monotonic, never wraps in any timescale that matters, and **keeps counting
/// while interrupts are masked**, which is precisely what makes it the only honest way to
/// measure how long a critical section held the CPU.
pub fn now() -> u64 {
    CNTVCT_EL0.get()
}

pub fn frequency() -> u64 {
    CNTFRQ_EL0.get()
}

/// Milliseconds since boot, from the counter rather than from the tick count.
///
/// **Deliberately not `ticks() * 10`.** If an interrupt is ever missed: a long critical
/// section, a slow handler: the tick count undercounts and time appears to slow down. The
/// hardware counter cannot lie. This is `Instant`, and it is the thing `core` could never give
/// us because nothing in `core` knows what time it is.
#[cfg_attr(not(test), allow(dead_code))] // this file's tests are the callers
pub fn uptime_ms() -> u64 {
    let freq = CNTFRQ_EL0.get();
    if freq == 0 {
        return 0;
    }
    now() * 1000 / freq
}

/// Busy-wait. Uses the counter, so it works with interrupts masked, which is exactly when a
/// tick-based delay would hang forever.
///
/// The callers are the milestone tour and the tests. All three alternate boot modes compile the
/// tour out and run no tests (`bench` diverges before it; `shell` and `initboot` skip it), so in
/// those three configurations this has no caller.
#[cfg_attr(
    any(feature = "shell", feature = "bench", feature = "initboot"),
    allow(dead_code)
)]
pub fn spin_for(counter_ticks: u64) {
    let start = now();
    while now().wrapping_sub(start) < counter_ticks {
        core::hint::spin_loop();
    }
}

/// Counter ticks in one timer period.
#[cfg_attr(not(test), allow(dead_code))] // this file's tests are the callers
pub fn interval() -> u64 {
    INTERVAL.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    //! Tests for the timer, and for the thing the whole locking discipline was written to
    //! prevent.

    /// The heartbeat is beating.
    #[test_case]
    fn the_timer_is_ticking() {
        use crate::arch::timer;

        // Name the core. `ticks()` reads a per-core counter and this thread can be migrated by a
        // steal at any preemption point, which would compare two unrelated counters (`ticks_on`).
        let core = crate::cpu::id();
        let before = timer::ticks_on(core);
        timer::spin_for(timer::interval() * 3);
        let after = timer::ticks_on(core);

        assert!(
            after > before,
            "no timer interrupt in three tick periods: the GIC or the timer is not delivering"
        );
    }

    /// Ticks arrive at the configured rate, proven by the grid rather than by the wall clock.
    ///
    /// This is the test that caught the drift. We re-armed with `CNTV_TVAL_EL0` (a *relative*
    /// countdown), so every period was `interval + handler latency`, the lateness compounded,
    /// and 100 Hz became about 70 Hz. Silently. `CNTV_CVAL_EL0` puts the deadlines on a fixed
    /// grid and the rate is right.
    ///
    /// The original assertion compared delivered ticks against elapsed counter time, one period
    /// of slack either way. That measures the emulator as much as the re-arm: the test runner
    /// passes no `-icount`, so `CNTVCT_EL0` follows host time, and a host that deschedules the
    /// vCPU for a few periods coalesces ticks into exactly the deficit the TVAL defect produced.
    /// It failed a quiet-window gate run on 2026-08-03 as "22 ticks in 25 periods" while the
    /// host compiled beside QEMU, the same shape its riscv64 twin kept failing CI with, and no
    /// margin separates "our re-arm is late" from "the emulator was not running"
    /// (notes/load-sensitive-assertions.md).
    ///
    /// So assert the re-arm LAW instead, the property this test exists for: over a window in
    /// which no miss was recorded, [`rearm`] moved `CNTV_CVAL_EL0` by **exactly one interval per
    /// delivered tick**. The TVAL defect fails this on the first tick (each re-arm lands late by
    /// the handler latency, so the sum overshoots the grid); a descheduled emulator cannot fail
    /// it, because a deschedule long enough to slip the grid is counted by `MISSED_TICKS` and
    /// the window is retried. Kept in lockstep with the riscv64 twin, whose grid is software
    /// (`DEADLINE`) because SBI's `set_timer` is write-only where CVAL reads back.
    #[test_case]
    fn ticks_arrive_at_the_configured_rate() {
        use crate::arch::timer;

        // A consistent (ticks, missed, deadline, counter) snapshot without masking: a tick
        // between the reads would skew them, so re-read until the tick count brackets the others
        // unchanged. The core id is in the bracket too: the statics and CVAL are per core, and a
        // snapshot pair taken on two cores compares unrelated grids.
        let snapshot = || loop {
            let core = crate::cpu::id();
            let t = timer::ticks();
            let m = timer::missed_ticks();
            let d = timer::deadline();
            let c = timer::now();
            if timer::ticks() == t && crate::cpu::id() == core {
                break (core, t, m, d, c);
            }
        };

        // A miss re-anchors the grid, which is correct behaviour (`rearm`'s safety valve), so a
        // window containing one proves nothing about the law either way: retry it. Eight
        // quarter-second windows all containing a miss is not load, it is either a pathological
        // host or a handler genuinely slower than a tick period, and both deserve a red run.
        let mut attempts = 0;
        let (elapsed_ticks, deadline_delta, expected) = loop {
            let (k0, t0, m0, d0, c0) = snapshot();
            timer::spin_for(timer::frequency() / 4); // a quarter of a second, by the counter
            let (k1, t1, m1, d1, c1) = snapshot();

            if k0 == k1 && m0 == m1 && t1 - t0 >= 2 {
                break (t1 - t0, d1 - d0, (c1 - c0) / timer::interval());
            }
            attempts += 1;
            assert!(
                attempts < 8,
                "no miss-free measurement window in eight tries: either the host is too \
                 contended to observe the grid, or the handler is slower than a whole tick \
                 period (see the_handler_keeps_up_when_no_lock_is_held)"
            );
        };

        assert_eq!(
            deadline_delta,
            elapsed_ticks * timer::interval(),
            "timer drift: {elapsed_ticks} ticks moved CNTV_CVAL_EL0 off the grid. Re-arming with \
             a RELATIVE countdown (TVAL) instead of an absolute deadline (CVAL) does exactly this."
        );

        // The one wall-clock bound a contended host cannot falsify: descheduling only DROPS
        // ticks, so more ticks than elapsed periods (plus one for starting mid-period) means the
        // timer is firing faster than the grid, which is `rearm`'s spin-forever failure mode.
        assert!(
            elapsed_ticks <= expected + 1,
            "{elapsed_ticks} ticks in {expected} periods: the timer fires faster than configured"
        );
    }

    /// The handler keeps up, **when nothing is holding a lock**.
    ///
    /// A missed deadline means a whole tick period elapsed before we re-armed. With interrupts
    /// live and no critical section in the way, that would mean the handler itself is too slow,
    /// which at milestone 6 would mean threads losing their time slices.
    ///
    /// Measured as a *delta*, not an absolute. The count is deliberately nonzero by now: the
    /// test below causes misses on purpose.
    #[test_case]
    fn the_handler_keeps_up_when_no_lock_is_held() {
        use crate::arch::timer;

        // Core-scoped, for `ticks_on`'s reason: a migration between the two reads compares two
        // cores' miss counts. That removes one of this test's two confounds. **The other is not
        // fixed here and cannot be**: with no `-icount`, a deschedule long enough to pass a
        // deadline is counted as a miss and the guest cannot tell it from a slow handler, which is
        // what the numbers below are for. See notes/load-sensitive-assertions.md.
        let core = crate::cpu::id();
        let before = timer::missed_ticks_on(core);
        timer::spin_for(timer::interval() * 5);
        let after = timer::missed_ticks_on(core);

        // The numbers, not just the fact. `now - next` is how late the re-arm was: a few hundred
        // counter ticks is a slow handler and is our bug; a whole `interval` or more is the
        // emulator having been descheduled, which says nothing about this kernel. The test runner
        // passes no `-icount`, so guest time follows host time and both are possible. Milestone 78
        // decides what this assertion should measure; until then it at least reports which it saw.
        let (now, next, misses) = super::miss_detail::last();
        assert_eq!(
            after,
            before,
            "the timer handler is taking longer than a whole tick period, with no lock held. \
             missed {} -> {}; last miss re-armed {} counter ticks late against an interval of {} \
             ({} misses recorded on this core). Late by much more than one interval means the \
             emulator was descheduled, not that the handler is slow.",
            before,
            after,
            now.saturating_sub(next),
            timer::interval(),
            misses,
        );
    }

    /// **The cost of masking, made visible.**
    ///
    /// `IrqSafeMutex` prevents the deadlock by masking interrupts for as long as the lock is
    /// held. That is not free, and this is the bill: hold a lock across a tick deadline and the
    /// tick is *late*. Hold it across more than one and a tick is **lost outright**: the
    /// deadline passes, we re-arm to a deadline already in the past, and the only sane thing to
    /// do is give up on it and re-anchor.
    ///
    /// This is exactly why DECISIONS.md §9 says **keep critical sections short**, and it is the
    /// reason that rule has teeth rather than being good manners. At milestone 6, a lost tick is
    /// a thread that didn't get preempted.
    ///
    /// The test asserts the cost is *real*, which is a strange thing to assert until you notice
    /// that if it ever stopped being real, `IrqSafeMutex` would have stopped masking, and the
    /// deadlock would be back.
    #[test_case]
    fn a_long_critical_section_costs_a_tick() {
        use crate::arch::timer;
        use crate::sync::{IrqSafeMutex, rank};

        static M: IrqSafeMutex<u32> = IrqSafeMutex::new(rank::FRAMES, 0);

        // `before` is read **inside** the critical section, and it names its core. Interrupts are
        // masked in there, so this thread can neither be preempted nor migrated and the measured
        // window is exactly the window under test. Read outside, it straddled a preemption point
        // and compared per-core counters across a possible steal (see `ticks_on`).
        let (core, before) = {
            let _guard = M.lock();
            let core = crate::cpu::id();
            let before = timer::missed_ticks_on(core);
            // Two whole tick periods with interrupts masked. At least one deadline passes while
            // we cannot service it.
            timer::spin_for(timer::interval() * 2 + timer::interval() / 2);
            (core, before)
        };

        // Let the pending interrupt land and the miss be counted. Bounded rather than a fixed
        // single period: the claim is that the miss *happens*, and a host that descheduled the
        // emulator only makes the delivery later.
        assert!(
            within_periods(20, || timer::missed_ticks_on(core) > before),
            "holding a lock across two tick periods did NOT lose a tick, which means \
             IrqSafeMutex is not masking interrupts and the deadlock is live"
        );
    }

    /// Uptime comes from the *counter*, not the tick count.
    ///
    /// If a tick were ever missed, `ticks * 10ms` would undercount and time would appear to
    /// slow down. The hardware counter cannot lie. This is what `Instant` is made of, and it is
    /// the thing `core` could never give us: nothing in `core` knows what time it is.
    #[test_case]
    fn uptime_advances_monotonically() {
        use crate::arch::timer;

        let a = timer::uptime_ms();
        timer::spin_for(timer::frequency() / 50); // 20 ms
        let b = timer::uptime_ms();

        assert!(b > a, "uptime went backwards or stalled: {a} -> {b}");
        assert!(
            b - a >= 15,
            "uptime advanced {} ms in 20 ms of counter time",
            b - a
        );
    }

    /// **THE TEST.**
    ///
    /// Everything in DECISIONS.md §9 and notes/locking.md exists to prevent one thing: a timer
    /// interrupt landing inside a critical section, taking the same lock, and spinning forever
    /// waiting for code that cannot run until it returns. On one core. Permanently.
    ///
    /// Until this milestone that was a hypothesis. There were no interrupts. Now there are, and
    /// this is the proof:
    ///
    ///   1. confirm ticks are flowing
    ///   2. take a lock and busy-wait across **three whole tick periods**
    ///   3. assert not one tick landed
    ///   4. release, and watch them resume
    ///
    /// Step 2 works because `spin_for` reads `CNTVCT_EL0`, the hardware counter, which **keeps
    /// counting while interrupts are masked**. A tick-based delay would simply hang here, which
    /// is its own kind of proof.
    #[test_case]
    fn holding_a_lock_masks_the_timer() {
        use crate::arch::{interrupts, timer};
        use crate::sync::{IrqSafeMutex, rank};

        static M: IrqSafeMutex<u32> = IrqSafeMutex::new(rank::FRAMES, 0);

        assert!(interrupts::enabled(), "test setup: interrupts should be on");

        // The timer is alive. Core-scoped, for `ticks_on`'s reason.
        let alive_on = crate::cpu::id();
        let t0 = timer::ticks_on(alive_on);
        timer::spin_for(timer::interval() * 2);
        assert!(
            timer::ticks_on(alive_on) > t0,
            "the timer is not ticking at all"
        );

        // **Both reads of the tick count happen inside the critical section**, and that is the
        // fix milestone 78 made here. `before` used to be read before `M.lock()`, which makes the
        // measured window wider than the property: a tick landing in the handful of instructions
        // between the read and the mask is charged to the lock. A contended host puts a tick there
        // almost by construction, because a descheduled vCPU resumes with its deadline already
        // past and takes the interrupt at the first instruction it executes. The riscv64 twin
        // failed CI that way on 2026-08-04 ("left: 41, right: 40", one surplus tick, on `rv64`,
        // the control model). The same window also straddled a preemption point, and TICKS is per
        // core, so a steal (§28.3) moving this thread compared two unrelated counters.
        let (core, before) = {
            let _guard = M.lock();
            // Interrupts are masked from here, so this core cannot switch threads: `cpu::id()` is
            // fixed for the whole block and both reads below are of one counter.
            let core = crate::cpu::id();
            let before = timer::ticks_on(core);

            // Thirty milliseconds. Three ticks' worth. Not one of them may land.
            timer::spin_for(timer::interval() * 3);

            assert_eq!(
                timer::ticks_on(core),
                before,
                "A TIMER INTERRUPT FIRED WHILE A LOCK WAS HELD. IrqSafeMutex is not masking, \
                 and the deadlock in notes/locking.md is live: a handler that touched this lock \
                 would spin forever waiting for code that cannot run."
            );
            (core, before)
        };

        // And the moment we let go, the pending interrupt is delivered. Bounded rather than a
        // fixed two periods, and read by core index because dropping the guard is a preemption
        // point: this thread may be on another core by the next instruction, and the core we left
        // keeps ticking either way.
        assert!(
            within_periods(20, || timer::ticks_on(core) > before),
            "interrupts did not resume after the lock was released: `restore` is broken"
        );
    }

    /// Wait for `cond`, bounded in **tick periods of the free-running counter**, checking once a
    /// period.
    ///
    /// Every use is on the "has it happened yet" side of a property (a pending interrupt being
    /// delivered, a miss being counted), which is the direction where a busy host produces a late
    /// pass rather than a wrong answer. The fixed spins these replaced turned a late delivery into
    /// a failure. See notes/load-sensitive-assertions.md.
    fn within_periods(periods: u32, mut cond: impl FnMut() -> bool) -> bool {
        for _ in 0..periods {
            if cond() {
                return true;
            }
            crate::arch::timer::spin_for(crate::arch::timer::interval());
        }
        cond()
    }
}
