# The instruction clock: timing claims a busy host cannot falsify

*(Milestone 78's last piece. `script/icount`, `kernel/src/icount.rs`, the `icount` feature, and the
`tick_trace` hooks in `kernel/src/arch/aarch64/timer.rs` and `kernel/src/arch/riscv64/timer.rs`.
Every name in that list is **provisional**: calef names the scripts, crates, programs and modules,
and a lane ships a provisional name and says so.)*

## The one fact everything here rests on

Under `-icount shift=0,sleep=off`, QEMU's virtual clock advances **by exactly one nanosecond per
guest instruction retired, and by nothing else.** Not by host time, not by how many other lanes are
gating on the same laptop, not by whether the emulator's thread was scheduled at all.

That single property is what turns a whole family of unaskable questions into assertions.
`notes/load-sensitive-assertions.md` states the problem twice, once per ISA, in the same words:
**from inside the guest, a 30 ms handler and a 30 ms deschedule are the same observation.** Milestone
78's other four rounds re-aimed everything that could be re-aimed at a property the host cannot
touch; two claims survived because no wall-clock margin can separate those two cases, and a margin
wide enough to try is a margin that no longer catches the defect. That is DECISIONS §61's scar, and
the block forbids it by name.

An instruction count is a third option. It does not widen a bound; it changes the unit the bound is
written in, to one the host has no access to.

## Running it

```sh
script/icount                    # both ISAs, ~7 s including two kernel builds
script/icount --arch aarch64     # one leg
script/icount --arch riscv64
```

It prints what it measured and then says whether the claims hold. A violated claim is a panic in the
guest, with the measured number and the bound in the message; `xtask` reports the panic and exits
nonzero.

### EXAMPLES

A clean run, both legs, 2026-08-17, on the pinned QEMU:

```
--- icount: aarch64, single hart, TCG + icount (one instruction = one nanosecond) ---
  cntfrq 62500000
  instructions_per_counter_tick 16
  tick_interval 625000 10000000
  calibration 2000032 2000000
  ticks 64
  arrival_instructions min 1008 mean 1008 max 1008
  handler_instructions mean 1056 max 1056
  missed_ticks 0
  early_arrivals 0
  done
icount: aarch64 claims hold

--- icount: riscv64, single hart, TCG + icount (one instruction = one nanosecond) ---
  cntfrq 10000000
  instructions_per_counter_tick 100
  tick_interval 100000 10000000
  calibration 2000100 2000000
  ticks 64
  arrival_instructions min 300 mean 300 max 400
  handler_instructions mean 800 max 900
  missed_ticks 0
  early_arrivals 0
  done
icount: riscv64 claims hold
```

**Read `min` and `max` before anything else.** On aarch64 they are the same number across 64
consecutive ticks, which is the instrument's whole claim demonstrated rather than argued: the
measurement has *zero* variance, so a bound on it is a statement about this kernel's code and about
nothing else. The riscv64 pair differ by one counter tick, which is that ISA's 100-instruction
quantization showing (see "Resolution" below), not jitter in the machine.

## What is asserted, and what each claim rules out

*(There are four. Claim 4 was added on 2026-08-18 by milestone 62, after an injection showed that
claims 1 to 3 could not see the timer drift bug at all; the section below it is the evidence.)*

### 1. The timer fired at the deadline the kernel armed

`arrival_instructions` is the distance from the deadline that fired to the handler observing it:
interrupt delivery, the vector, the register save, the dispatch, the GIC acknowledge on aarch64, and
the tick bookkeeping.

