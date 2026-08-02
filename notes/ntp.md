# NTP: the wire format, and the client that carries it

Two halves, built a day apart. **The wire format** is `crates/ntp_proto` (milestone 51 lane C): the
48 bytes of RFC 5905, the 1900-epoch fixed-point timestamp, the offset arithmetic, and the handful of
checks that are the whole of unauthenticated NTP's spoofing resistance. Pure computation, no socket,
no clock, no service, and its tests run in milliseconds on the host. **The client** is
`user/src/ntp.rs` (milestone 51 lane D), the process that turns those bytes into a clock correction,
and it is the second half of this file.

Milestone 51's other lanes own the RTC drivers and the clock service, and `date` plus the calendar
crate. The authority argument in full, that reading the clock is harmless and setting it is a real
capability, belongs with the service and is recorded in notes/clock.md.

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
`measured_boot` does for SHA-256. The bounded Kani harness stays as the in-gate regression guard on the low
corner where the truncation bug lives.

The general rule this is an instance of, and it is worth keeping: **a model checker is the tool for
domains too big to enumerate, not a better tool for domains that are not.** Check first whether you
can just try all of them.

## What is not in the crate

- No socket, no `net_stack` client, no retry scheduling, no server selection or clock filtering.
  Those belong to the component that carries these bytes, which is the rest of this file.
- No leap-second handling beyond passing the indicator through. A pending leap second is not a
  rejection: the server's clock is fine, the day is not 86400 seconds long, and interpreting that is
  the calendar's problem.
- No NTS, no MAC, as above.
- No `Duration`/`SystemTime` conversions. The crate is `no_std` and deals in `(u64, u32)`, so the
  clock service can hold whatever type it likes.

# The client (milestone 51 lane D)

`user/src/ntp.rs`. Five capability slots, and **the interesting one is the slot that is missing.**

```text
  entropy ──an endpoint──►┌──────────────┐──an endpoint──► net_stack ──► the network
  (8 random bytes)        │  ntp client  │ (the socket contract, UDP 123)
                          └──────┬───────┘
                                 │ an endpoint: PROPOSE
                         ┌───────▼────────┐
                         │ clock service  │──writes──► the clock page
                         └────────────────┘   (the client holds NO mapping of it,
                                               read-only or otherwise)
```

| slot | what it is | what it lets the client do |
|---|---|---|
| 0 | a report endpoint (WRITE) | say what happened |
| 1 | the socket contract's endpoint (WRITE) | send and receive datagrams |
| 2 | an untyped budget | mint and map one shared frame |
| 3 | the clock service's **propose** endpoint (WRITE) | **ask** for a time |
| 4 | the entropy service's endpoint (WRITE) | obtain eight random bytes |

There is no clock page here in either direction, so "set the time" is not an operation this process
can express. **A compromised NTP client can lie inside the service's bounds and can do nothing
else**: it cannot set the clock to 2038, it cannot move it backwards past a second, and it holds no
authority over anything but the socket it was given. Unix cannot make that claim, because `ntpd`
runs as root and `settimeofday` takes any value it is handed.

The claim is proved the way the machine proves things rather than argued.
`an_ntp_client_holds_no_writable_clock_page` spawns the **same binary with the same five slots**,
hands it the exact address at which a process holding the *set* authority maps the clock page
(`clock_service::CLOCK_VA`), and watches it write there and die of a fault, with the page's
generation unchanged on both sides of the attempt. The address is the one that would matter, so the
test cannot degenerate into poking at nothing; the boundary is the mapping, and knowing where to look
buys nothing.

## Reading the time without a mapping

The client needs T1 and T4 in the same timescale as the server's T2 and T3, and it holds no clock
page. So it uses `propose::STATE`, which the contract crate put there for exactly this caller: "a
proposer is exactly the process that may hold the endpoint and no mapping". One round trip, once, to
anchor `wall0` against the monotonic counter; everything after that is counter arithmetic, which is
ambient and costs one instruction.

The bootstrap case falls out rather than needing a branch. When the clock is `UNKNOWN` the service
answers 0, T1 and T4 are then measured from 1970, the offset comes out as the whole distance to the
server's clock, and the proposal lands on the server's time. That is the machine with no RTC, and it
is the case NTP exists for.

## The nonce comes from the entropy service, and refuses to come from anywhere else

`Query::with_nonce` is free hardening (above): a server never interprets the client's transmit
timestamp, so 64 random bits on the wire take an off-path attacker from about twelve bits of
guesswork to sixty-four. **That is worth exactly as much as the bits are unguessable.**

Until 2026-07-30 the only source in this tree was splitmix64 seeded off the virtual counter, which
notes/entropy.md calls predictable to anyone who can guess boot-relative time. The entropy service
(DECISIONS §44) landed that day, so the nonce is one eight-byte draw from it, one per attempt.

**A client with no entropy capability stops.** It reports `RPT_NO_ENTROPY` and exits *before it
touches the network at all*: no socket, no frame, no datagram. Falling back to the weak stream would
be §42's silent degradation in the one place where the entire value of the number is that nobody can
predict it, and it is the same call `SystemRng` makes when it panics rather than degrading. The two
failures stay distinguishable with no probe, because `entropy_proto::delivered` reads a byte count of
`0..=8` and every kernel `CALL` error is one of the small negatives: `NoSuchSlot` means "there is no
entropy service", 0 means "the service has none".

The test asserts what did *not* happen, which is the harder half: the server's report endpoint has no
waiting sender, so no request was ever built.

## One shot, because there is no timed wait

The syscall surface is `EXIT`, `YIELD`, `INVOKE`, `CAP_DELETE`. There is no sleep, no timeout and no
deadline anywhere in it, which is the milestone 51 block's open fork. So a poll interval is a
yield-spin: a thread that stays runnable for the whole interval and costs scheduler work in
proportion to it. At NTP's ordinary 64-second poll that is not a service anybody should ship.

