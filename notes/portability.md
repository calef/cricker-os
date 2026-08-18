# How portable kernels are written

## The structure: an `arch/` layer and a short list

Every portable kernel does the same thing. Linux has `arch/x86/`, `arch/arm64/`,
`arch/riscv/`, and ~20 more. NetBSD splits everything into MI (machine-independent) and MD
(machine-dependent). Windows NT shipped a literal `HAL.dll` from day one, which is how NT
ran on x86, MIPS, Alpha, PowerPC, Itanium, x64, and ARM.

What's surprising is **how short the per-architecture list is**:

1. **Boot and early init**: firmware to "Rust code with a stack." Wildly different everywhere.
2. **Context switch**: save/restore the register file. Pure assembly, ~50 lines.
3. **Exception entry/exit**: the vector table, plus the assembly that saves registers in and restores them out.
4. **Page table format**: the bits in a PTE are completely different on x86 and ARM.
5. **Atomics and memory barriers.**
6. **Cache maintenance**: ARM often needs explicit flushes; x86 is coherent for free.
7. **Syscall entry**: `syscall` on x86_64, `svc` on aarch64.
8. **Device discovery**: ACPI vs. Device Tree.
9. **Timers.**

**Everything else is portable**, and everything else is the overwhelming bulk of the code:
scheduler, filesystems, network stack, allocator policy, process management, and nearly all
drivers. A virtio-net driver does not care what CPU it's on.

## Two abstractions worth stealing

**Linux folds page table levels.** It defines a *generic* five-level page table model and
has each architecture map its real format onto it. An architecture with only four levels
declares the missing level "folded": a single-entry table compiled away to nothing. So
`mm/` is written once, against a model no hardware actually implements, and every
architecture fits itself into it.

**NetBSD's `bus_space`.** A driver never dereferences an MMIO pointer. It calls
`bus_space_read_4(tag, handle, offset)`. The `tag` encodes *how to actually perform an
access on this platform*, so the same driver works whether the device sits behind
memory-mapped I/O on ARM or behind x86's separate port-I/O instruction space. One driver,
radically different buses.

That second one is our "a driver never reaches into a kernel global" rule
([DECISIONS](../design/decisions/04-kernel-shape.md) §4), generalized and taken seriously. Remember it when we
write the UART driver.

## The thing that cannot be abstracted: the memory model

This is where portability actually gets hard, and no `arch/` directory saves you.

x86 has a **strong** memory model (roughly total store order). ARM is **weakly ordered**:
the CPU reorders loads and stores far more aggressively, and other cores can observe your
writes out of order.

The consequence is brutal. Write a lock-free data structure on x86, forget a memory
barrier, and **it works.** Perfectly. Forever. All tests pass. Then you run it on ARM and
it corrupts data once a week under load. **x86's strong ordering silently hides the bug,
and the bug was in portable-looking code the whole time.**

This is why Linux mandates `smp_mb()`, `READ_ONCE()`, `WRITE_ONCE()` everywhere, even where
x86 provably doesn't need them, and why it has a formal documented memory model. You cannot
retrofit this. The discipline is there from the start or the codebase is quietly full of
landmines that only detonate on the port.

## Port early, and port to something alien

Linux was x86-only for its first few years. Then Linus ported it to **DEC Alpha**: a 64-bit
RISC machine with the weakest memory model ever shipped and (early on) no byte-granularity
loads or stores.

Almost nobody used Alpha. That was never the point. Linus has said repeatedly that **the
Alpha port is what made Linux portable**, precisely because Alpha was so hostile and so
different that every hidden x86 assumption got forced into the open.

Porting to something *similar* teaches you nothing. Porting to something alien finds all of
it.

**Actionable: the second architecture should come early and be as different as possible.**

## What this means for nife

### We got lucky on the memory model

We start on **ARM, the weak one.** We physically cannot develop hidden strong-ordering
assumptions, because the hardware won't let us. If we later port to x86, our barriers just
become no-ops.

