# Extended attributes (milestone 57)

A named byte string attached to a file or a directory. Four verbs, a store the FS server keeps
above RedoxFS rather than inside it, and one property that is only available where we built it.

**Why this exists at all**: milestone 55 makes the board the Time Machine target for three family
members. Time Machine speaks SMB, Samba stores Apple's metadata as extended attributes
(`streams_xattr`), and RedoxFS has none. So this is on the critical path to hardware, not a
feature we thought would be nice.

The contract is `fs_proto::xattr`; the layer is `fs-server/src/lib.rs`; the mechanism was decided
in DECISIONS §34's 2026-07-31 amendment.

## The shape, in one picture

```text
  a client                    the FS server                    RedoxFS
  ────────                    ─────────────                    ───────
  GETXATTR  h "user.x"  ──►   node = handle h names            /.cricker-attrs/0000002a
  SETXATTR  h "user.x" v ─►   blob = read the node's file  ──► (one file per node,
  LISTXATTR h            ─►   apply the change                  named for its TreePtr id)
  REMOVEXATTR h "user.x" ►    write the blob back
```

Attributes key on the **node**, not on a path, because that is what the FS server already works in
(`handles: Vec<Option<TreePtr<Node>>>`). Everything interesting follows from that one choice.

## The four verbs

| Verb | Handle | Page | Second word | Reply |
|---|---|---|---|---|
| `GETXATTR` | the file or directory | the name in | the value out | `xattr::reply(kind, len)` |
| `SETXATTR` | the file or directory | the name, then the value | `xattr::spec(kind, value_len)` | 0 |
| `LISTXATTR` | the file or directory | the names out | 0 | bytes filled |
| `REMOVEXATTR` | the file or directory | the name in | 0 | 0 |

The handle field is the node itself rather than a parent directory, which is the one shape
difference from `OPEN`. An attribute has no name in any namespace, so there is nothing to resolve
it under, and a client that wants one opens the file first exactly as `fgetxattr(2)` does.

### The limits, and the reason for each number

| Limit | Value | Why that number |
|---|---|---|
| `xattr::MAX_NAME` | 255 bytes | Linux's `XATTR_NAME_MAX`. Nothing Samba writes is refused for its name; `user.com.apple.metadata:_kMDItemUserTags` is 40 |
| `xattr::MAX_VALUE` | 3072 bytes | A `SETXATTR` carries a name and a value in **one page**, so `MAX_NAME + MAX_VALUE` must leave 4096 visible room |
| `xattr::MAX_COUNT` | 16 per node | 16 names of 255 bytes plus their length prefixes is **4096 exactly**, which is what lets `LISTXATTR` have no cursor |

Each ceiling answers its own errno, and that is the point rather than tidiness: `ERANGE` for a name
this store cannot hold, `E2BIG` for an over-long value, `ENOSPC` for the seventeenth attribute. A
caller can act on which one it hit. DECISIONS §42's rule is that a verb which is offered fails
loudly rather than degrading, and a store that clipped a value to fit would hand back a file that
looked intact and was not.

### There is no attribute right

`GETXATTR` and `LISTXATTR` need `dir::READ` and are refused with `EBADF`. `SETXATTR` and
`REMOVEXATTR` need `dir::WRITE` and are refused with `dir::EROFS`. That is exactly what `READ` and
`WRITE` on the file need and answer, and no seventh rung was added to milestone 47's ladder.

An attribute is part of what a file *is*, not a separate object with its own authority, so a
capability that may read a file may read what is attached to it. The other half of the argument is
mechanical: adding a rung would silently widen or narrow **every grant that already exists**,
depending on which way the default fell, and milestone 47's whole monotonicity property is that a
capability's meaning cannot change out from under its holder.

## Why a layer, and why that is not the usual bad idea

On Linux, layering metadata above a filesystem is worthless: anything can `open(2)` the file
directly and walk around the layer. **Here nothing can.** Every path to these bytes goes through
`fs_proto`, so a layer above the filesystem is as authoritative as the filesystem. That is a
capability-system property doing real work rather than a consolation.

