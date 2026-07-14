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

---

# Milestone 7a: it is real now

The above was written before any of it existed. This is what actually happened.

## Entering EL0 is *returning from an exception that never happened*

There is no "drop to EL0" instruction. There is only `eret`, which restores whatever `SPSR_EL1`
says and jumps to `ELR_EL1`, and **the exception level to return to is a field in `SPSR_EL1`.**

So we do not need a new way *down*. We need a **fake way back**:

```rust
frame.write(TrapFrame {
    x: [0; 31],
    elr: USER_CODE_VA,       // where `eret` jumps
    spsr: 0,                 // ...and the level it jumps TO. Zero means EL0t.
    sp_el0: USER_STACK_TOP,  // ...on the stack it lands on
});
enter_userspace(frame)       // mov sp, x0  /  b exception_restore
```

`enter_userspace` is **two instructions**, and `exception_restore` was written at milestone 2 to
return from *interrupts*. It had no idea userspace was coming.

**This is the second time the project has pulled exactly this trick.** `Thread::spawn` fakes a
`switch_to` frame so the `ret` that *resumes* a thread also *starts* one
([threads.md](threads.md)). Both times, the "start" path turned out to be the "resume" path with
a **forged frame**, and no new code at all.

## And `SPSR_EL0T` is literally zero, which is worth staring at

- `M[4] = 0` → AArch64.
- `M[3:0] = 0b0000` → **EL0t**. (The `t` is for `SP_EL0`, the only stack pointer EL0 has. There
  is no EL0h.)
- `DAIF = 0` → **interrupts unmasked.**

That last one is the interesting bit. **IRQs are live the instant we land in EL0.** If they were
masked, a user program in a tight loop could never be preempted, and the machine would be gone.
Which is precisely the failure [DECISIONS §5](../DECISIONS.md) spent a whole milestone refusing
to accept.

## The trap frame grew a field, for free

`SP_EL0` is a **physically different register** from the `sp` the kernel uses. At EL1 we run with
`SPSel=1`, so `sp` means `SP_EL1`. Taking an exception from EL0 switches the hardware to
`SP_EL1` and leaves `SP_EL0` **untouched**, so the user's stack pointer just sits there.

It survives an exception on its own. What it does **not** survive is a context switch to another
*user* thread, which would spend `SP_EL0` and never give it back. So it belongs in the frame.

It cost nothing: it landed in the **padding word the frame already had**. `TrapFrame` is still
272 bytes.

## What milestone 4 had already paid for

Almost none of 7a was new. The kernel lives in `TTBR1` at `0xffff_...`; userspace lives in
`TTBR0` at `0x0000_...`; and **the hardware picks the table register from bits 63:48 of the
address** ([higher-half.md](higher-half.md)). So:

- **The kernel is mapped in every address space, for free.** Nobody copies anything.
- **A syscall does not switch page tables.** Nothing to flush, nothing to remap.
- **Installing a process is one `msr ttbr0_el1`.**
- `Flags::user_code()` and `Flags::user_data()` had been sitting in the `paging` crate, unused,
  since milestone 4.

## And there is deliberately NO syscall

The user program executes `svc #0` and **asks for nothing**. No syscall number, no argument
convention, no return value. The kernel counts it.

That restraint is the point. §8 said "if we find ourselves hacking in a syscall without having
had that conversation, the plan has failed." We had the conversation, and the answer was
[capabilities](capabilities.md). The syscall surface gets designed at 7d, in one piece, against
a capability table.

---

## Two bugs, and they were both worth having

### 1. Rust put the trap frame in read-only memory

The first version was `enter_userspace(&TrapFrame { .. })`. **Every field of that struct is a
compile-time constant**, so Rust *const-promoted* it into `.rodata`, and `mov sp, x0` pointed the
kernel's stack pointer at **read-only memory**.

The user's first `svc` then tried to write its trap frame there. And what happened next is the
best part:

> `SAVE_CONTEXT` faulted, which re-entered the vector, which ran `SAVE_CONTEXT` again with `sp`
> **272 bytes lower**, which faulted, which re-entered... **The kernel walked `sp` downward
> through the whole of `.rodata` and then all of `.text`, one fault at a time**, until it fell
> out of the bottom of the image into writable RAM, where `SAVE_CONTEXT` finally completed and
> the handler could speak.
>
> The fault report we finally got was its last words about the fault *before* it.

The fix is that **a user thread's `TrapFrame` is not an ordinary local.** It must sit at exactly
the address `SAVE_CONTEXT` will rebuild it at, which is `kernel_stack_top - 272`. `eret` leaves
`SP_EL1` just past it, and the hardware does not consult our intentions.

The contract was even written in the `vectors.s` comment. We wrote it down and then did not
honor it, which is a lesson about comments as much as about `.rodata`.

### 2. `TTBR0` is one register, and threads are not

The second version activated the address space in `exec` and left it there. Then:

> A user thread was still spinning at EL0 when the **next** `exec` installed a different
> `TTBR0`. It kept running, at EL0, **in somebody else's address space**, where its own code
> page was a page of zeroes. It died executing them. (`ESR` said `EC=0`, "Unknown reason", which
> is exactly what a word of zeroes decodes to.)

**An address space is a property of a thread**, so the context switch has to carry it, the same
way it carries a stack and a register file. `Thread` now owns an `AddressSpace`, and
`sched::schedule` installs the incoming thread's root (or an empty *reserved* table, for a kernel
thread) before it switches.

And ownership does the rest for free: the **reaper** drops a dead thread's `AddressSpace`, which
unmaps and frees the entire low half, exactly as it already did for its `KernelStack`.

## A user fault kills the thread, not the machine

```
  user thread 4 killed: Data abort from a lower EL
    pc 0x0000000000400008   far 0xffff000040080000   esr 0x9200000f
  the kernel is fine.
```

And it kills it by calling `sched::exit()` **from inside the exception handler**. That works
because milestone 6 already built the reaper, for an unrelated reason: a thread cannot free the
stack it is standing on, so the *next* thread does it. `exit()` never returns,
`exception_restore` is never reached, the `eret` never happens, and the user program is simply
not resumed.

So the mechanism behind *"a driver bug is a crashed process, not a dead machine"*
([DECISIONS §10](../DECISIONS.md)) was already sitting in the kernel, finished, before we knew we
needed it.

## The fault is a PERMISSION fault, and that word is the whole boundary

The outlaw program reads `0xffff_0000_4008_0000`. That address **is mapped**. It **is readable**.
The kernel reads it all day.

The hardware picks `TTBR1` from bits 63:48, walks **the kernel's own tables**, finds the page,
reads the `AP` bits, and says no.

```
esr 0x9200000f
     ^^         EC   = 0x24  data abort from a LOWER EL
            ^^  DFSC = 0b001111  PERMISSION fault  (not a translation fault)
```

A *translation* fault would only mean we had failed to map something, which would pass a sloppier
test and prove nothing. **A permission fault means the wall is there.**

## What it prints

```
  and now the other side of the boundary:

    hello  : reached EL0, executed 2 svc, returned to EL0 after each

  user thread 4 killed: Data abort from a lower EL
    pc 0x0000000000400008   far 0xffff000040080000   esr 0x9200000f
  the kernel is fine.
    outlaw : touched 0xffff000040080000, was killed, kernel survived (1 fault)

  the machine has run code it does not trust, and taken the CPU back.
```

Two `svc`, not one, and that is deliberate: **one proves we left. Two prove we came back.**
