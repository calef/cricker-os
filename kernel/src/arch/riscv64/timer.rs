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

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::cpu::{self, MAX_CPUS};

/// The S-mode timer interrupt cause (`scause` = 5, the Supervisor timer interrupt). The RISC-V
/// analog of aarch64's per-CPU timer INTID.
pub const TIMER_INTID: u32 = 5;

/// Ticks per second, the preemption rate. Same 100 Hz as aarch64.
pub const TICK_HZ: u64 = 100;

/// **The `time` CSR frequency, read from the machine** (`/cpus/timebase-frequency`, milestone 100).
///
/// It was a `const` of 10 MHz, QEMU `virt`'s number, carrying the comment "hardcoded until the DTB
/// parse lands". The DTB parse landed with milestone 60 and the comment outlived it by two months.
/// It was also a parity gap under rule 5: aarch64's twin has always read `CNTFRQ_EL0` and asserted
/// the value nonzero (`arch/aarch64/timer.rs`), so the two ISAs disagreed about whether the machine
/// gets to say how fast its own clock runs. RISC-V has no `CNTFRQ_EL0`; the device tree is the
/// architected answer, and the binding requires it.
///
/// Zero until [`init_frequency`] runs, which is a deliberate poison: every interval computed from it
/// would divide by zero, so a boot that forgot to read the machine cannot quietly run at QEMU's rate
/// on a board with a different one. The two readers assert it instead ([`frequency`], [`interval`]).
static TIMEBASE_HZ: AtomicU64 = AtomicU64::new(0);

/// **Read the counter rate out of the device tree.** Called once, on the boot hart, immediately
/// before [`init`] arms the first deadline on it.
///
/// It is a separate call rather than part of `init` because `init` also runs on every secondary,
/// which has no device-tree pointer and needs none: the counter is machine-wide, so the boot hart's
/// read serves them all. That is also the limitation worth naming, since the RISC-V binding permits
/// a per-hart `timebase-frequency` and a machine whose harts genuinely differ would be misread here.
/// `crates/isa`'s `cpu_list` reads `/cpus` first and falls back to the first hart's own property.
///
/// # Panics
///
/// If the tree does not state a frequency. The binding requires it, there is no architected register
/// to fall back to, and the previous behaviour (assume QEMU's 10 MHz) is what this milestone exists
/// to delete: a timer running at the wrong rate is a scheduler that preempts at the wrong rate and a
/// `sleep` that returns at the wrong time, with nothing anywhere reporting a fault.
pub fn init_frequency(dtb_ptr: usize) {
    // SAFETY: the pointer OpenSBI handed us in `a1`, named through the boot table's direct map.
    // `Dtb::from_ptr` re-checks the magic, and this runs on the same map `memory::init` uses.
    let dt = unsafe { dtb::Dtb::from_ptr(super::mmu::phys_to_virt(dtb_ptr as u64) as *const u8) }
        .expect("device tree is unreadable");
    let list = isa::cpu_list::CpuList::from_device_tree(&dt).expect("cannot read /cpus");
    let hz = list
        .timebase_hz
        .expect("the device tree states no /cpus/timebase-frequency, and RISC-V has no CNTFRQ_EL0");
    assert!(hz > 0, "a counter that never advances cannot drive a tick");
    TIMEBASE_HZ.store(hz, Ordering::Relaxed);
}

/// The counter rate the machine stated. Panics if read before [`init_frequency`], which is the same
/// discipline `arch::isa::get` uses: a plausible zero is worse than a panic naming this file.
fn timebase_hz() -> u64 {
    let hz = TIMEBASE_HZ.load(Ordering::Relaxed);
    assert!(
        hz > 0,
        "arch::timer used before arch::timer::init_frequency"
    );
    hz
}

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

/// The counter's frequency in Hz, as the machine stated it.
pub fn frequency() -> u64 {
    timebase_hz()
}

