# SMB: the network file service a Mac can mount (milestone 54)

The head of the customer path. macOS speaks SMB natively and the Time Machine target (milestone
55) requires it, so SMB is the one network file protocol this tree carries; the roadmap block
records why NFS and 9P were refused. What milestone 54 builds is the **adapter**: a program
holding one network endpoint and one share, translating SMB2 on the wire into the share seam on
the other side, with no storage authority of its own.

## The pieces

| Piece | Where | What it is |
|---|---|---|
| `smb_proto` | `crates/smb_proto/` | The whole wire format: framing, header, every command, NTLMSSP, minimal SPNEGO, and the per-connection state machine. Pure logic over byte slices, host-tested, `no_std`. Client-side builders live in the same crate so tests and the prober share every offset with the server. |
| `smb_server` | `user/src/smb_server.rs` | The adapter program: listen/accept through the socket contract (milestone 107), reassemble direct-TCP framing from bounded `RECV` chunks, hand messages to the state machine, chunk the answers back out. |
| The SMB prober | `xtask/src/main.rs` | The host side of the QEMU gate: a real SMB2 client that negotiates, sets up a guest session, connects the share, opens the fixture file and asserts its bytes, twice over two connections. |

The share behind the adapter is the `Share` trait in `crates/smb_proto/src/share.rs`. Today its
one implementation is `FIXTURE`, files baked into the binary, which is what lets the whole
protocol path run and gate with no FS service in the boot. The fs_proto-backed share, the program
holding a real directory capability into the FS server, is **the milestone's remaining piece**:
it implements the same trait inside `smb_server`, where the IPC lives, and no protocol code
moves.

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
- **One share, named `share`**, read-only, flat (no subdirectories yet).
- Compounds (macOS stats files as CREATE + QUERY_INFO + CLOSE related chains) are implemented;
  credits are granted as asked and never accounted.

## How it is tested

1. **Host tests** (`cargo test -p smb_proto`): the state machine driven through a full client
   session, the compound path, the read-only refusals with their statuses, the listing walk,
   SPNEGO round trips, and the transport framing.
2. **The QEMU gate**, both ISAs: the SMB adapter rides the milestone-107 inbound test's spawn
   (`a_host_process_connects_to_the_guest_and_is_answered`) as a **second client of the same
   `Stack` endpoint**, because a second `net_stack` does not fit the test boot (its 192-page
   region is never reclaimed; see `virtio::MAX_DEVICES` for the recorded failure). The runner
   adds a second `hostfwd` (`NIFE_SMB_HOSTFWD_PORT`) and xtask's SMB prober performs the
   mount-shaped exchange end to end while the echo prober runs beside it. Both verdicts gate.

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

`hello.txt` and `readme.md` are the fixture; `cat` the first and you are reading bytes served by
this kernel's userspace over its own TCP stack. Unmount before stopping QEMU, or Finder will beat
against a dead forward for a while.

## BUGS

- **A real Finder mount is the finish line and has not been performed yet.** The wire is proven
  end to end against this tree's own client (which shares no code path with macOS's) and against
  the specification documents, but macOS's smbfs is the judge that matters and it has not ruled.
  The most likely friction points, in order: a `QUERY_INFO` class it wants and we refuse, the
  absent `CHANGE_NOTIFY` (answered `STATUS_NOT_SUPPORTED`; clients are supposed to degrade to
  polling), and signing expectations on non-guest accounts (use Guest).
- **Guest means everyone.** Every AUTHENTICATE is accepted. Do not put anything on the share the
  local network may not read. There is also no rate limiting and no credit accounting.
- **Read-only, flat, fixture-backed.** Writes, creates and `SET_INFO` return
  `STATUS_ACCESS_DENIED`; there are no subdirectories; the files are baked into the binary. The
  fs_proto share backend is the next piece and lands behind the `Share` trait.
- **All timestamps are zero** (the server holds no clock capability), which macOS renders as
  January 1601 or similar nonsense dates. Cosmetic, and honest: the fixture has no dates.
- **ASCII names only.** A name with any non-ASCII UTF-16 unit is simply not found.
- **A dropped connection costs a 15 s stall** before the listener re-arms (`net_stack`'s bounded
  `RECV` wait). A clean unmount (LOGOFF) costs nothing. One connection is served at a time.
- **The test-boot listener is port 7779, not 445**, because it shares the inbound gate's listen
  grant range and `hostfwd` remaps ports anyway; the serve boot listens on 445 proper.
- `smb-serve` binds `127.0.0.1:10445` fixed, so two serve boots on one machine collide; the test
  boots pick free ports and do not.

## What remains for milestone 54, in order

1. **A real macOS mount**, which is the milestone's own finish line. Run the EXAMPLES above from
   a Mac and fix what smbfs objects to; expect a lane of protocol-detail work, not a redesign.
2. **The fs_proto-backed share**: `smb_server` holding a directory capability into the FS
   server, implementing `Share` over `fs_proto` calls (open/read/enumerate; milestone 47's
   rights split expresses "may write backups but not delete them" when writes come). This is
   what makes the adapter the roadmap's adapter rather than a demo of one.
3. **The write path** (milestone 55 needs it): `WRITE`, create dispositions, `SET_INFO`, and the
   share trait's widening.
4. **Identity**: the NTLMSSP proof check against milestone 65's `cred` service, so a share can
   be more than guest-readable. The seam is marked in `smb_proto::ntlmssp`.
