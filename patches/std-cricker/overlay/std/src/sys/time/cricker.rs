//! Time from the virtual counter, the one ambient readable the ABI grants (notes/abi.md).
//!
//! `Instant` is exactly what the hardware gives: monotonic ticks since boot, converted with the
//! counter frequency. `SystemTime` is the same value offset from `UNIX_EPOCH`, which makes it
//! monotonic-since-boot, **not wall-clock time**: the platform has no RTC and no NTP, so a
//! cricker-os "system time" honestly measures "since this machine came up". Recorded as a
//! caveat in notes/std.md; programs that difference `SystemTime`s get correct durations, and
//! programs that expect calendar dates get 1970 plus uptime, which is the truth available.

use crate::sys::pal::cricker::rt;
use crate::time::Duration;

fn ticks_to_duration(ticks: u64) -> Duration {
    let freq = rt::cntfrq();
    let secs = ticks / freq;
    let rem = ticks % freq;
    // rem < freq <= a few GHz, so rem * NANOS never overflows u128, and nanos < 1e9 fits u32.
    let nanos = (rem as u128 * 1_000_000_000 / freq as u128) as u32;
    Duration::new(secs, nanos)
}

fn now() -> Duration {
    ticks_to_duration(rt::now())
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Instant(Duration);

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct SystemTime(Duration);

pub const UNIX_EPOCH: SystemTime = SystemTime(Duration::from_secs(0));

impl Instant {
    pub fn now() -> Instant {
        Instant(now())
    }

    pub fn checked_sub_instant(&self, other: &Instant) -> Option<Duration> {
        self.0.checked_sub(other.0)
    }

    pub fn checked_add_duration(&self, other: &Duration) -> Option<Instant> {
        Some(Instant(self.0.checked_add(*other)?))
    }

    pub fn checked_sub_duration(&self, other: &Duration) -> Option<Instant> {
        Some(Instant(self.0.checked_sub(*other)?))
    }
}

impl SystemTime {
    pub const MAX: SystemTime = SystemTime(Duration::MAX);

    pub const MIN: SystemTime = SystemTime(Duration::ZERO);

    pub fn now() -> SystemTime {
        SystemTime(now())
    }

    pub fn sub_time(&self, other: &SystemTime) -> Result<Duration, Duration> {
        self.0.checked_sub(other.0).ok_or_else(|| other.0 - self.0)
    }

    pub fn checked_add_duration(&self, other: &Duration) -> Option<SystemTime> {
        Some(SystemTime(self.0.checked_add(*other)?))
    }

    pub fn checked_sub_duration(&self, other: &Duration) -> Option<SystemTime> {
        Some(SystemTime(self.0.checked_sub(*other)?))
    }
}
