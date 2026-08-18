# 54. A network file service a Mac can actually mount

**Status: BUILT** (2026-08-17). A real Mac's own `mount_smbfs` mounts a share served by nife's
userspace SMB adapter over its own TCP stack, reads files byte-correct, remounts, **writes**, reports
the volume's real free space, serves **subdirectories**, and since 2026-08-17 **knows who is asking**.
Four halves landed over three days and each is gated on both ISAs; the wire decisions of every one of
them are listed in its pull request and in notes/smb.md, being the expensive category.

- **The mount** (#210, 2026-08-15): macOS 26's `mount_smbfs` against the QEMU guest, `ls`,
  byte-correct reads, a clean unmount and a second mount. `crates/smb_proto` carries the wire format
  host-tested, and the one correction worth reading is the SMB1 wildcard negotiate a real macOS opens
  with, captured and pinned as a test.
- **The write path** (#245, 2026-08-16): `WRITE`, all six create dispositions, `SET_INFO`'s
  end-of-file, rename, disposition and basic classes, and delete-on-close, gated by a file the host
  writes over SMB2 and a *different in-guest process* reads back through the FS server. Read-only
  remains expressible and is refused at the protocol layer rather than at the filesystem.
- **`statfs` and subdirectories** (#255, 2026-08-16): `fs_proto` grew **op 18, `STATFS`**, so a client
  no longer sizes its work against a constant; the share model became a **tree**, with
  `crates/smb_proto/src/path.rs` parsing a share-relative path once at the wire's edge, refusing `..`
  there in a type the `Share` seam cannot be handed around.
- **Identity** (#274, 2026-08-17): `crates/smb_proto/src/authenticator.rs`, a seam in `Share`'s shape
  with three verdicts, and `smb_server::CredentialAuthenticator` over milestone 65's verify endpoint.
  A share can require an NTLMv2 proof, and **the SMB server never holds the key that verifies it.**

Nothing gated this after milestone 107 merged on 2026-08-04, and the block nevertheless sat behind a
stale IN-PROGRESS status for eleven days before anyone noticed (2026-08-15, §76's defect class),
which is worth leaving on the record next to the finished status.

## Why identity is the half worth reading

Writes made it urgent rather than cosmetic: guest means everyone, and on a writable share that means
everyone who can reach the port may change it. But the interesting part is the shape of the answer.

**The claim is that the SMB server authenticates a session without ever holding the key**, and the
kernel suite has asserted a sentence by that name since milestone 65 against a stand-in. It now
asserts it against the real adapter, and three separate mechanisms hold it up, deliberately at
different rungs of AGENTS.md's ladder:

1. **`ntlm` is a `[dev-dependencies]` of `smb_proto`, not a dependency.** The shipping protocol code
   *cannot* compute a proof or a session key, and Cargo is the mechanism rather than care. Rung one:
   the wrong state is unrepresentable.
2. **The seam carries no key material in either direction.** An `Attempt` is a challenge, two public
   names, a MAC and the client's own blob. [MS-NLMP] §4.2.4.1.2's `SessionBaseKey` is deliberately
   absent, because this server does not sign and a field with no consumer is a field somebody has to
   justify removing later.
3. **The kernel looks at the frame afterwards**, through the direct map, which no userspace program
   could do, and requires the published `NTOWFv2`, the published session key, and every other nonzero
   byte to be gone. That is the check the adapter could not make about itself.

**This is the property Samba cannot offer.** There, `smbd` opens the password database, so
compromising it leaks every hash: crackable offline, reusable wherever the password was reused. Here
the adapter's whole authority over identity is one endpoint, revoking it ends the access, and no
compromise of it yields the ability to forge anything.

**The gate is what makes that more than an argument.** The password exists only on the host: xtask's
prober computes a real proof over the challenge the *guest* chose, and sends three AUTHENTICATE
messages down one connection, an anonymous login refused, a one-bit forgery refused, the real thing
accepted and not flagged guest, trying a `TREE_CONNECT` after each refusal because a refusal that
only changes a status word is not a gate. Four processes are on the path of one `ls`, each holding
one authority: a host client that knows the password, an adapter that holds no key, a store that
holds no network, and a filesystem server that holds no network either.

## What is honestly not done, and where each piece now lives

A BUILT status is not a claim that nothing remains. These are recorded in notes/smb.md's BUGS and at
the features themselves, and none of them belongs to this milestone's question:

- **The demo boot still admits guests**, so the boot a person actually runs is still open to everyone
  who can reach the port, and its banner says so. The reason is not a flag: there is **no way to tell
  a running system a password.** The only thing in the tree that provisions the credential store is a
  test program carrying [MS-NLMP] §4.2.1's published fixture, and a demo whose password Microsoft
  printed would be worse than a labelled guest share. **What closes this is a provisioning path**,
  which is milestone 56's subject rather than this one's, and it is now the head of the customer path.
- **The adapter is configured with a resource name**, which is one authority more than it needs. The
  right answer is a narrower capability, so the endpoint *is* the credential for one resource and the
  name is implied and unforgeable: DECISIONS §27's argument applied to `cred_proto`, and a change to a
  contract two programs agree on, so it is calef's.
- **Sessions are not signed.** Identity buys authentication of the client, not integrity of the
  stream. That is what `SessionBaseKey` is for and why the credential service publishes one.
- **The server challenge is a clock, not entropy**, and a repeated challenge is what makes a captured
  proof replayable.
- **Milestone 55 is unblocked, and takes over from here.** Its remaining SMB-side gap was identity;
  what is left on its side is Apple's own surface (`AAPL` create context, Time Machine flags, Apple
  metadata, `posix_rename`, durability), which was deliberately kept out of this milestone.

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
