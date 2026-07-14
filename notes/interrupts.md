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
