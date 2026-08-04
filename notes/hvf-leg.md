# The HVF leg: the aarch64 suite on the physical core

*(Milestone 81. `script/gates`, `cargo xtask test --hvf`, `scripts/qemu-runner-aarch64.sh`.)*

Every test this project had ever run ran under TCG, where QEMU translates each aarch64 instruction
into host code. That is the right default (deterministic, identical on any host), but it is not a
real machine: no real caches, no real store buffer, and four "cores" that QEMU round-robins between
rather than four cores running at once. Apple's Hypervisor.framework runs the same kernel at guest
EL1 on the physical Apple Silicon core, four vCPUs on four host threads, and until the RISC-V board
arrives it is the only real silicon this project can test on at all.

## How to run it

```sh
script/gates            # fmt, lint, test (TCG, both ISAs), then this leg
script/test --hvf       # just this leg
```

`--hvf` is aarch64 only and says so if you ask for anything else: Hypervisor.framework runs the
host's own ISA, so `--arch riscv64 --hvf` is refused, and so is `--cpu`, because the guest runs the
physical core and `-cpu host` is the only answer.

It skips the host-logic crates, the vendored RedoxFS round trip and the `fs_server` core, and
prints one line saying so. Those are host code on the host; no accelerator exists on that path, so
re-running them costs about 30 seconds and proves nothing the TCG leg has not. What the leg does
re-run is everything an accelerator can change: the 234 kernel tests under QEMU, the host-side
scanout referee, and the post-run image checks.

## What it costs (measured 2026-08-04, M-series, warm build)

| command | what ran | wall clock |
|---|---|---|
| `script/test --arch aarch64` | host crates + the 234-test aarch64 kernel suite under TCG + image checks | 58 s, 52 s |
| `script/test --hvf` | the same 234-test suite on the physical core + image checks | 16 s, 12 s |
| `cargo test --workspace --exclude kernel --exclude user --exclude user_rt` | the host crates alone | 14 s |

The counts here read 232 when the timings were taken, and are 234 in the merged tree: milestone 86's
`time` brought two kernel tests with it. The number is a fact about the whole suite rather than
about this leg, so it is taken at the merge (CLAUDE.md), and the wall clocks are left as measured
because two tests do not move a twelve-second figure.

Two samples each, on a machine that was not otherwise idle (another lane was building). Subtract
the 14 seconds of host crates that only the TCG row contains and the comparison is about 40 s
against about 14 s for the same fixtures, the same 234 tests, and the same post-run image checks:
roughly a **3x** win on the leg, and more than that on the booted suite alone, since a fixed few
seconds of both numbers is cargo and image building.

So the leg adds 12 to 16 seconds to `script/gates`. Timing it before adopting it was the condition
milestone 81 set; the `--full` flag the block named as the fallback is not needed, because native
execution beat TCG by the margin the block hoped for.

## Why `script/gates` and not a workflow

GitHub's hosted macOS arm64 runners are themselves virtual machines and do not expose nested
virtualization, so HVF does not exist there. A self-hosted runner on the dev machine would close
that gap and is deliberately not part of this: it couples CI to a laptop that sleeps.

`script/gates` is the one command a person or an agent runs before pushing, which makes it the
place where every lane on this machine picks the leg up for free. When the host cannot supply HVF
the leg **skips loudly**, naming the reason (not macOS, not Apple Silicon, `kern.hv_support` off,
or a QEMU built without the accelerator) and saying in plain words that nothing in the run executed
on a physical core. A silent omission would let a Linux CI transcript read exactly like one from
this laptop, and the whole point of the leg is that those are not the same evidence.

## Semihosting is not answered under HVF, and this is what that looks like

The test harness reports its verdict by asking the host to exit: `testing::runner` ends in
`semihosting::exit`, and so do the panic handler and both watchdogs. **QEMU 11.0.2 does not
intercept ARM semihosting on the HVF path.** The tree already believed this (`notes/benchmarks.md`
records "semihosting does not work under HVF" and the bench kernel parks in `wfi` instead of
exiting); milestone 81 measured it, and measured what happens next.

The probe was nine instructions: write `A` to the PL011, then `hlt #0xf000` with `x0 = 0x18`
(`SYS_EXIT`).

| accelerator | result |
|---|---|
| TCG | the process exits at the trap |
| HVF | `A` appears, then nothing; QEMU runs until killed |

