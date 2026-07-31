# NTP, the wire format (milestone 51 lane C)

The 48 bytes of RFC 5905, the 1900-epoch fixed-point timestamp, the offset arithmetic, and the
handful of checks that are the whole of unauthenticated NTP's spoofing resistance. All of it in
`crates/ntp_proto`, which is pure computation: no socket, no clock, no service, and its tests run in
milliseconds on the host.

Milestone 51's other two lanes own the RTC drivers and the clock service, and `date` plus the
calendar crate. This note is the protocol half. The authority argument, that reading the clock is
harmless and setting it is a real capability, belongs with the service and is recorded there.

## Why the protocol is a crate and not part of a component

The same reason `fs_proto` and `gfx_proto` are crates. A wire format is arithmetic and byte layout,
which is the cheapest thing in the system to get wrong and the most expensive to debug from inside a
QEMU boot against a live server. Here it is 21 host tests and 7 Kani harnesses, and the whole lot
runs in under a second with no emulator.

It also puts the boundary in the right place ahead of time. The eventual NTP client will hold a
network capability and a capability to *propose* a time. It will not hold the clock. Keeping the
protocol in a crate with no I/O means that client cannot accidentally grow the ability to set
anything, because the code that knows what a time is has nothing to set.

## Scope: unauthenticated NTPv4, and what that is worth

**The crate implements plain NTPv4. It implements neither NTS (RFC 8915) nor the RFC 5905
symmetric-key MAC.** That is a decision, so here is the honest accounting.

What it buys: correct time from a reachable server on a path where nobody is injecting packets,
which is the ordinary case and is why plain NTP is what most machines still run.

What it does not buy: anything at all against an attacker who can see the request and beat the real
server's reply back. Everything between this crate and that attacker is the check list below plus an
unpredictable transmit timestamp. An **off-path** attacker, who cannot see the request, has to guess
a 64-bit nonce. An **on-path** attacker reads it and passes every check we make. Plain NTP has no
answer to that, and the crate says so in its own documentation rather than leaving it to be
discovered.

The consequence for the system is that an NTP-derived time is untrusted input, and milestone 51's
design already treats it that way: the client proposes, the service applies bounds, and a compromised
client can lie only inside those bounds and can do nothing else.

### Why NTS is a separate decision and not a stretch goal

NTS-KE is TLS 1.3. TLS needs certificate validation. Certificate validation needs a roughly correct
clock, which is the thing being obtained. The standard escape is a build-time "not before" timestamp
plus whatever the RTC says, and that is a real design choice with real consequences (a machine whose
image is older than its certificates fails to boot into a usable state), not a detail to settle
mid-implementation. The roadmap records it as a fork. What the crate deliberately does **not** do is
half of it: an extension-field parser with no cryptography behind it would put the letters NTS in the
tree while authenticating nothing, which is worse than the honest absence.

## The 2036 problem, and the pivot we chose

An NTP timestamp is 64 bits: 32 of seconds since **1 January 1900**, 32 of binary fraction. Two
things follow that catch people out.

**The epoch is not Unix's.** They differ by 2,208,988,800 seconds, which is 25,567 days: 70 years of
365 plus the 17 leap days from 1904 to 1968. 1900 is not a leap year (divisible by 100 and not by
400), and assuming it is puts the constant one day out.

**The seconds field wraps**, on 7 February 2036 at 06:28:16 UTC. 32 bits of seconds is 136 years, and
the field alone cannot say which 136 years it means. Something has to decide, and the choice is
visible in the decoded output of every timestamp the machine ever handles.

We take RFC 5905 §6's convention as a **fixed pivot**:

| seconds field | era | covers |
|---|---|---|
| high bit set (≥ 2^31) | era 0, counted from 1900 | 1968-01-20 to 2036-02-07 |
| high bit clear | era 1, counted from the 2036 rollover | 2036-02-07 to 2104-02-26 |

The alternative is real and is what several implementations do: pick the era that puts the timestamp
nearest to the time you already believe it is. It is more flexible and it is strictly worse here, for
three reasons.

1. **It makes decoding depend on the clock.** The same bytes parse to different instants depending on
   when you ask, which is a property no parser should have.
2. **It makes the function untestable** without injecting a "now", and therefore unprovable: Kani
   quantifies over inputs, and a hidden input is not one.
3. **It is wrong exactly when it matters.** This machine boots believing it is January 1970. A
   nearest-era heuristic on a machine whose clock is wrong picks the wrong era with total confidence,
   and the entire reason this crate exists is that the clock is wrong.

So the pivot is a pure function of its input, provable, and wrong only after 2104. The crate
therefore has a **documented expiry date**, which is better than the undocumented one every
implementation has.

The representable window in Unix seconds is `0 .. 4_233_462_144` (the epoch to 2104-02-26 09:42:24
UTC), clipped below at 1970 because the crate's Unix seconds are unsigned. `Timestamp::from_unix`
**refuses** anything outside it rather than wrapping, so the failure mode is `None` instead of a date
136 years off.

One more piece of the same problem, and it is the one that is easy to miss: **the offset and delay
arithmetic is modular, not absolute.** Differences are taken as `wrapping_sub` on the raw 64-bit
values and read back as signed, which is what makes an exchange straddling the 2036 boundary come out
as three seconds instead of minus 136 years. There is a test that does exactly that.

## The checks, which are the security

`Query::accept` is the only function in the crate that can reject anything. `Packet::parse` is total
on 48 bytes: it decodes and judges nothing. That split is deliberate, and it is what lets the
parse/serialise round trip be proved over arbitrary bytes while every judgement stays readable in one
place.

In order:

1. **Exactly 48 bytes.** Longer means extension fields or a MAC, and since we implement neither,
   accepting one would mean silently ignoring authentication data a server computed. Fail closed.
2. **Version 4, mode 4 (server).** Mode is what keeps an unsolicited broadcast out: nobody asked for
   it, so nobody should believe it.
3. **The origin timestamp equals the nonce we sent.** The load-bearing one. Checked before anything
   in the packet is believed, because it is the check that says the packet is a reply to *us*. It
   also rejects a stale reply to an earlier request.
4. **Stratum 0 is a kiss-o'-death**, reported as itself rather than as a generic failure: `RATE`
   means back off and `DENY` means go away, and a client that retries on those is the abusive client
   the packet exists to stop. Stratum above 15 is not a time source. A leap indicator of 3 is the
   server saying the same thing.
5. **The transmit timestamp is not zero**, which on the wire means "I do not know what time it is".
6. **The four timestamps agree with causality**: the server did not answer before it was asked, we
   did not receive before we sent, and the round trip is not shorter than the server's own
   turnaround. Three distinct rejections, because the third is possible while both halves of the
   first two are fine.
7. **The claimed root distance is inside 16 seconds** (RFC 5905's `MAXDISP`), which is deliberately
   far looser than the RFC's 1 s selection threshold. The split is whose job it is: **the crate
   rejects the impossible, the service applies policy.** A server 200 ms away over a bad path is a
   poor sample, and what to do with a poor sample is a decision made with the other samples in view.

### The nonce, and the free hardening

In plain NTP the client's transmit timestamp is echoed back in the origin field, so it is the only
thing an off-path attacker has to guess. RFC 5905 says to randomise its low-order bits, and how many
bits are actually random is set by the clock's precision: a microsecond-resolution clock leaves about
12, which is 4096 guesses. `Timestamp::randomise_low` does that and takes the bit count as a
parameter, because the caller knows its precision and the crate does not.

`Query::with_nonce` does better, and the reason it is free is worth stating: **a server never
interprets the client's transmit timestamp.** It copies it into the origin field and does nothing
else. So the value on the wire need not be a time at all. Send 64 random bits, keep the true send
time locally for the arithmetic, and the attacker's guess goes from a dozen bits to sixty-four. This
is what chrony does by default and it costs one extra field.

## What is proved, and one place where a solver was the wrong tool

Seven Kani harnesses, run by `script/verify`:

| harness | what it quantifies over |
|---|---|
| `the_era_pivot_is_exact_over_the_window` | every Unix second from 1970 to 2104, all 4.2 billion: encode then decode is the identity, across both eras and the boundary |
| `nanoseconds_survive_the_fixed_point` | nanoseconds under 2^16 (see below) |
| `decoding_any_wire_value_is_total` | all 2^64 timestamp bit patterns: no panic, and the nanosecond returned is always a valid sub-second |
| `out_of_range_is_refused` | every input to `from_unix`: `Some` exactly inside the window, never an aliased answer |
| `parse_then_serialise_is_the_identity` | all 2^384 48-byte packets |
| `an_unmatched_origin_is_always_rejected` | all 2^384 packets: no combination of the other 40 bytes gets one past the nonce check |
| `accepting_is_total_and_a_sample_is_coherent` | all 2^384 packets and any three timestamps: `accept` never panics or overflows, and an accepted sample has a non-negative delay, a stratum in 1..=15, mode 4, and a non-zero transmit |

The three 2^384 harnesses are the ones a model checker exists for: the domain is unenumerable and the
code is fed by the network.

**The nanosecond round trip is the exception, and it is the interesting finding.** The property is
`ticks_to_nanos(nanos_to_ticks(n)) == n`, and it is real: a tick is 2^-32 s, about 233 ps, so
nanoseconds fit inside ticks, but only if both directions round to nearest. Truncate either way and
1 ns becomes 4 ticks becomes 0 ns. Kani is bad at it, because a bit-blasting model checker is bad at
multiplication and this is two of them composed. Measured on an M-series laptop with kissat:

| bound on `n` | solver time |
|---|---|
| 2^16 | 9 s |
| 2^20 | 212 s |
| 2^30 (the real range) | killed at 10 minutes, twice |

About four times the work per bit, so the full range is out of reach by roughly five orders of
magnitude. Restating the division by its defining inequality (`t*d <= x < (t+1)*d`, which replaces a
division circuit with a shift-and-add one) did not help, which is what identified the multiplication
rather than the division as the cost.

The answer was not a cleverer harness. **The domain is 10^9 values, and running the code on every one
of them takes 0.6 s.** So the test does that: `every_nanosecond_survives_the_round_trip` is
exhaustive, which is a *complete* verification of the function, strictly stronger than any bounded
solver result. The crate gets `opt-level = 2` in the dev profile to keep it that quick, the same way
`measure` does for SHA-256. The bounded Kani harness stays as the in-gate regression guard on the low
corner where the truncation bug lives.

The general rule this is an instance of, and it is worth keeping: **a model checker is the tool for
domains too big to enumerate, not a better tool for domains that are not.** Check first whether you
can just try all of them.

## What is not here

- No socket, no `netstack` client, no retry or poll scheduling, no server selection or clock
  filtering. Those belong to the component that will carry these bytes.
- No leap-second handling beyond passing the indicator through. A pending leap second is not a
  rejection: the server's clock is fine, the day is not 86400 seconds long, and interpreting that is
  the calendar's problem.
- No NTS, no MAC, as above.
- No `Duration`/`SystemTime` conversions. The crate is `no_std` and deals in `(u64, u32)`, so the
  clock service can hold whatever type it likes.
