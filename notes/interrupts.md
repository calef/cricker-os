# Interrupts: the GIC and the timer

Milestone 5. The kernel is now **preemptible**: a timer interrupt can land between any two
instructions.

Which means every piece of the locking discipline we wrote in
[DECISIONS.md](../DECISIONS.md) §9 stops being a hypothesis.

## The GIC: the multiplexer in front of the CPU

The CPU has **one** IRQ input line. That's all. Everything a kernel wants from interrupts —
priorities, masking individual sources, routing to a particular core — lives in the interrupt
controller, not in the CPU.

Two halves, and the split *is* the design:

| | Where | Shared? | Does what |
|---|---|---|---|
| **Distributor** (GICD) | `0x0800_0000` | **one per machine** | which core gets an interrupt, and whether a source is enabled at all |
| **CPU interface** (GICC) | `0x0801_0000` | **one per core** (banked) | this core's own view: acknowledge, priority mask, end-of-interrupt |

N cores see their *own* CPU interface at the *same address* — the hardware banks the registers
per core. That's what makes "deliver this to core 3" something the hardware can do without the
software knowing.

Both addresses come from the [device tree](device-tree.md) (`intc@8000000`), not from a
constant.

## Three kinds of interrupt, and the numbering isn't arbitrary

| INTID | Kind | |
|---|---|---|
| 0–15 | **SGI** — Software Generated | one core kicking another. This is how SMP bringup and TLB shootdown work. |
| 16–31 | **PPI** — Private Peripheral | **per-core**. The timer is one. |
| 32+ | **SPI** — Shared Peripheral | the UART, the disk. Any core may service them. |

**The timer is a PPI (INTID 30), and it has to be.** A timer that fired on only one core could
not preempt threads running on the others. Every core has its own timer, its own countdown, and
its own interrupt, all wearing the same number.

The device tree says so: `interrupts = <1 14 ...>` on the timer node. Type 1 means PPI, 14 is
the PPI number, PPIs start at 16, so `16 + 14 = 30`.

## Priorities are backwards

**Lower value = higher priority.** And `GICC_PMR` is a *mask*: an interrupt is delivered only if
its priority is **strictly less than** PMR.

So `PMR = 0xff` means "let everything through" and `PMR = 0` means "let nothing through."

Get that comparison the wrong way round and you get a machine that takes no interrupts and
gives you no clue why. It's also why `gic::init` sets PMR **before** enabling the CPU interface:
the other order leaves a window where the interface is live with whatever the firmware left in
PMR, which on a cold boot is often zero.

## Acknowledge, then end-of-interrupt

```
IAR  (read)   -> "which interrupt?"   ...and READING IT IS WHAT TAKES IT.
EOIR (write)  -> "I'm done with it"
```

`IAR` has a **side effect**. Reading it acknowledges. Exactly once per interrupt.

And until `EOIR` is written, the GIC will not deliver another interrupt of equal or lower
priority. **Forget it and the timer fires exactly once and then never again**, which looks
nothing like "you forgot to write a register."

**INTID 1023 is spurious**: the GIC raised the line and then changed its mind (another core took
it, or it got masked). Do nothing, and in particular do **not** write EOIR — signalling
completion for an interrupt you never took corrupts the GIC's priority stack.

## IRQs dispatch by vector slot, not by ESR

`exception_dispatch` gets both the trap frame and *which of the sixteen vector slots fired*
([exceptions.md](exceptions.md)). For a fault we decode `ESR_EL1`. For an IRQ we must not.

**`ESR_EL1` describes a synchronous exception**: what instruction did what wrong. An IRQ is
*asynchronous*. It has nothing to do with the instruction it interrupted, and `ESR_EL1` still
holds whatever the last *synchronous* exception left there. Reading it in an IRQ handler is
reading a stale answer to a question nobody asked.

## The bug we shipped and then measured

The timer is **one-shot**. It fires, and then sits there with its status bit set, holding the
interrupt line high until the handler sets a new deadline.

There are two registers to do that with, and the difference is not cosmetic:

| | |
|---|---|
| `CNTP_TVAL_EL0` | a **relative countdown**. "Fire N ticks from *now*." |
| `CNTP_CVAL_EL0` | an **absolute deadline**. "Fire when the counter reaches exactly this." |

