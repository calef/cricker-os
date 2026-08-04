# 110. The recovery tool takes a device and a partition

**Status: NOT-STARTED.** Raised 2026-08-04 from `notes/host-recovery.md:263`. Milestone 57
(partitioning and formatting a real drive) is BUILT, so this is its residual, and it is small.

**The finding.** `tools/redoxfs_host` reads a filesystem out of an **image file**, not off a device.
`open_ro` hands a `DiskFile` straight to `FileSystem::open` with no offset, so the bytes at offset
zero have to be the filesystem. A real drive has a partition table there.

Everything the join needs already exists. `crates/gpt` parses and validates a GUID partition table.
The tool has the recovery verbs. What is missing is the arithmetic between them: open the device,
read the table, and start the engine at the partition's first LBA.

**The gap has a witness, which is the argument for closing it.** Milestone 57's post-run check
(`blank_check_after_run`, `xtask/src/main.rs:2015`) needs to read a filesystem the guest created
*inside a partition*. So it parses the table with `crates/gpt` and **slices the partition out into
its own file** before handing that file to the tool. The note's verdict: "Twenty lines, on the host,
in a build script: that is the join, written in the wrong place."

The version that belongs in the tool takes a device and a partition index and does the offset inside
`DiskFile`. The version that exists is a temp file in the test harness, which is fine for a gate and
useless at a keyboard.

**What it costs, and when.** Nearly nothing in code, and the whole point is *when* it is worth
having. The note puts it plainly: "the day somebody plugs the board's drive into a Mac at 2am is the
day the difference matters." A recovery tool that requires you to first `dd` a whole device into an
image, on a laptop that may not have room for it, is a recovery tool with a step in front of it at
the worst possible moment.

## Scope note

**Read-only, like the rest of the recovery path.** The tool does not write to an image it is
recovering, by design; `put` and `import` exist for building fixtures and open read-write. Taking a
device does not change that, and opening a *device* read-write by accident is a considerably worse
mistake than opening an image read-write.

**Not repair, and not FUSE.** If no header in the ring is valid the tool says so and stops; a
format-aware salvage tool is a real thing to want and is not this. The Linux FUSE mount stays behind
its feature flag, because turning it on would put `fuser` into the one tool in this tree with no
platform dependency at all.

**The harness's twenty lines should go when the tool grows them**, or this milestone has added a
second implementation instead of moving the first. `blank_check_after_run` calling the tool with a
partition index is the acceptance evidence.
