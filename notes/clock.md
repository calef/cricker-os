# Wall-clock time

What the machine knows about the time, who is allowed to change it, and why the three answers are
three different capabilities. Milestone 51 lane A; the decision and its argument are
[DECISIONS §43](../DECISIONS.md), the contract is `crates/clock_proto`.

## The thing this replaced

`SystemTime` used to be the monotonic counter offset from `UNIX_EPOCH`. That means the machine
reported **1 January 1970 plus however long it had been up**, and the interface said nothing about
it. `notes/std.md` carried the caveat, honestly, and the caveat was the problem: a program does not
read the notes. Differencing two `SystemTime`s gave a correct duration; any absolute reading was a
fiction that looked exactly like a fact.

The defect was never the missing hardware. It was that the interface lied, which is the same shape
[§42](../DECISIONS.md) forbids for filesystems: not that FAT is weak, but that `rename()` on it
succeeds and the caller cannot tell.

## Counter plus offset

```text
   Instant  =  counter                     (ambient, one instruction, nobody writes it)
   wall     =  counter + offset            (the offset is the only writable thing here)
```

The counter is `CNTVCT_EL0` on aarch64 and the `time` CSR on RISC-V, readable from EL0/U-mode
because the kernel opened it (see notes/abi.md). Nothing in this design writes it, and nothing can.

That is the whole payoff: **adjusting the wall clock cannot perturb monotonic time by construction.**
Unix has `adjtime` slewing partly because a backwards step breaks code that assumed time only moves
forward. Here a step is an offset write and `Instant` never observes it, so slewing becomes a policy
choice for wall-clock readers rather than a correctness requirement. The kernel test
`adjusting_the_wall_clock_leaves_the_monotonic_counter_alone` steps the clock half a second and
requires the counter to have moved only by the cost of the round trip.

## Three authorities

```text
                          the RTC's registers (a device mapping, one holder)
                                    │
                          ┌─────────▼────────┐
 propose ──an endpoint───►│  clock service   │
(bounded, policy applies) └─────────┬────────┘
                                    │ publishes
                          ┌─────────▼────────┐
                          │  the clock page  │◄── set: the SAME page, mapped read/WRITE
                          └─────────┬────────┘
                                    │ mapped read-only
                                readers
```

| authority | the capability | cost of a read/write |
|---|---|---|
| none | nothing | the machine does not know what time it is, and says so |
| read | the clock page, `MAP_RO` | two loads and an add; no syscall, no server |
| set | the clock page, `MAP_RW` | one seqlock publish; nothing polices it |
| propose | the propose endpoint, `WRITE` | one `CALL`; the service may refuse |

Nothing new was needed in the kernel: the ladder is `Frame` with `READ`, `Frame` with `WRITE`, and
`Endpoint` with `WRITE`. No new syscall, no new method, no new object type. Rights are already
introspectable, so `caps` shows which rung a process is on.

