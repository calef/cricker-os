# The CPU-model matrix

Every RISC-V result this project had before 2026-08-01 was taken on one emulated CPU:
`qemu-system-riscv64 -machine virt -cpu rv64`. `rv64` is QEMU's **maximalist** model. It turns on
essentially every ratified extension QEMU implements, so the machine we tested against had never
told the kernel no about anything.

The VisionFive 2's JH7110 is a SiFive U74, which is **RV64GC**. It is a much smaller machine. This
note is what happened when we ran the same suite against the narrow models, and what that does and
does not prove.

Milestone 59 built it. The question that started it was calef's, on 2026-08-01: should we modify
QEMU to match the chip? No. A forked emulator is a machine that exists nowhere, so it proves nothing
about the real chip and nothing about the standard emulator, and we pin QEMU (`.qemu-version`, and
CI builds it from source) for benchmark determinism, so a fork multiplies that maintenance. QEMU
already lets us **narrow** with `-cpu`, and narrowing is the whole idea.

## How to run it

```
script/cpu-matrix                      # every model in the default list
script/cpu-matrix sifive-u54           # just these
script/test --arch riscv64 --cpu sifive-u54    # one model, by hand
NIFE_CPU=sifive-u54 cargo run -p kernel --target riscv64imac-unknown-none-elf
```

`--cpu` reaches the QEMU runners as `NIFE_CPU`, which both `scripts/qemu-runner-riscv64.sh` and
`scripts/qemu-runner-aarch64.sh` read. Unset, they use what they always used: `rv64` on riscv64,
`cortex-a72` on aarch64. Nothing that existed before this flag changed its meaning.

`--arch aarch64|riscv64` runs one ISA leg instead of both, which is what makes the matrix cheap
(about 50 seconds per model on a warm tree, against about ten minutes for a full `script/test`).
**`--arch` did not exist before this milestone**, despite the brief saying it did; `cargo xtask test`
ran both legs unconditionally. Its default is still both, so the parity gate (DECISIONS §19) cannot
be weakened by forgetting to pass it.

Under HVF (`--hvf`) there is no CPU model to pick: the guest runs the physical Apple core, so
`-cpu host` is mandatory. `NIFE_CPU` set to anything else there is a hard error rather than a
value silently ignored.

## The result: the suite passes on all five models

2026-08-01, QEMU 11.0.2 (the pin in `.qemu-version`), macOS on Apple Silicon, load average 4.5
falling to 2.9 across the run, about four minutes for all five. **211 kernel tests, 211 passed, on
every model.** Each row was produced by exactly the command shown.

| model | `script/test --arch riscv64 --cpu <model>` | `riscv,isa` the machine advertises |
|---|---|---|
| `rv64` | 211 passed | `rv64imafdch_...` (42 named extensions after the base, incl. `svadu`, `sstc`, `zba`/`zbb`/`zbc`/`zbs`, and `h`) |
| `sifive-u54` | 211 passed | `rv64imafdc_zicntr_zicsr_zifencei_zihpm_sdtrig` |
| `rva22s64` | 211 passed | `rv64imafdcb_...` (RVA22 S profile, `svade`, `svinval`, `svpbmt`) |
| `rva23s64` | 211 passed | `rv64imafdcbvh_...` (RVA23 S profile, vector, `zicond`, pointer masking) |
| `thead-c906` | 211 passed | `rv64imafdc_..._xtheadba_xtheadbb_xtheadbs_xtheadcmo_...` |

The `riscv,isa` column is QEMU's own, and reproducible without booting anything:

```
$ qemu-system-riscv64 -machine virt,dumpdtb=u54.dtb -cpu sifive-u54 -bios none -display none
$ strings u54.dtb | grep '^rv64i.'
rv64imafdc_zicntr_zicsr_zifencei_zihpm_sdtrig
$ strings u54.dtb | grep 'riscv,sv'
riscv,sv39
```

So the reassuring thing the roadmap predicted is true, and now it is measured rather than argued:
**we are already portable to the board's ISA.** `sifive-u54` advertises RV64GC and nothing else, and
the whole suite runs on it, userspace and virtio and the IOMMU included.

Two things make that result mean something rather than nothing.

**We build for `riscv64imac`.** No `F`, no `D`. RV64GC is IMAFDC, so the compiler is already
forbidden from emitting an instruction the U74 lacks. That was true before this milestone and it is
why the milestone was expected to be cheap. What it never covered is the hand-written part:
the `asm!` in `kernel/src/arch/riscv64/`, and the CSRs, which QEMU may accept more permissively than
SiFive does. That is the part the matrix actually exercises.

