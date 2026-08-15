# Reading the backup from a MacBook or a Linux host (milestone 57)

The question that makes a backup credible rather than merely functional: **the board is dead, can I
get my data?** calef asked it about the RedoxFS volume the backup server writes, and the honest
first answer was "we would have to write something". This note is what got written, what upstream
already had, and the operational rule that has to hold for any of it to matter in a year.

## Upstream did not already solve this, and it is worth being precise about why

`vendor/redoxfs/src/bin/` has five binaries and it is easy to assume one of them extracts. None
does:

| Upstream binary | Direction | Use to recover? |
|---|---|---|
| `redoxfs-ar` | host directory **into** a new image | No. It is an *archiver*: it creates the filesystem as it writes. The name suggests `tar`, which reads too; this one only writes |
| `redoxfs-clone` | image **to** image | No. It copies a filesystem to another filesystem, not to host files |
| `redoxfs-mkfs` | makes an empty one | No |
| `redoxfs-resize` | grows one | No |
| `redoxfs` (`mount.rs`) | serves an image over FUSE | Yes, and that is the path we deliberately do not take. See below |

So the read side upstream ships is FUSE, and `fuse` is a feature `tools/redoxfs_host` excludes
(`default-features = false, features = ["std"]`). On Linux enabling it is nearly free. On macOS it
means macFUSE, a third-party system extension, and reduced security mode on Apple Silicon. **A
recovery story that begins "first, reboot into recovery mode and lower your Mac's security
settings" is not a recovery story.** The library, though, links here already with `std`, so the
read paths cost a tool instead: `ls`, `cat` and `extract` on top of the same engine, no FUSE, no
kernel extension, no root, identical on macOS and Linux.

## What the tool does

```
redoxfs_host partitions DEVICE            # the GUID partition table: number, size, type, label
redoxfs_host ls         VOLUME [PATH]     # a directory listing: kind, attributes, size, name
redoxfs_host cat        VOLUME PATH       # one file's bytes to stdout
redoxfs_host xattr      VOLUME PATH       # what extended attributes are on it
redoxfs_host xattr      VOLUME PATH NAME  # one attribute's bytes to stdout
redoxfs_host extract    VOLUME PATH DEST  # a whole subtree onto the host filesystem, attributes and all
```

Plus the pre-existing write side (`mkfs`, `put`) and one addition, `import IMAGE HOST_DIR`, which
copies a host directory into an image using upstream's own `redoxfs::archive`.

A **`VOLUME`** is an image file, or a device plus a partition selector (milestone 110, below).

`PATH` is always relative to the filesystem root, and `..` is refused, which is the same rule the FS
server enforces on the wire (notes/fs-server.md). `DEST` **becomes** the thing extracted: a
directory in the image lands as a directory at `DEST`, a file lands as the file `DEST`. That is
`cp -R SRC DEST` with a `DEST` that does not exist yet, and it avoids the "into or as?" ambiguity
that makes people run a recovery twice.

Files, directories and symlinks are extracted, with permission bits masked to `0o777`. Setuid,
setgid and sticky are dropped on purpose: an image is data being recovered, not something that gets
to hand out authority on the machine doing the recovery. Entry names are checked before use, so a
damaged or hostile image cannot walk out of the destination directory.

### The read paths do not write to the image

This is not a detail. The image you are extracting from may be the last copy of the data, and may
be on a failing disk or read-only media. So `ls`, `cat` and `extract` open the file without write
permission and pass `cleanup: false`, and there are two traps in doing that with this engine:

- **`FileSystem::open(.., cleanup: true)`**, which is what the mount path uses, tidies allocations
  and therefore writes. It is not needed to read correctly: `open` picks the newest *valid* header
  out of the ring either way, which is the crash-consistency property itself (notes/fs-server.md).
  Upstream's own `redoxfs-clone` reads its source disk exactly this way.
- **`Transaction::read_node` updates atime**, but only when the last read was more than an hour
  ago. That is the worst possible shape for a bug: every test on a freshly made image passes, and
  the first read of a real backup dirties a node and the header ring. `read_node_inner` is the same
  read without the timestamp, so the recovery paths use it. The round-trip test hashes the whole
  image file across a read and compares, which is what pins this down.

## The operational rule: keep the reader, or its exact pin, with the backup

