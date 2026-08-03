# The GUID Partition Table

Milestone 57 lane one, `crates/gpt`. The map that says where a filesystem starts, how to read one,
how to write one, and how to tell a broken one from a good one.

(The three letters are the UEFI disk format and predate the machine-learning sense of them by about
fifteen years. Nothing in this note is about a language model.)

## Why the OS needs it before it needs anything else about disks

`parted /dev/sda mkpart`, then `mkfs.ext4`, then mount. That is Chris's router setup, and cricker-os
had **no equivalent of the first step at all**. It is not optional, and not only because we might
want to partition something: **you cannot find a partition on a disk you did not create without
reading its table.** A block device hands you 131,072 blocks of undifferentiated bytes. Which of
them is the filesystem is written in the table and nowhere else.

So this is the mandatory half of the milestone even in the world where cricker-os never partitions
anything itself.

## What is actually on the disk

```text
  LBA 0     protective MBR   one fake record of type 0xEE covering the whole disk
  LBA 1     primary header   92 bytes: signature, geometry, two CRC32s, the disk GUID
  LBA 2..   entry array      128 entries x 128 bytes = 16 KiB = 32 blocks
  ...       the partitions, between first_usable_lba and last_usable_lba
  LBA n-33  backup array     the same 16 KiB again
  LBA n-1   backup header    the same header, my_lba and alternate_lba swapped
```

Four CRC-32s carry the whole integrity story: a header CRC and an array CRC, each stored twice. That
is the property worth naming, because it is unusual: **a GPT is a format that can tell you it is
broken.** Most on-disk structures cannot. It is also why every check in the crate is an error rather
than a warning: a table that half-validates is a table somebody is about to write a filesystem onto.

The header is 92 bytes of a 512-byte block, and the other 420 must be zero. The entry array is
128 entries whether or not you have 128 partitions; an entry is unused when its **type GUID** is all
zeros, which is the only thing that marks it, and not (this is the trap) its LBA fields.

## The crate does no I/O, and that is the whole design

Nothing in `crates/gpt` reads or writes a block device. Every function takes bytes the caller
already has and returns bytes the caller is about to place. Same discipline as `dtb` and `elf`, and
the reason is not tidiness: it is that the crate then compiles for the host, so its tests run in
milliseconds against disks that real tools made, instead of inside a QEMU boot. `#![no_std]`, no
allocation, no `unsafe`, and the entry array is a caller-supplied buffer so a kernel can hand it a
stack array.

The layering inside it is worth stating, because it is what makes the proofs possible:

| function | judges | can fail |
|---|---|---|
| `Entry::decode` | nothing | no. Every one of 2^1024 bit patterns is an entry |
| `Header::decode_fields` | nothing | no. Nine fields at nine offsets |
| `Header::decode` | one block on its own terms | signature, revision, size, both reserved regions, its own CRC |
| `Gpt::parse` | the table against the disk | array CRC, usable range, partitions off the end, partitions that overlap |
| `Gpt::check_backup` | the two copies against each other | nine fields, by name |
| `mbr::validate` | LBA 0 | signature, the protective record, hybrid MBRs |

`Entry::decode` being total is the same trick `ntp_proto` uses: decoding judges nothing, so the
round trip is provable over every input there is, and every rule lives in one place instead of being
scattered through the decoder.

## The three things that are easy to get wrong

### 1. A GUID is mixed-endian, and only partly

Everybody writes `C12A7328-F81F-11D2-BA4B-00A0C93EC93B`. The five groups are **not** stored the same
way. The first three are integers and go on disk little-endian; the last two are byte strings and go
in the order written:

```text
  28 73 2A C1  1F F8  D2 11  BA 4B  00 A0 C9 3E C9 3B
  \--------/   \---/  \---/  \---/  \-----------------/
   u32 LE      u16 LE u16 LE       as written
```

This is Microsoft's `GUID` struct layout, inherited. Get it wrong and you have a GUID that looks
entirely reasonable, matches nothing, and is byte-reversed in three groups out of five. `Guid` keeps
the on-disk bytes and does the swapping in two functions, one of which is proved.

### 2. `last_lba` is inclusive

A one-block partition has `first_lba == last_lba`. Two partitions where one ends on the block the
next begins on **overlap**, and two filesystems then fight over one sector forever. Half-open
thinking gives you an off-by-one that a casual test will not find, because the tables real tools
write are 2048-aligned with gaps.

### 3. The entry array's size is a `u32` times a `u32`

`entry_count * entry_size` from a hostile header overflows `usize` on a 32-bit target. The crate
returns an `Option` there rather than saturating, because saturating means CRC-ing a shorter buffer
than the header claimed and then declaring the table good.

## What the fixtures taught us

The tests run against two tables this crate did not write, from implementations that share no code:
`sgdisk` 1.0.10 (gptfdisk, C++) and macOS `diskutil`. Both are committed as the first 34 and last 33
blocks of a 64 MiB image, about 34 KB each. Regeneration commands are at the top of
`crates/gpt/tests/real_disks.rs`.

