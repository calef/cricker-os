# 146. NFS: serve and connect to network file systems

**Status: NOT-STARTED.** Minted 2026-08-21 by calef, as the other network file protocol milestone 54's
options table left explicitly *possible* and neither milestone 54 nor 55 would build. NFS is the
general-purpose network file system: a Mac can mount it, Linux and every BSD already does, and a
NAS on the same LAN serves it natively.

**Gate: NONE.** The adapter pattern is proven (milestone 54), `fs_proto` is stable, the network
stack runs, and the protocol is a published spec rather than a design fork. Nothing structural
stops a lane starting on either half; whether the first half is the server or the client is a
sequencing decision, not a dependency.

**In brief.** Two halves sharing one protocol library:

- **Phase A, the server**: an NFS adapter, like `smb_server` but for NFSv3 over UDP/TCP. A Mac's
  built-in `mount_nfs` or a Linux `mount -t nfs` against this board's IP would traverse the
  share's directory tree, read files, write them, and report free space, all through the same
  `fs_proto` seam the SMB adapter uses. The credential story is simpler than NTLMv2 (AUTH_SYS by
  default, with the option of Kerberos if the LAN demands it), and it is weaker: a uid/gid pair
  is a claim the server chooses to trust rather than a proof.

- **Phase B, the client**: nife mounting a remote NFS share. The shape inverts the adapter: a
  program holds a network endpoint to a remote server and an `fs_proto` *server* endpoint, so any
  nife caller can `ls`, `cat`, `cp` over a remote mount transparently. A `mount -t nfs` at
  swish's prompt is the demo, and `caps` says the mount point has the caller's directory
  capability to the remote, not to the local block device.

Both halves share **one `crates/nfs_proto`** carrying the ONC RPC / XDR layer, the portmapper
(`PMAP`) or rpcbind protocol, and the `MOUNT` and `NFS` procedure numbers and structures. The
SMB adapter proved that the protocol crate is the place for the host-testable wire logic; `nfs_proto`
is the same shape, and the rule that the seam never holds key material (§54) is even simpler here
because NFSv3 has no session key.

## Why NFS exists alongside SMB

Milestone 54 chose SMB because it was required for Time Machine, and it was right to. SMB serves
the macOS/backup path; NFS serves everything else:

- **Linux and BSD workstations** already mount NFS out of the box. A nife machine on a LAN with a
  Linux build server would share build artifacts, package caches, and source trees over NFS while
  serving backups over SMB. Two adapters, one store.
- **NAS appliances** (Synology, QNAP, TrueNAS) export NFS natively. Phase B mounts them, so nife
  gains terabytes of network-attached storage without a local block device.
- **The confinement story is the same across both protocols**: an NFS adapter holds exactly one
  directory capability and one network endpoint. It cannot enumerate outside the share, cannot
  reach the block device, and cannot name another share. `caps nfs_server` prints that difference,
  which is the demonstration milestone 123 asks for.

The shared adapter pattern is what makes each new protocol cheap: the serve loop, the shared-page
plumbing, the `fs_proto` decode, and the supervision entry are per-binary plumbing that decision 93's
rule already priced. Each new adapter adds the wire format only.

## What each phase owns, and what stops being nife's problem

### Phase A: the NFS server adapter

- `crates/nfs_proto`: ONC RPC / XDR framing, portmapper / rpcbind registration, `MOUNTPROC3_MNT`
  and `MOUNTPROC3_EXPORT`, `NFSPROC3_GETATTR`, `READ`, `WRITE`, `FSSTAT` (→ `STATFS`),
  `READDIRPLUS`, `CREATE`, `REMOVE`, `RENAME`, `MKDIR`, `RMDIR`, `SETATTR`, `LOOKUP`,
  `ACCESS`. Host-testable, like `crates/smb_proto`.
- `user/src/nfs_server.rs` or similar: the adapter binary, holding one directory capability and
  one endpoint. Receives `fs_proto` calls from the NFS decode and forwards them; the same
  capability-seam test milestone 54 ran applies: it cannot write outside the share, because no
  capability reaches there.
