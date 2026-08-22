//! **calendar: Unix seconds to a civil date, and back** (milestone 51, the wall-clock lane).
//!
//! The machine will soon know what time it is (an RTC, a clock service holding the offset, an NTP
//! client that may only *propose*). This crate is the part of that with no authority in it at all:
//! turning a count of seconds into a year, a month and a day, turning one back, and printing or
//! reading the handful of textual forms the rest of the system needs. It performs no IO, makes no
//! syscalls, allocates nothing, and knows nothing about the clock service. The roadmap's milestone
//! 51 block puts the split this way: "Timezone and calendar conversion are pure computation and
//! belong in a host-tested library crate, not in the service."
//!
//! That is DECISIONS §7 and §14 applied, the same shape as `grant_plan` and `fs_proto`: the arithmetic is
//! host-tested in milliseconds and machine-checked by Kani, and only the wiring needs QEMU.
//!
//! # The calendar
//!
//! Proleptic Gregorian, which means the Gregorian rules (leap years divisible by 4, except centuries,
//! except centuries divisible by 400) are projected backwards past 1582 rather than switching to
//! Julian. Every date library that speaks Unix time does this, because a Unix timestamp is defined as
//! a count of seconds and has no idea a calendar reform happened. Dates before October 1582 therefore
//! do **not** match what a historian would write, and that is a deliberate, universal convention
//! rather than a bug. Year 0 exists here and is a leap year; historians write 1 BC instead.
//!
//! Negative timestamps (before 1970) are fully supported. That was a choice, and the reason is that
//! refusing them would push the check onto every caller: file timestamps predating the epoch are
//! ordinary, and an RTC that has lost its battery reads as something arbitrary rather than something
//! polite.
//!
//! # The range, and what a timestamp outside it does
//!
//! Supported: **years 0000 through 9999**, that is [`MIN_TIMESTAMP`] through [`MAX_TIMESTAMP`]. The
//! bound comes from RFC 3339, whose `date-fullyear` is exactly four digits; a year outside it cannot
//! be written in the interchange format this crate parses and prints, so admitting it would create
//! values that can be computed but not spoken. A timestamp outside the range is [`Error::OutOfRange`]
//! rather than a wrapped or saturated date: silent degradation is the failure §42 forbids elsewhere,
//! and it is no more acceptable here.
//!
//! The year-2038 problem is a non-event on this system and the tests prove it rather than assuming
//! it: every timestamp here is [`i64`], and 2038-01-19T03:14:08Z is one second after
//! 03:14:07Z with nothing special about it.
//!
//! # Leap seconds do not exist here
//!
//! Unix time is defined so that every day is exactly 86,400 seconds. A leap second has no timestamp;
//! it is smeared or repeated by whatever sets the clock. This crate therefore has no way to *name*
//! one, and rather than pretend, [`DateTime::parse_rfc3339`] refuses a `:60` field with a distinct
//! [`Error::LeapSecond`] instead of quietly folding it onto `:59` or `:00`. A caller who wants the
//! clamp can apply it, having been told.
//!
//! # Time zones: a fixed offset is in scope, tzdata is not
//!
//! [`UtcOffset`] is a fixed number of minutes from UTC. That is enough to print a timestamp the way a
//! local user expects, enough to parse one someone else printed, and it is what RFC 3339 actually
//! carries on the wire.
//!
//! **The IANA time zone database is explicitly out of scope, and not just "not yet".** tzdata is a
//! ~450 KB compiled ruleset that changes several times a year, which makes it a *data distribution*
//! problem (who ships it, who updates it, which capability lets a program read it) rather than a
//! calendar problem. It also cannot be answered correctly without it: `America/Los_Angeles` is not
//! an offset, it is a function from instants to offsets with 100+ years of political history in it,
//! and a hardcoded "-08:00" would be wrong for a third of the year. So: no zone names, no DST, no
//! `TZ` variable. A program that needs local time is given an offset by whatever knows one. If the
//! system ever wants real zones, it is a separate crate with a file behind it, not a growth of this
//! one.
//!
//! # What it does not do
//!
//! No durations or calendar arithmetic ("one month later"), because the caller that wants them does
//! not exist yet and month arithmetic has no single correct answer (what is one month after
//! January 31?). No `strftime`. See [`Format`] for that argument.
//!
//! # Examples
//!
//! The two conversions are inverses, which is the property the whole crate exists to get right:
//!
//! ```
//! use calendar::{Civil, Weekday};
//!
//! // The Unix epoch, as a civil date. 1970-01-01 was a Thursday.
//! let epoch = Civil::from_unix(0).unwrap();
//! assert_eq!((epoch.year(), epoch.month(), epoch.day()), (1970, 1, 1));
//! assert_eq!(epoch.weekday(), Weekday::Thursday);
//! assert_eq!(epoch.to_unix(), 0);
//!
//! // And round-tripping an arbitrary instant.
//! let d = Civil::new(2026, 8, 2, 12, 30, 15).unwrap();
//! assert_eq!(Civil::from_unix(d.to_unix()).unwrap(), d);
//! ```
//!
//! Timestamps before the epoch are the case a truncating division gets wrong, so they are worth
//! showing rather than asserting:
//!
//! ```
//! use calendar::Civil;
//!
//! // One second before the epoch is the last second of 1969, not the first of 1970.
//! let d = Civil::from_unix(-1).unwrap();
//! assert_eq!((d.year(), d.month(), d.day()), (1969, 12, 31));
//! assert_eq!((d.hour(), d.minute(), d.second()), (23, 59, 59));
//! ```
//!
//! Every field is validated separately, so a caller can say what it did not like:
//!
//! ```
//! use calendar::{Civil, Error};
//!
//! assert_eq!(Civil::new(2026, 13, 1, 0, 0, 0), Err(Error::BadMonth));
//! // 2026 is not a leap year, so there is no 29 February.
//! assert_eq!(Civil::new(2026, 2, 29, 0, 0, 0), Err(Error::BadDay));
//! assert!(Civil::new(2024, 2, 29, 0, 0, 0).is_ok());
//! ```
//!
//! Name: unrecorded. Introduced 2026-07-30 for milestone 51's `date`. DECISIONS §46 records the
//! decision to write it rather than vendor it; nothing records the choice of word.

#![no_std]

/// The lowest supported year, 0000. See the module docs for why the range is what it is.
pub const MIN_YEAR: i32 = 0;

/// The highest supported year, 9999, which is RFC 3339's four-digit `date-fullyear`.
pub const MAX_YEAR: i32 = 9999;

/// 0000-01-01T00:00:00Z, the earliest instant this crate will name.
pub const MIN_TIMESTAMP: i64 = days_from_civil(MIN_YEAR, 1, 1) * SECS_PER_DAY;

/// 9999-12-31T23:59:59Z, the latest instant this crate will name.
pub const MAX_TIMESTAMP: i64 = days_from_civil(MAX_YEAR, 12, 31) * SECS_PER_DAY + SECS_PER_DAY - 1;

/// Seconds in a day. Exactly 86,400, always, by the definition of Unix time (see the module docs on
/// leap seconds).
pub const SECS_PER_DAY: i64 = 86_400;

/// What can go wrong. One enum for conversion and parsing both, because the API is small enough that
/// two would only make callers convert between them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// A timestamp outside [`MIN_TIMESTAMP`]..=[`MAX_TIMESTAMP`], or a year outside
    /// [`MIN_YEAR`]..=[`MAX_YEAR`].
    OutOfRange,
    /// Month was not 1..=12.
    BadMonth,
    /// Day was not 1..=(the length of that month in that year). This is the one that catches
    /// February 30, and February 29 in a non-leap year.
    BadDay,
    /// Hour was not 0..=23.
    BadHour,
    /// Minute was not 0..=59.
    BadMinute,
    /// Second was not 0..=59.
    BadSecond,
    /// A UTC offset beyond ±23:59, which RFC 3339's grammar cannot write.
    BadOffset,
    /// A seconds field of 60. Well-formed RFC 3339, and unrepresentable as a Unix timestamp; see the
    /// module docs.
    LeapSecond,
    /// The text is not RFC 3339.
    Syntax,
}

impl Error {
    /// A short message, for a shell that has no allocator and wants to print the refusal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Error::OutOfRange => "date outside the supported range (years 0000-9999)",
            Error::BadMonth => "month out of range",
            Error::BadDay => "day out of range for that month",
            Error::BadHour => "hour out of range",
            Error::BadMinute => "minute out of range",
            Error::BadSecond => "second out of range",
            Error::BadOffset => "UTC offset out of range",
            Error::LeapSecond => "leap second (:60) has no Unix timestamp",
            Error::Syntax => "not an RFC 3339 timestamp",
        }
    }
}

/// Is this a leap year in the proleptic Gregorian calendar?
///
/// The rule people half-remember, in full: every fourth year, except every hundredth, except every
/// four-hundredth. 1900 is **not** a leap year and 2000 **is**, which is the pair that breaks date
/// code written from memory, and the pair the tests pin.
pub const fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// How many days that month has in that year, or `None` if `month` is not 1..=12.
pub const fn days_in_month(year: i32, month: u8) -> Option<u8> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 => Some(if is_leap_year(year) { 29 } else { 28 }),
        _ => None,
    }
}

