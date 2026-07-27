# RedoxFS std-footprint audit (milestone 32 costing)

Audited 2026-07-27 against redoxfs **0.9.1** (gitlab.redox-os.org/redox-os/redoxfs, shallow
clone), by building it, not by reading about it. Verdict up front: **the port is cheap, and
milestone 32's risk (1) is retired.** The core compiles for both of our bare-metal targets
today, three missing imports away from clean.

## What the crate actually is

`std` is a **feature**, not an assumption: `#![cfg_attr(not(feature = "std"), no_std)]` with
`extern crate alloc` at the top of the lib. The `[features] std = [...]` set pulls the host
conveniences (getrandom, uuid/v4, fuser, termion, the five host binaries). The core that
remains without it, allocator, block layer, tree, htree, dir, node, record, transaction,
about **5,400 lines**, has zero `cfg(feature = "std")` gates in the operational paths.

## Proven by building

With `--no-default-features` and TWO added `use alloc::vec::Vec` lines, one each in
`filesystem.rs` and `record.rs`, fixing three E0425 sites (with std on, the prelude supplies
`Vec`; upstream does not CI the no_std path, so it bit-rotted):

```
cargo build --no-default-features                                        ok
cargo build --no-default-features --target riscv64imac-unknown-none-elf  ok
cargo build --no-default-features --target aarch64-unknown-none-softfloat ok
```

Same pinned nightly as the kernel. The whole dependency graph (aes, xts-mode, argon2,
lz4_flex, seahash, bitflags, endian-num, base64ct, uuid-core, redox_syscall) compiles for the
`none` targets; `redox_syscall` contributes only its error type here, which is the crate's
`Result` everywhere and maps cleanly onto our ABI errors at the server boundary.

## The seams, confirmed

- **`Disk` is three synchronous methods**: `read_at(block, &mut [u8])`, `write_at(block,
  &[u8])`, `size()`. That is precisely the shape of a blk-IPC client; the milestone-32 Disk
  impl is small.
- **What is std-gated in the core types is exactly creation**: `FileSystem::create`,
  `Header::new` (uuid v4), `Key::new` (getrandom). **Opening and operating an existing
  filesystem is fully no_std**, which matches the plan: `mkfs` and image inspection stay on
  the host (the FUSE half), cricker-os opens and serves.

## The real costs, priced

1. **A `GlobalAlloc` in the FS-server process.** The core is alloc-heavy and our userspace has
   no allocator yet. This is the untyped-backed allocator milestone 27's PAL needs anyway;
   whichever milestone lands first builds it, the other inherits it. This is the largest cost,
   and it was already on the books.
2. **The two-line import patch**, worth offering upstream (with a `--no-default-features` CI
   check so it stays fixed) so the pin can eventually drop it.
3. **Version pin at 0.9.1** with divergence recorded, per the vendored-engine discipline. One
   manifest wart to carry or upstream: `libc` is an unconditional dependency used only by the
   std binaries (harmless on `none` targets, proven, but it belongs behind the feature).
4. Unconditional crypto/compression deps (aes/xts/argon2/lz4) ride along even with encryption
   unused; binary-size bloat only, not a correctness or porting cost.

Not costs: no async runtime, no threads assumed, no libc reach-ins from the core, no
allocator assumptions beyond `alloc`.
