# 58. A wider archive name, and the one format change that had to bump the magic

2026-08-01, ahead of milestone 63's rename sweep. `crates/crickerfs`. See `notes/crickerfs.md`.

| | Before | After |
|---|---|---|
| `NAME_LEN` | 24 | **32** |
| `ENTRY_LEN` | 32 | **40** |
| `DIR_BLOCKS` | 4 | **6** |
| `MAX_FILES` | 63 | **76** |
| `MAGIC` | `CRKR0001` | **`CRKR0002`** |

## Why 32 and not more

`os_primitives_benchmarker` at 25 bytes is the longest name anyone has argued for, and 32 clears it
by seven. **Buying headroom beyond that buys nothing**, because there is no data migration: every
image regenerates from this crate, so the *next* raise is exactly as cheap as this one. Every extra
eight bytes of name is eight bytes off every entry in every image, paid now for a name nobody has
proposed.

Only widened. No shape change, no header change, no block-aligned entries; those would buy a
regularity nothing currently needs.

## The magic bumped, and the contrast is the reasoning

The `DIR_BLOCKS` change of 2026-07-30 **correctly did not bump**, because `start_block` is absolute:
no reader could tell the difference, so a version change would have broken the blk driver's hardcoded
check for nothing.

**A wider entry is the opposite case.** A reader still striding 32 bytes finds a plausible name at
the wrong offset and returns **the wrong file rather than an error**. Silence is the worst failure a
format can have, and turning it into `BadMagic` is exactly what a version field is for. It also
forced every reader to be visited, which is how the third one was found.

## Two things the plan got wrong, both caught by measuring

**The kernel-stack cost was already gone.** `DECISIONS` and the roadmap both stated the trade as
costing kernel stack, because `Fs` held `[Entry; MAX_FILES]` as a stack local on the boot and spawn
paths, and the FS server had once died 528 bytes short. **That stopped being true at `b9f4382`**, the
FS-server stack fix: `size_of::<Fs>()` is 24 bytes before and after, independent of `MAX_FILES`. The
statement was stale when it was written down, and a host test now pins it, so reintroducing the array
fails in milliseconds rather than faulting a guard page at boot.

**The `DIR_BLOCKS` margin was zero, not thin.** Widening alone drops `MAX_FILES` to 50, and **the
riscv64 initrd holds exactly 50 files**. It would have built once and failed on the next program
added. The reasoning behind the plan used the aarch64 count, 46, and never checked the other leg,
which is the parity discipline (§19) failing at the level of an estimate rather than a test.

## Two defects the change surfaced

- **`write_image` silently truncated an over-long name.** Two names agreeing in their first
  `NAME_LEN` bytes merged into one directory entry, and `init` loaded whichever was packed first. Now
  `Error::NameTooLong`, checked before a byte is written. Silent truncation in a *writer* is the same
  class of bug as a missing magic bump in a reader.
- **The format had a third reader nobody tracked.** `crates/virtio` restated the offsets by hand:
  stride 32, start block at +24, a literal `min(15)` bound. It now depends on `crickerfs` for
  `HEADER_LEN`, `ENTRY_LEN`, `NAME_LEN` and `ENTRIES_IN_FIRST_BLOCK`. It had to be found by hand,
  which is the argument for the magic bump restated as a fact.

## BUGS

- **`MAX_FILES` is a ceiling that grows with the suite rather than the system**, and the failure mode
  is invisible to each lane that causes it: every branch fits on its own and the union crosses the
  line at merge. It went 31 to 63 on 2026-07-30 for exactly this reason, and 63 to 76 here.
- **Nothing checks that a name fits before a rename lands.** `script/lint` gates many things; the
  archive limit is not one of them, and it was hit twice in one session by names that were settled
  before anyone counted bytes.
