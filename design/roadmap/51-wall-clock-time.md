# 51. Wall-clock time, the `date` command, and an NTP service

**Status: PARTIAL.**

**Lane A built 2026-07-30** (the two RTC drivers and the clock service; DECISIONS §43,
notes/clock.md). The machine knows what time it is, on both ISAs, and `SystemTime` is real. What the
build settled that this block left open, and one place it went somewhere the block did not predict:

- **The three authorities are three different objects, and only one is a message.** Reading is a
  **read-only mapping of the clock page** (two loads and an add, no syscall, no server); setting is
  the **same page mapped writable**; proposing is the endpoint. Nothing new in the syscall surface.
  The block imagined all three as capabilities without saying what kind; a process has one blocking
  wait point, so two message-borne authorities would have needed two servers.
- **Discovery is by `compatible`, not by node name**, because the aarch64 board calls the node
  `pl031@9010000` and the RISC-V one calls its RTC `rtc@101000`. `dtb::node_reg_compatible` is new
  for it, and the kernel passes the *binding* to the driver so the register layout comes from the
  machine rather than from `target_arch` (the VisionFive 2 is riscv64 with neither device).
- **The unknown state is the default**, since a zeroed page reads as `UNKNOWN`. Its one uncomfortable
  consequence: `SystemTime::now()` has no error channel, so an unknown clock is a **panic**, and std
  has no way for a program to ask before it asks. Recorded in DECISIONS §43 as a limit, not a win.
- **Still open:** nothing in this milestone. The timed-wait fork below is recorded here but is a
  kernel-surface decision of its own, tracked separately and not a milestone-51 deliverable.

**In brief.** The machine does not know what time it is, and says so in a way that is easy to miss:
`SystemTime` is the monotonic counter offset from `UNIX_EPOCH`, so **it reports January 1970 plus
uptime**. Give it a real clock, a `date` command, and a network time client, and take the chance to
put the authority in the right place.

**Why it matters.** Time is where a capability system gets to make a distinction Unix cannot afford
to. **Reading the clock is near-harmless; setting it is a genuine authority** over certificate
validation, log ordering, filesystem timestamps and build reproducibility. In Unix `ntpd` runs as
root and may set the clock to anything. Here the network client should not be able to set it at all.

## The starting position, which is honest but wrong

`notes/std.md` already records the caveat: "`SystemTime` is monotonic-since-boot, not wall-clock. No
RTC, no NTP, so 'system time' honestly measures 'since this machine came up'." Differencing two
`SystemTime`s gives a correct duration; any absolute reading is a fiction. It is also why file times
are `Unsupported` in the `std` PAL, per that file: "there is no wall clock to interpret it against
anyway". Milestone 47's `mkdir`/`create` work will want them, so this unblocks that.

## The time source, and it is two drivers because parity is a gate

Verified from the DTB fixtures in `crates/dtb/tests/fixtures/`, not assumed:

| Platform | Device | Address |
|---|---|---|
| QEMU `virt`, aarch64 | `arm,pl031` | `0x9010000` |
| QEMU `virt`, riscv64 | `google,goldfish-rtc` | `0x101000` |
| VisionFive 2 | its own RTC (board bring-up, milestone 16a) | via DTB |

Two small drivers, both discovered through `crates/dtb` rather than hardcoded, both following rule 2
(a driver takes a base address and knows nothing else). Neither is large; the point is that shipping
one and not the other is the bug rule 5 exists to catch.

## The design: an offset, which makes NTP safe for free

Keep the split the code already has. `Instant` stays the **raw monotonic counter**, ambient and
one instruction (see the §10 exception recorded in `arch/aarch64/timer.rs` and its riscv twin).
Wall-clock time becomes **counter + offset**, and the clock service owns the offset.

The payoff is that adjusting the wall clock cannot perturb monotonic time, **by construction rather
than by discipline**. Unix needs `adjtime` slewing partly because stepping the clock backwards breaks
things that assumed it only moves forward; here `Instant` never sees the adjustment, so a step is
just an offset write. Whether to *also* slew for the benefit of wall-clock readers is then a policy
choice the service can make, not a correctness requirement.

## Where the authority sits

- **The clock service** holds the RTC device capability and the offset. It is the only thing that can
  set the time.