**We are pinned at RedoxFS 0.9.1, on-disk format version 8, and a reader must match the format
version it reads.** `Header::valid` checks the version before it checks anything else, so an image
written by a different RedoxFS presents as *no valid header anywhere in the ring*, and the engine
reports that as ENOENT. Being told "no such file or directory" about a disk you are holding is the
wrong thing to be told while recovering a backup, so the tool reads the signature and version
straight off the disk when an open fails and says which version it found and which one it reads.
There is a test that forges the mismatch and asserts the message.

The rule that follows, and it is the whole point of this note:

> **A backup readable only by software you no longer have is not a backup.** Store the recovery
> tool, or the exact source it is built from, *with* the backup.

Concretely, that means the backup's off-site copy carries one of:

1. a built `redoxfs_host` binary for the host you would recover on (fine, but binaries rot across
   OS versions and architectures), or
2. the nife commit hash plus `vendor/redoxfs` at 0.9.1 and `patches/`, which is enough to
   rebuild the tool with nothing but a Rust toolchain, or
3. both, which costs a few megabytes.

Option 2 is the one that survives. It is also why `vendor/` is checked in rather than fetched: the
recovery path must not depend on a registry, a git host, or a company still existing.

The same discipline applies to any future pin bump. Bumping `vendor/redoxfs` to a version with a
different on-disk format silently strands every existing backup, so a bump is a migration, not an
upgrade, and it belongs in `DECISIONS.md` with a plan for the images already written.

## No filesystem-level encryption, so no key handling

Decided by calef on 2026-07-30 (roadmap, milestone 57): "If I'm struggling to get the data off, I'm
not all that worried about somebody else getting it." RedoxFS does support encryption
(`vendor/redoxfs/src/key.rs`), and this volume does not use it. Encryption belongs at the Time
Machine layer instead, where the Mac encrypts before anything is sent, so the server never holds
plaintext and recovery uses the client's key rather than the server's.

For this tool the consequence is a real simplification: every `FileSystem::open` passes `None` for
the password, and **there is no key handling anywhere in the recovery path**. Nothing to lose,
nothing to store, nothing to get wrong at 2am.

The caveat belongs here too. If Time Machine encryption *is* switched on, recovery then depends on
that password, which relocates the "can I get my data" risk rather than removing it. That password
belongs wherever the family's other credentials live, not only in one Keychain.

## The same-engine objection, answered once so nobody relitigates it

The reader shares any bug the writer has, because it is the same code. True, and true of every
filesystem: `e2fsprogs` shares lineage with the Linux ext4 driver. The risk that actually strands
data is an *undocumented* format, and RedoxFS is open source with upstream tooling. What the shared
engine does not give you is an independent check of the format, which is why the round-trip test
below writes with upstream's archiver rather than with our own `put`.

## How it is proven

`tools/redoxfs_host/tests/recovery.rs`, and the shape of the test is the argument:

- **Every step is a separate invocation of the built binary.** Nothing is shared between the write
  and the read but the bytes in the image file: no cached `FileSystem`, no warm allocator, no
  in-process state that could make a reader agree with a writer for the wrong reason. A test that
  writes and reads in one process proves the two halves of one program agree; this proves the
  format is on the disk.
- **The write side is upstream's archiver**, through `import`. If our reader only ever read images
  our writer made, a shared misunderstanding of the format would pass.
- **The tree is not flat and not small**: a 300 KiB file spanning several 128 KiB records, a
  three-deep directory chain, an empty file, an empty directory, a symlink, and an executable whose
  mode has to survive. Single-block flat cases hide exactly the bugs worth catching.
- The extracted tree is compared against the tree that went in, entry by entry, including file
  length, a content fingerprint, permission bits, and whether a symlink came back as a symlink or
  was flattened into a copy.
- Refusals are asserted, not assumed: `..`, `cat` of a directory, `ls` of a file, a missing name.
- The image is hashed before and after a read to prove the read wrote nothing.

## Extended attributes come back on the files (milestone 57)

An image written by nife carries a directory in its root called **`.nife-attrs`**, holding
one small file per node that has extended attributes, named for that node's `TreePtr` id in hex.
`redoxfs_host ls` shows it, `extract` copies it out, and upstream's FUSE mount would too.

That the store is *visible* here is deliberate rather than a leak, and both halves are worth saying:

- **It is unreachable through the contract.** No client of the FS server can open, create, list, or
  descend into it, in any directory. The confinement is the *contract's*, and a recovery host is not
  a client of the contract; it holds the image file.
