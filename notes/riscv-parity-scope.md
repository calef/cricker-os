# Scoping RISC-V / aarch64 feature parity

The RISC-V port proved the **capability core** on a second ISA: boot, MMU (Sv39), traps, the SBI
timer, the scheduler, preemption, U-mode programs and syscalls, capability invocation, IPC,
userspace-built processes, and device interrupts serviced by an unprivileged userspace driver. Rule
#1 held: a new ISA was a new `arch/` directory, not a diff across the kernel.

aarch64 is a strict **superset**. This note scopes the gap: what it would take to bring RISC-V to
parity, what each item proves, and in what order to do them.

A key framing runs through the whole list: **the demonstrator's thesis is about the kernel** (a
verified, portable capability microkernel). So the parity items that prove *kernel* properties on the
second arch are worth more than the ones that only port *userspace* apps.

Effort is session-sized: **S** = part of a session, **M** = one to two sessions, **L** = several.

---

## The gaps

### A. SMP (multi-hart): DONE.

RISC-V runs on all four harts, and the SMP test suite passes (riscv64 55, aarch64 116). Built in
four steps: **A1** the per-hart trap state (`sscratch` points at a per-hart `TrapStash` holding the
kernel `tp` and stack, replacing the single global that could not scale past one hart); **A2**
secondary bring-up via SBI HSM `sbi_hart_start` into a `secondary_boot` that replays the higher-half
transition, made robust to QEMU's non-deterministic boot hart by keying everything to
`arch::boot_cpu_id()` (logical id == hart id); **A3** IPIs via the SBI IPI extension (a supervisor
software interrupt, `scause` = 1, draining the inbox), lighting up `send_reschedule`; **A4** the SMP
tests un-gated and generalized off "boot core is 0". The subtle one was **TLB shootdown**: RISC-V has
no hardware TLB broadcast, so `flush_tlb` follows its local `sfence.vma` with an SBI RFENCE to the
other online harts, or a thread migrated to a core faults on a stale translation of its own stack.
Original scoping below.

### A (original scope). SMP (multi-hart): L, high risk. The only pure-kernel gap.

RISC-V runs single-hart today; `send_reschedule` is a no-op and the runner passes `-smp 1`. This is
the last big *primitive* aarch64 claims and riscv does not, and the only parity item that is genuine
new kernel work rather than porting userspace.

- **Prerequisite: per-hart trap state.** The leak-free `tp` fix uses a single global `KERNEL_TP`,
  and the trap frame is placed below `sp`. SMP needs each hart to have its own per-CPU pointer and
  trap-frame area: the standard approach is `sscratch` holding this hart's kernel context, swapped in
  at trap entry, set up per-hart at bring-up. This refactor touches the trap path (trap.s) and is the
  enabling step; it was flagged as a follow-up during the `tp` saga.
- **Secondary bring-up** via SBI HSM (`sbi_hart_start(hartid, addr, opaque)`): the boot hart starts
  the others into a secondary entry path that mirrors aarch64's `secondary_main`: set `stvec`, `tp`/
  `sscratch`, adopt the kernel `satp`, arm the timer (`sie.STIE`), create an idle thread and run
  queue, become a scheduler participant.
- **Per-hart PLIC context.** `plic::init` hardcodes context 1 (hart 0 S-mode). Each hart's context
  is `2*hart + 1` and needs its own threshold and enable bits; wire this into `arch::irq::init_this_cpu`
  (a no-op today).
- **IPIs** via the SBI IPI extension (`sbi_send_ipi`) → supervisor software interrupt (`scause` = 1)
  → drain inbox + reschedule, the riscv twin of the `RESCHED_SGI` path. This lights up the
  `send_reschedule` no-op.
- **Cross-hart shootdown**: `sfence.vma` / `fence.i` IPIs for TLB and icache coherence.
- **Proves:** the scheduler and capability model are SMP-safe on a second *weakly-ordered* ISA. The
  weak-memory discipline (built for ARM) should carry over; SMP is where it gets its second witness.

### B. In-kernel test suite on RISC-V: DONE.