- **Read-only mode first** is the responsible build order: an NFS export that a stranger can mount
  and read, then write after the write path is tested, exactly the order milestone 54 used.
- **`statfs` is free**, because `fs_proto` already has `STATFS` and the adapter fills the NFS
  `FATTR3_SPACE_AVAIL` and `FATTR3_SPACE_FREE` from it. Milestone 54 discovered the same was true
  for SMB, which is how the op 18 decision was made.

### Phase B: the NFS client

- The client is the inverse adapter: an NFS `CLNT` handle toward a remote server, speaking `fs_proto`
  on the *server* side. A local process opens the endpoint, gets back an `fs_proto` handle that
  transparently resolves to the remote mount.
- **Read-only mount first**, same sequencing argument: list a remote share, read a file from it,
  then write.
- **A mount command**: `mount -t nfs server:/export /mnt` at swish's prompt, which spawns the
  client binary with the share's endpoint and a local directory capability.
- **Authentication**: AUTH_SYS for phase B, with the uid/gid the caller presents; the remote NFS
  server decides whether to trust it. Kerberos (RPCSEC_GSS) is a later phase; recording it here so
  the crate's structure does not exclude it.

### What both phases share

- `crates/nfs_proto` carries the ONC RPC record marking (RFC 5531), XDR encode/decode, and the
  procedure tables. Built around the same principle as `smb_proto`: no key material lives in the
  protocol crate, and a host test covers every decode path.
- **No kernel surface changes.** The network stack routes NFS UDP/TCP ports the way it routes any
  socket traffic. The adapter pattern means the kernel never reads an NFS frame.
- **`crates/portmapper`** as a small sub-crate or inline in `nfs_proto` if any adapter needs to
  register and no other consumer exists yet. The SMB server registers nothing publicly; NFS
  traditionally registers with `rpcbind` / portmapper, which is specific enough to want its own
  decision file if the fork is real.

## What is honestly not decided

- **Whether a second protocol is worth building before milestone 55 lands.** SMB serves the
  mac; NFS serves everyone else. That everyone-else question is not time-critical, and the
  customer path principle puts a family backup target ahead of Linux interop. This milestone is
  gated by sequencing judgment rather than by a dependency.
- **UDP versus TCP.** NFSv3 uses both; the early code should support TCP first (the network stack
  is proven there) and add UDP when a NAS that uses only UDP demands it.
- **The portmapper fork.** A dedicated `rpcbind` process is the Unix shape; a static port
  assignment is simpler and leaks nothing a capability system does not already control. Recorded
  here rather than in a separate decision file; the first lane chooses.
- **The version: v3 versus v4.** NFSv4 replaces the mount protocol, portmapper and `NULL` auth
  with a single port, a single procedure, and a stateful lease model. Milestone 54's table says
  NFSv3 and the existence of macOS's `mount_nfs` on that row; NFSv4 would be the right choice if
  starting fresh, and v3 is the right choice for interop with any ten-year-old appliance.

## BUGS

- **Nothing here is measured against a real NFS server or client.** The protocol crate would be
  host-tested before it sees the guest, but the end-to-end claim (a Mac mounts nife over NFS with
  `mount_nfs -o vers=3`, or nife mounts a Synology) needs hardware and a LAN.
- **The NFS access model (AUTH_SYS) is not an identity model.** A uid/gid is a claim the server
  trusts by convention; there is no proof. This is the same limitation the milestone 54 table
  named under "NFSv3" and the reason SMB was required for Time Machine. If the demonstration
  target ever includes a multi-user nife with NFS, the credential story would need RPCSEC_GSS or
  a local mapping layer.
- **`crates/nfs_proto` would be the third protocol crate in this tree** (`smb_proto`, `mdns_proto`,
  then `nfs_proto`), and the milestone-54 and 55 lane experience says the protocol crate is the
  place the bugs concentrate. Budget for it.
- **No estimate of effort**, because the adapter pattern is proven but the XDR encoder-decoder is
  an entire C-callable standard that this tree would own rather than vendor. The SMB adapter
  shipped in one lane per half; NFS's halves share more code and may need fewer.
