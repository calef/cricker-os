# The RedoxFS filesystem server (milestone 32 phase 2)

A real copy-on-write filesystem we did not write, RedoxFS, running confined as a userspace
component and served over a capability-shaped contract. This is the flagship userspace-reuse
story the prior-art survey predicted (notes/prior-art.md, notes/redoxfs-audit.md): the kernel
confines a serious component it knows nothing about, and the thing milestone 31's per-file grants
will point at.

The written contract lives with its code in `crates/fs_proto`, the way the terminal contract lives
in `linedisc::proto` (notes/terminal-contract.md). This note is the design around it.

## Three processes, two protocols

```text
  disk ──virtio──►┌──────────────┐──blk IPC──►┌───────────┐──file IPC──► client
                  │ block server │            │ FS server │
                  └──────────────┘◄───────────└───────────┘◄──────────── (holds a directory cap)
                       owns the DMA             owns RedoxFS +
                       confinement              its own heap
```

Nobody names anyone else (endpoint-only naming, notes/ipc-naming.md). The FS server holds "an
endpoint I read and write blocks on"; the client holds "an endpoint that opens and reads files
under the one directory this endpoint is bound to." Rewire the endpoints and neither side can tell,
which is milestone 23's hot-swap claim in component form.

- **The block server** is a role of the virtio driver (`user/src/virtio.rs::run_blk_server`). It
  brings the RedoxFS disk up, then serves read/write/size over **blk IPC** forever. The device
  confinement is unchanged from any driver (the kernel owns the transport and validates every DMA
  descriptor, notes/dma.md), so a serving block driver is as confined as a reading one.
- **The FS server** (`fs-server/`, its own workspace because it links the vendored engine) runs the
  no_std RedoxFS core behind a `Disk` trait implemented over blk IPC, allocating everything from its
  own untyped budget through the milestone-27 `GlobalAlloc`. It serves **file IPC** to clients.
- **The client** (`user/src/fsclient.rs`) is the program a milestone-31 shell will be: it holds only
  a directory capability and opens files by name under it.

The kernel wires all three (`kernel/src/user.rs::fs_service`), handing each a `Spawn` literal that
is its entire authority. The kernel never sees a filesystem operation, an opcode, or a byte of file
data.

## The contract is capability-shaped from birth

- **The endpoint IS the directory capability.** A client reaches the FS server only by holding an
  endpoint the server reads on, and that endpoint is bound, in the server, to one directory node
  (the image root, in phase 2). Every name in an `OPEN` is resolved *under that bound directory*
  (`Server::open_file`): no absolute path, no `..` escape, no global namespace. A client with no
  such endpoint can open nothing, and the refusal is "you hold no such capability", not a
  permission check. Milestone 31 binds new endpoints to subdirectories or single files and hands
  them out as grant expressions; the server code already carries the bound-directory seam.
- **A handle is a server-minted token.** A successful `OPEN` returns a small integer the server
  issued and validates against this session's table (`Server::node`). Forging one is meaningless:
  the server honors only the handles it minted, in exactly one place.
- **Open-by-path exists only inside the server.** The client sends a name; the server resolves it.
  There is no path walking on the wire and no way to designate a file the granted directory does not
  contain.

## The error boundary, mapped exactly once

