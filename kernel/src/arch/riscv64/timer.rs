//! **The timer, RISC-V.** The S-mode analog of aarch64's generic virtual timer.
//!
//! RISC-V exposes a free-running counter through the `time` CSR (read with `rdtime`). The next tick
//! is scheduled through the **SBI TIME** extension (`sbi_set_timer`, an `ecall` to OpenSBI in
//! M-mode), which both arms the next S-mode timer interrupt and clears the pending one. The Sstc
//! extension's `stimecmp` CSR would avoid the M-mode round trip, but SBI TIME works on every
//! OpenSBI and is the portable choice for now. See notes/riscv-port.md.

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

/// Ticks since boot, maintained by [`tick`].
static TICKS: AtomicU64 = AtomicU64::new(0);

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
    sbi_set_timer(now() + interval());
    // SAFETY: setting sie.STIE only unmasks the timer source; it takes effect once SIE is on.
    unsafe { asm!("csrs sie, {}", in(reg) STIE, options(nomem, nostack, preserves_flags)) };
}

/// Handle a timer interrupt: count the tick and arm the next deadline (which also clears the pending
/// interrupt). Called from the trap dispatcher on `scause` = timer.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    sbi_set_timer(now() + interval());
}

/// Ticks since boot.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Milliseconds since boot, from the free-running counter (independent of the tick interrupt).
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

/// How many ticks were missed if the handler ran late. Not tracked yet (SBI set_timer re-arms from
/// `now`, so a late handler simply spaces the next tick out rather than dropping a count); returns 0.
pub fn missed_ticks() -> u64 {
    0
}
