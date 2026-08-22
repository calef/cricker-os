# The RedoxFS filesystem server (milestone 32 phase 2)

A real copy-on-write filesystem we did not write, RedoxFS, running confined as a userspace
component and served over a capability-shaped contract. This is the flagship userspace-reuse
story the prior-art survey predicted (notes/prior-art.md, notes/redoxfs-audit.md): the kernel
confines a serious component it knows nothing about, and the thing milestone 31's per-file grants
point at (they now exist; see the caretaker section below).

The written contract lives with its code in `crates/fs_proto`, the way the terminal contract lives
in `line_editor::proto` (notes/terminal-contract.md). This note is the design around it.

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

- **The block server** is a role of the virtio driver (`crates/virtio`'s `run_blk_server`, dispatched by `crates/virtio`). It
  brings the RedoxFS disk up, then serves read/write/size over **blk IPC** forever. The device
  confinement is unchanged from any driver (the kernel owns the transport and validates every DMA
  descriptor, notes/dma.md), so a serving block driver is as confined as a reading one.
- **The FS server** (`fs_server/`, its own workspace because it links the vendored engine) runs the
  no_std RedoxFS core behind a `Disk` trait implemented over blk IPC, allocating everything from its
  own untyped budget through the milestone-27 `GlobalAlloc`. It serves **file IPC** to clients.
- **The client** (`user/src/fs_test_client.rs`) is the program a milestone-31 shell will be: it holds only
  a directory capability and opens files by name under it.

The kernel wires all three (`kernel/src/user/fs_service.rs`), handing each a `Spawn` literal that
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
(`fs_server.rs::serve`), via `fs_proto::reply_err`, which is just the negated errno. The client
inverts it with `fs_proto::reply_errno`. Keeping the core in RedoxFS's vocabulary is what makes the
rule enforceable: there is no ABI type below the boundary to leak. The blk IPC has the same
convention (a negative reply is a negated errno), and the `Disk` impl maps a negative blk reply to
`Error::new(EIO)`, which is the trait's own vocabulary, not an ABI leak.

## The block server, and how it completes a request

RedoxFS reads a lot at open: it scans a 256-entry header ring for crash consistency
(`redoxfs::HEADER_RING`), so a mount is hundreds of block reads before it serves anything. Two
choices keep that inside the test's watchdog:

1. **A whole filesystem block per virtio request, and since milestone 138 step 4, up to sixteen of
   them in one.** The block server's DMA region is `1 + blk::TRANSFER_BLOCKS` contiguous pages:
   page 0 for the rings, request header and status, and the rest for the data buffer, which IS the
   region shared with the FS server (one page, one block, through step 3; sixteen pages, up to
   sixteen blocks, since). So one request moves one or more whole blocks and the device DMAs them
   straight into the FS server's region as a single descriptor, with no per-sector loop and no copy.
   The milestone-9 driver roles still transfer one 512-byte sector at a time; this role does not.
   This is what keeps the mount's read count in the low hundreds rather than thousands.
2. **Wait on the completion interrupt, the milestone-9 discipline** (`complete_blk`,
   `crates/virtio`). The kernel turns the device's completion IRQ into a message on the block
   server's `Irq` endpoint; the server WAITs for it, quiets the device, ACKs the line, and lets
   `used.idx` decide when the completion is really its own (a wakeup can be stale or coalesced), the
   same loop the read driver uses. This is a **correction** (fix/irq-delivery, 2026-07-29): the
   earlier note here claimed interrupt-driven completion "overran the watchdog" and forced a poll of
   the used ring. It does not. Booted with the WAIT path, the fs_server test passes on both ISAs at
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
place the nifefs disk LAST on the command line to keep it at slot 0 (the phase-1 driver tests use
`find_block_device`), which leaves the RedoxFS disk at the next slot for `find_block_device_n(1)`.
Getting this backwards silently hands the phase-1 tests the wrong disk; the runner comments say so.

## The FS server's stack is sized by measurement, because guessing it cost a day

RedoxFS recurses. A single `Transaction::read_block::<TreeList<..>>` activation carries a whole
4096-byte block plus scratch, so **one frame is 8 KiB**, and a tree walk stacks a dozen or more of
them. The FS server therefore gets a deep stack: `run` maps one page at `USER_STACK_VA` and
`fs_service::wire_servers` maps `FS_STACK_PAGES` more directly below it, out of fresh frames, so the
process sees one contiguous run down from `USER_STACK_TOP`.

That number used to be 32 (33 pages, 135,168 bytes), and it was chosen to be comfortably above the
read-and-write path. Adding `CREATE` and `TRUNCATE` (milestone 31 phase 2) took one more level of
tree recursion and it was **528 bytes short**. The FS server ran off the bottom of its stack
mid-request and the kernel killed it, correctly and legibly:

```text
  user thread 8589934629 killed: Data abort from a lower EL
    pc 0x00000000004000b0   far 0x00000000004dfe90   user sp 0x00000000004dfe90   esr 0x92000047
```

`far == sp`, one page below the bottom of the mapping, and `pc` disassembles inside
`read_block::<TreeList<TreeList<TreeList<BlockRaw>>>>` in the middle of its two 4 KiB `sub sp`
instructions. Nothing about that is ambiguous once you look at it. **What was not legible was
anything downstream**, and that is the part worth carrying off:

- The std client sat `Blocked` on a `CALL` that nobody would ever answer, because the endpoint's only
  receiver had just died. Blocking IPC has no "the server is gone" reply, so a dead server is
  indistinguishable, from a client, from a slow one.
- The suite's no-progress heartbeat credits work by *any* running thread, and earlier tests had left
  processes spinning on other cores, so it saw a healthy system for as long as you cared to wait.
- The only instrument that could fire was the per-test wall-clock ceiling. It fired at the budget,
  and **a ceiling failure reports the budget, not the cost**: "std_fs ran 914 s against a 900 s
  budget" was read as evidence of honest slowness and sent an investigation looking for a slow path
  in a test whose server had been dead since second three.

Two things came out of it. The thread dump now prints each thread's **address-space root**
(`sched::dump_threads`), because every user program links at `0x40_0000` and a bare `pc` resolves
plausibly against several binaries at once; threads sharing a root are one process, distinct roots
are distinct processes, and it is what separates a leftover spinner from the process under test. And
the stack size is now a measurement: the kernel fills every FS-server stack page with a poison word
before the process starts, and `fs_service::fs_stack_used` reports the deepest word that no longer
reads as poison. Measured across a mount, reads, writes, a create and two truncates:

| leg | high-water | of grant | headroom | when |
|---|---|---|---|---|
| aarch64 | 135,696 bytes | 397,312 | 66% | 2026-07-30, milestone 31 phase 2 |
| riscv64 | 135,824 bytes | 397,312 | 66% | 2026-07-30, milestone 31 phase 2 |
| aarch64 | 127,408 bytes | 397,312 | 68% | 2026-07-30, milestone 37 |
| riscv64 | 127,536 bytes | 397,312 | 68% | 2026-07-30, milestone 37 |

Both of the first pair were over the old 135,168-byte grant, so both legs were broken; the riscv leg
needs slightly more for the same recursion, which is why the number is measured per ISA rather than
assumed to transfer. `the_fs_servers_stack_still_has_headroom` (both ISAs) prints it every run and
fails under a quarter left, so the next verb that deepens a tree walk fails with a number instead of
a mystery.

**The second pair is 8 KiB lower and the cause is not attributed**, which is recorded rather than
smoothed over because an unexplained move in a safety instrument is worth more attention than a
comfortable one. 8 KiB is exactly one `read_block::<TreeList<..>>` activation, so it reads like one
less level of tree recursion on the deepest path. Milestones 41 through 45 landed between the two
measurements and any of them could have changed codegen; nothing in milestone 37 touches the read
path, and the crash test's own servers run against a shallower image so they can only lower the
maximum by not raising it. The number to trust is whichever the gate last printed, and the assertion
that matters (a quarter of the grant still free) is unaffected either way.

Since milestone 37 the high-water is a **maximum over every FS server a boot starts**, which now
includes the process that mounts a crashed disk. A mount that has to walk back a generation is the
case most likely to recurse further than a clean one, so it is exactly the case this instrument
should be watching, and until now it was not being watched at all.

The grant is 96 extra pages. That is deliberately well above the measurement rather than just above
it: recursion depth here tracks the *tree* depth, which grows with the image, so a size proven on a
16 MiB fixture is not proven on a real disk. 384 KiB of frames once per boot is cheap beside the FS
server's 8 MiB heap budget.

**Still open, and named rather than fixed:** a client of a dead server blocks forever. §26's fault
endpoint is the mechanism that would turn that into a message a supervisor can act on, and wiring the
FS service into a supervision tree is milestone 23's problem, not this one's. Until then, "the server
died" presents to a client as "the server is taking a while", which is the same shape of invisibility
this whole section is about.

## Crash consistency, measured (milestone 37)

This section used to be a sentence in "What is proven" saying RedoxFS is copy-on-write, so crash
consistency is designed in. That was a description of somebody else's design document, and DECISIONS
§34 made it a condition rather than a claim for exactly that reason. It is now a measurement.

**The property, stated so it can fail.** A workload is operations, each acknowledged only after the
engine commits it. Call the filesystem after the first `p` of them `S(p)`. For every point at which
the device could stop, a fresh mount recovers exactly `S(p)` for some `p`; `p` never goes backwards
as the cut advances; and where nothing is lost, `p` is the whole workload. That is prefix
consistency, and "an acknowledged write is wholly present or wholly absent" is a consequence of it.

**How the injector works, and why it is not an approximation.** The seam is `BlockIo`, the trait the
FS server reaches its disk through: on device it is `IpcDisk` calling the block server, on the host a
`Vec`. `fs_server/src/crash.rs` runs the workload **once** against a recorder that applies every
block write and appends it to an ordered log. That log is what the platter was asked to do, so the
disk after a failure at point `i` is the pristine image plus the first `i` entries, optionally with
one of them truncated. For a device that does not reorder, that is not a model of a crash, it *is*
the crash, and it costs a `memcpy` per fault point instead of a re-run of the engine. It also makes
every fault point start from a byte-identical image, which matters here more than anywhere: a crash
harness that leaves state behind between runs produces the exact class of false result the section
above spends 60 lines on.

| injection | fault points | result at the 8 KiB record | result at the 128 KiB record |
|---|---|---|---|
| power cut, at every write | 134 | all prefix-consistent | all prefix-consistent |
| power cut, last write torn, 4 offsets | 536 | all prefix-consistent | all prefix-consistent |
| a lying device (drop or tear one write, keep persisting after) | 268 | 196 recovered, 72 refused, **0 silently wrong** | 185 recovered, 83 refused, **0 silently wrong** |

**Both columns were re-run on 2026-08-18**, when milestone 138 took the record level from 5 to 1,
because a store's safety claim measured at one geometry is not automatically true at another. The
property is unchanged and the fault-point count is identical; what moves is *where* the lying device
is caught, and it moves the harmless way. Eleven cases go from "refused at a read" to "recovered",
which is what a smaller record predicts: a dropped write damages less. **0 silently wrong in both.**

**A correction this re-run forced, and it is not about the record level.** The counts above used to
read 93, 372 and 186, with 112 recovered and 74 refused. Those are milestone 37's, and the workload
has grown since: the same suite at the *old* record level counts 134 fault points today, not 93. The
numbers were stale before this lane touched anything, and nothing was checking them, because they
live in prose and the tests assert the property rather than the count. That is rung four working as
badly as rung four works; the property is gated and the arithmetic describing it is not.

The third row is the honest limit. RedoxFS's `Disk` trait has no flush and no barrier, so ordering is
the device's job, and a device that acknowledges a write it never persists can leave a valid commit
pointing at a block that never landed. What is guaranteed is that this is never *silent*: every
`BlockPtr` carries a seahash of the block it names, checked on every read.

**Half of that gap was ours and is closed** (milestone 55's durability half, 2026-08-18). The
sentence that used to sit here said our block server issued no `VIRTIO_BLK_T_FLUSH`, so on real
hardware the durability of the last acknowledged write was the device's word rather than ours. It
now issues one: `fs_proto::blk::FLUSH` (op 4) is a real device flush the block server waits on, and
`fs_proto::fs::SYNC` (op 19) is the file-service verb that asks for it. A device that never offered
`VIRTIO_BLK_F_FLUSH` gets `EOPNOTSUPP` all the way up rather than a quiet success, which is the part
that matters: the old behaviour's problem was not the missing flush so much as that nothing above it
could tell.

**The half that is still the device's is still the device's**, and it is what the third row
measures: ordering *between* writes, which nothing on this contract expresses. A `SYNC` says
"everything acknowledged so far is durable now"; it does not say "these two writes landed in this
order", and RedoxFS's `Disk` still has no barrier to say it with. A device that lies about its own
flush is likewise outside anything we can check. That is the engine's and the hardware's territory,
not the driver's.

**The controls.** Three, and the strongest needs no tampering at all: the lying-device sweep produces
74 images the filesystem refuses, so the injector is demonstrably destroying things. Then
`only_this_generation` blanks every header slot but one, taking the ring's history away, and **92 of
93 fault points stop mounting**, which isolates the fallback as the mechanism. Then, with no mount at
all, a commit torn at 2048 bytes fails `Header::valid()` while the previous generation's slot stays
valid and stays older.

**A fourth control turned up on its own, and it is the one worth remembering.** The harness's first
version treated any failed `open_file` as "the name is absent". A dropped write to a directory's tree
block makes that lookup answer `EIO`, so nine fault points reported filesystems that never existed,
empty root and all. It looked like a serious RedoxFS bug for about ten minutes and it was a test bug:
`ENOENT` is the only error that means absence, and the engine refusing to guess at a block whose
checksum does not match is the property working. An instrument that can produce a false positive and
did is an instrument connected to something.

**Two mechanisms this note had named wrong.** `cleanup: true` is **not** the header-ring replay. The
ring scan is unconditional in `FileSystem::open`: read all 256 slots, keep the newest whose seahash
checks out, ignore the rest. `cleanup` adds a tidy-up on top (release unused nodes, commit), and the
server passes it because a mount should not leak, not because recovery depends on it. And what the
scan keeps is not "the newest consistent generation" in any sense the engine computes; it is the
newest generation whose *header* still hashes, which suffices only because a commit's blocks are all
written before it.

### The device-level half, and why it has a disk of its own

The host sweep is exhaustive and the device test is one crash, because they answer different
questions. The device test's question is whether the property survives the real stack: a real virtio
write torn in half, a real FS-server process dying inside its own transaction, and a real second
process recovering the disk it left behind.

The injector lives at `IpcDisk` and is armed by the `Spawn` literal (`arg0` = which `WRITE` request to
die in, `arg1` = block writes to allow first, `arg2` = bytes of the last one that reach the platter).
The Spawn literal rather than a build flag, because the thing that crashes has to be the FS server
the gate otherwise runs and not a lookalike; `arg0 == 0` makes it inert, which is every boot but this
one. The tear is a read-modify-write with half the new contents laid over the old, which is what a
drive leaves when the rail collapses mid-block. `arg1` is **one**, because one is the count that
cannot miss: a write transaction always issues at least one block write, and a larger count is a
server that never dies and a test that hangs.

The recovery is a second FS-server process on the same block server and the same block page, with its
own file endpoint and its own stack. It carries nothing from the process that died. That it can do
this at all is endpoint-only naming doing its job: **the block server never learns its client died and
was replaced, because it never knew who its client was.** Its readiness sentinel is the consistency
result, because `Server::open` refuses an image it cannot make sense of.

**The disk is dedicated and regenerated every run.** This test deliberately leaves a filesystem
half-written; doing that to the shared fixture would make every other FS test's result depend on
whether this one ran first, which is the order-coupled gate this note already spends a section on.
`NIFE_KEEP_REDOXFS` deliberately does not apply to it: the cross-boot case is interesting for the
shared disk and is nothing but noise for this one.

**The assertion is the property, not an outcome.** Either payload passes; what fails is a mixture, a
length nobody wrote, or the pre-boot contents, which would mean an acknowledged write had vanished.
Pinning it to "payload A" would be pinning a detail of when RedoxFS happens to write its commit, and
the claim is not about that. Both legs currently report A, 66 bytes, whole, and the gate re-reads the
image afterwards with the host tool and the pinned engine, which is the half a cache cannot fake.

Two ceilings moved for it, and both are receipts rather than tuning. `MAX_DEVICES` went to 27 for the
second block server, the fourth such bump and another argument for the missing unregister. And the
crash servers' untyped budget is 2 MiB rather than 8: an untyped is **reserved**, not merely capped,
and three 8 MiB reservations do not fit in this machine's 128 MiB. The first symptom of that was
`init` failing to get its own budget several tests later, which is a long way from the cause.

## Never create on-device

The std-gated core APIs are exactly creation (`FileSystem::create`, uuid v4, getrandom). **The FS
server** only ever OPENS an image; entropy never becomes a *server* dependency. Test images are made
host-side by `tools/redoxfs_host` with the same pinned engine that serves them (roadmap §32 port
plan item 4). The host tool's `mkfs` was also fixed to start from an empty file: `DiskFile::create`
opens without truncating, so `mkfs` over an existing image left stale blocks past the new write and
produced an image that failed to open. Removing the file first makes it idempotent, which the test
flow relies on (it regenerates the image every run).

## `mkfs` on the target: `mkfs`, and the divergence it needed (milestone 57)

**The heading above still holds and is now narrower than it reads.** The *server* never creates, and
that is a property of the server rather than of the system: since 2026-08-03 a different program in
this same package does create one, on the target, over the same blk wire.

### The blocker was entropy, not `std`, and the first fix for it did not work

`FileSystem::create` and `create_reserved` carry `#[cfg(feature = "std")]`, and un-gating them is
mechanical for every call but one: `Header::new` stamps a v4 UUID through `Uuid::new_v4`, which is
`getrandom`. So the blocker is that **a filesystem needs a unique identifier and a `no_std` engine
has no randomness**, which is the same wall `crates/gpt` hits one crate over (notes/gpt.md refuses
to invent a partition GUID for the identical reason).

The first divergence taken for it, on the morning of 2026-08-03, made `Header::update_hash` public
on the reasoning that every `Header` field is already `pub`, so a `no_std` caller could **build** a
header and then **finish** it. The first half is true. The second does not follow, and the reason is
worth keeping because it is a general one about vendored code:

> Making a filesystem is not making a header. `create_reserved` lays down the tree list, the
> allocation list and the root node through `Transaction::write_block` and
> `FileSystem::reset_allocator`, both **private**. `Transaction::new` is `pub(crate)`, `sync_block`
> takes an `AllocCtx` the crate does not export, and three of `FileSystem`'s fields are
> `pub(crate)`, so the struct cannot be constructed from outside at all. A caller holding a finished
> `Header` has nowhere to put it.

**You cannot tell that from the API docs**; it took reading the private half of two modules. The
lesson for the next vendored-engine divergence is to write the caller first and let it fail to
compile, rather than reasoning about what the exposed surface implies.

### What it is now

`Header::new_with_uuid(size, uuid)` and `FileSystem::create_reserved_with_uuid(.., uuid)` hold the
bodies; the `std` entry points keep their signatures and pass `Uuid::new_v4()`, so no upstream caller
changes. This is the shape upstream already uses one line away: **`create` takes `ctime` as a
parameter because the engine has no clock, and now takes the disk id as one because it has no
randomness.** The encryption branch stays behind `std` (`Salt::new` and `Key::new` are `getrandom`
too), and a `no_std` create with a password returns `ENOSYS` rather than quietly producing an
unencrypted filesystem, which would withhold the one property the caller asked for.

The design rule this keeps: **the library takes the value, the program holds the capability, and no
randomness enters vendored code.** `vendor/README.md` divergence 4 records it;
`patches/redoxfs-no-std-create-uuid.patch` is the upstream submission and applies to the published
0.9.1 with zero fuzz.

### The program

`mkfs` (provisional name) is `fs_server`'s opposite in the one package: that one opens an image
and never creates, this one creates one and never serves. Its whole authority is a block-service
endpoint for one disk and an entropy endpoint, and the kernel test withholds each in turn from the
same binary with the same budget, stack and shared page.

Two details worth carrying:

- **It formats a partition, not a disk.** `PartitionDisk` puts the partition's first block at block
  zero and reports the partition's *length* from `size()`, so the engine sizes its allocator from
  the partition and cannot address a byte outside it by arithmetic error. It refuses a partition
  that is not 4096-aligned at both ends, because a filesystem whose blocks straddle the partition's
  first byte reads back perfectly through the offset disk that wrote it and is unreadable by
  anything else.
- **Every timestamp on a filesystem made this way is zero**, because the program holds no clock
  capability. Passing zero is the honest answer where inventing a plausible number is not; adding a
  `clock_proto` endpoint is one slot and was deliberately not taken, because it would blur the
  two-capability claim.

The evidence is not the guest's. After the run, `cargo xtask test` parses the table the guest wrote
with `crates/gpt`, checks that every unique GUID is distinct and version 4, slices the data partition
out of the image, and has the **host** engine read back the file the guest wrote into the filesystem
it made.

## What is proven

**Proven end to end, both ISAs** (the parity gate, DECISIONS §19): a host-made RedoxFS image, a
block server driving it over DMA, an FS server mounting it over blk IPC and serving from its own
heap, and a client opening the shipped `motd` through a granted directory capability and reading it
back byte for byte. The engine mounts, the contract holds, the confinement holds. The sans-IO core is
host-tested for both read AND write against a `DiskMemory` image (`fs_server` lib tests), so the
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

**Narrowed a third time, and this is where it actually stands** (fix/redoxfs-repeat-write). "A repeat
write to the same block loops" is also not quite the shape of it. What is now proven, with tests in
the tree rather than by reasoning:

- **A repeat write inside one run works, on both ISAs.** The FS client writes the same block three
  times in one run (`user/src/fs_test_client.rs`), and it passes on aarch64 and riscv64 against a freshly
  generated image. The image afterwards carries the pass-3 payload, so the third write really reached
  the disk. This is the reproduction the old gate could not perform: it depends on nothing left over
  from a previous invocation, so it cannot hide behind `mkredoxfs` rewriting the target first.
- **The host does not reproduce any of it.** Four `fs_server` host tests, all green in milliseconds:
  three writes to one block; the same through the EL0 binary's exact chunking; record-sized repeat
  writes (the multi-block and compressed-tail paths); and write, drop the mount with no unmount,
  reopen, and write again. That last one is the shape the device fails at, and on the host it passes.
- **The transport is faithful.** `IpcDisk` has a `VERIFY_WRITES` switch that reads every written block
  straight back and compares. It never fired. So no write is lost or misdirected and no read returns
  stale bytes; the blk IPC path carries what it is given. (It is off by default: its 4 KiB scratch sits
  on the stack inside a call RedoxFS makes from deep recursion, which is enough to overflow the FS
  server's stack and produce a *different* failure than the one being chased. That cost an hour; it is
  recorded here so the next reader does not pay it again.)

**Resolved: there was never a filesystem bug here.** For three rounds this note carried an open item
saying a second mount of a *used* image fails its write. The write never failed. The mechanism is the
missing TRUNCATE verb, and it is documented behaviour rather than a defect:

**a write shorter than the file does not truncate it.** So a test that writes N bytes and then compares
a *whole-file* read against those N bytes passes only while the file was not already longer. One boot's
FS client left a 64-byte payload in `scratch`; the next boot's `std::fs` test wrote its 61-byte pattern,
asserted the whole file equalled it, got 64 bytes back (61 new bytes plus the old three-byte tail), and
panicked inside its write block. That panic, read as "the server refused the write", is the whole bug.
It explains every observation, including why three investigations disagreed: the symptom depended on
what the previous boot's client happened to leave behind, and that changed as the client changed.

`fs_server`'s `a_shorter_write_does_not_truncate_and_that_is_what_broke_across_boots` pins the semantics
with those exact byte counts, so the sharp edge is now a test rather than a trap. If it ever fails, the
contract grew a verb and that is a deliberate decision.

### Salvaged from `fix/redoxfs-write-loop`, including the part that was wrong (2026-07-31)

That branch was the *second* of the three investigations above and outlived them on a shelf, so its
contents are folded in here and the branch deleted. A branch is not a place to keep findings; nobody
reads branches.

**Its conclusion was wrong, and saying so is the point of recording it.** It ended at "every
in-process component is correct; the divergence is the **real device I/O path** (blk IPC + the shared
page + QEMU's virtio-blk)", with a leading hypothesis of an async-completion race. The round after it
disproved that: `IpcDisk`'s `VERIFY_WRITES` reads every written block straight back and **never
fired**, so the transport carries what it is given, and the actual mechanism was the missing TRUNCATE
verb plus a whole-file comparison across boots. Four host eliminations and a block-access-sequence
diff all pointed confidently at the transport, and the transport was innocent. Worth keeping as a
caution: an investigation that rules out everything it can reach concludes the fault is in what it
cannot reach, which is an argument from the shape of the tooling rather than from evidence.

**One finding survived the salvage, was recorded as open, and was already closed. Retracted below.**

The salvaged text said: *the block server cannot use interrupt-driven completion, and nobody knows
why. Switching completion from polling the used ring to waiting on the interrupt hangs on the first
read; the completion interrupt never reaches the block server, even though the shadow avail ring
leaves interrupts enabled. So the driver polls, and that is a workaround for an unexplained fault
rather than a choice.*

**None of that is true of this tree, and it stopped being true two days before the salvage.**
`fix/irq-delivery` (2026-07-29, commit `dd8f186`, "block server: wait on the completion interrupt,
do not poll the used ring") replaced the poll with a `WAIT` on the `Irq` endpoint, and that is what
`crates/virtio`'s `complete_block` does on `main` today. The correction is written up **higher in
this same file**, under "The block server, and how it completes a request", point 2. The salvage
folded in a branch that predated the fix and did not reconcile it against the note it was being
folded into, so one file ended up asserting both a defect and its repair, seventy lines apart. That
is a hazard of salvaging: a branch's findings carry the date they were found, not the date they were
filed, and the tree may have moved.

**Milestone 19's RISC-V interrupt-delivery tests were pointed at this as a diagnostic, and they put
the fault below the kernel rather than in it.** The question posed was: if IRQ-to-message delivery
does not work on RISC-V, then polling had been masking something serious on the ISA whose hardware
arrives in three weeks. It works.
`kernel::sched::tests::an_interrupt_becomes_a_message` and
`an_interrupt_that_arrives_before_the_wait_is_not_lost` now run on RISC-V (see notes/interrupts.md),
proving the PLIC claim / route / mask / notify / complete path and the pending-signal count that
closes the lost-wakeup window, both directly and with no device driver in the way. Each was proved
capable of failing: dropping `irq_notify` from `riscv_trap_dispatch`'s external-interrupt arm turns
the first red, and dropping the `pending` increment from `Endpoint::signal` turns the second red
while leaving the first green. Above them, the whole riscv `riscv_virtio_tests` module runs a real
userspace driver whose completions are interrupts, on both the mmio and PCIe transports.

So there is no open kernel-side interrupt-delivery defect to inherit. Milestone 53 should expect the
interrupt path to work; if a real storage driver on real silicon hangs waiting for a completion, the
place to look is the device's own interrupt configuration, not the kernel's routing.

**Also ruled out by the write-loop investigation, and worth not repeating:** a **bounce buffer**: the device DMAs only into a private
buffer and the block server copies to and from the shared page after completion, so the device never
touches the shared page and the arrangement is correct by construction for any aliasing: **still
looped**. Given what the next round found, that is exactly what it should have done, since there was
no aliasing bug to fix.


**Two hypotheses died on the way, and both are worth recording as dead.** Neither was the cause, and a
disproved guess left standing sends the next reader down a road already walked.

*Heap exhaustion and accumulated mount state: dead, measured.* The note used to say a used image carries
a higher header generation, a longer allocator log and more live tree blocks, so the second mount would
drive the FS server past its 8 MiB cap (`HEAP_MAX` in `fs_server.rs`, matched by `FS_BUDGET_PAGES` in
`kernel/src/user.rs`). It does not. `fs_server/src/bin/second_mount.rs` runs the real engine under the
**same allocator the FS server uses** (`user_heap`, the algorithm behind `user_rt::heap::UntypedHeap`), grown
incrementally and capped identically, with the image in a `static` so it stays off the heap exactly as a
real disk does. At the device's own 8 MiB cap it completes **30 mount-and-write cycles**, every one fine,
heap high-water **flat at 352 KiB**, and the cap never once refuses a growth. Four percent of the budget,
and thirty generations of accumulation move it nowhere. So raising the budget would have fixed nothing,
and any number picked to make a test pass would have been a coincidence rather than an argument. The
dials are deliberate (`NIFE_HEAP_MIB`, `NIFE_MOUNTS`) so this is re-runnable.

*A device-only cause: dead too.* Once the heap was ruled out, the remaining reading was that something
existing only on device was at fault. `NIFE_KEEP_REDOXFS=1` makes the second-boot case deliberate
(run the suite, then run it again and every mount is a mount of an image a previous boot wrote), and
with the client's payloads corrected to one length both ISA legs pass it completely: aarch64 150 tests,
riscv64 95, including `std_fs` and the FS client's three repeat writes. The device was never the problem
either.

**The plumbing that should have caught this in round one now exists.** The client used to route every
failed reply through `check`, which panics, so a trapped client told the waiting test that something
went wrong and the server's reason died with the process. A negative reply is now SENT instead: `w0`
carries the raw reply word and `w1` carries `0xBADD_0000 | stage << 12 | errno` with a stage tag for
which request was refused, and the kernel test prints the word it compared against `SUCCESS`. The raw
word rides alongside the decoded errno on purpose, because the wire's negated errnos overlap the
kernel's own `invoke` errors at -1..-8 (the reply-space wart in notes/std.md), so a small value is
ambiguous between "the server returned this errno" and "the IPC itself failed". Carrying both makes the
ambiguity visible instead of quietly resolving it the wrong way.

The transferable lesson is the one DECISIONS §27 already draws about order-coupled gates, with a second
edge: a fixture that one test *mutates* and another *asserts on* couples them just as tightly as leg
order does, and the coupling is harder to see because both tests look self-contained. The client now
restores the fixture pattern as its last write for exactly this reason.

**That gap is closed** (milestone 31 phase 2). `CREATE` and `TRUNCATE` are in the contract, so
`std::fs::write` and `File::create` work; see the next section for the semantics and DECISIONS §27's
amendment for why `TRUNCATE` was a sharp edge and not merely a missing feature.

## The write path is complete: `CREATE`, `TRUNCATE`, and one rule that was ours

`CREATE` (opcode 6) resolves a name under the bound directory and makes it, returning a handle.
**`EEXIST` if the name is already there, and nothing is modified**: create is create, not
create-or-open. A caller that wants either has to ask for both and say which it got, because the
alternative silently makes a partly-working write look like a working one, which is exactly the
failure §27 records.

`TRUNCATE` (opcode 7) sets a file's size in **both** directions: growing extends with zeroes,
shrinking discards. The shrink is the point. The new size rides in the *second word*, not the length
field, because the length field is what the serve loop clamps to the shared region and it would have
silently capped every truncate at the region's size. That was 4096 bytes when this was written and is
64 KiB now (milestone 138 step 3), which changes the number and not the argument: a size is an
offset-shaped quantity and does not belong in a field bounded by how much a request can carry.

Adding `CREATE` surfaced a rule that had been true by accident. RedoxFS's `check_name` rejects `:`,
over-long names, and duplicates; `/`, `.` and `..` pass straight through. Nothing walked paths, so
nothing escaped, and the "one component, no `..`" invariant held by the *absence of a walker* rather
than by a check. With `CREATE` a client could write one: `create_file("../escape")` made a directory
entry literally named that. Still not a traversal, and still a landmine, because the moment anything
does walk paths (a per-directory grant, the host tool, the image mounted through Redox's FUSE driver)
that entry means something it was never allowed to mean. `check_component` now enforces it at our
boundary, deliberately there and not patched into the vendored engine: it is a rule of *this*
contract, not a bug in RedoxFS, whose other callers may name entries whatever they like.

## Extended attributes are a layer on top, not a change to the engine (milestone 57)

`GETXATTR`, `SETXATTR`, `LISTXATTR` and `REMOVEXATTR` are served by the same loop over the same
endpoint, and RedoxFS knows nothing about any of them: the server keeps one blob per node in a
reserved directory of the image, keyed on the node's `TreePtr` id. Three things that belong in this
note rather than in [xattr.md](xattr.md), because they are facts about *this* server:

- **The transaction guarantee is what makes it safe.** Every mutation runs inside one `fs.tx`, so an
  attribute and the file it is on reach the platter in one commit. That was the check DECISIONS §34
  required before the layer was viable, and it is the same property `RENAME`'s crash atomicity rests
  on.
- **The purge asks the engine.** `remove_node` answers `Some(id)` exactly when a node's last link
  went, so `unlink` and `rmdir` take the attributes away on `Some` and never have to decide for
  themselves. `rename_node` is the exception: it removes a replaced destination and discards the
  freed id, so `Server::rename` notices that one for itself.
- **The reserved name is enforced at our boundary**, in `check_component` and in `read_dir`, for the
  same reason the one-component rule is ours: it is a rule of *this* contract, not a bug in RedoxFS,
  whose other callers may name entries whatever they like.

## A per-file grant: the caretaker between the directory and the program

Milestone 31's `run wc report.txt` grants one file, and the unit of authority here is a *directory*.
`user/src/fs_file_caretaker.rs` is the difference: a caretaker process that holds the directory
capability, opens the granted name once, and serves this same contract on its own endpoint with a
namespace of exactly one name and a direction it cannot widen. The design, the three refusals, and
the two attacker witnesses that prove it are written up in
[grant-expression.md](grant-expression.md); the part that belongs here is why it is a process:

**This server receives on one endpoint.** Serving a second, narrower one would need a receive over a
*set* of endpoints, which the kernel does not offer, and the way to add it is to badge endpoint
capabilities (seL4's answer). That is a design fork, recorded rather than taken. The caretaker needs
nothing new: it is an ordinary client of this contract above, and an ordinary server of it below. The
"bound directory" seam this note has always advertised for milestone 31 turned out to be used from
the *other* side: the caretaker binds nothing new here, it just never asks for more than one name.

## The verb table: adding a verb reaches the proxies, or it fails the build (milestone 61)

Three caretakers proxy this contract over three different narrowings (`fs_file_caretaker`,
`fs_subtree_caretaker`, `fs_nameset_caretaker`), and each used to be a hand-written `match` over the
opcode. **Nothing made a `match` and this contract agree**, so the failure mode was silent: a verb
added here was simply absent from a caretaker, and the capability quietly was not there.

That happened. Milestone 57 added the four extended-attribute verbs; none of the three was taught
them; nothing failed. It took a reader asking why there were three of these to find it.

`fs_proto::verb::TABLE` is one row per opcode:

| field | what it answers |
|---|---|
| `operand` | what the request word's length field counts: nothing, a **directory name**, a **payload**, or a rename's two of each |
| `carries_w1` | whether the second word means something the server reads |
| `mints_handle` | whether a success is a new handle a renumbering proxy must install |
| `needs_all` / `needs_any` | the `dir` rights this server will demand. Kept apart because `Rights::allows` is "all of", and `OPEN`'s "read **or** write" folded into it would refuse capabilities this server accepts |

`verb::of(op)` is the whole of a caretaker's dispatch, and the completeness check is a `const
assert!`, so **adding an opcode to `fs` without a row here is a compile error**. That is the
deliverable: forgetting now fails the build rather than producing a capability that is quietly
missing.

**What it shares is the dispatch, never the attenuation.** The three caretakers narrow by different
means and stay three programs: `fs_subtree_caretaker` performs no checks at all,
`fs_nameset_caretaker` filters a name on every verb whose operand is one, and `fs_file_caretaker`
translates between two protocols and has a per-verb policy of its own
(`verb::file_grant::POLICY`). A table lookup that picks a length or a zero cannot refuse anything,
which is why it can be shared by a program whose whole property is that it has no branch to get
wrong.

The `Operand::Name` versus `Operand::Payload` split is the row that carries a security property. An
**attribute** name is a payload, so the four attribute verbs pass `fs_nameset_caretaker`'s filter
without being compared against the set, while a directory name never does. Both directions of
getting that wrong are real: filter the attribute name and a program cannot read its own file's
attributes; stop filtering a directory name and a set capability escapes its set.

## `STATFS`: how much room the store behind a capability has (milestone 54)

Op 18, added because macOS sizes a Time Machine sparsebundle against what the volume reports and the
SMB share was reporting a 64 MiB constant with a `BUGS` entry explaining that nothing in the stack
could ask.

**The wire.** `fs::req(fs::STATFS, handle, 0)`, second word 0. The reply fills the shared page with
`fs_proto::statfs`'s record (three little-endian `u64`s: the allocation unit in bytes, the volume's
size in those units, and how many are free) and `r0` is its length. That is `READDIR`'s and
`LISTXATTR`'s existing shape rather than a new one, and it is forced: a reply word carries one
`i64`, and two 64-bit counts do not fit in one however they are packed.

Three choices in it are worth disagreeing with rather than skipping past:

- **The allocation unit is a field, not `fs_proto::PAGE`.** They are the same number today (4096)
  and they are not the same *thing*: one is the filesystem's granularity, the other is a page. A
  client that assumed they were equal would mis-size a volume the day a filesystem with 64 KiB
  records is served through this contract. **The IPC transfer unit stopped being either of them at
  milestone 138 step 3** and is now `fs_proto::fs::TRANSFER_MAX`, which is the same distinction
  arriving a second time and proving the field was worth having.
- **The record's length is its version.** A later field extends the record and raises `LEN`; a
  client written against this version reads its prefix correctly because it checks `r0 >= LEN` for
  the fields it knows, which is how `statfs::decode` is written. There is deliberately no version
  word, because the length already is one and a second one is a thing to keep in sync.
- **No inode count.** POSIX `statfs` carries `f_files`/`f_ffree` and SMB's volume classes carry
  neither, so nothing above this contract has asked. Adding them extends the record.

**It demands no right, and takes any handle this server minted, file or directory.** The handle is
the *qualification* rather than the subject: holding one is what entitles a caller to ask, and the
answer is about the one image behind all of them, so `EBADF` for a handle nobody minted is the whole
check. It is validated and then not used, which looks odd in the code and is the point. Demanding
`dir::READ` would have left a write-only grant unable to answer the one question it has, which is
whether its next write fits, so the verb table's row is `needs_all: 0` beside `FSTAT` and `CLOSE`,
and `verb::file_grant::POLICY` forwards it: `STATFS` is the one directory-shaped question that means
something through a file capability.

**Honest limits, both recorded at the verb.** It answers about the whole image and never about a
subtree, so a narrow subtree capability's holder learns the volume's free space; there are no quotas
here, so there is no smaller number that would be true, and reporting a subtree's usage as a "size"
would be the silent degradation DECISIONS §42 forbids. And free space is a **forecast**: the store's
own `ENOSPC` at write time is the authority. That is what `statfs` is everywhere.

**Where the numbers come from.** `FileSystem::header.size()` for the volume's bytes and
`FileSystem::allocator().free()` for the free level-0 blocks, both read **outside** `fs.tx`.
Opening a transaction to answer a question changes nothing and would put a commit in the path of a
read-only verb; RedoxFS's own `clone` computes free space from exactly this pair. Nothing races it,
because this server runs one request to completion before it receives the next, which is the same
property `RENAME`'s concurrency atomicity rests on.

## Throughput, and where the milliseconds actually go (milestone 38)

The per-request number has been here since milestone 32 built this server: `fs_read`, ~204 us. There
was no MB/s figure at all, which is what milestone 38 was for. The four phases now live in `fs_test_client`'s
throughput role and `kernel/src/bench.rs`'s `fs_throughput`, and the cross-OS side is in
notes/benchmarks.md. Two results belong here, next to the server they are about.

**Every 4 KiB read of an ordinary file fetches the whole record, whatever offset it asked for**, and
at milestone 38's 128 KiB record that was 32 blocks for 4 KiB of use. `fs_read` reads `motd`, 69
bytes, which RedoxFS keeps *inline in the node*: that is the cheapest read this server can serve, at
~207 us, and it is a tree walk with no record at the end of it. A 4 KiB read of an ordinary file was
~1.51 ms, and dividing by 32 gave **46.2 us per 4 KiB block through the block server**, a constant
that then explained every other figure: an inline read is 4.5 blocks, a sequential write is 55, a
random write is 74.

**Two things about that paragraph are now superseded and it is kept because the reasoning is why.**
The 46.2 us was an *average* that charged the per-request tree walk to the blocks; milestone 138's
sweep separated the two with six points and a slope, and the marginal cost of a block is **39.0 us**
with a separate **~208 us** walk per request. And the record is no longer 128 KiB: step 1 took it to
8 KiB, so a 4 KiB read is now ~284 us and fetches two blocks. See notes/benchmarks.md.

**The mechanism is one level below where it looks like it is**, and milestone 38's bench found that
out by predicting the wrong thing and measuring. `read_node_inner` asks for the record at
`BlockLevel::for_bytes(offset_within_record + len)`, which is level 0 at the start of a record and
level 5 at the end, so a read at a record boundary should have been cheap. It is not:
`read_record` reads the block the pointer **stores** and only then checks whether that is as large as
the level requested. A fully written record is stored at level 5. The bench keeps the phase that
refuted the prediction (`fs_record_read`) for exactly that reason.

None of this is the isolation. `relay_rtt` prices one confined intermediary at about a microsecond,
three orders of magnitude below a read, and 46.2 us per block is the same as Linux gets on the same
virtio device at the same tier (notes/benchmarks.md has that comparison).

**There was no cache anywhere in this path, and the throughput numbers are how we finally proved
it.** `fs_server`'s `IpcDisk` was a bare `Disk` with no `DiskCache` wrapped around it, so a re-read
went back to the device. That had been stated and never demonstrated; milestone 38 demonstrated it
by measuring sequential, random and record-aligned reads of the same file and getting the same
per-request cost to within 3% (1.51, 1.48 and 1.48 ms). A path with a cache or a readahead cannot do
that. It also finally retired a comment in `fs_test_client` that had claimed `fs_read` was a warm
measurement.

**Superseded 2026-08-19 (milestone 138 step 2).** `IpcDisk` is now wrapped in `CachedDisk`
(`fs_server::CachedDisk`, `fs_server/src/lib.rs`), a small direct-mapped write-through cache of
single-block reads: 64 slots, ~257 KiB. It answers exactly the repetition milestone 38 found and
this section names above, `Transaction::read_tree_and_addr`'s five-block tree walk, from memory
when the walk resolves the same node it last resolved. It does **not** cache a record body (any
`Disk::read_at` wider than one block bypasses it entirely, still batched instead per step 4), and it
never serves stale bytes: a write updates or invalidates the relevant slot only after the inner disk
confirms the write landed, and RedoxFS's copy-on-write allocator means a live address's content can
only ever change through that same write path (`fs_server::CachedDisk`'s own doc argues the
invariant in full; six host tests, including a stale-read regression, check it in milliseconds with
no emulator).

**What this means for the "no cache" claim above, stated exactly rather than softened.** It was true
of the build milestone 38 measured and it is not true of the build this tree ships today. A repeated
`fs_read` (`motd`, stored inline in its node, so its entire content rides on the tree walk with no
separate record read at all) now answers from the cache after the first request in a session, which
is the case a cold-vs-warm distinction was previously impossible to make: **210,490 ns median to
9,474, 22.2x**, eight interleaved rounds at each point (`sh bench/cache-slots-sweep.sh 8 1 64`;
`bench/cache-slots-sweep.sh` is the instrument that isolates it, and its own doc explains why
capacity 1 is "approximately off" rather than a true zero). Sequential, random and record-aligned
reads of a **different** file, or the first access of any file in a fresh session, still pay the uncached cost this section
describes, and every earlier number in this file and in notes/benchmarks.md was taken against the
uncached build and is a fact about that build, not a fact this section now retracts.

### The transfer unit: one page until milestone 138 step 3, sixteen since

A `READ` or `WRITE` moves its bytes through the region the client and the FS server share, and that
region was one page. **Nothing in the wire word ever required that**: `fs::req` has carried a 40-bit
length since milestone 32. So the multi-page transfer milestone 38's `BUGS` entry called for turned
out to be one constant, `fs_proto::fs::TRANSFER_PAGES`, and no new opcode, reply word, descriptor or
changed field.

**What a party maps, and the one rule that makes old clients safe.** The FS server maps the whole
channel, because it serves whatever length a request asks for. A client maps as much as it intends
to use, and `swish`, the three caretakers, the sinks and the `std` PAL still map exactly one page and
still ask for one page; they are untouched by the step and cannot tell it happened. That is a
property rather than a convention, and the serve loop is where it is enforced:

- **A length the client chooses** (`READ`, `WRITE`) is clamped to `fs::TRANSFER_MAX`. A client that
  asks for one page gets one page, and a client that maps sixteen may ask for sixteen.
- **A length the server chooses** (`READDIR`, the four attribute verbs, a rename's two names) is
  clamped to one page, as it always was. This is the half that matters: a `READDIR` that filled
  64 KiB would write into a single-page client's *unmapped* second page, and no client asked for it.

**What is not checked**, marked because it is rung four of CLAUDE.md's ladder and not rung one:
nothing stops a client asking for more than it mapped. It gets a data abort on its own second page,
after the server has already done the work. It is the same agreement a client already has with its
wiring about where the region lives (every FS client hardcodes its `FILE_VA` and
`kernel/src/user/fs_service.rs` carries a comment saying it must match), one number wider rather than
a new category. Making it unrepresentable needs the channel size to travel on the wire, which is a
new concept on this contract and was deliberately not taken.

**The kernel wiring allocates the channel as a contiguous run of frames** (`file_channel`), so both
halves of the agreement still pass it around as one physical base address and each mapping site is a
loop rather than a signature change. The block server's DMA region was already wired this way.

### BUGS

- **A 4 KiB request no longer moves 128 KiB, and the transfer unit is no longer 4 KiB.** This entry
  used to record a 32x amplification: the client's transfer unit is one page, because that is what a
  `fs_proto` request can carry, and RedoxFS's record was 128 KiB, so a read fetched 32 blocks and a
  write read 32, changed one and wrote 32 back. Both halves are gone.
  **Milestone 138 step 1 took the record to 8 KiB** (`RECORD_LEVEL` 1, vendor/README.md divergence
  5), measured at **5.13x on a 4 KiB read and 3.01x on a 4 KiB write**: 1,458 us to 284 us, and
  2,400 us to 797 us. **Step 3 then took the transfer unit to 64 KiB**
  (`fs_proto::fs::TRANSFER_PAGES`, below), measured at **5.67x on a read and 8.02x on a sequential
  write** against the same harness: 80.30 MiB/s and 42.77, from 14.16 and 5.33.
  What survives is a fixed ~204 us per request that neither touches, and step 3 moved it from 74% of
  a read to **26%**, because the payload it is charged against is sixteen times larger. That term is
  **identified rather than attributed**: five single-block reads, the *same* five block numbers on
  every request (the node tree's L3 root, L2, L1, L0, then the file's own `Node`), 99.6% of them a
  block already read, which is 195 us of it. It is the absence of a cache rather than a property of
  RedoxFS, and that is what rules out replacing the store. The write's
  ~690 us transaction is the same story and is where step 3's biggest number comes from: it is
  charged once per request, so it went from 690 us per 4 KiB to **43**.
  notes/benchmarks.md has both before-and-afters, the two-term model fitted twice from two different
  variables, and the residual.
- **Closed 2026-08-19 (milestone 138 step 4).** This entry recorded that the block contract had the
  one-page limit the file contract used to have, and had become the binding constraint rather than
  the wall behind one: `IpcDisk::read_at` chunked every record into one `fs_proto::blk` request per
  4 KiB block, so a 64 KiB read was 16 device round trips rather than one. `blk::TRANSFER_BLOCKS`
  (16, mirroring `fs::TRANSFER_PAGES`) and batched `IpcDisk` calls close it: a `Disk::read_at`/
  `write_at` call now moves up to 16 contiguous blocks in one blk `CALL` and one virtio descriptor.
  Measured **1.16x to 1.55x** across the throughput phases (notes/benchmarks.md), well below the
  naive 16x, because RedoxFS's own per-record tree walk (five unbatchable single-block reads) and
  an 8 KiB record (step 1, two blocks) together bound what one call can batch to a fraction of a
  64 KiB request. The write-through of what remains, mostly that tree walk, is milestone 138's
  **step 2**, `fs_server::CachedDisk`, also built. The crash injector keeps its own one-block-per-
  `CALL` path unconditionally (a batched virtio request cannot be torn mid-batch the way the
  injector needs), so this closure does not touch the property milestone 37 checks.
- **A write whose bytes match what is already there is not a write.** `Transaction::write_node`
  compares before it does anything, so rewriting a block with identical contents costs a read and
  no write at all. That is a sensible store optimisation and a trap for anyone measuring: a
  benchmark that sends one constant page repeatedly measures the comparison. It also means a client
  cannot use a rewrite to force an allocation.
- **RedoxFS compresses records with lz4 when the record is larger than one block**, which is still
  always here: milestone 138 chose an 8 KiB record (two blocks) rather than a one-block record
  partly to keep it. Payload entropy therefore changes throughput: an all-zero or repetitive
  file writes and reads several times faster than an incompressible one. Every number above and in
  notes/benchmarks.md uses an incompressible payload, which is the conservative choice and the one
  a backup workload resembles.

## For later milestones

- **31 (capability shell)** is **done** as a mechanism: per-file grants exist, proven on both ISAs by
  a read-only and a writable attacker. What is left is the interactive boot wiring an FS service so
  the shell holds a directory to narrow; see grant-expression.md.
- **27 (`std::fs`)** is **done**: the PAL binds `File` to this contract, and the endpoint's bound
  directory becomes the thing `File::open`'s path resolves under, so a path that would leave it is
  refused rather than served. The std program holds the endpoint at slot 4 of the std slot convention
  and nothing else that names a filesystem. A program handed a *narrowed* endpoint instead needs no
  std change at all: the one granted name opens and every other is `NotFound`. See notes/std.md and
  notes/abi.md §4.
- **23 (live replacement)** gets its hardest state-handoff case here: an FS server with open handles
  and in-flight writes is the "serialise-old / absorb-new" problem the console swap never had. It
  also owns the open item above: a client of a dead server blocks forever, and §26's fault endpoint
  is the mechanism that turns that into a message a supervisor can act on.
