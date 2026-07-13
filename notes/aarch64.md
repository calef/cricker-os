# aarch64

## What it is

An instruction set architecture (ISA) is the contract between software and silicon: which
instructions exist, what registers there are, how memory is addressed, how privilege
works. Compile for aarch64 and you get bytes an aarch64 CPU knows how to execute. Feed
those bytes to an x86 CPU and you get garbage.

**aarch64 is the 64-bit mode of ARM's architecture**, introduced with ARMv8-A in 2011.

Also called ARM64, arm64, and AArch64. Apple says arm64, Rust and GCC say aarch64, ARM's
own docs say AArch64. Same thing. The naming is just a mess.

Arm Ltd. designs the ISA and licenses it; Apple, Qualcomm, Broadcom, and Amazon build the
chips. Our MacBook is aarch64. So is every Raspberry Pi since the 3, and most of AWS.

## Why we chose it over x86_64

x86 grew organically from the Intel 8086 in 1978 and never threw anything away. It still
boots in a 16-bit mode from 1978, transitions to 32-bit, then to 64-bit. Instructions are
1 to 15 bytes long. Learning to boot it means learning a lot of Intel history.

aarch64 was designed clean, in one go, by people who had watched x86 accumulate scar
tissue for thirty years:

- **Fixed 32-bit instruction width.** Every instruction is exactly 4 bytes.
- **31 general-purpose 64-bit registers** (`x0`–`x30`), plus a stack pointer. x86 has 16.
- **Load/store architecture.** Arithmetic happens only on registers. Touching memory
  requires an explicit load or store. x86 lets you add directly to a memory location;
  ARM makes you load, add, store. More instructions, far simpler model.

## Exception levels (the privilege model)

This is the thing an OS lives and dies on.

| Level | Who lives here |
|---|---|
| **EL0** | Userspace. Programs. Least privilege. |
| **EL1** | **The kernel. This is where cricker-os lives.** |
| **EL2** | Hypervisor. Runs virtual machines. |
| **EL3** | Secure firmware. Below everything. |

Clean, numbered, orthogonal. Higher number, more power.

x86's equivalent is rings 0–3 (nobody uses 1 and 2), plus System Management Mode bolted
on the side, plus VMX root/non-root for virtualization, layered on over decades.

**The EL0/EL1 boundary is the single most important line in the OS.** Everything until
milestone 7 runs at EL1. Milestone 7 is the moment we construct a world at EL0, drop into
it, and catch it when it asks us for something. That transition *is* what an operating
system is.

## Registers

**General purpose:** `x0`–`x30`, 64-bit. (`w0`–`w30` are the lower 32 bits of the same
registers.) `x30` is the link register: it holds the return address after a `bl` call.
The stack pointer `sp` is separate. The program counter `pc` is not directly writable.

**System registers:** a separate namespace only privileged code can touch. Read/written
with the special `MRS` and `MSR` instructions. A userspace program can never touch these.
A kernel does almost nothing else.

| Register | What it does | Milestone |
|---|---|---|
| `VBAR_EL1` | Address of the exception handler table | 2 |
| `TTBR0_EL1` / `TTBR1_EL1` | Physical address of the page tables | 4 |
| `SCTLR_EL1` | Master system control, including "is the MMU on?" | 4 |
| `MPIDR_EL1` | Which CPU core am I? (used to park cores 1-3 at boot) | 1 |
| `CurrentEL` | Which exception level am I running at? | 2 |

The `aarch64-cpu` crate is a typed Rust wrapper over exactly these, so we write
`SCTLR_EL1.modify(SCTLR_EL1::M::Enable)` instead of hand-writing assembly and getting a
bit position wrong.

## Control flow: `b`, `bl`, `ret`

`b label` is a plain unconditional jump. Sets `pc` to the target. A `goto`.

`bl label` (**b**ranch with **l**ink) does the same jump **and** stores the address of the
*next* instruction into `x30`. "Link" = "remember where to come back to."

`ret` is just: *jump to the address in `x30`*. That's the entire instruction.

So a function call on aarch64 is `bl foo` ... `ret`. **No stack involved.** Compare x86,
where `call` pushes the return address to the stack and `ret` pops it.

### The consequence

`x30` holds exactly **one** return address. If `foo` calls anything, that second `bl`
overwrites `x30` and `foo` has forgotten how to get home. Which is exactly why the
prologue in [the stack note](stack.md) exists:

```asm
stp  x29, x30, [sp, #-32]!   ; stash x30 before it gets clobbered
```

The stack is not where return addresses *go* on ARM. It's where they get **parked** when
a function needs `x30` for a call of its own.

Corollary: a **leaf** function (calls nothing) never has to save `x30`, so it skips the
prologue entirely and just `ret`s.

### The four variants

| Instruction | Jumps to | Saves return address? |
|---|---|---|
| `b label` | a fixed label | no |
| `bl label` | a fixed label | **yes**, into `x30` |
| `br xN` | the address in register `xN` | no |
| `blr xN` | the address in register `xN` | **yes**, into `x30` |

The register forms exist because `b`/`bl` encode the target as a signed offset *inside the
instruction*, and instructions are only 32 bits wide. 26 bits are available for the
offset, scaled by 4, giving a reach of **±128 MiB**. To call farther, load the full 64-bit
address into a register and use `blr`. Function pointers and virtual dispatch use `blr`
for the same reason: the target isn't known at assembly time.

### `cbnz` / `cbz`

"Compare and branch if (not) zero." aarch64 folds the compare and the branch into one
instruction for the common test-against-zero case. We use `cbnz x0, park` in `boot.s` to
park CPU cores 1-3 in a spin loop while we stay single-core.

## Decoding `aarch64-unknown-none-softfloat`

That's our Rust **target triple**, and every piece is meaningful:

- **`aarch64`** — the ISA.
- **`unknown`** — the vendor. Nobody in particular.
- **`none`** — **the operating system: there isn't one.** This is what "bare metal" means,
  spelled out in the target name. No syscalls, no libc, no `std`. It's why the kernel is
  `#![no_std]`.
- **`softfloat`** — do not use the hardware floating-point / SIMD registers.

That last one is a real kernel design decision, not a technicality. aarch64 has 32
FP/SIMD registers (`v0`–`v31`, 128 bits each). If kernel code used them, every interrupt
and every context switch would have to save and restore all of them, which is a lot of
bytes to shuffle on every timer tick. So kernels traditionally don't use floating point at
all, and the `softfloat` target enforces that at compile time.

---

*Add to this file as new aarch64 concepts come up.*
