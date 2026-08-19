# 99. Where Apple's metadata lands: stream or sidecar

**Status: DECIDED.** Raised 2026-08-18 by a milestone 55 lane, which was asked to answer the fork
rather than build it. **Decided the same day by calef: option 1, plus a new milestone for the
file-server workload** (milestone 137).

**Minted 99 by the integrator at merge.** The lane proposed 97, which had been taken in the meantime
by two sections on another branch (97 is advisory CI checks, 98 the `OPENDIR` attenuation fork),
both raised earlier. They are written without the section sigil here on purpose: until that branch
lands they name nothing, and a well-formed citation to an absent section is what `script/decisions`
exists to catch. That is the ordinary collision between lanes that cannot see each other, and it is
why a lane ships a number it expects to lose.

## The decision

**Option 1. This server does not claim `FILE_NAMED_STREAMS`, and Apple metadata lands in the `._`
sidecars the Mac writes for itself.** Zero new code, zero new wire surface, and it is already in
force: it is what the tree does today, and the evidence below is that it works because *the client*
implements the fallback rather than because we implemented anything.

**And the scope split, which is the half that changes what happens next.** The lane's real finding
was not that one option beat the others. It was that **Time Machine does not touch this surface at
all**, so the metadata work was never a backup prerequisite and only looked like one because
milestone 55 carried two workloads under one block. Those workloads are now two milestones: 55 stays
the Time Machine target, and **milestone 137 is the share as a Mac file server**, which is where
options 2, 3 and 4 go and where the question reopens on purpose.

**What this does not decide.** It does not refuse named streams forever. It says the customer path
does not need them and that the milestone which does need them should be the one to choose, with its
own reader in mind. Milestone 137's gate is `DECISION` for exactly that reason.

**The cost of being wrong is bounded**, which is what made a decision cheap here despite the
irreversible category: nothing is written to disk in a format we chose, because the sidecar is the
Mac's format and the Mac both writes and renames it. Reversing this is claiming a bit we currently
leave clear.

## What is being decided

Milestone 55's block names its remaining metadata work as "the stream-versus-sidecar choice, then
the SMB stream surface". Concretely, three questions that have to be answered together, because the
answer to the first determines whether the other two exist:

1. **Does this server tell a Mac that it supports named streams?** One bit,
   `FILE_NAMED_STREAMS` in `FileFsAttributeInformation`, currently clear. It is the switch, and the
   evidence below is that it is the *only* switch: macOS decides everything else from it.
2. **If yes, what does a stream become on the disk?** An extended attribute in milestone 57's store,
   an AppleDouble file the server writes, or a file of its own under a reserved name.
3. **If yes, what is the on-disk name?** A stream called `AFP_Resource` stored as an attribute has
   to be stored under *some* attribute name, and `redoxfs_host` and any future recovery host read
   it. That is a thing two programs agree on in the strict sense.

## Is the premise true? Time Machine does not need any of this

The question AGENTS.md puts fourth is the one that moved the most here, so it goes first.

**A Time Machine backup over SMB writes a sparse bundle, and a sparse bundle is directories and
band files.** The metadata a Mac cares about lives inside the disk image's own filesystem, which
the server never sees. From the most specific write-up found on the point:

> no extended attributes or forked files. The sparse bundle format provides a virtual disk, whose
> content is stored in relatively small (around 1GB) files called bands.

