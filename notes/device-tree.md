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
| 5 | `intc` — where the GIC is, and which interrupt the timer uses |
| 8 | `virtio_mmio` — where the block device is |
| Pi port | all of it, because none of the addresses will match |

---

*Add to this file as new device tree concepts come up.*
