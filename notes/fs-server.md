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

## The block server, and why it polls

RedoxFS reads a lot at open: it scans a 256-entry header ring for crash consistency
(`redoxfs::HEADER_RING`), so a mount is hundreds of block reads before it serves anything. Two
choices keep that inside the test's watchdog:

1. **A whole filesystem block per virtio request.** The block server's DMA region is two contiguous
   pages: page 0 for the rings, request header and status, page 1 for the 4096-byte data buffer,
   which IS the page shared with the FS server. So one request moves an eight-sector block and the
   device DMAs it straight into the FS server's page, with no per-sector loop and no copy. The
   milestone-9 driver roles still transfer one 512-byte sector at a time; this role does not.
2. **Poll the used ring, do not wait on the interrupt.** QEMU pops descriptors and writes the used
   ring synchronously inside the `QUEUE_NOTIFY` MMIO write (notes/dma.md), so by the time the
   kernel's `NOTIFY` returns the completion is already done. Waiting on the interrupt per read pays
   a full reschedule each time, and hundreds of those overran the watchdog. `complete_blk` polls
   instead; it faults if the device has not completed within a generous bound, which a synchronous
   device never hits. This is a QEMU-tuned choice, recorded honestly: the interrupt-driven path is
   what the milestone-9 roles use, and real asynchronous hardware would want it back.

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

## What is proven, and the open item

**Proven end to end, both ISAs** (the parity gate, DECISIONS §19): a host-made RedoxFS image, a
block server driving it over DMA, an FS server mounting it over blk IPC and serving from its own
heap, and a client opening the shipped `motd` through a granted directory capability and reading it
back byte for byte. This is the whole read path: the engine mounts, the contract holds, the
confinement holds. The sans-IO core is host-tested for both read AND write against a `DiskMemory`
image (`fs-server` lib tests), so the filesystem *logic* is proven on both paths.

**The open item: on-device writes.** The write plumbing is all in place and host-proven (the
`Server::write` lib test round-trips a write through a full close and reopen), but the on-device
end-to-end write currently **loops inside RedoxFS's allocator commit on bare metal**, even on a
pristine, freshly-created image. The symptom, from instrumenting the kernel's virtio submit path:
the FS server issues one write, then spins re-reading the same handful of allocator blocks (the
`prev`-chain walk in `Transaction::sync_allocator`, `vendor/redoxfs/src/transaction.rs`), issuing no
further writes, until the watchdog fires. The reads return correct data (the on-disk image stays
intact and the host tool reads it after the run), so it is not disk corruption and not a blk-IPC
stall; the identical `write_node`+commit runs cleanly on the host with `DiskMemory` and the std
allocator. The difference is the cricker runtime, `IpcDisk` and the untyped-backed `GlobalAlloc`,
against the vendored engine's no_std write path, which the 0.9.1 audit compiled but never ran a
write through on bare metal. This is the milestone's remaining work: a design/redoxfs-internals
investigation (heap-interaction or a no_std write bug worth reporting upstream), raised rather than
papered over. The client's green test is read-only by consequence, and says so in its source.

## For later milestones

- **31 (capability shell)** hands out endpoints bound to specific directories or files as grant
  expressions; the server's bound-directory and handle-table seams are already the shape it needs.
- **27 (`std::fs`)** binds its `Unsupported` `fs` paths to this contract, phase two of that
  milestone, not this one.
- **23 (live replacement)** gets its hardest state-handoff case here: an FS server with open handles
  and in-flight writes is the "serialise-old / absorb-new" problem the console swap never had.