// Howard Hinnant's civil-date algorithms (the ones in "chrono-Compatible Low-Level Date Algorithms",
// and the same pair inside libc++ and every serious date library). They are not obvious, so what
// each does and why it is shaped that way:
//
// The trick is to shift the year so it *starts in March*. Then the leap day is the last day of the
// year rather than a discontinuity in the middle of it, and every month-length irregularity collapses
// into one closed-form expression, `(153*m + 2)/5`, which walks the 31/30/31/30/31 pattern exactly.
// The era is a 400-year cycle, which is the period of the whole Gregorian rule and contains exactly
// 146,097 days; working within one era makes the century corrections plain division.
//
// Both directions are pure integer arithmetic with no loops and no tables, which is also what makes
// them a good Kani target: the round-trip proof below is a formula, not a search.

/// Days since 1970-01-01 for a civil date. Private: the public entry points validate first, and this
/// has a precondition (`month` in 1..=12, `day` in 1..=31) it does not check.
///
/// `-719468` is the shift from the internal era-relative day count to the Unix epoch: it is how many
/// days lie between 0000-03-01 and 1970-01-01.
const fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let (y, m, d) = (year as i64, month as i64, day as i64);
    // March-based year: January and February belong to the *previous* one.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // year of era, [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // March = 0, February = 11
    let doy = (153 * mp + 2) / 5 + d - 1; // day of the March-based year, [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // day of era, [0, 146096]
    era * 146_097 + doe - 719_468
}

/// The inverse. Private for the same reason: the caller has already bounded `days`.
const fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    // The three corrections undo the leap-day insertions: /1460 removes the 4-year days, /36524 puts
    // back the century non-leaps, /146096 removes the 400-year exception to that exception.
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // back to January = 1
    let y = if m <= 2 { y + 1 } else { y }; // undo the March-based shift
    (y as i32, m as u8, d as u8)
}

/// A day of the week. `Sunday` is first because that is the order the day-number arithmetic falls
/// out in (`(days + 4) mod 7`, since 1970-01-01 was a Thursday); nothing here depends on the
/// convention, and [`Weekday::iso_number`] gives the ISO 8601 one, where Monday is 1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Weekday {
    /// ISO 8601 day 7.
    Sunday,
    /// ISO 8601 day 1, the week's first day per [`Weekday::iso_number`].
    Monday,
    /// ISO 8601 day 2.
    Tuesday,
    /// ISO 8601 day 3.
    Wednesday,
    /// ISO 8601 day 4.
    Thursday,
    /// ISO 8601 day 5.
    Friday,
    /// ISO 8601 day 6.
    Saturday,
}

impl Weekday {
    /// The three-letter abbreviation, which is the only natural language in this crate. See
    /// [`Format::Human`] for why there is no month-name table to go with it.
    pub const fn abbrev(self) -> &'static str {
        match self {
            Weekday::Sunday => "Sun",
            Weekday::Monday => "Mon",
            Weekday::Tuesday => "Tue",
            Weekday::Wednesday => "Wed",
            Weekday::Thursday => "Thu",
            Weekday::Friday => "Fri",
            Weekday::Saturday => "Sat",
        }
    }

    /// The full English name.
    pub const fn name(self) -> &'static str {
        match self {
            Weekday::Sunday => "Sunday",
            Weekday::Monday => "Monday",
            Weekday::Tuesday => "Tuesday",
            Weekday::Wednesday => "Wednesday",
            Weekday::Thursday => "Thursday",
            Weekday::Friday => "Friday",
            Weekday::Saturday => "Saturday",
        }
    }

    /// ISO 8601's numbering: Monday is 1, Sunday is 7.
    pub const fn iso_number(self) -> u8 {
        match self {
            Weekday::Monday => 1,
            Weekday::Tuesday => 2,
            Weekday::Wednesday => 3,
            Weekday::Thursday => 4,
            Weekday::Friday => 5,
            Weekday::Saturday => 6,
            Weekday::Sunday => 7,
        }
    }

    const fn from_days(days: i64) -> Weekday {
        // 1970-01-01 was a Thursday, so day 0 is index 4 with Sunday at 0.
        match (days + 4).rem_euclid(7) {
            0 => Weekday::Sunday,
            1 => Weekday::Monday,
            2 => Weekday::Tuesday,
            3 => Weekday::Wednesday,
            4 => Weekday::Thursday,
            5 => Weekday::Friday,
            _ => Weekday::Saturday,
        }
    }
}

/// A calendar date and a wall-clock time, with **no zone attached**. "The 30th of July 2026 at
/// 12:34:56" is a `Civil`; which instant that is depends on an offset, which is [`DateTime`]'s job.
///
/// The fields are private and the constructor validates, so **a `Civil` that exists is a real
/// date**: there is no February 30 in this type, and [`Civil::to_unix`] is therefore infallible.
/// That is the same discipline the capability types use (a `Cap` you hold is one that was minted),
/// and it is what makes the conversion arithmetic total rather than defensive.
///
/// The derived `Ord` is chronological, because the fields are declared most-significant first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Civil {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

impl Civil {
    /// Build a date, checking every field. Returns the *specific* [`Error`] for the field that was
    /// wrong, so a `date -s` verb can say what it did not like.
    pub const fn new(
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Result<Civil, Error> {
        if year < MIN_YEAR || year > MAX_YEAR {
            return Err(Error::OutOfRange);
        }
        let Some(len) = days_in_month(year, month) else {
            return Err(Error::BadMonth);
        };
        if day == 0 || day > len {
            return Err(Error::BadDay);
        }
        if hour > 23 {
            return Err(Error::BadHour);
        }
        if minute > 59 {
            return Err(Error::BadMinute);
        }
        if second > 59 {
            return Err(Error::BadSecond);
        }
        Ok(Civil {
            year,
            month,
            day,
            hour,
            minute,
            second,
        })
    }

    /// The UTC civil date of a Unix timestamp.
    pub const fn from_unix(secs: i64) -> Result<Civil, Error> {
        if secs < MIN_TIMESTAMP || secs > MAX_TIMESTAMP {
            return Err(Error::OutOfRange);
        }
        // Euclidean division, not truncating: for a negative timestamp, `-1 / 86400` is 0 in Rust and
        // the answer we need is -1 (the day before the epoch). Truncating division here is the classic
        // way a date library gets 1969-12-31 wrong.
        let days = secs.div_euclid(SECS_PER_DAY);
        let sod = secs.rem_euclid(SECS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        Ok(Civil {
            year,
            month,
            day,
            hour: (sod / 3600) as u8,
            minute: ((sod / 60) % 60) as u8,
            second: (sod % 60) as u8,
        })
    }

    /// The Unix timestamp of this date, read as UTC. Infallible: [`Civil::new`] already established
    /// that the date is real and in range.
    pub const fn to_unix(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * SECS_PER_DAY
            + self.hour as i64 * 3600
            + self.minute as i64 * 60
            + self.second as i64
    }

    /// Days since 1970-01-01, discarding the time of day.
    pub const fn days_since_epoch(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    /// Which day of the week this date falls on.
    pub const fn weekday(&self) -> Weekday {
        Weekday::from_days(self.days_since_epoch())
    }

    /// Day of the year, 1..=366. January 1 is 1; 366 happens only in a leap year, which is one of
    /// the Kani harnesses rather than a comment.
    pub const fn day_of_year(&self) -> u16 {
        let jan1 = days_from_civil(self.year, 1, 1);
        (self.days_since_epoch() - jan1 + 1) as u16
    }

    /// The year, 0..=9999.
    pub const fn year(&self) -> i32 {
        self.year
    }
    /// The month, 1..=12.
    pub const fn month(&self) -> u8 {
        self.month
    }
    /// The day of the month, 1..=31.
    pub const fn day(&self) -> u8 {
        self.day
    }
    /// The hour, 0..=23.
    pub const fn hour(&self) -> u8 {
        self.hour
    }
    /// The minute, 0..=59.
    pub const fn minute(&self) -> u8 {
        self.minute
    }
    /// The second, 0..=59.
    pub const fn second(&self) -> u8 {
        self.second
    }
}

/// A fixed offset from UTC, in minutes. **Not a time zone**: see the module docs for why the IANA
/// database is out of scope rather than pending.
///
/// The range is ±23:59, which is RFC 3339's grammar rather than geography (real offsets run
/// -12:00..+14:00). Parsing accepts what the grammar allows; nothing here needs to be stricter than
/// the format it reads.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct UtcOffset {
    minutes: i16,
}

impl UtcOffset {
    /// UTC itself, the offset almost everything in this system should be using.
    pub const UTC: UtcOffset = UtcOffset { minutes: 0 };

    /// An offset of `minutes` from UTC, positive east. Refuses anything beyond ±1439.
    pub const fn from_minutes(minutes: i32) -> Result<UtcOffset, Error> {
        if minutes <= -1440 || minutes >= 1440 {
            return Err(Error::BadOffset);
        }
        Ok(UtcOffset {
            minutes: minutes as i16,
        })
    }

    /// An offset written the way people write it: `from_hm(-8, 0)` is `-08:00`, `from_hm(5, 30)` is
    /// `+05:30`. Both arguments carry the sign, and mixing them (`from_hm(-8, 30)` meaning
    /// -08:30) is why `minutes` is signed here rather than a magnitude plus a flag.
    pub const fn from_hm(hours: i32, minutes: i32) -> Result<UtcOffset, Error> {
        if (hours < 0) != (minutes < 0) && hours != 0 && minutes != 0 {
            return Err(Error::BadOffset);
        }
        if minutes <= -60 || minutes >= 60 {
            return Err(Error::BadOffset);
        }
        UtcOffset::from_minutes(hours * 60 + minutes)
    }

    /// The offset in minutes, positive east of Greenwich.
    pub const fn minutes(self) -> i32 {
        self.minutes as i32
    }

    /// The offset in seconds, which is the form the conversions want.
    pub const fn seconds(self) -> i64 {
        self.minutes as i64 * 60
    }
}

/// An instant, seen from a particular offset. This is what a timestamp on the wire is: a civil
/// reading plus the offset it was read at, which together name one instant.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DateTime {
    civil: Civil,
    offset: UtcOffset,
}

impl DateTime {
    /// Pair a civil reading with the offset it was read at. Infallible: both halves are already
    /// valid, and no arithmetic happens until [`DateTime::to_unix`].
    pub const fn new(civil: Civil, offset: UtcOffset) -> DateTime {
        DateTime { civil, offset }
    }