**The narrowing is enforced, not just advertised.** See the preflight below.

## The preflight, and why the matrix would otherwise be theatre

QEMU's `virt` machine writes a `riscv,isa` string into the device tree per CPU model. That string is
a **claim**. If a future QEMU kept the claim but stopped trapping instructions the model does not
have, all five runs would still go green while proving nothing at all, and nothing else in the
matrix would notice. This project has been bitten by exactly that shape twice: `script/fmt` accepted
`--check` and ignored it for months, and the hand-maintained test list in `xtask` quietly covered 82
fewer tests than it claimed.

So `script/cpu-matrix` measures it before it runs anything. Five instructions at `0x8000_0000`,
entered in M-mode with `-bios none`:

```
auipc  t0, 0            t0 = 0x8000_0000
addi   t0, t0, 16       t0 = the wfi below
csrw   mtvec, t0        so a trap lands somewhere harmless
sh1add a1, a0, a0       0x20a525b3, a Zba instruction
wfi                     the landing pad, for the trap and the fall-through alike
```

`-d int` then logs the exception, or does not:

```
$ cat target/cpu-matrix/preflight-sifive-u54.log
riscv_cpu_do_interrupt: hart:0, async:0, cause:0000000000000002, epc:0x000000008000000c,
                        tval:0x0000000020a525b3, desc=illegal_instruction
$ cat target/cpu-matrix/preflight-rv64.log
(empty)
```

`rv64` executes it, `sifive-u54` refuses it. The assertion is **two-sided** on purpose: a check that
only looked for the trap would pass silently if the log format ever changed, which is the failure it
exists to catch.

