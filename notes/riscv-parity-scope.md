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

### A. SMP (multi-hart) — DONE.

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

### A (original scope). SMP (multi-hart) — L, high risk. The only pure-kernel gap.

RISC-V runs single-hart today; `send_reschedule` is a no-op and the runner passes `-smp 1`. This is
the last big *primitive* aarch64 claims and riscv does not, and the only parity item that is genuine
new kernel work rather than porting userspace.

- **Prerequisite — per-hart trap state.** The leak-free `tp` fix uses a single global `KERNEL_TP`,
  and the trap frame is placed below `sp`. SMP needs each hart to have its own per-CPU pointer and
  trap-frame area: the standard approach is `sscratch` holding this hart's kernel context, swapped in
  at trap entry, set up per-hart at bring-up. This refactor touches the trap path (trap.s) and is the
  enabling step; it was flagged as a follow-up during the `tp` saga.
- **Secondary bring-up** via SBI HSM (`sbi_hart_start(hartid, addr, opaque)`): the boot hart starts
  the others into a secondary entry path that mirrors aarch64's `secondary_main` — set `stvec`, `tp`/
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

### B. In-kernel test suite on RISC-V — DONE.

RISC-V now boots the kernel test harness and passes **51 portable tests** (`cargo xtask test` runs
both arches: aarch64 116, riscv64 51). The gap is the aarch64-specific tests, gated off riscv: the
userspace-exec suite in `user.rs` (37 tests driving hand-written aarch64 programs through `exec`), the
SMP tests (workstream A), and the two SGI interrupt tests. Three real things surfaced: the
`sifive_test` finisher (`0x10_0000`) had to be mapped device-typed and reached through the direct map
(the boot tour halts via `wfi` and never exercised `semihosting::exit` under paging); the timer
watchdog needed wiring into riscv `timer::tick`; and a genuine race — a timer tick landing inside
`sched::init` ran the deferred `schedule()` before the idle thread was registered ("nothing runnable
and no idle thread"), fixed by masking interrupts across init, which aarch64 gets for free by bringing
the scheduler up before enabling interrupts. Original scoping below.

### B (original scope). In-kernel test suite on RISC-V — M, low-medium risk. Highest value per effort.

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

### C. virtio-blk + on-disk filesystem — DONE, over BOTH transports; and the "blocked" record was WRONG (correction below).

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
change) — plus then extracting the userspace driver from `hello` into a portable binary, granting it
the DMA region + device MMIO + Irq cap (the PLIC path from parity's earlier work), and the
`virtio_service` wiring. aarch64's virtio works fully (the userspace-driver-reads-a-disk test passes);
this is a transport-availability gap on riscv, not a kernel defect. Original scope below.

### C (original scope). virtio-blk + on-disk filesystem — M. The driver + DMA model.

aarch64 runs a userspace virtio-blk driver that reads crickerfs off a virtio-mmio disk, with the
kernel touching no DMA. RISC-V has the MMIO constants (`VIRTIO_MMIO_BASE`, `VIRTIO_IRQ_BASE`) but no
driver run. The virtio-mmio driver is largely portable (MMIO + virtqueues + DMA).

- Kernel: `find_block_device` (from the DTB `virtio_mmio@` nodes or by probing), route the device's
  PLIC IRQ to the userspace driver (the routing mechanism is done), hand it DMA-capable frames.
- Runner: attach a `virtio-blk` disk (`CRICKER_DISK`, as aarch64 does).
- **Proves:** userspace device drivers *with DMA* on the second arch, and the "kernel issued no
  virtio command and touched no DMA" claim on riscv. Self-contained; depends on nothing else here.

### D. Full integrated boot + interactive shell — DONE.

The interactive shell runs on RISC-V, over the serial: `echo` echoes, `run 9` spawns a worker that
computes 81. A new portable `sysinit` (the counterpart of hello's aarch64-tied `init_boot`) is loaded
as the boot process and, from an untyped budget plus the NS16550 device cap and the UART Irq cap,
builds the console server, input driver, and shell out of its own budget and wires them; the kernel
parks. The three shell programs became arch-neutral -- the UART register layout is gated per arch
(PL011 vs NS16550, including that the NS16550 clears its RX interrupt on read, with no ICR). The
wiring is arch-neutral: `sysinit` maps whatever device cap the kernel grants and delegates whatever
Irq cap it is handed, so the same binary would drive a PL011. `--features shell` selects it (the
riscv initboot). aarch64 keeps hello's init_boot; its shell still works. Original scope below.

### D (original scope). Full integrated boot + interactive shell — M–L. Mostly userspace porting.

aarch64 boots userspace init as the boot process, which builds the whole system (console + input +
shell + spawn service). RISC-V demonstrates init building *one* worker, then halts. Closing this is
mostly porting userspace, not proving new kernel behavior.

- Port the device-specific programs to the NS16550: `console.rs` (writes the UART, ~6 PL011 register
  sites) and `input.rs` (reads RX + the UART IRQ, ~2 sites). Either parameterize the register layout
  or ship NS16550 variants. `shell.rs` is already mostly portable (IPC, no direct hardware).
- A riscv `spawn_init` (or a generalized one) that grants the PLIC/NS16550 equivalents of the
  GIC/PL011/IRQ capabilities aarch64's grants.
- Wire the riscv boot to hand off to init-as-PID-1 instead of halting.
- **Proves:** the full interactive system runs on riscv. Lowest *kernel* value of the list; highest
  app-porting cost. Do last, or skip if the goal is "prove the kernel," not "ship the system."

### E. Benchmarks — DONE.

All eleven primitives plus CoreMark run on RISC-V, single-hart and SMP. `bench.rs` moved its timing
to `arch::timer::now`/`frequency` (rdtime on riscv), the boot reaches `bench::run` under `--features
bench`, and `initrd-riscv` packs `elbench` + `coremark`. Two fixes fell out of `spawn_el0` (the fast
userspace spawn+reap loop): elbench's 9-instruction `CHILD_STUB` was aarch64 machine code (added the
riscv `li`/`ecall` version), and the `MAP_CODE` syscall never synced the icache on the userspace
map-executable path (a correctness fix for both arches, latent until a spawn loop stressed it).

**And the numbers are now comparable.** `cargo xtask bench --riscv` runs the same suite on riscv under
the same deterministic `-icount` instrument (single hart, so an idle `wfi` cannot jump virtual time to
the timer and inflate the spawn primitives), with its own baseline (`bench/baseline-riscv.txt`) for
`--check`/`--save`. The comparable metric is **ns/iter**: under `-icount shift=0` virtual time advances
~1 ns/instruction, and ns/iter divides out each arch's timer frequency (aarch64 CNTFRQ vs riscv's
10 MHz), so it is frequency-normalized instructions-per-primitive. CoreMark lands within ~7% across
the two ISAs (same workload), which is the sanity check that the comparison is sound. Original scope
below.

