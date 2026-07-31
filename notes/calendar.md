# The calendar crate: seconds to a date, and back

`crates/calendar` is milestone 51's pure-computation lane. It converts a Unix timestamp to a civil
date and back, says which day of the week it is, prints five formats, and reads RFC 3339. It holds no
capability, performs no IO, allocates nothing, and does not know the clock service exists. Everything
here runs on the host in milliseconds and is machine-checked by Kani.

The roadmap's milestone 51 block set the split: "Timezone and calendar conversion are pure
computation and belong in a host-tested library crate, not in the service." This is that crate. The
service that owns the offset, the RTC drivers, the NTP client that may only *propose* a time, and the
`date` command are other lanes; each of them will depend on this one and none of them appears in it.

## Why a calendar is worth writing down at all

It looks like a solved problem and it is a famous source of bugs, because three separate things are
each slightly harder than they look.

**The leap-year rule has three clauses and most people remember one.** Every fourth year, except
every hundredth, except every four-hundredth. 1900 is not a leap year; 2000 is. Code written from
"divisible by four" gets both wrong, and code written from "except centuries" gets 2000 wrong. Both
are in the test table, alongside the dates either side of them, because the failure is not that the
predicate returns the wrong boolean, it is that a whole day's worth of dates shift.

**Negative timestamps are where truncating division lies to you.** In Rust `-1 / 86400` is `0`, so a
converter that divides seconds by a day to get a day number reports 1970-01-01 for every instant in
the last day of 1969. The fix is `div_euclid`/`rem_euclid`, and the test that catches it is a
timestamp of `-1`. A date library that only tests dates after the epoch will not find this.

**The month lengths have no formula, unless you move the start of the year.** The algorithms here are
Howard Hinnant's (the same pair inside libc++ and most serious date libraries), and the trick is to
treat March as month zero. Then February, the irregular one, is the last month of the year, the leap
day is appended rather than inserted, and every month length collapses into `(153*m + 2)/5`. The
400-year era is the period of the whole Gregorian rule and contains exactly 146,097 days, so the
century corrections become plain division. Both directions are closed-form: no loops, no tables.

That last property is what makes this an unusually good verification target. A round trip through
both algorithms is one algebra problem, not 315 billion executions.

## The API

| | |
|---|---|
| `Civil` | a date and a time with no zone: "2026-07-30 at 12:34:56". Fields are private and the constructor validates, so **a `Civil` that exists is a real date** and `to_unix` is infallible. There is no February 30 in this type. |
| `UtcOffset` | a fixed number of minutes from UTC. Not a time zone; see the scope note below. |
| `DateTime` | a `Civil` plus the `UtcOffset` it was read at, which together name one instant. This is what an RFC 3339 string is. |
| `Weekday` | seven variants, `abbrev()`, `name()`, `iso_number()` (Monday is 1). |
| `Format` | five renderings, below. |
| `Formatted` | the output, in a fixed 32-byte buffer. No allocator, no borrow of the value it came from. |
| `Error` | one enum for conversion and parsing both, with `as_str()` for a shell with no allocator. |

The validating-constructor choice is the same discipline the capability types use: make the invalid
state unrepresentable at the boundary, and the arithmetic downstream is total rather than defensive.
It is why `Civil::to_unix` returns `i64` and not `Result<i64, _>`, and why the Kani harnesses can
assert an equality rather than an implication.

### The range: years 0000 through 9999

`MIN_TIMESTAMP` is 0000-01-01T00:00:00Z and `MAX_TIMESTAMP` is 9999-12-31T23:59:59Z. The bound comes
from RFC 3339, whose `date-fullyear` is exactly four digits: a year outside it cannot be *written* in
the interchange format the crate parses and prints, so admitting it would create values that can be
computed and not spoken. Outside the range is `Error::OutOfRange`, never a wrapped or saturated date.
Silent degradation is what DECISIONS §42 forbids for filesystems and it is no better here.