    /// What the clock reads at `offset` when the Unix time is `secs`.
    ///
    /// `OutOfRange` here means the *local* reading falls outside years 0000-9999, which for any
    /// offset differs from the UTC bound by less than a day. That edge is the only place a caller
    /// can be surprised, and it is why this returns a `Result` where [`DateTime::new`] does not.
    pub const fn from_unix(secs: i64, offset: UtcOffset) -> Result<DateTime, Error> {
        let Some(local) = secs.checked_add(offset.seconds()) else {
            return Err(Error::OutOfRange);
        };
        match Civil::from_unix(local) {
            Ok(civil) => Ok(DateTime { civil, offset }),
            Err(e) => Err(e),
        }
    }

    /// The instant this names, as a Unix timestamp.
    ///
    /// Note this is *not* bounded by [`MIN_TIMESTAMP`]/[`MAX_TIMESTAMP`]: a valid local reading at
    /// the extreme end of the range with a large offset lands up to a day outside it. The value is
    /// still exact; it just may not survive a trip back through [`Civil::from_unix`].
    pub const fn to_unix(&self) -> i64 {
        self.civil.to_unix() - self.offset.seconds()
    }

    /// The local civil reading.
    pub const fn civil(&self) -> Civil {
        self.civil
    }

    /// The offset it was read at.
    pub const fn offset(&self) -> UtcOffset {
        self.offset
    }
}

/// The formats this crate can print. Five, chosen rather than derived, and the argument for each is
/// that something in this system needs exactly it.
///
/// **Why not `strftime`.** A format-string interpreter would be a second parser (one whose errors
/// appear at runtime, in a `no_std` program with no allocator, for a string nobody type-checked) in
/// exchange for combinations nothing here asks for. `%c` alone drags in locales. The five below
/// cover every consumer milestone 51 actually has, and adding a sixth is a two-line match arm plus a
/// test, which is a cheaper way to be wrong than a grammar is.
///
/// **Why no month names.** [`Format::Human`] keeps the ISO date and prepends the weekday, instead of
/// copying `date`'s `Wed Jul 30 12:34:56 UTC 2026`. Two reasons: that layout does not sort, and it
/// needs a month-name table which is the first step towards a locale question this crate has no
/// business answering. The weekday abbreviation is the entire natural-language surface here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    /// `2026-07-30T12:34:56Z`, or `2026-07-30T12:34:56+05:30` away from UTC. RFC 3339, the
    /// interchange form: what [`DateTime::parse_rfc3339`] reads back, what a log line or an NTP
    /// exchange should carry. Zero offset prints as `Z`.
    Rfc3339,
    /// `2026-07-30`. The date alone, for a directory listing or a filename.
    Date,
    /// `12:34:56`. The time alone.
    Time,
    /// `Thu 2026-07-30 12:34:56 UTC`, with the offset in place of `UTC` when there is one. The
    /// default a `date` command with no arguments should print: sortable, unambiguous, and it tells
    /// you the day of the week, which is the one thing a human wants that ISO does not give.
    Human,
    /// `1785414896`. The raw seconds, for arithmetic and for a caller that wants to hand the number
    /// straight back to the clock service.
    Unix,
}

/// The longest output any [`Format`] produces is `Human` at an extreme offset,
/// `Thu 9999-12-31 23:59:59 +23:59`, which is 30 bytes.
const FMT_CAP: usize = 32;

/// A formatted timestamp, in a fixed buffer. No allocator, and no lifetime tied to the value it came
/// from, so a caller can format and then print at leisure.
#[derive(Clone, Copy)]
pub struct Formatted {
    buf: [u8; FMT_CAP],
    len: u8,
}

impl Formatted {
    /// The bytes. Always ASCII (proved: `every_format_is_ascii`).
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }

    /// The text.
    ///
    /// Total, with no panic path: the bytes are ASCII by construction and the Kani harness
    /// `every_format_is_ascii` proves the `unwrap_or` branch is unreachable for every timestamp and
    /// every format. A `.expect()` here would be a panic in a userspace program to defend against a
    /// case that cannot arise.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).unwrap_or("")
    }
}

impl core::fmt::Display for Formatted {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl core::fmt::Debug for Formatted {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

impl AsRef<str> for Formatted {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// A cursor that writes into the fixed buffer. Every `push` is bounds-checked and silently drops on
/// overflow, which cannot happen ([`FMT_CAP`] is two bytes larger than the longest output) and is
/// still better than a panic in a program that only wanted to print the time.
struct Writer {
    buf: [u8; FMT_CAP],
    len: usize,
}

impl Writer {
    const fn new() -> Writer {
        Writer {
            buf: [0; FMT_CAP],
            len: 0,
        }
    }

    fn byte(&mut self, b: u8) {
        if self.len < FMT_CAP {
            self.buf[self.len] = b;
            self.len += 1;
        }
    }

    fn str(&mut self, s: &str) {
        for &b in s.as_bytes() {
            self.byte(b);
        }
    }

    /// A fixed-width zero-padded number, which is every numeric field in every format here except
    /// `Unix`.
    fn pad(&mut self, value: u32, width: usize) {
        let mut digits = [0u8; 10];
        let mut n = 0;
        let mut v = value;
        loop {
            digits[n] = b'0' + (v % 10) as u8;
            v /= 10;
            n += 1;
            if v == 0 {
                break;
            }
        }
        for _ in n..width {
            self.byte(b'0');
        }
        while n > 0 {
            n -= 1;
            self.byte(digits[n]);
        }
    }

    fn signed(&mut self, value: i64) {
        if value < 0 {
            self.byte(b'-');
        }
        // Via u64 so i64::MIN has no special case.
        let mut v = value.unsigned_abs();
        let mut digits = [0u8; 20];
        let mut n = 0;
        loop {
            digits[n] = b'0' + (v % 10) as u8;
            v /= 10;
            n += 1;
            if v == 0 {
                break;
            }
        }
        while n > 0 {
            n -= 1;
            self.byte(digits[n]);
        }
    }

    /// `+HH:MM` / `-HH:MM`.
    fn offset(&mut self, offset: UtcOffset) {
        let m = offset.minutes();
        self.byte(if m < 0 { b'-' } else { b'+' });
        let m = m.unsigned_abs();
        self.pad(m / 60, 2);
        self.byte(b':');
        self.pad(m % 60, 2);
    }