So this is a **one-shot synchroniser**: up to three requests a couple of milliseconds apart, one
proposal, exit. That is not a workaround and it is not a stub; it is the honest shape available, and
**a long-running client is the timed-wait fork's to build.** Adding a sleep syscall to get one would
be settling that fork by accident, in the milestone least entitled to settle it.

**A kiss-o'-death is not retried**, and this is a property of the client rather than of the crate. A
rejected reply *is* retried, because it may have been a spoof that beat the real server back; stratum
0 is an instruction (`RATE` means back off, `DENY` means go away) and retrying into one is the
abusive behaviour the packet exists to stop. The test counts requests: three for a bad origin, one
for a kiss.

## The test server, and what it does and does not prove

The client's whole network authority is one endpoint capability, so the tests **substitute the peer
at that boundary**: a second role of the same binary holds `READ` on the endpoint the client holds
`WRITE` on, and speaks the same socket contract (`crates/socket_proto/src/lib.rs`, the same file `net_stack`
compiles) while being an NTP server on the other side of it. The client cannot tell, and **there is
no test-only branch anywhere in the client**. That is the shape a capability system makes available,
and it is why this is the honest choice rather than a compromise.

What it proves: the socket-contract glue (minting a frame, delegating it, the destination header,
`SENDTO`/`RECV` framing), that the 48 bytes are a well-formed NTPv4 client packet addressed to port
123, that the nonce is 64 unpredictable bits rather than a clock reading, that a reply failing
`Query::accept` moves nothing, that an accepted sample reaches the clock as a **proposal** (the page
publishes `SYNCED`, which nothing but the service can write), and that a proposal outside the policy
is refused **by the service** while the client behaves perfectly.

What it does not prove, stated plainly because a test server is exactly the kind of thing that gets
overclaimed:

- **Not that smoltcp, UDP, IPv4 and the NIC carry the bytes.** Milestone 30's socket-contract tests
  prove that path with a real datagram over the confined NIC, and this lane deliberately does not
  re-prove it.
- **Not anything about a real time server.** Nothing in QEMU's slirp answers UDP 123 (its built-in
  servers are TFTP, DHCP and a DNS NAT, and `guestfwd` is TCP-only), so there is no offline peer to
  point at; pointing the gate at a public server would make it depend on somebody else's network, the
  way the DNS check already has to be non-gating for exactly that reason.
- **Not that the client and the real `net_stack` agree**, beyond both compiling `netproto.rs`. The
  stub implements the server side of that contract, and a misreading of it would be a misreading on
  both sides at once. Compiling the same file rather than a copy is what keeps that risk to the
  semantics rather than the layout.

The honest summary: the client is proven against a server we wrote, over a network we wrote, and the
parts we did not write are proven elsewhere.

## What is proven, and where

Six kernel tests (`ntp_tests` in `kernel/src/user.rs`), not arch-gated: one portable binary, so
aarch64 and riscv64 run literally the same assertions. The suite went from 181 to 187 tests on
aarch64 and 152 to 158 on riscv64.

| test | what would have to be broken for it to pass anyway |
|---|---|
| `an_ntp_exchange_reaches_the_clock_as_a_proposal` | the request is NTPv4, mode 3, port 123; the correction arrives; the page says `SYNCED`, which only the service writes; the clock moved by about what the server claimed |
| `a_reply_that_fails_validation_never_becomes_a_proposal` | a bad origin, a kiss-o'-death and a 20-byte datagram each leave the page's generation untouched, and the kiss is not retried |
| `a_proposal_outside_the_policy_is_refused_by_the_service` | two hours forward and ten seconds back are both refused, by the service, with the client behaving perfectly |
| `an_ntp_client_holds_no_writable_clock_page` | the write faults, at that exact address on aarch64, and the process does not survive it |
| `the_nonce_on_the_wire_is_random_and_is_not_the_clock` | two exchanges differ, and neither transmit field is within an hour of the clock |
| `without_entropy_the_client_refuses_rather_than_guessing` | the refusal names `NoSuchSlot`, and no request was ever sent |

Every one of those assertions was **watched fail** before it was trusted: the write removed from the
probe, `with_nonce` swapped for the plain form, a proposal made despite a rejection, a fallback
inserted where the entropy refusal is, the kiss-o'-death `break` deleted, the offset dropped from the
correction, and the destination port hard-coded past the wiring. Seven mutations, seven failures,
each naming the right thing.

Two of them found a real defect in the *tests* rather than in the client, and it is worth recording
because it is the shape a test-server design invites: with the client mutated to carry on, an
unbounded `ipc_recv` on a report nobody would ever send became a sixty-second watchdog hang instead
of an assertion. Both waits are now bounded, so "the client never got there" fails in two seconds
with a sentence.

## What the client does not do

- **No server selection, no clock filter, no combining.** RFC 5905's selection and clustering
  algorithms exist because a real client polls several servers and weighs their samples. One server,
  one sample, one proposal is what a one-shot synchroniser can honestly do, and the machinery for
  more wants the poll loop that wants the timed wait.
- **No slewing.** Nothing here gradually corrects; a proposal is a step the service bounds. Slewing
  is a policy the *service* could adopt, and DECISIONS §43 records why it is a choice rather than a
  correctness requirement: `Instant` is never in the write path, so a step cannot perturb monotonic
  time.
- **No NTS and no MAC**, as the crate half of this file records at length.
- **No `init` endowment.** Nothing in the interactive boot runs the NTP client; the tests wire it.
  Ambient time synchronisation would be ambient authority, and which process may propose a time
  should be as visible as which process may reach the network.