That author's minimum working Samba configuration is `vfs objects = fruit` plus
`fruit:time machine = yes`, and their reason for dropping the rest is the same one: `catia` and
`streams_xattr` serve a Mac using the share as a file server, which a backup is not.
(<https://ddaa.net/2025/time-machine-samba-config.html>, opened 2026-08-18.)

**The second-hand corroboration is Samba's own module.** `vfs_fruit`'s header comment says the
module intercepts `AFP_AfpInfo` and `AFP_Resource` and defers every other named stream to
`vfs_streams_xattr`, and its `fruit:metadata` and `fruit:resource` settings decide where those two
go. None of that machinery is reached by a client writing band files.
(<https://raw.githubusercontent.com/samba-team/samba/master/source3/modules/vfs_fruit.c>, opened
2026-08-18.)

**The third piece of evidence is a whole server.** `ksmbd`, the in-kernel Linux SMB server, is used
as a Time Machine target and its entire handling of Apple's create context is four lines: find the
`AAPL` tag, set `conn->is_aapl`, and then use that flag in exactly two places, both of which zero
`UniqueId` in a directory-information record. It emits **no** answering `AAPL` context and claims no
`FULL_SYNC`. Its `FILE_NAMED_STREAMS` bit is set only when a share carries
`KSMBD_SHARE_FLAG_STREAMS`, which is off by default.
(<https://raw.githubusercontent.com/torvalds/linux/master/fs/smb/server/smb2pdu.c>, lines 3958-3964,
4447, 4467 and 6020, opened 2026-08-18.)

**What this does not dissolve.** Two things survive, and they are why this is still a decision
rather than a deletion:

- **A Mac that uses the share for anything else does write metadata**, and `smb-serve` exists to be
  mounted by hand. The moment a person drags a folder onto the share, the question is live.
- **calef's reference implementation has it turned on**, so "what the working system does" and "what
  the backup needs" are not the same answer, and this proposal should not pretend they are.

**So the honest framing of the fork is not "which one does milestone 55 need".** It is: **this is a
file-server feature that milestone 55 inherited by proximity.** Whether it belongs in milestone 55
at all is part of what calef is being asked.

## What macOS actually does, which is the fact the options hang from

This is the part that was worth reading source for, because it inverts the usual shape of a
compatibility decision. **The sidecar is not something the server implements. It is what the client
does when the server says nothing.**

Three files, read on 2026-08-18, not recalled:

**1. The client's four extended-attribute entry points refuse outright without the bit.** In Apple's
SMB client, `smbfs_vnop_getxattr`, `setxattr`, `listxattr` and `removexattr` each open with the same
guard, under a comment that says in the client's own words that the flag is how it learns the server
supports streams:

```c
	if (!(share->ss_attributes & FILE_NAMED_STREAMS)) {
		error = ENOTSUP;
		goto exit;
	}
```

(<https://raw.githubusercontent.com/apple-oss-distributions/smb/main/kernel/smbfs/smbfs_vnops.c>,
at lines 6724, 6981, 7066 and 7269.)

**2. The mount's advertised capabilities follow the same bit.** `smbfs_vfs_getattr` sets
`VOL_CAP_INT_NAMEDSTREAMS | VOL_CAP_INT_EXTENDED_ATTR` only when `FILE_NAMED_STREAMS` is present,
and `VOL_CAP_INT_READDIRATTR` needs it as well; `smbfs_vnop_getattrlistbulk` refuses with `ENOTSUP`
without it. So the bit also governs the bulk-listing path, which is the client-side half of the
`READ_DIR_ATTR` capability this server already declines.
(<https://raw.githubusercontent.com/apple-oss-distributions/smb/main/kernel/smbfs/smbfs_vfsops.c>,
lines 1537-1563; `smbfs_vnops.c` line 9308.)

**3. `ENOTSUP` from a filesystem is where the kernel's own AppleDouble writer takes over.** This is
the load-bearing one. XNU's VFS layer catches exactly that errno and falls back:

```c
	error = VNOP_GETXATTR(vp, name, uio, size, options, context);
	if (error == ENOTSUP && !(options & XATTR_NODEFAULT)) {
		/*
		 * A filesystem may keep some EAs natively and return ENOTSUP for others.
		 */
		error = default_getxattr(vp, name, uio, size, options, context);
	}
```

The same pattern guards `vn_setxattr`, `vn_removexattr` and `vn_listxattr`. The default
implementation writes an ordinary file in the same directory, named with a prefix the file spells
out as `#define ATTR_FILE_PREFIX "._"`, and for a volume root it uses `._.` instead.
(<https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/vfs/vfs_xattr.c>, lines
170-176, 258-262, 310-314, 374-380, 1383, 2113-2141.)

**Put the three together and the consequence is this: the sidecar option is already implemented, by
Apple, on the other end of the wire, and this tree is already running it.** A Mac writing an xattr
to this share today creates a file called `._name` beside `name`, which the share stores as ordinary
bytes because it is ordinary bytes. Nothing in `smb_proto` needs to know.

**The naming, so the options below are unambiguous.** The client maps
`com.apple.ResourceFork` to the stream `AFP_Resource`, `com.apple.FinderInfo` to the stream
`AFP_AfpInfo`, and every other extended attribute to a stream of its own name; the stream names on
the wire carry the `:$DATA` type suffix. `AFP_AfpInfo` is a fixed 60 bytes, `0x3c`, which is
`AFP_INFO_SIZE` in Samba's `source3/include/MacExtensions.h` and `uint8_t afpinfo[60]` in Apple's
client. `AFP_Resource` is unbounded: Apple's `AD_XATTR_MAXSIZE` resolves to `INT32_MAX`, and Samba's
module says in its own header that the resource stream "may be arbitrarily large, thus it can't be
stored in an xattr on most filesystem".

**A correction this lane owes, and it is in this tree.** `crates/fs_proto/src/lib.rs` says in
`xattr::MAX_VALUE`'s documentation that "Apple's `AFP_AfpInfo` is 402 bytes". It is 60. The 402 is a
different blob: it is the AppleDouble-shaped buffer Samba uses on its *netatalk* metadata path,
`uint8_t ad_data[402]` in `fruit_fstatat_meta`. The two were conflated, the number is quoted in a
sizing argument, and it is fixed in this lane's diff.

## What this tree already does in the analogous case

Six greps, and three of them changed an answer.

**The `nifefs` 32-byte name limit does not bite, at all.** It is the **boot archive**, the flat
read-only initrd format in `crates/nifefs`, and it has nothing to do with what the SMB share stores.
The share is RedoxFS through `fs_proto`, and RedoxFS's limit is `DIR_ENTRY_MAX_LENGTH = 252`. The
brief that produced this proposal listed `NAME_LEN` as in scope; it is not, and saying so is worth
more than a paragraph weighing it.

**The binding name limits are `smb_proto`'s own**, and they are smaller than anything below them:
`path::MAX_COMPONENT` is 64 bytes and `path::MAX_PATH` is 128, both chosen against the sparsebundle
workload and both stated as this share's bound rather than the filesystem's.

**`Path::parse` already accepts a sidecar name and refuses nothing about it.** A component is
refused only when it is empty, exactly `.` or `..`, contains `/`, or is over-long. `._name` and
`._.` both parse. So option 1 below needs no change even to the parser.

**`Path::parse` also accepts a colon**, which is recorded in the module's own BUGS as one of the
reserved characters this server does not check. Today that is a compatibility gap. The day this
server claims `FILE_NAMED_STREAMS` it stops being one: `foo:AFP_Resource:$DATA` would parse as a
single 22-byte component and create a literal file of that name on the image. **Any option other
than 1 has to fix that first**, and the fix belongs in `path.rs` where the parse is.

**`FileStreamInformation` is already answered**, at `(1, 22)` in `smb_proto::server`: a file reports
one unnamed `::$DATA` stream and a directory reports none. It is the right skeleton for enumerating
named streams and it is already exercised.

**The attribute store exists and its ceilings are the ones that decide option 2.** Milestone 57 put
`GETXATTR`, `SETXATTR`, `LISTXATTR` and `REMOVEXATTR` in `fs_proto` as ops 14-17, and
`fs_proto::xattr` sets `MAX_NAME = 255`, `MAX_COUNT = 16` per node, and **`MAX_VALUE = 3072`**. The
last of those already carries a BUGS entry saying a resource fork larger than it is refused with
`E2BIG`, and §57's own BUGS says the 3 KiB ceiling is "untested against real Time Machine traffic".
It is now tested against the specification instead, and the answer is that `AFP_AfpInfo` fits with
two orders of magnitude to spare and `AFP_Resource` has no bound at all.

**The `Share` seam is 16 methods over 3 implementations** (`FIXTURE` and `MemoryShare` in
`smb_proto`, `FsShare` in `user/src/smb_server.rs`), and every widening of it has to land in all
three. That is the unit of cost for options 2 through 4.

**And this tree already argued the sidecar's central defect, in its own voice.** `notes/xattr.md`
records that a rename is free for a node-keyed attribute store and says why a sidecar is not:

> AppleDouble sidecars get exactly this wrong: a `._file` beside `file` has to be moved by hand, and
> every tool that renames without knowing about the convention orphans the metadata.

That argument is correct and it is aimed one layer off from this fork. It refuses a *server-side*
path-keyed store. It does not reach a sidecar the **client** writes and the client renames, because
the party that has to know the convention is the party that invented it. Where it does reach: any
other holder of the directory capability, meaning this tree's own `mv`, `redoxfs_host`, and a
recovery host.

## Prior art outside the tree, and what the reference implementation actually does

**calef's router does not run either pure option.** Milestone 55's block records the working
`[global]` stanza, which sets `fruit:metadata = stream` and does **not** set `fruit:resource`. Per
Samba's manual page, `fruit:resource` defaults to `file`, described there as "use a ._ AppleDouble
file compatible with OS X and Netatalk". So the reference implementation is a **hybrid**: Finder
metadata in an extended attribute, resource forks in `._` sidecars on the disk. That is option 3
below, and it is the only option with a working deployment behind it.
(<https://www.samba.org/samba/docs/current/man-html/vfs_fruit.8.html>, opened 2026-08-18.)

**Samba gates the extended-listing capability on the same bit the client does.** In `vfs_fruit.c`'s
`AAPL` handler, `SMB2_CRTCTX_AAPL_SUPPORTS_READ_DIR_ATTR` is claimed only when the client asked for
it **and** `handle->conn->fs_capabilities & FILE_NAMED_STREAMS`. This tree declines
`READ_DIR_ATTR` for a stated reason already, and the two reasons turn out to be the same reason.

**Samba has a migration for sidecar to stream, and it is on by default.** `fruit:convert_adouble`
defaults to `true` and calls `ad_convert` when a file with a `._` companion is opened. That the
reference implementation shipped a converter is the strongest available evidence about how
reversible this decision is, in both directions: it is reversible, and it costs somebody a
converter.

**`ksmbd` is the small-server data point.** Its stream support is real when enabled, maps a stream to
an extended attribute named with a `DosStream.` prefix matching Samba's `streams_xattr`, and
implements `FILE_STREAM_INFORMATION` by enumerating that prefix. It is roughly 200 lines spread
across create, query-info, set-info and rename. That is the closest available measurement of what
option 2 costs somebody who already has an attribute store, which we do.

## The options

Costs are stated per option in four currencies: lines of new code, methods added to the `Share`
seam, wire surface added, and bytes at rest per file that carries metadata. Where a number is an
estimate rather than a measurement it says so.

### Option 1: say nothing, and let the client write sidecars

Leave `FILE_NAMED_STREAMS` clear. macOS's own VFS writes `._name` files, which this share already
stores, lists, renames and deletes as ordinary files.

| | |
|---|---|
| New code | **Zero.** This is the tree's current behaviour. |
| `Share` methods added | 0 |
| Wire surface added | 0 |
| Bytes at rest | one extra directory entry, one extra node, and at least one 4096-byte RedoxFS block per file that carries metadata. XNU's `ATTR_BUF_SIZE` is 4096, described in its own comment as "default size of the attr file and how much we'll grow by", so the sidecar is unlikely to be smaller than the block it occupies. |
| The 3 KiB attribute ceiling | does not apply. A resource fork of any size is an ordinary file. |
| `MAX_COMPONENT = 64` | **bites at the edge.** A 63-byte name has no expressible sidecar, and the create is refused with `STATUS_OBJECT_NAME_INVALID`. Band files are 1 to 8 hex characters, so the backup path never meets it. |

**What it costs that is not code.** The metadata is opaque to everything on this side of the wire.
`redoxfs_host extract` recovers `._` files as files, which a Mac can merge with `dot_clean(1)` and
nothing else can read. This tree's own `mv` orphans a sidecar. A listing shows twice as many entries
as a user expects, which Samba hides with `fruit:veto_appledouble` and calef's router does **not**
(his stanza sets it to `no`, so the working system already shows them).

**Two things it does not cost, which the tree's existing prose implies it would.** It does not throw
away milestone 57's work, because the attribute store serves every non-SMB holder of a directory
capability and always did. And it does not lose metadata on rename in the case that matters, because
the renaming party is the client that wrote both files.

**Optional additions, each independently cheap and none required**: filter `._*` from
`QUERY_DIRECTORY` (roughly 10 lines in `smb_proto::server`, and it makes the share lie about its own
contents, which is a §42 question); or refuse to create `._*` (which would break the client rather
than tidy it).

### Option 2: named streams, stored as extended attributes

Claim `FILE_NAMED_STREAMS`, parse `name:stream:$DATA` at the wire, and map a stream onto the
milestone 57 attribute store.

| | |
|---|---|
| New code | **estimated 500 to 700 lines** plus tests: a stream-name parse in `path.rs` and the colon fix beside it, stream-aware CREATE with dispositions, READ/WRITE/CLOSE and `SET_INFO`'s end-of-file on a stream handle, `FileStreamInformation` enumeration, delete-on-close as a remove. The estimate's basis is ksmbd's equivalent at roughly 200 lines of C over an existing xattr syscall, times this tree's usual ratio of documented Rust with host tests to reference C. |
| `Share` methods added | **4 to 6**, in all three implementations. |
| Wire surface added | one volume-attribute bit, one `QUERY_INFO` class widened, one path grammar. **No new `fs_proto` op**, which is the option's best property: ops 14-17 already do the work. |
| Bytes at rest | one extra directory entry, one extra node, one 4096-byte block for the node's blob under `.nife-attrs`. **The same order as option 1**, which was not obvious before it was counted. |
| The 3 KiB ceiling | **decides this option.** `AFP_AfpInfo` is 60 bytes and fits. A resource fork is unbounded and does not. Every write is a whole-value read-modify-write of up to 3 KiB, because the contract has no partial-value verb. |
| `MAX_COUNT = 16` | a file may carry at most 16 attributes, and macOS turns *every* extended attribute into a stream, so a file with 16 xattrs meets `ENOSPC`. |

**The unavoidable sub-decision.** A stream needs an on-disk attribute name. `user.DosStream.<name>:$DATA`
is what Samba's `streams_xattr` and ksmbd both use, so a disk written this way is readable by both;
anything else is ours alone. That is a thing two programs agree on and it is calef's.

**What it buys that option 1 does not.** Metadata survives a rename performed by anything, because
the store keys on the node. `redoxfs_host xattr` already prints it. And it is the precondition for
`READ_DIR_ATTR`, which is the one place a measurable performance win might be hiding, since the
client's bulk-listing path is gated on the same bit. Unmeasured, and it would move server cost up
while moving round trips down, so it should not be claimed until it is measured.

**Recorded honestly**: this option ships a share that refuses a large resource fork with an error the
client will surface as a failed copy. That is §42-correct and it is still a user-visible refusal that
option 1 does not have.

### Option 3: named streams, hybrid at rest, matching the reference

`AFP_AfpInfo` and ordinary extended attributes become attributes; `AFP_Resource` becomes a `._name`
AppleDouble file that **the server** writes and parses. This is `fruit:metadata = stream` with
`fruit:resource = file`, which is what calef's router runs.

| | |
|---|---|
| New code | **option 2 plus an estimated 250 to 400 lines**: an AppleDouble header encoder and decoder, entry table handling, and the empty-fork cases Samba spends two configuration options on. |
| `Share` methods added | option 2's, plus a path for the resource fork. |
| Wire surface added | option 2's. |
| Bytes at rest | option 2's, plus option 1's sidecar for any file with a resource fork. |
| The 3 KiB ceiling | **retired for the case that hits it.** The resource fork is a file. |

**Its one real argument** is that the on-disk result is byte-compatible with the working reference,
so a disk could move between the two servers. Whether anyone would ever move one is a question for
calef, and it is the only question that makes the AppleDouble encoder worth writing.

**Its cost that is not lines**: AppleDouble is a format two programs agree on, and this is the option
that puts *us* on the writing end of it rather than the storing end.

### Option 4: named streams, streams stored as files under a reserved name

Claim the bit, and store every stream as a real file under a reserved directory the way
`.nife-attrs` already holds attribute blobs. No AppleDouble, no 3 KiB ceiling, no compatibility with
anything outside this tree.

| | |
|---|---|
| New code | option 2's, plus a store: **estimated 300 to 500 lines** in the FS server or in the adapter. |
| `Share` methods added | option 2's. |
| Wire surface added | option 2's, **plus a reserved name and possibly a new `fs_proto` verb**, since a stream is a file-shaped object with offsets and the attribute verbs are not. |
| Bytes at rest | one node and one block per stream. |
| The 3 KiB ceiling | gone. |

**The honest reading**: this is the option with the fewest limits and the most invention, and its
argument is a general SMB file server rather than a Time Machine target. Milestone 55's scoping
decision already went the other way once, on the ranking principle.

## How reversible is it, and who has already acted

**Nobody has acted on it yet, which is the whole reason to decide it now.** No Mac has written a byte
of metadata to this share; `smb-serve` has been mounted read-only once and writable never, and the
`AAPL` context has never met a real client. The window in which this is cheap is open and it closes
the first time somebody's backup disk has bytes on it.

**By AGENTS.md's test it is irreversible in two of the listed categories at once.** It is a thing two
programs agree on, and it is bytes at rest that a customer depends on. The code is not the expensive
part in any of the four options.

**The migration exists and costs a converter.** Samba's `fruit:convert_adouble` proves that sidecar
to stream is possible and that somebody has to write it. The reverse direction, stream to sidecar,
has no reference implementation that this lane found.

**One asymmetry worth weighing.** Option 1 is the only option in which the *client* owns the format,
so a change in what macOS writes is not a change we have to chase. Options 2 through 4 put this tree
on the hook for a format Apple can change.

## What is blocked until this is answered

- **The `FILE_NAMED_STREAMS` bit**, and with it `READ_DIR_ATTR` and the client's bulk-listing path.
- **The colon in `smb_proto::path`.** It is recorded as a compatibility gap today and becomes a
  correctness bug the moment any option other than 1 is chosen. It should be fixed in the same lane
  that claims the bit, not before and not after.
- **Nothing on the Time Machine path.** Stated again because it is the finding most likely to be
  forgotten between reading this and acting on it.

## What this lane could not answer

- **Whether macOS in practice writes any extended attribute onto a Time Machine share.** The
  sparsebundle's contents do not, by construction. Whether `backupd` or Finder stamps the bundle
  directory or the share root is a question for a real Mac on real hardware, which milestone 55
  already records as its own gap.
- **The `._` file's actual byte length.** XNU's `ATTR_BUF_SIZE` of 4096 is the constant in the
  header; the writer itself now lives in a userspace `doubleagent` service that this lane did not
  read. The claim above is bounded rather than exact: whatever the length, it occupies at least one
  4096-byte RedoxFS block.
- **Whether `READ_DIR_ATTR` is a net win.** It moves work from round trips to the server and this
  tree has no measurement of either side.
- **The line estimates for options 2 through 4 are estimates.** The only measured comparable is
  ksmbd's C, and the ratio applied to it is a judgement.

## See also

- `design/roadmap/55-time-machine.md`, whose Apple-metadata paragraphs this answers.
- `notes/smb.md`, whose BUGS entry "Apple metadata is not implemented at all" is the reader-facing
  statement of the same gap, and whose Apple section is the `AAPL` claim table.
- `notes/xattr.md` for the store, its ceilings, and the rename property.
- §42 for the rule that a verb which is offered must be truthful, which is what makes the 3 KiB
  refusal in option 2 a stated cost rather than a bug.
- §57 for the caretaker forwarding, whose BUGS list already carried "`MAX_VALUE` at 3 KiB is
  untested against real Time Machine traffic".
