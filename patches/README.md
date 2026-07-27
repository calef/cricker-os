# Patches carried against upstream projects

One file per patch, in `git format-patch` form, applied with `git am`. Each exists to be
upstreamed; an entry leaves this directory when the pin that needed it advances past a release
containing the fix.

- `redoxfs-no-std-vec-import.patch` — fixes redoxfs's no_std build (Vec imports the std prelude
  masked; four E0425 sites across filesystem.rs and record.rs) and adds a
  `--no-default-features` CI job so the configuration cannot bit-rot again. Written against
  master @ 99bc185 (2026-07-27); milestone 32's 0.9.1 pin carries the same fix. Submission:
  fork on gitlab.redox-os.org, `git am` this file on a branch, push, open the MR; see
  notes/redoxfs-audit.md for the audit that produced it.