Pointing `mtvec` at the `wfi` first is not decoration. Without it the trap goes to `mtvec = 0`, the
fetch there faults, and the fault storm writes **200 MB of log in three seconds**. With it the log is
one line, and `wfi` is a real vCPU halt, so QEMU sits at 0% host CPU either way (CLAUDE.md, "Never
leave QEMU running").

The check discriminates. Run against the other three models it reports `rva22s64` executed,
`rva23s64` executed, `thead-c906` **trapped**, which is both directions demonstrated and also a
reminder that the C906 is a genuinely different machine: it has no standard bitmanip at all, only
its own `xtheadba`/`xtheadbb`/`xtheadbs`.

## What the narrow models would have caught, and did not have to

Worth naming, because a green run is only interesting if you can say what it ruled out.

**Hardware A/D update (`svadu`).** `rv64` has it: the machine sets a page table entry's Accessed and
Dirty bits itself. `sifive-u54` and `thead-c906` do not have it at all, and `rva22s64`/`rva23s64`
advertise `svade`, its opposite (the machine faults and software must set them). A kernel that left
A and D clear would work on `rv64` and page-fault forever on the board. `crates/paging/src/sv39.rs`
sets both eagerly, with a comment saying why, so this was already closed; the matrix is what turns
"we think we handled that" into a machine that would have punished us and did not.

**Sv57 versus Sv39.** `rv64` advertises `mmu-type = riscv,sv57`. Every narrow model advertises
`riscv,sv39`, which is what the U74 has. We run Sv39 on all of them.

**The `sstc` extension**, which `rv64` and `rva23s64` have and `sifive-u54` does not: it puts the
timer compare in a CSR (`stimecmp`) instead of behind an SBI call. `arch/riscv64/timer.rs` goes
through SBI, so it works on both.

## BUGS

- **`sifive-u54` in QEMU is still QEMU.** It will not reproduce the JH7110's cache behaviour, its
  real memory map, or its errata. This catches the ISA-and-CSR class of bug and is not a substitute
  for the board. Nothing in this note should be read as "the VisionFive 2 will boot."

- **A green matrix is not a portable kernel.** It is the absence of one specific class of failure.

- **`the_canary_reports_a_byte_that_changed_behind_its_back` flaked on `thead-c906`, and the
  model was innocent: the flake was a race in the canary's own single-flight protocol** (observed
  2026-08-15, four runs of one tree: two failures with the flipped byte uncounted, two passes;
  diagnosed and fixed the same day). The canary's `check()` was single-flight behind an
  `IN_CHECK` flag and returned silently when it lost the compare-exchange. Timer ticks on other
  harts call `check()` too (secondaries are online in the suite), so the test's decisive check
  could lose the slot to a tick's pass that had read the scratch byte *before* the flip; the
  test's call then checked nothing, and the flip went uncounted. Nothing in that is c906-specific
  beyond timing; this model, the matrix's slowest, is merely where the window landed twice. The
  suite's logs show the benign half of the same interleaving routinely on every model and both
  ISAs: a tick's pass absorbing the flip (the `0xa5 -> 0x5a` canary line) before the test's own
  check runs. The fix (crates/canary_gate, loom-searched): the serialization is one state word
  with exclusive guards, `check()` reports whether a pass actually ran, the tick shrugs at a
  refusal, and the test loops until a pass of its own completes. The rework also closed a second
  hole the old spelling permitted and loom falsifies (an `arm()` could rewrite the watch plan
  under a checker that had seen `ARMED` but not yet won `IN_CHECK`, a torn-plan wild read),
  which no observed failure required but a corruption instrument must not contain.

  Two CI failures were briefly attributed to this bug and are NOT it, recorded here because they
  were sighted through this entry: an aarch64 kernel-suite death (PR #204's branch, run
  31907966383 attempt 1) read as "same-EL data abort just after the canary line", and a c906
  matrix death (milestone 54's branch, run 31910308865 attempt 1) at the riscv trap reporter.
  The logs acquit the canary: both are the kernel's own **stack-overflow report**, a store into a
  THREAD stack guard page with `sp` 4096 bytes past the bottom of a 16 KiB stack, both during
  `supervision_tests::a_faulting_child_reports_to_its_supervisor_and_is_reaped_then_respawned`,
  right after the user-fault kill report (slot 87 on aarch64, slot 102 on c906; on aarch64 the
  canary had disarmed 21 seconds earlier). That is one real, separate bug, seen on both ISAs on
  slow hosts; it wants its own lane.

- **The 2026-08-15 attempt-1 deaths on loaded runners were a real kernel thread-stack overflow,
  since diagnosed and fixed** (aarch64 run 31907966383 and c906 run 31910308865, both during
  `supervision_tests::a_faulting_child_reports_to_its_supervisor_and_is_reaped_then_respawned`,
  both a store into a THREAD stack guard page with `sp` 4096 bytes past the bottom of a 16 KiB
  stack). Symbolized against bit-identical rebuilds of CI's binaries: no frame outran the guard;
  the sum of a deep standing path, a blocked thread's residue, and a preemption landing at the
  deepest instant simply exceeded 16 KiB, and a loaded host multiplies preemptions until one
  lands there. Fixed 2026-08-15: thread stacks are 24 KiB, the tick path's disarmed canary check
  no longer bills a 592-byte frame, the thread high-water tripwire is sized against worst-case
  stacking, and a thread-guard fault now prints the dead stack's `.text`-pointing words so the
  next such report symbolizes itself. The full analysis is in notes/stack.md; the load-legibility
  rule stands, but this failure was never a false one.

  One c906-specific residue: the c906 report's `sepc` named an `auipc`, an instruction that
  cannot raise a store fault, while `stval` told the truth. On this model treat `sepc` in this
  failure class as approximate; the aarch64 twin of the same fault carried exact state.

- **The ASID probe does not vary across models, so the one test written *for the board* is still
  untested.** Every model above printed `satp.ASID: 16 bits implemented`, including `sifive-u54`.
  QEMU does not model a reduced `satp.ASID` width per CPU. `satp.ASID` is WARL and RISC-V permits an
  implementation to hardwire all of it to zero, which is the cheap option for a small core;
  `the_hardware_has_at_least_the_asid_bits_the_allocator_assumes` in `arch/riscv64/mmu.rs` exists
  precisely because the U74 has not been checked. **No `-cpu` value available to us exercises its
  failing branch.** The board will be the first machine that can. Until then the unconditional
  `sfence.vma` in `write_satp` is what keeps address spaces from aliasing, and it stays.

- **The matrix runs riscv64 only.** `NIFE_CPU` works on the aarch64 runner too, and nothing uses
  it: QEMU's aarch64 models are not a live question for this project the way the RISC-V ones are,
  because there is no aarch64 board arriving. If a Pi 4 port happens, `-cpu cortex-a72` versus
  `cortex-a53` is the same exercise and the mechanism is already there.

- **Vendor extensions are advertised, not exercised.** `thead-c906` passing says our code does not
  trip over a machine that lacks standard extensions. It says nothing about the C906's non-standard
  page-table attribute bits, which QEMU models behind `xtheadmae` and which the `virt` machine does
  not turn on.

- **Five models is a sample, not a proof.** `qemu-system-riscv64 -cpu help` lists 26. The five here
  were picked for a reason (see the comment block in `script/cpu-matrix`), and a sixth that broke
  something would be a better result than these five passing.

- **The matrix inherits the suite's load sensitivity, and multiplies its exposure by five.** Several
  kernel tests assert against wall clock: `a_thread_that_never_yields_is_preempted_anyway` gave the
  polite thread one second, `the_handler_keeps_up_when_no_lock_is_held` counts missed ticks,
  `a_finished_thread_is_reaped_and_its_memory_returned` waits on the reaper. A busy host fails them.

  *(Two of those three have since been re-aimed: the reaper test in milestone 78's first round and
  the preemption test in its second, which replaced the one-second deadline with a budget of 200
  delivered ticks. `the_handler_keeps_up_when_no_lock_is_held` is the one that stays, because it
  cannot be re-aimed on this instrument; see notes/load-sensitive-assertions.md.)*

  Two runs in this milestone did exactly that, and both were worth chasing rather than shrugging at,
  because the whole point of the matrix is that a model-specific failure is real news. **Neither was
  model-specific**, and the evidence is worth keeping:

  - `rva23s64` failed the preemption test at load average 4.0. Re-run quiet, it passed four times out
    of four.
  - `rva22s64` failed the reaper test during a Time Machine backup (load average 7.5 with the CPU 92%
    idle, which is the tell: `backupd` was in uninterruptible I/O wait, so the load average was
    measuring disk, not CPU). Re-run once the backup finished, the whole matrix went green.
  - Under eight spinning processes on an eight-core host, **`rv64` failed too**, at
    `arch/riscv64/timer.rs`'s missed-tick assertion, and `rva23s64` failed at two other wall-clock
    assertions (`smp.rs:258`, `sched.rs:2611`).

  **The control failing is what settles it**: `rv64` is the model every RISC-V result in this tree
  was taken on, so if it fails under load then load is the variable. Across the milestone, 28
  riscv64 legs on an unloaded host produced 2 failures, both while something else was using the
  machine; 4 legs under deliberately induced load produced 3, including the control.

  So CLAUDE.md's rule applies here with force. **Load causes false failures, not false passes**, so a
  green matrix under load is conclusive and a red one is not. Before you diagnose a model, re-run it
  quiet, and check `top` rather than only the load average. The CI job is five sequential QEMU runs
  where `test` does one, so it is five times the existing exposure to a noisy runner rather than a
  new kind of risk.

  **The converse of that rule is a tool, and it went unused for a month.** "A green matrix under
  load is conclusive" also means loading the host on purpose is the cheapest way to *find* a
  load-sensitive assertion, which is a different question from finding a model-specific bug. The
  recipe is in notes/load-sensitive-assertions.md; on its first use it turned up two assertions in
  one run that a month of CI had not.

  An earlier version of this paragraph ended "the fix is those tests' timing budgets", and that was
  wrong for most of the family: several of these assertions failed with counts *below* their
  baselines, which no timing budget explains and no widening fixes. Milestone 78 rescoped them; the
  per-assertion verdicts and the deficit-versus-surplus diagnostic are in
  notes/load-sensitive-assertions.md. Dropping a model is still not the fix.

  **This job then became a merge blocker, on 2026-08-04, and that is what finished the family.**
  Three sites failed across four models in a handful of runs, on pull requests whose diffs could
  not reach them (an `xargs` change failed a timer assertion): `arch/riscv64/timer.rs`'s masking
  test on `rv64`, the control; `smp.rs`'s placement probe on `rva23s64` and `thead-c906`;
  `sched.rs`'s preemption test on `sifive-u54`. None was a timing budget either. Each measured
  across instructions outside the property, and contention widened the window until a race that had
  never lost started losing. The second round's verdicts are in the same note. **One of them was not
  load sensitivity at all**: the placement probe was waiting on a condition the scheduler could not
  reach once every core was busy, so its 60 s budget was reporting a wedge as a timeout.

## Where it sits in CI

**Its own job (`cpu-matrix` in `.github/workflows/ci.yml`), not extra work inside `script/test`.**
The matrix is the same riscv64 suite five times over, so folding it into the main gate would make
every run four times longer for a check whose whole point is that it rarely changes anything. On its
own runner it costs nothing a developer waits for.

It runs on every push and pull request rather than nightly. The change that breaks this is a change
to `kernel/src/arch/riscv64/`, which arrives in a pull request, and a nightly would report the
failure a day after the merge that caused it.
