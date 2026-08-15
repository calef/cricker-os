# 57. Partitioning and formatting a real drive, and extended attributes

**Status: BUILT.**

**In brief.** calef's router setup is `parted` then `mkfs.ext4` then three mounted partitions. Plus
the xattr gap milestone 55 surfaced. **Nearly all of this is testable in QEMU against virtio-blk with
no board**, so it is schedulable before 2026-08-21 rather than waiting on hardware.

**Status, taken from the tree on 2026-08-03 rather than from this entry.** The reading half is done
end to end: `crates/gpt` parses and writes tables, `disk_surveyor` reads a real one off a virtio-blk
device on both ISAs, `crates/block_roster` answers "what drives are attached" as a read-only mapping,
the extended-attribute layer and its host recovery half are built, and `tools/redoxfs_host` extracts.
**What is left is writing on the target**, and both halves of it are gated on the same thing:
randomness. See the corrected table below, which used to say "the tools, none of which exist".

## Extended attributes: decided in direction, open in mechanism

**calef decided 2026-07-30: extended attributes, not AppleDouble sidecars**, on the grounds that we
will want them anyway. Agreed, and it does not reopen §34: that entry surveyed ext2, FAT32/exFAT,
littlefs, btrfs, ZFS and F2FS before choosing RedoxFS, and **xattrs were never the deciding axis**, so
the requirement adds a gap to fill rather than a comparison to redo. ext4 works on the router but
importing it means importing C, which §34 chose RedoxFS specifically to avoid, and there is no
`no_std` Rust ext4.