/// Counter ticks between two timer interrupts (the reload interval): one tick period.
pub fn interval() -> u64 {
    timebase_hz() / TICK_HZ
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
    // SAFETY: `csrs` sets a bit in a supervisor CSR and touches no memory, which the options state. Enabling the EL0 counter read is the intent (the userspace self-timing seam).
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
    // The deadline that just fired, named rather than folded into the sum below: milestone 78's
    // instruction-count instrument compares the arrival against it, and on this ISA that comparison
    // is the whole claim (SBI's `set_timer` is write-only, so nothing else proves the firmware was
    // armed with the word this array holds).
    let fired = DEADLINE[id].load(Ordering::Relaxed);
    let mut next = fired + interval();

    if next <= now {
        MISSED_TICKS[id].fetch_add(1, Ordering::Relaxed);
        // Test builds only: keep the numbers, so a failure says HOW LATE rather than only that it
        // was late. Three relaxed stores, no branch on the hot path, and nothing printed: this runs
        // in trap context and DECISIONS §9's rule (handlers record and defer) applies to
        // diagnostics too. aarch64's `rearm` has carried this since milestone 78; the twin was the
        // rule-5 gap this closes.
        #[cfg(test)]
        miss_detail::record(now, next);
        next = now + interval();
    }

    DEADLINE[id].store(next, Ordering::Relaxed);
    sbi_set_timer(next);

    // The instruction-count instrument (milestone 78), in the boot mode that owns it and nowhere
    // else. **After the SBI call on purpose**: the `ecall` into M-mode firmware and back is a real
    // part of what this handler costs and is the one part aarch64 does not pay, so a measurement
    // that stopped short of it would compare two different spans across the ISAs. Absent from the
    // test and shipping builds. See kernel/src/icount.rs.
    #[cfg(feature = "icount")]
    crate::icount::tick_trace::record(fired, now, super::timer::now());
}

/// **Instructions between a deadline and the handler observing it** (milestone 78's instrument),
/// the ceiling `icount::run` asserts, and on this ISA the claim the milestone was left holding.
///
/// `DEADLINE` is the kernel's own array; SBI's `set_timer` is write-only, so reading it back proves
/// only that we can remember what we meant to write. This bound is what proves the firmware was
/// armed with it: the span from that word to the trap handler observing the interrupt, in
/// instructions, on a clock the host cannot move. An implementation that kept `DEADLINE` on the
/// grid and armed SBI from `now()` would pass every other test in this tree and leave this bound
/// within a few ticks.
#[cfg(feature = "icount")]
pub const ARRIVAL_BOUND: u64 = 1_500;

/// **Instructions from a deadline to the next one being armed** (milestone 78's instrument): the
/// whole handler, [`ARRIVAL_BOUND`]'s span plus the tick bookkeeping, the `DEADLINE` store and the
/// SBI `ecall` that arms the firmware.
///
/// Larger than the aarch64 twin's for a reason that is real rather than sloppy: this ISA arms its
/// timer through a firmware call, and the round trip into OpenSBI is inside the span. A miss is
/// this number exceeding one tick period, which is 10,000,000 instructions of virtual time at
/// 100 Hz, so the bound is three orders of magnitude tighter than the assertion it replaces.
#[cfg(feature = "icount")]
pub const HANDLER_BOUND: u64 = 2_500;

/// Execute `2 * iters` instructions and return that count, so `icount::calibrate` can check the
/// virtual clock against a known instruction count and refuse to measure anything if this boot is
/// not on the instrument. The aarch64 twin, with this ISA's two-instruction loop.
///
/// `addi`/`bnez` is two instructions per iteration. Both may be assembled compressed on this target
/// (`riscv64imac`), which changes their encoding and not their count: `-icount` counts instructions
/// retired, and a compressed instruction is one.
///
/// **The return is the loop body only**; materializing the operand costs a couple of instructions
/// either side, which is why the caller's tolerance is a percentage of a seven-figure window rather
/// than an equality.
///
/// Here rather than in `icount.rs` because DECISIONS §3 puts every `asm!` under `arch/`.
#[cfg(feature = "icount")]
pub fn calibration_loop(iters: u64) -> u64 {
    // SAFETY: a self-contained counted loop in a caller-saved scratch register. It touches no
    // memory, makes no call, and leaves the register dead, which is what the operand spec and
    // `nomem`/`nostack` state.
    unsafe {
        asm!(
            "2:",
            "addi {n}, {n}, -1",
            "bnez {n}, 2b",
            n = inout(reg) iters => _,
            options(nomem, nostack),
        );
    }
    2 * iters
}

