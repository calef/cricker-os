//! **The timer, RISC-V.** The S-mode analog of aarch64's generic virtual timer.
//!
//! RISC-V exposes a free-running counter through the `time` CSR (read with `rdtime`). The next tick
//! is scheduled through the **SBI TIME** extension (`sbi_set_timer`, an `ecall` to OpenSBI in
//! M-mode), which both arms the next S-mode timer interrupt and clears the pending one. The Sstc
//! extension's `stimecmp` CSR would avoid the M-mode round trip, but SBI TIME works on every
//! OpenSBI and is the portable choice for now. See notes/riscv-port.md.
//!
//! # The deadline is remembered in software, and that is the whole difference
//!
//! aarch64 re-arms from `CNTV_CVAL_EL0`, an absolute deadline it can **read back out of the
//! hardware**, so `next = CVAL + interval` puts the deadlines on a fixed grid for free. RISC-V's
//! SBI `set_timer` is write-only: the firmware takes an absolute `time` value and gives nothing
//! back. So the grid has to be kept here, in [`DEADLINE`].
//!
//! Until milestone 19's test lane this file did the easy thing instead and re-armed with
//! `now() + interval` from inside the handler, which is **the same drift bug aarch64 shipped and
//! measured**: `now()` is read after the trap entry and the SBI round trip, so every period is
//! `interval + latency`, the lateness compounds, and the configured rate is not the delivered rate.
//! Nothing caught it because this ISA had no timer tests. See notes/riscv-arch-tests.md.

use crate::cpu::{self, MAX_CPUS};
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

/// The S-mode timer interrupt cause (`scause` = 5, the Supervisor timer interrupt). The RISC-V
/// analog of aarch64's per-CPU timer INTID.
pub const TIMER_INTID: u32 = 5;

/// Ticks per second, the preemption rate. Same 100 Hz as aarch64.
pub const TICK_HZ: u64 = 100;

/// The `time` CSR frequency on QEMU's `virt` machine: 10 MHz. Properly this comes from the device
/// tree (`/cpus/timebase-frequency`); hardcoded until the DTB parse lands, and flagged so.
const TIMEBASE_HZ: u64 = 10_000_000;

/// `sie.STIE`, bit 5: the Supervisor Timer Interrupt Enable.
const STIE: u64 = 1 << 5;

/// Ticks since boot, maintained by [`tick`]. **Per hart**, like aarch64's.
///
/// It was one global until milestone 19's test lane, which is wrong for the same reason DECISIONS
/// §11 gives on aarch64: the timer is per hart, so a single counter is advanced by every hart's tick
/// and "holding a lock stops *my* ticks" stops being observable. Masking `sstatus.SIE` masks this
/// hart alone; the other three keep counting into the same word.
static TICKS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Deadlines that had already passed by the time this hart re-armed. **Should be zero.** A nonzero
/// count means a whole tick period elapsed inside the handler or inside a critical section, which is
/// a real problem and not a rounding error. Per hart, for the reason [`TICKS`] is.
static MISSED_TICKS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// The absolute `time` value this hart's next tick is due at: the grid, anchored when this hart
/// armed its first deadline. aarch64 keeps this in `CNTV_CVAL_EL0` and reads it back; SBI's
/// `set_timer` is write-only, so we keep our own copy (see the module header).
static DEADLINE: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Call SBI TIME `set_timer(next)`: schedule the next S-mode timer interrupt for when the `time`
/// counter reaches `next`, and clear any pending timer interrupt. An `ecall` from S-mode traps to
/// OpenSBI in M-mode.
fn sbi_set_timer(next: u64) {
    const SBI_TIME_EID: usize = 0x5449_4D45; // "TIME"
    const SBI_SET_TIMER_FID: usize = 0;
    // SAFETY: an SBI call. a7 = extension id, a6 = function id, a0 = the absolute deadline. The
    // firmware clobbers a0/a1 (the return); nothing else.
    unsafe {
        asm!(
            "ecall",
            in("a7") SBI_TIME_EID,
            in("a6") SBI_SET_TIMER_FID,
            inout("a0") next => _,
            lateout("a1") _,
            options(nostack),
        );
    }
}

/// The free-running counter (`rdtime`). Real: a leaf read with no dependency on the timer being set
/// up, the RISC-V counterpart of reading `CNTVCT`.
pub fn now() -> u64 {
    let t: u64;
    // SAFETY: reads the time CSR. No side effects.
    unsafe { asm!("rdtime {}", out(reg) t, options(nomem, nostack, preserves_flags)) };
    t
}

/// The counter's frequency in Hz.
pub fn frequency() -> u64 {
    TIMEBASE_HZ
}

/// Counter ticks between two timer interrupts (the reload interval): one tick period.
pub fn interval() -> u64 {
    TIMEBASE_HZ / TICK_HZ
}

