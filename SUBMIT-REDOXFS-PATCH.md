# Submitting the redoxfs no_std patch upstream

Deliberately uncommitted (your own instruction, 2026-07-27): this is a personal to-do, set
aside to come back to. Delete this file once the MR is open; the durable artifacts are
`patches/redoxfs-no-std-vec-import.patch` and `patches/README.md`, which are committed.

## One-time setup

1. Sign in at https://gitlab.redox-os.org (it offers GitHub sign-in).
2. Open https://gitlab.redox-os.org/redox-os/redoxfs and click **Fork**.

## Submit

```bash
git clone https://gitlab.redox-os.org/<your-username>/redoxfs.git
cd redoxfs
git checkout -b no-std-vec-import
git am ~/projects/cricker-os/patches/redoxfs-no-std-vec-import.patch
git push -u origin no-std-vec-import
# GitLab prints a "create merge request" URL — open it
```

## MR title

Fix no_std build: import Vec where the std prelude no longer supplies it

## MR description (paste as-is)

Building with `--no-default-features` fails with `E0425: cannot find type Vec` at three sites
in `filesystem.rs` and one in `record.rs`. `Vec` is supplied by the std prelude when the `std`
feature is on, but the no_std path (`no_std` + `extern crate alloc`) must import it from
`alloc`. This adds the two imports, and adds a CI job building `--no-default-features` so the
advertised no_std configuration can't bit-rot unnoticed again.

Verified: `cargo +nightly build --no-default-features` succeeds on the host and cross-compiles
for bare-metal targets (`riscv64imac-unknown-none-elf`, `aarch64-unknown-none-softfloat`).

## Afterward

- Note the MR URL in notes/redoxfs-audit.md (a one-line "submitted upstream: <url>").
- When a release containing the fix ships and milestone 32's pin advances past it, delete
  `patches/redoxfs-no-std-vec-import.patch` and its README entry, per the patches/ discipline.
- Delete this file.
