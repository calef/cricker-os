# 77. `crates/paging`: a module per ISA, a type per page-table configuration

**Status: NOT-STARTED**, and deliberately **waiting for a trigger**. Split out of milestone 73 on
2026-08-03 once it stopped being a rename.

**Gate: MILESTONE 24.** Deliberately waiting for a trigger rather than for a lane: the second
aarch64 page-table configuration is what names the axis, and the block refuses to guess between a
granule change and a VA-width change. Milestone 24's Virtualization.framework board is the named
candidate (Apple Silicon's 16 KiB granule); milestone 88's server shapes could supply it instead.

## What is wrong, stated precisely

`crates/paging` exports two implementations of one `PageFormat` trait:

| type | `LEVELS` | granule | VA bits |
|---|---|---|---|
| `Sv39` | 3 | 4 KiB | 39 |
| `Aarch64` | 4 | 4 KiB | 48 |

Both are **configurations**. `Sv39` names one; `Aarch64` names an architecture while describing one.
A reader meeting `paging::Aarch64` beside `paging::Sv39` has to know that the first is not the general
aarch64 case in order to read the second correctly.

## The asymmetry is not ours, which is why the obvious fix is wrong

**RISC-V enumerates its configurations and names each one**: Sv39, Sv48, Sv57. The name *is* the
configuration. **ARM parameterises instead**: there is one format, and the level count and VA width
fall out of `TCR_EL1.T0SZ` and the granule field. The 4 KiB, 48-bit, 4-level arrangement has no short
ARM name; you describe it.

So renaming `aarch64.rs` to `vmsav8_64.rs` trades a name that is under-specific for one that is
**over**-specific: VMSAv8-64 also covers the 16 KiB and 64 KiB granule configurations this file does
not implement. And renaming `sv39.rs` to `riscv64.rs` is worse, because Sv48 would then have nowhere
to live, and CLAUDE.md is explicit that a standard term a reader already knows is the best name
available.

## The shape, when it is time

A module per ISA, which is rule 1's shape and what `kernel/src/arch/<isa>/` has done since milestone 1:

```
crates/paging/src/
  aarch64/mod.rs      the aarch64 configurations
  riscv64/mod.rs      pub struct Sv39;   (and Sv48, milestone 60)
  domain.rs
  lib.rs              trait PageFormat
```

**The module carries the ISA and the type carries the configuration**, so each architecture names its
configurations in its own vocabulary without the flat namespace forcing them into one list. The
asymmetry stops being visible: you never see `Sv39` beside `Aarch64` again, which is the whole of what
made it jarring.

## Why this WAITS, and what the trigger is

**We do not know the second aarch64 configuration yet.** Apple Silicon's native 16 KiB granule is the
best guess (milestone 24's Virtualization.framework board would meet it), but a VA-width change
(52-bit, LPA2) varies `LEVELS` and `SPLIT_SHIFT` instead, and those two possibilities want different
names. `Granule4K` is a false claim if what actually varies is the VA width.

CLAUDE.md's rule applies directly, and milestone 60's entry states it for this exact situation: **do
not build a chip abstraction on one configuration; the second one should tell us what the abstraction
is.** Doing this early means renaming 174 call sites across 16 files twice if the guess is wrong, and
the current name blocks nothing until a sibling actually exists.

So the trigger is the arrival of a second aarch64 configuration, and the deliverable until then is
this entry plus a comment in `crates/paging/src/lib.rs` pointing at it, so the next reader who notices
the asymmetry finds the reasoning instead of filing it again.

## Scope note

When it happens: module move, type rename, and every call site. `Aarch64` and `Sv39` appear **174
times across 16 files**, including `kernel/src/arch/*/mmu.rs`, both `iommu.rs` files, and
`crates/paging/tests/mapping.rs`. No behaviour change, and milestone 69's proof obligation applies.