Verified: **RedoxFS has no xattr support.** **The fork is closed as of 2026-07-31: the layer**
(§34's amendment). Reversibility decides it: `fs_proto` hides which implementation was chosen, so
the format extension stays available later without any client changing. Attributes key on
`TreePtr<Node>`, so **rename is free and correct**, which sidecars get wrong.
Before designing the attribute layer, read `design/haiku-bfs-and-packages.md`: BFS made attributes
typed and indexed with live queries over them, and the point of knowing that is to avoid designing
something that **forecloses** indexing later, even though SMB only needs opaque blobs now.

**BUILT 2026-07-31: the layer, on both ISAs** (notes/xattr.md). Four verbs in `fs_proto`
(`GETXATTR`, `SETXATTR`, `LISTXATTR`, `REMOVEXATTR`) and a store the FS server keeps in a reserved
directory of the image, one blob per node, keyed on the `TreePtr` id. No new rung on the rights
ladder: reading an attribute takes what reading the file takes, changing one takes what writing
takes. Three limits with a reason each, and the third is load-bearing rather than arbitrary: sixteen
attributes of 255-byte names is **exactly one page**, which is why `LISTXATTR` needs no cursor and
therefore cannot be observed half-changed. Every ceiling refuses with its own errno (§42).

BFS is not foreclosed: every attribute carries a `u32` type code the layer stores, returns, and never
interprets, so an indexed store later is a change of implementation rather than a format migration
plus a wire break.

The three ways to get it wrong, all of which §34's amendment named in advance: the purge rides the
same transaction as the removal and asks the engine (`remove_node` answers `Some(id)` exactly when a
node's last link went), the store's name is unnameable and unlistable in every directory, and a
shrinking blob is truncated to length so the reader never walks records nobody wrote. The
rename-replacement case is the one removal the engine cannot report, and the server notices it.

**BUILT 2026-08-01: the recovery side, and two of the three named gaps closed.** `redoxfs_host
extract` puts the attributes back on the extracted files (`setxattr` on macOS, `lsetxattr` on Linux,
neither following a symlink), `ls` marks an entry that has them with `@`, and `xattr IMAGE PATH
[NAME]` renders or dumps them without extracting. The type code cannot come along, because no host
filesystem has a field for one, so each is named and counted and the raw store still comes out beside
the tree as its only home. Nothing about attributes can fail an extraction: a damaged blob, a name
Linux refuses for want of a `user.` prefix, a destination filesystem that holds none, each is
reported and walked past. The counts print even when zero, because "0 attributes reattached" is what
tells you the destination cannot hold them and a summary that hid the zero would read like a backup
that never had any (§42). The fixture is written by `fs_server::Server` itself, for the same reason
the tree fixture goes in through upstream's archiver.

The store directory now goes with the last attribute on the filesystem, which closes a limitation
recorded for a reason that was wrong: `remove_node` on a directory already refuses with `ENOTEMPTY`,
so the emptiness check is the engine's and costs no walk. It matters because `extract` copies the
store out, and a leftover empty `.cricker-attrs` would land in a recovered Documents folder. And
crash atomicity is measured rather than inherited: milestone 37's sweep now carries each name's
attributes in its state and four attribute operations in its workload, interleaved with a write to
the same file, so "the file and its metadata land together" is decided rather than argued.

**What was still not done here, and is now:** the caretakers (`fs_file_caretaker`,
`fs_subtree_caretaker`, `fs_nameset_caretaker`) answered `EOPNOTSUPP` to all four verbs rather than
forwarding, so a program behind a per-file grant could not reach its file's attributes. **Milestone
61 closed it**, and found the general defect underneath: nothing made a caretaker and the contract
agree, so a whole contract addition reached none of them and nothing failed.


- **Extend the on-disk format.** Correct, and atomic by construction since the metadata rides
  RedoxFS's own copy-on-write transaction. The cost is that §34 chose RedoxFS partly for being
  maintained upstream, pinned at 0.9.1 with a patch discipline that is currently two `Vec` imports; a
  format extension is a materially larger divergence that every future pin bump pays for. Upstreaming
  is the mitigation.
- **Layer xattrs in the FS server.** Normally dismissible, because on Linux anything can open the file
  directly and bypass the layer. **Here nothing can**: all access goes through `fs_proto`, so a layer
  above the filesystem is as authoritative as the filesystem. A genuine capability-system advantage.

**The check that decided it, and it was small: does RedoxFS let us group a file write and a metadata
write into one transaction?** If yes, layering is safe and much cheaper. If no, atomicity between a
file and its metadata cannot hold across a crash (§42's exact territory, and a rename must move both
together), and the format extension is the only correct answer.

**Answered yes, 2026-07-31, before the layer was built** (notes/xattr.md): `fs.tx(|tx| …)` groups
arbitrary mutations into one commit, so the file write and the attribute write land together and a
delete removes both or neither. Milestone 37's crash sweep then measured it rather than inheriting
it. This paragraph read as an open question until 2026-08-03; it is not one.

## The tools

**This table was written 2026-07-30 saying "none of which exist" and was wrong within a day.** It
is corrected here on 2026-08-03 from the tree rather than from the plan, and the correction is
itself the finding: three of its four rows had landed and the entry still read as though nothing
had. Take a status from the merged tree.

| Need | Status | Note |
|---|---|---|
| **GPT parsing** | **Built** 2026-07-30 | `crates/gpt`, proved against tables `sgdisk` and macOS `diskutil` wrote. Mandatory even if we never write one: you cannot find a partition on a real disk without reading the table |
| GPT writing | **Built** 2026-07-30 | `Gpt::create`, `write_primary_header`, `write_backup_header`, `mbr::write`. Re-emitting `sgdisk`'s table reproduces its bytes exactly. What it will not do is invent a unique GUID, which is the row below |
| **Reading a table on the target** | **Built** 2026-08-03 | `disk_surveyor` over the block service, both ISAs, against an image built from the `sgdisk` fixture. notes/block-devices.md |
| Block device enumeration | **Built** 2026-08-03 | `crates/block_roster`: a read-only page the kernel writes, listing what is attached and deliberately **not** how big it is. Listing and holding are different authorities, and the negative control writes to the roster and dies |
| Partitioning **on** the target | Blocked on entropy | `Gpt::create` is proved and needs a unique GUID per partition. A GUID that is not random is not unique; the entropy service is where a caller gets one. **No pin divergence needed**, so this is the cheaper of the two write halves |
| `mkfs` on the target | Blocked on entropy **and a pin divergence** | The finding below. Not blocked on `std`, which is what it looked like |

**What remains is the write half, and both halves of it are the same wall**: an identifier that must
be unique needs randomness, and neither `crates/gpt` nor a `no_std` RedoxFS has any. The difference
between the two is that partitioning needs only plumbing (an entropy endpoint into the program that
does it) while `mkfs` also needs a change inside `vendor/redoxfs`, which is a decision.

## Finding 2026-08-01: `mkfs` on the target is blocked on **entropy**, not on `std`

Investigated and measured, because "the FS server is `no_std` and the creation APIs are std-gated"
reads like a dead end and is not the real constraint.

`FileSystem::create` and `create_reserved` carry `#[cfg(feature = "std")]`, and so do the imports
they need and `Header::new`. Un-gating them is mechanical for all but **one** call:
`Header::new` stamps a fresh v4 UUID into the header with `uuid::Uuid::new_v4()`, which is
`getrandom`, which is the std path. The encryption branch wants randomness too (`Salt::new`,
`Key::new`), and that one does not matter here because this volume is deliberately unencrypted.

So the blocker is that **a filesystem needs a unique identifier and the engine has no source of
randomness in a `no_std` build.** cricker-os does: milestone 55's entropy service. The shape of the
fix is therefore small and upstreamable, and it is the shape upstream already uses one line away:
`create` takes `ctime` and `ctime_nsec` as *parameters* precisely because a `no_std` engine has no
clock. A `Header::new_with_uuid(size, uuid: [u8; 16])` does for randomness exactly what those
parameters do for time, and the caller (which has an entropy capability) supplies it.

**The same problem appears twice in this milestone, and has the same answer both times.**
notes/gpt.md already records that `crates/gpt` will not invent a partition GUID, for the identical
reason: "a GUID that is not random is not unique, this crate has no randomness, and inventing one
from a counter would be worse than refusing." Partitioning and formatting on the target are both
gated on plumbing the entropy service to the program that does them, and neither is gated on `std`.

This is a **decision for calef**, because the fix is a divergence from the pin (`patches/README.md` records the
patch and how to submit it, which is the mitigation), and §46's rule is that taking one is a decision rather than a convenience. It is
also worth weighing against the pragmatic alternative: `redoxfs_host` on a Mac can partition and
format the drive today, which is what actually gets a disk ready for the board on 2026-08-21, and the
target-side version is then a capability demonstration rather than a prerequisite.

**GPT is a good crate to write.** Pure computation, well specified, so it is host-tested with tests in
milliseconds, and it has real Kani targets: CRC round-trip, primary and backup headers agreeing,
entry-array bounds, and refusing a table whose entries overlap.

**Built 2026-07-30: `crates/gpt`**, the parsing and writing halves both. Parse, validate (four CRC-32s,
the geometry, overlapping partitions, the protective MBR, the backup against the primary) and create,
with no I/O at all: the caller supplies blocks and receives blocks, so the whole thing is host-tested.
Seven Kani harnesses in `script/verify`. The claim that makes it credible is that it is tested against
**two real tables this project did not write**, from `sgdisk` and from macOS `diskutil`, committed as
fixtures; re-emitting `sgdisk`'s table reproduces its bytes exactly, and so does rebuilding it from
scratch. Two findings landed in notes/gpt.md: **macOS writes no GPT partition names at all**, so
nothing may identify a partition by its label, and the two tools disagree about the protective MBR's
CHS fields, which is why those are not validated. The cricker-os partition type GUID is DECISIONS §45.
That sentence used to end "what remains on this milestone is unchanged: the transaction check for
xattrs, `mkfs` on the target, block-device enumeration, and the host extraction tool", and by
2026-08-03 three of those four were done and the fourth had turned out to be a decision. The list
now lives in the table above, corrected from the tree.

## The capability shape is the demonstration

Partitioning and `mkfs` are **destructive** and need authority over a *whole block device*. So the
tool holds one device capability and can destroy exactly that device and nothing else. Compare
`parted /dev/sda` as root, where a typo reaches any disk in the machine, and calef's own instructions
carry a "confirm the target device path before proceeding" warning precisely because the tool cannot
enforce it. **Here the warning is structural**: the tool was handed one disk.

That also makes it a natural place for milestone 47's `enumerate` right to earn itself: listing
attached devices and holding one of them are different authorities.

**Built 2026-08-03, and it did not become an `enumerate` *right*** (notes/block-devices.md). The
prediction that it would was reasonable and the tree said otherwise: `dir::ENUMERATE` is a bit in a
capability a server checks, and a device listing has no server to check it. So the listing is a
**read-only mapping** instead, `crates/block_roster`, which is the compositor's window-enumeration
shape (DECISIONS §33) rather than the filesystem's. There is nothing to authorize at read time
because the authorization happened when the mapping was made, and a program that holds no mapping
has nowhere to look rather than a request that gets refused.

Two consequences worth recording. The roster carries **no capacity**, because a size is a fact about
a device you hold and you get it from `blk::SIZE`, which takes the endpoint; answering it in the
listing would quietly make the listing the more powerful of the two authorities, and it would mean
bringing a PCIe function up on behalf of a `ls`. And the negative control is what turns this from a
description into a claim: the same binary, given the roster and no disk, writes to the roster's exact
address and dies.

## Reading the drive from a MacBook or a Linux host: BUILT 2026-07-30

**The question that makes a backup credible rather than merely functional: the board is dead, can I
get my data?** calef asked it, and the answer turns out to be that we disabled the feature.

**Correction to this section's original heading, which said "which upstream already solved".** It
half did. Upstream solved *mounting* (FUSE), and that is the path we deliberately do not take.
Nothing upstream ships extracts: `redoxfs-ar` is an archiver that only writes (and creates the
filesystem as it goes, so it cannot even be pointed at an existing image), `redoxfs-clone` copies an
image to another image, `redoxfs-mkfs` and `redoxfs-resize` are what their names say. The
extraction verbs did not exist and are now ours. See notes/host-recovery.md.

`vendor/redoxfs` already ships `src/mount/fuse.rs`, a `redoxfs` mount binary, and `redoxfs-ar`,
`redoxfs-clone`, `redoxfs-resize`. Upstream's default features are `["std", "log", "fuse"]`. Our host
tool depends on it with `default-features = false, features = ["std"]`, so **`fuse` is excluded by our
own choice** and re-enabling it is a feature flag plus the `fuser` dependency.

**What shipped**: `redoxfs_host ls IMAGE [PATH]`, `cat IMAGE PATH`, `extract IMAGE PATH DEST`, plus
`import IMAGE HOST_DIR` on the write side (upstream's own `redoxfs::archive`, which is what makes
the round-trip test read something our writer did not produce). Paths resolve from the image root and
`..` is refused, the same rule the FS server enforces on the wire. `fuse` is still off and `fuser` is
still not a dependency.

**Two things the build found that the plan did not predict.** First, the recovery reads must not
write to the image, and the engine makes that easy to get wrong twice: `FileSystem::open`'s
`cleanup` pass tidies allocations, and `Transaction::read_node` updates atime **only when the last
read was more than an hour ago**, which passes every test on a freshly made image and then dirties
the first real backup you touch. Read-only opens plus `read_node_inner` fix both, and the test hashes
the whole image across a read. Second, the operational rule can enforce itself: `Header::valid`
checks the format version first, so a mismatched reader sees no valid header anywhere and the engine
says ENOENT, which reads as "no such file or directory" about a disk you are holding. The tool now
reads the signature and version straight off the disk when an open fails and names the mismatch,
with a test that forges it.

Three paths, and they are not equally good:

| Path | Cost | Verdict |
|---|---|---|
| **Extend `tools/redoxfs_host` with `ls` / `cat` / `extract`** | Small; the engine already links there with `std` | **DONE 2026-07-30**, plus `import`, `mkfs`, `put` and (2026-08-01) `xattr`. No FUSE, no kernel extension, no root, identical on macOS and Linux. The thing you want at 2am with a dead board. Upstream's `redoxfs-ar` did not cover it: it only writes |
| **Linux mount via the `fuse` feature** | A feature flag | Nearly free, and upstream maintains it: it is how Redox developers work with images |
| **macOS mount via macFUSE** | A third-party system extension plus reduced security mode on Apple Silicon | Works, genuinely awkward. **Optional convenience, not the recovery story** |

**This removes the strongest argument for switching filesystems.** Interop was the one thing ext4
genuinely bought that RedoxFS appeared not to; it turns out RedoxFS buys it too, with a tool instead
of a kernel driver.

**The operational rule that follows: keep the recovery tool, or its exact source pin, with the
backup.** We are pinned at 0.9.1 (on-disk format version 8) and a reader must match the on-disk
format version. A backup readable only by software you no longer have is not a backup. Written up
with what the off-site copy has to carry in notes/host-recovery.md, which also draws the consequence
for future pin bumps: a bump to a different on-disk format strands every image already written, so it
is a migration, not an upgrade.

The same-engine objection is weaker than it looks and is recorded so nobody relitigates it: yes, the
reader shares any bug the writer has, but that is true of every filesystem (`e2fsprogs` shares lineage
with the kernel driver). The real risk is an *undocumented* format, and RedoxFS is open source with
upstream tooling.

## Decided: no filesystem-level encryption on the backup volume

**calef, 2026-07-30**: "If I'm struggling to get the data off, I'm not all that worried about somebody
else getting it." RedoxFS supports encryption (`src/key.rs`, and the read path calls `decrypt`), and
we are deliberately not using it here.

**It is the right call, and for a stronger reason than the one given.** Encryption belongs at the Time
Machine layer, and calef's own setup instructions already offer it ("Optionally enable Encrypted
Backups"). The Mac encrypts before anything is sent, so **the server never holds plaintext**, recovery
uses the client's key rather than the server's, and filesystem encryption underneath would be
redundant while putting a key on the machine most likely to be compromised. It also strengthens
milestone 55's claim: a compromised SMB adapter leaks ciphertext.

Two consequences. The recovery tool needs **no key handling at all**, which is a real simplification.
And if Time Machine encryption *is* enabled, recovery then depends on that password, which relocates
the "can I get my data" risk rather than removing it, so the password belongs wherever the family's
other credentials live rather than only in one Keychain.

**Sequencing, rewritten 2026-08-03 because everything it sequenced has happened.** The GPT crate
(2026-07-30), the transaction check (2026-07-31, answered yes), the host extraction tool
(2026-07-30, the cheapest credibility win here), the extended-attribute layer and its recovery half
(2026-07-31 and 2026-08-01), and the block-device path with a real table read on the target
(2026-08-03) are all done. Real drives arrive with milestone 53; the board arrives around
2026-08-21.

**What is left is one decision and one small piece of plumbing behind it**, and they are not the
same size:

- **Partitioning on the target** needs an entropy endpoint in the program that does it, and nothing
  else. `Gpt::create` is built and proved. No pin divergence. This is a lane.
- **`mkfs` on the target** needs that plus `Header::new_with_uuid` inside `vendor/redoxfs`, which is
  a new entry in `vendor/redoxfs.divergence.patch` and a new file in `patches/` for upstream
  submission. §46's rule makes that calef's call, and the honest alternative is still on the table:
  `redoxfs_host` on a Mac formats the drive today, which is what actually gets a disk ready for the
  board, and the target-side version is then a capability demonstration rather than a prerequisite.

**Effort: not estimated.** The GPT crate turned out to be about one lane on the history-calibrated
scale, and so did the block-device lane.
