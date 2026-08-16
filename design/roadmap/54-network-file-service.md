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

What remains, in order: **a `statfs` verb for `fs_proto`** (free space is a nominal constant today,
and macOS sizes a Time Machine sparsebundle against what the volume reports), **subdirectories**
(the share model is flat and a sparsebundle is a directory of band files, which makes this the
largest piece between here and milestone 55), and **identity** beyond guest-for-everyone, which
writes made more urgent than reads did. The wire decisions of both halves are listed in their pull
requests for review, being the expensive category.

**It stays PARTIAL rather than BUILT deliberately**: the two items above are both required by
milestone 55, and marking 54 done would leave them with no home.

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