A parser that only round-trips its own output proves nothing, because every mistake it makes going
in it makes symmetrically coming out. What the two independent writers bought:

- **macOS does not write GPT partition names.** The image was made with
  `diskutil partitionDisk ... MS-DOS ESP 16M HFS+ MACDATA 0b`, both volumes are named, and both GPT
  entries have 72 bytes of zero where the name goes. On a Mac the volume name lives in the
  filesystem and the partition table is not asked. `sgdisk` and Linux tools do write it. **The
  consequence is a design constraint on anything built on this crate**: a partition browser cannot
  key off the GPT name, because on any disk a Mac touched there is not one. Identify by type GUID, or
  by unique GUID for a particular partition.
- **The two tools write different CHS bytes and both are right.** `sgdisk` computes the real
  cylinder/head/sector of the last block (`28 20 08`); macOS writes `FE FF FF`, the "out of range,
  use LBA" marker. Nothing has read a CHS field in thirty years. A validator written from one tool's
  output would have rejected the other's disk outright, and `mbr::validate` therefore checks the two
  fields that do the protecting (start at LBA 1, cover the disk) and ignores CHS.
- **Both write 128 entries of 128 bytes, and identical geometry**, arrived at independently. The
  spec does not require it; it is just what everybody does.

Re-emitting `sgdisk`'s table reproduces its bytes exactly, and *rebuilding* it from its parsed
description with `Gpt::create` also reproduces its bytes exactly, which is the strongest available
statement about the writer: not "our writer agrees with our reader" but "our writer agrees with
gptfdisk".

## Hybrid MBRs are refused, deliberately

A hybrid MBR puts real partition records beside the protective one so a GPT-unaware system can boot
from them. Boot Camp made a generation of them. It is also the format's worst failure mode: one disk
described twice, by two tools that will not agree for long. `mbr::validate` returns
`MbrProblem::ExtraRecord` so the message can say what it found rather than "invalid MBR". Relaxing
this is a decision somebody can make later with the error already named.

## The cricker-os partition type GUID

