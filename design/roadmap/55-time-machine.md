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
  just no longer a stack-deep one. **It is written up as §99 and waiting on calef** (2026-08-18),
  with four options and their measured costs. The finding that block's reader most needs: **none of
  it is on the Time Machine path.** A backup writes a sparse bundle, which is directories and band
  files with no extended attributes and no forks, so this is a file-server feature milestone 55
  inherited by proximity rather than a Time Machine requirement. The second finding is that the
  sidecar option is not something this server implements: macOS's own VFS writes `._name` files when
  a share does not claim `FILE_NAMED_STREAMS`, so **the tree is already running it**, at zero lines.
  **The status does not move.** That lane answered the fork instead of building it, which is what it
  was asked for; 55 stays PARTIAL until a decision lands and something is built on it.

What remains of this milestone: Apple metadata (the choice above, then the SMB stream surface) and
the first contact with a real Mac. The durability macOS trusts landed 2026-08-18; see below.

**The durability gap closed 2026-08-18** (pull request #311), and the way it closed is worth more
than the code: the open question was whether to keep claiming `FULL_SYNC` when the stack did not
back it, and the answer was to make the claim true instead of answering the question. Two opcodes on
two contracts. `fs_proto::blk::FLUSH` (op 4) is a real `VIRTIO_BLK_T_FLUSH` the block server waits
for the device to complete, gated on `VIRTIO_BLK_F_FLUSH` being offered so a device that cannot
flush produces `EOPNOTSUPP` rather than a quiet success. `fs_proto::fs::SYNC` (op 19) is the
file-service verb behind SMB2's `FLUSH`: any handle the server minted, `dir::WRITE` required,
refused with `EROFS`.

**What makes it a gate rather than a code path** is where the witness stands. Both verbs answer with
a **count of completed device flushes** instead of a zero, so the QEMU suite's in-guest verifier,
which has synced nothing itself, can find the count already advanced when it first asks. Nothing but
the SMB server answering the host prober's `FLUSH` over TCP can have moved it. Then it syncs twice
and requires the count to move again, which is what separates a device round trip from a server
answering a constant. Both ISAs.

What is left is narrower and is recorded where a reader meets the claim: the sync is **device-wide**
rather than per file, and **nothing fences**, because `fs_proto` has no ordering primitive. A device
that lies about its own flush is outside anything a protocol can check, which is the same limit
notes/fs-server.md's crash-injection table records from the other side.

**The 64 KiB transfer reached the mounted share 2026-08-19**, and it is the first thing in this
block that made a backup *faster* rather than possible. Milestone 138 step 3 grew the file
contract's transfer to 64 KiB and measured 8.02x on a sequential write; **none of it reached a
mount**, because `smb_server` chunked every SMB read and write into `fs_proto::PAGE`-sized requests
and so turned a Mac's 64 KiB write into sixteen 4 KiB ones. Both clamps now read
`fs::TRANSFER_MAX` from the contract, and the kernel maps the whole channel at the adapter's
`FS_VA` because nothing checks that a client asked for no more than it mapped.

Measured through the host SMB prober rather than through `fs_proto` (`bench/smb-throughput.sh`, two
rounds, aarch64): **write 4.8x (0.065 to 0.31 MiB/s), read 2.4x (0.15 to 0.36)**. Most of the write
speedup arrived and about half the read speedup did, and **where the rest went is a different
contract**: `socket_proto::DATA_MAX` is 4080 bytes, so every 64 KiB SMB message still crosses the
socket contract about seventeen times in each direction, and that is now the dominant cost of a
transfer. It is step 3's defect one contract over; notes/smb.md's `BUGS` carries it with its
promotion trigger. **SMB's own ceiling binds too**: `smb_proto::MAX_TRANSACT` is 64 KiB for
`MaxRead`, `MaxWrite` and `MaxTransact`, so raising `fs::TRANSFER_PAGES` past 16 buys this path
nothing without raising that as well.

What that is worth to a backup, stated at the depth it was measured: **the write path alone** goes
from the record-level sweep's 17.6 hours for a 100 GiB first backup to **40 minutes** at step 3's
42.77 MiB/s, and this is what puts a Mac's bytes on that path. **No end-to-end hours figure is
offered**, on purpose: the table above is a debug build under QEMU user-mode networking, which is
the wrong instrument for a wall clock, and what transfers from it is the ratio rather than the rate.

**The status does not move.** This is throughput on a share that already worked; what remains is
unchanged, and the honest reading is that this milestone got cheaper to *use* rather than closer to
done.

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

## The path to BUILT, ordered and gated (scoped 2026-08-22)

This is the milestone's own scoping lane, run against the six-questions framework AGENTS.md asks a
fork to answer before it reaches calef. **The finding is that there is no fork left to bring him.**
Both irreversible decisions this block ever carried are already decided (the SMB3 subset, 2026-08-15;
the metadata stream-versus-sidecar split, §99, 2026-08-18), the mDNS wire protocol was written rather
than taken as a dependency and is built, and what remains is one gate: contact with a real Mac. That
gate is not code waiting to be written, it is **hardware waiting to exist**, and it has a dependency
chain worth making explicit because nothing in this block currently names it.

### Complete, and what closed each piece

1. **The scoping decision itself**, calef, 2026-08-15: the SMB3 subset Time Machine needs, not a
   general server. **Gate: NONE**, above.
2. **Discovery**, pull request #246, 2026-08-16: `crates/mdns_proto`, `crates/mdns_config`, and
   `user/src/mdns_responder.rs` answer `_smb._tcp`, `_adisk._tcp` and `_device-info._tcp` against the
   measured reference, both ISAs. Written, not vendored: it is a from-scratch parser and responder over
   an existing `smoltcp` feature flag, not a new dependency, which is §46's "write it, it's on the
   verification path" read correctly the first time. See notes/mdns.md.
3. **The `AAPL` create context and the Time Machine flag**, pull request #292, 2026-08-17: the bit
   that makes macOS accept the share at all.
4. **Identity**, inherited from milestone 54, pull request #274, 2026-08-17: NTLMv2 while the adapter
   holds no key.
5. **The Apple-metadata fork, answered rather than built**, §99, 2026-08-18: option 1 (say nothing,
   let the client write `._` sidecars, zero new code), and the workload that actually wants named
   streams split out as its own milestone, 137. The load-bearing finding survives repeating: **a Time
   Machine backup is a sparse bundle, directories and band files, and never touches this surface at
   all.** Milestone 55 carried it by proximity, not by requirement.
6. **Durability**, pull request #311, 2026-08-18: `FULL_SYNC` is now a claim backed by a real device
   `VIRTIO_BLK_T_FLUSH`, witnessed by a flush count that only a real round trip can advance.
7. **Throughput**, 2026-08-19: the 64 KiB transfer reaches the mount; measured 4.8x write / 2.4x read
   through the host SMB prober. `socket_proto::DATA_MAX` is now the binding ceiling and already carries
   its own promotion trigger in notes/smb.md's BUGS, which step C below is what fires it.

### Remaining: one gate, and it is transitively blocked on a milestone that is not this one

**Nothing left here is a design decision.** The remaining work is real-Mac contact, and three things
stand between here and it, in dependency order. This block's existing "Nothing here has met a Mac"
paragraph (below) names the QEMU limitation; it does not name the harder blocker underneath it, which
this scoping pass found by reading milestone 53 rather than assuming a board and a cable would do.

**Step A. A real NIC driver, on some board.** Every network driver in the tree is `virtio-net`, which
only exists under QEMU; real silicon has none. Milestone 53 ("The board's own peripherals: network and
storage on real silicon") is **Status: PARTIAL, Gate: HARDWARE**, and its own text is direct: "this is
where virtio stops carrying us." What it still owes is the JH7110's Synopsys DesignWare GMAC driver on
the VisionFive 2 (the riscv64 board already on the desk since 2026-08-14) and the PLDA XpressRICH
root-complex work that carries the NVMe driver to the real M.2 slot. **aarch64 has no board at all
yet**; notes/aarch64-board-survey.md is still choosing one. So the realistic near-term path to a real
Mac is through riscv64, not aarch64, purely because that is where hardware already exists. That is a
sequencing observation, not a design fork: it costs nothing to reverse if an aarch64 board arrives
first, and it does not change what either driver has to do.

**Step B. Multicast on a real segment.** slirp cannot carry it, so nothing about discovery has been
proven end to end; the closest this tree has come is an injected query that accidentally escaped
QEMU's NAT and came back from the real router, which is evidence the segment exists and not evidence
the responder works on it. Once Step A gives a board a real interface, this step is `dns-sd -B
_adisk._tcp` from a real Mac against the running responder, on the real family segment.

**Step C. A real backup, and a real power cut.** This project's own standard for durability claims is
"tested by actually cutting power" (milestone 53's storage half), which a QEMU flush count cannot
stand in for. That needs milestone 53's NVMe path on real silicon, not just the network half, so a
full validation of this milestone's `FULL_SYNC` claim waits on more of 53 than discovery alone does.
A narrower first experiment (mount, small write, clean unmount) needs only Step A and could run before
persistent storage lands, and separating those two experiments is worth doing explicitly rather than
waiting for all of 53 to call any of it started.

**Adjacent, not blocking: milestone 131.** The share this milestone builds still admits guests, and a
family backup target that admits guests is not one calef would trust with the only copy. Milestone 131
("A share is configured, not compiled, and its secret arrives from somewhere") is what closes that,
and it is **not** a prerequisite for the protocol-correctness gate above: the first real Mac contact
can and should happen against the guest share this tree already has, the same way every QEMU gate does
today. It is a prerequisite for the day this stops being a lab experiment and starts being the family's
actual backup target, which is a distinct milestone this block should stop trying to also be.

**What this scoping pass did not find**: an irreversible fork inside milestone 55 itself that needs
calef's decision before Steps A through C can start. The mDNS library question, the vendor-versus-write
question, and the Apple-metadata question that this lane's brief asked about are all already answered
in the sections above, most of them months before this pass began. The one sequencing call made here
(riscv64 before aarch64, because the hardware already exists) is offered as a recommendation rather
than a question, per AGENTS.md's own rule that a reversible fork gets a recommendation, not a lane.

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

**Corrected 2026-08-18, and the correction is about the reference rather than about us.** The
router's stanza sets `fruit:metadata = stream` and does **not** set `fruit:resource`, whose default
is `file`, meaning a `._` AppleDouble sidecar. So the working reference implementation is a
**hybrid**: Finder metadata in an extended attribute, resource forks in sidecars on the disk. Read
off Samba's own manual page rather than recalled. §99 carries this and the rest of the evidence.

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

**Superseded 2026-08-22, and left standing because it shows the delta between assumption and
measurement.** This section is the milestone's original framing, written before the capture it asks
for had happened. Every capture it calls for has since happened: the router is confirmed full Samba
with `vfs_fruit` (below), the mDNS records are captured (notes/mdns.md), and the SMB3 subset question
it poses at the end is the one decided 2026-08-15 and recorded at the top of this block. Read it as
the record of what was unknown, not as a live task list; "The path to BUILT" above is the current one.

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
  **Built 2026-08-18**, above.

**Considered and rejected: porting Samba over the §31 C seam.** It is superficially the right move,
since we already confine a component we did not write (RedoxFS) and the seam exists for exactly this.
It does not survive contact: Samba assumes `fork`, threads, and an enormous POSIX surface, and
milestone 52 records that we have no `fork` and that getting one is not cheap. Worth stating, because
it is an honest limit of the C-seam story rather than a gap nobody noticed.

~~**The scoping decision to make first**, before any code: whether to implement the subset of SMB3
that Time Machine needs, or a more general SMB3 server.~~ **Decided 2026-08-15**, at the top of this
block: the subset. The general server is milestone 137's question to reopen if it ever wants one, not
this milestone's.

~~**Effort: not estimated, and deliberately so.**~~ **Superseded 2026-08-22.** Effort from here to
BUILT is now bounded rather than open: it is milestone 53's hardware gate (Steps A through C above),
not an unscoped rewrite. "The path to BUILT" above is the current re-scope this note asked for.
