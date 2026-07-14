# The higher-half kernel

The kernel lives at `0xffff_0000_...`, out of `TTBR1_EL1`. Userspace will get the bottom half
and `TTBR0_EL1`.

## Why it has to be this way

At milestone 7, every process gets its own page tables, and a context switch swaps them.

**If the kernel lived in `TTBR0`, swapping `TTBR0` would delete the kernel.** The first
context switch into a user process would unmap the code doing the switching, mid-instruction.

aarch64 hands us the answer directly: **two translation table base registers.**

| Top 16 bits of the VA | Register | Who |
|---|---|---|
| all **zero** | `TTBR0_EL1` | the process (swapped on every context switch) |
| all **one** | `TTBR1_EL1` | the kernel (never moves) |

The kernel's mappings are in `TTBR1` and simply never change. A syscall requires **no address
space switch at all**: the trap goes to EL1, the kernel is already mapped, and it can read the
user's memory through `TTBR0`, which is still loaded.

x86_64 has only one such register and has to achieve the same effect by convention. This is
one of the places aarch64's clean-sheet design visibly pays.

## The chicken and egg

Link the kernel at a high virtual address, and **every absolute address the compiler baked
into the binary is a VA that doesn't work until the MMU is on.** But the code that turns the
MMU on is inside that binary, and it's running at a *physical* address, because that is where
the bootloader put it.

Two facts get us out.

### 1. `adrp` is PC-relative, so it yields physical addresses right now

```asm
adrp x0, __stack_top        // x0 = (PC & ~0xfff) + linker_offset
```

The linker computes `linker_offset` from **virtual** addresses. But `PC` is currently a
**physical** address, and `VA - PA` is a constant (`0xffff_0000_0000_0000`). The two
differences cancel, and you get the **physical** address of the symbol. Free of charge.

Which is why `boot.s` uses `adrp` and never `ldr x0, =symbol` before the MMU is on: a literal
pool holds the absolute VA, which is exactly the thing that doesn't work yet.

*(Literal pools holding **constants** are fine. The load itself is PC-relative; it's the value
that would be wrong.)*

### 2. Bits 63:48 aren't translated, so ONE table is both maps

`KERNEL_VA_BASE = 0xffff_0000_0000_0000` touches **only bits 63:48**, which are never part of
any page-table index ([page-tables.md](page-tables.md)).

So `PA` and `PA | KERNEL_VA_BASE` have **identical L0/L1/L2/L3 indices**. The identity map and
the high-half map are the *same table contents*.

Which means `boot.s` can build **one** two-page table and point *both* `TTBR0` and `TTBR1` at
it. There is no careful dance. That is the whole trick.

Three things fall out of choosing that base, and all three are load-bearing:

- `VA = PA | KERNEL_VA_BASE` is exact and reversible by masking. No arithmetic, no overflow,
  no per-region offset table.
- The boot map is one table serving both halves (above).
- The kernel gets the entire top half and userspace the entire bottom half, with no
  negotiation.

## The sequence

```
boot.s, running at the PHYSICAL address, MMU off:
  1. park cores 1..n
  2. sp = adrp(__stack_top)          <- physical
  3. zero .bss                        <- adrp, physical  (the boot tables live here)
  4. build a crude map: two 1 GiB blocks (device @ 0, RAM @ 0x4000_0000)
  5. TTBR0 = TTBR1 = that one table
  6. SCTLR_EL1.M = 1                  <- MMU on; we survive via TTBR0's identity map
  7. sp = ldr(=__stack_top)           <- NOW a literal pool means what it says. HIGH.
  8. br  ldr(=kernel_main)            <- jump to the high half

mmu.rs, running HIGH, MMU on:
  9. build fine-grained tables (W^X, guard page, direct map)
 10. TTBR1 = new root
 11. EPD0 = 1                         <- TTBR0 OFF. It is now free for userspace.
```

**The boot map is deliberately coarse and permissive**: two 1 GiB blocks, and the RAM one is
executable everywhere. It exists to survive twenty instructions. `mmu.rs` immediately replaces
it with a map that enforces W^X and punches out the guard page. Linux does exactly this, for
exactly this reason.

## The direct map

Every byte of physical memory is permanently visible at `pa | KERNEL_VA_BASE`.

This is how the kernel touches a frame the allocator just handed it. **With paging on, a
physical address the kernel cannot *name* is a physical address it cannot use.** Zeroing a new
page table, filling a new user page, reading the device tree: all of it goes through the
direct map.

So the kernel now deals in two kinds of address, and the type system does not distinguish
them:

| | Speaks | Example |
|---|---|---|
| `frames::Frame` | **physical** | what the allocator hands out |
| linker symbols | **virtual** | `__text_start`, `__stack_top` |
| the device tree pointer from `x0` | **physical** | QEMU speaks physical |
| anything you dereference | **virtual** | necessarily |

Mixing them up is the new class of bug this milestone introduces. `phys_to_virt` /
`virt_to_phys` are the only bridge, and every crossing goes through one of them.

## How the tests caught the mistakes

Turning `TTBR0` off is what made the errors *loud*, and it caught two immediately:

```
[EXCEPTION]  Data abort from the same EL
  FAR_EL1   0x0000000044000000
```

That's a test dereferencing the device tree's **physical** address. Before, the identity map
made it work by accident. Now, a low address does not exist, and the mistake faults on the
spot with the offending address printed.

Same for the UART. That's a feature: **an identity map that lingers is an identity map that
hides physical/virtual confusion**, right up until userspace shows up and the confusion becomes
a security hole.

## What this unblocks

`TTBR0_EL1` is now empty, disabled, and reserved. At milestone 7:

1. Build a page table for a user process.
2. Load it into `TTBR0`.
3. `eret` to EL0.

The kernel doesn't move. It doesn't have to.

---

*Add to this file as new address-space concepts come up.*