    fn finish(self) -> Formatted {
        Formatted {
            buf: self.buf,
            len: self.len as u8,
        }
    }
}

impl DateTime {
    /// Render in one of the five [`Format`]s. No allocation; the result owns its bytes.
    pub fn format(&self, format: Format) -> Formatted {
        let mut w = Writer::new();
        let c = self.civil;
        match format {
            Format::Rfc3339 => {
                write_date(&mut w, &c);
                w.byte(b'T');
                write_time(&mut w, &c);
                if self.offset.minutes == 0 {
                    w.byte(b'Z');
                } else {
                    w.offset(self.offset);
                }
            }
            Format::Date => write_date(&mut w, &c),
            Format::Time => write_time(&mut w, &c),
            Format::Human => {
                w.str(c.weekday().abbrev());
                w.byte(b' ');
                write_date(&mut w, &c);
                w.byte(b' ');
                write_time(&mut w, &c);
                w.byte(b' ');
                if self.offset.minutes == 0 {
                    w.str("UTC");
                } else {
                    w.offset(self.offset);
                }
            }
            Format::Unix => w.signed(self.to_unix()),
        }
        w.finish()
    }
}

impl Civil {
    /// Render this date as UTC. Shorthand for `DateTime::new(c, UtcOffset::UTC).format(f)`, which is
    /// what most callers want.
    pub fn format(&self, format: Format) -> Formatted {
        DateTime::new(*self, UtcOffset::UTC).format(format)
    }
}

fn write_date(w: &mut Writer, c: &Civil) {
    w.pad(c.year as u32, 4);
    w.byte(b'-');
    w.pad(c.month as u32, 2);
    w.byte(b'-');
    w.pad(c.day as u32, 2);
}

fn write_time(w: &mut Writer, c: &Civil) {
    w.pad(c.hour as u32, 2);
    w.byte(b':');
    w.pad(c.minute as u32, 2);
    w.byte(b':');
    w.pad(c.second as u32, 2);
}

impl DateTime {
    /// Parse RFC 3339, which is the interoperable subset of ISO 8601 and the only textual timestamp
    /// this crate reads.
    ///
    /// ```text
    /// YYYY-MM-DDTHH:MM:SS[.fraction](Z | ±HH:MM)
    /// ```
    ///
    /// Three deliberate relaxations, each of which RFC 3339 itself sanctions:
    ///
    /// - **Lowercase `t` and `z`** are accepted (§5.6 says they may be).
    /// - **A space in place of `T`** is accepted (§5.6's NOTE allows it by agreement, and it is what
    ///   a person types at a shell prompt).
    /// - **Fractional seconds are parsed and discarded.** This clock has one-second resolution, so
    ///   keeping them would mean either lying about precision or growing the type. Refusing them
    ///   would reject perfectly ordinary timestamps from other systems.
    ///
    /// Everything else is strict: exactly four year digits and two of everything else, no missing
    /// offset (an RFC 3339 timestamp always carries one, which is the whole difference from bare
    /// ISO 8601), no trailing bytes. `:60` is [`Error::LeapSecond`], not a syntax error, because it
    /// is well-formed text this calendar cannot represent (see the module docs).
    pub fn parse_rfc3339(s: &str) -> Result<DateTime, Error> {
        DateTime::parse_rfc3339_bytes(s.as_bytes())
    }

    /// [`DateTime::parse_rfc3339`] over bytes, which is what the grammar is actually defined on: RFC
    /// 3339 is ASCII, and every byte outside it is a syntax error whether or not it forms valid
    /// UTF-8.
    ///
    /// This is the real implementation and the `&str` version is the wrapper, not the other way
    /// round, for two reasons. A caller holding a network buffer or a shell argument has bytes and
    /// would otherwise have to validate UTF-8 first, only for this function to reject every non-ASCII
    /// byte anyway. And it lets the totality proof quantify over **arbitrary bytes**, including
    /// sequences that are not UTF-8 at all, rather than over the subset `from_utf8` would have let
    /// through.
    pub fn parse_rfc3339_bytes(b: &[u8]) -> Result<DateTime, Error> {
        // "0000-00-00T00:00:00Z" is 20 bytes, the shortest legal input.
        if b.len() < 20 {
            return Err(Error::Syntax);
        }
        let year = number(b, 0, 4)? as i32;
        expect(b, 4, b'-')?;
        let month = number(b, 5, 2)? as u8;
        expect(b, 7, b'-')?;
        let day = number(b, 8, 2)? as u8;
        match b[10] {
            b'T' | b't' | b' ' => {}
            _ => return Err(Error::Syntax),
        }
        let hour = number(b, 11, 2)? as u8;
        expect(b, 13, b':')?;
        let minute = number(b, 14, 2)? as u8;
        expect(b, 16, b':')?;
        let second = number(b, 17, 2)? as u8;
        if second == 60 {
            return Err(Error::LeapSecond);
        }

        let mut i = 19;
        if b[i] == b'.' {
            i += 1;
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i == start {
                return Err(Error::Syntax); // a '.' with no digits after it
            }
        }

        if i >= b.len() {
            return Err(Error::Syntax);
        }
        let offset = match b[i] {
            b'Z' | b'z' => {
                i += 1;
                UtcOffset::UTC
            }
            sign @ (b'+' | b'-') => {
                // ±HH:MM, six bytes.
                if i + 6 > b.len() {
                    return Err(Error::Syntax);
                }
                let oh = number(b, i + 1, 2)? as i32;
                expect(b, i + 3, b':')?;
                let om = number(b, i + 4, 2)? as i32;
                if om > 59 {
                    return Err(Error::BadOffset);
                }
                i += 6;
                let total = oh * 60 + om;
                UtcOffset::from_minutes(if sign == b'-' { -total } else { total })?
            }
            _ => return Err(Error::Syntax),
        };
        if i != b.len() {
            return Err(Error::Syntax);
        }

        let civil = Civil::new(year, month, day, hour, minute, second)?;
        Ok(DateTime::new(civil, offset))
    }
}

/// `width` ASCII digits at `at`, as a number. Refuses anything else, including a leading `+` or a
/// space, which is what keeps the grammar fixed-width.
fn number(b: &[u8], at: usize, width: usize) -> Result<u32, Error> {
    if at + width > b.len() {
        return Err(Error::Syntax);
    }
    let mut v = 0u32;
    for &c in &b[at..at + width] {
        if !c.is_ascii_digit() {
            return Err(Error::Syntax);
        }
        v = v * 10 + (c - b'0') as u32;
    }
    Ok(v)
}

fn expect(b: &[u8], at: usize, c: u8) -> Result<(), Error> {
    if b.get(at) == Some(&c) {
        Ok(())
    } else {
        Err(Error::Syntax)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A whole civil reading as a tuple, which is how the tables below state their expectations.
    type Fields = (i32, u8, u8, u8, u8, u8);

    /// A date without a time, for the rollover table.
    type Ymd = (i32, u8, u8);

    fn fields(c: &Civil) -> Fields {
        (
            c.year(),
            c.month(),
            c.day(),
            c.hour(),
            c.minute(),
            c.second(),
        )
    }

    /// The dates a reader can check against a wall calendar, plus the two the range is defined by.
    /// Every timestamp here was cross-checked against Python's `datetime` rather than derived from
    /// this crate, so the table is an independent witness rather than a snapshot of our own output.
    const KNOWN: &[(i64, i32, u8, u8, u8, u8, u8)] = &[
        (0, 1970, 1, 1, 0, 0, 0),                  // the epoch itself
        (-1, 1969, 12, 31, 23, 59, 59),            // one second before it
        (1_000_000_000, 2001, 9, 9, 1, 46, 40),    // the billennium
        (951_782_400, 2000, 2, 29, 0, 0, 0),       // the leap day the 400 rule creates
        (-2_203_891_200, 1900, 3, 1, 0, 0, 0),     // the day after the 1900 leap day that isn't
        (1_709_208_000, 2024, 2, 29, 12, 0, 0),    // an ordinary leap day
        (1_785_414_896, 2026, 7, 30, 12, 34, 56),  // the day this was written
        (MIN_TIMESTAMP, 0, 1, 1, 0, 0, 0),         // 0000-01-01T00:00:00Z
        (MAX_TIMESTAMP, 9999, 12, 31, 23, 59, 59), // 9999-12-31T23:59:59Z
        (-62_135_596_800, 1, 1, 1, 0, 0, 0),       // 0001-01-01, where Python's calendar starts
    ];

    #[test]
    fn known_timestamps_convert_both_ways() {
        for &(ts, y, mo, d, h, mi, s) in KNOWN {
            let c = Civil::from_unix(ts).unwrap();
            assert_eq!(fields(&c), (y, mo, d, h, mi, s), "from_unix({ts})");
            assert_eq!(Civil::new(y, mo, d, h, mi, s).unwrap().to_unix(), ts);
        }
    }

    /// 1900 is not a leap year and 2000 is. This is the pair that breaks date code written from
    /// memory: the "divisible by 4" rule gets both wrong, and "except centuries" gets 2000 wrong.
    #[test]
    fn the_century_rules() {
        assert!(!is_leap_year(1900));
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(2100));
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(2023));
        assert!(is_leap_year(0)); // year 0 is divisible by 400

        assert_eq!(days_in_month(1900, 2), Some(28));
        assert_eq!(days_in_month(2000, 2), Some(29));
        assert_eq!(days_in_month(2024, 2), Some(29));
        assert_eq!(days_in_month(2023, 2), Some(28));
        assert_eq!(days_in_month(2024, 13), None);
        assert_eq!(days_in_month(2024, 0), None);

        // The dates themselves, not just the predicate.
        assert_eq!(Civil::new(1900, 2, 29, 0, 0, 0), Err(Error::BadDay));
        assert!(Civil::new(2000, 2, 29, 0, 0, 0).is_ok());
        assert_eq!(Civil::new(2023, 2, 29, 0, 0, 0), Err(Error::BadDay));
        assert!(Civil::new(2024, 2, 29, 0, 0, 0).is_ok());
    }