The argument that actually decided it was **reversibility** (DECISIONS §34). `fs_proto` hides which
implementation was chosen, so if attributes later prove central enough to justify diverging from a
pinned upstream, or if the change is accepted into RedoxFS, the implementation moves and no client
changes. Choosing the layer is therefore low-regret rather than a bet.

And it is crash-atomic, which was the check that had to pass before the layer was viable at all.
`fs.tx(|tx| …)` groups arbitrary mutations into one commit, so the file write and the attribute
write land together and a delete removes both or neither.

## The property only available here: rename is free

Set an attribute on a file, rename the file, read the attribute back. It works, and **nothing in
the rename path knows attributes exist**, because a rename changes a directory entry and the store
keys on the node.

AppleDouble sidecars get exactly this wrong: a `._file` beside `file` has to be moved by hand, and
every tool that renames without knowing about the convention orphans the metadata. So would any
path-keyed store. This correctness is only reachable inside the FS server, which is an argument for
the layer rather than a consolation for not forking the format.

## The three ways to get this wrong, and what each cost

DECISIONS §34's amendment named all three in advance. They are worth restating as the *mechanism*
rather than as a checklist.

### 1. A freed node's blob must die with it, or the next node inherits it

Node ids are recycled. If a file's attributes outlive the file, the next node the engine issues
that id gets somebody else's metadata attached to it. That is a correctness bug wearing a
housekeeping costume.

The purge asks the engine rather than guessing: `remove_node` answers `Some(id)` **exactly when a
node's last link went**, and `None` when a link remains, so `unlink` and `rmdir` purge on `Some`
and the decision is never ours to get wrong.

Rename replacement is the one removal the engine cannot tell us about. `rename_node` calls
`remove_node` on the destination and throws the freed id away, so `Server::rename` notices for
itself, and only when the destination's links were down to one.

The test provokes the reuse rather than reasoning about it (create, set, unlink, then create until
a node comes back with the freed id) and **asserts that the reuse happened**. Without that
assertion the interesting half of the test would go quietly vacuous the day the allocator changed.

### 2. The store must be invisible, in both directions

`.cricker-attrs` is refused by every name-taking verb (`check_component`) and filtered out of every
listing (`Server::read_dir`). A store a client could name would be part of the namespace, and then
"the attributes of a file" would be reachable as ordinary bytes by anything holding the directory
they live in.

The filter runs **before the cursor is applied**, not after. A filter after `skip(cursor)` would
make one real entry vanish at whichever offset the store happened to sort to, which is the sort of
bug that shows up as "one file is missing from `ls` on Tuesdays".

### The store directory goes with the last attribute, and the reason it was worth doing

An earlier version of this note recorded "the store directory is never removed" as a limitation, on
the grounds that noticing it had emptied would cost a walk. **That was wrong about the cost.**
`remove_node` with `MODE_DIR` already refuses a directory that still has entries, with `ENOTEMPTY`,
so the engine's own check *is* the emptiness test and there is nothing to enumerate. Attempting the
removal and accepting the refusal costs one lookup, on the removals that emptied a blob.

The reason to bother is on the recovery side rather than the byte. `redoxfs-host extract` copies the
store out with the tree, so a leftover empty `.cricker-attrs` would land in somebody's recovered
Documents folder as a directory nothing explains. A filesystem with no attributes on it is now
indistinguishable from one that never had any.

Both routes to empty are tested, because they are different code: `remove_xattr` writes an empty
blob and goes through `write_attrs`, while `unlink` purges one and does not. The assertion in the
middle is the one that keeps the test honest, and it says the store must **stay** while a second
node still has attributes; without it the test would pass just as well against an implementation
that removed the store on every removal.

### 3. A shrinking blob must be truncated, or the reader walks records nobody wrote

A write does not truncate (DECISIONS §27, four times corrected). So `write_attrs` writes the blob
and then truncates the store file to exactly its length. Without that, removing an attribute leaves
the previous blob's tail behind and the record walk reads it as more attributes. Here that failure
would present as *corrupted metadata* rather than as a longer file, which is worse, because the
checksums cannot see it: every block is exactly what somebody wrote.

## The type code, which nothing reads

