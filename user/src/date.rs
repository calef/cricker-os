//! **`date`**: print the wall-clock time (milestone 51; DECISIONS §43, notes/clock.md,
//! notes/calendar.md, notes/date.md).
//!
//! The whole program is: read a page, add the ambient counter, hand the number to `calendar`, send
//! the bytes. It holds two capabilities and neither of them can change anything.
//!
//! # It reads. It does not set. (And that is not an omission.)
//!
//! DECISIONS §43 splits the clock into three authorities that are three *different objects*: read
//! is a **read-only mapping** of the clock page, set is the **same page mapped read/write**, and
//! propose is an **endpoint**. `date` holds the first one, and holding it is the entire reason it
//! cannot do the other two: there is no flag it could pass and no method it could call, because
//! the authority it lacks is a page permission rather than a check somewhere in this file.
//!
//! So there is no `date -s` here, deliberately. A program that set the clock would be a *different*
//! binary holding a *different* capability, and one that did both in one process would be exactly
//! the conflation Unix's `date` inherits and this design refuses. That program is not built; when
//! it is, it will be about thirty lines and it will be recognisable by its wiring rather than by
//! its argument parsing.
//!
//! # The unknown clock is an answer, not a crash
//!
//! `clock_proto::state::UNKNOWN` is a real state and it is the **default**: a frame nobody has
//! published to reads as unknown, so a machine with no RTC (or one whose RTC read a time the clock
//! service did not believe) says so rather than reporting 1970 (DECISIONS §42's no-silent-
//! degradation rule, on the time axis).
//!
//! `std`'s `SystemTime::now()` has to **panic** there, because it has no error channel. `date` has
//! one: its output. So this program reads the state word *before* it reads the offset, and prints a
//! sentence naming which of the two causes it was. The two are worth telling apart because they
//! call for different fixes: nobody granted this process a clock, or the machine itself never
//! learned the time.
//!
//! # Arguments, which arrive in registers because that is the ABI
//!
//! There is no argv on this ABI; a program gets three words at `_start` (notes/abi.md). So:
//!
//! | | |
//! |---|---|
//! | `a0` | which [`Format`] to print, `FMT_*` below. `0` is `Human`, so a bare spawn is `date` with no arguments. |
//! | `a1` | the UTC offset in **minutes**, as a signed value. `0` is UTC, which is the default and the only sane one here: there is no tzdata (notes/calendar.md). |
//! | `a2` | non-zero to add a second line naming the clock's **provenance**, which is the only way the four-state model is visible to a person. |
//!
//! # EXAMPLES
//!
//! ```text
//! date                      Thu 2026-07-30 12:34:56 UTC
//! date  (a0 = FMT_RFC3339)  2026-07-30T12:34:56Z
//! date  (a0 = FMT_UNIX)     1785414896
//! date  (a1 = 330)          Thu 2026-07-30 18:04:56 +05:30
//! date  (a2 = 1)            Thu 2026-07-30 12:34:56 UTC
//!                           date: clock source: rtc, generation 1
//! ```
//!
//! And the case worth reading twice, because it is the one every other OS gets wrong quietly:
//!
//! ```text
//! date  (no clock service)  date: the time is unknown: the machine has no clock it believes
//! date  (no clock granted)  date: the time is unknown: this process holds no clock capability
//! ```
//!
//! # BUGS
//!
//! - **One-second resolution, and the read is not atomic with the print.** The seqlock makes the
//!   *page* read consistent; the counter is sampled just after it. A clock stepped between the two
//!   would be printed as the new time, which is right, but the two lines of a `a2` run can straddle
//!   a step and disagree about the generation. Nothing here needs better and pretending otherwise
//!   would be the lie this milestone exists to remove.
//! - **A fixed UTC offset is not a time zone.** `a1 = -480` is Pacific Standard Time for part of the
//!   year and wrong for the rest. notes/calendar.md records why tzdata is out of scope.
//! - **No `strftime`.** Five named formats, because a format-string interpreter is a second parser
//!   with runtime errors in a program that has no allocator (notes/calendar.md).

#![no_std]
#![no_main]

use calendar::{DateTime, Format, UtcOffset};
use clock_proto::{ClockPage, state};
use user_rt::{cntfrq, exit, invoke, now, send};