**On riscv64 this is the claim milestone 78 was left holding, and it is the whole reason the
instrument exists.** SBI's `set_timer` is write-only. `DEADLINE` is the kernel's own array, so
reading it back proves only that the kernel can remember what it meant to write; the block says so
in as many words ("on aarch64 the equivalent value is in a register the hardware itself consults, so
the readback is evidence and on riscv64 it is bookkeeping"). The residual gap is an implementation
that keeps `DEADLINE` on the grid and arms SBI with something else, and it would pass every other
test in this tree.

It does not pass this one, and that was proved by building it rather than by arguing it. See "The
injections" below, which also corrects the expectation this paragraph used to state: the existing
suite **does** notice both of the implementations tried, and it **misdiagnoses both**, which turns
out to be the more interesting result and the one the milestone is actually about.

aarch64 asserts the same thing, at the same site, for the reason rule 5 gives: a claim that holds on
one ISA and is unmade on the other is the gap, not the parity.

### 2. The handler takes fewer than N instructions

`handler_instructions` is deadline to re-armed: claim 1's span plus the tick bookkeeping, the
deadline write, and on riscv64 the SBI `ecall` round trip into OpenSBI and back. That trip is inside
the measurement on purpose; it is a real part of what this ISA's handler costs and it is the part
aarch64 does not pay, so a measurement that stopped short of it would be comparing two different
spans across the two ISAs.

**This is the quantity `MISSED_TICKS` has always been a coarse proxy for.** A miss is this number
exceeding one whole tick period, which is **10,000,000 instructions of virtual time on both ISAs**:
ten milliseconds at 100 Hz, and one instruction is one nanosecond. The missed-tick assertions could
therefore only ever say "the handler did not take ten milliseconds", and could not tell a handler
that took ten milliseconds from an emulator that was not running for ten milliseconds. The bounds
here are **about 4,000 times tighter** and have no second explanation.

*(Corrected 2026-08-18. This paragraph said "625,000 instructions of virtual time on aarch64",
which is the tick interval in **counter ticks** wearing instructions' units: 625,000 counter ticks
at 16 instructions each is the 10,000,000 above, and the run's own `tick_interval 625000 10000000`
line prints both numbers side by side. The same slip is in the doc comment on `HANDLER_BOUND` in
`kernel/src/arch/aarch64/timer.rs`, fixed in the same change, and it carried the understated "more
than two orders of magnitude" with it. It is the mistake this whole note exists to make hard, made
in the note: a number in the wrong unit, next to the right one.)*

The bounds live in the arch layer (`ARRIVAL_BOUND`, `HANDLER_BOUND`) because the trap path is the
arch's own. They are **ceilings with room for ordinary codegen movement, not baselines**: a change
that halves the number is not a failure, and one that doubles it is a fact worth stopping for. As of
2026-08-17 the margins are roughly 2x on aarch64 and 3x on riscv64 over what the machine reports,
and the run prints what it saw beside the bound, so the margin is visible rather than asserted
about.

### 3. Zero missed ticks, which is only assertable here

This one closes a recorded `BUGS` entry rather than adding a claim. The miss taxonomy on both ISAs
exists to tell a slow handler from a descheduled emulator by how late the re-arm was, and
`notes/load-sensitive-assertions.md` is honest that its cut leaves a window one tick period wide in
which a host deschedule is still blamed on this kernel (measured, 2026-08-16: a lateness of 0.83 of
an interval from a probe holding a lock across two and a half periods).

Virtual time has no deschedules. So here a miss has exactly one possible cause and needs no taxonomy
at all, and `missed_ticks == 0` is a bare assertion.

## Why it is opt-in, and why that is the design rather than a compromise

**`-icount` is not a flag that observes. It changes what QEMU is.** But the first thing to say is a
correction, because the milestone block states the cost as *"icount is slower and changes what the
suite measures"* and nobody had ever measured the first half of that. **On compute it is not
slower.**

| identical bench boot, `-smp 1`, three runs each | wall clock |
|---|---|
| `-icount shift=0,sleep=off` | 2.47 s, 2.50 s, 2.61 s |
| no icount | 2.69 s, 2.62 s, 2.80 s |

Measured 2026-08-17, same binary, marker to marker (boot to `bench: done`), on a laptop carrying
other lanes' gates. The instrument was if anything marginally *faster*, because `sleep=off`
fast-forwards virtual time through the idling the bench does between phases, and that gain covers
icount's per-instruction overhead. So "it would slow the suite down" is not the argument, and
repeating it would have been an unmeasured claim inside a milestone about unmeasured claims. The
second half of the block's sentence is exactly right, and is the whole of what follows.

**The two real reasons are these.**

*One virtual clock for every vCPU.* An idle hart parked in `wfi` jumps that clock forward to the
next event, so multi-hart timing is fiction here. That is why this boot and the bench boot are both
`-smp 1`, and why the placement probe can never move here (a cross-core delivery test cannot run on
a one-core instrument, `notes/benchmarks.md`). A suite run this way would not fail; it would
**silently stop proving every cross-core property it exists to prove**, which is worse.

*A clock-bound wait stops costing host time and starts costing instructions.* This boot samples 64
tick periods, which is 0.64 s of virtual time and therefore 6.4x10^8 guest instructions, and it
spends about 3 s of wall clock retiring them. A plain TCG guest reaches the same counter value in
0.64 s. So the suite's `spin_for`s and clock-bounded `wait_for`s would cost roughly five times what
they cost today, and the compute-bound parts would cost nothing extra. That is a real bill, and it
is a different bill from the one everyone assumed.

Putting it on `script/test` would therefore change what all ~400 tests measure in order to sharpen
two of them. So this is its own boot mode, its own feature and its own command, on the model
`script/bench` already set. **Nothing on the test path changes at all**: the `tick_trace` hooks are
`#[cfg(feature = "icount")]`, so the test and shipping builds do not contain them, and what this
boot measures is therefore the handler that ships rather than a handler carrying an instrument.

**Its own feature, which implies `bench` rather than extending it.** The two boots park at the same
point, so they leave exactly the same set of functions unreferenced, and `bench` already carries that
set spelled out one `cfg` at a time across five files. Declaring `icount` independent means writing
every one of those conditions again, in `main.rs`, `sched.rs` and `user.rs`, to describe a fact
already described; the other way out is a crate-level `allow(dead_code)`, and DECISIONS §38 refuses
that, correctly, and `script/lint` enforces the refusal. (It refused this lane's first attempt, which
is the mechanism working.)

What that costs is that the icount kernel carries the benchmark code, compiled and never run.
Nothing measures *this* binary against a baseline, so the ±5% codegen drift that makes `bench --check`
a coarse tripwire is not a cost here. The property that mattered is kept: **`script/bench`'s own
binary is untouched**, so no baseline had to be re-saved to admit an unrelated instrument, which is a
tripwire losing its history for nothing.

**What it costs to run:** 7.0 s wall for both ISAs including two kernel builds (2026-08-17, same
conditions).

## Resolution: what this instrument can and cannot see

Virtual time is exact in nanoseconds, but the guest reads it through a **divided counter**, and that
division is the instrument's resolution:

| ISA | counter | frequency | one counter tick |
|---|---|---|---|
| aarch64 | `CNTVCT_EL0` | 62.5 MHz | **16 instructions** |
| riscv64 | `rdtime` | 10 MHz | **100 instructions** |

The conversion is exact only because both frequencies divide a gigahertz; `icount.rs` asserts that
rather than assuming it, so a board whose counter did not would fail loudly instead of quietly
printing rounded numbers.

**Resolution is not the same as noise, and the difference is the useful part.** The aarch64 numbers
are identical across 64 consecutive ticks, so a change of 32 instructions in the handler moves the
reported number by exactly 2 counter ticks and cannot be mistaken for anything. This is a very
different instrument from the `bench --check` tripwire, which compares *whole-binary* counts across
*different builds* and is honest about ±5% codegen drift between them.

### The pricing question milestone 106 could not answer

`notes/timed-wait.md` (the pricing lane for milestone 51's deadline fork) wanted "a dedicated
`--features bench` probe under `-icount shift=0`" and could not build one, so it priced the
deadline check by **reading disassembly** instead: +30 instructions on aarch64 and +31 on riscv64 in
`sched::on_tick`, against a whole-tick path of ~491 and ~400 static instructions. Its own words:
"Nothing was measured under icount, because the icount tripwire's own note records ±5% codegen drift
between binaries, which swamps a 30-instruction change: the disassembly is the finer instrument
here, not the coarser one."

That was true of the tripwire and it is not true of this. The quantity here is a **localized span
inside one run**, not two binaries' totals, and it has no variance at all:

- **aarch64: yes, and it is measured rather than argued.** Injecting exactly 200 instructions into
  the handler moved the reported number by 208, on every one of the 64 ticks, with the residual
  smaller than one counter tick (the table above). A +30 change is therefore two counter ticks of
  movement on a number that does not otherwise move at all. Build with the prototype, build without
  it, run `script/icount --arch aarch64` twice, and the difference is the answer.
- **riscv64: not at this resolution.** One counter tick is 100 instructions, so a +31 change is
  visible only as an occasional single-tick step, and the honest reading is "under 100 and not zero".
  The disassembly stays the finer instrument on that ISA.

The caveat both legs share: the measured span is the **timer handler**, so it answers "what does a
deadline check cost the tick" and not "what does it cost anything else".

### 4. The armed deadline advanced by exactly one interval per delivered tick

The re-arm law, and it is here because **claims 1 to 3 are blind to a violation of it.** Milestone
62 injected the defect the aarch64 timer's module header has warned about since milestone 6,
re-anchoring the grid from `now` inside `rearm` rather than advancing it from the deadline that
fired: the bug that made 100 Hz configured into about 70 Hz delivered. `script/test` went red.
**This instrument went green with every number byte-identical to a clean run** (arrival min/mean/max
1008, handler 1056, `missed_ticks 0`).

The reason is claim 1's own subject. It compares each arrival against *the deadline that fired*, so
a kernel that re-anchors the whole grid arms the timer with the very word it records and satisfies
the comparison on every tick forever. Claim 2's span starts at the same place, and claim 3 counts
misses, of which re-anchoring produces none. Nothing moves.

Here the law needs **no retry loop at all**, which is claim 3 paying for itself. The suite's twin
must find a window in which `MISSED_TICKS` did not move, because a miss re-anchors the grid on
purpose, and on a loaded host it sometimes cannot find one in eight tries; that is the whole of
milestone 62's disposition of it. Virtual time has no deschedules, `missed == 0` is asserted a few
lines above, and the window is therefore miss-free by assertion.

| | clean | drift injected |
|---|---|---|
| aarch64, `deadline_delta` over 64 ticks | 40,000,000 of 40,000,000 | **40,004,032**, red |
| riscv64, `deadline_delta` over 64 ticks | 6,400,000 of 6,400,000 | **6,400,231**, red |

The excess prices the defect rather than merely detecting it: 4,032 counter ticks over 64 is 63 per
tick, or 1,008 instructions, which is exactly the arrival latency the same run printed. On riscv64,
231 over 64 is 3.6 counter ticks, or ~360 instructions, against a printed arrival of 300 to 400.

## The injections, and what each one settled

Every injection was reverted. Recorded in full, including the expectation they refuted, because a
failed prediction is how the useful facts arrived here as well.

### Two implementations of the defect claim 1 names

The block names the residual exactly: *"an implementation that maintains `DEADLINE` correctly and
arms SBI with something else"*. Two were written, both one line in `rearm`, both keeping the grid
store untouched.

| injection | what it is | `script/icount --arch riscv64` | the riscv64 leg of `script/test` |
|---|---|---|---|
| `sbi_set_timer(now + interval())` | re-anchor on every tick, the classic drift bug, with the grid still kept correctly beside it | **red**: arrival 420,400 instructions against a bound of 1,500 | **red**, at `the_handler_keeps_up_when_no_lock_is_held` |
| `sbi_set_timer(next + interval() / 4)` | a *fixed* quarter-period offset: no drift, no accumulation, the delivered rate still exactly 100 Hz | **red**: arrival 2,500,400 instructions, which is the injected 25,000 counter ticks plus the usual ~400, on every one of the 64 ticks | **red**, at `ticks_arrive_at_the_configured_rate` |

**The expectation was that the suite would not notice, and it did.** That is worth stating plainly
because it was written down as a prediction first. What it did not do is say what was wrong:

- the first one failed as *"the timer handler is taking longer than a whole tick period, with no lock
  held... Late by less than one interval means the handler itself is slow, which is this kernel's
  bug."* The handler is not slow. It is the assertion that broke #204, #210 and #215 on aarch64 in a
  single afternoon, so a reader triaging that red run has to decide "known or real" about the exact
  assertion this milestone was raised over.
- the second failed as *"no miss-free measurement window in eight tries: either the host is too
  contended to observe the grid, or the handler is slower than a whole tick period"*. The first half
  of that sentence is an invitation to re-run and move on.

The icount message names the defect: *"either the trap path grew, or the timer was armed with
something other than the deadline the kernel recorded"*, on a number with no host term in it.

**So the instrument's value is diagnostic certainty rather than detection**, and that is the
milestone's own thesis rather than a consolation. The block's cost line is "every red check in this
repository currently needs a human to decide 'known or real', and on 2026-08-03 that judgement was
made at least six times and got the wrong answer twice." Both of these injections produce exactly
that judgement call on the test path and none of it here.

### A known instruction count, to measure the resolution

The third injection asks what this instrument can see rather than whether it fires: exactly 200
instructions added to the top of the aarch64 `tick`, via the same assembly loop the calibration
uses.

| | arrival | handler |
|---|---|---|
| clean | 1,008 | 1,056 |
| +200 instructions injected | 1,216 | 1,264 |
| **delta** | **+208** | **+208** |

Both numbers moved by the same amount, on every one of the 64 ticks, and the eight-instruction
residual over the 200 injected is the operand setup around the loop: **smaller than one counter
tick.** That is the resolution claim measured rather than reasoned, and it is what makes the
paragraph below about milestone 106 a statement of fact.

## The calibration, and why a boot refuses to measure without it

The failure this repository keeps writing comments about is the **manufactured fact**: a variable
set to a missing file, a device that was not attached, a flag that silently did nothing. Booted
without `-icount shift=0` this kernel runs perfectly well, and every number above would become a
wall-clock number wearing an instruction's units. That is worse than an error, because it looks like
a measurement.

So the boot proves it is on the instrument before it measures anything with it. The arch layer runs
a loop of a known instruction count (`subs`/`b.ne` on aarch64, `addi`/`bnez` on riscv64, two
instructions per iteration, written in assembly precisely so the expected count is known rather than
whatever the optimizer decided this week), and the virtual clock must agree to within 1%:

| leg | expected instructions | virtual time observed |
|---|---|---|
| aarch64 | 2,000,000 | 2,000,032 ns |
| riscv64 | 2,000,000 | 2,000,100 ns |

Those thirty-two and one hundred nanoseconds are the operand setup either side of the loop and the
counter's own quantization. The 1% tolerance is nowhere near wide enough to admit either of the
other two ways this kernel is ever run: plain TCG retires far fewer than one instruction per
nanosecond of wall clock, and HVF on this host retires several.

The assembly lives under `arch/` rather than in `icount.rs` because DECISIONS §3 puts every `asm!`
there.

## BUGS

- **The riscv64 resolution is 100 instructions, and that is coarse enough to matter.** A change
  smaller than one `rdtime` tick is invisible on that leg except as an occasional single-tick step in
  the max. Nothing in the kernel can fix this: the counter's frequency is the machine's. A finer
  probe would have to read `rdcycle`/`rdinstret`, which QEMU does map to the instruction count under
  icount but which S-mode may only read when the firmware permits it (`mcounteren`), so it is a
  dependency on OpenSBI's configuration rather than on the architecture. **Not tried here.**
- **The bounds are ceilings and a regression under them is invisible.** A change that adds 300
  instructions to the aarch64 handler passes, because the bound is 2,000 and the number is 1,008.
  That is the deliberate trade against codegen drift: an exact baseline here would be a second
  `bench --check` with all of that instrument's re-save churn and none of its coverage. If the
  numbers ever need to be tracked rather than bounded, a baseline file is the obvious next step and
  the run already prints everything one would contain.
- **Only the timer handler is instrumented.** The two claims this milestone was left holding are both
  about the timer, so that is what got hooks, and the file is deliberately not a framework. Any other
  path wanting an instruction-denominated claim needs its own `tick_trace`-shaped recording, which is
  three relaxed counters and a call site.
- **The measured handler is the shipping handler, not the tested one.** The `icount` build is not a
  test build, so the `#[cfg(test)]` watchdog feed and `miss_detail` recording are absent from the
  numbers above. That is the right subject for a claim about what the handler costs, and it does mean
  the figure is a few instructions below what the suite's own handler executes.
- **`-smp 1`, always.** Under `-icount` all vCPUs share one virtual clock and an idle secondary
  parked in `wfi` jumps that clock forward to the next event, so a multi-hart timer measurement is
  measuring the other harts' idle jumps. This is the same constraint the bench instrument carries and
  the reason the placement probe stays on the wall clock (`notes/load-sensitive-assertions.md`).
- **The instrument is a boot mode, so it does not run under `script/test` or `script/gates`.** It is
  wired into CI beside the bench tripwire, which shares its QEMU cache and its path filter. A
  developer who never runs `script/icount` locally will find out in CI rather than before pushing,
  which is the cost of not putting `-icount` on the test path.
- **The failure path had to be bounded on the host, and finding that out leaked two emulators.**
  The guest's panic handler halts rather than exiting, so a violated claim prints its message and
  then the pipe never closes: `xtask` blocked on the read forever and QEMU stayed alive at ~80% of a
  core, twice, on a laptop already carrying four other lanes' gates. Counting "a few more lines"
  after the panic was the first fix and it hung for the same reason, one line further on. The
  mechanism now is a watchdog thread with a 300 s deadline that a seen panic shortens to three
  seconds, and killing QEMU is what closes the pipe and ends the read. `run_bench` has the same
  shape and the same exposure, and was not changed here.
- **Numbers are from one QEMU.** icount counts guest instructions, so the emulator's version is part
  of what they mean, exactly as `bench/baseline-*.txt` records for itself. `script/qemu-check` warns
  when the QEMU on PATH is not the pinned one.

## See also

- design/roadmap/78-load-sensitive-assertions.md: the milestone, and the day's evidence
- notes/load-sensitive-assertions.md: the five rounds, and both places the two claims were deferred
  to this instrument
- notes/benchmarks.md: the other icount consumer, why it is `-smp 1`, and the ±5% codegen drift
- notes/timed-wait.md: milestone 106's pricing lane, whose measurement this answers
- notes/cpu-models.md: the load-sensitivity evidence, including the control model failing
