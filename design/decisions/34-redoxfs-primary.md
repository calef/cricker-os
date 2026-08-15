# 34. RedoxFS is the primary filesystem, on three conditions

**Status: AMENDED.** (four amendment blocks below, including the one that closes condition 1.)

**Decided 2026-07-29 (calef), with the conditions attached deliberately so the label and its caveats
land together.** RedoxFS is the primary on-disk filesystem. It is not yet the *root* filesystem, and
§34.3 below is why that is a separate piece of work rather than a relabelling.

## Why this commitment is cheaper here than the words suggest

In a monolithic kernel, choosing a primary filesystem means linking tens of thousands of lines of
someone else's code into the TCB, where a bug in it is a kernel bug. Here the FS server is a confined
userspace component holding a capability to a block device, so a RedoxFS defect is a **data-integrity
bug, not a system compromise**: the kernel does not trust it and cannot be broken by it. That is what
the structure was for, and it means this decision is revisable at the cost of one component, which
milestone 23 exists to demonstrate. Recording it as a decision is still right, because a default
nobody wrote down is a decision nobody can revisit.

## What earns it the role

- **It is already somebody's root filesystem.** Redox OS runs on it. That is the exact use being asked
  of it, exercised by a real system rather than inferred from a design document, and it is the single
  strongest argument in its favour.
- **Copy-on-write with transactions**, so crash consistency is designed in rather than bolted on. That
  is the one property a primary filesystem must have and the most expensive thing to write oneself.
- **Rust, and no_std on both bare targets**, proven by us. It does not drag a libc into the FS server.
  (§31 makes a C component possible now, but possible is not free.)
- **Maintained upstream**, pinned at 0.9.1 with a patch discipline (`patches/`, two `Vec` imports).
- It is the reuse thesis made concrete: a real filesystem we did not write, running confined.

## The three conditions

1. **Crash consistency must be tested, not asserted (milestone 37). MET, 2026-07-30**; the
   measurement and the exact claim it earns are in the amendment below. It is RedoxFS's central
   selling point and we had never injected a torn write or a power cut. The claim rested on the
   upstream design description. For a project whose rule is measure rather than argue, that gap was
   worse than the missing verbs were, and it is the first thing a skeptic should ask about.
2. **Throughput must be measured (milestone 38).** `fs_read` reports the whole-path cost of a real
   read (~204 us under HVF, device-dominated, with `relay_rtt` putting the isolation tax three orders
   of magnitude below it), and it is deliberately ungated because the path is interrupt-driven. What
   does not exist is any MB/s figure, or any comparison against ext4 or APFS. The phrase "primary
   filesystem" invites a comparison we currently cannot make.
3. **The write path must be honestly complete**, which is `CREATE` and `TRUNCATE` (milestone 31 phase
   two, in flight). §27 records why `TRUNCATE` is not merely a feature: a write that half-works reads
   as a write that failed, and that sharp edge cost a day and produced three wrong root causes.

One encouraging measurement already in hand: the real engine under the FS server's own allocator, at
the 8 MiB cap, held a high-water of **352 KiB across thirty mount-and-write cycles**. So the budget is
generous headroom rather than a requirement, which was the main worry about it on small hardware.

## What would reverse this

If RedoxFS turned out to need `std`, or to need an allocation guarantee the budget model cannot give,
or if its **repair and recovery tooling is absent**. The first two have been probed and came out fine.
The third is unchecked, and for a primary filesystem "what do you do with a corrupted one" is a fair
question that deserves an answer before the label hardens.

## Alternatives considered

`crickerfs` is not among them, because it is not a competitor: a boot archive and a read-write
filesystem are different jobs, and the initrd wants exactly what crickerfs is. It stays.

- **Write our own.** Rejected on the same grounds as milestone 32 originally: the thesis is the kernel
  confining the filesystem, not the filesystem. A crash-consistent CoW filesystem is a large, subtle
  project that proves nothing the thesis needs.
- **ext2.** Simple, well documented, Rust implementations exist, and it buys real interop (mount the
  image on Linux). Rejected for a *root*: no journaling, so power loss means `fsck` and possible loss,
  which is a step down from CoW. ext4 has no serious no_std Rust write implementation, and writing one
  correctly is its own multi-month project.
- **FAT32 / exFAT.** `fatfs` is mature and no_std. Rejected for a root on semantics: no crash
  consistency at all, no permissions, no symlinks. It is the right answer for a future *boot* partition
  where interop is the point, and wrong for anything that must survive a power cut.
- **littlefs.** Genuinely power-fail-resilient, and wrong on two axes: it targets raw NAND/NOR with
  wear levelling rather than a block device, at microcontroller scale, and it is C, so it would put a
  foreign component in the storage path for no thesis gain.
