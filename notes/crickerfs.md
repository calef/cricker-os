# crickerfs: the boot archive, and the day its name limit had to move

`crates/crickerfs` is the read-only, flat, fixed-everything archive the kernel is handed as an
initrd and the EL0 blk driver reads off a disk. One crate defines the format and every reader uses
it, which is why a format change is tractable at all.

This note is the record of the 2026-08-01 change: `NAME_LEN` 24 to 32, `DIR_BLOCKS` 4 to 6, and the
magic `CRKR0001` to `CRKR0002`. The crate's own header is the reference; this is the reasoning.

## Why it moved

Names in this system are decided by what a program does, and the naming tenet (CLAUDE.md) says who
gets to decide them. By 2026-08-01 the archive limit had quietly joined that conversation:

| Settled name | Bytes | Against `NAME_LEN = 24` |
|---|---|---|
| `fs_subtree_caretaker` | 20 | four bytes spare, so a fourth qualifier does not fit |
| `sub_server_supervisor` | 21 | three spare |
| `os_primitives_benchmarker` | 25 | **does not fit at all** |

Milestone 63 needs the last of those, so raising the limit is a prerequisite rather than a tidy-up.
It was done first and on its own merits, deliberately: choosing a worse name to fit a limit, and
raising a limit because a name demanded it, are both the wrong way round.

## The trade, measured

`NAME_LEN` sits inside the directory entry, so widening the name widens the entry and fewer entries
fit in a block.

| | Before | After |
|---|---|---|
| `NAME_LEN` | 24 | **32** |
| `ENTRY_LEN` (name + two `u32`s) | 32 | **40** |
| `DIR_BLOCKS` | 4 | **6** |
| Directory bytes in every image | 2048 | **3072** |
| `MAX_FILES` | 63 | **76** |
| Entries visible in block 0 alone | 15 | **12** |
| Files in the aarch64 initrd | 46 | 46 |
| Files in the riscv64 initrd | 50 | 50 |
| Image bytes, aarch64 initrd | 5 886 976 | 5 888 000 |
| `size_of::<Fs>()`, the kernel-stack charge | 24 | **24** |

Two of those rows are the whole decision.

**`DIR_BLOCKS` had to move with `NAME_LEN`, and the margin was zero rather than thin.** Widening
alone would have taken `MAX_FILES` from 63 down to 50, and the riscv64 initrd holds **exactly 50
files**. It would have built, once, and the next program added to it would have failed. That was
measured after the fact rather than predicted, which is the argument for measuring: the plan in the
roadmap reasoned about the aarch64 archive, which carries 46, and the riscv64 one carries four more
because its `init` is a separate program from `hello`.

That ceiling gets crossed by lanes that cannot see each other. It went from 31 to 63 on 2026-07-30
when three of them landed together and made 32 files, so the cost is invisible to every branch that
causes it and lands on whoever merges. Six blocks put the ceiling above where it was, at 76, for 1 KB
of image bytes in a multi-megabyte initrd. Five blocks would have given exactly 63, which optimises
for the number looking unchanged rather than for the failure it exists to prevent.

**The kernel-stack cost is zero, and that is new.** `Fs` used to copy every directory entry into a
fixed `[Entry; MAX_FILES]`, and `Fs` is a stack local on the kernel's boot and spawn paths, so
`MAX_FILES` was kernel stack: raising it to 63 on 2026-07-30 overflowed a four-page kernel stack the
same day, faulting on the guard page while parsing the initrd. That array was removed in the fix, and
entries are decoded from the borrowed image one at a time. So the roadmap's statement of the trade
("it costs kernel stack, because `Fs` holds `entries` as a fixed array") was **already stale when it
was written**, and the raise was much cheaper than the plan expected. The only per-entry stack left is
one decoded `Entry`, which went from 32 bytes to 40.