The calendar is **proleptic Gregorian**: the Gregorian rules projected backwards past the 1582 reform
rather than switching to Julian. Every library that speaks Unix time does this, because a Unix
timestamp is a count of seconds and has no idea a reform happened. Dates before October 1582 are
therefore not what a historian would write, and year 0 exists here (a leap year) where historians
write 1 BC. Convention, stated rather than discovered.

### Year 2038 is a non-event, and the test says so rather than the comment

Every timestamp here is `i64`. `i32::MAX` seconds is 2038-01-19T03:14:07Z, the last instant a 32-bit
`time_t` can name, and the next second is ordinary. The test asserts both, and a date a century
further on, because "we are 64-bit so it is fine" is exactly the kind of claim that is true right up
until someone stores a timestamp in a `u32` field.

### Leap seconds have no timestamp, so parsing one is an error with its own name

Unix time is defined so every day is exactly 86,400 seconds. A leap second is not representable; it
is smeared or repeated by whatever sets the clock. RFC 3339 can *write* `23:59:60`, so the parser
meets one eventually, and it returns `Error::LeapSecond` rather than folding it onto `:59` or `:00`.
A caller who wants the clamp can apply it, having been told. Quietly mapping it would be a lie about
which second, in a crate whose entire job is to say which second.

## Time zones: the scope note, stated plainly

**A fixed UTC offset is in scope. The IANA time zone database is not, and not merely "not yet".**

