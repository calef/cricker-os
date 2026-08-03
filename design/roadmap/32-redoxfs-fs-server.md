# 32. A real filesystem: RedoxFS behind a capability FS server

**Status: BUILT.**

**In brief.** A write-capable block path, an FS-server **component** whose handles are capabilities from birth (open-by-path exists only INSIDE the server, relative to a granted directory cap), and **RedoxFS** as the on-disk engine, ported behind its own `Disk` trait over blk IPC

**Why it matters.** the flagship **userspace-reuse** story the prior-art note predicted: a real CoW filesystem we did not write, running confined; and the thing 31's per-file grants point at

**Phase 1 built** (the write-capable block path; DECISIONS §22 area, notes/dma.md). **Phase 2
built, read path** (2026-07-28; DECISIONS §27, notes/fs-server.md): RedoxFS runs confined as a
three-process userspace service (block server over blk IPC, FS server over the vendored no_std core
with its own untyped heap, client holding a directory capability), and a client opens the shipped
`motd` through a granted directory capability and reads it back, proven on aarch64 and riscv64 with
a host-tool consistency check. The contract is capability-shaped from birth and adds no syscall; the
error type maps to the wire exactly once; creation stays host-side. **Phase 2 write path proven
too** (2026-07-29, through `std::fs`): the old "on-device writes loop inside RedoxFS's allocator
commit" open item was stale and is corrected in DECISIONS §27 and notes/fs-server.md. A guest write
now reads back when the host tool reopens the image, and that reopen is in the gate. What remains is a
*contract* gap, not a write-path one: no `CREATE` and no `TRUNCATE` verb, so `std::fs::write` and
`File::create` are honestly Unsupported and a write means opening a file the image already carries.

**Deliverable.** Three pieces. A **write-capable block path** (the driver and the confinement
validator already speak both directions; the write verbs and tests are the new work). An
**FS-server component** whose contract is capability-shaped from birth: a file handle is a
capability; open-by-path exists only inside the server, resolved relative to a *granted
directory capability*, so designation keeps flowing the 31 way and no global namespace ever
appears. And **RedoxFS as the on-disk engine**: port the `redoxfs` core behind its own `Disk`
trait, implemented over blk IPC.

**Why RedoxFS.** The prior-art survey named it the best single candidate the day the reuse
rule was written: a real CoW, transactional filesystem in Rust, MIT-licensed, shipping daily in
Redox, and only loosely coupled to Redox's syscalls precisely because it also runs on
Linux/FUSE, which is itself a gift (images can be created and inspected on the host with the
same code that serves them on cricker-os). It is the flagship form of the userspace-reuse
thesis: the kernel confining a serious component we did not write.

**The port plan, fixed by the audit** (notes/redoxfs-audit.md; done against 0.9.1, by
building, so the implementer starts here rather than rediscovering):

1. **Pin 0.9.1**, vendor or patch-dep with the audit's patch: two added `use alloc::vec::Vec`
   lines (one each in `filesystem.rs` and `record.rs`, fixing three E0425 sites; with std on,
   the prelude supplied `Vec`, so the untested no_std path bit-rotted). Offer it upstream,
   ideally with a `--no-default-features` CI check, so the pin can eventually drop it. Build with `--no-default-features` on the workspace nightly; both
   bare-metal targets are proven to compile.
2. **The allocator comes first** and is shared work with 27's PAL: an untyped-backed
   `GlobalAlloc` in `user_rt`. The core is alloc-heavy; nothing else runs without this.
3. **The `Disk` impl is a blk-IPC client**: `read_at(block, &mut [u8])`, `write_at(block,
   &[u8])`, `size()`, all synchronous, returning `syscall::error::Result`; map that error type
   to ABI errors once, at the server boundary, and nowhere else.
4. **Only operate on-device; never create.** The std-gated core APIs are exactly creation
   (`FileSystem::create`, uuid v4, getrandom): `mkfs` and inspection stay host-side via FUSE.
   The server opens an existing image, full stop; entropy never becomes a userspace dependency.
5. Known-and-accepted: the unconditional `libc` dep is a manifest wart (host-binaries-only,
   proven harmless on `none` targets), and aes/xts/argon2/lz4 ride along as binary size with
   encryption unused in phase one.

**Risks, priced.** (1) ~~The core's std/alloc footprint needs auditing~~ **Audited, retired**
(notes/redoxfs-audit.md): `std` is a feature, not an assumption; the ~5,400-line core compiles
for BOTH of our bare-metal targets today, three bit-rotted imports away from clean, and the
`Disk` trait is three synchronous methods shaped exactly like a blk-IPC client. The one real
cost is a `GlobalAlloc` for the FS-server process, which milestone 27's PAL needs anyway;
creation paths (`mkfs`, uuid, entropy) are the only std-gated core APIs and stay on the host. (2) The write path is new on our
side, driver through validator through tests, and errors there eat filesystems; the CoW design
is forgiving, but the tests must include kill-mid-write. (3) Upstream coupling: pin a version,
carry patches, and record divergence, the same discipline as any vendored engine.

**Prior art and reuse.** RedoxFS is the reuse. Alternatives on the record: FAT (host interop
and simplicity, no integrity story), littlefs (proven, C, wrong-language FFI for less gain
than ghostty-vt would buy). Feeds 31 (per-file grants), 23 (a component with real state to
hand off across a live swap, the hardest handoff case yet named), 27 (`std::fs`).
**Effort: 3 lanes** (measured: the write-capable block path, the FS server, then integration).
