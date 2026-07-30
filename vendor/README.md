# Vendored upstream code

Engines we pin by carrying the source in-tree, per milestone 32's vendored-engine discipline:
pin a version, carry patches, record divergence. Vendoring (rather than a registry or git
dependency) is what lets the pin carry a patch and keeps the build hermetic.

## redoxfs 0.9.1

The on-disk engine for milestone 32's FS server (design/roadmap.md §32; the audit that chose
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
     never ride into our builds. `tools/redoxfs-host` is a separate own-workspace crate for the
     same reason; a cricker-os *member* cannot depend on an in-tree crate that another workspace
     owns without a "multiple workspace roots" error, so both live outside.
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
  `--no-default-features` (pure no_std core); `tools/redoxfs-host` builds it with `std` only,
  deliberately not `fuse`, so host mkfs/inspection needs no macFUSE. Creation APIs
  (`FileSystem::create`, uuid, getrandom) are std-gated and stay host-side; the server only
  ever opens an existing image (roadmap §32, port plan item 4).
- **Kept honest by:** `cargo xtask test` runs the host round-trip test (`cargo test --manifest-path
  tools/redoxfs-host/Cargo.toml`) and builds the no_std core for both bare-metal targets
  (`cargo build --manifest-path vendor/redoxfs/Cargo.toml --no-default-features --target ...`), so
  the pin cannot bit-rot silently. `script/lint` and `script/fmt` gate the host tool by the same
  `--manifest-path`, since it is outside the main workspace their `--workspace`/`--all` sweeps see.
  `script/vendor-verify` asks the different question those cannot: not "does it still build" but
  "is this tree what we say it is".