`UtcOffset` is minutes from UTC, ±23:59 (RFC 3339's grammar, not geography). It is enough to print a
timestamp the way a local user expects and enough to read one someone else printed, and it is what the
wire format actually carries.

What tzdata would add is a different kind of thing. "America/Los_Angeles" is not an offset; it is a
function from instants to offsets with a century of political history in it, and hardcoding "-08:00"
is wrong for a third of the year. The database is ~450 KB of compiled rules that change several times
a year, which makes it a **data distribution problem**: who ships it, who updates it, which capability
lets a program read it, what happens when it is stale. Those are good questions for this OS and none
of them is a calendar question. So there are no zone names, no DST, no `TZ`. A program that wants
local time is handed an offset by whatever knows one. If real zones are ever wanted, they are a
separate crate with a file behind it, not a growth of this one.

## The five formats, and why not `strftime`

| `Format` | Example | Who wants it |
|---|---|---|
| `Rfc3339` | `2026-07-30T12:34:56Z` | interchange: logs, NTP, anything another system reads. Round-trips through the parser. Zero offset prints `Z`. |
| `Date` | `2026-07-30` | a directory listing, a filename |
| `Time` | `12:34:56` | a log line that already knows the day |
| `Human` | `Thu 2026-07-30 12:34:56 UTC` | what `date` with no arguments should print |
| `Unix` | `1785414896` | arithmetic, and handing the number back to the clock service |

**Why not a format-string interpreter.** `strftime` is a second parser: its errors appear at runtime,
in a `no_std` program with no allocator, over a string no compiler checked, in exchange for
combinations nothing in this system asks for. `%c` alone drags in locales. Five constructors cover
every consumer milestone 51 has, and adding a sixth is a match arm and a test, which is a cheaper way
to be wrong than a grammar is.

**Why `Human` is not `date`'s traditional output.** `Wed Jul 30 12:34:56 UTC 2026` does not sort, and
it needs a month-name table, which is the first step toward the locale question this crate has no
business answering. Keeping the ISO date and prefixing the weekday gives a human the one thing ISO
does not (which day of the week it is) and stays sortable. The seven weekday abbreviations are the
crate's entire natural-language surface.

Output goes into a fixed 32-byte buffer, sized two bytes over the longest possible rendering
(`Fri 9999-12-31 23:59:59 +23:59`, 30 bytes). `Formatted::as_str` has no panic path: it is
`from_utf8(...).unwrap_or("")`, and the `every_format_is_ascii` harness proves the fallback is
unreachable.

## Parsing: RFC 3339, strictly, with three sanctioned relaxations

```text
YYYY-MM-DDTHH:MM:SS [.fraction] (Z or ±HH:MM)
```

Accepted beyond the strict letter, each because RFC 3339 itself says so:

- **lowercase `t` and `z`** (§5.6 permits them);
- **a space in place of `T`** (§5.6's NOTE permits it by agreement, and it is what a person types at
  a prompt);
- **fractional seconds, parsed and discarded.** This clock has one-second resolution. Keeping them
  would mean lying about precision or growing the type; refusing them would reject ordinary
  timestamps other systems emit.

Everything else is strict: four year digits and two of everything else, no unpadded fields, **no
missing offset** (which is the whole difference between RFC 3339 and bare ISO 8601), no trailing
bytes. `Error::Syntax` for malformed text, and a specific error for text that is well-formed and
unrepresentable (`LeapSecond`) or out of range (`BadDay`, `BadOffset`, ...), so a `date -s` verb can
say what it did not like.

The parser is fed text its caller did not write: a `date -s` argument, and eventually an NTP-adjacent
exchange. Totality on hostile bytes is therefore a security property rather than tidiness, and it is
one of the harnesses.

**`parse_rfc3339_bytes` is the real entry point and `parse_rfc3339(&str)` is its wrapper**, which is
the opposite of the usual arrangement and was decided by the proof. RFC 3339 is ASCII, so bytes are
what the grammar is defined on; a caller holding a network buffer would otherwise have to validate
UTF-8 first, for a function that rejects every non-ASCII byte anyway. And the totality harness can
then quantify over **arbitrary bytes**, including sequences that are not UTF-8 at all, which is
exactly what a network client will hand it. It also happened to take the harness from over ten
minutes to seventeen seconds; see notes/verification.md.

## What is proved, and the bounds

Eleven harnesses, all `SUCCESSFUL`, about seven minutes. See notes/verification.md for the table and
for the finding that came out of building them, which is worth more than the harnesses themselves:
**the calendar arithmetic is cheap to prove and a 64-bit division by 86,400 is not**, and iterating a
slice whose length is symbolic costs more than the parser wrapped around it.

Ten of the eleven run over the **full supported range**, unbounded below the type: the two Hinnant
algorithms are mutual inverses for every one of the 3,652,425 days, every day number decodes to a
date that exists, the calendar never steps backwards, day-of-year is 366 exactly on 31 December of a
leap year, the parser is total on arbitrary bytes, and everything the crate prints it reads back for
every representable date at every legal offset. The eleventh, the seconds round trip, is bounded to a
four-year window straddling the epoch, because that one division is what bounded model checking here
cannot swallow whole; the window is chosen so it contains the bug it exists to catch (truncating
division, which only shows up on a negative timestamp).

Both load-bearing properties were **falsified before being believed**: reducing the leap-year rule to
"divisible by four" fails the round trip in 8 seconds, and replacing `div_euclid` with `/` fails the
seconds harness in 32.

The unit tests are the complement, not a duplicate: they pin the cases that historically break date
code (1900 and 2000, every month-end rollover, the epoch, `-1`, 2038) against values cross-checked
with Python's `datetime` rather than against this crate's own output. A proof says the algorithms are
mutual inverses; only an independent witness says they are inverses of the *right* function.

## What it deliberately does not do

- **No durations or calendar arithmetic.** "One month after January 31" has no single correct answer,
  and the caller that wants it does not exist yet.
- **No week numbers.** ISO 8601 week-of-year has its own edge cases (a January date can be in week 52
  of the previous year) and nothing asks for it.
- **No sub-second resolution.** The clock this will read is a one-second RTC plus an offset.
- **No `no_std` gymnastics for a `Display` on `Civil`.** Formatting is explicit through `Format`, so
  a caller never accidentally prints a shape the parser cannot read back.