**Porting weak → strong is easy. Porting strong → weak is where projects die.** Had we
picked x86 first we'd have been building a landmine field for our future selves.

### Device discovery is the real portability wall, not the CPU

ACPI vs. Device Tree is a difference in the whole *model* of how you learn what hardware
exists. Much deeper than a shim.

### The Device Tree, and a correction (now resolved)

The DTB (Device Tree Blob) describes every device on the machine: where the UART is, where
RAM starts and ends, where the interrupt controller lives, how many CPUs there are. It is
the machine **telling us** what it is, as opposed to us **looking it up** and hardcoding it.
That difference is exactly the difference between a kernel that runs on one board and a
kernel that can be told what board it's on.

**An earlier draft of this note claimed QEMU's `virt` machine passes a DTB pointer in `x0`
at entry, full stop. That was wrong, and milestone 1 proved it: we printed `x0` and got
zero.** The truth is conditional on what kind of file you hand QEMU.

| What you hand `-kernel` | How QEMU boots it | `x0` at entry |
|---|---|---|
| flat binary with an arm64 `Image` header | **Linux boot protocol** | **DTB pointer** |
| an **ELF** | bare-metal: copy segments, set PC, go | **not populated** (we observed 0) |

Milestone 1 shipped an ELF, so we got the bare-metal path and nobody handed us anything.

**Fixed.** We now emit a flat binary carrying a 64-byte arm64 Image header, QEMU recognizes
it as a kernel, and `x0` arrives holding a real device tree pointer (`0x4400_0000` on
`virt`). Two tests hold the line: one asserts the pointer is nonzero, one reads
`0xd00dfeed` at it. See [boot-protocol.md](boot-protocol.md).

This also moves us toward the Pi, which boots a flat `kernel8.img` and has no use for an ELF
at all. Not a detour from the port; the first step of it.

---

*Add to this file as new portability concerns come up.*

## A first rehearsal: hardware virtualization on the dev Mac

Before the Raspberry Pi, there is a cheaper way to find our QEMU-shaped assumptions: run the kernel
on the real Apple Silicon core under Hypervisor.framework (`cargo xtask run --hvf`). It runs the
same `virt` devices but the real CPU, so it surfaces *CPU* assumptions while QEMU still holds the
*devices*. It already caught one: we used the physical timer, which a hypervisor reserves, and
switched to the virtual timer. See [virtualization.md](virtualization.md).

## Measured against Liedtke's doctrine, 2026-08-18

calef asked how true this OS is to Liedtke's position that a microkernel should be **rewritten per
architecture**, exploiting whatever the specific processor gives you. Everything above this section
describes how portable kernels are written; this one measures whether we are one, and against the
one authority who argued we should not be.

### What Liedtke actually claimed, and who withdrew it

"On micro-Kernel Construction" (SOSP 1995) argues that a microkernel implementation should not
strive for portability, because a hardware abstraction adds overhead and hides hardware-specific
optimisation opportunities. His evidence was that the "compatible" i486 and Pentium had shifted the
trade-offs enough to imply significantly different optimal implementations.

**The position did not survive its own author.** Elphinstone and Heiser's 20-year retrospective
(SOSP 2013) puts it flatly: the argument "was debunked by Liedtke himself, with the high-performance
yet portable Hazelnut kernel and especially Pistachio", which reached 80 to 90 percent
architecture-agnostic code. Their verdict table for this design principle reads **"Replaced:
Non-portable implementation by significant portion of architecture-agnostic code."** Pistachio ports
to MIPS, Alpha, 64-bit PowerPC and ARM each changed less than 10 percent of the code.

So the doctrine to measure against is not "rewrite per architecture". It is the weaker and more
interesting one that replaced it: be portable in structure, and specialise exactly where the
hardware pays you to.

### Where we sit

Derived from the tree, not remembered (`find kernel/src -name '*.rs' | xargs wc -l`, against the same
over `kernel/src/arch`):