**Why set is a page and not a message.** A process has exactly one blocking wait point, because this
kernel has no wait-any primitive and no threads sharing an address space. Two message-borne
authorities would therefore need two server processes, and the second would hold full set authority
anyway. Making set a page write is not a workaround for that limitation: writing the offset is what
setting the clock *means*, and §33 settled the same shape for the compositor ("the compositor's
authority is memory, not messages"). §43 records the alternative (a minted endpoint carrying a
badge, §25's tracked later step) and why this design does not need it.

## The seqlock, and why the memory ordering is load-bearing

The clock page is four words: a magic, a sequence, a state, and the offset. Readers are many and
writers are few, and there is no lock a process could hold across an address-space boundary, so it is
a seqlock:

- a writer claims the sequence with a **compare-exchange** from even to odd (writers are multiple by
  the capability layout, even if two racing is not a design anyone wants), stores the data, then
  releases the sequence to even;
- a reader takes the sequence, reads the data, fences, and takes the sequence again; equal and even
  means the reading is whole.

This is the project's rule 4 in a place where it matters: on a strongly ordered machine a sloppy
version passes every test forever, and on ARM and RISC-V it does not. The acquire on the writer's
claim keeps its stores from being hoisted above it; the release keeps them all visible to any reader
that sees the even sequence; the reader's fence keeps its data loads from sinking below the second
sequence read, which is what makes the check mean anything.

A torn read here would be a wrong time rather than a crash, which is the worst kind of bug to leave
possible, so the invariant has its own test in `clock_proto`.

## "I do not know what time it is" is a state, and it is the default

`clock_proto::state` has four values:

| | meaning |
|---|---|
| `UNKNOWN` | no clock page, no service, no RTC, or an RTC reading nobody believes |
| `RTC` | read once at startup from the hardware clock |
| `SET` | written outright by an authority holding the page read/write |
| `SYNCED` | an accepted proposal, i.e. an external source the service bounded |

A frame nobody has published to is zero, and zero is `UNKNOWN`, so the honest answer is what you get
by default rather than something initialisation has to remember. The clock service **does not
publish a reading it does not believe**: an RTC outside the sanity window leaves the clock unknown
rather than confidently wrong.

`SET` and `SYNCED` are kept apart because "a human told me" and "an external source I bounded" are
different provenance, and that difference is what a caller weighing a certificate expiry wants.

## The policy, and the asymmetry that is the point

`clock_proto::policy` is a pure function, host-tested in milliseconds, and it lives in the contract
crate rather than inside the service so a well-behaved proposer can predict the answer. The bounds
are public because the authority was never secrecy about them; it is that a proposer cannot write
the page.

| rule | value |
|---|---|
| sanity floor | 2026-01-01 (also the build-era anchor NTS's chicken-and-egg needs) |
| sanity ceiling | 2100-01-01 |
| max step forward | 1 hour |
| max step backward | **1 second** |

Forward and backward are not the same problem. Moving forward skips over instants nobody has
observed yet. Moving backward makes instants happen **twice**, which is what breaks log ordering,
cache expiries, build stamps, and anything that recorded a timestamp and assumed it would not be
reissued. Hence three orders of magnitude between the two constants, and a test that fails if
somebody tidies them into one.

When the clock is `UNKNOWN` a plausible proposal is accepted outright. That is the bootstrap, not a
hole: a machine that does not know the time holds no belief a step limit could protect.

## Two drivers, because parity is a gate

| board | binding | address | register layout |
|---|---|---|---|
| QEMU `virt`, aarch64 | `arm,pl031` | `0x9010000` | one 32-bit `DR` at offset 0, **seconds** since the epoch |
| QEMU `virt`, riscv64 | `google,goldfish-rtc` | `0x101000` | `TIME_LOW` at 0, `TIME_HIGH` at 4, **nanoseconds**; read LOW first, it latches HIGH |
| VisionFive 2 | its own RTC | via DTB | milestone 16a, board bring-up |

Both drivers are in the one portable `clock` binary, compiled on both ISAs, each taking a base
address and knowing nothing else (rule 2).

**Discovery is by `compatible`, and this is where matching on the node name finally ran out.** The
aarch64 board calls the node `pl031@9010000`; the RISC-V board calls its RTC `rtc@101000`. No name
prefix finds both, so `dtb::node_reg_compatible` is new for this milestone. `node_reg`'s own comment
had predicted needing it ("a real driver would match `arm,cortex-a15-gic`... written down for the Pi
port"), and this is that day. Two wrinkles the fixture tests pin: `compatible` is a NUL-separated
list and a match on **any** entry counts (the PL031 declares `"arm,pl031", "arm,primecell"`), and the
node writes `reg` **before** `compatible`, so the decode has to wait for the node to close rather
than decide at the `reg` property.

The kernel passes the *binding* to the service at spawn, so the driver picks its layout from what
the machine said rather than from `target_arch`. That is not fastidiousness: the VisionFive 2 is
riscv64 and has neither of these devices, so an ISA-keyed driver would compile clean and read
garbage on the first real board.

## Where the interactive boot puts it (milestone 51's wiring)

Both ISAs' `--features shell` boots start the clock service before init exists and grant **init** the
page with `READ` and `GRANT`: slot 3 on RISC-V (`riscv_shell_boot`), slot 5 on aarch64 (`spawn_init`,
boot role only). init hands a read-only copy plus a read-only mapping at `0x00c0_0000` to any child
whose `grant_plan` manifest declares `clock`, which today is `date` and nothing else.

Three things about that shape are deliberate:

- **The grant does not depend on what the machine has.** A boot with no `clock` program in its initrd
  gets a zeroed frame instead, which reads as `UNKNOWN` and is the honest answer. Slot numbers that
  moved with the hardware would be a wiring nobody could check by reading.
- **The shell is not on the path.** It holds no clock and cannot delegate one, so the set of processes
  that can read the time is decided by manifests init reads rather than by anything typed at a prompt.
  `caps date` prints the row so a person can still see it (notes/date.md).
- **`READ` all the way down.** Nothing between the kernel and a spawned child ever holds the writable
  mapping, which is why "there is no `date -s`" is a fact about the wiring rather than a missing flag.

`script/shell-check` types `date` at the real prompt on both ISAs and requires `UTC` in the answer,
which is the one word neither unknown-clock sentence contains.

## What a std program sees

`SystemTime::now()` is the clock page plus the counter, and a std program's whole wall-clock
authority is **slot 5** (a `Frame` capability with `READ`) and a read-only mapping at
`rt::CLOCK_PAGE`. See notes/std.md.

The uncomfortable part, recorded rather than smoothed over: `SystemTime::now()` has **no error
channel**, so the only loud refusal available when the clock is unknown is a **panic**. That is what
it does, with a message naming which of the two causes it was. A program that never asks the time is
unaffected, but a program cannot ask whether it *can* ask, because std has no way to represent "I do
not know". The readable form of the state lives one level down in `clock_proto` for anything that
wants to check first, and a `no_std` component simply reads the page.

## What this lane did not build

- **No timed wait.** There is still no sleep, no timeout, and no deadline anywhere in the kernel;
  `thread::sleep` is a yield-spin and stays one. The three candidate shapes are in the milestone 51
  block and the choice is open. The distinction worth holding onto: *reading* time is ambient and
  harmless, *blocking* on time is a scheduler interaction, and that is the part that wants a
  capability.
- **No calendar, no `date`, no NTP.** Sibling lanes. The propose endpoint is the seam NTP arrives
  at, and the sanity floor above is the anchor its NTS bootstrap needs. (`crates/calendar` and
  `date` have since landed; see notes/calendar.md and notes/date.md.)
- **No alarm interrupt.** Both RTCs have one; nothing here uses it. The service reads the clock once
  at startup and lets the monotonic counter carry the time, because re-reading would import the
  RTC's drift and coarse resolution into a clock that already has better.
- **The unknown-clock path is not proven in the guest.** Both QEMU boards always have a working RTC,
  so the service's refusal to publish an implausible reading is host-tested and the std panic is
  proven by construction rather than by a booted test.

  **Half of that is no longer true, and the reasoning was the part that was wrong.** It is about the
  *machine*, and what a reader tests against is the *page*: a frame nobody has published to reads as
  `UNKNOWN`, which is exactly what a reader on a machine with no believable RTC holds. So the
  `date` lane proves the **reader's** unknown-clock path in the guest, on both ISAs, by allocating a
  blank frame and granting it (notes/date.md,
  `kernel::user::date_tests::an_unknown_clock_is_said_plainly_rather_than_printed_as_1970`). What
  remains proven only by construction is the **service's** side: its refusal to publish an
  implausible RTC reading is still host-tested, because that genuinely does need a machine whose RTC
  lies, and neither QEMU board has one.