- **And a backup that carries the store carries the metadata.** The format is written down in
  `fs_proto::xattr::store` precisely so a person holding a damaged image can read it: a record is a
  name length, a `u32` type code, a `u16` value length, then the name and the value, little-endian.

But a blob called `0000002a` is not a recovery. **The tool now puts the attributes back on the
extracted files**, which is the half that decides whether the backup did its job: Time Machine's
sparsebundle carries Apple's metadata in exactly these attributes, so the part of the backup a Mac
needs in order to make sense of the rest was the part that used to come out unreadable.

`setxattr` on macOS, `lsetxattr` on Linux, neither following a symlink. Directories get theirs as
well as files. `ls` marks an entry that carries attributes with `@`, which is the same marker macOS
`ls -l` uses, and `xattr` renders or dumps them without extracting anything.

### Three things it is honest about, because §42 is the rule here too

**The type code cannot come along.** No host filesystem has a per-attribute type word, so the `u32`
kind is dropped, each non-`RAW` one is named on stderr, and the count is in the summary. It is not
lost: the raw store is still extracted beside the tree, and that is where the codes live. That is
what makes "dropped" honest rather than lossy.

**Nothing about attributes can fail an extraction.** A damaged blob, a name Linux refuses for want
of a `user.` prefix, an attribute on a symlink (which Linux refuses outright), a destination
filesystem that holds none at all: each is reported, counted, and walked past. A recovery that
abandoned a hundred thousand files over one bad blob would be worse than the gap it fixes.

**The counts are printed even when they are zero**, and that is the important one. "0 attributes
reattached" on a backup you know carried some is the line that tells you the destination filesystem
cannot hold them. A summary that mentioned attributes only when the number was non-zero would read
identically to a backup that never had any, which is the failure this whole feature exists to
prevent: a recovery that looks complete and is not.

### EXAMPLES

A whole recovery, on a Mac, with the attributes on the other end. This is a real transcript.

```console
$ redoxfs_host ls backup.img /
dir         4096  .nife-attrs
dir         4096  nested
file@         13  photo.jpg

$ redoxfs_host xattr backup.img photo.jpg
         6  kind 0x43535452 'CSTR'  user.com.apple.metadata:_kMDItemUserTags
        32  kind 0x00000000  user.com.apple.FinderInfo

$ redoxfs_host xattr backup.img photo.jpg user.com.apple.metadata:_kMDItemUserTags
Family

$ redoxfs_host extract backup.img / recovered
redoxfs_host: recovered/photo.jpg: attribute user.com.apple.metadata:_kMDItemUserTags kept its
  6 bytes but not its type code 0x43535452; host filesystems have no field for it (see
  .nife-attrs in the extracted tree)
extracted / to recovered: 4 files, 3 directories, 0 symlinks, 165 bytes,
  3 attributes reattached, 1 type codes dropped

$ xattr -l recovered/photo.jpg
com.apple.provenance:
user.com.apple.FinderInfo:
user.com.apple.metadata:_kMDItemUserTags: Family
```