/// Slot 0: where the output goes. An endpoint with `WRITE`, and the same 16-bytes-per-message
/// framing the std PAL's stdout uses (`w0` = the byte count, `w1`|`w2` = the bytes, little-endian),
/// so a reader that can already reassemble a std program's stdout can read this too.
const REPORT: u64 = 0;

/// Slot 1: the clock page's `Frame` capability, with `READ` and nothing else. Its *presence* is
/// what [`clock_page`] probes for; its rights are what stop this program setting the clock.
const CLOCK_SLOT: u64 = 1;

/// Where the wiring maps the clock page, read-only. Must match `kernel/src/user.rs`'s
/// `date_tests::CLOCK_VA`.
const CLOCK_VA: u64 = 0x00c0_0000;

// The `a0` format selector. Must match kernel/src/user/date_tests.rs. Zero is `Human` so that a
// spawn that passes no arguments at all gets the default a person wants.
const FMT_HUMAN: u64 = 0;
const FMT_RFC3339: u64 = 1;
const FMT_DATE: u64 = 2;
const FMT_TIME: u64 = 3;
const FMT_UNIX: u64 = 4;

#[unsafe(no_mangle)]
pub extern "C" fn _start(fmt: u64, offset_minutes: u64, provenance: u64) -> ! {
    // The state first, before anything computes a time from the offset. This ordering is the whole
    // difference between this program and `SystemTime::now()`: it has an error channel and uses it.
    let Some(page) = clock_page() else {
        line(b"date: the time is unknown: this process holds no clock capability");
        end();
    };
    let r = page.read();
    if !state::known(r.state) {
        line(b"date: the time is unknown: the machine has no clock it believes");
        // Still worth saying which nothing it was, when asked: an unknown clock has a generation
        // too, and "nobody has ever published" reads differently from "somebody published unknown".
        if provenance != 0 {
            source_line(r.state, r.generation);
        }
        end();
    }

    let Ok(offset) = UtcOffset::from_minutes(offset_minutes as i64 as i32) else {
        line(b"date: that UTC offset is not one RFC 3339 can write");
        end();
    };

    // Nanoseconds to seconds, truncating towards zero, which is exact here: the wall clock is a
    // `u64` count of nanoseconds since 1970 and this calendar's second is the smallest unit it has.
    let unix_secs = (clock_proto::wall_nanos(r.offset_nanos, monotonic_nanos())
        / clock_proto::NANOS_PER_SEC) as i64;

    // Out of range is not reachable from a clock the service believed (its sanity window is 2026 to
    // 2100 and the calendar's is 0000 to 9999), and it is still an answer rather than a panic,
    // because "unreachable" is a claim about today's policy and this is a claim about arithmetic.
    let Ok(dt) = DateTime::from_unix(unix_secs, offset) else {
        line(b"date: the clock reads a time outside the calendar's range");
        end();
    };

    line(dt.format(format_of(fmt)).as_bytes());
    if provenance != 0 {
        source_line(r.state, r.generation);
    }
    end();
}

/// The `a0` selector as a [`Format`]. An unrecognised value is `Human` rather than an error: this
/// is a register, not a parsed string, and the caller that got it wrong wants the default.
fn format_of(fmt: u64) -> Format {
    match fmt {
        FMT_HUMAN => Format::Human,
        FMT_RFC3339 => Format::Rfc3339,
        FMT_DATE => Format::Date,
        FMT_TIME => Format::Time,
        FMT_UNIX => Format::Unix,
        _ => Format::Human, // a caller that got the selector wrong wants the default
    }
}