### E (original scope). Benchmarks — M. Cross-arch numbers.

aarch64 runs CoreMark and the elbench EL0 primitive suite (null syscall, context switch, IPC RTT, map,
spawn), plus cross-OS comparisons. RISC-V runs none. The workloads are userspace and mostly portable
(`coremark` is compute; `elbench` uses `user_rt::now`, which is `rdtime` on riscv).

- Make the `bench` boot mode reachable on riscv.
- Resolve the timing caveat honestly: `user_rt::cntfrq` is hardcoded to the QEMU virt 10 MHz timebase
  on riscv (there is no `CNTFRQ` register); a real number needs the frequency handed to userspace
  (an aux-vector entry from the DTB `timebase-frequency`).
- **Proves:** comparable performance on a second arch — the "measure, don't argue" ethos, with riscv
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

1. **B — tests.** Cheapest real win; the semihosting primitive is already there. Makes the same suite
   green on both arches and every later claim checkable. Start here.
2. **A — SMP.** The last true primitive, and the only new kernel work. Highest value for "the kernel
   is portable," highest risk. The per-hart trap refactor is the gate.
3. **C — virtio + DMA.** Self-contained; proves the driver/DMA model on riscv.
4. **E — benchmarks.** Cross-arch numbers, once the timebase caveat is fixed.
5. **D — full boot / shell.** Most app-porting, least new kernel proof. Do last, or treat as optional.

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
