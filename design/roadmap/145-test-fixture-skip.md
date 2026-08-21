# 145. A test that needs hardware the boot doesn't have can say so

**Status: NOT-STARTED.**

**Gate: NONE.** Minted provisionally by calef on 2026-08-21, during milestone 16a's bench session;
the integrator should confirm the number at merge.

## What this is

Milestone 16a's on-board test-suite exit (§144's sibling finding, recorded in
notes/visionfive2.md) ran the kernel's `#[test_case]` suite on the VisionFive 2 for the first
time. After six real hardware-assumption bugs were found and fixed (PR #380), the suite hit a
different kind of wall: `kernel/src/nvme.rs`'s end-to-end test expects a synthetic NVMe
controller that `xtask` always attaches under QEMU (`NIFE_NVME`), and the board's manual U-Boot
boot attaches nothing. Its own comment already says why this is not a bug: "the test flow always
attaches a controller... absence is a lost QEMU flag, not a machine without a disk."

A survey across `kernel/src/` found at least 31 more `#[test_case]`s across 7 files with the same
shape, gated on env vars the QEMU runner scripts set and a bare board boot cannot:

| File | Fixture | Env var |
|---|---|---|
| `kernel/src/nvme.rs` | NVMe controller | `NIFE_NVME` |
| `kernel/src/user/credential_tests.rs` | virtio-rng | `NIFE_RNG` |
| `kernel/src/user/disk_tests.rs` | virtio-rng, disk_surveyor/disk_partitioner programs | `NIFE_RNG` |
| `kernel/src/user/display_tests.rs` | virtio-gpu-pci | `NIFE_GPU` |
| `kernel/src/user/ntp_tests.rs` | virtio-rng | `NIFE_RNG` |
| `kernel/src/user/compositor_tests.rs` | virtio-gpu (compositor's device) | `NIFE_GPU` |
| `kernel/src/user/entropy_tests.rs` | virtio-rng | `NIFE_RNG` |

`scripts/qemu-runner-riscv64.sh` wires roughly forty `NIFE_*`-gated synthetic devices. Every one
of these tests is correct as written for the machine it has always run on. None of them is wrong
the way the six bugs milestone 16a's bench session found were wrong (a hardcoded fact about
QEMU's specific configuration that is false on other hardware). These are correct claims about a
machine that a bare board boot is not.

## Why this is a design question, not six-more-bugs

The kernel's test harness has no notion of "skip." `kernel/src/testing.rs`'s `Testable` trait is
`Fn()`: a test either returns (pass) or panics (fail, caught by the panic handler which prints
`NIFE-TEST-EXIT: FAIL <code>` and calls `arch::semihosting::exit`). There is no third outcome.

The boot **tour** (not a `#[test_case]`, the ordinary non-test boot path in `kernel/src/main.rs`)
already has the pattern this milestone would extend to tests: `println!("... skipped (no 'outlaw'
program in the initrd)")` instead of asserting the fixture is there. The tour was always meant to
run on a machine of unknown provisioning; the test suite was never designed for that, because it
never needed to be until a board existed to boot it on.

## What this milestone would decide

1. **A way for a test to declare what it needs**, and a way to check that before running it.
   Candidate shapes, not yet decided between:
   - A `Testable` variant (or a second trait) that returns `Result<(), Skipped>` instead of
     nothing, and the runner counts skips separately from passes in the final line.
   - A capability-probe convention: tests that already look up a device (`virtio::find(...)`,
     etc.) check for `None` and call a `skip!()` macro instead of `.expect()`, which prints
     `test NAME ... skipped (no virtio-rng device)` and returns early, no new trait needed.
   - A `#[cfg(feature = "board")]` split, mirroring the exit-path split milestone 16a's test-exit
     PR already introduced for `semihosting.rs`: some tests simply don't compile into the board
     test binary at all. Cheapest, but loses coverage information (a skipped test that print its
     absence is more honest than a test invisible from the binary).
2. **Whether "skipped" should still count toward the final line.** `test result: ok. N passed`
   currently means N ran and N passed. A board run that skips 31 tests and passes the rest 248
   should say so exactly (`ok. 248 passed, 31 skipped`), not roll skips into either bucket
   silently.
3. **Whether skipping is itself something to gate on.** A suite that silently skips more and
   more tests as it moves to new hardware could hide an actual regression behind "well, it's
   just skipped there." Worth deciding whether a board run reports its skip count loudly enough
   that a growing number gets noticed, the same way `script/lint`'s counted-claims check already
   catches drifting numbers elsewhere in this tree.

## What this does NOT include

- **Building synthetic fixtures the board can actually attach** (an SD-card-resident RNG source,
  a USB NVMe enclosure, etc.). That is a hardware-acquisition question, is much more expensive
  than a software mechanism, and does not scale: forty env vars is forty pieces of hardware.
- **Fixing any of the six bugs milestone 16a's bench session already found and fixed.** Those
  were wrong assumptions about real hardware and are done (PR #380). This milestone is about the
  tests that are correct and simply need hardware the board doesn't have.
- **Deciding which mechanism, yet.** The three candidates above are options, not a recommendation.
  This block exists to scope the question; the decision belongs in a `design/decisions/` entry
  once someone has weighed the trade (a new trait touches every existing `#[test_case]`'s type
  signature implicitly through the blanket impl; a macro touches only the ~31 sites that need it;
  a cfg split is nearly free but hides skipped coverage from the binary itself).

## Prior art in this tree

- `kernel/src/main.rs`'s boot tour already has the "print and move on" pattern for a missing
  fixture; whatever this milestone builds should look like that, not invent a new vocabulary.
- `crates/isa`'s device-tree readers (`CpuList`, `PlicContexts`) already model "the tree did not
  say" as `None`/empty rather than an error, which is the same shape one level up: a fixture the
  boot did not provide is not a machine that is broken, either.