RedoxFS speaks its own error type everywhere (`syscall::error::Error`, redox_syscall's errno). The
sans-IO core (`fs_server::Server`) and the `Disk` impl both return `syscall::error::Result`,
unmapped. The translation to the wire happens in **one place**, the FS server's serve loop
(`fsserver.rs::serve`), via `fs_proto::reply_err`, which is just the negated errno. The client
inverts it with `fs_proto::reply_errno`. Keeping the core in RedoxFS's vocabulary is what makes the
rule enforceable: there is no ABI type below the boundary to leak. The blk IPC has the same
convention (a negative reply is a negated errno), and the `Disk` impl maps a negative blk reply to
`Error::new(EIO)`, which is the trait's own vocabulary, not an ABI leak.

## The block server, and how it completes a request

RedoxFS reads a lot at open: it scans a 256-entry header ring for crash consistency
(`redoxfs::HEADER_RING`), so a mount is hundreds of block reads before it serves anything. Two
choices keep that inside the test's watchdog:

1. **A whole filesystem block per virtio request.** The block server's DMA region is two contiguous
   pages: page 0 for the rings, request header and status, page 1 for the 4096-byte data buffer,
   which IS the page shared with the FS server. So one request moves an eight-sector block and the
   device DMAs it straight into the FS server's page, with no per-sector loop and no copy. The
   milestone-9 driver roles still transfer one 512-byte sector at a time; this role does not. This
   is what keeps the mount's read count in the low hundreds rather than thousands.
2. **Wait on the completion interrupt, the milestone-9 discipline** (`complete_blk`,
   `user/src/virtio.rs`). The kernel turns the device's completion IRQ into a message on the block
   server's `Irq` endpoint; the server WAITs for it, quiets the device, ACKs the line, and lets
   `used.idx` decide when the completion is really its own (a wakeup can be stale or coalesced), the
   same loop the read driver uses. This is a **correction** (fix/irq-delivery, 2026-07-29): the
   earlier note here claimed interrupt-driven completion "overran the watchdog" and forced a poll of
   the used ring. It does not. Booted with the WAIT path, the fs-server test passes on both ISAs at
   the 4-core SMP boot (aarch64 141 tests, riscv64 84 tests), the mount's hundreds of WAIT-driven
   completions all landing well inside the 60 s watchdog. The completion IRQ reaches the block
   server's endpoint exactly as it reaches any milestone-9 driver's; the whole-block reads above are
   what keep the reschedule count affordable. The prior "hangs on the first read" report did not
   reproduce; the machine overruled the note. QEMU still completes synchronously inside the
   `QUEUE_NOTIFY` write (notes/dma.md), so the interrupt is already pending by the time the server
   WAITs, and the kernel's pending-signal count (DECISIONS §9a) makes that WAIT return at once rather
   than block on an event already over.

The disk order matters and is a real hazard. QEMU's `virt` assigns virtio-mmio devices to slots in
**reverse** command-line order, and the kernel finds block devices by ascending slot. So the runners
place the crickerfs disk LAST on the command line to keep it at slot 0 (the phase-1 driver tests use
`find_block_device`), which leaves the RedoxFS disk at the next slot for `find_block_device_n(1)`.
Getting this backwards silently hands the phase-1 tests the wrong disk; the runner comments say so.

## Never create on-device

The std-gated core APIs are exactly creation (`FileSystem::create`, uuid v4, getrandom). The FS
server only ever OPENS an image; entropy never becomes a userspace dependency. Test images are made
host-side by `tools/redoxfs-host` with the same pinned engine that serves them (roadmap §32 port
plan item 4). The host tool's `mkfs` was also fixed to start from an empty file: `DiskFile::create`
opens without truncating, so `mkfs` over an existing image left stale blocks past the new write and
produced an image that failed to open. Removing the file first makes it idempotent, which the test
flow relies on (it regenerates the image every run).

## What is proven

**Proven end to end, both ISAs** (the parity gate, DECISIONS §19): a host-made RedoxFS image, a
block server driving it over DMA, an FS server mounting it over blk IPC and serving from its own
heap, and a client opening the shipped `motd` through a granted directory capability and reading it
back byte for byte. The engine mounts, the contract holds, the confinement holds. The sans-IO core is
host-tested for both read AND write against a `DiskMemory` image (`fs-server` lib tests), so the
filesystem *logic* is proven on both paths independently of any device.

**The write path is proven on-device too, which is a correction** (2026-07-29, milestone 27 phase
two). This note used to record an open item: the end-to-end write "loops inside RedoxFS's allocator
commit on bare metal even on a pristine image", spinning on the `prev`-chain walk in
`Transaction::sync_allocator` and issuing no further writes until the watchdog fired. It does not.
Driven through `std::fs` (the milestone-27 PAL, `OpenOptions::write(true)` on the image's `scratch`
file), the write completes on both ISAs, reads back through the server, and reads back byte for byte
when the **host tool reopens the image afterwards** with the pinned engine, which is the part a cache
cannot fake. That reopen is in the gate: `redoxfs_check_after_run` compares `scratch` against the
fixture, and `mkredoxfs` rewrites it to a placeholder before every run, so the check passing means
this run's guest write landed on the disk.

The likely reason it works now is the interrupt-delivery fix above: the block server WAITs on the
device's completion interrupt instead of polling the used ring, the same correction this note already
had to make for the read path. That is stated as *likely*, not proven: what was measured is that the
write completes, not why the poll path did not. The milestone-32 client stays read-only by choice
now rather than by blocker, and its source says so; the write is proven through `std::fs`, which
drives the same contract through more layers.

**The remaining gap is in the contract, not the write path.** There is no `CREATE` and no `TRUNCATE`
verb, so `std::fs::write` and `File::create` are honestly `Unsupported` and writing means opening a
file the image already carries. Adding both verbs is possible (`Transaction::create_node` is not
std-gated; "never create on-device" below is about creating a *filesystem*, which needs uuid and
getrandom, not a file), but it widens the contract and belongs to a deliberate decision. See
notes/std.md.

## For later milestones

- **31 (capability shell)** hands out endpoints bound to specific directories or files as grant
  expressions; the server's bound-directory and handle-table seams are already the shape it needs.
- **27 (`std::fs`)** is **done**: the PAL binds `File` to this contract, and the endpoint's bound
  directory becomes the thing `File::open`'s path resolves under, so a path that would leave it is
  refused rather than served. The std program holds the endpoint at slot 4 of the std slot convention
  and nothing else that names a filesystem. See notes/std.md and notes/abi.md §4.
- **23 (live replacement)** gets its hardest state-handoff case here: an FS server with open handles
  and in-flight writes is the "serialise-old / absorb-new" problem the console swap never had.
