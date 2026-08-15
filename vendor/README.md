# Vendored upstream code

Engines we pin by carrying the source in-tree, per milestone 32's vendored-engine discipline:
pin a version, carry patches, record divergence. Vendoring (rather than a registry or git
dependency) is what lets the pin carry a patch and keeps the build hermetic.

## redoxfs 0.9.1

The on-disk engine for milestone 32's FS server (design/roadmap/32-redoxfs-fs-server.md; the audit that chose
and priced it is notes/redoxfs-audit.md).

- **Source:** the published crates.io package `redoxfs-0.9.1.crate`,
  sha256 `a66d0c043a5768739851a7e5775192a70fdffdbf3418c22fd7927415d41a87c3`,
  upstream git sha `473b4baeb041ebe14504f30693393b1cae52558c` (see `.cargo_vcs_info.json`).
- **Divergence from the published package, exhaustively:**
  1. `src/filesystem.rs`, `src/record.rs`: the two `use alloc::vec::Vec` imports that fix the
     bit-rotted no_std build (three E0425 sites). The same fix as
     `patches/redoxfs-no-std-vec-import.patch` (written against upstream master); each site is
     marked with a `cricker-os pin divergence` comment. Drops when upstream ships it.
  2. `Cargo.toml`: an empty `[workspace]` table at the top, marking this crate as its OWN workspace
     root (also commented as a pin divergence in the file). This is what keeps it out of the
     cricker-os workspace, so our `-D warnings` clippy gate and `cargo fmt` never touch upstream
     code we do not own, and its default features (which pull `fuser`, a macFUSE build on macOS)
     never ride into our builds. `tools/redoxfs_host` is a separate own-workspace crate for the
     same reason; a cricker-os *member* cannot depend on an in-tree crate that another workspace
     owns without a "multiple workspace roots" error, so both live outside.
  3. `src/header.rs`: `Header::update_hash` made `pub`. **This is the first divergence that changes
     what upstream OFFERS rather than fixing a build, and the distinction is worth keeping**, because
     the two age differently: 1 and 2 drop when upstream fixes them, while this one must be
     re-applied forever and can conflict if upstream changes the method.

     Why it was taken: `Header::new` is `#[cfg(feature = "std")]` purely because it calls
     `uuid::Uuid::new_v4()`, so a `no_std` caller cannot use it. Every `Header` field is already
     `pub`, so we can BUILD one and source the uuid from our own entropy service; what we could not
     do is finish it, because an unhashed header is an invalid filesystem. So this exposes an
     existing method rather than adding an API.

     Chosen over adding a `new_with_uuid` constructor deliberately (calef, 2026-08-03: the minimum
     viable divergence). A visibility change on an existing method is the smallest thing that
     unblocks `mkfs` on the target, and far less likely to conflict on a pin bump than a new
     constructor whose name and signature upstream might choose differently.

     **It is not sufficient, and nothing outside this directory calls it today** (milestone 57's
     write half, 2026-08-03). The premise above is that a `no_std` caller can build a header and
     therefore make a filesystem, and the second half does not follow. Making a filesystem is
     `FileSystem::create_reserved`, which lays down the tree list, the allocation list and the root
     node through `Transaction::write_block` and `FileSystem::reset_allocator`; **both are private,
     `Transaction::new` is `pub(crate)`, `sync_block`'s `AllocCtx` parameter names a trait the crate
     does not export, and three of `FileSystem`'s fields are `pub(crate)` so the struct cannot even
     be built from outside.** A caller holding a finished `Header` has nowhere to put it. That is
     what divergence 4 is for. This one is kept rather than reverted because it was a deliberate
     call and reverting one is calef's to make; it is a one-line drop whenever he wants it.
  4. `src/header.rs`, `src/filesystem.rs`: **the uuid becomes an argument, and the creation path
     builds for `no_std`.** `Header::new_with_uuid(size, uuid)` and
     `FileSystem::create_reserved_with_uuid(.., uuid)` hold what used to be `Header::new`'s and
     `create_reserved`'s bodies; the `std` entry points keep their signatures and pass
     `Uuid::new_v4()`, so no upstream caller changes. The encryption branch stays behind `std`
     (`Salt::new` and `Key::new` are `getrandom` too) and a `no_std` create with a password returns
     `ENOSYS` rather than quietly making an unencrypted filesystem.

     This is the same shape upstream already uses one line away: `create` takes `ctime` as a
     parameter because a `no_std` engine has no clock, and now takes the disk id as one because it
     has no randomness. The caller that supplies it is `mkfs`, which holds an entropy endpoint;
     **no randomness enters vendored code.**

     Ages like divergence 3 (re-applied forever, can conflict on a pin bump), and is the one most
     worth upstreaming: `patches/redoxfs-no-std-create-uuid.patch` is the submission, and it applies
     to the published 0.9.1 with zero fuzz. Approved by calef 2026-08-03.
- Everything else is byte-identical to the published package, including files we do not use
  (`Makefile`, `test.sh`, upstream CI configs) and `Cargo.lock`.
- **Proved rather than asserted, since 2026-07-30:** `script/vendor-verify` fetches the published
  tarball, checks its sha256 against `redoxfs.pin`, applies `redoxfs.divergence.patch`, and requires
  the result to be byte-for-byte the tracked contents of this directory. The two items above **are**
  that patch. After a deliberate change, regenerate with `script/vendor-verify --write-patch` and
  extend the list here; anything else is drift.
- **A correction, because the first run of that check found one.** This file used to carry a third
  divergence: "`Cargo.lock` present and committed. The published library package ships without one."
  That was wrong twice over. The published package *does* ship a lockfile, and ours was not
  upstream's: deleting it and letting cargo regenerate re-resolved 25 dependencies to whatever was
  current that day (`syn` split across 2.x and 3.x, `jiff` 0.2.31 to 0.2.35, `proc-macro-error2`
  gone). Nobody had touched the filesystem code, but nobody could have proved that either, which is
  the whole problem. Upstream's lockfile is restored, `--no-default-features --locked` builds green
  on both bare targets against it, and the claim is now checkable.
- **License:** upstream's own `LICENSE` (MIT), unchanged. The cricker-os dual-license terms do
  not apply inside this directory.
- **Feature use here:** the kernel-facing consumer (phase 2's FS server) builds it
  `--no-default-features` (pure no_std core); `tools/redoxfs_host` builds it with `std` only,
  deliberately not `fuse`, so host mkfs/inspection needs no macFUSE. The FS server still only ever
  opens an existing image (roadmap §32, port plan item 4); since divergence 4 the *creation* path
  builds for `no_std` as well, and `mkfs` is the one program that uses it. What stays std-gated
  is the randomness: `FileSystem::create`, `create_reserved`, `Header::new` and the encryption
  branch, all because they invent a value rather than take one.
- **Kept honest by:** `cargo xtask test` runs the host round-trip test (`cargo test --manifest-path
  tools/redoxfs_host/Cargo.toml`) and builds the no_std core for both bare-metal targets
  (`cargo build --manifest-path vendor/redoxfs/Cargo.toml --no-default-features --target ...`), so
  the pin cannot bit-rot silently. `script/lint` and `script/fmt` gate the host tool by the same
  `--manifest-path`, since it is outside the main workspace their `--workspace`/`--all` sweeps see.
  `script/vendor-verify` asks the different question those cannot: not "does it still build" but
  "is this tree what we say it is".