- **btrfs / ZFS / F2FS.** No no_std Rust implementation, and a size that would dominate the project.
- **Build on a proven transactional store** (SQLite being the most battle-tested crash-consistency
  implementation in existence, needing only a VFS shim, which is precisely the seam §31 built).
  Interesting and not recommended: file-data performance would be poor and the novelty would need
  defending for no thesis benefit. Recorded because the crash-consistency argument for it is real.

## Amendment (2026-07-30): the xattr requirement, measured rather than argued

Milestone 55 surfaced a requirement §34 never considered: Time Machine over SMB wants **extended
attributes** (Samba's `streams_xattr`), and RedoxFS has none. calef's read was that this suggested the
choice was wrong rather than something to patch around, and **the strongest half of that is correct**:
extending RedoxFS's on-disk format would destroy two of the four reasons for choosing it, since it
would no longer be the filesystem Redox actually runs and every pin bump would pay for the divergence.
"We will patch xattrs in" is doubling down and should not have been offered as a co-equal option.

**What the requirement is not, stated precisely:** the application is not blocked. Samba's
`fruit:metadata = netatalk` keeps the identical Apple metadata in AppleDouble sidecar files and needs
no xattrs at all. So Time Machine runs on RedoxFS today; what is unavailable is the clean way to do
it. The preference for xattrs is right (we will want them anyway) but there is no time pressure, and
that distinction is what let this be decided by measurement.

**The check that decided it, and its result.** The question was whether a layer *above* the filesystem
could be crash-atomic, since a rename must move a file and its metadata together or a crash leaves
them inconsistent (§42's territory). **RedoxFS groups arbitrary mutations into one transaction**:
`fs.tx(|tx| …)` exposes `create_node`, `write_node`, `rename_node`, `truncate_node` and `remove_node`
on one `Transaction`, committed together, and `fs_server` already relies on it ("existence is checked
inside the same transaction as the create"). So **the layer is safe, the format stays unforked, and
§34 stands.**

The layer is also the *anti*-lock-in option rather than the sunk-cost one: xattrs implemented in the
FS server work over **any** backing filesystem, so they decouple the requirement from this decision.
Normally such a layer is bypassable and therefore worthless; here nothing can bypass it, because all
access goes through `fs_proto`. That is a capability-system property doing real work.

**Also found, and it corrects a claim made in milestone 55:** `rename_node` and
`rename_node_no_replace` already exist in RedoxFS. `fruit:posix_rename` and §42's rename work are
blocked on `fs_proto` lacking the verb, **not** on the engine.

## Amendment (2026-07-31): the xattr fork is closed, the layer, and it is reversible

The amendment above left the mechanism open. **Decided: extended attributes are implemented as a
layer in the FS server, not as an extension to RedoxFS's on-disk format.**

**The argument that decides it is reversibility, and it outranks the others.** `fs_proto` is the
contract; whether an attribute lives in a node's on-disk structure or in a store the server manages is
**invisible above that boundary**. So choosing the layer does not foreclose the format extension, if
attributes later prove central enough to justify diverging from a pinned upstream, or if the change is
accepted upstream, the implementation moves and no client changes. That makes this a low-regret
decision rather than a bet, which is the right shape for a mechanism nobody has exercised yet.

The supporting reasons, in order:

1. **It is crash-atomic, and that was measured rather than assumed.** `fs.tx(|tx| …)` groups arbitrary
   mutations into one commit, so a file write and its attribute write land together, and a delete
   removes both or neither. This was the check that had to pass before the layer was viable at all.
2. **Attributes key on `TreePtr<Node>`, not on a path**, because that is what the FS server already
   works in (`handles: Vec<Option<TreePtr<Node>>>`). **Rename is therefore free and correct**: a
   rename changes a directory entry, not the node, so attributes follow the file without any code
   knowing that a rename happened. AppleDouble sidecars get exactly this wrong, and so would any
   path-keyed store. This correctness property is **only available inside the FS server**, which is an
   argument for the layer rather than merely a consolation for not forking the format.
3. **It decouples the requirement from the filesystem choice.** A layer works over any backing
   filesystem, so if RedoxFS is ever replaced the attributes come along.
4. **It preserves §34's upstream reason.** The pin stays at 0.9.1 with a two-import divergence patch
   rather than a format fork every future bump would pay for.
5. **Nothing can bypass it**, because all access goes through `fs_proto`. On Linux this style of layer
   is worthless (anything can open the file directly), and here it is authoritative. A capability
   property doing real work.

**Three things to get right when building it**, recorded now because each is a way to get it subtly
wrong:

- **The attribute store must be invisible to enumeration.** A directory listing must not show it, or
  it becomes part of the namespace clients can name.
- **Deletion must be in the same transaction as the file's**, or a deleted file leaks its attributes
  and a later node reusing that pointer inherits them. That is a correctness bug wearing a
  housekeeping costume.
- **Recovery sees it.** `redoxfs_host extract` and upstream's FUSE mount will show the store as
  ordinary data. That is acceptable and arguably good (the attributes come out with the backup), but
  it must be a decision rather than a surprise, and `notes/host-recovery.md` should say so.

**What was rejected: extending the on-disk format.** Correct, and atomic by construction, and it costs
two of §34's four reasons: RedoxFS would stop being the filesystem Redox actually runs, and every pin
bump would pay for the divergence. Reversibility makes that cost avoidable rather than merely
deferred. calef's objection to "patching it in" was right, and the answer is not to patch it in.

## Amendment (2026-07-30): ZFS and XFS, and why RedoxFS is better-shaped than its size suggests

calef asked whether OpenZFS is best in class. **For a backup server, yes**: end-to-end checksums with
*repair*, snapshots, `send`/`recv`, scrub, no RAID write hole. And it is unavailable to us, for a
reason worth distinguishing from "too big": at roughly 400k lines of C it would *be* the project, but
more decisively **it is not a component you confine, it is a subsystem you host.** OpenZFS needs a
Solaris Porting Layer (kmem, mutexes, condvars, taskqs, VFS integration, page cache); §31's seam
confines a narrow interface, and ZFS's interface to its host kernel is enormous. Its ARC also expects
gigabytes where the VisionFive 2 has 4 to 8, and CDDL is worth checking against our licence posture
before publication.

**XFS: excellent, aimed at another problem, and weakest exactly where this application cares.** XFS v5
checksums **metadata only**; file data is unprotected, so bit rot in a backup is invisible until
restore. Its strengths (allocation groups for parallel metadata, extents, large-file throughput) target
a workload we do not have. It is journaling rather than copy-on-write, has no snapshots, cannot shrink,
and is ~100k lines of C more entangled with Linux VFS than ext4.

**The finding that reframes the comparison.** RedoxFS is architecturally ZFS-shaped where it counts:
copy-on-write, transactions, and **checksums stored in the parent `BlockPtr` and verified on every
`read_block`** (seahash recomputed, `EIO` on mismatch). The hash living in the *pointer* rather than
the block makes it a Merkle tree, which is ZFS's design and is strictly stronger than a header
checksum, because a header checksum cannot catch a misdirected write. It also encrypts. **So RedoxFS
beats XFS on data integrity and matches ZFS's integrity architecture, at a fraction of the size.**

What it genuinely lacks: **snapshots**, self-healing, scrub, compression, RAID-Z.

**And on a single disk, ZFS's headline advantage largely evaporates**, which is the observation that
should settle the anxiety. Self-healing requires redundancy; with one drive ZFS also only *detects*
corruption. calef's topology is a single USB drive, so the real gap is snapshots, scrub and
compression rather than integrity. Scrub is buildable on what exists today: read every block, let the
checksums verify themselves, report failures. That is a small program, not a filesystem feature.

**So the direction is snapshots, not migration.** Copy-on-write makes them tractable, and it is the one
missing property that matters for backups. If ZFS-class *redundancy* is ever wanted, that is a
multi-disk story and nothing within reach provides it.

**The pattern across the whole survey**, now covering ext2, ext4, XFS, btrfs, ZFS, F2FS, FAT and
littlefs: every mature filesystem with the properties we want is a large C codebase entangled with a
Unix kernel. That is not a coincidence to route around; it is why the choice was a small Rust
filesystem with the right architecture rather than a large one with more features.

## The alternative that could supersede this, and is not a filesystem choice

**A read-only measured root plus a writable layer.** §22 already gives us measured boot, so hashing a
read-only root image would extend integrity verification from init to the entire system, with writes
landing in a smaller, less critical layer (RedoxFS or anything else). That is a *stronger security
story* than a writable RedoxFS root, it sidesteps the repair question above by making the root
reproducible rather than repairable, and it is the shape Android and ChromeOS chose (dm-verity plus an
overlay). It is recorded here as the thing most likely to make this section a footnote, and it competes
on architecture rather than on engine quality, which is why choosing RedoxFS now costs little.

Note that switching engines would not address condition 1 at all: **no candidate's crash consistency
is tested here.** That is a gap in our harness, not in RedoxFS, and it is why the conditions matter
more than the choice. *That sentence was true when it was written and is now the thing milestone 37
fixed; the harness exists, and it would measure any engine put behind the same trait.*

## Amendment (milestone 37, 2026-07-30): condition 1 is met, and the claim is narrower than the words it replaces

**RedoxFS is crash consistent, in a sense that is now measured rather than described.** The docs may
stop saying "designed for crash consistency". They should not start saying it without the scope
below, because the scope is where the interesting part is.

**What is proven.** Take a workload of operations, each acknowledged only after the engine commits
it, and call the filesystem after the first `p` of them `S(p)`. Then:

> **For every point at which the device could stop, a fresh mount recovers exactly `S(p)` for some
> `p`.** Never a blend of two states, never a half-applied operation, never a length nobody wrote,
> never a mount that fails.

That is **prefix consistency**, and it is deliberately stated as a stronger property than the one the
milestone asked for. "Every acknowledged write is either wholly present or wholly absent" falls out
of it, and so does the thing that phrasing leaves open: a state where a later operation survived and
an earlier one did not. Two further assertions make it a measurement rather than a shape. `p` must be
**non-decreasing** as the cut point advances, so a later crash can never lose more than an earlier
one; and at the last cut point `p` must be the whole workload, so a filesystem that recovered the
initial state every time (perfectly prefix-consistent, perfectly useless) fails.

**The numbers, host side, exhaustive** (`fs_server/tests/crash_consistency.rs`, 0.6 s):

| injection | fault points | result |
|---|---|---|
| power cut, every write | 93 | all prefix-consistent, `p` monotonic, `p` = 7 with nothing lost |
| power cut with the last write **torn**, 4 offsets | 372 | all prefix-consistent |
| a device that **lies** (drop or tear one write, keep persisting after) | 186 | 112 recovered, 1 refused at the mount, 73 refused at a read, **0 silently wrong** |

**The limit, stated as plainly as the guarantee, because it is real.** RedoxFS's `Disk` trait has no
flush and no barrier, so write *ordering* is the device's job. A device that acknowledges a write it
has not persisted and then persists later ones can leave a valid commit pointing at a block that
never landed, and no filesystem promises otherwise. What RedoxFS does promise, and what the third row
measures, is that this is **never silent**: every `BlockPtr` carries a seahash of the block it names,
checked on every read, so a lost or torn block is an error rather than a wrong answer. Our block
server issues no `VIRTIO_BLK_T_FLUSH`, so on real hardware with a volatile write cache the durability
of the *last* acknowledged write is the device's word rather than ours; that is a gap in our driver,
not in the engine, and it is recorded here rather than in a footnote.

**The controls, which are the reason the rest counts.** Three, of increasing directness. The
lying-device sweep needs no tampering at all and still produces 74 images the filesystem refuses, so
the injector is demonstrably destructive. Removing the header ring's older generations leaves **92 of
93 fault points unmountable**, which isolates the fallback as the mechanism rather than a guess. And
a commit torn at 2048 bytes fails `Header::valid()` outright while the previous generation's slot
stays valid and stays older, which is the whole recovery argument in three assertions and no mount.

**A fourth control arrived unbidden and is worth more than the other three.** The first version of
the harness read *any* failed lookup as "the name is absent", so a dropped write to a directory's
tree block produced what looked like a filesystem that never existed, empty root and all. Nine fault
points reported a filesystem bug that was a test bug: `ENOENT` is the only error that means absence,
and the engine refusing to guess at a block whose checksum did not match is the property working.
An instrument that can produce a false positive and did is an instrument that is connected to
something.

**Two mechanisms we had named wrong, corrected here.** `cleanup: true` is **not** the header-ring
replay; `FileSystem::open` scans all 256 slots and keeps the newest valid one unconditionally, and
`cleanup` only releases unused nodes and commits on top. And the recovery is not "the newest
consistent generation" in any sense the engine computes: it is the newest generation whose *header*
still hashes, which is enough only because a commit's blocks are all written before it.

**On device, both ISAs, on a disk of its own.** The FS server is killed one block write into its
second transaction, with that block torn in half by a real virtio write, announcing the cut on its
readiness endpoint so the kill is provably the injector's. A **different FS-server process** then
mounts the same disk through the same block server, which is endpoint-only naming doing its job: the
block server never learns its client died and was replaced, because it never knew who its client was.
Its readiness sentinel is the consistency result, since `Server::open` refuses an image it cannot
make sense of. Both legs recover the acknowledged payload, whole, and the host tool re-reads the image
afterwards with the pinned engine and agrees.

**The crash test owns its disk, and that is §27's lesson applied before the fact rather than after.**
This test deliberately leaves a filesystem half-written. On the shared fixture, every other FS test's
result would have depended on whether this one ran first, which is precisely the order-coupled gate
that manufactured three incompatible root causes from three honest investigations. The fixture is
regenerated every run, `CRICKER_KEEP_REDOXFS` deliberately does not apply to it, and on the host every
fault point starts from a byte-identical clone of one image built in-process.

**What is still a design claim.** Ordering and durability at the device, above. Repair and recovery
tooling for an image the checksums *do* reject, which "What would reverse this" already names as
unchecked, and which this milestone sharpens rather than answers: we can now produce such an image
deliberately, and there is still nothing to hand a user who has one. Condition 2 (throughput,
milestone 38) is untouched.
