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

// A few of these have no non-test caller yet: uptime_ms and now() are the Instant milestone 6
// and 8 will build on, and missed_ticks is a health counter. They are exercised by the tests.
#![allow(dead_code)]

use crate::drivers::gic;
use aarch64_cpu::registers::{CNTFRQ_EL0, CNTV_CTL_EL0, CNTV_CVAL_EL0, CNTVCT_EL0};
use core::sync::atomic::{AtomicU64, Ordering};
use tock_registers::interfaces::{Readable, Writeable};

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
static TICKS: AtomicU64 = AtomicU64::new(0);

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

    let interval = freq / TICK_HZ;
    INTERVAL.store(interval, Ordering::Relaxed);

    gic::enable(TIMER_INTID);

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
        MISSED_TICKS.fetch_add(1, Ordering::Relaxed);
        next = now + interval;
    }

    CNTV_CVAL_EL0.set(next);
}

/// Deadlines that had already passed by the time we re-armed. **Should be zero.** A nonzero
/// count means the handler is taking longer than a whole tick period, which is a real problem
/// and not a rounding error.
static MISSED_TICKS: AtomicU64 = AtomicU64::new(0);

pub fn missed_ticks() -> u64 {
    MISSED_TICKS.load(Ordering::Relaxed)
}

/// Called from the IRQ handler. **Must re-arm**, or the interrupt line stays high forever and
/// the machine drowns in its own timer.
///
/// This is the whole handler, and it is deliberately tiny: bump a counter, reload the
/// countdown, return. DECISIONS.md §9 — **interrupt handlers record and defer; they do not do
/// work.** At milestone 6 this will also set a "reschedule wanted" flag, and the *scheduler*
/// will act on it in normal context.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    rearm(INTERVAL.load(Ordering::Relaxed));
}

/// Ticks since boot.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// The raw counter. Monotonic, never wraps in any timescale that matters, and **keeps counting
/// while interrupts are masked** — which is precisely what makes it the only honest way to
/// measure how long a critical section held the CPU.
pub fn now() -> u64 {
    CNTVCT_EL0.get()
}

pub fn frequency() -> u64 {
    CNTFRQ_EL0.get()
}

/// Milliseconds since boot, from the counter rather than from the tick count.
///
/// **Deliberately not `ticks() * 10`.** If an interrupt is ever missed — a long critical
/// section, a slow handler — the tick count undercounts and time appears to slow down. The
/// hardware counter cannot lie. This is `Instant`, and it is the thing `core` could never give
/// us because nothing in `core` knows what time it is.
pub fn uptime_ms() -> u64 {
    let freq = CNTFRQ_EL0.get();
    if freq == 0 {
        return 0;
    }
    now() * 1000 / freq
}

/// Busy-wait. Uses the counter, so it works with interrupts masked, which is exactly when a
/// tick-based delay would hang forever.
pub fn spin_for(counter_ticks: u64) {
    let start = now();
    while now().wrapping_sub(start) < counter_ticks {
        core::hint::spin_loop();
    }
}

/// Counter ticks in one timer period.
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

        let before = timer::ticks();
        timer::spin_for(timer::interval() * 3);
        let after = timer::ticks();

        assert!(
            after > before,
            "no timer interrupt in three tick periods: the GIC or the timer is not delivering"
        );
    }

    /// Ticks arrive at roughly the rate we asked for.
    ///
    /// This is the test that caught the drift. We re-armed with `CNTV_TVAL_EL0` (a *relative*
    /// countdown), so every period was `interval + handler latency`, the lateness compounded,
    /// and 100 Hz became about 70 Hz. Silently. `CNTV_CVAL_EL0` puts the deadlines on a fixed
    /// grid and the rate is right.
    #[test_case]
    fn ticks_arrive_at_the_configured_rate() {
        use crate::arch::timer;

        let t0 = timer::ticks();
        let c0 = timer::now();

        timer::spin_for(timer::frequency() / 4); // a quarter of a second, by the counter

        let elapsed_ticks = timer::ticks() - t0;
        let elapsed_counter = timer::now() - c0;

        // How many ticks *should* have fired in that much counter time?
        let expected = elapsed_counter / timer::interval();

        // Allow one either way: we may start or stop mid-period.
        assert!(
            elapsed_ticks + 1 >= expected && elapsed_ticks <= expected + 1,
            "timer drift: {elapsed_ticks} ticks in {expected} periods. \
             Re-arming with a RELATIVE countdown (TVAL) instead of an absolute deadline (CVAL) \
             does exactly this."
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

        let before = timer::missed_ticks();
        timer::spin_for(timer::interval() * 5);

        assert_eq!(
            timer::missed_ticks(),
            before,
            "the timer handler is taking longer than a whole tick period, with no lock held"
        );
    }

    /// **The cost of masking, made visible.**
    ///
    /// `IrqSafeMutex` prevents the deadlock by masking interrupts for as long as the lock is
    /// held. That is not free, and this is the bill: hold a lock across a tick deadline and the
    /// tick is *late*. Hold it across more than one and a tick is **lost outright** — the
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

        let before = timer::missed_ticks();

        {
            let _guard = M.lock();
            // Two whole tick periods with interrupts masked. At least one deadline passes while
            // we cannot service it.
            timer::spin_for(timer::interval() * 2 + timer::interval() / 2);
        }

        // Let the pending interrupt land.
        timer::spin_for(timer::interval());

        assert!(
            timer::missed_ticks() > before,
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

        // The timer is alive.
        let t0 = timer::ticks();
        timer::spin_for(timer::interval() * 2);
        assert!(timer::ticks() > t0, "the timer is not ticking at all");

        let before = timer::ticks();

        {
            let _guard = M.lock();

            // Thirty milliseconds. Three ticks' worth. Not one of them may land.
            timer::spin_for(timer::interval() * 3);

            assert_eq!(
                timer::ticks(),
                before,
                "A TIMER INTERRUPT FIRED WHILE A LOCK WAS HELD. IrqSafeMutex is not masking, \
                 and the deadlock in notes/locking.md is live: a handler that touched this lock \
                 would spin forever waiting for code that cannot run."
            );
        }

        // And the moment we let go, the pending interrupt is delivered.
        timer::spin_for(timer::interval() * 2);
        assert!(
            timer::ticks() > before,
            "interrupts did not resume after the lock was released: `restore` is broken"
        );
    }
}