    /// Every rollover a month can have: 28 to 29, 29 to 1, 30 to 1, 31 to 1, and December 31 to
    /// January 1 (which is also a year rollover).
    #[test]
    fn month_and_year_ends_roll_over() {
        let cases: &[(Ymd, Ymd)] = &[
            ((2023, 2, 28), (2023, 3, 1)),  // non-leap February ends at 28
            ((2024, 2, 28), (2024, 2, 29)), // leap February does not
            ((2024, 2, 29), (2024, 3, 1)),
            ((1900, 2, 28), (1900, 3, 1)), // the century that is not a leap year
            ((2000, 2, 28), (2000, 2, 29)), // the century that is
            ((2026, 4, 30), (2026, 5, 1)), // a 30-day month
            ((2026, 7, 31), (2026, 8, 1)), // a 31-day month
            ((2026, 8, 31), (2026, 9, 1)), // the two 31s in a row
            ((2026, 12, 31), (2027, 1, 1)), // the year end
        ];
        for &((y1, m1, d1), (y2, m2, d2)) in cases {
            let a = Civil::new(y1, m1, d1, 23, 59, 59).unwrap();
            let b = Civil::from_unix(a.to_unix() + 1).unwrap();
            assert_eq!(
                fields(&b),
                (y2, m2, d2, 0, 0, 0),
                "one second after {y1}-{m1}-{d1}T23:59:59Z"
            );
        }
    }

    /// The year-2038 problem, proved to be a non-event here rather than assumed. `i32::MAX` seconds
    /// is 2038-01-19T03:14:07Z, the last instant a 32-bit `time_t` can name; the next second is
    /// ordinary, and so is a timestamp a century past it.
    #[test]
    fn the_2038_boundary_is_not_a_boundary() {
        let last32 = i32::MAX as i64;
        let c = Civil::from_unix(last32).unwrap();
        assert_eq!(c.format(Format::Rfc3339).as_str(), "2038-01-19T03:14:07Z");

        let next = Civil::from_unix(last32 + 1).unwrap();
        assert_eq!(
            next.format(Format::Rfc3339).as_str(),
            "2038-01-19T03:14:08Z"
        );
        assert_eq!(next.to_unix() - c.to_unix(), 1);

        // And a long way past it, where a 32-bit counter would have wrapped several times.
        let far = Civil::new(2138, 1, 19, 3, 14, 8).unwrap();
        assert_eq!(Civil::from_unix(far.to_unix()).unwrap(), far);
    }

    /// Times before the epoch are ordinary. The trap is truncating division: in Rust `-1 / 86400`
    /// is 0, so a naive implementation reports 1970-01-01 for every instant in the last day of 1969.
    #[test]
    fn negative_timestamps_are_ordinary() {
        for secs in [-1i64, -86_399, -86_400, -86_401, -1_000_000_000] {
            let c = Civil::from_unix(secs).unwrap();
            assert_eq!(c.to_unix(), secs);
            assert!(c.year() < 1970);
        }
        assert_eq!(
            Civil::from_unix(-86_400)
                .unwrap()
                .format(Format::Rfc3339)
                .as_str(),
            "1969-12-31T00:00:00Z"
        );
        assert_eq!(
            Civil::from_unix(-86_401)
                .unwrap()
                .format(Format::Rfc3339)
                .as_str(),
            "1969-12-30T23:59:59Z"
        );
    }

    #[test]
    fn out_of_range_timestamps_are_refused_not_wrapped() {
        assert_eq!(Civil::from_unix(MIN_TIMESTAMP - 1), Err(Error::OutOfRange));
        assert_eq!(Civil::from_unix(MAX_TIMESTAMP + 1), Err(Error::OutOfRange));
        assert_eq!(Civil::from_unix(i64::MIN), Err(Error::OutOfRange));
        assert_eq!(Civil::from_unix(i64::MAX), Err(Error::OutOfRange));
        assert_eq!(Civil::new(10_000, 1, 1, 0, 0, 0), Err(Error::OutOfRange));
        assert_eq!(Civil::new(-1, 1, 1, 0, 0, 0), Err(Error::OutOfRange));
    }

    #[test]
    fn each_bad_field_names_itself() {
        assert_eq!(Civil::new(2026, 0, 1, 0, 0, 0), Err(Error::BadMonth));
        assert_eq!(Civil::new(2026, 13, 1, 0, 0, 0), Err(Error::BadMonth));
        assert_eq!(Civil::new(2026, 1, 0, 0, 0, 0), Err(Error::BadDay));
        assert_eq!(Civil::new(2026, 4, 31, 0, 0, 0), Err(Error::BadDay));
        assert_eq!(Civil::new(2026, 1, 1, 24, 0, 0), Err(Error::BadHour));
        assert_eq!(Civil::new(2026, 1, 1, 0, 60, 0), Err(Error::BadMinute));
        assert_eq!(Civil::new(2026, 1, 1, 0, 0, 60), Err(Error::BadSecond));
    }

    /// Weekdays, pinned against dates a reader can check. 1970-01-01 was a Thursday, which is the
    /// anchor the arithmetic uses, so the other four are the ones that would catch an off-by-one.
    #[test]
    fn weekdays() {
        let cases: &[((i32, u8, u8), Weekday)] = &[
            ((1970, 1, 1), Weekday::Thursday),
            ((2000, 1, 1), Weekday::Saturday),
            ((2000, 2, 29), Weekday::Tuesday),
            ((1900, 1, 1), Weekday::Monday),
            ((2026, 7, 30), Weekday::Thursday),
            ((1969, 12, 31), Weekday::Wednesday), // before the epoch, where rem_euclid matters
            ((2038, 1, 19), Weekday::Tuesday),
        ];
        for &((y, m, d), want) in cases {
            assert_eq!(
                Civil::new(y, m, d, 0, 0, 0).unwrap().weekday(),
                want,
                "{y}-{m}-{d}"
            );
        }
        assert_eq!(Weekday::Monday.iso_number(), 1);
        assert_eq!(Weekday::Sunday.iso_number(), 7);
        assert_eq!(Weekday::Thursday.abbrev(), "Thu");
        assert_eq!(Weekday::Thursday.name(), "Thursday");
    }

    #[test]
    fn day_of_year() {
        assert_eq!(Civil::new(2026, 1, 1, 0, 0, 0).unwrap().day_of_year(), 1);
        assert_eq!(
            Civil::new(2026, 12, 31, 0, 0, 0).unwrap().day_of_year(),
            365
        );
        assert_eq!(
            Civil::new(2024, 12, 31, 0, 0, 0).unwrap().day_of_year(),
            366
        );
        assert_eq!(Civil::new(2024, 3, 1, 0, 0, 0).unwrap().day_of_year(), 61);
        assert_eq!(Civil::new(2023, 3, 1, 0, 0, 0).unwrap().day_of_year(), 60);
        assert_eq!(
            Civil::new(1900, 12, 31, 0, 0, 0).unwrap().day_of_year(),
            365
        );
        assert_eq!(
            Civil::new(2000, 12, 31, 0, 0, 0).unwrap().day_of_year(),
            366
        );
    }