Every attribute carries a `u32` **kind**. The layer stores it, returns it, and never interprets it.
`xattr::RAW` is zero, so a POSIX-style client that knows nothing about kinds writes the right one by
writing nothing.

Carrying a field nothing reads is speculative surface, and it is deliberate. `design/haiku-bfs-and-packages.md`
is the reason: BFS made attributes **typed and indexed**, with live queries over them, and its
author went on to build Spotlight. That is the ambitious version of this feature and it is a real
destination. A store of untyped blobs cannot become an indexed one later without a format migration
*and* a wire break; a store that round-trips a type code can. Four bytes a record and one packed
word, paid once, buys the option.

The structural link worth carrying: a BFS query returns **a set of files**, and milestone 47 already
decided that a set of files is granted by an `swarden` attenuated to a name set. So if attributes
ever become queryable here, the granting story is already designed.

**BUGS.** The kind is 31 bits, not 32, because it rides in the sign-protected half of a reply word
(the protocol's error convention is that a negative reply is a negated errno). BFS-style
four-character codes are ASCII and fit. A code with its top bit set is refused with `EINVAL` rather
than truncated into a type nobody wrote.

## EXAMPLES

Setting and reading one, through the sans-IO core (this is what a host test does):

```rust
let mut srv = Server::open(disk)?;
let h = srv.open_file("Documents.sparsebundle")?;

// 'CSTR' is BFS's spelling of a string type. Samba would write xattr::RAW.
srv.set_xattr(h, b"user.com.apple.metadata", 0x4353_5452, b"...opaque bytes...")?;

let mut page = [0u8; 4096];
let (kind, n) = srv.get_xattr(h, b"user.com.apple.metadata", &mut page)?;
assert_eq!(kind, 0x4353_5452);
```

The same thing on the wire, which is what `user/src/fsclient.rs` does:

```rust
// SET: the name and the value go into the shared page back to back; the value's length and its
// type code ride packed in the second word.
put_page(name);
put_page_at(name.len(), value);
call(FILE, fs::req(fs::SETXATTR, handle, name.len() as u64),
     xattr::spec(kind, value.len() as u64));

// GET: the name goes out on the page and the value comes back on it, so the reply carries the
// kind and the length and the bytes are read out of the page afterwards.
put_page(name);
let (r0, _) = call(FILE, fs::req(fs::GETXATTR, handle, name.len() as u64), 0);
let (kind, len) = (xattr::reply_kind(r0 as i64), xattr::reply_value_len(r0 as i64));
```

Getting them back off a dead board, which is what the whole feature is for:

```console
$ redoxfs-host xattr backup.img photo.jpg
         6  kind 0x43535452 'CSTR'  user.com.apple.metadata:_kMDItemUserTags
        32  kind 0x00000000  user.com.apple.FinderInfo
$ redoxfs-host extract backup.img / recovered
extracted / to recovered: 4 files, 3 directories, 0 symlinks, 165 bytes,
  3 attributes reattached, 1 type codes dropped
```

The attributes are on the recovered files, the raw store comes out beside them (which is where the
type codes stay, since no host filesystem has a field for one), and anything the host refused is
counted and named. See [notes/host-recovery.md](host-recovery.md).

## BUGS

Named here because a reader who meets the feature deserves to meet its edges at the same time.

- **You cannot create a file called `.cricker-attrs` anywhere**, at any rights, in any directory.
  The refusal is `EINVAL`, the same one `..` gets, because the name is not expressible here rather
  than not permitted. Reserving it in every directory instead of only in the root is a deliberate
  trade: the rule a client has to remember is one sentence.
- **The caretakers do not forward attribute requests.** `fwarden`, `dwarden` and `swarden` answer
  `EOPNOTSUPP` to all four verbs. That is §42-honest (a verb that is not offered fails loudly, and
  `EINVAL` would have read as "you sent nonsense") and it is uniform across all three on purpose, so
  behaviour does not depend on which caretaker happens to be in the chain. It is also a real gap: a
  program behind a per-file grant cannot read its file's attributes. Pass-through is buildable and
  is not built.
- **An unlinked-but-open file loses its attributes immediately.** POSIX would let `fgetxattr` keep
  working through the open handle until the last one closed; here the purge is in the unlink's
  transaction, because that is the only place it can be crash-atomic with the removal. Deferring it
  to the close would mean a server that died in between leaked the blob. The name goes and the bytes
  stay (that is `unlink`, and it is measured); the attributes go with the name.
- **All blobs live in one flat directory.** RedoxFS directories are H-trees, so lookup is hashed
  rather than linear, but a filesystem where most files carry attributes has a directory with an
  entry per file. That is fine for a backup target and is untested at a million entries.
- **No `XATTR_CREATE`/`XATTR_REPLACE`.** Set is set-or-replace, one operation. Linux's flags are
  emulable above a store only racily, and §42 forbids offering a verb whose guarantee we cannot
  meet. An attribute has no handle and no identity of its own, so "it already had a value" is not a
  fact a caller can act on differently.
- **`MAX_VALUE` is 3 KiB, and Samba's `streams_xattr` can be asked to hold whole alternate data
  streams.** A resource fork larger than 3 KiB is refused with `E2BIG`, loudly. Lifting the limit
  means chunking the transfer across requests, which this contract does not do for anything today.
  Whether Time Machine over `fruit:metadata = stream` ever exceeds it is unmeasured, and the board
  is where it will be measured.

## How it is proven

**Host, milliseconds, no emulator.** The whole of the layer's semantics is pure functions over a
byte slice (`fs_proto::xattr::store`), so what replaces what, which ceiling refuses, and what a
truncated blob means are all tested without a filesystem: seven tests in `fs_proto`, including a
sweep that cuts a blob at every byte and asserts it reads back **short rather than wrong**. Ten more
in `fs-server` drive the real engine against a `DiskMemory` image: persistence across a mount the
attribute was not written in, the rename property, the purge with a provoked node-id reuse, the
replaced-destination purge, the store's invisibility to seven verbs and to a listing, both rights
directions, every ceiling, and the shrink-without-a-tail.

**Crash-consistent, measured rather than argued** (milestone 57, closing what used to be a BUGS
entry here). The old claim was sound and second-hand: every mutation runs inside one `fs.tx`, and
milestone 37's sweep proves prefix consistency for whatever a transaction contains, so the property
held by construction. It is now in the sweep. `fs-server/tests/crash_consistency.rs` reads each
name's attributes as part of the filesystem's state, and the workload grew four attribute operations
covering the three shapes the store has: **creating** it (two node creations in one commit),
**growing** a blob, and **shrinking** one, which is the path that must truncate afterwards. They are
interleaved with a write to the same file, so the claim being decided is the interesting one: an
attribute lives in a *different file* from the data it describes, and a recovery holding the new
bytes without the new attribute (or the reverse) is a state that never existed and fails the sweep.

The workload's own sanity check does double duty here. An attribute operation changes nothing
observable unless the snapshot reads attributes, so a harness that quietly stopped looking fails at
the fixture rather than passing everywhere below it.

**On device, both ISAs** (DECISIONS §19). `user/src/fsclient.rs`'s proof role carries a witness that
reports a bitmap, and the kernel test asserts an **exact** set, so a client that could do nothing
and one that could do everything both fail. Eight claims: set and read back *with the type code*,
listed and nothing else, survived a rename, gone after a remove, gone after an unlink and remake,
an over-long value refused with `E2BIG`, the store unnameable, and the store absent from a full
cursor-driven enumeration of the root. It rides the existing FS-service test's third report word
rather than booting the three-process stack a second time.

## See also

- DECISIONS §34 and its 2026-07-31 amendment: why the layer, and the three things to get right.
- DECISIONS §42: a filesystem declares what it offers and must be truthful about it. Every refusal
  above is an instance.
- [notes/fs-server.md](fs-server.md) for the contract this extends and the transaction guarantee it
  rests on.
- [notes/host-recovery.md](host-recovery.md) for what a recovery host sees.
- [design/haiku-bfs-and-packages.md](../design/haiku-bfs-and-packages.md) for the typed, indexed
  version this deliberately does not foreclose.
