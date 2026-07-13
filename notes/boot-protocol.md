# The boot protocol, and the arm64 Image header

## The question QEMU is asking

When you say `-kernel foo`, QEMU has to decide **what kind of thing `foo` is**. It doesn't
ask you. It sniffs the file, and the answer determines how much help you get.

| What you hand it | How QEMU boots it | `x0` at entry |
|---|---|---|
| an **ELF** | bare metal: copy the segments to the addresses in the program headers, set PC to the entry point, release the CPU | **nothing.** Registers are not populated. |
| a **flat binary with an arm64 Image header** | the **Linux boot protocol**: generate a device tree, place it in memory, hand you a pointer to it | **the DTB address** |

Milestone 1 shipped an ELF. We printed `x0` and got zero, which is how we found out the
claim "QEMU passes a device tree pointer in x0" is only true for the second row.

**A 64-byte header is the entire difference.**

## The header

From Linux's `Documentation/arch/arm64/booting.rst`. Ours is
`kernel/src/arch/aarch64/image_header.s`.

| offset | size | field | ours |
|---|---|---|---|
| `0x00` | 4 | `code0` | `b _boot` — the entry point *is* byte 0, so this jumps over the header |
| `0x04` | 4 | `code1` | 0 |
| `0x08` | 8 | `text_offset` | `0x80000` — load us this far into RAM |
| `0x10` | 8 | `image_size` | how much **memory** we occupy, including `.bss` and stack |
| `0x18` | 8 | `flags` | `2` = little-endian, 4 KiB pages |
| `0x20`–`0x37` | 24 | reserved | 0 |
| `0x38` | 4 | `magic` | `0x644d5241`, which is `"ARM\x64"` little-endian |
| `0x3c` | 4 | reserved | 0 |

You can see all of it:

```bash
cargo xtask image
```

### Three details that are easy to get wrong

**`text_offset` and the linker script must agree.** QEMU loads the image at
`RAM_base + text_offset`. RAM starts at `0x4000_0000` on `virt`, and `text_offset` is
`0x8_0000`, so we land at `0x4008_0000`. That is exactly where `link.ld` puts us. **These are
two independent numbers that have to match**, and nothing checks them for you.

**`image_size` must cover `.bss` and the stack, not just the file.** The flat binary stops
after `.data`, because `.bss` occupies no file bytes ([elf.md](elf.md)). But `image_size` is
a statement about *memory*, not about the file. Understate it and the bootloader will happily
place the device tree blob on top of our `.bss` or our boot stack, and we'll destroy it the
first time we push a stack frame. So `link.ld` computes it as `__stack_top - __image_start`.

**`code0` must not touch `x0`.** The entry point is the first byte of the image, so `code0`
executes before anything else in the kernel. Ours is a single `b _boot`, which leaves the
device tree pointer sitting exactly where QEMU left it.

### The failure mode has no diagnostics

If the magic is wrong, QEMU does not complain. It silently decides the file is an anonymous
blob, boots it anyway, and hands you a zero in `x0`. Which looks exactly like a bug in your
own code.

That is why `cargo xtask image` exists, and why there are two tests.

## Why the tests boot the same way the real thing does

`.cargo/config.toml` points cargo's runner at `scripts/qemu-runner.sh`, which strips the ELF
to a flat binary before launching QEMU. So `cargo test` and `cargo xtask run` take **the
identical boot path.**

That was a deliberate choice. It would have been easier to leave the tests booting the ELF
(they don't need the device tree). But a test harness that exercises a different boot path
than the real kernel is testing a fiction, and the difference would eventually be exactly
where a bug lived.

## The two tests

`device_tree_pointer_was_provided` asserts `x0` was nonzero. A zero means we've silently
regressed to the ELF path, which is easy to do by editing one line of the runner script and
otherwise impossible to notice.

`device_tree_has_the_right_magic` reads the first four bytes at that pointer and expects
`0xd00dfeed`. A nonzero pointer is necessary but not sufficient; this proves it points at an
actual device tree.

Note the byte order: **the DTB magic is big-endian**, so we `u32::from_be` it. The device tree
format predates the little-endian consensus and never changed. Every field in a DTB is
big-endian, which will matter a lot when we actually parse one.

## What we get from this

The kernel no longer *assumes* what machine it's on. It can be **told**.

Right now we still hardcode `0x0900_0000` for the UART, which is a fact we looked up. The DTB
is the machine telling us, and it also describes where RAM starts and ends (milestone 3 wants
that), where the interrupt controller lives (milestone 5 wants that), and how many CPUs exist.

## And it moves us toward the Pi

A Raspberry Pi boots a flat `kernel8.img`, not an ELF. It has no use for our ELF at all.

So this wasn't a detour from the Pi port. It was the first step of it.

---

*Add to this file as new boot-protocol details come up.*