- **Readers** hold a read capability. Nearly everything.
- **The NTP client** holds a network capability and a capability to **propose** a time, which is
  deliberately not the same as setting one. The service applies policy: sanity bounds, a maximum
  step, and a refusal to move backwards past a threshold.

That attenuation is the milestone's demonstrable claim, and it is one Unix cannot make: a compromised
NTP client here can lie *within the service's bounds* and can do nothing else. It cannot set the clock
to 2038, and it holds no authority over anything but the network socket it was given.

## `date`, the deliverable

**Built 2026-07-31** (notes/date.md; `user/src/date.rs`, `kernel::user::date_tests`).

**Reachable from a test, not from the prompt, and that is why this milestone is `PARTIAL` and not
`BUILT`** (found by Chris, 2026-07-31, by typing `date` at `script/server` and getting "unknown
command"). The binary is in the initrd and tested on both ISAs, but `grant_plan::Prog` knows only `worker`,
`budgeter`, `heeder` and `spinner`, so the shell cannot spawn it. The lane deferred that as
"milestone 31's manifest machinery", which is a defensible scope call that nonetheless leaves the
*command* half of "the `date` command" undone. **A program a user cannot invoke is not a command**,
and the status said otherwise until he checked. Being folded into the milestone 47 grammar lane, which
owns `grant_plan` and is removing `run`: after which `date` at the prompt is exactly what he typed. A hundred
lines, most of them comments, because the design had settled everything interesting first: read the
page, add the counter, hand the number to `calendar`. Five formats, a fixed UTC offset in minutes,
and an optional second line naming the clock's **provenance**, which renders `clock_proto`'s four
states for a person and is a distinction no Unix `date` can print. Three things the build is worth
recording for:

- **The absence of `date -s` is a fact about the wiring, not a missing flag.** It holds a read-only
  mapping, so there is no argument it could take and no method it could call. That is the claim
  below made concrete rather than asserted.
- **The unknown clock is a sentence**, with the two causes told apart (`the machine has no clock it
  believes` / `this process holds no clock capability`), because `date` has an error channel where
  `SystemTime::now()` has only a panic. Falsified before it was believed: removing the state check
  makes it print `Thu 1970-01-01 00:00:04 UTC`, and the test catches exactly that string.
- **It closes DECISIONS §43's own scope note** that "the unknown-clock path is not proven in the
  guest". That reasoning was about the machine (both QEMU boards have a working RTC) and the thing
  under test is the page: a frame nobody has published to *is* that machine, as far as any reader
  can tell, so the test allocates one and grants it.

Reads the wall clock and formats it. Timezone and calendar conversion are pure computation and belong
in a host-tested library crate, not in the service (§14's rule about what compiles for the host).
Setting the time is a separate verb with a separate capability, and `date -s` in one binary that does
both is exactly the conflation this design refuses.

**The library half is built** (2026-07-30): `crates/calendar`, host-tested and Kani-proved, holds the
civil-date arithmetic, the weekday and day-of-year, five formats and an RFC 3339 parser, and depends
on nothing in this milestone. Eleven harnesses, ten of them over the full ten-thousand-year range.
Two scope calls are recorded in notes/calendar.md rather than left ambiguous: **a fixed UTC offset is
in and the IANA tzdata is out** (zone rules are a data-distribution problem, not a calendar one), and
there is no `strftime`, five named formats instead. The command itself still waits on the service. The
lane also produced a verification finding worth more than the crate: a 64-bit division by 86,400 is
what bounded model checking chokes on here, not the calendar, and the `&str` boundary costs more than
the parser behind it (notes/verification.md).

## NTP, and the chicken-and-egg worth recording before it is discovered

Buildable today: `net_stack` runs smoltcp, and NTP is UDP on port 123. Two honest problems:

1. **Plain NTP is unauthenticated and trivially spoofable.** NTS (RFC 8915) is the answer, and it
   needs TLS, which needs certificate validation, which needs **a roughly correct clock**. The
   standard escape is a build-time "not before" timestamp plus the RTC's rough value, and it should
   be chosen deliberately rather than discovered halfway through.
2. **The RTC may be wrong or absent**, so the service needs a defined state for "I do not know what
   time it is" rather than confidently reporting 1970. That state should be visible to readers, not
   papered over, which is the same rule §42 sets for filesystems: no silent degradation.

**The wire format is built** (`crates/ntp_proto`, notes/ntp.md): the 48-byte NTPv4 packet, the
1900-epoch fixed-point timestamp with a fixed era pivot for the 2036 rollover, the offset and delay
arithmetic in modular form so an exchange across that rollover comes out right, and the seven
response checks that are the whole of plain NTP's spoofing resistance. Host-tested and Kani-proved
(the era pivot over all 4.2 billion seconds from 1970 to 2104; parse/serialise and the origin-nonce
check over all 2^384 packets). Problem 1 above is **recorded, not solved**: the crate is
unauthenticated NTPv4 and says so in its own documentation, NTS stays a separate decision, and the
crate does not implement half of it.

**The client is built** (2026-07-31, `user/src/ntp.rs`, notes/ntp.md). It holds **five capability
slots and none of them is the clock page**, so the block's claim above is now a fact the machine
enforces rather than a design intention: `an_ntp_client_holds_no_writable_clock_page` gives the same
binary the same five slots plus the exact address a *setter* maps the page at, and it faults. Four
things the build settled or found:

- **`propose::STATE` is how a client with no mapping reads the time**, which is what the contract
  crate put it there for. One round trip to anchor against the monotonic counter, and the
  unknown-clock bootstrap falls out with no branch: the service answers 0, T1 and T4 are measured
  from 1970, and the proposal lands on the server's time.
- **The nonce is one draw from the entropy service, and its absence is a refusal.** No capability
  means no request at all, not a fallback to the counter-seeded stream, because
  `Query::with_nonce`'s 64 bits are worth nothing if they are guessable (§42's rule, §44's source).
- **A kiss-o'-death is not retried** while an ordinary rejection is, which is a property of the
  client rather than of the crate, and the test counts requests to prove it.
- **It is a one-shot synchroniser, not a continuously polling service,** because the timed-wait fork
  below is unsettled. A
  poll interval is a yield-spin; adding a sleep syscall to get a real one would settle that fork by
  accident. Three attempts a couple of milliseconds apart, one proposal, exit.

The test server is a second role of the same binary holding `READ` on the endpoint the client holds
`WRITE` on, so the client's network path is substituted **at the capability boundary** and its code
has no test-only branch. What that leaves unproven is recorded rather than glossed: smoltcp, UDP and
the NIC are milestone 30's to prove, and nothing in slirp answers UDP 123, so there is no offline
real server to point a gate at.

## The fork this exposes, which is bigger than the milestone

There is **no timed wait anywhere in the kernel**. The syscall surface is `EXIT`, `YIELD`, `INVOKE`,
`CAP_DELETE`, and `sched.rs` twice calls out its own "no-timeout limitation". So `thread::sleep` is a
yield-spin, which is the *correct* implementation given what exists (it does not monopolise a core),
but it keeps a thread runnable for the whole sleep and costs scheduler work proportional to duration.

Three candidate shapes, and this is a design fork to settle before building:

- **A new `SYS_SLEEP` syscall.** Simplest, ambient, and not capability-shaped, which is a strike.
- **A timer object with a `WAIT` method.** Capability-shaped and consistent with the model; the most
  machinery.
- **A deadline on `Endpoint::RECV`/`CALL`.** One primitive that fixes sleep, the RECV no-timeout
  limitation the kernel already complains about twice, and the shell's `^C` busy-poll that
  `linedisc`'s `OP_INTRCOUNT` doc describes as waiting for "the blocking notification primitive".
  **Three problems, one addition**, which is why it looks strongest.

Worth separating clearly: *reading* time is ambient and harmless, *blocking* on time is a scheduler
interaction and is the part that wants a capability.

**Sequencing.** The RTC drivers and the clock service are independent of the shell milestones and
could start any time; `date` follows the service; NTP follows `date` and wants the network stack
settled. The timed-wait fork is separable and should be decided on its own, since it serves more than
this milestone. **Effort: 3 lanes estimated** (drivers plus service, `date` plus the calendar crate,
NTP), noting estimates for unbuilt work are guesses on a history-calibrated scale.
