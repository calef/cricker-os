# 55. Time Machine: SMB3 with Apple's extensions, and mDNS

**Status: NOT-STARTED.**

**Gate: DECISION, MILESTONE 65, MILESTONE 107.** The scoping decision comes before any code: the
subset of SMB3 that Time Machine needs, or a general SMB3 server. Milestone 65 holds the key
`ntlm_response` computes with, and 107 is what lets a Mac connect at all. One dependency this block
names has no owner anywhere on the roadmap: `fs_proto` has no `RENAME` verb and the std PAL answers
`Unsupported`.

**In brief.** The actual goal, and **probably the largest single piece of work in the project**. It is
recorded at full size deliberately, because the failure mode here is starting it while imagining it is
"a file server".

## The reference implementation is known, and Chris supplied its exact configuration

**Chris's router is a GL.iNet GL-BE9300 (Flint 3) running OpenWrt, serving three family Time Machine
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

Verified, not assumed: **no xattr support in `fs_proto`, in the FS server, or in vendored RedoxFS.**
`streams_xattr` stores Apple metadata in NTFS-style alternate data streams backed by filesystem
xattrs, and we have neither layer.

**There is an escape, and it should be chosen deliberately rather than discovered late.** Samba's
`fruit:metadata` also accepts `netatalk`, which keeps the same metadata in **AppleDouble sidecar
files** (`._name`) needing no filesystem support whatsoever. Chris's router uses `stream` because ext4
has xattrs. So this is a **design choice between adding xattrs down the whole stack (protocol, FS
server, RedoxFS) and accepting sidecar files**, not the hard blocker it first appears to be.

## `fruit:posix_rename` lands squarely on work already scoped

Rename over an open file, which is precisely the territory of §42 (a filesystem declares what it
offers and must be truthful) and milestone 47's `mv` section. Note the current state: **`fs_proto` has
no `RENAME` verb at all** and `rename` is `Unsupported` in the std PAL. So milestone 55 has a hard
dependency on that gap being closed, and §42's concurrency-versus-crash atomicity split is exactly the
distinction Time Machine's durability expectations will test.

## Three users, and this is where the thesis gets a concrete demonstration

Chris's setup serves **graeme, corinne and chris**, one partition and one share each, and privacy
between family members rests on Samba correctly honouring a "Read-Write User = graeme" line in a
config file. A Samba bug, a misedit, or a path-traversal flaw crosses that boundary.

**Ours would be three adapter instances, each holding one directory capability**, and one adapter
**cannot name** another's partition. Not an ACL check that could be wrong: no capability, no path, no
way to express the request. That is the security claim of the whole project, stated in terms of
something Chris actually relies on, which makes it the best demonstration target on the roadmap.

It also means milestone 56's credential service holds **three identities**, not one, from the start.
(Built that way: the store's capacity is three, and the fourth `PUT` is refused with `FULL` rather
than silently replacing somebody, which is a thing the tests show.)

## mDNS is required after all, measured 2026-07-30

I hoped this could be dropped, on the grounds that Chris adds the share manually and the SMB-side
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

**Chris's router serves Time Machine over SMB today (2026-07-30).** That is a working reference
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
