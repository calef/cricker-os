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
- `redoxfs-no-std-create-uuid.patch` — lets a `no_std` caller create a filesystem by supplying the
  disk id, the same way `create` already takes `ctime` because the engine has no clock
  (`Header::new_with_uuid`, `FileSystem::create_reserved_with_uuid`; the `std` entry points keep
  their signatures and no existing caller changes). Written against the published 0.9.1, which is
  what milestone 32 pins, rather than against master: it applies there with zero fuzz, and rebasing
  it onto master is the submitter's first step. Same submission route as above. Milestone 57's write
  half needed it because on a bare-metal target the randomness comes from a service the caller
  holds, never from the filesystem library; see notes/crickerfs.md.