/// Ticks since boot, **on this hart**.
pub fn ticks() -> u64 {
    ticks_on(cpu::id())
}

/// Ticks since boot on a **named** hart, which is what a test needs when the thread doing the
/// measuring can move between the two reads.
///
/// [`TICKS`] is per hart, so `ticks()` before a wait and `ticks()` after it are the same counter
/// only if nothing migrated the caller in between; a kernel thread on a run queue can be stolen by
/// an idle core at any preemption point (DECISIONS §28.3). Reading by index makes the pair name one
/// hart on purpose. See notes/load-sensitive-assertions.md.
#[cfg_attr(not(test), allow(dead_code))]
pub fn ticks_on(hart: usize) -> u64 {
    TICKS[hart].load(Ordering::Relaxed)
}

/// Milliseconds since boot, from the free-running counter (independent of the tick interrupt).
///
/// Part of the arch timer contract rather than of any caller; this file's tests are what exercise
/// it, exactly as on aarch64.
#[cfg_attr(not(test), allow(dead_code))]
pub fn uptime_ms() -> u64 {
    now() / (timebase_hz() / 1000)
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
    missed_ticks_on(cpu::id())
}

/// This hart's missed ticks, by hart index. The [`ticks_on`] argument, applied to the miss count:
/// a test that reads it either side of a wait must name the hart, or a migration compares two
/// unrelated counters.
#[cfg_attr(not(test), allow(dead_code))]
pub fn missed_ticks_on(hart: usize) -> u64 {
    MISSED_TICKS[hart].load(Ordering::Relaxed)
}

/// This hart's next armed deadline (test support): the grid cell [`rearm`] advances from.
///
/// Exposed so the drift test can assert the re-arm law directly (deadlines advance by exactly one
/// interval per delivered tick) instead of inferring it from a wall-clock tick rate, which a
/// descheduled emulator falsifies. aarch64 reads its deadline back out of `CNTV_CVAL_EL0`; this is
/// the software copy that SBI's write-only `set_timer` forces us to keep anyway (module header).
#[cfg_attr(not(test), allow(dead_code))]
pub fn deadline() -> u64 {
    DEADLINE[cpu::id()].load(Ordering::Relaxed)
}

/// Why a miss happened, kept only in test builds. The aarch64 twin, word for word in intent.
///
/// [`missed_ticks`] says a deadline was already past when [`rearm`] ran. It does not say by how
/// much, and the difference is the whole taxonomy: **late by less than one interval is a slow
/// handler and is this kernel's bug; late by a whole interval or more is the emulator having been
/// descheduled and says nothing about this kernel.** Without the numbers those two are the same
/// observation from inside the guest, which is the position milestone 78 records the suite being
/// in, and which broke three unrelated pull requests on aarch64 in one afternoon before the twin
/// there got these numbers.
///
/// The last miss only, plus a count. A burst records once and reports the final pair, which is
/// enough to tell the two cases apart and cheaper than a ring buffer in trap context.
#[cfg(test)]
pub mod miss_detail {
    use core::sync::atomic::{AtomicU64, Ordering};

    use crate::cpu::{self, MAX_CPUS};

    static NOW: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
    static NEXT: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
    static COUNT: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

    /// Record the counter and the deadline it had already passed. Called from [`super::rearm`], in
    /// trap context, so this is three relaxed stores and nothing else.
    pub fn record(now: u64, next: u64) {
        let id = cpu::id();
        NOW[id].store(now, Ordering::Relaxed);
        NEXT[id].store(next, Ordering::Relaxed);
        COUNT[id].fetch_add(1, Ordering::Relaxed);
    }

    /// This hart's last miss as `(now, next, count)`. `now - next` is how late the re-arm was, in
    /// counter ticks; compare it against [`super::interval`] to tell a slow handler from a
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

#[cfg(test)]
mod tests {
    //! Tests for the SBI timer, and for the thing the whole locking discipline was written to
    //! prevent.
    //!
    //! The RISC-V twins of the aarch64 timer tests. Every delay here is measured with `rdtime`, the
    //! free-running counter, because it **keeps counting while interrupts are masked**, which is
    //! exactly the condition three of these tests need to observe. A tick-based delay would simply
    //! hang, which is its own kind of proof and no use as a test.

