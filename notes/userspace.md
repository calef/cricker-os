# Userspace

**The concept the whole project is aimed at.** Milestone 7 is where a Rust program that
boots becomes an operating system, and this is what changes.

## Userspace is a cage the kernel builds and the CPU enforces

The critical thing, easy to miss: **this is not a software convention.** The kernel does not
inspect a program, decide it isn't allowed to do something, and politely decline. **The
silicon refuses.**

Userspace is the mode where a program runs behind three walls, and all three are hardware.

## Wall 1: Privilege

Our kernel runs at **EL1**. A user program runs at **EL0**. (See [aarch64.md](aarch64.md).)

At EL0 certain instructions **simply do not work**. Write `TTBR0_EL1` (the page table base
register) from a user program and the CPU does not execute it. It traps. Same for most system
registers, cache maintenance, `eret`.

This is not the kernel checking and saying no. **The instruction decoder itself faults.**
There is no code path to bypass, because there is no code involved.

## Wall 2: The address space

Each process gets its **own page tables** (`TTBR0_EL1`). Process A's `0x1000` and process B's
`0x1000` map to different physical RAM.

The kernel's own memory lives in the high half via `TTBR1_EL1`, and its page table entries are
marked privileged-only. At EL0, touching them faults.

So a user program **cannot name kernel memory**. Not "isn't permitted to read it." It executes
a load, the MMU walks the page table, finds a privileged mapping, and raises a fault instead of
returning data. **The address does not resolve.**

This is what [mmu.md](mmu.md) was building toward.

## Wall 3: The only door is a syscall

A user program cannot `bl kernel_function`. That address is unmapped or privileged.

The **only** way to make the kernel do anything is to deliberately trap:

```asm
svc #0
```

Look at what the hardware does, unaided, the instant that executes:

- switches to **EL1**
- switches `sp` to **`SP_EL1`** — the kernel's stack, which userspace could not have corrupted,
  because it is a *different register* userspace has no access to (see [stack.md](stack.md))
- saves the return address in `ELR_EL1` and the processor state in `SPSR_EL1`
- jumps to **`VBAR_EL1` + a fixed offset**, an address *the kernel* chose

The kernel now runs at full privilege, at a location it picked, on a stack it owns, with the
user's registers sitting there as **inputs to be validated** rather than instructions to be
obeyed. It does the work, and `eret` drops back to EL0.

**We already know this shape.** It is exactly what `hlt #0xF000` does to us today, with QEMU
playing the kernel. We are on the calling side of a mechanism we are about to implement on the
receiving side. See [semihosting.md](semihosting.md).

## What the walls buy

**A buggy program crashes itself, not the machine.** The difference between a segfault and a
kernel panic is entirely a consequence of walls 1 and 2.

**Programs cannot read each other's memory.** A password manager's secrets are not visible to a
browser tab.

**The kernel can actually enforce policy**, because every request comes through one door it
controls. File permissions, resource limits, quotas: none of it is enforceable if programs can
talk to the disk controller directly.

**You can run untrusted code at all.** Without this, every program you download runs as root.

MS-DOS had no userspace. Every program ran at full privilege, wrote anywhere in memory, and
drove hardware directly. Which is exactly why one bad program took down the machine, and why
viruses were trivial. Protected mode plus the MMU is the invention that made modern computing
possible.

## Where cricker-os is right now

**There is no userspace at all.** Every line we have written runs at EL1, at full privilege.
Our `println!` could rewrite the page tables if it wanted. There are no processes, no
isolation, and nothing to protect the kernel from, because there is nothing but kernel.

**Milestone 7 is where we build the cage.** Concretely:

1. Construct a page table that maps a program's code and data, and **not** the kernel.
2. Set `SPSR_EL1` to say "return to EL0."
3. Point `ELR_EL1` at the program's entry.
4. `eret`.

The CPU drops privilege and starts running code that can no longer touch us. Then we catch it
when it comes back through `svc`.

## This explains the milestone order

Milestone 7 is not arbitrarily placed. It is **the first point at which all three prerequisites
exist**:

| Wall | Needs | Milestone |
|---|---|---|
| The syscall door | exception vectors, `VBAR_EL1` | **2** |
| Address space isolation | the MMU and page tables | **4** |
| Taking the CPU back from a program that won't yield | preemptive threads, timer interrupts | **5, 6** |

That last row is the async argument ([DECISIONS.md](../DECISIONS.md) §5) arriving from a
different direction. **A user program never calls `.await`.** If you cannot take the CPU away
from it by force, you cannot safely run it, and you do not have an operating system.

Everything from here to milestone 7 is building the machinery required to make a cage that
holds.

---

*Add to this file as new privilege/isolation concepts come up.*