Re-arming with `TVAL = interval` in the handler makes the real period

```
    interval  +  however long it took to get into the handler
```

Every tick starts its countdown *late*, and **the lateness is never recovered**. The clock just
runs slow, forever, and nothing tells you.

Measured, in QEMU, at a configured 100 Hz:

```
  +250ms: 17 ticks fired   <- should be 25.  ~70 Hz.  30% of our preemptions, gone.
```

`CVAL` puts the deadlines on a **fixed grid**: `next = previous + interval`, anchored at boot. A
slow handler makes *one* tick late; it does not push the next one out too.

```
  +250ms: 25 ticks fired   <- correct
```

One register.

### The safety valve

If we fall so far behind that the next deadline is *already in the past*, `previous + interval`
would fire immediately, and again, and we'd spin in the handler forever paying down a debt we
cannot pay.

So: give up on the missed ticks and re-anchor the grid to now. Every kernel does this and every
kernel calls it the same thing — **dropping ticks** — and it is worth counting, because a
nonzero count means the handler is taking longer than a whole tick period.

## Uptime comes from the counter, not the tick count

`uptime_ms()` reads `CNTPCT_EL0` and divides. **Deliberately not `ticks * 10`.**

If a tick is ever missed — a long critical section, a slow handler — the tick count undercounts
and *time appears to slow down*. The hardware counter cannot lie.

**This is `Instant`.** It is the thing `core` could never give us, and the reason is exact:
nothing in `core` knows what time it is.

## The test the whole locking discipline was written for

Everything in [locking.md](locking.md) exists to prevent one thing: a timer interrupt landing
inside a critical section, taking the same lock, and spinning forever waiting for code that
cannot run until it returns. On one core. Permanently.

Until this milestone that was a **hypothesis**. There were no interrupts.

`holding_a_lock_masks_the_timer`:

1. confirm ticks are flowing
2. take a lock, and busy-wait across **three whole tick periods**
3. assert **not one tick landed**
4. release, and watch them resume

Step 2 works because `spin_for` reads `CNTPCT_EL0`, which **keeps counting while interrupts are
masked**. A tick-based delay would simply hang there, which is its own kind of proof.

## And the cost of masking, made visible

`a_long_critical_section_costs_a_tick` asserts that holding a lock across two tick periods
**loses a tick**. The deadline passes while we cannot service it, we re-arm to a deadline
already in the past, and the only sane move is to drop it.

That is the bill for the deadlock prevention, and it is why "**keep critical sections short**"
(DECISIONS §9) has teeth rather than being good manners. At milestone 6, a lost tick is a thread
that didn't get preempted.

It is a strange thing to *assert*, until you notice: if that cost ever stopped being real,
`IrqSafeMutex` would have stopped masking, and the deadlock would be back.

---

*Add to this file as new interrupt sources come up.*

---

# Milestone 9a: an interrupt becomes a message

DECISIONS §10 promised this and notes/capabilities.md sketched it. Here it is.

## The problem a userspace driver has with interrupts

A driver at EL0 (milestone 8 put one there) cannot install an interrupt handler: handlers run at
EL1, in the kernel's vector table, at a privilege the driver does not have. And the kernel cannot
handle a device interrupt itself, because **it does not know what the device is** — that was the
whole point of moving the driver out.

So the interrupt has to reach the driver as something the driver *can* receive. It becomes a
message.

## The shape

```
  device raises INTID  ──►  kernel handle_irq:  mask INTID at the GIC
                                                turn it into a notification
                                                EOI
                                                        │
                       driver blocked in WAIT  ◄────────┘  wakes
                                                │
                       reads the device, quiets its interrupt
                                                │
                       invoke(irq_cap, ACK)  ──►  kernel: re-enable INTID at the GIC
```

The kernel's half does nothing device-specific. It masks the line, delivers a message, and later
re-enables the line when the driver says the device is quiet. Everything that knows what a *virtio
block device* is lives in userspace.

## Why the interrupt gets masked the instant it fires

Device interrupts are usually **level-triggered**: the device holds its interrupt line asserted
until the driver does something to quiet it (for virtio, reads `InterruptStatus` and writes
`InterruptACK`). If the kernel left the line enabled and just EOI'd, the GIC would see the line
still asserted and **re-deliver immediately, forever** — an interrupt storm the machine never
climbs out of, because the only code that can quiet the device is the driver, which never gets to
run.

