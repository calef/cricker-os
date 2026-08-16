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

## The pieces

| Piece | Where | What it is |
|---|---|---|
| `smb_proto` | `crates/smb_proto/` | The whole wire format: framing, header, every command (both directions since 2026-08-16), NTLMSSP, minimal SPNEGO, and the per-connection state machine. Pure logic over byte slices, host-tested, `no_std`. Client-side builders live in the same crate so tests and the prober share every offset with the server. |
| `smb_server` | `user/src/smb_server.rs` | The adapter program: listen/accept through the socket contract (milestone 107), reassemble direct-TCP framing from bounded `RECV` chunks, hand messages to the state machine, chunk the answers back out. |
| The SMB prober | `xtask/src/main.rs` | The host side of the QEMU gate: a real SMB2 client that negotiates, sets up a guest session, connects the share, opens the seeded file and asserts its bytes, then writes a second file it never reads back, twice over two connections. |

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
- **Guest sessions, NTLMSSP-shaped.** The server answers the NTLMSSP dance (raw or wrapped in
  SPNEGO, which is how macOS sends it) so a conforming client can finish it, then admits everyone
  as guest and says so (`SESSION_FLAG_IS_GUEST`). Nothing is verified, no secret is stored
  anywhere (DECISIONS §79's constraint), and no session is signed. Identity later means wiring
  the proof check to milestone 65's `cred`/`ntlm` machinery at the one marked point in
  `smb_proto::ntlmssp`.
- **`MaxTransactSize`/`MaxReadSize`/`MaxWriteSize` = 65536**, the floor mainstream clients are
  written against, and exactly the static buffer the allocator-less server carries.
- **One share, named `share`**, flat (no subdirectories), **writable when the boot says so.**
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
- **Free space is nominal** (`smb_proto::share::NOMINAL_VOLUME_BYTES`, 64 MiB). `fs_proto` has no
  `statfs` verb, so nothing between the adapter and RedoxFS knows the image's size. A read-only
  share reports zero free, which is what makes macOS refuse a write client-side.
- Compounds (macOS stats files as CREATE + QUERY_INFO + CLOSE related chains) are implemented;
  credits are granted as asked and never accounted.
- **The SMB1 probe.** The machine overruled the assumption that a modern client opens with SMB2:
  macOS's `mount_smbfs` still opens with an **SMB1** multi-protocol NEGOTIATE (`\xFFSMB`,
  command `0x72`, dialect strings `NT LM 0.12`, `SMB 2.002`, `SMB 2.???`), and the first cut of
  this server dropped it as not-SMB2, which presented as every real mount timing out while the
  test suite stayed green (the suite's prober politely opened with SMB2). The fix is [MS-SMB2]
  §3.3.5.3.1: answer the probe with an SMB2 NEGOTIATE response carrying the wildcard revision
  `0x02FF`, after which the client negotiates properly. The captured bytes are pinned as a host
  test in `smb_proto::server`, so the message a real client actually sends is now part of the
  gate. An SMB1-only client (no SMB2 dialect strings) is still dropped.

## How it is tested

1. **Host tests** (`cargo test -p smb_proto`): the state machine driven through a full client
   session, the compound path, the read-only refusals with their statuses, the listing walk,
   SPNEGO round trips, and the transport framing.
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

   The adapter rides the milestone-107 inbound test's spawn
   (`a_host_process_connects_to_the_guest_and_is_answered`) as a **second client of the same
   `Stack` endpoint**, because a second `net_stack` does not fit the test boot (its 192-page
   region is never reclaimed; see `virtio::MAX_DEVICES` for the recorded failure). The test
   wires the FS service, seeds the gate's file through it, and grants the adapter the directory
   capability; the runner adds a second `hostfwd` (`NIFE_SMB_HOSTFWD_PORT`) and xtask's SMB
   prober performs the mount-shaped exchange end to end (asserting the seeded file's bytes)
   while the echo prober runs beside it. Both verdicts gate. This is the first boot that holds
   the block server, the FS server, `net_stack` and the SMB adapter at once, so the test prints
   the free-frame count where it wires them; the day the budget stops fitting, the number is
   already in the transcript.

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

## BUGS

- **`mount_smbfs` has ruled; Finder's dialog has not.** The command-line mount (which uses the
  same smbfs kext Finder does) works end to end, but nobody has yet clicked through Connect to
  Server and browsed the share in a Finder window; expect that to exercise `CHANGE_NOTIFY`
  (answered `STATUS_NOT_SUPPORTED`; clients degrade to polling) and possibly more `QUERY_INFO`
  classes. Non-guest accounts are untested and would meet signing expectations; connect as Guest.
- **Guest means everyone.** Every AUTHENTICATE is accepted. Do not put anything on the share the
  local network may not read. There is also no rate limiting and no credit accounting.
- **The write path has never met a real Mac.** The 2026-08-15 mount was against a read-only
  share; the write half is gated by host tests and by the QEMU prober, which is a conforming
  client this tree wrote, and a conforming client is not the same thing as `smbfs`. Expect the
  first writable Finder copy to find something, most likely in the `SET_INFO` classes or in a
  `QUERY_INFO` class nothing has asked for yet. This is the same gap the SMB1 probe fell into.
- **Flat, and writing cannot change that.** The share model has no subdirectory nodes, so a
  directory on the image shows in listings with the directory attribute but answers NOT_FOUND
  when opened, and no disposition creates one: `MKDIR` is a verb this share does not offer. The
  doc tree's directories are the visible case, and a client trying to save into a folder it can
  see is the failure to expect.
- **Free space is a constant.** `FileFsSizeInformation` reports
  `smb_proto::share::NOMINAL_VOLUME_BYTES` (64 MiB) because `fs_proto` has no `statfs` verb, so a
  client that sizes its work against the answer is sizing it against a number nothing measured. A
  write past the real end of the image fails with `STATUS_DISK_FULL` at the write rather than
  being predicted. **Time Machine will care**: macOS sizes a sparsebundle against reported free
  space, so a `statfs` verb is on milestone 55's path rather than beside it.
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
- **Names are capped at 64 bytes** (`smb_proto::server::MAX_NAME`), because a handle keeps its own
  copy of its name and the table lives on the adapter's small stack. A longer name is
  `STATUS_OBJECT_NAME_INVALID`, said out loud rather than truncated into some other file's name.
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
3. **A `statfs` verb for `fs_proto`**, which the write path found and did not take: free space is
   a constant today (BUGS above), and Time Machine sizes its sparsebundle against what the volume
   reports. A new verb is a contract change and is the integrator's to mint, not a lane's.
4. **Subdirectories**, which Time Machine also needs: a sparsebundle is a *directory* of band
   files. The share model is flat and `MKDIR` is unoffered, so this is the largest single piece
   between here and milestone 55.
5. **Identity**: the NTLMSSP proof check against milestone 65's `cred` service, so a share can
   be more than guest-readable. The seam is marked in `smb_proto::ntlmssp`. **Writes raised the
   stakes**: guest means everyone, and on a writable share that means everyone may change it.