Booting the real kernel says what the guest sees. The trap is **not swallowed**: it raises a real
synchronous exception into the guest's own vector table, `EC 0x00` ("Unknown reason",
`ESR_EL1 = 0x02000000`), with `x0 = 0x18` and `x9 = 0x20026` still in the registers. Our handler
does the correct thing for an exception it does not know, which is to panic, and the panic handler
under `cfg(test)` calls `semihosting::exit`, which takes the same trap again. **Four cores doing
that write interleaved garbage at native speed, forever.** `kernel/src/arch/aarch64/semihosting.rs`
predicted exactly this in a comment ("without one it raises a real exception") without knowing it
had a case.

So the host, not the guest, ends the run. `hvf_kernel_leg` owns the QEMU child and reads its
transcript, the same shape `run_bench` has used since milestone 21, and takes its verdict from
three markers the harness prints *before* the exit that will not happen: `test result: ok. N
passed`, `[PANIC] `, and `WATCHDOG:`. Once something has failed it relays 200 more lines (enough
for the longest thread dump) and stops, because there is no end of output to read to. The first
version of this leg had no such budget and left an xtask spinning on the storm for four minutes,
holding `target/crickerfs-blank.img` and failing the *next* run with `Failed to get "write" lock`,
which is precisely the trap CLAUDE.md warns about.

## The SMMU correction

`scripts/qemu-runner-aarch64.sh` used to attach `iommu=smmuv3` on the TCG path only, on the
recorded ground that "smmuv3 emulation alongside HVF acceleration is the fragile combination."
**Nobody had run it.** The suite on the physical core runs green with the SMMU attached, including
`a_backing_outside_the_grant_is_refused_by_the_iommu`, the test that asserts the *hardware* faults
a DMA leaving its domain.

The belief was also costing a real gap while it stood: with no SMMU under HVF, the compositor test
fails outright ("a virtio-gpu is present but the IOMMU is not active"), and the DMA-confinement
tests would have had no hardware to assert about. The flag now applies on both paths, which is
where it belongs on principle too: the accelerator chooses how CPU instructions execute, while the
SMMU sits in front of the PCIe root complex and translates *device* traffic that QEMU emulates in
the host process either way. This is a correction to the record, in the sense CLAUDE.md means: the
machine overruled the comment.

## What the physical core found: five assertions about "has it happened yet"

Every failure belonged to the milestone-78 family (`notes/load-sensitive-assertions.md`), and every
one was found from the **opposite direction**. That note's diagnostic is that a slow machine
produces a deficit; HVF is a *fast* machine, and it produces the same deficit for the mirror-image
reason. A yield count is not a duration in either direction: on a loaded host this core burns cheap
yields while another core is descheduled, and on the physical core it burns them in nanoseconds
while another core has not been dispatched yet.

They arrived one per run, over five runs (so the leg is a *sampler* of this defect, not a detector
of it), except one that was fixed on inspection because it was the same shape a few lines from a
failing sibling.