So `handle_irq` masks the INTID at the distributor (`gic::disable`) the moment it fires. The driver
services the device, then calls `ACK` on its `Irq` capability, and only then does the kernel
re-enable the line (`gic::enable`). Until then the interrupt is held off. **This is exactly seL4's
IRQHandler protocol**, and it is what lets a process that holds no privilege safely own an
interrupt.

## An interrupt is not a rendezvous

IPC on an endpoint (milestone 7e) is synchronous: a sender waits for a receiver. An interrupt
cannot wait. It fires whether or not the driver happens to be blocked in `WAIT` at that instant,
and it must not be lost if the driver is a hair late.

So the notification is **asynchronous**, and the mechanism is one counter: `Endpoint::pending`. If
a thread is waiting, the interrupt wakes it. If not, `pending` is incremented, and the next `WAIT`
drains it instead of blocking. An interrupt that fires one instruction before the driver calls
`WAIT` is remembered, not dropped. There is a test named exactly that.

## The capability

`Object::Irq(intid)`. Its holder can:

- `WAIT` — block until the interrupt fires (internally, `RECV` on the endpoint the kernel routed
  the interrupt to).
- `ACK` — re-enable the interrupt at the GIC, after quieting the device.

A driver that holds this capability can receive one specific interrupt and nothing else. It cannot
mask other interrupts, cannot touch the GIC directly, cannot see any other device's line. The
authority is exactly one INTID, handed over deliberately.

## Testing it with no device

The whole path is exercised by a **software-generated interrupt** (SGI): `gic::send_sgi` raises
INTID 1 from software, with no hardware behind it. A thread blocks in `WAIT`, the test raises the
SGI, the handler routes it, the thread wakes. Deterministic, and it needs no disk. The virtio
driver (9b) will use the same path with a real device interrupt in place of the SGI.

---

# IRQ affinity: spreading device lines off core 0 (fix/irq-delivery, 2026-07-29)

Until now every SPI targeted core 0: `gic::enable` wrote `ITARGETSR[intid] = 1` (bit 0). Under SMP
that funnels every device interrupt onto one core, and because the handler that turns an interrupt
into a message runs on the core that took it (`handle_irq` calls `irq_notify`, which wakes the
driver onto `cpu::current`), the driver wake lands on core 0 too. Every disk and NIC completion, and
the driver work it triggers, re-concentrates on core 0 no matter where the threads were spawned.

The fix distributes SPI lines across the online cores. The **policy** lives in `arch::irq::enable`
(it may read `smp::online_count`; it is arch glue, not a driver): each SPI is assigned a target core
the first time it is enabled, round-robin over the online cores, and that assignment is **stable**
(`IRQ_TARGET`, an atomic per-INTID slot). Stability matters because the `Irq` capability's ACK
re-enables the line on every completion; re-rolling the target each time would make the line hop
cores on every interrupt. The **mechanism** stays in the driver: `gic::enable(intid, target_cpu)`
writes `ITARGETSR[intid] = 1 << target_cpu`. PPIs and SGIs are per-core, so the target is ignored
for them (the timer PPI, the reschedule SGI). Rule #2 holds: the GIC driver is told which core, it
does not decide.

What this does and does not buy, measured honestly:

- It **does** move each device's interrupt (and the wake it causes) off core 0 onto its assigned
  core. Verified: the full aarch64 suite (disk, PCIe disk, both DHCP round trips) stays green with
  the lines spread, and a diskless boot's device IRQs land on cores other than 0.
- It **does not**, on its own, make the heavy `std_net` pipeline (smoltcp in `netstack`, plus the std
  program) go faster under SMP, because that pipeline is a chain of IPC rendezvous, and a rendezvous
  wake still lands on the *waker's* core (`cpu::current`). Spreading the interrupt moves the whole
  chain to the interrupt's core; it does not parallelize it. Parallelizing the pipeline needs the
  rendezvous/device-IRQ **wake placement** to be load-aware, which is the scheduler's call
  (DECISIONS §28 territory), not the interrupt controller's. See the `std_net` note below.

## The riscv PLIC side, done the same way (parity §19)

