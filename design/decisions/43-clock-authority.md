# 43. Reading the clock is a page, setting it is a page you may write, proposing is an endpoint

**Built 2026-07-30** (milestone 51 lane A: the two RTC drivers and the clock service). Concept note:
notes/clock.md. The contract is `crates/clock_proto`.

Before this, `SystemTime` was the monotonic counter offset from `UNIX_EPOCH`, so the machine
reported **January 1970 plus uptime** and nothing in the interface said so. The defect was never the
missing hardware; it was that a caller could not tell a wrong answer from a right one, which is
[§42](42-truthful-filesystem.md)'s
rule on a second axis.

## Wall clock is counter plus offset, and that is what protects `Instant`

`Instant` stays the **raw monotonic counter**: ambient, one instruction, readable at EL0 because the
kernel opened `CNTKCTL_EL1.EL0VCTEN` (and `scounteren.TM` on RISC-V). Wall-clock time is
`counter + offset`, and the offset is the only thing anyone ever writes.

The payoff is that **adjusting the wall clock cannot perturb monotonic time, by construction rather
than by discipline.** Unix reaches for `adjtime` slewing partly because stepping the clock backwards
breaks code that assumed it only moved forward; here the counter is not in the write path at all, so
a step is an offset write and `Instant` never sees it. Slewing for wall-clock readers' benefit
becomes a policy the service *may* adopt rather than a correctness requirement. A test asserts the
property directly (`adjusting_the_wall_clock_leaves_the_monotonic_counter_alone`).

## Three authorities, three different objects, and only one of them is a message

- **Read** is a **read-only mapping of the clock page**. No endpoint, no syscall, no round trip: two
  loads and an add. This is what "reading the clock is near-harmless" looks like when it is
  expressed as a capability rather than asserted in prose.
- **Set** is a **read/write mapping of the same page**. Writing the offset *is* setting the clock,
  and nothing polices it, which is what makes it the authority.
- **Propose** is an **endpoint** the clock service serves. A proposer holds no writable page, so the
  only thing it can do is ask, and `clock_proto::policy::decide` answers.

So the ladder is the kernel's own and needed nothing new: no capability, `Frame` with `READ`,
`Frame` with `WRITE`, `Endpoint` with `WRITE`. **No new syscall, no new method number, no new object
type**, and the authority a process holds is already introspectable, which is what `caps` prints.