The last command is macOS's own `xattr(1)`, not ours, which is the point of running it: the
attribute is on the file according to a program this project did not write. (`com.apple.provenance`
is the Mac's own addition to a freshly written file, not something out of the image.)

### How the reattachment is proven

`tools/redoxfs_host/tests/attributes.rs`, and the shape of it is again the argument.

- **The fixture is written by `fs_server::Server`**, the sans-IO core that runs on the board, driven
  over a `DiskFile`. Not by a second writer in this crate: a reader and a writer in one crate can
  share a misunderstanding of the format and agree perfectly. This is the same reason `recovery.rs`
  fills its image through upstream's archiver rather than through our own `put`.
- The tree is not flat: a 1 KiB attribute, a typed one, one on a **directory**, and one on a file a
  level down.
- The round trip is closed on the host file, and then confirmed by `/usr/bin/xattr` where it exists.
- **A host that cannot hold attributes is a case, not a skip.** If the destination refuses them the
  test asserts that the tool *said so*, with a count. That path is real: `/tmp` is `tmpfs` on many
  Linux CI runners, and `user.*` attributes on `tmpfs` need kernel 6.6.

### BUGS

- **A Linux host refuses a name with no `user.` prefix.** The store holds bytes and requires no
  namespace (`fs_proto::xattr::valid_name` refuses only NUL and over-length), because there is no
  privilege here for a namespace to mean. Linux does have one, and `lsetxattr` answers `EPERM` for a
  name outside it. The tool reports the errno rather than rewriting the name: silently turning `foo`
  into `user.foo` would hand back a file whose metadata does not say what the backup said. In
  practice Samba writes `user.`-prefixed names, so this bites a name nife invented, not a name
  that came from a client.
- **Linux refuses attributes on a symlink at all**, for any `user.*` name. macOS takes them
  (`XATTR_NOFOLLOW`). So the same image extracted on the two hosts can differ in exactly that one
  place, and only the Linux run says so.
- **A value larger than the destination filesystem's ceiling is refused by the host**, counted, and
  named. `MAX_VALUE` here is 3 KiB, well under any host's limit, so this is a guard rather than a
  case anyone has hit.
- **The tool has no attribute call for a platform that is neither macOS nor Linux.** It refuses with
  a message saying the attributes are still in the extracted `.nife-attrs`, rather than compiling
  to a silent success. FreeBSD spells this `extattr_set_link` and is not wired up.
- **`extract` still copies `.nife-attrs` out**, even now that the attributes are also on the
  files. That is deliberate (it is the only home the type codes have, and the last-resort record if
  a host refused everything) and it does mean a recovered tree carries one directory a user did not
  put there. An image with no attributes left on it no longer has the directory at all, since
  milestone 57 made the last attribute take the store with it.

## A device and a partition, not only an image file (milestone 110)

A real disk has a partition table where an image has a filesystem, so the tool now takes a device
plus a **partition selector** and does the offset itself. `crates/gpt` reads the table, the
partition's first LBA becomes block zero of a `PartitionDisk`, and nothing above the disk layer
knows a partition was involved. That is the same shape the board's own `mkfs` uses
(`fs_server/src/bin/mkfs.rs`), which is why the host reads the partition by the same rules the board
wrote it by rather than by a second implementation that could disagree.

**The gap had a witness, and closing it was the acceptance test.** Milestone 57's post-run check
(`blank_check_after_run` in xtask) needed to read a filesystem the guest created inside a partition,
so it parsed the table and **sliced the partition out into its own file** before handing that file
to the tool. Twenty lines, on the host, in a build script: the join, written in the wrong place.
Those lines are gone and the check passes unchanged, which is a stronger claim than a new test would
have been: it shows the capability *moved* rather than being written twice.

### Two selectors, and why not a third

| Selector | Spelling | For |
|---|---|---|
| Partition number | `--partition 3` | A person with a listing in front of them. 1-based, so it is the number in `/dev/sda3`, in macOS's `disk4s3`, and in `sgdisk -i 3` |
| Type GUID | `--partition-type EC5CC08B-...` | A program. It names what the partition **is**, so it survives a disk being re-partitioned, and it is how the guest finds its own data partition (DECISIONS §45) |

**Not the partition name.** GPT labels are cosmetic and frequently absent: macOS writes none at all
(notes/gpt.md), so a selector keyed on one would fail on exactly the disk the recovery story is
about. Both flag names are **provisional** (CLAUDE.md: names are calef's call).

The selector is **refused on `mkfs`, `put` and `import`**. Those verbs open read-write, recovery
never does, and opening a *device* read-write by accident is a considerably worse mistake than
opening an image read-write.

`partitions DEVICE` prints the table, because `--partition 3` is only obvious once something has
printed the three.

### The correction: the old tool did not fail on a partitioned disk, which is worse

The milestone's premise was that the tool "reads an image file, not a raw device", and the machine
overruled it during the work. **`FileSystem::open` scans blocks 0..65536 for a valid header**
(`vendor/redoxfs/src/filesystem.rs`), so handing it a partitioned device does not fail. It finds
whichever filesystem lies in the first 256 MiB and opens that, and the test written to assert a
clean refusal failed by *succeeding*.

That is not the join, and it is worse than not working, for three reasons:

- **It reads a partition nobody named.** On a disk with two RedoxFS volumes it takes the lower one,
  and a recovery that read the wrong partition looks exactly like a recovery that worked.
- **It stops at 256 MiB.** A data partition further out is invisible, and a boot partition and a
  root partition ahead of the data is the ordinary layout rather than a contrived one.
- **It sizes the engine from the whole device**, so the allocator believes it owns bytes that belong
  to other partitions. Harmless while every path is read-only, and a corruption waiting for the day
  one is not.

So reading a device whole is now a **warning, not a refusal**: the bytes do come back, and a tool
that refused to read a disk it can read would be the wrong answer at 2am. The line says what
actually happened and names the flag that removes the guess.

### EXAMPLES

A real transcript, against `target/nifefs-blank.img` after a test run. Nothing on that disk was
written by the host: **the guest partitioned it and made the filesystem**, and this is a Mac reading
it back with no `dd` in front of it.

```console
$ redoxfs_host partitions target/nifefs-blank.img
512-byte logical blocks, disk GUID 8784494D-E9F9-48FB-AFC9-F57D43A563B8
  #   first LBA    last LBA          size  type
  1        2048       10239       4.0 MiB  EFI system  nife boot
  2       10240       30719      10.0 MiB  Linux filesystem  nife root
  3       30720      129023      48.0 MiB  nife data  nife data

$ redoxfs_host ls target/nifefs-blank.img / --partition 3
file          63  made-on-target

$ redoxfs_host extract target/nifefs-blank.img / /tmp/recovered --partition 3
extracted / to /tmp/recovered: 1 files, 1 directories, 0 symlinks, 63 bytes, 0 attributes reattached
```

And what it says if you forget the selector, which is the correction above made audible:

```console
$ redoxfs_host cat target/nifefs-blank.img made-on-target
redoxfs_host: target/nifefs-blank.img has a GUID partition table (512-byte blocks, 3
partitions), so it is a device rather than an image. Read whole, the engine takes whichever
filesystem its scan meets first and sizes itself from the whole device. Name the partition with
--partition N (`redoxfs_host partitions` prints them).
CRK57: this filesystem was created by nife on the target
```

### How it is proven

`tools/redoxfs_host/tests/partition.rs`, and the fixture is the argument again. The device is a file
with a real table at the front and a **filesystem built as an ordinary image and then placed** in
the third partition, so the bytes in the partition were produced by a writer that knew nothing about
partitions: an offset wrong by one block in either direction opens nothing. Three partitions, so a
selector has to *pick* rather than find the only filesystem there is, and one of them starts on an
odd LBA so the alignment refusal is exercised rather than asserted.

The test that matters most is `a_partition_past_the_engines_scan_is_reachable_only_by_name`: a
320 MiB device with its data partition at 272 MiB, which the engine's own header scan cannot reach.
Read whole it fails; named, it reads. That is the line between the capability being new and being
cosmetic.

What none of this can catch is a table this project's parser and this project's writer agree is
wrong. `blank_check_after_run` closes that, because the table it reads was written by the *guest*.

### BUGS

- **It has never been run against a real raw device.** Everything here is proven against files, and
  a device brings its own rules: macOS wants `/dev/rdiskN` for unbuffered access and may want root,
  and a raw device reports a **length of zero** through `metadata`, which is why the
  "partition runs past the end of the file" check applies only to regular files. The arithmetic is
  the same either way; the plumbing around it is not proven.
- **A partition whose first byte is not on a 4096-byte boundary is refused**, not rounded in, which
  is the same rule the board's `mkfs` applies when creating one. A filesystem placed there reads
  back correctly only through a disk offset by the same fraction.
- **The logical block size is guessed**, 512 first and then 4096, because nothing in a GPT records
  it and nothing on the blk wire reports it (`disk_surveyor`'s BUGS). A wrong guess fails on the
  signature rather than reading a plausible wrong table, but a disk with some other block size is
  not read at all.
- **A hybrid MBR is refused by `crates/gpt`**, so a disk that Boot Camp or an old macOS installer
  touched will not parse here even though its GPT is fine. That refusal is deliberate (the same disk
  described twice, by two tables that can disagree) and it is inherited, not decided here.
- **`--partition` on the write verbs is refused rather than implemented.** Formatting one partition
  of a device from the host is a real thing to want and is not this: it would be the first path in
  this tool that can destroy a partition it was not aimed at.
- **The backup table is not consulted.** If the primary header is damaged the tool says the disk has
  no table, while `Gpt::check_backup` exists and the backup copy is probably intact. Reading the
  backup when the primary fails is the obvious next thing and is not built.

## What this does not do, and where it goes next

- **It does not write to an image it is recovering**, by design. `put` and `import` exist for
  building fixtures and open read-write; the recovery verbs never do.
- **No repair.** If no header in the ring is valid, the tool says so and stops. A format-aware
  salvage tool (walk the tree from an older generation, recover what parses) is a real thing to
  want and is not this.
- **The Linux FUSE mount stays available** as a feature flag if it is ever wanted for convenience.
  It is not the recovery story, and turning it on would put `fuser` into the one tool that
  currently has no platform dependency at all.