    /// **The counter rate came out of the device tree, not out of this file** (milestone 100).
    ///
    /// The tree is re-read here, independently of the boot path, and the two answers must agree.
    /// That is what makes this a test of *reading* rather than of a constant: `TIMEBASE_HZ` was
    /// 10 MHz compiled in and QEMU `virt` is 10 MHz, so an assertion against the number alone would
    /// pass just as happily on the old code. Asserting against the blob would not.
    ///
    /// aarch64 has no twin because it needs none: `CNTFRQ_EL0` is architected, and its timer has
    /// always read it and asserted it nonzero. That asymmetry was the rule-5 parity gap this closed.
    #[test_case]
    fn the_counter_rate_is_the_one_the_tree_states() {
        use core::sync::atomic::Ordering as O;

        let ptr = crate::DTB.load(O::Relaxed);
        // SAFETY: the pointer firmware handed us, already parsed twice on this boot.
        let dt =
            unsafe { dtb::Dtb::from_ptr(crate::arch::mmu::phys_to_virt(ptr as u64) as *const u8) }
                .expect("device tree is unreadable");
        let stated = isa::cpu_list::CpuList::from_device_tree(&dt)
            .expect("cannot read /cpus")
            .timebase_hz
            .expect("QEMU virt states /cpus/timebase-frequency");

        assert_eq!(
            crate::arch::timer::frequency(),
            stated,
            "the timer is running at a rate the machine did not state",
        );
        assert_eq!(
            crate::arch::timer::interval(),
            stated / super::TICK_HZ,
            "the tick interval is derived from the stated rate",
        );
    }

    /// The heartbeat is beating.
    #[test_case]
    fn the_timer_is_ticking() {
        use crate::arch::timer;

        // Name the hart. `ticks()` reads a per-hart counter and this thread can be migrated by a
        // steal at any preemption point, which would compare two unrelated counters (`ticks_on`).
        let hart = crate::cpu::id();
        let before = timer::ticks_on(hart);
        timer::spin_for(timer::interval() * 3);
        let after = timer::ticks_on(hart);

        assert!(
            after > before,
            "no timer interrupt in three tick periods: SBI set_timer, sie.STIE, or the scause=5 \
             arm of the trap dispatcher is not delivering"
        );
    }