/// **Where the time came from**, which is the readable form of `clock_proto`'s four states.
///
/// Worth printing because the provenance is a real distinction this system makes and no `date` on a
/// Unix can: "a human set it" and "an external source the service bounded accepted it" are
/// different claims, and a caller weighing a certificate expiry wants the difference. The
/// generation says how many times the page has been published to, so a reader can see whether the
/// clock has been stepped since boot.
fn source_line(st: u64, generation: u64) {
    fn push(buf: &mut [u8; 64], n: &mut usize, bytes: &[u8]) {
        for &b in bytes {
            if *n < buf.len() {
                buf[*n] = b;
                *n += 1;
            }
        }
    }

    let mut buf = [0u8; 64];
    let mut n = 0;
    push(&mut buf, &mut n, b"date: clock source: ");
    push(
        &mut buf,
        &mut n,
        match st {
            state::RTC => b"rtc" as &[u8],
            state::SET => b"set",
            state::SYNCED => b"synced",
            _ => b"unknown",
        },
    );
    push(&mut buf, &mut n, b", generation ");

    let mut digits = [0u8; 20];
    let mut d = digits.len();
    let mut v = generation;
    loop {
        d -= 1;
        digits[d] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    push(&mut buf, &mut n, &digits[d..]);
    line(&buf[..n]);
}

/// **Announce the end of the stream, then exit** (milestone 50).
///
/// `date` was already writing the sink contract's `BYTES` framing before that contract had a name,
/// because `OP_BYTES` is zero and a bytes message's first word is therefore its byte count
/// (notes/sink-protocol.md). What it was missing is the other half: a reader downstream of a `|`
/// has no way to know the producer is finished except by being told, and inferring it from a death
/// notification would be a fact about process supervision standing in for a fact about a stream.
///
/// So every exit path from `_start` goes through here. The send blocks until somebody takes it,
/// which for a reader that stopped at the newline means this process stays parked; that is the same
/// rendezvous every send on this system has, and the alternative (a stream whose end is invisible)
/// makes `date | wc` impossible.
fn end() -> ! {
    send(REPORT, sink_proto::eof(), 0, 0);
    exit();
}

/// The clock page, or `None` when this process was granted no clock.
///
/// The probe has to answer **without touching the page**, because a process that was granted no
/// clock has nothing mapped at [`CLOCK_VA`] and a read there would fault instead of returning an
/// answer. So it invokes the capability in the slot with a method number no object type defines:
/// an empty slot answers `NoSuchSlot`, and a real `Frame` answers `BadMethod`, which is a refusal
/// from an object that exists and is therefore proof one is there. Same shape as the std PAL's
/// `granted()`; the two are the same problem.
fn clock_page() -> Option<ClockPage> {
    /// A method number no object type defines, so the invocation can only ever be refused.
    const NO_SUCH_METHOD: u64 = 0xffff;
    // SAFETY: a syscall that cannot succeed; the kernel validates the slot before the method.
    let r = unsafe { invoke(CLOCK_SLOT, NO_SUCH_METHOD, 0, 0, 0) };
    if r == abi::Error::NoSuchSlot as i64 {
        return None;
    }
    // SAFETY: the wiring maps the clock page read-only at CLOCK_VA alongside the capability the
    // probe just found, and nothing unmaps it. Without the capability we never build the pointer.
    Some(unsafe { ClockPage::new(CLOCK_VA) })
}

/// Monotonic nanoseconds since boot, from the ambient counter.
///
/// Split into whole seconds and a remainder for the reason `user/src/clock.rs` records: at 62.5 MHz
/// the naive `ticks * 1_000_000_000` overflows a `u64` about five minutes into a boot.
fn monotonic_nanos() -> u64 {
    let freq = cntfrq();
    let ticks = now();
    let secs = ticks / freq;
    let rem = ticks % freq;
    secs * clock_proto::NANOS_PER_SEC + rem * clock_proto::NANOS_PER_SEC / freq
}

/// Send `bytes` and a newline to the output endpoint, 16 bytes per message.
fn line(bytes: &[u8]) {
    let mut out = [0u8; 96];
    let n = bytes.len().min(out.len() - 1);
    out[..n].copy_from_slice(&bytes[..n]);
    out[n] = b'\n';
    for chunk in out[..n + 1].chunks(16) {
        let mut w1 = 0u64;
        let mut w2 = 0u64;
        for (i, &b) in chunk.iter().enumerate() {
            if i < 8 {
                w1 |= (b as u64) << (8 * i);
            } else {
                w2 |= (b as u64) << (8 * (i - 8));
            }
        }
        send(REPORT, chunk.len() as u64, w1, w2);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // Nothing here should panic: the calendar's formatting has no panic path (its `every_format_is_
    // ascii` harness proves the `unwrap_or` is unreachable) and every fallible conversion above is
    // matched. A fault the kernel turns into a kill is the honest signal if one ever does.
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `brk` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem));
    };
    #[cfg(target_arch = "riscv64")]
    // SAFETY: `ebreak` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
