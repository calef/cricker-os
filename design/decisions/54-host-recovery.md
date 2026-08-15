# 54. Recovering a backup includes its metadata, and formatting a disk needs entropy

**Status: DECIDED.**

Milestone 57. `tools/redoxfs_host`, `fs_proto::xattr::store`. See `notes/host-recovery.md`.

## The rule

**A recovery tool that returns the bytes and drops the metadata has not recovered the backup.**
calef set the standard for this whole area: if he is struggling to get the data off, the backup has
failed at its job. Time Machine's Apple metadata lives in extended attributes, so on this
deliverable the attributes are not decoration, they are part of the file.

`redoxfs_host extract` reattaches them (`setxattr` on macOS, `lsetxattr` on Linux, neither following
a symlink), and the evidence is **macOS's own `/usr/bin/xattr -l` reading the recovered file**,
rather than the tool checking its own work.

## Three behaviours, all §42

- **The type code cannot survive**, because no host filesystem has a field for it. Each non-`RAW`
  one is **named on stderr and counted**, and the raw store is extracted beside the tree so the
  codes remain available. That is what makes "dropped" honest rather than lossy.
- **Nothing about attributes may fail an extraction.** A damaged blob, a Linux kernel refusing a
  name without a `user.` prefix, a destination with no attribute support: reported, counted, walked
  past. The bytes are the thing you came for.
- **Counts print even at zero**, because "0 attributes reattached" is the line that tells a reader
  their destination cannot hold them. A silent success and a silent incapacity look identical.

## Formatting from nife is blocked on entropy, and that is the whole reason

Worth recording because it is not the reason anyone expected. `FileSystem::create` is `std`-gated
and un-gating it is mechanical **for every call but one**: `Header::new` stamps a v4 UUID, which is
`getrandom`, and a `no_std` engine has no randomness.

**The same wall appears twice in this milestone.** `notes/gpt.md` already refuses to invent a
partition GUID for the identical reason. So partitioning *and* formatting from the target are gated
on the entropy service reaching the program that does them, and on nothing else.

The shape that unblocks it is `Header::new_with_uuid(size, uuid: [u8; 16])`, which does for
randomness exactly what `create`'s existing `ctime` parameter does for time, and is upstreamable
rather than a divergence. Weigh it against the fact that `redoxfs_host` on a Mac can partition and
format the drive **today**, which is what actually gets a disk ready for the board.

## BUGS

- **The host tool reads an image file, not a partitioned device.** Both halves exist (`crates/gpt`
  and the engine); the join does not.
- **An unlinked-but-open file loses its attributes immediately**, unlike POSIX, because the purge
  has to be inside the unlink's transaction to be crash-atomic with it.
- **`MAX_VALUE` at 3 KiB is untested against real Time Machine traffic.** Over-long values are
  refused loudly with `E2BIG` rather than truncated, so this will be found out on the board.
