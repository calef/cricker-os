# SMB: the network file service a Mac can mount (milestone 54)

The head of the customer path. macOS speaks SMB natively and the Time Machine target (milestone
55) requires it, so SMB is the one network file protocol this tree carries; the roadmap block
records why NFS and 9P were refused. What milestone 54 builds is the **adapter**: a program
holding one network endpoint and one share, translating SMB2 on the wire into the share seam on
the other side. Its only storage authority is the one directory capability it is granted, so
"what can the network reach" is a statement about its cspace, not about a check it passes.

**A real Mac has mounted it** (2026-08-15, macOS 26 `mount_smbfs` against the QEMU guest): the
share mounts, `ls` lists it, both fixture files read back byte-correct, the volume arrives
read-only (macOS honours the `READ_ONLY_VOLUME` attribute, so a write is refused client-side
before it reaches the wire), a clean unmount works, and a second mount proves the listener
re-arms for a real client, not only for the test prober. The one correction the real client
forced is recorded below under "the SMB1 probe".

**The write path landed on 2026-08-16** and it is gated but not yet Mac-mounted: that run was
against a read-only share, and nobody has repeated it against a writable one. The BUGS section
says so where it matters. What exists now is `WRITE`, all six create dispositions, `SET_INFO`'s
end-of-file, rename, disposition and basic classes, delete-on-close, and a share that is
writable or not **by declaration**, refusing at the protocol layer rather than at the
filesystem.

**Identity landed on 2026-08-17**, which was the last item on this milestone's list. A share can
now require an NTLMv2 proof that milestone 65's credential service accepts, and **the SMB server
never holds the key that verifies it**: it holds one endpoint to a sealed store, and `smb_proto`
takes the `ntlm` crate as a *dev*-dependency, so the shipping protocol code cannot compute a proof
at all. Both ISAs' gates now run an authenticated share, with a host process computing a real proof
over the challenge the guest issued, and the kernel then reads the page between the adapter and the
store and requires it to be empty. **The demo boot (`smb-serve`) still admits guests and still says
so**, for a reason worth knowing before you read further: there is no way to *tell* it a password.
See BUGS.

## The pieces

| Piece | Where | What it is |
|---|---|---|
| `smb_proto` | `crates/smb_proto/` | The whole wire format: framing, header, every command (both directions since 2026-08-16), NTLMSSP, minimal SPNEGO, create contexts including Apple's `AAPL` (2026-08-17), and the per-connection state machine. Pure logic over byte slices, host-tested, `no_std`. Client-side builders live in the same crate so tests and the prober share every offset with the server. |
| `smb_server` | `user/src/smb_server.rs` | The adapter program: listen/accept through the socket contract (milestone 107), reassemble direct-TCP framing from bounded `RECV` chunks, hand messages to the state machine, chunk the answers back out. |
| The SMB prober | `xtask/src/main.rs` | The host side of the QEMU gate: a real SMB2 client that negotiates, **authenticates with a real NTLMv2 proof it computes itself**, connects the share, opens the seeded file and asserts its bytes, then writes a second file it never reads back, twice over two connections. It is the only party anywhere that knows the password. |
| The authenticator seam | `crates/smb_proto/src/authenticator.rs` | `Share`'s sibling: a trait with no IO, three verdicts, and an `Attempt` carrying only public bytes and a MAC. `NoIdentity` is the guest policy, spelled as a value so a boot has to *say* it wants guests. |
| `CredentialAuthenticator` | in `smb_server` | The implementation that does the IO: one `CALL` on the credential service's verify endpoint. Holds no key and asks for no session key. |

The share behind the adapter is the `Share` trait in `crates/smb_proto/src/share.rs`, with a
boot-time choice between its implementations (`smb_server`'s `arg2`, which since the write path
says both which backing and which direction):

- **`FsShare`** (in `smb_server`, where the IPC lives): the real one. The adapter holds a
  directory capability into the FS server (the endpoint IS the capability, DECISIONS §27) and
  answers every `Share` question with `fs_proto` verbs, so what a mounted client reads **and
  writes** is the RedoxFS image. This is what the test boots and `smb-serve` wire whenever a
  RedoxFS disk is attached, both of them read-write. Landing the read half changed no protocol
  code, which was the seam's whole promise; the write half did change the seam, and the two
  changes are listed under the wire decisions below because they are contract changes rather
  than code.

  The `fs_proto` verbs it uses, at the rights those verbs document, are `OPEN` and `READ`
  (`dir::READ`), `WRITE` and `TRUNCATE` (`dir::WRITE`), `CREATE` (`dir::CREATE`), `UNLINK`
  (`dir::REMOVE`), and `RENAME` (`REMOVE` on the source, `CREATE` on the destination). Nothing
  was invented: the adapter asks, and the FS server refuses what the capability does not carry.
- **`FIXTURE`** (in `smb_proto`): files baked into the binary, kept as the no-disk fallback. It
  is what lets the protocol path run with no FS service in the boot, and what the host tests
  drive the state machine against, where a share that cannot be wrong is a feature. **Read-only,
  and it is the trait's worked example of a backing that says so**: it implements `writable()` as
  `false` and none of the write half, so the trait's defaults refuse everything.