**Why set is memory and not a message, which is the one judgement call here.** A process has exactly
one blocking wait point: this kernel has no wait-any primitive and no threads sharing an address
space (the constraint `compositor.rs` records, and the same one §26.5 declines to lift). So a design
where `set` and `propose` were both messages would have needed two server processes, and the second
would have held full set authority anyway, moving the problem rather than solving it. Making `set` a
page write is not a workaround for that: **"set the offset outright" already means writing the
offset**, and §33 settled the same shape for the compositor, whose authority is memory rather than
messages. The alternative worth naming is a *minted* endpoint carrying a badge the server can read
(§25's tracked later step), which would let one endpoint carry two authorities. It is not built, and
this design does not need it.

## Propose is deliberately not set, and the bounds are public

The service applies four rules, stated in the contract crate so they are host-tested in
milliseconds and so a well-behaved proposer can predict the answer:

| rule | value | why |
|---|---|---|
| sanity floor | 2026-01-01 | no machine running this code existed earlier; also the build-era anchor the NTS chicken-and-egg needs, chosen on purpose rather than discovered halfway through |
| sanity ceiling | 2100-01-01 | a clock attack pushes past a certificate expiry rather than nudging |
| max step forward | 1 hour | absorbs a machine that slept or an RTC set to local time; will not walk past an expiry in one step |
| max step **backward** | 1 second | moving forward skips instants nobody observed; moving backward makes instants happen **twice**, which is what breaks log ordering, cache expiries and build stamps |

The asymmetry is the substance, not timidity, and it has its own test so a later tidying of the two
constants into one has to mean it. Publishing the bounds costs nothing: the authority was never
secrecy about them, it is that the proposer cannot write the page.

**When the clock is unknown there is nothing to step from**, so a plausible proposal is accepted
outright. That is the bootstrap, not a hole: a machine that does not know the time holds no belief a
step limit could protect, and the sanity window still applies.

An accepted proposal lands as `state::SYNCED` rather than `state::SET`, because "an external source
I bounded" and "a human told me" are different provenance, and the difference is exactly what a
caller weighing a certificate expiry wants.

## The unknown state is a real state, and 1970 is not a time

`clock_proto::state` has four values and the first is `UNKNOWN`. A page nobody has published to is
zero, and zero reads as unknown, so the honest answer is the **default** rather than something
initialisation has to remember. An RTC that is absent, or whose reading falls outside the sanity
window, leaves the clock unknown; the service does not publish a value it does not believe.

For `std` this is where the rule bites hardest, because `SystemTime::now()` has **no error channel**.
The only loud refusal available is a panic, and that is what an unknown clock gets, with a message
naming which of the two causes it was (no capability, or no believable RTC). A program that never
asks the time is unaffected. Recorded as a real limitation rather than a clean win: std has no way
to represent "I do not know", so a program cannot ask before it asks, and the readable form of the
state lives one level down in `clock_proto` for anything that wants to check first.

## Two drivers, because parity is a gate, and the binding chooses between them

`arm,pl031` at `0x9010000` (one 32-bit register, **seconds**) on aarch64 `virt`;
`google,goldfish-rtc` at `0x101000` (two 32-bit registers, **nanoseconds**, low first because it
latches high) on riscv64 `virt`. Both are in the one portable `clock` binary on both ISAs, both take
a base address and know nothing else (rule 2), and both are found through `crates/dtb`.

**Discovery is by `compatible`, not by node name**, and this is where that shortcut finally ran out:
the aarch64 board calls the node `pl031@9010000` and the RISC-V board calls its RTC `rtc@101000`, so
no name prefix finds both. `dtb::node_reg_compatible` is new for this, and `node_reg`'s own comment
had predicted needing it. The kernel passes the *binding* to the service at spawn, so the driver
picks its register layout from what the machine said rather than from `target_arch`. That matters
concretely rather than theoretically: the VisionFive 2 is riscv64 and has neither device, so an
ISA-keyed driver would compile clean and read garbage on the first real board.

## What this lane deliberately did not do

- **No timed wait.** The milestone block's fork (a `SYS_SLEEP`, a timer object, or a deadline on
  `Endpoint::RECV`/`CALL`) is unsettled and serves more than this milestone. `thread::sleep` stays a
  yield-spin. Reading time is ambient and harmless; **blocking** on time is a scheduler interaction,
  and that is the part that wants a capability.
- **No calendar and no `date`.** Timezone and calendar conversion are pure computation and belong in
  a host-tested crate (§14), which is a sibling lane. Nothing here formats anything.
- **No NTP.** The propose endpoint is the seam it will arrive at, and the sanity floor above is the
  anchor its NTS bootstrap needs.
- ~~**The unknown-clock path is not proven in the guest.**~~ **Closed 2026-07-31 by `date`**, and the
  way it was closed corrects the reasoning above rather than merely satisfying it. This entry argued
  the path was untestable because "both QEMU boards always have a working RTC", but that is a claim
  about the *machine*, and what a reader actually tests against is the *page*. **A frame nobody has
  published to is exactly that machine as far as any reader can tell.** So the test allocates one,
  zeroes it, grants it read-only, and requires `date` to say the time is unknown. No absent RTC
  needed. The general lesson is worth more than the test: a scope note that says "we cannot test this
  because the hardware always works" is often describing the wrong boundary.

  `date` distinguishes the two causes, because they call for different fixes: *the machine has no
  clock it believes* versus *this process holds no clock capability*. The second is probed **without
  touching the page**: a process granted no clock has nothing mapped at `CLOCK_VA`, so reading to
  find out would fault instead of answering; it invokes the slot with a method no object defines,
  the same shape as the std PAL's `granted()`.