    /// Ticks arrive at the configured rate, proven by the grid rather than by the wall clock.
    ///
    /// This is the test aarch64 wrote after measuring 100 Hz configured and ~70 Hz delivered, and
    /// **RISC-V had the same defect** when this test was written: `tick` re-armed with
    /// `sbi_set_timer(now() + interval)`, a deadline relative to the moment the handler read the
    /// clock, so every period ran long by the trap entry plus the SBI round trip and the lateness
    /// compounded. The fix is the same shape as aarch64's, with the grid kept in software because
    /// SBI has no register to read a deadline back from. See the module header.
    ///
    /// The original assertion here compared delivered ticks against elapsed counter time, one
    /// period of slack either way. That measures the emulator as much as the handler: the test
    /// runner passes no `-icount`, so `rdtime` follows host time, and a host that deschedules the
    /// vCPU for a few periods coalesces ticks into exactly the deficit the old defect produced.
    /// It failed gate runs on contended hosts, including on `rv64`, the control model, and no
    /// margin can separate "our re-arm is late" from "the emulator was not running"
    /// (notes/cpu-models.md BUGS; notes/load-sensitive-assertions.md).
    ///
    /// So assert the re-arm LAW instead, which is what the test was always responsible for and is
    /// deterministic: over a window in which no miss was recorded, [`rearm`] moved the deadline by
    /// **exactly one interval per delivered tick**. The re-arm-from-`now` defect fails this on the
    /// first tick (each deadline lands late by the trap-plus-SBI latency, so the sum overshoots
    /// the grid); a descheduled emulator cannot fail it, because a deschedule long enough to slip
    /// the grid is counted by `MISSED_TICKS` and the window is retried.
    #[test_case]
    fn ticks_arrive_at_the_configured_rate() {
        use crate::arch::timer;

        // A consistent (ticks, missed, deadline, counter) snapshot without masking: a tick
        // between the reads would skew all of them coherently-looking, so re-read until the tick
        // count brackets the others unchanged. The hart id is in the bracket too: these statics
        // are per hart, and a snapshot pair taken on two harts compares unrelated grids.
        let snapshot = || loop {
            let hart = crate::cpu::id();
            let t = timer::ticks();
            let m = timer::missed_ticks();
            let d = timer::deadline();
            let c = timer::now();
            if timer::ticks() == t && crate::cpu::id() == hart {
                break (hart, t, m, d, c);
            }
        };

        // A miss re-anchors the grid, which is correct behaviour (`rearm`'s safety valve), so a
        // window containing one proves nothing about the law either way: retry it.
        //
        // **Exhausting the budget is not a failure, and milestone 62 settled that on 2026-08-18.**
        // It used to be. The assertion that stood here read "either the host is too contended to
        // observe the grid, or the handler is slower than a whole tick period", and the word
        // deciding between those two is `or`: this is the family's signature confound, a claim
        // whose truth depends on the host, written from inside a guest that cannot see the host.
        // The acceptance run fired it four times in forty-five loaded runs, twice per ISA, and
        // widening the budget is the one fix DECISIONS §61 and this milestone both forbid by name,
        // because it changes how often you notice rather than what is measured.
        //
        // What is kept is everything below, and it is the whole reason this test exists: the
        // re-arm law is exact, and a host can only cost us the chance to measure it, never the
        // answer. What is given up is the second, implicit claim, which `script/icount` now
        // asserts with no host term in it (claim 2, the handler bounded at 2,500 instructions
        // against a measured 1,056/900, and claim 3, zero missed ticks) and with the law itself
        // beside them (claim 4, added by this milestone after an injection showed the instrument
        // was blind to the very defect this test catches). CI runs `script/icount` on every change
        // that is not documentation only. See notes/load-sensitive-assertions.md.
        const ATTEMPTS: u32 = 8;
        let mut attempts = 0;
        let measured = loop {
            let (h0, t0, m0, d0, c0) = snapshot();
            timer::spin_for(timer::frequency() / 4); // a quarter of a second, by the counter
            let (h1, t1, m1, d1, c1) = snapshot();

            if h0 == h1 && m0 == m1 && t1 - t0 >= 2 {
                break Some((t1 - t0, d1 - d0, (c1 - c0) / timer::interval()));
            }
            attempts += 1;
            if attempts == ATTEMPTS {
                break None;
            }
        };

        // Loud rather than silent, because a test that quietly measures nothing is worse than one
        // that flakes: the reader has to be able to tell "the law held" from "the law was not
        // looked at". The numbers come with it so a human can triage without re-running, which is
        // what `miss_detail` was added for in the first place.
        let Some((elapsed_ticks, deadline_delta, expected)) = measured else {
            let (now, next, misses) = super::miss_detail::last();
            crate::println!(
                "    (UNMEASURED: no miss-free window in {} tries, so the re-arm law was not \
                 tested this run. {} misses recorded on this hart, the last re-armed {} counter \
                 ticks late against an interval of {}. A miss re-anchors the grid, so a window \
                 containing one proves nothing either way, and whether these misses are a \
                 contended host or a slow handler is the one question a wall clock cannot answer \
                 from inside the guest. `script/icount` answers it and asserts this same law with \
                 no host term in it. Milestone 62; notes/load-sensitive-assertions.md.)",
                ATTEMPTS,
                misses,
                now.saturating_sub(next),
                timer::interval(),
            );
            return;
        };

        assert_eq!(
            deadline_delta,
            elapsed_ticks * timer::interval(),
            "timer drift: {elapsed_ticks} ticks moved the deadline off the grid. Re-arming from \
             `now()` inside the handler instead of from the previous deadline does exactly this."
        );

        // The one wall-clock bound a contended host cannot falsify: descheduling only DROPS
        // ticks, so more ticks than elapsed periods (plus one for starting mid-period) means the
        // timer is firing faster than the grid, which is `rearm`'s spin-forever failure mode.
        assert!(
            elapsed_ticks <= expected + 1,
            "{elapsed_ticks} ticks in {expected} periods: the timer fires faster than configured"
        );
    }