The PLIC equivalent spreads device sources across harts, the same round-robin-with-a-stable-target
shape as the GIC. The PLIC delivers a source to a **context** (a hart at a privilege level, `2*hart+1`
for S-mode on QEMU `virt`), so the pieces are:

- **Every hart sets `SEIE` early** (`arch::irq::init_this_cpu` now unmasks supervisor external
  interrupts alongside the software-interrupt IPI source). This is safe before the PLIC base is even
  known: `SEIE` with no source enabled for the hart's context delivers nothing. It sidesteps the
  ordering hazard that a secondary comes online (running `init_this_cpu`) *before* the boot path
  calls `plic::init`; the CSR is unmasked now, the context is set up later.
- **The boot hart opens a target hart's context and routes the source to it** (`target_context` in
  `arch/riscv64/irq.rs`). The threshold and enable registers are global PLIC MMIO, so the boot hart,
  which runs every `enable` (test wiring, driver spawn), can open any hart's context and enable a
  source on it. The target is chosen round-robin over the online harts and is **stable per source**
  (`SOURCE_CTX`), for the same reason the GIC target is stable: the ACK re-enables the line on every
  completion, and the mask and the re-enable have to name the same context.
- **Each hart claims and completes against its own context** (`this_s_context` = `2*cpu::id()+1`,
  passed to `plic::claim`/`complete`/`disable` from the external-interrupt handler). A source targets
  exactly one hart, so the hart that takes it is the hart it is enabled on, and the mask/complete land
  on the right context. The PLIC driver stays mechanism-only: it is told the context, it does not read
  the hartid (rule #2, DECISIONS §4).
- **The enable bits are serialized; nothing else in the driver is.** The sentence two bullets up, "the
  threshold and enable registers are global PLIC MMIO," is exactly what makes this necessary, and the
  assembly audit ([arch-audit.md](arch-audit.md), finding 3) is where it was caught. One enable
  register carries **32 sources** of a context, so setting one source's bit is a read-modify-write over
  a word its neighbours share, and the boot hart running `enable` can collide with another hart's
  handler running `disable` on a neighbour. A lost update either masks a device forever (its driver
  blocks on an interrupt that never arrives) or leaves a level-triggered source live after the handler
  masked it (an interrupt storm on that hart). So `enable`/`disable` go through one helper holding an
  `IrqSafeMutex` at `rank::IRQ_CONTROLLER`, the same rank the GIC's lock takes.

  It has to be `IrqSafeMutex` rather than a plain lock, and that is the §9 deadlock rather than
  caution: `disable` runs *in the handler* while `enable` runs in thread context with interrupts on,
  so a plain spinlock would let a thread take the word on a hart and then have that hart's own
  handler spin on it forever. Nothing else in the driver takes the lock, deliberately:
  claim/complete is per-context and therefore hart-local (the hot path stays lock-free), the
  per-source priority register is unshared, and the threshold write is a whole-word store of `0`, so
  it is idempotent rather than an RMW. **aarch64 never needed this**: the GIC's
  `ISENABLER`/`ICENABLER` are write-1-to-set and write-1-to-clear, so one store touches one line and
  the architecture supplies the atomicity that the PLIC's plain read/write bits do not.

### The lost update, step by step

The abstract phrase "read-modify-write" hides the bug, so here it is concretely. Say the disk is
source 8 and the NIC is source 10. Both bits live in the same 32-bit enable word (sources 0..31 of
that context), so a driver touching *its own* source still writes the neighbour's bit back, because
the only way to change one bit of that word is to store all 32.

Hart 0 is in thread context enabling the disk. Hart 1 is inside its handler masking the NIC. Without
the lock they interleave:

| step | hart 0 (`enable(8)`)        | hart 1 (`disable(10)`)      | word in the PLIC |
|------|----------------------------|-----------------------------|------------------|
| 1    | read → `0b0100` (bit 10)   |                             | `0b0100_0000_0000` |
| 2    |                            | read → `0b0100` (bit 10)    | `0b0100_0000_0000` |
| 3    | or in bit 8                |                             | `0b0100_0000_0000` |
| 4    | write `bits 8,10`          |                             | `0b0101_0000_0000` |
| 5    |                            | clear bit 10 from ITS copy  | `0b0101_0000_0000` |
| 6    |                            | write `0b0000`              | `0b0000_0000_0000` |

Hart 1's copy of the word was read at step 2, before hart 0's store at step 4. Its store at step 6
is computed from that stale copy, so it does not just clear bit 10, it *un-sets bit 8 as collateral*.
Hart 0's enable is gone, and nothing anywhere reports an error: the PLIC has no notion of a
disagreement, it just holds whichever word was written last.

Both directions of that race are bad, and they fail differently, which is why neither is tolerable:

- **A lost `enable`** (above) silently masks a device forever. The driver blocks in `Irq::WAIT` on an
  interrupt the PLIC will never route to it. The symptom is a hang in code that has no bug.
- **A lost `disable`** is the mirror: the handler thinks it masked a level-triggered source, but the
  bit is back on and the line is still asserted, so the PLIC re-delivers immediately, forever. That
  is an interrupt storm, and the hart it lands on stops making progress.

This is also why the window is small but not negligible. It is the few instructions between the read
and the write, hit only when two harts touch the same 32-source word at once, which is exactly the
kind of race that passes a thousand test runs and then fails in front of an audience. The audit found
it by reading for the pattern rather than by waiting for it to bite ([arch-audit.md](arch-audit.md),
finding 3).

The fix is one `IrqSafeMutex` around the read and the write together, so steps 1-4 and 2-6 cannot
interleave: whichever hart takes the lock second re-reads the word *after* the first hart's store and
computes from current state. It is the smallest possible critical section (a read, an or/and-not, a
write) and it is off the hot path, so it costs nothing measurable.

Proven the same way as the GIC: the riscv suite is green at the 4-hart boot with device sources
spread across harts (disk read, the interrupt-driven fs-server block server, both routed to whatever
hart the round-robin picked, plus the SMP placement tests). As on aarch64, this distributes the
interrupt and the wake it causes; it does not by itself parallelize the `std_net` pipeline, which
needs the load-aware rendezvous wake (see below).

## The `std_net` SMP hang was not an interrupt-delivery bug (resolved)

Recorded so it is not re-diagnosed as one, and left in place with its resolution because the wrong
diagnosis was tempting and the right one took two agents to reach.

The `std_net` test (smoltcp in `netstack` serving a std program's `UdpSocket`/`TcpStream`) used to hang
under the 4-core boot on **both** ISAs, watchdog-killed at 60 s, while in the same run the hand-built
DHCP round trips (`virtio_net`, `virtio_net_pci`) passed and the interrupt-driven fs-server block
server passed. That asymmetry was the tell: interrupt delivery under SMP was sound, and the hang was
specific to the heavier, longer, timer-driven smoltcp pipeline.

Two things were wrong, and only one of them was scheduling:

1. **A real deadlock.** `netstack` blocked on the NIC interrupt while smoltcp still had a retransmit
   pending, so neither side would move: the timer that would have retransmitted was never polled
   because the thread was parked in `Irq::WAIT`, and no packet was coming to wake it. That is a
   mutual-idle deadlock, not slowness, and no amount of core placement fixes it. `netstack` now bounds
   its wait by smoltcp's own next-poll deadline.
2. **Serialization on one core.** With the deadlock gone the pipeline ran, but slowly: its threads
   are woken by a mix of device IRQ and IPC rendezvous, and both wakes pinned to one core. DECISIONS
   §28's wake split fixed the half that mattered (device-IRQ wakes go load-aware, IPC rendezvous
   wakes stay local, because a rendezvous partner is about to run on the caller's core and moving it
   only adds a migration).

An intermediate hypothesis of mine was **wrong and worth keeping written down**: I expected IRQ
affinity alone to fix it. It cannot. Spreading interrupts across harts relocates where a wake lands;
it does not parallelize a chain of request/response rendezvous, which is serial by construction. The
agent disproved it by measurement rather than argument.

`std_net` now passes on both ISAs. It is also the longest honest test in the suite at roughly 300 to
344 s on aarch64, because it is a real DHCP lease plus TCP and UDP round trips through an emulated NIC
under TCG. That length is legitimate work, not a symptom, which is precisely what makes it awkward for
the watchdogs: see the per-test ceiling discussion in [scheduler.md](scheduler.md).