- **`MemoryShare`** (in `smb_proto`, `#[cfg(test)]`): a writable share in memory, so the write
  path's host tests have something to write *to*. The fixture's argument, one direction over.

The gate proves the distinction rather than asserting it: the combined boot first runs
`fs_test_client`'s seed role, which writes `fs_proto::fixture::SMB_SEED` through the FS server,
and the prober then opens that file over the mount and asserts its bytes. Bytes a different
process put on the filesystem through fs_proto coming back over TCP is the claim
"RedoxFS -> fs_proto -> `Share` -> SMB2 -> TCP" made checkable; the baked-in fixture could not
have answered it.

## The wire decisions, and why

These are the expensive-to-reverse choices (AGENTS.md, "anything two programs agree on"), listed
so review can happen where the cost is:

- **Direct TCP on port 445**, the 4-byte zero-type NetBIOS-shaped prefix. No port 139, no
  NetBIOS session service.
- **SMB 2.1 (`0x0210`), only.** 2.0.2 predates features macOS wants; the 3.x family drags in
  signing enforcement, encryption and `VALIDATE_NEGOTIATE_INFO`, none needed for a first mount.
  macOS negotiates 2.1 happily (it is the dialect of a decade of NAS boxes).
- **NTLMv2, and guest only when a boot asks for it.** The server answers the NTLMSSP dance (raw or
  wrapped in SPNEGO, which is how macOS sends it), takes the AUTHENTICATE apart, and asks the
  `smb_proto::authenticator::Authenticator` seam whether the proof checks out. Three answers, three
  wire outcomes: `Authenticated` is `STATUS_SUCCESS` with `SessionFlags` **clear**, `Guest` is
  `STATUS_SUCCESS` with `SESSION_FLAG_IS_GUEST` set (the honest label for "nothing was verified"),
  and `Refused` is `STATUS_LOGON_FAILURE` with the connection left open so a client can retry, which
  macOS does after prompting.
- **`STATUS_LOGON_FAILURE` (`0xC000006D`) for a bad proof *and* for an anonymous client**, and it is
  one status on purpose: distinguishing them would make session setup an oracle for which accounts a
  store holds. It is what Windows and Samba answer, so a real client's retry logic already knows it.
- **An anonymous AUTHENTICATE is a distinct thing from a failed one.** `Authenticator::anonymous`
  answers it, defaulting to a refusal, and that default is the entire difference between a guest
  share and an authenticated one. It matters because `mount_smbfs -N` and this tree's own prober both
  send an AUTHENTICATE with every field empty: a server that read "no proof" as "nothing to check,
  therefore fine" would admit exactly the caller identity exists to shut out.
- **The seam carries no key material in either direction, and `SessionBaseKey` is deliberately not
  in it.** An `Attempt` is the server challenge, the presented account and domain (UTF-16LE, as they
  arrived), the `NTProofStr`, and the client's blob: all public, or a MAC that is worthless without
  the key. [MS-NLMP] §4.2.4.1.2's session key is what a *signing* server would need, this one does
  not sign, so the adapter never asks the credential service for it. Adding it later is a widening
  with a stated reason rather than a field somebody has to justify removing.
- **The presented account name is not a lookup key.** `cred_proto::verify::NTLM_PROOF` names a
  *resource*, which the adapter is configured with, so the wire's only contribution is challenge,
  blob and proof. The account is bound **cryptographically** instead: the stored `NTOWFv2` was
  derived over the account that owns the resource, so a client claiming a different name derives
  under a different key and fails, and nothing anywhere compares strings. See BUGS on why the
  resource being a *constant* rather than an implication of the endpoint is the part that is wrong.
- **Sessions are still not signed.** A session is proven at setup and unprotected afterwards.
  Identity buys authentication of the client, not integrity of the stream; that is in BUGS.
- **`MaxTransactSize`/`MaxReadSize`/`MaxWriteSize` = 65536**, the floor mainstream clients are
  written against, and exactly the static buffer the allocator-less server carries.
- **One share, named `share`**, a **tree** of directories and files, **writable when the boot says
  so.**
  The direction is `smb_server`'s `arg2`, which the write path grew from a flag into three values
  (fixture, fs-backed read-only, fs-backed read-write) because "which backing" and "which
  direction" are two questions and a boolean answered only one. Both boots that exist wire
  read-write.
- **Read-only is refused at the protocol layer, not at the filesystem.** Every mutating command
  asks `Share::writable()` *before* the backing hears about it, so a read-only share is read-only
  even over a directory capability that would have permitted the write. `Share::writable` has no
  default, so a backing cannot be written without stating its direction; the mutating trait
  methods then default to a refusal as an independent second line. The status is
  `STATUS_ACCESS_DENIED` throughout, including for the timestamp write a copy ends with, because
  a partial refusal is worse than a whole one.
- **`FILE_OPEN_IF` on a read-only share is demoted to `FILE_OPEN`, not refused.** "Open it if it
  is there" is answerable without writing anything, and clients that open everything that way
  would otherwise break on a share they are only reading.
- **The status a write refusal carries is `ACCESS_DENIED`, not `MEDIA_WRITE_PROTECTED`.** It is
  what the read-only mount was proven against with a real Mac, and what the host tests pin;
  changing it would be a wire change bought with nothing.