`crates/crickerfs`'s test `fs_does_not_grow_with_max_files` pins that: it asserts `size_of::<Fs>()`
is a borrowed slice plus a count, so reintroducing the array fails a host test in milliseconds
instead of faulting a guard page during boot.

## Why 32 and not more

`os_primitives_benchmarker` at 25 is the longest name anyone has argued for, and 32 clears it by
seven bytes. Every extra 8 bytes of name is 8 bytes off every entry in every image, and the next
raise is exactly as cheap as this one was, because **there is no data migration**: every image
regenerates from this crate on every build. Buying headroom now buys nothing that cannot be bought
later at the same price.

## Why the magic changed, when last time it did not

`CRKR0001` survived the 2026-07-30 two-to-four-block change on an explicit argument: `start_block` is
an **absolute** block number, so no reader has to know `DIR_BLOCKS` to find data, and bumping the
version would only have broken the blk driver's hardcoded check for no reader-visible gain.

The same argument bumps it here, because a wider entry is the opposite case. A reader still striding
32 bytes finds a plausible NUL-terminated name at the wrong offset and a start block cut out of the
middle of a name, and returns **the wrong file rather than an error**. That is the failure a version
field exists to prevent: `Error::BadMagic` instead of silence. It also makes a stale `target/*.img`
from before the change fail loudly, and it forced every reader to be visited rather than found later.

## The three readers

A format change has to reach all of them, and one of them was the reason this needed care.

1. **The kernel** (`kernel/src/user.rs`, `kernel/src/main.rs`), which parses the initrd to find
   `init`. Uses `crickerfs::Fs`.
2. **`xtask`** (`mkinitrd`, `mkinitrd_riscv`, `mkdisk`), which writes every image and then parses it
   back to hash the boot programs. Uses `crickerfs::write_image` and `Fs`.
3. **The EL0 blk driver** (`crates/virtio`), which walked the directory out of a 512-byte DMA buffer
   with the offsets **restated by hand**: stride 32, start block at +24, a `count.min(15)` bound with
   the 15 written as a literal. It was the only place in the tree that had to be found rather than
   recompiled, so it now depends on `crickerfs` and takes `HEADER_LEN`, `ENTRY_LEN`, `NAME_LEN` and
   `ENTRIES_IN_FIRST_BLOCK` from it. That is CLAUDE.md's rule 7 applied to a constant instead of a
   module: what two binaries must agree on is a crate.

## A silent truncation, found on the way and fixed

`write_image` used to write `name.len().min(NAME_LEN)` bytes, so a name that was too long was
**silently truncated**. Two names agreeing in their first `NAME_LEN` bytes become one directory
entry, and `init` then loads whichever program was packed first, arbitrarily far from the edit that
caused it. Packing `os_primitives_benchmarker` under the old limit would have produced
`os_primitives_benchmark` and no error at all.

It is now `Error::NameTooLong`, checked for every name before a byte is written, so a rejected
archive is never a half-written one. The build stops and names the file. This is why the limit is
worth stating where a reader meets it (the crate's `# Names` and `# BUGS` sections) rather than only
in a constant.

## BUGS

- **A reader holding one block cannot see the whole directory.** The EL0 blk driver buffers block 0
  only, so it can find the first `ENTRIES_IN_FIRST_BLOCK` files (12) and no more. It walks the tiny
  three-file test disk, not the initrd, so nothing hits it today, and nothing in the format announces
  the limit either.
- **`MAX_FILES` still grows with the suite, not with the system.** 76 is 26 clear of the fuller of
  the two initrds (riscv64, at 50), and the pressure on it is the number of programs the test suite
  ships. The real fix, if it recurs, is not another `DIR_BLOCKS` bump but a directory whose size is
  written in the superblock, so a reader learns it from the image instead of from a constant every
  reader has to agree on.
- **The archive has no directories, no writes, and no permissions.** That is the design, not a gap:
  the read-write filesystem is the RedoxFS server in `fs-server/` (DECISIONS §34).
