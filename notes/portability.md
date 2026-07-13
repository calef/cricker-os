# How portable kernels are written

## The structure: an `arch/` layer and a short list

Every portable kernel does the same thing. Linux has `arch/x86/`, `arch/arm64/`,
`arch/riscv/`, and ~20 more. NetBSD splits everything into MI (machine-independent) and MD
(machine-dependent). Windows NT shipped a literal `HAL.dll` from day one, which is how NT
ran on x86, MIPS, Alpha, PowerPC, Itanium, x64, and ARM.

What's surprising is **how short the per-architecture list is**:

1. **Boot and early init** — firmware to "Rust code with a stack." Wildly different everywhere.
2. **Context switch** — save/restore the register file. Pure assembly, ~50 lines.
3. **Exception entry/exit** — the vector table, plus the assembly that saves registers in and restores them out.
4. **Page table format** — the bits in a PTE are completely different on x86 and ARM.
5. **Atomics and memory barriers.**
6. **Cache maintenance** — ARM often needs explicit flushes; x86 is coherent for free.
7. **Syscall entry** — `syscall` on x86_64, `svc` on aarch64.
8. **Device discovery** — ACPI vs. Device Tree.
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
([DECISIONS.md](../DECISIONS.md) §4), generalized and taken seriously. Remember it when we
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

## What this means for cricker-os

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