    #[test]
    fn the_five_formats() {
        let dt = DateTime::from_unix(1_785_414_896, UtcOffset::UTC).unwrap();
        assert_eq!(dt.format(Format::Rfc3339).as_str(), "2026-07-30T12:34:56Z");
        assert_eq!(dt.format(Format::Date).as_str(), "2026-07-30");
        assert_eq!(dt.format(Format::Time).as_str(), "12:34:56");
        assert_eq!(
            dt.format(Format::Human).as_str(),
            "Thu 2026-07-30 12:34:56 UTC"
        );
        assert_eq!(dt.format(Format::Unix).as_str(), "1785414896");

        // Away from UTC the same instant reads differently and says so.
        let ist = DateTime::from_unix(1_785_414_896, UtcOffset::from_hm(5, 30).unwrap()).unwrap();
        assert_eq!(
            ist.format(Format::Rfc3339).as_str(),
            "2026-07-30T18:04:56+05:30"
        );
        assert_eq!(
            ist.format(Format::Human).as_str(),
            "Thu 2026-07-30 18:04:56 +05:30"
        );
        assert_eq!(ist.format(Format::Unix).as_str(), "1785414896"); // the instant is unchanged

        let pst = DateTime::from_unix(1_785_414_896, UtcOffset::from_hm(-8, 0).unwrap()).unwrap();
        assert_eq!(
            pst.format(Format::Rfc3339).as_str(),
            "2026-07-30T04:34:56-08:00"
        );

        // Zero padding everywhere, including a year that needs three leading zeros.
        let early = Civil::new(7, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(
            early.format(Format::Rfc3339).as_str(),
            "0007-01-02T03:04:05Z"
        );

        // A negative Unix value prints its sign.
        assert_eq!(
            Civil::from_unix(-1).unwrap().format(Format::Unix).as_str(),
            "-1"
        );

        // The buffer is big enough for the longest output, which is Human at an extreme offset.
        let widest = DateTime::new(
            Civil::new(9999, 12, 31, 23, 59, 59).unwrap(),
            UtcOffset::from_hm(23, 59).unwrap(),
        );
        assert_eq!(
            widest.format(Format::Human).as_str(),
            "Fri 9999-12-31 23:59:59 +23:59"
        );
        assert!(widest.format(Format::Human).as_bytes().len() <= FMT_CAP);
    }

    #[test]
    fn parse_accepts_rfc3339() {
        let cases: &[(&str, i64)] = &[
            ("1970-01-01T00:00:00Z", 0),
            ("2026-07-30T12:34:56Z", 1_785_414_896),
            ("2026-07-30t12:34:56z", 1_785_414_896), // lowercase, allowed by 5.6
            ("2026-07-30 12:34:56Z", 1_785_414_896), // space separator, the 5.6 NOTE
            ("2026-07-30T12:34:56.789Z", 1_785_414_896), // fraction parsed and dropped
            ("2026-07-30T12:34:56.000000001Z", 1_785_414_896),
            ("2026-07-30T18:04:56+05:30", 1_785_414_896), // same instant, other offset
            ("2026-07-30T04:34:56-08:00", 1_785_414_896),
            ("2026-07-30T12:34:56+00:00", 1_785_414_896), // an explicit zero offset
            ("1969-12-31T23:59:59Z", -1),
            ("2038-01-19T03:14:08Z", 2_147_483_648),
            ("0000-01-01T00:00:00Z", MIN_TIMESTAMP),
            ("9999-12-31T23:59:59Z", MAX_TIMESTAMP),
        ];
        for &(text, secs) in cases {
            let dt = DateTime::parse_rfc3339(text).unwrap_or_else(|e| panic!("{text}: {e:?}"));
            assert_eq!(dt.to_unix(), secs, "{text}");
        }

        // The offset survives parsing, it is not normalised away.
        let dt = DateTime::parse_rfc3339("2026-07-30T18:04:56+05:30").unwrap();
        assert_eq!(dt.offset().minutes(), 330);
        assert_eq!(dt.civil().hour(), 18);
        assert_eq!(
            dt.format(Format::Rfc3339).as_str(),
            "2026-07-30T18:04:56+05:30"
        );
    }

    #[test]
    fn parse_refuses_what_it_should() {
        let cases: &[(&str, Error)] = &[
            ("", Error::Syntax),
            ("2026-07-30", Error::Syntax), // no time, no offset
            ("2026-07-30T12:34:56", Error::Syntax), // no offset: this is the ISO/RFC difference
            ("2026-07-30T12:34:56Zx", Error::Syntax), // trailing bytes
            ("2026-07-30X12:34:56Z", Error::Syntax), // wrong separator
            ("2026/07/30T12:34:56Z", Error::Syntax),
            ("26-07-30T12:34:56Z", Error::Syntax), // two-digit year
            ("2026-7-30T12:34:56Z", Error::Syntax), // unpadded month
            ("2026-07-30T12:34:56.Z", Error::Syntax), // a dot with no digits
            ("2026-07-30T12:34:56+0530", Error::Syntax), // offset needs its colon
            ("2026-07-30T12:34:56+05", Error::Syntax),
            ("2026-07-30T12:34:60Z", Error::LeapSecond), // the one that is not a syntax error
            ("2026-07-30T12:60:56Z", Error::BadMinute),
            ("2026-07-30T24:00:00Z", Error::BadHour),
            ("2026-02-30T12:34:56Z", Error::BadDay),
            ("2023-02-29T12:34:56Z", Error::BadDay), // valid-looking, wrong year
            ("2026-13-01T12:34:56Z", Error::BadMonth),
            ("2026-07-30T12:34:56+05:60", Error::BadOffset),
            ("2026-07-30T12:34:56+24:00", Error::BadOffset),
            // Malformed twice over, and the offset is judged before the trailing byte is. The
            // six-byte check ahead of the offset is a length check; a mutant that turns it into a
            // trailing-bytes check refuses the same inputs and renames this refusal Syntax.
            ("2026-07-30T12:34:56+05:60x", Error::BadOffset),
        ];
        for &(text, want) in cases {
            assert_eq!(DateTime::parse_rfc3339(text), Err(want), "{text:?}");
        }
    }

    /// Format and parse are inverses over the whole supported range, sampled densely. The Kani
    /// harness proves this for every timestamp in a smaller window; this one walks the extremes and
    /// a large stride, which is the coverage a bounded proof cannot reach.
    #[test]
    fn format_and_parse_round_trip_across_the_range() {
        let mut secs = MIN_TIMESTAMP;
        // Prime-ish, so it lands on every hour and weekday over time. Under Miri the walk is
        // sampled three orders of magnitude coarser: the native stride is ~315,000 format+parse
        // round trips, which an interpreter turns from a second into the better part of an hour.
        // ~300 samples keep the round trip on Miri's paths; the native run keeps the density.
        // "Miri-clean" means the sampled walk (notes/undefined-behavior.md).
        let stride = if cfg!(miri) { 1_000_036_991 } else { 1_000_037 };
        while secs <= MAX_TIMESTAMP {
            let dt = DateTime::from_unix(secs, UtcOffset::UTC).unwrap();
            let text = dt.format(Format::Rfc3339);
            let back = DateTime::parse_rfc3339(text.as_str()).unwrap();
            assert_eq!(back.to_unix(), secs, "{}", text.as_str());
            secs += stride;
        }
    }

    #[test]
    fn utc_offsets() {
        assert_eq!(UtcOffset::UTC.minutes(), 0);
        assert_eq!(UtcOffset::from_hm(5, 30).unwrap().minutes(), 330);
        assert_eq!(UtcOffset::from_hm(-8, 0).unwrap().seconds(), -28_800);
        assert_eq!(UtcOffset::from_minutes(-1439).unwrap().minutes(), -1439);
        assert_eq!(UtcOffset::from_minutes(1440), Err(Error::BadOffset));
        assert_eq!(UtcOffset::from_minutes(-1440), Err(Error::BadOffset));
        assert_eq!(UtcOffset::from_hm(5, -30), Err(Error::BadOffset)); // mixed signs
        assert_eq!(UtcOffset::from_hm(-5, 30), Err(Error::BadOffset)); // and mixed the other way
        // Both halves negative is one sign written twice, not mixed signs, and it is how -05:30 is
        // written. Only the negative-hours row of the guard's truth table proves that; with 5, -30
        // alone the `hours < 0` test can rot into anything and every case still lands right.
        assert_eq!(UtcOffset::from_hm(-5, -30).unwrap().minutes(), -330);
        assert_eq!(UtcOffset::from_hm(0, 60), Err(Error::BadOffset));

        // A local reading can fall outside the range even when the instant is inside it. That edge
        // is documented on from_unix rather than hidden.
        assert_eq!(
            DateTime::from_unix(MAX_TIMESTAMP, UtcOffset::from_hm(1, 0).unwrap()),
            Err(Error::OutOfRange)
        );
    }

    /// `Civil`'s derived ordering is chronological, which the formatting relies on being true and
    /// which a field reorder would silently break.
    #[test]
    fn civil_ordering_is_chronological() {
        let a = Civil::new(2026, 7, 30, 12, 0, 0).unwrap();
        let b = Civil::new(2026, 8, 1, 0, 0, 0).unwrap();
        let c = Civil::new(2027, 1, 1, 0, 0, 0).unwrap();
        assert!(a < b && b < c);
        assert!(a.to_unix() < b.to_unix() && b.to_unix() < c.to_unix());
    }

    // The tests from here down each pin something milestone 85's mutation run showed nothing
    // pinned. See notes/mutation-testing.md.

    /// The parser's edges the refusal table above did not reach: a fraction that runs off the end
    /// of the input (the digit scan's bound could become `<=` and read one past), an offset whose
    /// colon is elsewhere than checked (the `expect` index could become `i - 3`, which lands on the
    /// seconds colon and is ':' in EVERY well-formed input, so garbage like `+05300` parsed), and
    /// the last legal offset minute.
    #[test]
    fn parser_edges_the_mutation_run_found() {
        assert_eq!(
            DateTime::parse_rfc3339("2026-07-30T12:34:56.5"),
            Err(Error::Syntax),
            "input ends inside the fraction"
        );
        assert_eq!(
            DateTime::parse_rfc3339("2026-07-30T12:34:56+05300"),
            Err(Error::Syntax),
            "six bytes of offset, colon in the wrong place"
        );
        let ok = DateTime::parse_rfc3339("2026-07-30T12:34:56+05:59").unwrap();
        assert_eq!(
            ok.offset().minutes(),
            5 * 60 + 59,
            ":59 is the last legal offset minute"
        );
    }

    /// A known Sunday, absolutely, not relative to another weekday: the cycle test above proves
    /// the weekdays rotate, and rotation is preserved by deleting one match arm and letting the
    /// wildcard eat it.
    #[test]
    fn the_epoch_week_lands_on_the_right_days() {
        let sunday = Civil::new(1970, 1, 4, 0, 0, 0).unwrap();
        assert_eq!(sunday.weekday(), Weekday::Sunday);
        let epoch = Civil::new(1970, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(epoch.weekday(), Weekday::Thursday);
    }

    /// Zero is "0": the sign branch in the Unix format is `value < 0`, and `<=` would print "-0",
    /// which is not a timestamp anything reads back.
    #[test]
    fn unix_zero_formats_without_a_sign() {
        let epoch = Civil::new(1970, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(epoch.format(Format::Unix).as_str(), "0");
    }

    /// `Formatted` speaks for itself through three impls, and each has to say the same thing: a
    /// replaced body returning "" or Ok(()) would print nothing, silently, everywhere a timestamp
    /// is shown.
    #[test]
    fn formatted_says_the_same_thing_three_ways() {
        use core::fmt::Write;

        /// This crate is `no_std` with no allocator even under test, so catching what `{}` and
        /// `{:?}` actually emit takes a fixed buffer that implements `fmt::Write`.
        struct Sink {
            buf: [u8; 64],
            len: usize,
        }
        impl Sink {
            fn new() -> Sink {
                Sink {
                    buf: [0; 64],
                    len: 0,
                }
            }
        }
        impl Write for Sink {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                let end = self.len + s.len();
                self.buf[self.len..end].copy_from_slice(s.as_bytes());
                self.len = end;
                Ok(())
            }
        }
        impl Sink {
            fn as_str(&self) -> &str {
                core::str::from_utf8(&self.buf[..self.len]).unwrap()
            }
        }

        let f = Civil::new(2026, 7, 30, 12, 34, 56)
            .unwrap()
            .format(Format::Rfc3339);
        assert_eq!(f.as_str(), "2026-07-30T12:34:56Z");
        assert_eq!(f.as_ref(), f.as_str());

        let mut shown = Sink::new();
        write!(shown, "{f}").unwrap();
        assert_eq!(shown.as_str(), "2026-07-30T12:34:56Z");

        let mut debugged = Sink::new();
        write!(debugged, "{f:?}").unwrap();
        assert_eq!(debugged.as_str(), "\"2026-07-30T12:34:56Z\"");
    }

    /// Every refusal has words, and its own words: two variants sharing a message would make a
    /// refusal ambiguous at the one place it is read, a shell with no allocator.
    #[test]
    fn every_error_has_its_own_message() {
        const ALL: &[Error] = &[
            Error::OutOfRange,
            Error::BadMonth,
            Error::BadDay,
            Error::BadHour,
            Error::BadMinute,
            Error::BadSecond,
            Error::BadOffset,
            Error::LeapSecond,
            Error::Syntax,
        ];
        for (i, a) in ALL.iter().enumerate() {
            assert!(!a.as_str().is_empty());
            for b in &ALL[i + 1..] {
                assert_ne!(a.as_str(), b.as_str());
            }
        }
    }
}

/// **The machine-checked half** (`script/verify`, notes/verification.md).
///
/// A calendar looks like an ideal bounded-model-checking target: closed-form integer arithmetic, no
/// loops, no tables, no allocation. It half is, and where it is not turned out to be the interesting
/// finding of this lane, so it is recorded here rather than left as an unexplained set of bounds.
///
/// **The calendar arithmetic is cheap to prove and the seconds-per-day division is not.** The
/// Hinnant algorithms round-trip over the *entire* supported range, all 3,652,425 days of it, in
/// under a minute. Add one `div_euclid(86_400)` over a symbolic 64-bit timestamp and the same
/// property does not finish in twenty. That is not a fact about calendars; it is that a bit-blasted
/// 64-bit divider with an unconstrained dividend is one of the hardest things you can hand a CDCL
/// SAT solver, and the divisions inside `civil_from_days` are cheap only because the era arithmetic
/// keeps their operands small. Measured, not assumed: one day of timestamps proves in 3s, four years
/// in 64s, the full range in neither twenty minutes nor with kissat instead of CaDiCaL.
///
/// So the harnesses are **factored along that seam** rather than bounded uniformly:
///
/// - the calendar half (`days <-> (y, m, d)`, and everything derived from a day number) is proved
///   over the **full range**, unbounded below the type;
/// - the seconds half (the `86_400` split, which is the only place the expensive division appears)
///   is proved over a **four-year window that straddles the epoch**, chosen because the risk it
///   carries is "did someone use truncating division instead of Euclidean", which is a property of
///   the *code* and shows up on any negative timestamp. The window contains negative seconds, zero,
///   positive seconds, a leap day and two year boundaries.
///
/// The two compose: `from_unix` is `civil_from_days(secs.div_euclid(86_400))` plus a time of day,
/// and `to_unix` is `days_from_civil(...) * 86_400` plus the same time of day. Saying that plainly is
/// better than a uniform bound that quietly excluded the year 3000, and the unit test
/// `format_and_parse_round_trip_across_the_range` walks the whole ten thousand years at a stride to
/// cover what a bounded proof cannot.
#[cfg(kani)]
mod verification {
    use super::*;

    /// The first and last day numbers in the supported range: 0000-01-01 and 9999-12-31.
    const FIRST_DAY: i64 = days_from_civil(MIN_YEAR, 1, 1);
    const LAST_DAY: i64 = days_from_civil(MAX_YEAR, 12, 31);

    /// **Any representable `DateTime`**, built from six symbolic fields and a symbolic offset, or
    /// `None` for the field combinations that are not a real date.
    ///
    /// The text harnesses take their input this way rather than from a symbolic timestamp, and that
    /// is the difference between a proof over one day and a proof over the whole range: constructing
    /// the value from fields skips `from_unix`, and `from_unix` is where the expensive
    /// `div_euclid(86_400)` lives (see the module comment). Coverage is identical, since every
    /// `DateTime` this crate can hold is exactly a valid `Civil` and a valid offset.
    fn any_datetime() -> Option<DateTime> {
        let year: i32 = kani::any();
        let month: u8 = kani::any();
        let day: u8 = kani::any();
        let hour: u8 = kani::any();
        let minute: u8 = kani::any();
        let second: u8 = kani::any();
        let om: i32 = kani::any();
        kani::assume((-1439..=1439).contains(&om));
        let civil = Civil::new(year, month, day, hour, minute, second).ok()?;
        Some(DateTime::new(civil, UtcOffset::from_minutes(om).unwrap()))
    }

    /// **The central theorem: the two calendar algorithms are mutual inverses.** For every one of the
    /// 3,652,425 days in the supported range, `civil_from_days` then `days_from_civil` returns the
    /// day it started with. No bound below the type's own.
    ///
    /// If this holds, everything else about the calendar follows: the leap-year rules, the month
    /// lengths, the century exceptions and their exception are all *inside* these two functions, and
    /// a bijection cannot have them wrong in a way that cancels (a wrong month length would have to
    /// map two days to one date, which is not injective).
    #[kani::proof]
    fn the_calendar_algorithms_are_mutual_inverses() {
        let days: i64 = kani::any();
        kani::assume((FIRST_DAY..=LAST_DAY).contains(&days));
        let (y, m, d) = civil_from_days(days);
        assert_eq!(days_from_civil(y, m, d), days);
    }

    /// **A day number always decodes to a date that exists.** For every day in the range: the year is
    /// in range, the month is 1..=12, and the day is within *that month's length in that year*, which
    /// is the leap-year rule applied. The strongest phrasing is the last line: the validating
    /// constructor accepts what the decoder produced, so `from_unix` cannot manufacture a `Civil` that
    /// `Civil::new` would have refused.
    ///
    /// This is the harness that stops the bijection above from being vacuous. A decoder that
    /// consistently invented February 30 would still be a bijection.
    #[kani::proof]
    fn a_day_number_always_decodes_to_a_real_date() {
        let days: i64 = kani::any();
        kani::assume((FIRST_DAY..=LAST_DAY).contains(&days));
        let (y, m, d) = civil_from_days(days);
        assert!((MIN_YEAR..=MAX_YEAR).contains(&y));
        assert!((1..=12).contains(&m));
        assert!(1 <= d && d <= days_in_month(y, m).unwrap());
        assert!(Civil::new(y, m, d, 0, 0, 0).is_ok());
    }

    /// **The other direction, over every field value there is.** Year, month, day, hour, minute and
    /// second are all fully symbolic, so February 30, hour 99 and year 10,000 are among the inputs.
    /// For every combination `Civil::new` accepts, the date survives a trip through its day number,
    /// and the timestamp it computes lies inside the supported range.
    ///
    /// `to_unix` appears here in full (it multiplies by 86,400 rather than dividing, which is the
    /// cheap direction), so the range assertion is proved for every representable date.
    #[kani::proof]
    fn every_real_date_survives_its_own_day_number() {
        let year: i32 = kani::any();
        let month: u8 = kani::any();
        let day: u8 = kani::any();
        let hour: u8 = kani::any();
        let minute: u8 = kani::any();
        let second: u8 = kani::any();

        let Ok(civil) = Civil::new(year, month, day, hour, minute, second) else {
            return; // refused, and `a_day_number_always_decodes_to_a_real_date` covers the accepted set
        };
        assert_eq!(
            civil_from_days(civil.days_since_epoch()),
            (year, month, day)
        );
        let secs = civil.to_unix();
        assert!((MIN_TIMESTAMP..=MAX_TIMESTAMP).contains(&secs));
        // Not vacuous, and specifically not on the case that matters: the constructor does accept a
        // 29th of February, so this is quantifying over leap days rather than over an input set the
        // validation quietly emptied.
        kani::cover!(month == 2 && day == 29);
    }

    /// **Time only moves forward.** Every day in the range decodes to a date that sorts strictly
    /// after the day before it, under `Civil`'s derived `Ord`. That is what makes `Format::Date`
    /// sortable text and what a log reader assumes, and it rules out the class of bug where a
    /// boundary (a month end, a leap day, a century) steps the calendar backwards for one day.
    ///
    /// Stated on **adjacent** days rather than on an arbitrary pair, which is the same theorem: the
    /// order is transitive, so `d(n) < d(n+1)` for every `n` gives `d(a) < d(b)` for every `a < b`.
    /// It is also several times cheaper to check, because a second symbolic day number costs a second
    /// copy of the whole decode: 228s measured for the pair, 38-50s for the adjacent form. Worth
    /// stating rather than quietly enjoying, since it reads like a weaker claim and is not: this is a
    /// proof of the general property, phrased as its induction step.
    #[kani::proof]
    fn each_day_sorts_after_the_one_before_it() {
        let days: i64 = kani::any();
        kani::assume((FIRST_DAY..LAST_DAY).contains(&days));
        let (ay, am, ad) = civil_from_days(days);
        let (by, bm, bd) = civil_from_days(days + 1);
        assert!(
            Civil::new(ay, am, ad, 0, 0, 0).unwrap() < Civil::new(by, bm, bd, 0, 0, 0).unwrap()
        );
    }

    /// **Day of year is 1..=366, and 366 happens only on 31 December of a leap year.** Full range,
    /// every day. The `if and only if` is the part worth having: a day-of-year that saturates at 365,
    /// or that reaches 366 in a common year, both fail this.
    #[kani::proof]
    fn day_of_year_is_bounded_and_366_means_leap() {
        let days: i64 = kani::any();
        kani::assume((FIRST_DAY..=LAST_DAY).contains(&days));
        let (y, m, d) = civil_from_days(days);
        let c = Civil::new(y, m, d, 0, 0, 0).unwrap();
        let doy = c.day_of_year();
        assert!((1..=366).contains(&doy));
        assert_eq!(doy == 366, is_leap_year(y) && m == 12 && d == 31);
        assert!(doy <= 365 || is_leap_year(y));
        // The interesting value is reachable: without this, "366 implies leap year" would hold
        // vacuously on an implementation that never returns 366.
        kani::cover!(doy == 366);
    }

    /// **The week advances one day at a time and closes after seven.** Three facts, over every day in
    /// the range: consecutive days differ, the ISO number steps 1..=7 in order (not merely changes),
    /// and seven days on is the same weekday. The last is what makes it a cycle rather than a
    /// sequence, and together they pin the `rem_euclid` that a truncating `%` would break for
    /// pre-epoch dates.
    #[kani::proof]
    fn weekdays_advance_one_day_at_a_time() {
        let days: i64 = kani::any();
        kani::assume((FIRST_DAY..LAST_DAY).contains(&days));
        let today = Weekday::from_days(days);
        let tomorrow = Weekday::from_days(days + 1);
        assert!(today != tomorrow);
        assert_eq!(tomorrow.iso_number(), today.iso_number() % 7 + 1);
        assert_eq!(Weekday::from_days(days + 7), today);
    }

    /// **The seconds round trip**, which is the calendar theorem above plus the one thing it does not
    /// cover: the `86_400` split, and whether the code divides the way a calendar needs.
    ///
    /// Bounded to 1968-01-01T00:00:00Z through 1971-12-31T23:59:59Z, and the window is chosen rather
    /// than convenient. It straddles the epoch, so it contains negative timestamps, zero, and positive
    /// ones; in Rust `-1 / 86400` is `0` and the answer a calendar needs is `-1`, so truncating
    /// division here reports 1970-01-01 for every instant in the last day of 1969, and *only* a
    /// negative timestamp catches it. It is four years, so it contains a leap day (1968-02-29), all
    /// twelve month lengths, and three year boundaries.
    ///
    /// What the bound excludes is the *calendar* behaviour at other years, and that is exactly what
    /// `the_calendar_algorithms_are_mutual_inverses` proves over the full range. The composition is
    /// stated in the module comment above.
    #[kani::proof]
    fn unix_to_civil_and_back_is_the_identity() {
        const START: i64 = days_from_civil(1968, 1, 1) * SECS_PER_DAY;
        const END: i64 = days_from_civil(1971, 12, 31) * SECS_PER_DAY + SECS_PER_DAY - 1;
        let secs: i64 = kani::any();
        kani::assume((START..=END).contains(&secs));

        let c = Civil::from_unix(secs).expect("in range");
        assert!(c.hour() <= 23 && c.minute() <= 59 && c.second() <= 59);
        assert_eq!(c.to_unix(), secs);
        // The two things the window was chosen for are reachable inside it, checked rather than
        // asserted in prose: a pre-epoch timestamp (where truncating division breaks) and a leap day.
        kani::cover!(secs < 0);
        kani::cover!(c.month() == 2 && c.day() == 29);
    }

    /// **Formatting is total, never truncates, and is always printable ASCII**, for every format, at
    /// every legal UTC offset, over one day of timestamps.
    ///
    /// It proves the fixed buffer is big enough (the writer's silent-drop branch is never taken for
    /// any input it can be handed), and it is what makes `Formatted::as_str`'s `unwrap_or("")`
    /// unreachable rather than an untested fallback. That last step is one line of composition rather
    /// than a proof through `core::str::from_utf8`: **ASCII is a subset of UTF-8**, so a buffer whose
    /// every byte is ASCII decodes, always. Proving it *through* the standard library's validator
    /// instead costs minutes of symbolic execution and adds nothing.
    #[kani::proof]
    #[kani::unwind(34)]
    fn every_format_is_ascii() {
        let Some(dt) = any_datetime() else { return };
        printable(&dt.format(Format::Rfc3339));
        printable(&dt.format(Format::Date));
        printable(&dt.format(Format::Time));
        printable(&dt.format(Format::Human));
        printable(&dt.format(Format::Unix));
    }

    /// The check `every_format_is_ascii` applies, written as an **index** loop over the fixed buffer
    /// rather than a `for` over the variable-length slice.
    ///
    /// That is not a style preference, it is the difference between a proof that finishes and one that
    /// does not. Iterating a slice whose *length* is symbolic makes CBMC branch on the length at every
    /// step, and symbolic execution alone ran past ten minutes; the same property over the fixed array
    /// with an `i < len` guard verifies in seconds. Worth knowing before writing the next harness over
    /// a `&[u8]`.
    fn printable(f: &Formatted) {
        assert!(f.len as usize <= FMT_CAP);
        assert!(f.len > 0, "no format is empty");
        let mut i = 0;
        while i < FMT_CAP {
            if i < f.len as usize {
                let b = f.buf[i];
                assert!(b.is_ascii() && b >= 0x20, "output must be printable ASCII");
            }
            i += 1;
        }
    }

    /// **The `Unix` format fits any `i64`**, `i64::MIN` included, where the `-` and twenty digits are
    /// the longest a signed 64-bit number can be. Separate from the harness above because this arm's
    /// length depends on the magnitude of the number, which is precisely what a fixed buffer is at
    /// risk from, and unbounded because the digit loop is where that risk lives.
    #[kani::proof]
    #[kani::unwind(21)]
    fn the_unix_format_fits_any_i64() {
        let secs: i64 = kani::any();
        let mut w = Writer::new();
        w.signed(secs);
        let f = w.finish();
        assert!(f.len as usize <= 20); // "-9223372036854775808"
        assert!(f.len as usize <= FMT_CAP);
    }

    /// **The parser never panics on arbitrary bytes.** Every input byte is symbolic, at every length
    /// up to 26 (enough for a full `YYYY-MM-DDTHH:MM:SS.f+HH:MM` and more). It may return any
    /// `Error`; what it may not do is index out of bounds, overflow, or divide by zero.
    ///
    /// A `date -s` argument and, later, anything an NTP exchange hands over are bytes this program did
    /// not write, so totality here is a security property rather than tidiness. Nothing is assumed
    /// about the bytes at all: they need not be UTF-8, let alone ASCII, which is why the harness goes
    /// through [`DateTime::parse_rfc3339_bytes`].
    #[kani::proof]
    #[kani::unwind(28)]
    fn parse_is_total_on_hostile_bytes() {
        const N: usize = 26;
        let bytes: [u8; N] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= N);
        let _ = DateTime::parse_rfc3339_bytes(&bytes[..len]);
        // Not vacuous: some input in this set actually parses, so the harness is proving the
        // success path total and not merely that every early return returns.
        kani::cover!(DateTime::parse_rfc3339_bytes(&bytes[..len]).is_ok());
    }

    /// **Everything this crate prints, it can read back**, for every representable `DateTime`: every
    /// date in the ten-thousand-year range, at every minute of offset from -23:59 to +23:59.
    ///
    /// This is what makes RFC 3339 usable as the system's interchange form: a timestamp `date` wrote
    /// and `date -s` read is the same instant *and* the same offset (the equality is on the whole
    /// `DateTime`, so an offset silently normalised away would fail it).
    #[kani::proof]
    #[kani::unwind(28)]
    fn rfc3339_output_parses_back_to_itself() {
        let Some(dt) = any_datetime() else { return };
        let text = dt.format(Format::Rfc3339);
        assert_eq!(DateTime::parse_rfc3339_bytes(text.as_bytes()), Ok(dt));
        // Both output shapes are reachable: the 20-byte `Z` form and the 25-byte `±HH:MM` form. They
        // are different code on both sides, so a harness that only ever saw one would prove half.
        kani::cover!(dt.offset().minutes() == 0);
        kani::cover!(dt.offset().minutes() != 0);
    }
}