- **`DesiredAccess` is not gated.** A create asking for write access on a read-only share is
  refused by its *disposition*, and the commands are refused by command. Gating the access mask
  as well risked breaking the proven read mount (macOS asks for generic masks it does not use),
  and it would buy no property the disposition gate does not already hold.
- **A file is named by an opaque id the backing mints, not by its index in the listing.** The
  read-only trait could use an index because nothing reordered the directory; a writable share
  reorders it on every create. The fs-backed share makes the id the FS server's own handle, which
  also retires the open-per-request cost the read path recorded.
- **`FileAllocationInformation` is a no-op and `FileBasicInformation` is discarded.** Both are
  successes that change nothing, and both are in BUGS: preallocation is a hint whose obvious
  implementation (truncate) would zero-extend a file the client is about to fill, and there is no
  clock capability here to record a timestamp against.
- **Free space is the image's, through `fs_proto`'s `STATFS`** (op 18, milestone 54). The record
  is three little-endian `u64`s in the shared page (allocation unit, total units, free units) and
  `r0` is its length, which is `READDIR`'s and `LISTXATTR`'s existing shape: a reply word carries
  one `i64` and this answer is three numbers. **The record's length is its version**, so a later
  field extends it and a client written against this one reads its prefix; there is deliberately no
  version word, because the length already is one. The verb demands **no right** and takes any
  handle the server minted, file or directory: the handle is the qualification rather than the
  subject, and demanding `READ` would leave a write-only grant unable to answer the one question it
  has. A backing that cannot ask (the baked-in fixture has no volume) answers `None` and the
  protocol layer falls back to `NOMINAL_VOLUME_BYTES`, stated rather than silent. **A read-only
  share reports zero free** whatever the image says, which is the same statement `READ_ONLY_VOLUME`
  makes one field over and is what makes macOS refuse a write client-side.