| | architecture-agnostic |
|---|---|
| Liedtke's original L4 (1993, i486) | ~0 percent, assembly per processor |
| Pistachio | 80 to 90 percent |
| **nife `kernel/src`** | **82 percent** (8,489 of 47,525 lines under `arch/`; 79 percent excluding the test and bench wiring) |
| seL4, x86 against ARM | ~50 percent |

Hand-written assembly is 1,152 lines, 2.4 percent of the kernel. Rule 1 holds: the only two `asm!`
hits outside `arch/` are comments describing assembly that was removed.

**We are at Pistachio's number and are less specialised than seL4.** The retrospective explains its
own lower figure, and the explanation is not that seL4 is more Liedtke-true by intent: about half of
its code is virtual memory, which is necessarily architecture-specific, and the fraction is high
because seL4 is *smaller overall* with the agnostic resource management pushed to userland. Our
`arch/` tree shows the same shape, with the two `mmu.rs` files at 2,781 lines making up a third of it.

### Where we genuinely do exploit the processor

The line count hides this, and it is the half of the doctrine that survived. The `arch/` directories
do not abstract the two machines into a common denominator:

- **TLB shootdown.** aarch64 issues `tlbi aside1is` and the *hardware* broadcasts it across the
  inner-shareable domain. RISC-V has no such instruction, so it makes SBI RFENCE calls into firmware
  that send IPIs. Two unrelated mechanisms for one intent, neither levelled down to the other.
- **ASID width.** aarch64 mandates 8 bits, so the context-switch TLB flush disappears entirely.
  RISC-V permits `satp.ASID` to be **zero bits wide**, so the kernel probes the width at boot and
  keeps flushing on every switch when the field is absent (asids.md). That is per-processor
  adaptation of exactly the kind Liedtke argued for, and the portable-looking alternative (always
  flush) would have cost aarch64 the win.
- **`TTBR0`/`TTBR1`.** On aarch64 the kernel lives in `TTBR1` and never moves, so a syscall needs no
  address-space switch at all (higher-half.md). RISC-V has one `satp` and no such split.

### Where we are not, and it is the one exception seL4 kept

The retrospective, one sentence after the 50 percent figure: **"There is little architecture-specific
optimisation except for the IPC fastpath."** That is the single place seL4 stayed Liedtke-true, and
it is precisely the thing this tree does not have. On the narrow measure Liedtke cared most about,
hand-tuning the hot path to the processor, we score zero. design/decisions/95-a-proven-ipc-fastpath.md
is the open decision about whether to change that, and milestone 132 is the gate that measured the gap.

**A finding worth stating because the opposite is the natural assumption: §19 does not forbid it.**
The parity tenet says an architecture is a new `arch/` directory, "never a fork of the **feature
matrix**", which governs which capabilities ship rather than whether implementations are shared. A
hand-written per-arch fastpath is permitted outright, provided both ISAs get one or the gap goes in a
scope note, and rule 1 actively gives it a home. The tenet a reader would expect to block
Liedtke-style specialisation is orthogonal to it.

### One constraint Liedtke did not have, and we share it with seL4

§4.6 of the retrospective records that the traditional approach of hand-crafted assembler fast paths
"was unsuitable for seL4, as the verification framework could only deal with C", which forced
assembler down to the bare minimum. Our Kani boundary has the same shape for the same reason, which
is why §95 recommends Rust over assembly and records that seL4's own assembly fastpath variant was
written, never verified, and is not used.

### An honest mark against the advice this note already gave

"Port early, and port to something alien" is above, with the Alpha argument that porting to something
*similar* teaches you nothing. **We ported to RISC-V**, which is another weakly-ordered load-store
RISC. It found real bugs and reached full parity, so it was not wasted, but it validated less than
the note's own standard asks for: it could not have exposed a hidden weak-ordering assumption,
because it shares the assumption. The alien port this note called for is x86_64, which §19 names as a
declared target that does not exist yet, and where TSO makes the weak-first discipline pay out in the
direction rule 4 predicted.
