# 55. Time Machine: SMB3 with Apple's extensions, and mDNS

**Status: PARTIAL.** The **discovery half is built** (pull request #246, 2026-08-16): a responder
program binds UDP 5353 through a grant, announces `_smb._tcp`, `_adisk._tcp` and
`_device-info._tcp` from a configuration document carrying the values measured off calef's router,
and answers browses and legacy one-shot queries per RFC 6762 §6.7, gated on both ISAs. It holds a
report endpoint, the stack endpoint and a budget and **nothing else**: no share, no file, no TCP
port, where the reference implementation is one process with one config file.

**Gate: NONE.** The scoping decision is made: **the subset of SMB3 that Time Machine needs**, not
a general server (calef, 2026-08-15). Decided on the ranking principle: every part of a general
server the subset omits serves no customer this project has, and the subset's ceiling is
measurable against the working router where a general server's is a guess. The choice forecloses
little: milestone 54's mountable-share core and its protocol crate are the shared substrate, and
a general server would grow from the same crates. (The former MILESTONE 65 and 107 halves cleared
2026-08-04; found stale 2026-08-15 with the statuses that hid them.) Milestone 65 holds the key
`ntlm_response` computes with, and 107 is what lets a Mac connect at all. One dependency this block
names was recorded here as unowned: `RENAME`. **That is no longer true** (corrected 2026-08-14):
`fs_proto::fs::RENAME` is op 11, fully specified with its rights (`REMOVE` on the source directory,
`CREATE` on the destination) and its atomicity, and the std PAL implements `rename` against it. So
this block's third gate has closed and only the decision, 65 and 107 remain.

**The `AAPL` create context and the Time Machine flag landed 2026-08-17** (pull request #292), which
is the piece macOS refuses the share without. `crates/smb_proto` grew the create-context chain
([MS-SMB2] §2.2.13.2) and the `AAPL` tag's meaning as two modules, host-tested, and the QEMU gate's
prober now hangs the context off its first CREATE the way a Mac does and checks each claim
separately, on both ISAs. What the server claims is `UNIX_BASED`, **`FULL_SYNC`** (which is
`fruit:time machine = yes`, the one bit that makes macOS willing to hold a backup) and the model
string `TimeCapsule`; what it declines to claim, with a reason each, is `READ_DIR_ATTR`,
`OSX_COPYFILE`, `NFS_ACE`, `CASE_SENSITIVE` and `RESOLVE_ID`. notes/smb.md's Apple section is the
table.

**Two of this block's five remaining items changed shape on inspection**, which is worth more than
the code was:

- **`posix_rename` is not work.** The two behaviours Samba's `fruit:posix_rename` switches on are
  renaming onto an existing name and renaming a file that is open. The first is already
  `fs_proto::fs::RENAME`'s documented semantics; the second cannot fail here because this server
  enforces no share modes at all, so there is no sharing violation for POSIX semantics to be an
  exception to. The real defect next door is that `ReplaceIfExists = 0` is ignored, which is a
  rename that clobbers when the client asked it not to.
- **The metadata fork is a smaller question than this block assumed.** "We have no extended
  attributes at all" was true when it was written and is not now: milestone 57 built xattrs into
  `fs_proto` (ops 14-17), the FS server and the store. So the layer that made `streams_xattr` look
  expensive exists, and what is missing is the **SMB** half of alternate data streams (a stream name
  in a CREATE path, `FileStreamInformation`, `FILE_NAMED_STREAMS` in the volume attributes). The
  stream-versus-sidecar choice is still open and still a decision about what lands on disk; it is
  just no longer a stack-deep one.

What remains of this milestone: Apple metadata (the choice above, then the SMB stream surface), the
durability macOS trusts (below), and the first contact with a real Mac.

**The durability gap is named where a reader meets the claim, and it is the piece that should land
before anybody's real backup does.** `FULL_SYNC` is claimed further than the stack currently backs
it. True: the FS server puts every `fs_proto` write through one RedoxFS transaction that commits to
the header ring before the reply, so there is no write-back cache above the block device and SMB2's
`FLUSH` genuinely has nothing to do. Not true: the block server issues no `VIRTIO_BLK_T_FLUSH`, so
the durability of the last acknowledged write is the device's word rather than ours
(notes/fs-server.md's crash-injection table records the same gap from the other side). Closing it is
a device flush in the block server and a sync verb in `fs_proto`.

**The identity substrate arrived 2026-08-17** (milestone 54, pull request #274), which was this
block's other SMB-side prerequisite and the reason a Time Machine target could not have been serious
before: a backup share that admits guests is a share anyone on the segment can rewrite. What this
milestone inherits is a share that requires an NTLMv2 proof, an `Authenticator` seam in `Share`'s
shape, and a server that authenticates while holding no key. What it inherits as a *problem* is that
the boot a person runs still admits guests, because nothing can tell a running system a password: a
Time Machine target is the first thing in this tree that genuinely needs a **provisioning path**, and
that is milestone 56's shape rather than either of these two blocks'. See notes/smb.md's BUGS.


**Nothing here has met a Mac.** QEMU's user-mode networking cannot carry multicast to the host, so
`dns-sd -B` finds nothing under the emulator by construction; IGMP snooping, forwarding TTLs, a
live segment's mDNS traffic and a real querier all need hardware on the family network. The
lane's gate did accidentally prove the segment exists: its injected query escaped slirp, reached
the real router, and came back NATed with the very records the test expected, which is a false
green it caught with a source-address filter and recorded.

**In brief.** The actual goal, and **probably the largest single piece of work in the project**. It is
recorded at full size deliberately, because the failure mode here is starting it while imagining it is
"a file server".

## The reference implementation is known, and calef supplied its exact configuration

**calef's router is a GL.iNet GL-BE9300 (Flint 3) running OpenWrt, serving three family Time Machine
targets through Samba with `vfs_fruit` (2026-07-30).** So the reference is full Samba, not `ksmbd`,
and the working `[global]` stanza is on the record:

```
fruit:aapl = yes                 fruit:metadata = stream
fruit:time machine = yes         fruit:model = TimeCapsule
vfs objects = catia fruit streams_xattr
fruit:posix_rename = yes         fruit:nfs_aces = no
fruit:veto_appledouble = no      fruit:delete_empty_adfiles = yes
fruit:wipe_intentionally_left_blank_rfork = yes
```

That is a measured feature list rather than a guess, and it decodes into these requirements:

| Setting | What we must implement |
|---|---|
| `fruit:aapl = yes` | **The AAPL SMB2 create context.** The core of it: macOS negotiates Apple extensions on connect and will not accept the share without them |
| `fruit:time machine = yes`, `model = TimeCapsule` | Advertise the share as a Time Machine target and return the model string |
| `streams_xattr` + `metadata = stream` | **Alternate data streams**, for Finder metadata and resource forks. See below, this is the expensive one |
| `fruit:posix_rename = yes` | **Rename over an open file**, POSIX semantics |
| `catia` | Character mapping for names macOS permits and the backing filesystem does not |

## The discovery that changes scope: we have no extended attributes at all

**Stale as of 2026-08-17, and left standing because the argument below is still the argument.**
Milestone 57 built xattrs into `fs_proto` (`GETXATTR`/`SETXATTR`/`LISTXATTR`/`REMOVEXATTR`, ops
14-17), the FS server and the store, so the sentence this section is named after is no longer true
and the choice it frames is no longer a stack-deep one. What is still missing is the **SMB** half:
alternate data streams, which is a stream name in a CREATE path, `FileStreamInformation` in
`QUERY_INFO`, and `FILE_NAMED_STREAMS` in the volume attributes. The stream-versus-sidecar decision
is still open and still a decision about what lands on disk.

Verified, not assumed (**when this was written**): **no xattr support in `fs_proto`, in the FS
server, or in vendored RedoxFS.**
`streams_xattr` stores Apple metadata in NTFS-style alternate data streams backed by filesystem
xattrs, and we have neither layer.

**There is an escape, and it should be chosen deliberately rather than discovered late.** Samba's
`fruit:metadata` also accepts `netatalk`, which keeps the same metadata in **AppleDouble sidecar
files** (`._name`) needing no filesystem support whatsoever. calef's router uses `stream` because ext4
has xattrs. So this is a **design choice between adding xattrs down the whole stack (protocol, FS
server, RedoxFS) and accepting sidecar files**, not the hard blocker it first appears to be.

## `fruit:posix_rename` lands squarely on work already scoped

**Corrected again, 2026-08-17: it is not work at all.** The two behaviours Samba's
`fruit:posix_rename` switches on are renaming onto an existing name (already
`fs_proto::fs::RENAME`'s documented semantics) and renaming a file that is open (which cannot fail
here, because the SMB server consults `ShareAccess` nowhere and has neither oplocks nor leases, so
there is no sharing violation for POSIX semantics to be an exception to). This section's remaining
value is the correction below and §42's atomicity split, which is still exactly what Time Machine's
durability expectations will test.


Rename over an open file, which is precisely the territory of §42 (a filesystem declares what it
offers and must be truthful) and milestone 47's `mv` section.

**Corrected 2026-08-14.** This paragraph said `fs_proto` had "no `RENAME` verb at all" and that the
PAL answered `Unsupported`, and called that a hard dependency. Both halves were wrong by the time
anyone read them. `fs_proto::fs::RENAME` is op 11 with its rights and its atomicity documented, and
the PAL's `rename` packs both names into the shared page and issues the request; it returns
`unsupported_err()` only when no filesystem is granted at all, which is equally true of `open`.

**The correction matters more than the fact.** A false blocker on the customer path makes the work
look harder than it is, and the cost lands on whoever reads this block deciding whether to start. It
survived because a milestone block is written once and the tree keeps moving; §42's
concurrency-versus-crash atomicity split is still exactly the distinction Time Machine's durability
expectations will test, and that half was always right.

## Three users, and this is where the thesis gets a concrete demonstration

calef's setup served **graeme, corinne and chris** when this was written; as of 2026-08-15 it is
**corinne and chris** (measured: the router's `_adisk._tcp` TXT advertises `dk0=adVN=corinne` and
`dk1=adVN=chris`), graeme having migrated to Windows, whose backups leave Time Machine entirely.
One partition and one share each, and privacy between family members rests on Samba correctly
honouring a "Read-Write User = corinne" line in a config file. A Samba bug, a misedit, or a path-traversal flaw crosses that boundary.

**Ours would be one adapter instance per user, each holding one directory capability**, and one adapter
**cannot name** another's partition. Not an ACL check that could be wrong: no capability, no path, no
way to express the request. That is the security claim of the whole project, stated in terms of
something calef actually relies on, which makes it the best demonstration target on the roadmap.

It also means milestone 56's credential service holds **three identities**, not one, from the start.
(Built that way: the store's capacity is three, and the fourth `PUT` is refused with `FULL` rather
than silently replacing somebody, which is a thing the tests show.)

## mDNS is required after all, measured 2026-07-30

I hoped this could be dropped, on the grounds that calef adds the share manually and the SMB-side
`fruit:time machine = yes` might be what makes it acceptable. **Measured, and no**: `dns-sd -B
_adisk._tcp` on his network returns `GL-BE9300` in `local.`, so the router runs an mDNS responder and
advertises itself as a Time Machine target. The reference implementation does it, and the only way to
prove it *unnecessary* would be to disable it on a working family backup system, which is not a trade
worth making. **Assume required.**

So this milestone carries **two protocols**: SMB3 on TCP and mDNS/DNS-SD on UDP multicast (`5353`,
`224.0.0.251` / `ff02::fb`), the latter reusing the DNS wire format plus DNS-SD's PTR/SRV/TXT
convention and the probe-before-claim rules. **Check whether smoltcp gives us multicast group
membership** before estimating it.

**One structural detail from the measurement:** there is **one** `_adisk._tcp` instance for **three**
shares. The advertisement is per *server*, with the disks enumerated inside its TXT record
(`dk0=…`, `dk1=…`), not one announcement per share. Emitting three would be wrong.

Three service types are in scope: `_smb._tcp` (the server), `_adisk._tcp` (the Time Machine flags,
which is what populates the backup-disk list), and `_device-info._tcp` (the model string, where
`fruit:model = TimeCapsule` surfaces and which sets the icon macOS shows).

**Still to capture, and free:** `dns-sd -L GL-BE9300 _adisk._tcp local` prints the actual TXT keys and
flag values. Those bytes *are* the specification for what we must emit, and having the working ones
beats deriving them from the RFC.

## The remaining scope risk is still worth measuring directly

**calef's router serves Time Machine over SMB today (2026-07-30).** That is a working reference
implementation on his own network, so the requirement list below stops being something to guess at.
**The first task of this milestone needs no board and no code**: capture the SMB session between the
Mac and the router and read off the truth. The negotiated dialect, the capability bits, which create
contexts actually appear, what the mDNS records advertise, and which operations Time Machine really
issues. That converts this milestone's largest risk from unknown scope into a measured feature list,
and it is exactly the "measure, do not argue" rule applied to a requirement rather than a benchmark.

**Worth establishing what the router runs**, because it bounds the answer: if it is full Samba with
`vfs_fruit`, the reference is large; if it is **`ksmbd`** or another minimal server, then a much
smaller implementation is already known to satisfy Time Machine, and that is the target to match.

**What Time Machine over a network is believed to require** (from knowledge, *superseded by the
capture above* the moment it exists):

- **SMB3, not AFP.** Apple deprecated and removed AFP serving; SMB is the supported path.
- **Apple's SMB extensions**, the `AAPL` create context, which is what Samba implements as
  `vfs_fruit`. Without it macOS will mount the share but not accept it as a backup destination.
- **mDNS/Bonjour advertisement**, `_smb._tcp` plus `_adisk._tcp` carrying the Time Machine flags, or
  the share is not offered in the Time Machine UI. That is a second protocol (mDNS) on top of the
  first.
- **Durability semantics macOS trusts.** Time Machine writes a sparse bundle and depends on the server
  honouring flushes. This is the same clause §42 makes central, arriving as a compatibility
  requirement: a server that lies about durability produces backups that cannot be restored.

**Considered and rejected: porting Samba over the §31 C seam.** It is superficially the right move,
since we already confine a component we did not write (RedoxFS) and the seam exists for exactly this.
It does not survive contact: Samba assumes `fork`, threads, and an enormous POSIX surface, and
milestone 52 records that we have no `fork` and that getting one is not cheap. Worth stating, because
it is an honest limit of the C-seam story rather than a gap nobody noticed.

**The scoping decision to make first**, before any code: whether to implement the subset of SMB3 that
Time Machine needs, or a more general SMB3 server. The subset is much smaller and much less useful for
anything else; the general one is a project in its own right.

**Effort: not estimated, and deliberately so.** Anyone picking this up should re-scope it from scratch
against a verified requirement list rather than trusting this block.