/// Start the periodic timer: arm the first deadline through SBI, and enable the S-mode timer
/// interrupt in `sie`. The caller enables interrupts globally (`sstatus.SIE`) when it is ready to
/// take them.
pub fn init() {
    // Anchor this hart's grid, then arm it. Every later deadline is this one plus a whole number of
    // intervals, so however long a handler takes, the tick after it is still where it was going to
    // be. See `rearm`.
    let first = now() + interval();
    DEADLINE[cpu::id()].store(first, Ordering::Relaxed);
    sbi_set_timer(first);
    // SAFETY: setting sie.STIE only unmasks the timer source; it takes effect once SIE is on.
    unsafe { asm!("csrs sie, {}", in(reg) STIE, options(nomem, nostack, preserves_flags)) };

    // Let U-mode read the `time` CSR (`rdtime`), the RISC-V twin of aarch64 opening
    // `CNTKCTL_EL1.EL0VCTEN` in the arch/aarch64 timer. `crates/user_rt`'s `now()` needs it, and
    // through it so do std's `Instant`, `thread::sleep` and the random seed.
    //
    // **This was a latent board bug, not a new feature.** `user_rt` documented U-mode `rdtime` as
    // working "because the kernel sets scounteren.TM"; the kernel never set it. It worked anyway
    // because QEMU's OpenSBI leaves the bit permitted, so the whole riscv std stack (smoltcp's
    // timestamps in std_net, for one) has been riding firmware default rather than anything we
    // chose. On a platform whose firmware clears it, every std program would take an illegal
    // instruction trap the first time it read a clock, which is a miserable thing to debug during
    // board bring-up. Setting it here makes the documented claim true and removes the dependency.
    //
    // Per-hart, so it belongs in this per-hart init: `scounteren` is not shared between harts, and a
    // secondary that skipped it would fault only on the threads that happened to land there.
    //
    // SAFETY: setting scounteren.TM only *permits* a read U-mode could already attempt; it grants no
    // authority to affect anything. The same eyes-open exception to §10 that aarch64 records, with
    // the same reason: the cross-OS primitive suite needs userspace self-timing to be comparable to
    // lmbench. CY (cycle) and IR (instret) stay closed.
    const TM: u64 = 1 << 1;
    unsafe { asm!("csrs scounteren, {}", in(reg) TM, options(nomem, nostack, preserves_flags)) };
}

/// Handle a timer interrupt: count the tick and arm the next deadline (which also clears the pending
/// interrupt). Called from the trap dispatcher on `scause` = timer.
pub fn tick() {
    TICKS[cpu::id()].fetch_add(1, Ordering::Relaxed);
    // In a test build, feed the hang watchdog: a lost IPC wakeup would otherwise block a test
    // forever and the whole run would hang silently. Driven from the timer so it costs nothing.
    #[cfg(test)]
    crate::testing::watchdog_tick();
    rearm();
}

/// Move this hart's deadline forward by exactly one interval.
///
/// **`deadline + interval`, not `now + interval`.** That is the entire point, and it is aarch64's
/// `rearm` with the register read replaced by a load: the deadlines sit on a grid anchored at
/// [`init`], so a slow handler makes *one* tick late instead of pushing every later one out too.
///
/// The `if` is the safety valve. If we fell so far behind that the next deadline is already in the
/// past, arming it would fire immediately, and again, and spin here forever paying off a debt we
/// cannot afford. So we give up on the missed tick, count it, and re-anchor the grid to now. Every
/// kernel calls this dropping ticks.
fn rearm() {
    let id = cpu::id();
    let now = now();
    let mut next = DEADLINE[id].load(Ordering::Relaxed) + interval();

    if next <= now {
        MISSED_TICKS[id].fetch_add(1, Ordering::Relaxed);
        next = now + interval();
    }

    DEADLINE[id].store(next, Ordering::Relaxed);
    sbi_set_timer(next);
}

/// Ticks since boot, **on this hart**.
pub fn ticks() -> u64 {
    TICKS[cpu::id()].load(Ordering::Relaxed)
}

/// Milliseconds since boot, from the free-running counter (independent of the tick interrupt).
///
/// Part of the arch timer contract rather than of any caller; this file's tests are what exercise
/// it, exactly as on aarch64.
#[cfg_attr(not(test), allow(dead_code))]
pub fn uptime_ms() -> u64 {
    now() / (TIMEBASE_HZ / 1000)
}

/// Busy-wait for `counter_ticks` of the free-running counter.
pub fn spin_for(counter_ticks: u64) {
    let start = now();
    while now().wrapping_sub(start) < counter_ticks {
        core::hint::spin_loop();
    }
}

