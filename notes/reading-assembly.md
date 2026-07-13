# Reading aarch64 assembly

aarch64 assembly is far more regular than x86. Five rules decode almost everything.

## The five rules

**1. Destination comes first.**

```asm
add x0, x1, x2      ; x0 = x1 + x2
sub sp, sp, #32     ; sp = sp - 32
mov x0, x1          ; x0 = x1
```

Read `op dest, src1, src2` as `dest = src1 op src2`.

**2. Brackets mean "the memory at this address."** The most important rule.

```asm
mov x0, x1          ; copy the VALUE in x1 into x0
ldr x0, [x1]        ; x1 holds an ADDRESS. load the 8 bytes there into x0.
```

No brackets: a register's contents. Brackets: dereference. It's `x` vs `*x`.

**3. `#` means a literal number.** `#32` is thirty-two.

**4. `ldr` loads, `str` stores.** Memory→register, register→memory. aarch64 is a load/store
architecture: **these are the only instructions that touch memory.** Everything else works
on registers.

**5. Lines ending in `:` are labels** (names for addresses). Lines starting with `.` are
**directives** to the assembler (`.section`, `.global`, `.align`), not CPU instructions.

## Registers

| Name | What it is |
|---|---|
| `x0`–`x30` | 64-bit general purpose |
| `w0`–`w30` | the **lower 32 bits of the same registers**. `w0` is not a different register from `x0`, it's a smaller window onto it. |
| `x29` / `fp` | frame pointer |
| `x30` / `lr` | link register (return address) |
| `sp` | stack pointer |
| `xzr` / `wzr` | **the zero register.** Reads always give 0. Writes are discarded. |

`xzr` is strange and appears constantly. It isn't a register that *holds* zero, it's a
hardwired source of zeroes and a bit bucket. `str xzr, [x0]` writes eight zero bytes.
`cmp x0, x1` is literally "subtract into `xzr`": do the subtraction, throw away the
answer, keep only the flags.

## Addressing modes (the confusing part)

| Written | Address used | Side effect on `x0` |
|---|---|---|
| `[x0]` | `x0` | none |
| `[x0, #16]` | `x0 + 16` | none |
| `[x0, #16]!` | `x0 + 16` | **`x0 = x0 + 16`** (*pre*-index) |
| `[x0], #16` | `x0` | **`x0 = x0 + 16`** (*post*-index) |

`!` means "write the updated address back into the register."

**Mnemonic for pre vs post: look at where the offset sits relative to the closing
bracket.** Inside → applied *before* the access. Outside → *after*.

```asm
str xzr, [x0], #8       ; write 8 zero bytes at x0, THEN advance x0 by 8
```

A zeroing loop in one instruction. Exactly what our `.bss` loop does.

## The pseudo-instruction that looks like a lie

```asm
ldr x0, =__stack_top
```

There is no aarch64 instruction that loads a 64-bit constant, because instructions are
only 32 bits wide. So this isn't real. The **assembler** sees `=`, stashes the 64-bit value
in a "literal pool" nearby, and rewrites the line as a PC-relative load from there.

Worth knowing: when you disassemble, you won't see `ldr x0, =__stack_top`. You'll see
`ldr x0, #0x24` and wonder where your symbol went.

## System registers: `mrs` and `msr`

The privileged register namespace (see [aarch64](aarch64.md)) is not addressable by normal
instructions. Two special ones:

- `mrs x0, mpidr_el1` — **read** a system register into a general register
- `msr vbar_el1, x0` — **write** a general register into a system register

Mnemonic: the general-purpose register is always the one nearer the `r` in the mnemonic.
(`mrs` = *move register from system*, `msr` = *move system from register*.)

## Worked example: our `boot.s`

Every construct above appears here.

```asm
_start:
    mrs     x0, mpidr_el1        ; read the "which core am I" SYSTEM register into x0
    and     x0, x0, #0xff        ; keep only the low byte (the core number)
    cbnz    x0, park             ; if it's not zero we're not core 0 -> go park

    ldr     x0, =__stack_top     ; the address the linker script gave us
    mov     sp, x0               ; sp = that. NOW we can call Rust functions.

    ldr     x0, =__bss_start     ; x0 = start of .bss
    ldr     x1, =__bss_end       ; x1 = end of .bss
zero_bss:
    cmp     x0, x1               ; compare (subtract, keep only the flags)
    b.hs    bss_done             ; branch if x0 >= x1 (unsigned) -> finished
    str     xzr, [x0], #8        ; write 8 zero bytes at x0, then x0 += 8
    b       zero_bss             ; loop
bss_done:

    bl      kernel_main          ; call into Rust. never returns.

park:
    wfe                          ; "wait for event" - sleep this core, low power
    b       park                 ; if something wakes it, sleep again
```

See [the stack note](stack.md) for why `sp` must be set before that `bl`, and
[aarch64](aarch64.md) for the `b`/`bl`/`ret` family.

## Condition suffixes

`cmp` sets flags; the next branch reads them.

| Suffix | Meaning |
|---|---|
| `b.eq` / `b.ne` | equal / not equal |
| `b.hs` / `b.lo` | unsigned >= / < ("higher or same" / "lower") |
| `b.ge` / `b.lt` | signed >= / < |

`cbz` / `cbnz` fuse the compare and branch for the common test-against-zero case.

## Tools

**[godbolt.org](https://godbolt.org)** (Compiler Explorer). Language: Rust. Target:
aarch64. Paste a function, watch the assembly appear beside it, color-coded line by line.
The fastest way to build intuition. Write a loop, see what it becomes. Write a struct, see
how it's laid out.

**`cargo objdump`** to disassemble our kernel and see what the compiler did with our Rust.

**GDB** (once attached to QEMU):

| Command | Does |
|---|---|
| `layout asm` | live disassembly view |
| `x/10i $pc` | show the next 10 instructions |
| `info registers` | dump the register file |
| `si` | step one instruction |

---

*Add to this file as new instructions come up.*