`EC5CC08B-D749-4434-AC38-A274C50385BA`, `gpt::guid::types::CRICKER_DATA`, a cricker-os data partition
holding a RedoxFS volume. A random version-4 GUID generated on 2026-07-30 and **fixed forever**: a
type GUID's whole job is to not collide with anybody else's and there is no registry to ask. See
DECISIONS §45 for why it is never to change; the short version is that the recovery story ("the
board is dead, can I get my data") depends on a `sgdisk -p` five years from now still showing it.

## How it is proved, and the division of labour

This crate is the clearest case yet of the rule `ntp_proto` wrote down: **where a domain is small
enough to count, count it; a model checker is for the domains that are not.**

### Counted, exhaustively

| what | cases | time |
|---|---|---|
| every single-byte corruption of `sgdisk`'s **header block** | 130,560 | 1 s |
| every single-**bit** corruption of `sgdisk`'s 16 KiB **entry array**, plus every byte complemented | 147,456 | 4 s |
| every single-byte corruption of a small (4-entry) table, header and array both | 261,120 | instant |
| every single-byte corruption of `sgdisk`'s entry array | 4,177,920 | **163 s**, `#[ignore]` |

Each of these is *complete* for its table: not a sample, not a bound. The last one is committed but
ignored, because 4.18 million CRC-32s over 16 KiB each is 68 GB of polynomial. It was run and it
passes; the bit-flip sweep is what the gate runs.

The 163 seconds is worth recording as a measurement rather than a guess: a byte-at-a-time
table-driven CRC-32 runs at about **400 MB/s**, roughly eight cycles per byte, because the table
load is a serial dependency in the loop. That is the number that decided the test structure.

### Proved, symbolically

`script/verify` runs eight harnesses over `gpt`, 95 seconds in total on an M-series laptop:

| harness | what it quantifies over | time |
|---|---|---|
| `crc32_matches_its_bitwise_definition` | every 8-byte input | 20 s |
| `a_single_byte_change_always_changes_the_crc` | every 8-byte buffer, every position, every replacement | 33 s |
| `an_entry_survives_the_round_trip` | all 2^1024 partition entries | 8 s |
| `a_guid_survives_printing_and_parsing` | all 2^128 GUIDs, out through text and back | 5 s |
| `overlap_is_exactly_sharing_a_block` | every pair of LBA ranges (complete, not bounded) | 2 s |
| `a_headers_fields_survive_the_round_trip` | every value of all nine header fields | 3 s |
| `the_header_fields_partition_the_block` | every 512-byte block: no two fields share a byte | 2 s |
| `create_never_lays_out_a_table_parse_would_reject` | every disk size and every partition placement | 22 s |

Three of these need their reasoning spelled out, and one of them is a correction.

**Why the CRC harnesses are bounded at 8 bytes.** CRC-32 is a shift-and-xor over every bit of the
input, so the formula the solver gets grows with the byte count. The bound does not buy "CRC-32 is
correct for short inputs", which would be a nearly worthless claim; it buys the *structure*: that
the table-driven implementation agrees with the bitwise definition, and that a changed byte always
changes the answer. Both are properties of the algorithm rather than of the length, and the lengths
that actually occur (92 bytes, 16 KiB) are covered exhaustively above, over bytes real tools wrote.

The table-versus-definition harness earns its keep specifically because the table is derived by
`const` evaluation from the polynomial. A wrong table that happened to be self-consistent would
round-trip our own output perfectly and pass every test in the suite except the published check
values, and it would fail on every real disk.

**A symbolic CRC-32 is very expensive, and it decided the shape of two harnesses.** The first
attempt at the header round trip went through `encode_into` and `decode`, so the formula carried two
92-byte CRC-32s over symbolic bytes. It ran **274 seconds without finishing**, against a
`script/verify` budget of about three minutes per harness (the same budget the DMA harnesses record
in notes/verification.md). The measured reason is in the two CRC harnesses above: one table CRC plus
one bitwise CRC over 8 symbolic bytes is 20 s, two table CRCs over the same 8 bytes is 33 s, so **the
table version costs roughly five times the bitwise one** for the solver. A table lookup on a symbolic
index is a 256-way multiplexer, and there is one per byte from the first symbolic byte onward.

So `Header` grew a `decode_fields`/`encode_fields` pair, and the round trip is proved in two halves
that compose for a reason you can read in three lines of source rather than assume: everything
`encode_into` adds is four bytes at offset 16, everything `decode` adds is checks, and the CRC field
is not one of the nine fields, so neither half can disturb the other. **3 seconds instead of 274**,
and it is a better decomposition anyway: the layout and the integrity are different claims.

The same reasoning moved the create-then-parse harness. A byte-level version has four symbolic CRC
chains in it and did not finish either. The byte-level identity is proved instead where it can be
proved *completely*, in `tests/real_disks.rs`: `Gpt::create` reproduces `sgdisk`'s 512-byte header,
its backup header and its 16 KiB array exactly. What that test cannot do is vary the disk size, so
the harness takes that dimension and asserts every geometry rule `Gpt::parse` enforces on what
`Gpt::create` produced, for **every** `u64` disk size. A writer that emits a table its own reader
rejects is the worst failure available to this crate, and an unusual disk size is exactly where it
would hide.

`overlap_is_exactly_sharing_a_block` is the one that is complete rather than bounded, because it is
pure integer comparison. It states the property as its definition (the intersection is non-empty)
rather than as the implementation, so the proof is not the code compared against itself.

## What this crate does not do

Stated plainly, because a demonstrator's docs are part of the deliverable:

- **No I/O, by design.** Somebody still has to read LBA 1. That is the block-device lane of
  milestone 57, and it is separate on purpose. **Built 2026-08-03** (notes/block-devices.md):
  `disk_surveyor` reads the table off a virtio-blk device, backup half included, on both ISAs. The
  crate gained one module for it, `gpt::span`, which computes *where* to read when the disk's
  logical block (512) and the block service's transfer unit (4096) are different numbers. That is
  arithmetic, not I/O, and it lives here because three open-coded divisions in a driver is how an
  off-by-one gets blamed on a CRC.
- **No unique GUID generation.** `Entry::new` takes both GUIDs from the caller. A GUID that is not
  random is not unique, this crate has no randomness, and inventing one from a counter would be
  worse than refusing. Milestone 55's entropy work is where a caller gets one.

  **This turns out to be the same wall `mkfs` on the target hits, and that is worth knowing**
  (measured 2026-08-01, design/roadmap.md's milestone 57 block). RedoxFS stamps a v4 UUID into a
  fresh header with `uuid::Uuid::new_v4()`, so `FileSystem::create` is `std`-gated for the same
  reason this crate refuses to invent a GUID: an identifier that has to be unique needs randomness,
  and neither a `no_std` engine nor a pure-computation crate has any. Partitioning a real drive from
  cricker-os and formatting one are therefore blocked on the same thing, the entropy service reaching
  the program that does them, and on nothing else.
- **No alignment policy.** `Gpt::create` places partitions exactly where it is told. The 2048-block
  (1 MiB) convention that keeps a partition off an SSD erase-block boundary is policy, and a format
  crate that silently moved a partition would be doing policy behind its caller's back.
- **Entry sizes over 128 bytes are read as 128.** The spec allows the entry to grow; nothing writes
  a bigger one, and decoding the first 128 bytes and ignoring the rest is what a reader that does
  not know a later revision can honestly do.
- **The reserved-tail check is the strictest thing here**, and the likeliest to need relaxing. UEFI
  2.10 §5.3.2 says the block after the 92-byte header must be zero, both fixtures comply, and a
  header with a perfectly good CRC is still refused if one byte after it is not zero. If a real disk
  ever trips it, relax the check and record the disk; do not weaken the CRC.
- **4K-native disks are tested but not witnessed.** The block size is taken from the length of the
  block the caller passes, there is a test that builds and parses a 4096-byte-block table, and no
  real 4Kn disk has been through it because we do not have one.
