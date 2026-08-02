# The device tree

**The machine describing itself.** Where RAM is, where the UART is, where the interrupt
controller lives, how many CPUs exist, what firmware has already claimed.

The alternative is hardcoding, which is what milestone 1 did (`0x0900_0000` for the UART,
read off a `dtc` dump by hand). Hardcoding gives you a kernel that runs on exactly one
board. The device tree gives you a kernel that can be *told* what board it's on.

QEMU hands us a pointer to one in `x0`, but **only because we ship a flat arm64 Image**.
See [boot-protocol.md](boot-protocol.md).

## Everything is big-endian

The FDT format predates the little-endian consensus and never changed. **Every integer in
the blob is big-endian**, on a machine that is little-endian.

Forget one byte-swap and you get a plausible-looking number that is wrong by a factor of
16 million, which is exactly the kind of bug that survives a code review. Our parser routes
every read through `be32` or `be64` so there is no path that forgets.

Even the magic: `0xd00dfeed`, stored big-endian. The kernel test that validates the pointer
does `u32::from_be(magic)`.

## The layout

```
+------------------+
| header (40 B)    |  magic, totalsize, and offsets to the three blocks below
+------------------+
| memory reserve   |  (address, size) pairs. DON'T TOUCH THESE.
|   block          |  Terminated by an all-zero entry.
+------------------+
| structure block  |  the tree itself, as a token stream
+------------------+
| strings block    |  every property name, deduplicated, null-terminated
+------------------+
```

**The reservation block is deliberately dead simple**, and it comes first, precisely so a
kernel can honour it without parsing anything. It's the firmware saying "I have things in
here." QEMU's `virt` leaves it empty; real boards often don't, and a kernel that skips it
will happily allocate over the firmware's own tables.

## The structure block is a token stream

Five tokens, all 4-byte aligned:

| Token | Followed by |
|---|---|
| `FDT_BEGIN_NODE` (1) | a null-terminated node name, padded to 4 bytes |
| `FDT_END_NODE` (2) | nothing |
| `FDT_PROP` (3) | `len`, `nameoff` (an index into the strings block), then `len` bytes of value, padded |
| `FDT_NOP` (4) | nothing. Lets a bootloader blank out a node in place, without rewriting the blob. |
| `FDT_END` (9) | nothing. Done. |

Property *names* live in a separate strings block and are referenced by offset, because
`#address-cells` appears hundreds of times in a real tree and storing it once is worth the
indirection.

## The part that will bite you: cells

A `reg` property is a list of (address, size) pairs. **But how many 32-bit words each of
those takes is not fixed.**

It's declared by `#address-cells` and `#size-cells` **on the parent node**. So to decode a
`/memory` node's `reg`, you first need the *root's* cell counts.

```dts
/ {
    #address-cells = <0x02>;      // addresses are 2 cells = 64 bits
    #size-cells = <0x02>;         // sizes are 2 cells = 64 bits

    memory@40000000 {
        reg = <0x00 0x40000000 0x00 0x8000000>;
        //     \______________/  \___________/
        //      address (2 cells)  size (2 cells)
        //      = 0x4000_0000      = 128 MiB
    };
};
```

The spec's *defaults* are 2 and 1, which almost nothing uses. Read them rather than assume;
our parser does, and it's one of the two things (with endianness) most likely to be silently
wrong.

## Reading one yourself

```bash
qemu-system-aarch64 -machine virt,dumpdtb=virt.dtb -cpu cortex-a72 -nographic
dtc -I dtb -O dts virt.dtb | less
```

`dtc` ships with QEMU via Homebrew. Genuinely worth ten minutes of scrolling: it is a full
description of the machine we've been booting, and after milestone 3 we're only reading the
first two lines of it.

**One gotcha:** QEMU pads its dump to a full megabyte and *says so in the header*, so the raw
dump is a 1 MB file describing 7 KB of tree. Round-trip it through `dtc -I dtb -O dtb` to
compact it. That's how the test fixture in `crates/dtb/tests/fixtures/` was made.

## What we read, and what we ignore

Today we read exactly two things: the `/memory` nodes (where RAM is) and the reservation
block (what not to touch). That's all milestone 3 needs.

Everything else is still hardcoded, including the UART. **That is correct and should stay
that way for now**, and the reason is a nice chicken-and-egg: the parser is the thing most
likely to have a bug, and `println!` is how you'd debug it. So the console has to come up
*before* the device tree is parsed, which means the console cannot depend on it.

Later milestones read more:

| Milestone | Wants |
|---|---|
| 5 | `intc`, where the GIC is, and which interrupt the timer uses |
| 8 | `virtio_mmio`, where the block device is |
| Pi port | all of it, because none of the addresses will match |

## BUGS

**The blob is the first thing this kernel reads and the last thing it can check against anything
else.** The pointer comes from firmware, before there is a frame allocator or any way to report a
failure, so every limitation here is a limitation on the boot path specifically.

- **A node nested deeper than 16 is invisible to `node_reg` and `node_reg_compatible`.** Both carry
  `#address-cells`/`#size-cells` down a fixed 16-entry per-depth stack, and past it they stop
  tracking, so they refuse to match rather than decode a region with cell widths they no longer
  know. Nothing in the trees we boot nests past 4. `node_prop` keeps no per-depth state and so has
  no such limit, which means the two lookups disagree about how deep a tree can be. Until
  2026-08-02 `node_reg` did **not** refuse: it matched at any depth and then indexed past the stack,
  which was an out-of-bounds panic on a 17-deep tree (milestone 42, notes/fuzzing.md).
- **A `reg` pair whose `start + size` wraps 64 bits is `Error::RegionOverflow`, not a clamped
  region.** `kernel/src/memory.rs` decides where RAM is from these, and "the firmware's memory map
  is impossible" is something a boot path should be told. `Region::end()` saturates as a backstop,
  because the type is `pub` with `pub` fields.
- **`memory_regions` finds `/memory` nodes by NAME, not by `device_type`.** `device_type = "memory"`
  is the more correct check and arrives after the node name; the name is unambiguous on every board
  we have met. A board that spells it otherwise would have its RAM missed entirely.
- **`node_reg` matches a node-name prefix rather than `compatible`.** That is a deliberate
  simplification for two boards, documented at the function. `node_reg_compatible` is the correct
  one and is what milestone 51's RTC lookup uses.
- **Nothing validates the structure block's nesting balance.** A blob with more `FDT_END_NODE`
  tokens than `FDT_BEGIN_NODE` is walked rather than refused: the three `usize` walkers saturate
  their depth at zero and the three `i32` ones let it go negative. Neither can index anything (every
  array access is guarded and the negative depths simply match nothing), so it is a wart rather than
  a hole, but an unbalanced tree is not told apart from a well-formed one.

`crates/dtb/tests/hostile.rs` holds the regressions for the first two, hand-built rather than
fuzzer-minimized, so a reader meets the attack next to the code.

---

*Add to this file as new device tree concepts come up.*