- **`sched::a_blocked_waiter_wakes_with_an_error_when_its_endpoint_is_revoked`** ("the revoked
  waiter never woke"). One `yield_now()` was the entire wait for the waiter to block on the
  endpoint, under a comment saying "single core: one yield lets the waiter run", which stopped
  being true when DECISIONS §28 scattered placement across cores. Under HVF the reclaim ran before
  the waiter had ever been scheduled, so there was nothing to wake. The test now waits, on the
  clock, for the endpoint's receive queue to actually hold the waiter, which needed a new
  `#[cfg(test)] sched::endpoint_waiting_receivers` beside the existing
  `endpoint_waiting_senders`. Its second wait (50 yields for `WOKE`) is now `wait_for` too.
- **`sched::other_threads_run_while_one_is_blocked`** ("a worker made no progress while another
  thread was blocked on IPC"). 100 yields on this core, then assert the worker on another core had
  incremented a counter. Now `wait_for(|| PROGRESS > 0)`. Nothing is weakened: if a blocked thread
  were requeued and starved the worker, the counter stays at zero for the full two seconds and the
  assertion fails with the message it always had. Its teardown also changed from 20 yields to
  waiting for both of its own threads to be gone, because this test's late-landing teardown is
  exactly the neighbouring state that made *other* tests' accounting fail in milestone 78.

- **`user::reap_tests::reaping_an_uncollected_corpse_leaves_no_ghost_on_the_endpoint`** ("the
  corpse never parked on its supervision endpoint"). 4000 yields waiting for a dying child on
  another core to park its death message. This one passed three HVF runs and failed the fourth,
  which is worth stating: **the leg samples this defect, it does not detect it.** Now
  `wait_for(|| endpoint_waiting_senders(fault_ep) == 1)`.
- **`user::supervision_tests`'s reclaim after a respawn** (2000 yields, no assertion at all, so a
  timeout silently left an unreclaimed region for a neighbour to trip over). Not a failure; fixed
  on inspection, and it now asserts.

- **`sched::a_thread_that_never_yields_is_preempted_anyway`** ("the spinner never ran at all"). Not
  a yield count, and the most interesting of the five, because it is a **vacuity** guard that the
  faster machine turned into a failure. The test spawns a hostile spinner and a polite thread on
  the same core, waits for the polite one to report, sets `STOP`, and then checks the spinner had
  spun at least once, since a polite thread running on a core nobody was monopolizing proves
  nothing about preemption. Which of the two runs first is a race, and on the physical core it came
  out the other way: the polite thread finished, `STOP` was set, and the spinner was killed before
  its first increment. It now waits (its own bounded second) for the spinner to have spun before
  stopping it, so the guard fires on a genuinely dead spinner rather than on a scheduling order.

`user::tests::wait_for` became `pub(super)` so the two `user` modules could use the existing
implementation instead of becoming the sixth and seventh copies of it.

**None of these was a timer-behaviour failure**, which is the finding worth stating plainly,
because timers were what the milestone expected to be perturbed.

## Timer behaviour: what did NOT differ, and why

Under HVF guest time is host time (`CNTVCT_EL0` is passed through at the host's 24 MHz) and there
is no icount instrument, so the milestone expected the wall-clock assertions to be the casualties.
None of them failed. The reason is milestone 78, which landed the night before: the timer-drift
twins had already been re-aimed at the **re-arm law** (over a window in which `MISSED_TICKS` did
not move, the deadline advanced by exactly one interval per delivered tick), and that law is a
statement about state the kernel owns, not about how fast the clock runs. A test that measures the
right thing does not care which accelerator it is on, which is a stronger endorsement of that work
than the milestone could have written for itself.

The assertions that *do* keep wall-clock exposure passed here as well: the handler-latency pair
(`the_handler_keeps_up_when_no_lock_is_held`), named in that note's BUGS as unfixed, and
`ticks_arrive_at_the_configured_rate`'s surviving bound. HVF makes them no worse in principle: a
deschedule of a host thread running a vCPU produces the same missed tick either way.

### The settle windows the leg makes weaker, not flakier

Three sites spend 400 yields to "let it settle" and then assert that **nothing more** happened
(`user::c_seam_tests` line ~122, `user::authority_tests` ~121, `user::live_swap_tests` ~185, each
asserting `endpoint_waiting_senders(report) == 0`). These do not fail under HVF and will not: a
shorter real settle window makes a negative assertion easier to pass, so the leg quietly proves
*less* there rather than going red. They are left alone deliberately, because turning a settle
window into a wall-clock duration is a design question (how long is "nothing more happened"?) and
not the mechanical fix the four above took. Recorded here so the next reader does not mistake their
silence for a clean bill.

## BUGS

- **A failing run leaves an exception storm behind it.** The kernel has no way to know its
  semihosting exit will not be answered, so any failure under HVF ends in an unbounded panic loop
  on four cores. The host stops reading and kills the child, so the cost is bounded in practice,
  but a transcript from a failed HVF run ends in interleaved garbage after the 200-line budget, and
  anyone driving the runner **by hand** under HVF (rather than through `xtask`) will get a QEMU
  that never stops. Use `scripts/qemu-bounded.sh` for that. A guest-side fix (recognising the
  semihosting trap in the Unknown-reason handler and parking in `wfi` instead of panicking) is not
  built here; it would touch the exception path for a test-only benefit.
- **The leg is not a CI gate and cannot be one.** Nothing enforces that it ran. `script/gates` is
  the enforcement, and `script/gates` is a convention.
- **One machine, one model, no variation.** `-cpu host` is mandatory under HVF, so this leg says
  nothing about other aarch64 implementations; that job stays with `script/cpu-matrix` (which is
  riscv64's) and with the second board.
- **The leg samples the yield-count defect rather than detecting it.** One of the four found here
  passed three consecutive HVF runs before failing. So a green HVF leg is evidence, not proof, and
  a red one on an unchanged tree should be read as this family before anything else.
- **The suite still exits through semihosting on the TCG leg**, so the two legs report their
  verdicts by different mechanisms. A change to the harness's final line
  (`test result: ok. N passed`) would silently turn this leg into one that always fails, and no
  test asserts that string. It is matched in `hvf_kernel_leg`.

## See also

- design/roadmap/81-hvf-leg.md (the block)
- notes/scripts.md (`script/gates` and `script/test`, and where the leg sits in them)
- notes/load-sensitive-assertions.md (milestone 78: the family both failures belong to)
- notes/benchmarks.md (`--real`, the other HVF caller, and the exit trick this leg reuses)
- notes/semihosting.md (the mechanism that is not answered here)
- notes/framebuffer-contract.md (the scanout the referee checks, and why the guest cannot)