    // **`the_handler_keeps_up_when_no_lock_is_held` was deleted here** (milestone 62, 2026-08-18),
    // in lockstep with the aarch64 twin, whose comment at the same place in
    // `kernel/src/arch/aarch64/timer.rs` carries the injection table and the full argument. Rule 5:
    // an assertion this tree stops trusting on one ISA is not one it keeps on the other, and the
    // one-day gap in 2026-08-16's round is the record of what the alternative costs.
    //
    // In short. What it meant to prove: with interrupts live and no lock held, a missed deadline
    // would mean the handler itself is too slow. What it measured: the missed-tick delta over five
    // tick periods, classified by how late the re-arm was, failing under one interval and passing
    // at one or more. **Its true-positive band and its false-positive band are the same band**: a
    // handler slow by 1.5 periods failed correctly, a handler slow by 2.5 periods passed while
    // printing "the emulator was descheduled; not this kernel's bug, not failed", and a real host
    // deschedule inside that same band failed twice per ISA in milestone 62's acceptance run. From
    // inside the guest the two causes are one observation and no cut separates them.
    //
    // The claim is on `script/icount`, in instructions the host cannot move: the handler bounded
    // deadline-to-re-armed at `HANDLER_BOUND` against a measured 900 here, `missed_ticks == 0` with
    // no taxonomy, and since this milestone the re-arm law beside them. `miss_detail` survives,
    // consumed now by the drift test's unmeasured-window report. See
    // notes/load-sensitive-assertions.md and notes/instruction-clock.md.

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

        // `before` is read **inside** the critical section, and it names its hart. Interrupts are
        // masked in there, so this thread can neither be preempted nor migrated and the measured
        // window is exactly the window under test. Read outside, it straddled a preemption point
        // and compared per-hart counters across a possible steal (see `ticks_on`).
        let (hart, before) = {
            let _guard = M.lock();
            let hart = crate::cpu::id();
            let before = timer::missed_ticks_on(hart);
            // Two and a half tick periods with interrupts masked. At least one deadline passes while
            // we cannot service it, and one more passes before we can re-arm.
            timer::spin_for(timer::interval() * 2 + timer::interval() / 2);
            (hart, before)
        };

        // Let the pending interrupt land and the miss be counted. Bounded rather than a fixed
        // single period: the claim is that the miss *happens*, and a host that descheduled the
        // emulator only makes the delivery later.
        assert!(
            within_periods(20, || timer::missed_ticks_on(hart) > before),
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

        // The timer is alive. Hart-scoped, for `ticks_on`'s reason.
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
        // past and takes the interrupt at the first instruction it executes. That failed CI on
        // 2026-08-04 as "left: 41, right: 40", one surplus tick, on `rv64`, the control model.
        // The same window also straddled a preemption point, and TICKS is per hart, so a steal
        // (§28.3) moving this thread compared two unrelated counters.
        let (hart, before) = {
            let _guard = M.lock();
            // Interrupts are masked from here, so this hart cannot switch threads: `cpu::id()` is
            // fixed for the whole block and both reads below are of one counter.
            let hart = crate::cpu::id();
            let before = timer::ticks_on(hart);

            // Thirty milliseconds. Three ticks' worth. Not one of them may land.
            timer::spin_for(timer::interval() * 3);

            assert_eq!(
                timer::ticks_on(hart),
                before,
                "A TIMER INTERRUPT FIRED WHILE A LOCK WAS HELD. IrqSafeMutex is not masking \
                 sstatus.SIE, and the deadlock in notes/locking.md is live: a handler that touched \
                 this lock would spin forever waiting for code that cannot run."
            );
            (hart, before)
        };

        // And the moment we let go, the pending interrupt is delivered. Bounded rather than a
        // fixed two periods, and read by hart index because dropping the guard is a preemption
        // point: this thread may be on another hart by the next instruction, and the hart we left
        // keeps ticking either way.
        assert!(
            within_periods(20, || timer::ticks_on(hart) > before),
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