/// This hart's missed ticks: deadlines that had already passed when [`rearm`] ran.
///
/// **This used to be a stub returning 0**, and the comment on it argued that a missed tick was not a
/// meaningful idea on RISC-V because SBI `set_timer` re-arms from `now`. That was backwards: re-arming
/// from `now` is what made the count unmeasurable, not what made it unnecessary. With the grid in
/// [`DEADLINE`] the count is real, and it is what makes the cost of masking interrupts visible (see
/// this file's `a_long_critical_section_costs_a_tick`).
#[cfg_attr(not(test), allow(dead_code))]
pub fn missed_ticks() -> u64 {
    MISSED_TICKS[cpu::id()].load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    //! Tests for the SBI timer, and for the thing the whole locking discipline was written to
    //! prevent.
    //!
    //! The RISC-V twins of the aarch64 timer tests. Every delay here is measured with `rdtime`, the
    //! free-running counter, because it **keeps counting while interrupts are masked**, which is
    //! exactly the condition three of these tests need to observe. A tick-based delay would simply
    //! hang, which is its own kind of proof and no use as a test.

    /// The heartbeat is beating.
    #[test_case]
    fn the_timer_is_ticking() {
        use crate::arch::timer;

        let before = timer::ticks();
        timer::spin_for(timer::interval() * 3);
        let after = timer::ticks();

        assert!(
            after > before,
            "no timer interrupt in three tick periods: SBI set_timer, sie.STIE, or the scause=5 \
             arm of the trap dispatcher is not delivering"
        );
    }

    /// Ticks arrive at roughly the rate we asked for.
    ///
    /// This is the test aarch64 wrote after measuring 100 Hz configured and ~70 Hz delivered, and
    /// **RISC-V had the same defect** when this test was written: `tick` re-armed with
    /// `sbi_set_timer(now() + interval)`, a deadline relative to the moment the handler read the
    /// clock, so every period ran long by the trap entry plus the SBI round trip and the lateness
    /// compounded. The fix is the same shape as aarch64's, with the grid kept in software because
    /// SBI has no register to read a deadline back from. See the module header.
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
            "timer drift: {elapsed_ticks} ticks in {expected} periods. Re-arming from `now()` \
             inside the handler instead of from the previous deadline does exactly this."
        );
    }

    /// The handler keeps up, **when nothing is holding a lock**.
    ///
    /// A missed deadline means a whole tick period elapsed before we re-armed. With interrupts live
    /// and no critical section in the way, that would mean the handler itself is too slow, and a
    /// thread would lose a time slice it was owed.
    ///
    /// Measured as a *delta*, not an absolute: the count is deliberately nonzero by now, because the
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
    /// `IrqSafeMutex` prevents the deadlock by masking interrupts for as long as the lock is held.
    /// That is not free, and this is the bill: hold a lock across more than one tick deadline and a
    /// tick is **lost outright**, because when the handler finally runs, the next deadline on the
    /// grid is already in the past and the only sane thing to do is give up on it and re-anchor.
    ///
    /// This is why DECISIONS §9 says keep critical sections short, and it is what gives that rule
    /// teeth rather than good manners. The test asserts the cost is *real*, which is a strange thing
    /// to assert until you notice that if it stopped being real, `IrqSafeMutex` would have stopped
    /// masking, and the deadlock would be back.
    #[test_case]
    fn a_long_critical_section_costs_a_tick() {
        use crate::arch::timer;
        use crate::sync::{IrqSafeMutex, rank};

        static M: IrqSafeMutex<u32> = IrqSafeMutex::new(rank::FRAMES, 0);

        let before = timer::missed_ticks();

        {
            let _guard = M.lock();
            // Two and a half tick periods with interrupts masked. At least one deadline passes while
            // we cannot service it, and one more passes before we can re-arm.
            timer::spin_for(timer::interval() * 2 + timer::interval() / 2);
        }

        // Let the pending interrupt land.
        timer::spin_for(timer::interval());

        assert!(
            timer::missed_ticks() > before,
            "holding a lock across two tick periods did NOT lose a tick, which means \
             IrqSafeMutex is not masking interrupts and the deadlock in notes/locking.md is live"
        );
    }

    /// Uptime comes from the *counter*, not the tick count.
    ///
    /// If a tick were ever missed, `ticks * 10ms` would undercount and time would appear to slow
    /// down. `rdtime` cannot lie: it is the hardware counter, and it is what `Instant` is made of.
    /// Nothing in `core` knows what time it is; this is where that comes from.
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
    /// Everything in DECISIONS §9 and notes/locking.md exists to prevent one thing: a timer
    /// interrupt landing inside a critical section, taking the same lock, and spinning forever
    /// waiting for code that cannot run until it returns. On one hart. Permanently.
    ///
    /// The RISC-V mechanism is `sstatus.SIE` rather than `PSTATE.DAIF`, and this is its first
    /// witness:
    ///
    ///   1. confirm ticks are flowing
    ///   2. take a lock and busy-wait across **three whole tick periods**
    ///   3. assert not one tick landed
    ///   4. release, and watch them resume
    ///
    /// Step 3 is also what forced [`TICKS`](super::TICKS) to become per-hart. Masking `SIE` masks
    /// this hart; with one global counter the other three harts' ticks landed in the same word and
    /// the assertion could never hold, which is DECISIONS §11's reasoning arriving on the second ISA.
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
                "A TIMER INTERRUPT FIRED WHILE A LOCK WAS HELD. IrqSafeMutex is not masking \
                 sstatus.SIE, and the deadlock in notes/locking.md is live: a handler that touched \
                 this lock would spin forever waiting for code that cannot run."
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
