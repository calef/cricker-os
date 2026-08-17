# 54. A network file service a Mac can actually mount

**Status: PARTIAL.** The finish line's first half fell 2026-08-15 (pull request #210): a real Mac's
own `mount_smbfs` mounted a share served by nife's userspace SMB adapter over its own TCP stack,
read files byte-correct, and remounted; `crates/smb_proto` carries the wire format host-tested, and
the one correction worth reading is the SMB1 wildcard negotiate a real macOS opens with, captured
and pinned as a test. The **write path** followed (pull request #245): `WRITE`, all six create
dispositions, `SET_INFO`'s end-of-file, rename, disposition and basic classes, and delete-on-close,
over the `fs_proto`-backed share, gated on both ISAs by a file the host writes over SMB2 and a
*different in-guest process* reads back through the FS server. Read-only remains expressible and is
refused at the protocol layer rather than at the filesystem.

**Gate: NONE.** The protocol question is settled (SMB, because calef's router already serves
Time Machine over it) and what is left is an adapter holding one directory capability and one
network endpoint. The old gate was milestone 107's missing listen verb; 107 merged 2026-08-04,
and this block sat behind a stale IN-PROGRESS status for eleven days before anyone noticed
(2026-08-15, §76's defect class). Nothing blocks the head of the customer path.

**The `statfs` verb and subdirectories both landed** (pull request #255, 2026-08-16), which was two
of the three remaining items. `fs_proto` grew **op 18, `STATFS`**: a record in the shared page with
`r0` as its length, no right demanded, any handle the server minted, and the record's length is its
version so a later field extends it rather than breaking a reader. The SMB volume classes report the
image's real numbers through it, so a client no longer sizes its work against a constant. The share
model became a **tree**: `crates/smb_proto/src/path.rs` parses a share-relative path once at the
wire's edge and refuses `..` there, in a type the `Share` seam cannot be handed around, and the seam
grew `open_dir`, `close_dir`, `mkdir`, `rmdir` and a per-directory listing. The adapter walks a path
one `OPENDIR` per component, because `fs_proto` resolves a single component under a handle and never
a path, which is the contract's shape rather than a limitation. The gate makes a directory over
SMB2, writes a file inside it, and a different in-guest process descends to it through the FS
server; the verdict distinguishes "the directory was never made" from "a *file* was made with a
separator in its name", which is what a flat share produces and what looks identical on the wire.

What remains is **identity** beyond guest-for-everyone, which writes made more urgent than reads
did. The wire decisions of every half are listed in their pull requests for review, being the
expensive category.

**The status does not move, and stays PARTIAL deliberately**: identity is required by milestone 55
and marking 54 done would leave it with no home. That was already this block's reason for staying
`PARTIAL` and it is unchanged; what changed is that the list it was holding open is now one item
long instead of three.

**In brief.** The board serves files over a protocol macOS speaks natively, so it is useful before
Time Machine specifically is solved.

**The protocol choice is the whole decision, and it is not obvious.**

| Option | macOS support | Size | Note |
|---|---|---|---|
| **9P** | **None** | Small | Plan 9's protocol, closest to our model, and calef cannot mount it. A demonstrator win with no user |
| **NFSv3** | Built in (`mount_nfs`) | Medium | RPC/XDR, mount protocol, portmapper. Usable immediately for general storage. **Not** a supported Time Machine target |
| **SMB3** | Built in | **Large** | **The one that is actually required**: the only path to Time Machine (milestone 55) |
| WebDAV | Built in | Small | HTTP-based, and not a Time Machine target |

**calef's router already exposes SMB for Time Machine (2026-07-30), which settles this.** SMB is
required regardless, so NFSv3 would be work thrown away, and 9P would be a demonstrator exercise with
no user. **Do not build a second protocol just to have an easier first one.**

What survives is a better decomposition than "pick a protocol". **The file service already exists**:
`fs_proto` over RedoxFS, milestone 32. A network protocol is therefore an **adapter** that speaks the
wire on one side and `fs_proto` on the other, holding **one directory capability and one network
endpoint**. So this milestone is the adapter *pattern* plus whatever protocol milestone 55 needs, and
9P or NFSv3 become optional later adapters rather than prerequisites.

That framing sharpens the security claim rather than just simplifying the build. The SMB adapter is a
**protocol translator with no storage authority at all**: it cannot reach the block device, cannot
enumerate outside the share, and speaks to the FS server only through the same contract every other
client uses. A compromise yields the share's contents and nothing structural.

**The capability shape, whichever protocol wins.** The service holds the share's directory capability
and a network endpoint. It cannot enumerate outside the share because no capability reaches there;
milestone 47's `enumerate`/`open`/`create`/`remove` split is what expresses "this client may write
backups but not delete them", which is a genuinely useful thing to be able to say to a backup client.

**Effort: not estimated**, and it depends entirely on the protocol chosen.