- **A path is parsed once, at the wire's edge** (`crates/smb_proto/src/path.rs`), and the `Share`
  seam takes a `Path` that cannot be constructed without that parse. What a client is allowed to
  *say* is wire format, so `..` dies where the bytes arrive rather than wherever a backing happens
  to look at it. `.` is refused as well, for a different reason: it is a second spelling of a path,
  and a handle's path is its identity here (rename and delete-on-close both read it back), so one
  path per name is cheaper to hold than a canonicaliser. A forward slash is refused because SMB's
  separator is backslash and accepting both would let two clients spell one file two ways. A single
  leading and a single trailing separator are stripped, because clients send `dir\`.
- **`fs_proto` resolves a component under a handle and never a path**, so the adapter walks: one
  `fs::OPENDIR` per component, then the verb on the leaf under the parent's handle. That is the
  contract's shape rather than a limitation to route around, and it is why the descent's rights are
  exactly what the share will use rather than `dir::ALL`: `OPENDIR` refuses with `EPERM` when the
  intersection with the parent is smaller than the request, so asking for everything would fail on
  a capability that was correctly narrowed.
- **`MKDIR` and `RMDIR` are separate from `CREATE` and `UNLINK`**, and `RMDIR` takes only an empty
  directory. A call that removed whatever it found would put a subtree behind one message, and no
  capability check afterwards could undo that; the recursion belongs in whoever is deleting, as a
  loop of individually refusable steps. A client's delete-on-close on a non-empty directory
  therefore leaves it there.
- **A directory may be renamed in place but not moved into another directory**, which is
  `fs_proto::fs::RENAME`'s own boundary (the cycle guard is an ancestry walk in a server whose stack
  is measured at three quarters used). The one refusal this layer adds is moving a directory into
  its own subtree, checked on paths here because this is the only layer holding both sides as paths
  at once.
- Compounds (macOS stats files as CREATE + QUERY_INFO + CLOSE related chains) are implemented;
  credits are granted as asked and never accounted.
- **The `AAPL` create context is answered, and the bits it claims are the table above** (milestone
  55). The chain is walked generically, the tag is matched, and the answer echoes the request
  bitmap and carries exactly the answers it asked for. **A context this server does not implement
  is walked past in silence, never refused**, because an unanswered context is how this mechanism
  says "not implemented" and refusing would trade a working mount for a diagnosis nobody reads. A
  malformed chain is the same: the open still succeeds with no context back.
- **`FLUSH` resolves its file id and then does real work** (milestone 55). The file id is checked
  first, so a stale handle is `STATUS_FILE_CLOSED` rather than a blanket yes; then `Share::sync`
  is called, which on the fs-backed share is `fs_proto::fs::SYNC` and, under that, a
  `VIRTIO_BLK_T_FLUSH` the device completes before the reply. A backing that cannot flush its
  storage returns an error and the client sees it. See the Apple section for why that mattered
  enough to be worth a milestone of its own.
- **The SMB1 probe.** The machine overruled the assumption that a modern client opens with SMB2:
  macOS's `mount_smbfs` still opens with an **SMB1** multi-protocol NEGOTIATE (`\xFFSMB`,
  command `0x72`, dialect strings `NT LM 0.12`, `SMB 2.002`, `SMB 2.???`), and the first cut of
  this server dropped it as not-SMB2, which presented as every real mount timing out while the
  test suite stayed green (the suite's prober politely opened with SMB2). The fix is [MS-SMB2]
  §3.3.5.3.1: answer the probe with an SMB2 NEGOTIATE response carrying the wildcard revision
  `0x02FF`, after which the client negotiates properly. The captured bytes are pinned as a host
  test in `smb_proto::server`, so the message a real client actually sends is now part of the
  gate. An SMB1-only client (no SMB2 dialect strings) is still dropped.

## The Apple half: the `AAPL` create context (milestone 55, 2026-08-17)

macOS mounts a plain SMB2 share and **never offers one as a Time Machine destination**. What it
looks for is a create context: it hangs an `AAPL`-tagged blob off the first CREATE of a tree
connect and reads the server's answering context off the response. That is the whole of
`fruit:aapl = yes` on the reference implementation, and it is the first line of the working
configuration design/roadmap/55-time-machine.md records.

Two modules, because they are two things:

- **`crates/smb_proto/src/create_context.rs`** is the chain ([MS-SMB2] §2.2.13.2): generic, and
  reusable because a real macOS CREATE also carries `DHnQ` (durable handle), `MxAc` (maximal
  access), `QFid` (on-disk id) and `RqLs` (lease), and the server has to walk past them to find
  the one it answers.
- **`crates/smb_proto/src/apple.rs`** is what the `AAPL` tag means. There is **no public
  specification**: [MS-SMB2] defines the container and says nothing about this tag, so the layout
  is the one Samba's `vfs_fruit` puts on the wire and macOS has been talking to for a decade. That
  file says so at the top, and every constant in it is there because the reference emits it.

**What this server claims, and it is the expensive half**, because a claim is something a client
acts on:

| word | set | left clear, and why |
|---|---|---|
| server capabilities | `UNIX_BASED` | `READ_DIR_ATTR` would promise Apple's extended listing (Finder info and fork sizes inside the dirinfo) and there is no Finder info here; `OSX_COPYFILE` would promise a server-side copy that arrives as an `FSCTL` this server refuses; `NFS_ACE` is off on the reference too (`fruit:nfs_aces = no`) |
| volume capabilities | **`FULL_SYNC`** | `CASE_SENSITIVE` would be untrue in the other direction: the backing filesystem is case-sensitive but this server folds every name to lower case at the wire, so what a client can observe is a share that is not. `RESOLVE_ID` would promise resolving a file by an on-disk id nothing here mints |
| model | `TimeCapsule` | matching `fruit:model = TimeCapsule`. **Not** the `_device-info` mDNS model, which the reference sets to `MacSamba`; notes/mdns.md's capture found the working reference running with the two disagreeing, so they are two knobs and not one |

**`FULL_SYNC` is `fruit:time machine = yes`**, and it is the single bit on the SMB side that makes
macOS willing to hold a backup here.

**It was claimed further than the stack backed it, and as of 2026-08-18 it is not.** The gap is
worth keeping on the record rather than quietly deleting, because it is the shape of mistake this
project is most exposed to: two layers, one of them genuinely covered, and a claim written against
the covered one.

- **Always true**: the FS server puts every `fs_proto` write through one RedoxFS transaction that
  commits to the header ring *before* the reply. There is no write-back cache above the block
  device for a flush to push, so SMB2's `FLUSH` has nothing to do at that layer.
- **Was not true until milestone 55's durability half**: the block server issued no
  `VIRTIO_BLK_T_FLUSH`, so the durability of the last acknowledged write was the device's word
  rather than ours. A host that lost power could lose a write this server had acknowledged, and
  nothing in the stack was even asking the device about it.

**What closed it**, and it is two new opcodes on two contracts:

| contract | opcode | what it does |
|---|---|---|
| `fs_proto::blk` | `FLUSH` (4) | the block server issues `VIRTIO_BLK_T_FLUSH` and waits for the device's completion. `EOPNOTSUPP` if the device never offered `VIRTIO_BLK_F_FLUSH`, so a device with no flush is a loud refusal rather than a quiet success |
| `fs_proto::fs` | `SYNC` (19) | the file-service verb behind SMB2's `FLUSH`. Any handle the server minted, `dir::WRITE` required, refused with `EROFS` |

Both answer with a **count of completed device flushes** rather than a zero, which is what makes
the gate falsifiable: two syncs that return the same number mean the second never reached the
device. See `fs_proto::fs::SYNC` for the full argument, including why the rights are write-side and
why the `EOPNOTSUPP` travels to the client unmapped.

The honest sentence now: **after a successful `FLUSH`, every write this server acknowledged is on
the medium the device calls durable.** What "durable" means is still the device's definition, and a
device that lies about its own flush is outside anything a protocol can check.

## How it is tested

1. **Host tests** (`cargo test -p smb_proto`): the state machine driven through a full client
   session, the compound path, the read-only refusals with their statuses, the listing walk,
   SPNEGO round trips, and the transport framing.

   **Identity's nine are in there too**, against a `#[cfg(test)]` authenticator that holds the
   password (which is the *credential service's* position, and the one the SMB server is never in).
   Two of them are the ones that would go green on a decorative implementation and so are worth
   naming: an anonymous AUTHENTICATE must be refused by a share with an authenticator, and a refused
   session must leave the share *unreachable* rather than merely answer a status. The others pin the
   guest label being clear on a proven session and set on an unproven one, a retry succeeding on the
   same connection after a refusal, a captured proof failing against the next connection's challenge,
   a proof derived over a different domain failing, and `LOGOFF` forgetting that anybody was named.
2. **The QEMU gate**, both ISAs, and it now proves bytes crossing in **both** directions with a
   different process witnessing each. The read leg asserts a file `fs_test_client`'s seed role
   put on the filesystem; the write leg has xtask's prober create a file over SMB2, write it in
   two chunks at two offsets plus a tail, cut the tail off with `SET_INFO`, stamp its timestamps
   and close, and then **deliberately not read it back**. A second in-guest process
   (`fs_test_client`'s verify role, holding a directory capability and nothing that names the
   network) reads it through the FS server after the adapter has stopped serving, and reports a
   classification: exact, absent, wrong size (the truncate leg), or wrong bytes (an offset or
   chunking bug). A prober that read back its own write would prove only that the adapter
   remembers, which an adapter can do with no filesystem under it at all.

   **The subdirectory leg is the same discipline one level up** and is the one check the prober
   genuinely cannot make for itself. It makes a directory over the wire with `FILE_DIRECTORY_FILE`,
   writes a file inside it by its full path, and lists the directory back (a listing is a fact about
   the server's own view, so a leaf shown as a full path is a bug this side *can* see). What it
   cannot see is whether a **directory** reached RedoxFS: a share that ignored the separator would
   create a file literally called `tm_bands\band0` in the share root, and that is indistinguishable
   from success on the wire. The verify role descends with `fs::OPENDIR` and reports
   `DIR_IS_A_FILE` when the answer is `ENOTDIR`, which is exactly that failure named.

   The prober also asserts `FileFsFullSizeInformation` is **not** the nominal constant, which is the
   `STATFS` half arriving where Time Machine will read it.

   **The Apple leg rides the first CREATE**, because that is where a Mac puts it: the prober's open
   of the seeded file carries the `AAPL` context, and the same response that has to report the
   file's real size has to carry the answering context. What that proves over a host test is the
   whole adapter: the context had to be chunked out through the socket contract, reassembled by a
   real TCP stack, and arrive with its `CreateContextsOffset` still measured from the right place.
   The prober names each claim separately, so a bit that goes missing says which one it was rather
   than "the bytes differ".

   **The identity leg is three AUTHENTICATE messages down one connection**, in the order that makes
   each one mean something: an anonymous login (which is what this prober itself sent until identity
   landed, and what the guest used to admit) refused, a real proof with one bit flipped refused, and
   the real thing accepted and **not** flagged guest. After each refusal it tries a `TREE_CONNECT`
   and requires `STATUS_USER_SESSION_DELETED`, because a refusal that only changes a status word is
   not a gate.

   What that arrangement proves, and no unit test could: **the password exists only on the host.**
   Inside the guest it exists only as an `NTOWFv2` inside a sealed credential store held by a process
   with no network; the adapter that answers the exchange holds one endpoint to that store and cannot
   compute any of the three proofs; the bytes it then serves come from a third process that holds no
   network either. Four processes, four authorities, one `ls`. And the kernel closes it from the
   outside: `assert_smb_held_no_key` reads the frame the adapter and the store share **through the
   direct map**, which no userspace program could do, and requires the published `NTOWFv2`, the
   published `SessionBaseKey`, and every other nonzero byte to be absent. That is the check the
   adapter could not make about itself, and it is what turns milestone 65's
   `an_smb_server_authenticates_a_session_without_ever_holding_the_key` from a claim about a
   stand-in into a claim about the real SMB server.

   The adapter rides the milestone-107 inbound test's spawn
   (`a_host_process_connects_to_the_guest_and_is_answered`) as a **second client of the same
   `Stack` endpoint**, because a second `net_stack` does not fit the test boot (its 192-page
   region is never reclaimed; see `virtio::MAX_DEVICES` for the recorded failure). The test
   wires the FS service, seeds the gate's file through it, grants the adapter the directory
   capability, **and hands it the credential service's verify endpoint** (the same sealed store the
   milestone-56 tests use, latched once per boot); the runner adds a second `hostfwd`
   (`NIFE_SMB_HOSTFWD_PORT`) and xtask's SMB prober performs the mount-shaped exchange end to end
   (asserting the seeded file's bytes) while the echo prober runs beside it. Both verdicts gate.
   This is the first boot that holds the block server, the FS server, `net_stack`, the SMB adapter
   **and the credentialer** at once, so the test prints the free-frame count where it wires them;
   the day the budget stops fitting, the number is already in the transcript.

## EXAMPLES

Run the gate the way CI does:

```sh
script/test               # both ISAs; the smb check reports beside the inbound check
```

Serve the share to a real Mac (see BUGS for what to expect):

```sh
cargo xtask smb-serve     # boots the kernel under QEMU, SMB forwarded to 127.0.0.1:10445
```

Then, on the Mac (which can be the same machine):

- Finder, Go > Connect to Server (Cmd-K), server address `smb://127.0.0.1:10445/share`, and
  choose **Guest** when asked how to connect; or
- `mkdir /tmp/nife-share && mount_smbfs -N //GUEST@127.0.0.1:10445/share /tmp/nife-share`

The share is the RedoxFS image (`smb-serve` builds a fresh one) and it is **read-write**, which
the boot's banner says too: `cat /tmp/nife-share/motd` and you are reading bytes that came off a
real (virtual) block device, through the block server, the FS server, and the SMB adapter, over
this kernel's own TCP stack; `echo hello > /tmp/nife-share/scratch` and they go back the same
way. Every session is admitted as guest, so anything that can reach the forwarded port can
change the image; on loopback that is this machine, and on a real network it would be everyone. If the boot printed the
fixture-fallback line instead (no RedoxFS disk), the files are `hello.txt` and `readme.md`,
baked into the adapter. Unmount before stopping QEMU, or Finder will beat against a dead forward
for a while.

**Why this says Guest when the milestone shipped identity.** The demo boot has no way to be told a
password (BUGS, first entry), so it wires the guest share on purpose and its banner says so. The
authenticated share is what both ISAs' gates run, and the way to watch it work is the gate's own
transcript:

```sh
script/test 2>&1 | grep -A3 'smb check'
```

which reports the anonymous login refused, the one-bit forgery refused, and a real NTLMv2 proof
accepted. If you want to try an authenticated mount by hand today you would have to boot the gate's
wiring rather than `smb-serve`, and the account would be the published fixture in
`cred_proto::fixture`, which is exactly why the demo does not ship it.

## BUGS

- **`mount_smbfs` has ruled; Finder's dialog has not.** The command-line mount (which uses the
  same smbfs kext Finder does) works end to end, but nobody has yet clicked through Connect to
  Server and browsed the share in a Finder window; expect that to exercise `CHANGE_NOTIFY`
  (answered `STATUS_NOT_SUPPORTED`; clients degrade to polling) and possibly more `QUERY_INFO`
  classes. Non-guest accounts are untested and would meet signing expectations; connect as Guest.
- **Guest means everyone.** Every AUTHENTICATE is accepted. Do not put anything on the share the
  local network may not read. There is also no rate limiting and no credit accounting.
- **No Mac has seen the `AAPL` answer.** The context is gated by host tests and by the QEMU prober,
  and the prober is a client this tree wrote against the same constants the server answers with, so
  it agrees by construction. Whether macOS's `smbfs` accepts these bytes, and whether the Time
  Machine UI then offers the share, is unproven and needs the kernel on hardware on the family
  network (the discovery half needs that too; slirp carries no multicast).
- **`FULL_SYNC`'s durability is now backed, and here is what it still is not.** The device is
  flushed (see the Apple section), so the entry that used to sit here is closed. What remains:
  **the sync is device-wide, never per file.** A client that flushes one handle makes the whole
  image durable, which is more work than it asked for and is the only thing anything below here
  can do. And **nothing fences**: there is no ordering primitive on `fs_proto`, so a client issuing
  a write and a flush concurrently gets no guarantee between them. A backup client's own sequence
  is write-then-flush, which is why this has not needed one.
- **Apple metadata is not implemented at all.** No alternate data streams, so no `AFP_AfpInfo` and
  no `AFP_Resource`: Finder labels, resource forks and the extended-listing capability
  (`READ_DIR_ATTR`, deliberately not claimed) all rest on that surface. The layer under them is not
  missing: milestone 57 added the four extended-attribute verbs to `fs_proto`, ops 14-17. What does
  not exist is the **SMB** half, which is a stream name in a CREATE path, `FileStreamInformation`
  in `QUERY_INFO`, and
  `FILE_NAMED_STREAMS` in the volume attributes. The stream-versus-sidecar decision milestone 55's
  block frames is therefore still open, and it is now a smaller question than that block assumed:
  the layer that was missing when it was written is not missing any more.
- **`ReplaceIfExists = 0` is ignored: a rename always replaces.** `FileRenameInformation`'s first
  byte says whether the client will accept clobbering the destination, and this server does not
  read it, because `fs_proto::fs::RENAME` replaces an existing name of the same kind and offers no
  way to say no (its own doc refuses `renameat2`'s `NOREPLACE` on §42 grounds: emulating it with
  link-then-unlink is racy and it is not portable). So a client that asked for a rename to fail on
  a collision gets a silent overwrite. That is the wrong direction to fail in and it is the one
  thing in this area worth fixing next; the fix is a `NOREPLACE` question in `fs_proto`, not
  something this layer can answer.

- **The demo boot still admits guests, so the thing a person actually runs is still open to
  everyone who can reach the port.** `--features smb_serve` wires `SHARE_FS_READ_WRITE`, not
  `SHARE_FS_AUTHENTICATED`, and its banner says so. The reason is not laziness and not a flag: there
  is no way to *tell* that boot a password. The only thing in the tree that provisions the credential
  store is `credentialer_test_client`'s provisioner role, carrying [MS-NLMP] §4.2.1's published
  fixture, and a demo whose password Microsoft printed would be worse than a labelled guest share.
  **What closes this is a provisioning path**, which is milestone 56's territory, and it is the
  entry on the list below.
- **An authenticated share authenticates exactly one account**, because it is configured with one
  resource. Several accounts mean several adapters, one directory capability each. That fits Time
  Machine (one share per Mac) and it is a real limit on anything else.
- **The adapter's resource name is a constant naming a test fixture**, in
  `smb_server::CredentialAuthenticator::resource`. The right fix is not a configuration string, it is
  a **narrower capability**: a request that names its resource is the adapter choosing which record to
  ask about, which is one authority more than it needs, and the endpoint should *be* the credential
  for one resource so the name is implied and unforgeable. That is DECISIONS §27's argument applied
  to `cred_proto`, and it is a change to a contract two programs agree on.
- **Sessions are not signed.** A proven session is unprotected once established, so an attacker on
  the path can inject into it. Nothing here is worse than the guest share it replaces, and the honest
  reading is that identity buys authentication of the *client*, not integrity of the *stream*.
- **The server challenge is the adapter's `now()`**, a clock rather than entropy. Two connections in
  the same tick would repeat a challenge, and a repeated challenge is what makes a captured proof
  replayable. The fix is an entropy capability (milestone 56's service) and one more slot.
- **There is one verify page, so one verify client.** The credential service maps a single frame for
  its client side, so two SMB adapters would interleave requests in one page and read each other's
  answers. The gates are fine (one adapter, a single-threaded runner) and a deployment with two
  shares is not. The fix is a frame per client, in the service's wiring rather than in the contract.
- **No rate limiting and no credit accounting.** Nothing costs an attacker anything to retry, and
  an Argon2id verification is deliberately expensive, so a login flood is a denial of service against
  the credential service that every other client shares.
- **The write path has never met a real Mac.** The 2026-08-15 mount was against a read-only
  share; the write half is gated by host tests and by the QEMU prober, which is a conforming
  client this tree wrote, and a conforming client is not the same thing as `smbfs`. Expect the
  first writable Finder copy to find something, most likely in the `SET_INFO` classes or in a
  `QUERY_INFO` class nothing has asked for yet. This is the same gap the SMB1 probe fell into.
- **A path costs one descent per component, on every call.** `open("a\\b\\c")` is two
  `fs::OPENDIR`s and an `fs::OPEN`, and the two directory handles are opened and closed again for
  the next call. There is no cache, because a cache here would have to be invalidated by every
  other client of the same FS server and this adapter cannot see them. Reads and writes are
  unaffected: they go through the handle CREATE minted.
- **Free space is a forecast, not a reservation.** The numbers are the image's now, but two clients
  writing concurrently both see a count that was true when it was read, and a write past the real
  end still fails with `STATUS_DISK_FULL` at the write. That is what `statfs` is everywhere.
  `STATFS` also answers about the **whole image**, never about a subtree, so a share served over a
  narrow directory capability still reports the volume's free space; there are no quotas in this
  filesystem, so there is no smaller number that would be true.
- **A directory moved into another directory is refused, and the status is unhelpful.**
  `fs_proto::fs::RENAME` answers `EINVAL`, which this share has no word for and reports as
  `STATUS_UNEXPECTED_IO_ERROR`. Renaming a directory in place works, which is the case a client
  performs; moving one between folders in Finder is the failure to expect.
- **The reserved characters SMB forbids in a name are not checked** (`:` `*` `?` `"` `<` `>` `|`).
  A client that creates `a?b` gets a file called `a?b` on the image, which no Windows client can
  open afterwards. A compatibility gap rather than a safety one; nothing macOS sends contains them.
- **Nothing bounds a path's depth**, only its total length (`smb_proto::path::MAX_PATH`, 128 bytes).
  A path of many one-byte components is legal and slow, per the descent cost above.
- **Timestamps are accepted and thrown away.** `SET_INFO`'s `FileBasicInformation` succeeds and
  changes nothing (no clock capability here, and `fs_proto`'s `FSTAT` carries no times), so a
  client that sets a modification time and reads it back gets the epoch. Refusing it instead
  would make every copy report failure, which is worse.
- **`FileAllocationInformation` does nothing**, deliberately: preallocation is a hint, and
  turning it into a truncate would zero-extend a file the client is about to fill.
- **A handle leaks if a connection dies mid-file.** The adapter releases an FS handle at CLOSE or
  when the connection's state machine is dropped; a connection torn down between a CREATE and its
  CLOSE leaves one handle in the FS server's table for the life of that server. Bounded per
  connection by `MAX_HANDLES`, unbounded across them.
- **A listing still costs a walk.** `QUERY_DIRECTORY` re-walks `READDIR` from cursor 0 per entry
  and pays an OPEN + FSTAT + CLOSE to learn each size, because `fs_proto`'s dirent records carry
  name and kind only. Reads and writes no longer pay it: the id is the FS server's handle.
- **`FILE_SUPERSEDE` is `FILE_OVERWRITE_IF` with a different `CreateAction`.** Superseding
  properly replaces a file's identity, attributes and all, and this model has no attributes to
  replace.
- **A single name is capped at 64 bytes and a whole path at 128** (`smb_proto::path`'s
  `MAX_COMPONENT` and `MAX_PATH`), because a handle keeps its own copy of its *path* and the table
  lives on the adapter's stack: `MAX_HANDLES * MAX_PATH` bytes of the `Connection`. Either bound
  exceeded is `STATUS_OBJECT_NAME_INVALID`, said out loud rather than truncated into some other
  file's name.
- **Only lower-case names are reachable over the mount.** The wire folds names to lower-case
  ASCII before lookup and RedoxFS is case-sensitive, so an upper-case name on the image can be
  listed but never opened.
- **All timestamps are zero** (the server holds no clock capability, and fs_proto's FSTAT does
  not carry times), which macOS renders as January 1601 or similar nonsense dates. Cosmetic, and
  honest: nothing here has a date to report.
- **ASCII names only.** A name with any non-ASCII UTF-16 unit is simply not found.
- **A dropped connection costs a 15 s stall** before the listener re-arms (`net_stack`'s bounded
  `RECV` wait). A clean unmount (LOGOFF) costs nothing. One connection is served at a time.
- **The test-boot listener is port 7779, not 445**, because it shares the inbound gate's listen
  grant range and `hostfwd` remaps ports anyway; the serve boot listens on 445 proper.
- `smb-serve` binds `127.0.0.1:10445` fixed, so two serve boots on one machine collide; the test
  boots pick free ports and do not.

## What remains for milestone 54 and beyond, in order

1. ~~The fs_proto-backed share~~ **Done** (2026-08-15): `smb_server::FsShare`, gated on both
   ISAs by the seeded-file exchange above. Milestone 47's rights split (a directory capability
   that may write backups but not delete them) becomes expressible the moment writes exist.
2. ~~The write path~~ **Done** (2026-08-16): `WRITE`, all six create dispositions,
   `SET_INFO`'s end-of-file, rename, disposition and basic classes, delete-on-close, the `Share`
   trait's widening **and** its error channel, and the handle cache (the id is the FS server's
   handle). Gated on both ISAs by a write the guest reads back through the FS server in a
   different process. Milestone 47's rights split is now expressible end to end: a directory
   capability carrying `WRITE | CREATE` and not `REMOVE` gives a share that takes backups and
   destroys nothing, and the FS server enforces it under an adapter that never sees the mask.
3. ~~A `statfs` verb for `fs_proto`~~ **Done** (2026-08-16): `fs::STATFS`, op 18, and the SMB
   volume classes report the image's real numbers through it. The wire decisions are in the
   section above and in pull request #255.
4. ~~Subdirectories~~ **Done** (2026-08-16): `smb_proto::path`, the `Share` seam's directory ids
   and its `mkdir`/`rmdir`/`open_dir` verbs, and the adapter's per-component walk. Gated on both
   ISAs by a directory the host makes over SMB2 and a different in-guest process descends into.
5. ~~`fruit:posix_rename`~~ **Already true, checked 2026-08-17 rather than built.** The two
   behaviours Samba's `fruit:posix_rename` switches on are renaming onto an existing name and
   renaming a file that is open. The first is `fs_proto::fs::RENAME`'s documented semantics
   already ("if the destination name exists it is replaced, provided it is the same kind"). The
   second cannot fail here because this server enforces no share modes at all: `ShareAccess` is
   never consulted, there are no oplocks and no leases, so there is no sharing violation for POSIX
   semantics to be an exception to. Milestone 55's block listed this as work; it is not, and the
   real gap next door is `ReplaceIfExists` in the BUGS section above.

6. **Identity**: the NTLMSSP proof check against milestone 65's `cred` service, so a share can
   be more than guest-readable. The seam is marked in `smb_proto::ntlmssp`. **Writes raised the
   stakes**: guest means everyone, and on a writable share that means everyone may change it.

5. ~~Identity~~ **Done** (2026-08-17): `smb_proto::authenticator`, the AUTHENTICATE parse in
   `smb_proto::ntlmssp`, and `smb_server::CredentialAuthenticator` over milestone 65's verify
   endpoint. Gated on both ISAs by a host process computing a real NTLMv2 proof over the guest's own
   challenge, with the two refusals asserted beside it and the kernel checking the frame afterwards.
   The wire decisions are in the section above and in pull request #274.

## What remains after milestone 54, in order

1. **A provisioning path**, and it is the one that matters, because until it exists the boot a person
   actually runs (`smb-serve`) admits guests to a writable share. Nothing in the tree can tell a
   running system a password: the only provisioner is a test program with a published fixture in it.
   That is milestone 56's shape (design/roadmap/56-secrets-and-entropy.md), and identity landing has
   made it the head of this path rather than a supporting item.
2. **The resource should be implied by the capability, not named in the request.** The adapter is
   configured with a resource name, which is one authority more than it needs; the endpoint should
   *be* the credential for one resource. DECISIONS §27's argument applied to `cred_proto`, and a
   change to a contract two programs agree on, so it is calef's.
3. **Signing**, which is what `SessionBaseKey` is for and the reason the credential service publishes
   one. A proven session is currently unprotected once established.
4. **An entropy capability for the server challenge**, which is `now()` today.
5. **A frame per verify client**, so two shares can be two adapters.