RISC-V now boots the kernel test harness and passes **51 portable tests** (`cargo xtask test` runs
both arches: aarch64 116, riscv64 51). The gap is the aarch64-specific tests, gated off riscv: the
userspace-exec suite in `kernel/src/user/tests.rs` (37 tests driving hand-written aarch64 programs through `exec`), the
SMP tests (workstream A), and the two SGI interrupt tests. Three real things surfaced: the
`sifive_test` finisher (`0x10_0000`) had to be mapped device-typed and reached through the direct map
(the boot tour halts via `wfi` and never exercised `semihosting::exit` under paging); the timer
watchdog needed wiring into riscv `timer::tick`; and a genuine race: a timer tick landing inside
`sched::init` ran the deferred `schedule()` before the idle thread was registered ("nothing runnable
and no idle thread"), fixed by masking interrupts across init, which aarch64 gets for free by bringing
the scheduler up before enabling interrupts. Original scoping below.

**Follow-on (milestone 19): the suite was at parity on the portable tests and not on the arch ones.**
`arch/aarch64` carried 21 unit tests and `arch/riscv64` carried zero, across the same three files, so
the properties that differ between the ISAs were asserted on one side only. The twins are now
written, and they found three defects nothing else would have: the timer delivering 80 Hz against a
configured 100 (the relative-re-arm drift bug aarch64 shipped and fixed back at milestone 5, arrived
at here by a different route), `missed_ticks` a stub returning 0, and a single global tick counter
for a per-hart timer. **The lesson is B's own lesson repeated one level down:** a parity claim is only as
wide as the suite that checks it, and "both ISAs run the same suite" was true of the portable half
and not of the arch half. See notes/riscv-arch-tests.md, which also sizes the remaining
`kernel::user::tests` port.

**Follow-on (milestone 19, 2026-07-31): the last two gated portable tests are gated no longer.**
`kernel::sched::tests::an_interrupt_becomes_a_message` and
`an_interrupt_that_arrives_before_the_wait_is_not_lost` were the "two SGI interrupt tests" named
above. Neither property is architectural (IRQ-to-IPC delivery, and a lost-wakeup race); only the
*trigger* was, and it is now behind three per-arch functions in the test module. RISC-V raises the
console UART's own transmit-empty line into the PLIC, because it has no SGI and its two other
candidates are worse (the SBI IPI lands on the software-interrupt arm, which never reaches
`irq_route`; the PLIC's pending block is read-only, and QEMU ignores writes to it). The legs are
**not** twins in what the trigger costs, and notes/interrupts.md says which way each one is wider.
That leaves `kernel::sched::tests` fully portable, and the remaining aarch64-only tests are the ones
that genuinely need aarch64 machine code or a GIC.

**Follow-on (milestone 19, 2026-07-31): `kernel::user::tests` runs on both ISAs.** It was the last
whole module gated to one architecture, and the reason turned out not to be the tests. The module
comment said "every test drives a hand-written aarch64 program through `exec` and reads aarch64 fault
registers", which was true and was the wrong thing to fix: the *scaffolding* was aarch64, and the
tests came along unchanged once three things moved.

1. **A portable last-fault record.** `arch::UserFault` (Permission / Translation / Other, each
   carrying Read / Write / Fetch) plus the address, in place of two public `ESR`/`FAR` statics every
   test decoded inline. This is what keeps the assertion "a **permission** fault at exactly this
   address" rather than softening it to "a fault happened", which would have been a test converted
   into something that passes for the wrong reason. **The two ISAs are not symmetric here and the
   asymmetry is documented in the code:** aarch64 is *told* (`ESR_EL1`'s fault status code
   distinguishes permission from translation in silicon, at the instant of the fault); RISC-V is not
   (`scause` has one code per access kind and never says why the walk refused), so its classifier
   walks the tables the hardware just walked. That walk happens after the fault, which is a real gap
   and is written up as a `BUGS` note on `riscv64::exceptions::classify`. aarch64's answer is a
   measurement; RISC-V's is an inference.

2. **The hand-assembled programs became real ELFs.** Five `global_asm!` blobs (three aarch64, two
   RISC-V) are gone, along with `exec`, the one-page raw-machine-code loader they needed. Their
   behaviours are ordinary, so they are the `outlaw` binary (two roles: read a forbidden address,
   round-trip through user mode) and the `spinner` that §24's interrupt work already built. Every program the
   kernel runs now arrives as an ELF. The privilege-boundary test hands the forbidden *address* to
   the program in a register rather than baking a constant into machine code, which is the trick that
   makes one program serve two ISAs with different kernel address spaces.

3. **`hello` builds for RISC-V, and always could have.** It carries the milestone 7-19 role catalogue
   (the printing client, the untyped demo, the granter and receiver, the call server, the aspace
   builder, the init roles), and xtask's comment claimed it was "aarch64-wired". Three quarters of
   that claim was already false (console, input and shell were in the riscv build list directly
   below it) and the last quarter was six syscalls hand-rolled in aarch64 `asm!` naming x0/x2/x3/x4/x8,
   which on RISC-V are the zero register, sp, gp, tp and fp. `user_rt` had had portable versions of
   all six since 19f.6 lifted the runtime out; the duplicates simply never got deleted. A stale
   comment stood in for a real blocker for a year, which is this note's recurring lesson in a new
   costume.

**Three defects the port surfaced, all invisible until the tests ran on the second machine.**

- **init built the wrong program.** Several init roles in `hello` build a child out of *this
  binary's own* ELF and re-enter it at another role, and they found that ELF by reading the archive
  entry literally named `"init"`. Right on aarch64, where hello *is* the boot program; wrong on
  RISC-V, where `init` is the portable `builder` demo. init built a child out of `builder`, started
  it at a role `builder` does not have, and the child reached for an initrd mapping it did not own
  and died. Nothing said "wrong program": the test waiting on that child's report just never got one
  and the 90 s ceiling fired. `hello::ROLES_ENTRY` now names the entry per ISA, matching the kernel's
  `INIT_ROLES_ENTRY`, and the two must agree.

- **The fault record was published before it was written.** Every test that reads the last-fault
  record watches `USER_FAULTS` rise and then calls `last_user_fault()`, so the counter is the
  record's publication flag; it was being bumped *first*, on both ISAs, all relaxed. A reader
  therefore gets either an earlier fault's record (the assertion satisfied by the wrong evidence) or,
  on the boot's first fault, a zero that decodes as "nothing faulted". The tests only passed because
  something earlier in the suite had already faulted at the same address for the same reason. Fixed
  by storing the record first and making the counter's `fetch_add` the `Release`, with an `Acquire`
  fence in the accessor.

- **A reap wait that was really waiting for the whole machine.**
  `reclaim_frees_a_started_then_exited_childs_regions` waited for `thread_count()` to return to a
  baseline it sampled at the top of the test. `thread_count()` is the size of the *entire* thread
  table, and the top of the test is exactly when the previous tests' processes are still tearing
  down, so the baseline was a number the system would move on its own. It failed that way once on
  RISC-V, where the slower machine leaves more teardown in flight, and passed on a re-run, which is
  the signature of a wait written against something wider than the property. `sched::thread_present`
  asks whether *this* child was reaped. Third time for this shape: the wait was a yield count until
  §28's cross-core placement broke it, then a clock-bounded headcount until this. **Widening the
  timeout would have hidden it each time.**

**The assertions were broken on purpose to check they still bite** (a ported test that has never
failed is not evidence it still catches what the original caught). Four representative properties,
one per category, each broken in the kernel or in the user program and each confirmed red on
**both** ISAs before being restored:

| Category | What was broken | What happened, on both ISAs |
| --- | --- | --- |
| Fault assertions | `outlaw`'s `READ_KERNEL` reads an *unmapped* address instead of the kernel's | `a_user_program_cannot_read_a_kernel_address` fails on `Translation(Read)` versus `Permission(Read)`. This is the one worth having: it proves the permission-versus-translation distinction is load-bearing on RISC-V too, where it is *derived* by walking the tables rather than read out of a register. |
| ELF rejection | `paging::Mapper::map`'s `WrongHalf` guard disabled | `an_elf_that_asks_to_be_loaded_over_the_kernel_is_refused` fails with `left: None`: the load fully succeeded, mapping a user program on top of the kernel. |
| Capability rules | the `GRANT` check on `SEND_CAP` disabled | `a_capability_can_be_delegated_over_ipc_and_grant_gates_re_delegation` fails as a lost-wakeup hang, identically on both. The receiver's re-delegation is no longer refused, so it blocks in the send it expected to fail and its verdict never arrives. Red, but through the watchdog rather than the assertion. |
| `userspace_init_*` | hello's `child()` reports `CHILD_WORD ^ 1` | `userspace_init_parses_an_elf_and_builds_a_running_child` fails on the exact word. |

The first of those is also what found the publication-ordering defect above, because breaking a test
and running it *alone* puts it in a state the full suite never reaches.

**What stays aarch64-only, and why.** Each is written at the test, not in a blanket module comment,
because a blanket comment is exactly how the old claim survived past being true.

| Test | Reason |
| --- | --- |
| `el1_runs_on_sp_el1` | **No RISC-V analogue exists.** RISC-V does not bank `sp` by privilege level; there is one `sp`, swapped with `sscratch` on trap entry. The hazard (two names for one register, silently) cannot arise, so a twin would have nothing to assert. |
| ~~`asid_tagging_keeps_address_spaces_apart_without_flushes`~~ | **Closed by milestone 58; it runs on both ISAs now.** It was here because the property was not true on RISC-V: `write_satp` issued an unconditional `sfence.vma` on every root switch, so a twin would have read the right byte because everything was just flushed, not because the tagging works. The row stays, struck through, because the *reason* is the useful part: this is what a test that cannot fail for its stated reason looks like before anyone notices. |
| `the_hardware_says_el0_cannot_read_the_kernels_memory` | Twin exists: `riscv_virtio_tests::the_page_tables_say_u_mode_cannot_read_the_kernels_memory`. Kept separate on purpose, because the *mechanism* is the subject: aarch64 asks the silicon (`AT S1E0R`), RISC-V has no such instruction and walks in software. Merging them would assert only what both can say. |
| `userspace_init_delegates_an_interrupt_to_a_child` | RISC-V has no second interrupt to raise. Its only hand-assertable line is the console UART's, which `spawn_init` is already routing for the input driver, so a twin would prove delivery through whichever route was bound last rather than through the delegated capability. The property is covered by `riscv_virtio_tests::a_userspace_driver_reads_a_file_from_a_virtio_disk` (which asserts `ROUTED_IRQS` rises while a userspace driver waits on its own Irq cap) and by `sched::tests::an_interrupt_becomes_a_message`. |
| `userspace_init_builds_a_driver_that_reads_real_hardware` | The assertion is `0xB105F00D` in the PL011's PrimeCell identification registers, and RISC-V `virt` has no PL011. That constant is what makes the test exact rather than "the read did not fault"; substituting a virtio magic number would be a different test wearing this one's name. Device delegation to a userspace driver is proved on RISC-V by the virtio-blk driver test, which is a stronger version of the same claim. |
| 24 device / filesystem / network tests | **Twins already exist** in `riscv_virtio_tests`, which drives the same properties through the dedicated `blk` and `net_stack` binaries. Running both copies would double the suite's slowest tests (including the ~300 s `std_net`) to prove nothing new. This duplication is itself worth revisiting: see the open gap below. |

### Closed gap: RISC-V allocated ASIDs and then threw them away

`riscv64::mmu` did the whole ASID dance. `asid_bits()` probed the implemented width at boot,
`ttbr0_value` packed the ASID into `satp[59:44]`, `flush_asid` existed for teardown. And then
`write_satp` ended with a bare `sfence.vma`, which invalidates **everything**, on every address-space
switch. So the tagging cost what it cost and bought nothing: RISC-V was still swinging the
sledgehammer aarch64 put down at milestone 15.

Found while deciding whether `asid_tagging_keeps_address_spaces_apart_without_flushes` could be
ported. It could not, honestly, and that was the finding.

**Milestone 58 closed it**, and the sequencing is the lesson. The flush was not merely slow, it was
covering for two things: `flush_asid` was local, because `sfence.vma` does not broadcast and RISC-V
has no hardware equivalent of `tlbi aside1is`; and `satp.ASID` may be zero bits wide on conforming
hardware where aarch64 mandates eight, so `crates/asid`'s 255 numbers rest on an assumption that
holds on one ISA and not the other. So: the SBI RFENCE shootdown first, then the removal, gated on a
boot-time probe rather than on the specification. The aarch64 witness now runs on both ISAs, and
`an_asid_flush_reaches_the_other_cores` proves the broadcast half on both. The benchmark did **not**
improve, and cannot on an emulator that models no TLB-miss cost. See notes/riscv-tlb-shootdown.md.

### Open gap: `tests` and `riscv_virtio_tests` overlap by 24 tests

`riscv_virtio_tests` was written when `tests` was unreachable from RISC-V, so it re-derives the disk,
FS and network properties with its own copies of `wait_for`, the net selectors, and the transcript
plumbing. Now that `tests` runs on both, 24 of its tests are gated to aarch64 solely because the twin
exists.

The finishing pass measured the overlap rather than guessing at it, and the answer is more
encouraging than "not textually identical" suggested. Of the 24 shared names, **nine bodies are
byte-for-byte identical** (the five socket-contract tests, `std_net`, the two smoltcp DHCP tests, and
the FS server's stack-headroom check). Of the fifteen that differ, thirteen differ **only in which
image drives the driver**: aarch64 passes `init_image()`, because there the virtio driver is a role
of `hello`, and RISC-V passes `blk_image()`, the dedicated binary. The remaining two differ only in a
comment and in an assertion message. There is no behavioural divergence anywhere in the 24.

So the merge is smaller than it looked: pick `blk` on both ISAs (a dedicated binary is the better
choice regardless, and hello would keep its role for nothing), delete `riscv_virtio_tests`' 24 copies
along with its duplicate `wait_for`, net selectors and image helpers, and keep its two genuinely
RISC-V-only tests (`a_faulting_user_thread_is_killed_and_the_kernel_survives` and
`the_page_tables_say_u_mode_cannot_read_the_kernels_memory`, the software-walk twin of the `AT S1E0R`
test). **Still not done**, and deliberately: it doubles the aarch64 leg's slowest tests during the
transition and it is a change to what the *disk and network* tests prove, which is a different
subject from making the userspace suite portable. It is now a measured piece of work rather than an
estimated one.

### CLOSED 2026-08-02: `no_leaked_threads` has never policed `user::tests`

**Fixed, and the scope below turned out to be right on every point.** The gap is closed, the two
spinners are reaped, and the probe (now `thread_leak_police`, named to sort after `tests`) runs last
and is green on both ISAs: 216 aarch64, 215 riscv64.

Three things worth keeping from doing it, because the analysis below could not have predicted them:

- **The bug bit on CI before the fix landed**, on 2026-08-02, exactly as this section forecast and in
  the forecast's own words. `reclaim_frees_a_started_then_exited_childs_regions` ran **90 s against a
  budget it normally clears in under 5**, tripping the watchdog on a pull request that had touched
  only `dtb` and `crickerfs`. The starvation reaches a test on a branch that cannot have caused it,
  which is what made it look like a flake worth re-running. **It was not a flake.**
- **The probe was proven to bite before being believed.** Leaking the spinner on purpose fails it,
  `1 thread(s) are still runnable after the suite quiesced`, with the dump. A reordered probe that
  has never failed is not evidence it polices anything.
- **The free-frame shift this section flagged as the reason it needed a full run did not materialise.**
  Killing `untyped_demo` frees its frames, and every later baseline in the suite was expected to move;
  both ISAs pass unchanged. Recorded because the risk was real and correctly identified, and the
  measurement is what retires it rather than the argument.

The original analysis follows, unedited, because the plan it lays out is what was built.

---

The leak police sorts by module path, and `no_leaked_threads` sorts before `tests`. So the one module
whose whole subject is user threads has never been checked for leaving any behind, which is how four
never-exiting spinners accumulated in it unnoticed until one of them starved
`reclaim_frees_a_started_then_exited_childs_regions` off a four-hart machine.

**Measured, not estimated.** Moving the probe to run last (rename the module so it sorts after
`tests`) and having it report rather than assert gives, on the merged tree:

```
[PROBE] leaked runnable = 2, table = 87     # aarch64
[PROBE] leaked runnable = 2, table = 87     # riscv64
```

Identical on both legs. The two are `untyped_demo` (pc deep inside `hello`) and `spinner` (pc at its
entry, a tight loop), exactly the two named below. The other 85 threads in the table are **Blocked**,
which is the healthy steady state: they are the long-lived userspace servers earlier tests started,
waiting on endpoints. A thread dump full of Blocked user threads is not a leak, and reading it as one
sends the investigation to the wrong place.

Worth stating plainly, because the direction is counter-intuitive: **de-gating the module did not make
this worse in aggregate.** aarch64 carried **four** of these before (`spinner`, `untyped_demo`,
`printing_client`, `self_check_client`); making the two one-shot roles `exit()` cut it to two. RISC-V
went from zero to two, because the module did not run there at all. So RISC-V now sits in exactly the
condition aarch64 has been in for many milestones, rather than a worse one, and aarch64 improved.

Two of the four were one-shot roles with nothing left to do and now `exit()`. The other two cannot,
and that is the obstacle:

- `a_user_program_that_never_yields_is_preempted_anyway` runs `spinner`, whose entire point is that
  it never yields, never syscalls, and never returns. A thread that exits would not test anything.
- `a_process_spends_untyped_and_the_kernel_never_allocates` reads the kernel's free-frame count the
  instant its child reports. If the child exited there, the number read would be the teardown's
  rather than the measurement's, so the child spins to hold the state still.

Both are spawned **bare** (`sched::spawn` around `user::run`), not into a reclaimable region, so
there is no `reclaim_region` to arm the kill with and no other way for a test to tear down a user
thread it started. Making the module policeable therefore means giving the kernel a way to kill a
bare user thread by `Tid`, which is a kernel change with its own design questions, not a test
reordering. Recorded here rather than done under a test lane. Reordering the runner *without* that
change would simply turn a silent gap into a permanently red gate.

**What the change would be, precisely, so it can be scoped without rediscovering it.** The mechanism
already exists and is proven: `reap_region_objects` sets `t.killed = true` on every live thread in a
region and the scheduler converts a killed thread to a corpse at its next preemption (DECISIONS §16's
armed kill, the tier §24's `^C` escalation stands on). What is missing is only the ability to name
**one thread** instead of a region.

1. `sched::kill_thread(tid: Tid)`, test-support, roughly ten lines: take `SCHED`, resolve `tid`, set
   `killed = true`. No new syscall and no change to the user-visible surface (rule 3 is about the
   syscall boundary; this is an in-kernel function for in-kernel tests).
2. The two tests above kill their subject after asserting, and wait for `thread_present` to go false.
3. `no_leaked_threads` then moves to run last, and polices the module for the first time.

**The risk that makes it its own piece of work rather than a footnote:** killing `untyped_demo` frees
its frames, so every later free-frame baseline in the suite may shift. That is precisely the class of
change that needs a full run on both ISAs to believe, and it is why it does not belong bolted onto a
test-portability lane.

### CORRECTED 2026-08-02: most of this was a placement bug, not host load

**The section below is kept because its measurement is sound and its conclusion was wrong**, and the
way it was wrong is the useful part.

`every_secondary_runs_scheduled_work` was failing because each secondary's probe was spawned with
plain `sched::spawn`, which places by **§28's power of two choices**: the lighter of two randomly
sampled cores, deliberately not the spawner's. The probes scattered, and any core nobody sampled
never set its `RAN_ON` slot. **The test was waiting on a condition that could not become true**, and
passed only when the random placement happened to cover every core. Fixed by spawning the probe with
`spawn_on(cpu::id(), ..)`, which is what the test always meant.

**Why "host-load sensitive" was a believable wrong answer.** A random placement moves between runs
*exactly* the way a contended runner does. On 2026-08-02 it failed three pull requests in a row on
three different RISC-V CPU models, and that wandering was read as evidence about the host. It is
equally the signature of nondeterministic placement, and nothing in the failure distinguishes them.

**What distinguished them was a fix that did not work.** Widening the wait from 10 s to 60 s changed
nothing. A deadline cannot fix an unreachable condition, so the failure surviving a 6x wider bound
ruled out slowness in a way no amount of re-running could have. The corroboration was in the same
logs the whole time: `a_batch_of_cpu_bound_work_reaches_every_core` and `all_secondaries_came_online`
**pass** in the runs where this fails, so the cores are online and running work and only the per-core
attribution breaks.

**This is the second time §28 has invalidated a placement assumption in a test**, and both are in this
file. The reap wait above was a yield count until §28's cross-core placement broke it. The shape is
worth naming: a test that spawns a thread and then asserts something about *where* it ran is relying
on placement, and §28 made placement random. `sched::spawn` is now the wrong call in any test whose
subject is a particular core; `spawn_on` is the one that means what such a test says.

The load sensitivity below is **real and separately measured**, and the 60 s bound was kept for it.
It was the wrong explanation for this failure, not a wrong measurement.

---

### The suite's deadline tests are host-load sensitive, on `main`, today

Worth recording separately because it was nearly misattributed. `kernel::smp::tests` uses a ten-second
wall-clock `wait_for` and asserts that work placed on a core actually ran there. Under host
contention (several QEMU instances competing for eight cores, which is the normal condition when
concurrent lanes are running), the guest's vCPU threads are starved and the assertion fails:

```
run 4: FAIL secondary cores did not run scheduled work in time
TOTAL main: pass=5 fail=1
```

That is **unmodified `main`**, six runs under synthetic load. The same tests pass reliably on a quiet
machine. So a failure in `kernel::smp::tests` is evidence about the host before it is evidence about
the diff, and the control worth running first is *the same load against `main`*. These tests run
before `kernel::user::*`, so nothing that module leaks can reach them; the ordering alone rules out
the tempting explanation.

### B (original scope). In-kernel test suite on RISC-V: M, low-medium risk. Highest value per effort.

The 116 kernel tests boot under QEMU on aarch64 and signal pass/fail via semihosting exit. RISC-V has
no in-kernel test run. **The hard part is already done:** `arch::semihosting::exit` exists on riscv
(QEMU virt's test-finisher). What remains:

- `xtask test()` grows a riscv kernel-test build + boot (the `riscv64imac` target, the riscv runner,
  TCG for deterministic semihosting).
- Gate the arch-specific tests (the SGI-triggered interrupt tests) to aarch64; the rest test portable
  logic (scheduler, caps, page-table math) and should pass on riscv unchanged.
- **Proves:** the same verified behavior holds on both arches. This is the strongest *parity signal*
  there is, and it aligns with the verified-Rust thesis. Do it first: cheap, and it makes every later
  parity claim checkable on riscv.

### C. virtio-blk + on-disk filesystem: DONE, over BOTH transports; and the "blocked" record was WRONG (correction below).

**Correction (2026-07-27, evening).** The blocker recorded below does not reproduce. Booting the
riscv kernel with the runner's exact flags (QEMU 11.0.2, `-global virtio-mmio.force-legacy=false
-device virtio-blk-device`) finds a modern virtio-mmio block device at slot 7 (0x10008000, PLIC
IRQ 8); `find_block_device` reports it and the kernel registers the transport. The most likely
mechanism for the false record, stated as inference: both runners silently dropped the disk when
`CRICKER_DISK` named a file that did not exist (`[ -f ]` guard), and no riscv xtask path builds
`crickerfs.img`, so a riscv run on a clean target directory booted disklessly and every slot
honestly read device-id 0 ("empty"). The conclusion "QEMU prefers PCIe for riscv" was reasonable
and wrong; the machine was never asked the question. Both runners now fail loudly on a missing
disk file so this class of record cannot be manufactured again. The machine overrules the
documentation; the PCIe transport keeps its own justification (notes/pcie-transport-scope.md: the
door to NVMe and real hardware) but is **not** a parity-C prerequisite. The original, wrong entry
is kept below, unedited, because the correction is the instructive part.

**Completed (2026-07-27, the same night).** Over mmio: the `blk` dedicated binary (the shared
`virtio` module behind the 19f skeleton), `virtio_service` wiring unchanged, and the three disk
tests (read, DMA-escape attacker, indirect attacker) green on riscv; found and fixed the PLIC
boot-hart-lottery bug on the way (every plic::init site hardcoded context 1 while sie.SEIE was set
on whatever hart OpenSBI elected). Then over PCIe as well: the PCIe transport (DECISIONS §18,
notes/pcie.md) runs the byte-identical driver against the same image attached as virtio-blk-pci,
with the completion arriving as INTx through the PLIC. **Every parity workstream (A-E) is now
done; aarch64 and riscv64 are at feature parity, DMA confinement included.**

### C (superseded record). PARTIAL (kernel-side discovery done; blocked on QEMU transport).

The kernel-side enumeration is arch-correct on RISC-V now: the virtio-mmio slot layout (32 slots
0x200 apart on aarch64, 8 slots 0x1000 apart on riscv) moved into arch constants, the transport
window is mapped device-typed, and `find_block_device` probes it and reads valid magic on riscv. The
userspace driver itself is nearly portable (342 lines, one `dmb ish` to arch-gate to `fence`).

**Blocked, and honestly:** QEMU 11's riscv `virt` does not auto-plug `-device virtio-blk-device` into
the virtio-mmio slots (all eight read magic ok but device-id 0 = empty); it prefers the PCIe
transport. So there is no mmio block device for the kernel's mmio driver to find. Finishing C needs
either a way to force a virtio-mmio disk on riscv `virt`, or a PCIe virtio transport (a larger driver
change), plus then extracting the userspace driver from `hello` into a portable binary, granting it
the DMA region + device MMIO + Irq cap (the PLIC path from parity's earlier work), and the
`virtio_service` wiring. aarch64's virtio works fully (the userspace-driver-reads-a-disk test passes);
this is a transport-availability gap on riscv, not a kernel defect. Original scope below.

### C (original scope). virtio-blk + on-disk filesystem: M. The driver + DMA model.

aarch64 runs a userspace virtio-blk driver that reads crickerfs off a virtio-mmio disk, with the
kernel touching no DMA. RISC-V has the MMIO constants (`VIRTIO_MMIO_BASE`, `VIRTIO_IRQ_BASE`) but no
driver run. The virtio-mmio driver is largely portable (MMIO + virtqueues + DMA).

- Kernel: `find_block_device` (from the DTB `virtio_mmio@` nodes or by probing), route the device's
  PLIC IRQ to the userspace driver (the routing mechanism is done), hand it DMA-capable frames.
- Runner: attach a `virtio-blk` disk (`CRICKER_DISK`, as aarch64 does).
- **Proves:** userspace device drivers *with DMA* on the second arch, and the "kernel issued no
  virtio command and touched no DMA" claim on riscv. Self-contained; depends on nothing else here.

### D. Full integrated boot + interactive shell: DONE.

The interactive shell runs on RISC-V, over the serial: `echo` echoes, `run 9` spawns a worker that
computes 81. A new portable `system_initializer` (the counterpart of hello's aarch64-tied `init_boot`) is loaded
as the boot process and, from an untyped budget plus the NS16550 device cap and the UART Irq cap,
builds the console server, input driver, and shell out of its own budget and wires them; the kernel
parks. The three shell programs became arch-neutral -- the UART register layout is gated per arch
(PL011 vs NS16550, including that the NS16550 clears its RX interrupt on read, with no ICR). The
wiring is arch-neutral: `system_initializer` maps whatever device cap the kernel grants and delegates whatever
Irq cap it is handed, so the same binary would drive a PL011. `--features shell` selects it (the
riscv initboot). aarch64 keeps hello's init_boot; its shell still works. Original scope below.

### D (original scope). Full integrated boot + interactive shell: M–L. Mostly userspace porting.

aarch64 boots userspace init as the boot process, which builds the whole system (console + input +
shell + spawn service). RISC-V demonstrates init building *one* worker, then halts. Closing this is
mostly porting userspace, not proving new kernel behavior.

- Port the device-specific programs to the NS16550: `console.rs` (writes the UART, ~6 PL011 register
  sites) and `input.rs` (reads RX + the UART IRQ, ~2 sites). Either parameterize the register layout
  or ship NS16550 variants. `swish.rs` is already mostly portable (IPC, no direct hardware).
- A riscv `spawn_init` (or a generalized one) that grants the PLIC/NS16550 equivalents of the
  GIC/PL011/IRQ capabilities aarch64's grants.
- Wire the riscv boot to hand off to init-as-PID-1 instead of halting.
- **Proves:** the full interactive system runs on riscv. Lowest *kernel* value of the list; highest
  app-porting cost. Do last, or skip if the goal is "prove the kernel," not "ship the system."

### E. Benchmarks: DONE.

All eleven primitives plus CoreMark run on RISC-V, single-hart and SMP. `bench.rs` moved its timing
to `arch::timer::now`/`frequency` (rdtime on riscv), the boot reaches `bench::run` under `--features
bench`, and `initrd-riscv` packs `os_primitives_benchmarker` + `coremark`. Two fixes fell out of `spawn_el0` (the fast
userspace spawn+reap loop): os_primitives_benchmarker's 9-instruction `CHILD_STUB` was aarch64 machine code (added the
riscv `li`/`ecall` version), and the `MAP_CODE` syscall never synced the icache on the userspace
map-executable path (a correctness fix for both arches, latent until a spawn loop stressed it).

**And the numbers are now comparable.** `cargo xtask bench --riscv` runs the same suite on riscv under
the same deterministic `-icount` instrument (single hart, so an idle `wfi` cannot jump virtual time to
the timer and inflate the spawn primitives), with its own baseline (`bench/baseline-riscv64.txt`) for
`--check`/`--save`. The comparable metric is **ns/iter**: under `-icount shift=0` virtual time advances
~1 ns/instruction, and ns/iter divides out each arch's timer frequency (aarch64 CNTFRQ vs riscv's
10 MHz), so it is frequency-normalized instructions-per-primitive. CoreMark lands within ~7% across
the two ISAs (same workload), which is the sanity check that the comparison is sound. Original scope
below.

### E (original scope). Benchmarks: M. Cross-arch numbers.

aarch64 runs CoreMark and the os_primitives_benchmarker EL0 primitive suite (null syscall, context switch, IPC RTT, map,
spawn), plus cross-OS comparisons. RISC-V runs none. The workloads are userspace and mostly portable
(`coremark` is compute; `os_primitives_benchmarker` uses `user_rt::now`, which is `rdtime` on riscv).

- Make the `bench` boot mode reachable on riscv.
- Resolve the timing caveat honestly: `user_rt::cntfrq` is hardcoded to the QEMU virt 10 MHz timebase
  on riscv (there is no `CNTFRQ` register); a real number needs the frequency handed to userspace
  (an aux-vector entry from the DTB `timebase-frequency`).
- **Proves:** comparable performance on a second arch: the "measure, don't argue" ethos, with riscv
  as a new data point next to the L4 lineage. Depends on the timing fix for the numbers to be honest.

---

## Dependencies and sequence

```
B (tests) ─────────────── independent, enables checking everything after
A (SMP) ── needs per-hart trap-state refactor (self-contained otherwise)
C (virtio) ────────────── independent
E (bench) ── needs the timebase-to-userspace fix for honest numbers
D (boot/shell) ── needs console/input ported to NS16550
```

**Recommended order, by kernel-value-per-effort:**

1. **B: tests.** Cheapest real win; the semihosting primitive is already there. Makes the same suite
   green on both arches and every later claim checkable. Start here.
2. **A: SMP.** The last true primitive, and the only new kernel work. Highest value for "the kernel
   is portable," highest risk. The per-hart trap refactor is the gate.
3. **C: virtio + DMA.** Self-contained; proves the driver/DMA model on riscv.
4. **E: benchmarks.** Cross-arch numbers, once the timebase caveat is fixed.
5. **D: full boot / shell.** Most app-porting, least new kernel proof. Do last, or treat as optional.

If the goal is **"the kernel is at parity,"** A + B + C + E is the set, and D is system integration
rather than a kernel claim. If the goal is **"the whole system runs on riscv,"** add D.

---

## Post-parity correction (2026-07-27, milestone 32): user faults did not kill on riscv

Parity was declared with a hole nothing had tested: **the riscv trap dispatcher could not kill a
faulting user thread.** A U-mode `ebreak` was counted and stepped over, so a userspace panic
handler (every one of which ends in `ebreak`, expecting to die) resumed into its own spin loop and
lived forever; any other U-mode fault fell into an arm that panicked the whole kernel, behind a
comment claiming no user thread could run on RISC-V yet, which had been false since milestone 20.
DECISIONS §10's "a driver bug is a crashed process, not a dead machine" was an aarch64-only
property, and the parity record above did not say so.

Nothing noticed because no riscv test ever made a user thread fault. All the parity-C drivers
either succeed or are refused politely; the fault path had no witness. Milestone 32's
kill-mid-write test is the first riscv test that *requires* a user thread to genuinely die, and it
flushed the gap out within minutes of being written.

The fix mirrors aarch64's `user_fault`: a U-mode breakpoint or fault increments `USER_FAULTS`,
prints the legible kill line, and `sched::exit()`s the thread from the trap handler (the same
context the blocking-syscall path already schedules away from). The S-mode `ebreak` self-test
keeps its step-over. Proven by `a_faulting_user_thread_is_killed_and_the_kernel_survives` (spawn
the blk binary at a bogus role: panic, `ebreak`, killed, reaped) and by the kill-mid-write pair.

Two lessons, both repeats: a stale comment is misinformation with authority (the teardown note's
"a TODO that outlives the decision that resolved it" rule, hit again); and a parity claim is only
as wide as the suite that checks it. The fault path was in the suite's blind spot on both counts.
