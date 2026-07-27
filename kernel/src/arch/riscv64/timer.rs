//! **The timer, RISC-V.** The S-mode analog of aarch64's generic virtual timer.
//!
//! RISC-V exposes a free-running counter through the `time` CSR (read with `rdtime`) and schedules
//! the next tick with `stimecmp` (the Sstc extension) or an SBI TIME call. The free-running read and
//! the frequency are real here; programming the compare, counting ticks, and the derived helpers are
//! the timer step. See notes/riscv-port.md.

use core::arch::asm;

/// The S-mode timer interrupt cause (`scause` = 5, the Supervisor timer interrupt). The RISC-V
/// analog of aarch64's per-CPU timer INTID.
pub const TIMER_INTID: u32 = 5;

/// Ticks per second, the preemption rate. Same 100 Hz as aarch64.
pub const TICK_HZ: u64 = 100;

/// The `time` CSR frequency on QEMU's `virt` machine: 10 MHz. Properly this comes from the device
/// tree (`/cpus/timebase-frequency`); hardcoded until the DTB parse lands, and flagged so.
const TIMEBASE_HZ: u64 = 10_000_000;

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

/// Start the periodic timer (program the first `stimecmp`, enable the S-timer in `sie`). Timer step.
pub fn init() {
    unimplemented!("riscv timer init (stimecmp / SBI TIME + sie): the timer step")
}

/// Handle a timer interrupt: rearm the compare and advance the tick count. Timer step.
pub fn tick() {
    unimplemented!("riscv timer tick (rearm stimecmp): the timer step")
}

/// Ticks since boot. Timer step (needs the tick counter the timer interrupt maintains).
pub fn ticks() -> u64 {
    unimplemented!("riscv tick counter: the timer step")
}

/// Milliseconds since boot. Timer step.
pub fn uptime_ms() -> u64 {
    unimplemented!("riscv uptime: the timer step")
}

/// Busy-wait for a number of counter ticks. Timer step (can build on `now`, deferred for parity).
pub fn spin_for(counter_ticks: u64) {
    let _ = counter_ticks;
    unimplemented!("riscv spin_for: the timer step")
}

/// Counter ticks between two timer interrupts (the reload interval). Timer step.
pub fn interval() -> u64 {
    unimplemented!("riscv timer interval: the timer step")
}

/// How many ticks were missed if the handler ran late. Timer step.
pub fn missed_ticks() -> u64 {
    unimplemented!("riscv missed ticks: the timer step")
}
