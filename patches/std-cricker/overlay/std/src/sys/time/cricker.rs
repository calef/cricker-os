//! Time on cricker-os: the monotonic counter for `Instant`, and a granted clock for `SystemTime`.
//!
//! # `Instant` is the counter, and nothing may perturb it
//!
//! Exactly what the hardware gives: monotonic ticks since boot, converted with the counter
//! frequency, read with one instruction and no syscall (the one ambient readable the ABI grants,
//! notes/abi.md). Nothing in this file, and nothing in the clock service, ever touches it.
//!
//! # `SystemTime` is the counter plus a granted offset (milestone 51, DECISIONS §43)
//!
//! Wall-clock time is **counter + offset**, and the offset lives in a page the clock service
//! publishes and a reader holds **read-only**. So `SystemTime::now()` is two loads and an add, with
//! no server round trip, and adjusting the wall clock cannot move `Instant` **by construction**:
//! the counter is not in the write path.
//!
//! Reading the clock is therefore an authority a program either was granted or was not:
//!
//! - **Slot [`rt::CLOCK_SLOT`]** holds a `Frame` capability naming the clock page, and the loader
//!   maps that page read-only at [`rt::CLOCK_PAGE`]. A program given both has a real wall clock.
//! - A program given neither **does not know what time it is**, and this file says so rather than
//!   guessing.
//!
//! # What replaced the lie, and why the replacement panics
//!
//! Until this milestone `SystemTime` was the monotonic counter offset from `UNIX_EPOCH`, so the
//! machine reported **January 1970 plus uptime**. That is the exact failure mode DECISIONS §42
//! forbids: not that the platform lacked a clock, but that the interface lied about it, and a
//! caller could not tell a wrong answer from a right one.
//!
//! `SystemTime::now()` has no error channel, so the only loud refusal available is a **panic**, and
//! that is what an unknown clock gets. A program that never asks the time is unaffected; a program
//! that asks gets told, with the reason, instead of quietly stamping a log with 1970. The readable
//! form of the same state lives one level down in `clock_proto::state`, for anything that wants to
//! check before asking. See notes/std.md.

use crate::sync::atomic::{AtomicU8, Ordering};
use crate::sys::pal::cricker::clockproto::{self, ClockPage};
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

/// Monotonic nanoseconds since boot, the input to the wall-clock sum.
fn monotonic_nanos() -> u64 {
    let d = now();
    d.as_secs() * clockproto::NANOS_PER_SEC + d.subsec_nanos() as u64
}

// --- Is a clock reachable at all? -------------------------------------------------------------
//
// This has to be answerable WITHOUT touching the clock page, because a program that was not granted
// one has no page mapped and a probe that read it would fault instead of returning an answer. So
// the probe is an invocation of the capability in the slot, with a method number no object defines:
//
//   - no capability in the slot: the kernel answers `NoSuchSlot` (-1).
//   - the clock page's Frame capability: the kernel answers `BadMethod` (-5), which is a refusal
//     from a real object and therefore proof that one is there.
//
// Cached, because the answer cannot change: a cspace slot's contents are fixed at spawn on this
// ABI (0 = not yet asked, 1 = granted, 2 = not granted). Same shape as the fs PAL's probe.

static GRANTED: AtomicU8 = AtomicU8::new(0);

/// A method number no object type defines, so the invocation can only ever be refused.
const NO_SUCH_METHOD: u64 = 0xffff;

fn granted() -> bool {
    match GRANTED.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            // SAFETY: a plain syscall that cannot succeed; the kernel validates the slot.
            let r = unsafe { rt::invoke(rt::CLOCK_SLOT, NO_SUCH_METHOD, 0, 0, 0) };
            let ok = r != -1; // anything but "the slot is empty" means a capability is there
            GRANTED.store(if ok { 1 } else { 2 }, Ordering::Relaxed);
            ok
        }
    }
}

/// The clock page, or `None` when this process was granted no clock.
fn page() -> Option<ClockPage> {
    // SAFETY: the loader maps the clock page read-only at CLOCK_PAGE alongside the capability the
    // probe just found in CLOCK_SLOT, and nothing unmaps it. Without the capability we never build
    // the pointer at all, which is what keeps the unmapped case from faulting.
    granted().then(|| unsafe { ClockPage::new(rt::CLOCK_PAGE) })
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

    /// The wall clock, or a panic naming exactly why there is not one.
    ///
    /// The two failing cases are deliberately distinguished in the message, because they call for
    /// different fixes: nobody granted this process a clock capability, or the machine itself never
    /// learned the time (no RTC in the device tree, or one whose reading failed the sanity window
    /// the clock service applies).
    pub fn now() -> SystemTime {
        let Some(page) = page() else {
            panic!(
                "SystemTime::now() on cricker-os: this process holds no clock capability, so it \
                 does not know what time it is. `Instant` works and measures duration; wall-clock \
                 time is a grant (DECISIONS §43, notes/std.md)."
            )
        };
        let r = page.read();
        if !clockproto::state::known(r.state) {
            panic!(
                "SystemTime::now() on cricker-os: the machine does not know what time it is (no \
                 RTC, or an RTC reading outside the sanity window). Reporting 1970 instead would \
                 be the lie this refusal exists to replace (DECISIONS §42, §43)."
            );
        }
        let nanos = clockproto::wall_nanos(r.offset_nanos, monotonic_nanos());
        SystemTime(Duration::new(
            nanos / clockproto::NANOS_PER_SEC,
            (nanos % clockproto::NANOS_PER_SEC) as u32,
        ))
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
