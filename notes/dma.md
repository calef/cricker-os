# Confining DMA without an IOMMU

The security audit's most severe finding: a userspace virtio driver could make the device DMA over
any physical memory. This note is how that hole was closed.

## Why it was a hole

The whole kernel is built to confine what a process can touch, and the MMU enforces it perfectly:
a process at EL0 cannot read, write, or execute a byte it was not mapped. But **the device is not
a process, and it is not behind the MMU.** A virtio block device is a second bus master: it reads
descriptors and does DMA against raw *physical* addresses, and page-table permissions: W^X, the
AP bits, the TTBR0/TTBR1 split, everything: simply do not apply to it.

The driver writes those physical addresses into the queue's descriptors. In milestone 9 the driver
owned the device registers and rang it directly, so a *hostile* driver could put the physical
address of the kernel image (or another process's frames) into a descriptor, mark it
device-writable, and issue a read. The device would DMA disk contents straight over kernel memory.
Nothing faulted, because the device honours no permissions. The driver was confined; the device it
drove was not.

Milestone 9's isolation was real for a driver *bug* (a bad address points at the driver's own
unmapped memory and faults it) and false for a driver *malice* (a deliberate kernel address just
succeeds). That is the gap.

## Why not an IOMMU

An IOMMU is the *clean* answer: it sits between the device and memory and translates every address
the device emits, confining it to a region the kernel programmed: **generically, with zero device
knowledge in the kernel.** That is why real systems use one, and it is what DECISIONS §10 meant by
"they had to bolt the isolation on afterwards with an IOMMU."

It is not reachable from here. QEMU `virt`'s SMMUv3 only covers the PCIe bus, not the platform
virtio-mmio devices we use. Getting behind it would mean switching to virtio-pci and writing a
PCIe enumerator, an SMMUv3 driver, and the virtio-pci transport: three substantial subsystems for
one gap. And real cheap hardware (a pre-4 Raspberry Pi) often has no usable IOMMU either, so the
software approach is the more broadly useful skill.

One honest nuance: an IOMMU is not *free* even when you have one. Someone still programs the
stage-2 tables that confine the device, and that someone is the kernel. The IOMMU buys
*generality* (no transport knowledge needed), not the absence of a trusted DMA policy.

## The fix: the kernel mediates the two DMA-critical powers

Without an IOMMU, something trusted must check every address the device will touch and refuse
anything outside the driver's region. The kernel takes back exactly two powers and leaves the rest
to the driver:

1. **The ring addresses.** The kernel programs `QUEUE_DESC/DRIVER/DEVICE` to fixed offsets in the
   driver's DMA region (`SETUP_QUEUE`). The driver never chooses them, so the rings themselves are
   always inside the region.
2. **The "go" signal.** The driver cannot write `QUEUE_NOTIFY`; it calls `NOTIFY`, and the kernel
   **validates every newly-published descriptor before ringing the device.** If any descriptor's
   `addr..addr+len` falls outside the region, the kernel refuses and the device is never told to
   go.

Everything else stays in the userspace driver: feature negotiation, the block request format,
sectors, reading results. The kernel owns the virtio **transport** (the descriptor and available
ring layout, enough to validate DMA) and knows nothing about **block devices**. The driver reaches
the device only through a `Virtio` capability; the device registers are no longer mapped into it.

This is a software stand-in for an IOMMU. It is less general (it understands the transport) but it
closes the hole: the device can only ever DMA within the driver's own region, so a hostile driver
can, at worst, corrupt itself.

## What is proved, and what is only mitigated (read this before quoting the milestone)

Milestone 35 machine-checked this boundary, and the shortest true summary of the result is longer than
one clause. So, the whole map first, because "DMA confinement is proved" said flat is wrong in a way
that matters for milestone 16a:

| The address path | What confines it | The evidence |
|---|---|---|
| An address in a **virtqueue descriptor** (every byte a disk or a NIC touches, and the GPU's command ring) | the shadow-ring validator below, plus the IOMMU where there is one | **Proved for every input.** Seven Kani harnesses over `crates/dma_validator`: both directions, indirect descriptors, chain cycles, ring-index wraparound, overflowing arithmetic, multi-queue blocks, and the mutated-after-validation race. Plus the end-to-end attacker tests, both ISAs, both transports. |
| The **page set of an IOMMU domain** (which frames the hardware will translate at all) | the domain builder | **Proved for every grant.** Six harnesses over `paging::domain`: the domain maps every whole page of the grant and no byte outside it. Format-independent, so one proof covers SMMUv3 and the RISC-V IOMMU. The remaining link (`Mapper::map` writes exactly the one leaf it is told) stays tested on both formats. |
| An address in a **device command payload** (virtio-gpu resource backings) | the IOMMU, and nothing else | **Partly proved, by the row above, and only there.** The validator cannot see these addresses at all: they are not in its input, so nothing about them is provable *from the transport*. What the domain proof buys is that the barrier's allow-list is exact; that the hardware then faults an address outside it stays an attacker test. |

Note what the middle row does for the third, because it is the one useful thing milestone 35 could prove
about the payload path and it is easy to miss. A payload-borne address is stopped by having **no
translation in the device's domain**, so "the domain maps exactly the grant" is precisely the property
that barrier needs, and it is now proved for every grant rather than tested on a few. The payload path
therefore improved from "tested end to end" to "the allow-list is proved exact, the hardware honouring it
is tested end to end". That is a real narrowing of the gap and it is *not* the same as closing it: the
transport still cannot see these addresses, and the hardware is still doing the enforcing.

That third row is the one to carry away. A virtio-gpu's backing addresses ride inside a
`RESOURCE_ATTACH_BACKING` **command payload** (DECISIONS §29, notes/framebuffer-contract.md). The
kernel bounds the descriptor that *carries* the command, so the payload bytes are in-region; the
addresses inside those bytes are never parsed. Proving the validator harder does nothing for this,
because the addresses never enter it, and teaching the transport to read virtio-gpu commands would put
device knowledge in the layer DECISIONS §18 keeps device-neutral and start a per-device arms race.
`the_iommu_refuses_the_gpu_a_framebuffer_outside_the_drivers_grant` is the evidence that the IOMMU
catches it, on both ISAs, by asserting on the hardware's own fault queue.

**And this is exactly where the argument for proving the validator now, rather than later, inverts.**
The reason milestone 35 was load-bearing is that milestone 16a's board, the VisionFive 2, **has no
IOMMU**: on first silicon the validator stops being defence in depth and becomes the sole DMA
confinement, so it had better be proved rather than sampled. That reasoning holds for the descriptor
path because the validator covers it. For the payload path there is no such comfort: with no IOMMU,
**nothing covers it.** Not the validator (wrong input), not the hardware (absent). A display driver on
that board is therefore either trusted with all of physical memory, or the transport grows a
virtio-gpu-aware check and pays the §18 cost on purpose. Whoever sequences 16a picks one; this note
exists so it is a decision and not a discovery. Under HVF the same gap is open, PCIe DMA being
unconfined there by standing default.

## The validator

Since milestone 35 the validation logic lives in `crates/dma_validator`, a host-testable pure-logic
crate the kernel's `validate_and_shadow` calls, and it is **machine-checked**: seven Kani harnesses
prove no descriptor the walk copies into the shadow escapes the granted region or is indirect, for
every input (both directions, multi-queue, chain cycles, ring-index wraparound, and the
mutated-after-validation race), and that the walk terminates. This was the last isolation boundary
that was attacker-tested but not proved; notes/verification.md has the harness table and, more
important, **the bounds with their justifications** (the short version: the queue size the proof fixes
is the system's own `QSIZE`, not a proof convenience, and the loop bounds are set one above what the
code can need so Kani's unwinding assertion turns them into a termination proof). The crate also now
*owns* the ring layout constants that `kernel/src/virtio.rs` aliases, because a proof about a copy of
the layout proves nothing about the layout that runs. The rest of this section describes what that
logic does.

`kernel/src/virtio.rs::validate_and_shadow` is the security-critical code. On `NOTIFY` it walks the
available ring from the last-validated index to the current one, and for each new head follows the
descriptor chain, checking that every `addr..addr+len` (with overflow rejected) lies within
`[dma_base, dma_base + dma_size)`. Each validated descriptor is *copied into the shadow ring the
device reads* (see the shadow-ring section below); that copy, not the validation alone, is what
closes the race. The chain walk is bounded by the queue size, so a malicious `next`-pointer cycle
cannot hang the kernel. The *outer* walk is bounded too: the available ring holds only `QSIZE` slots,
so a batch claiming more than `QSIZE` newly-available entries is malformed and refused before a
single descriptor is touched. Without that guard a driver could set `avail.idx` tens of thousands
past the last-validated index and spin the loop up to 65535 times, all under the `DEVICES` lock with
interrupts masked (bounded, single-core, but a needless latency spike). It is written to take the
driver and shadow ring addresses and read/write word pairs, so a test builds fake regions and
exercises it directly.

## The proof

Three tests, and one of them is the attack:

- `the_validator_refuses_a_descriptor_that_escapes_the_dma_region`: a unit test that builds a fake
  region and checks a good chain passes, a descriptor pointing at kernel memory is refused, one
  running past the end is refused, and a cycle terminates.
- `the_kernel_refuses_a_dma_descriptor_that_escapes_the_drivers_region`: **end to end**: a
  malicious driver at EL0 holds a real `Virtio` capability, points a descriptor at the kernel
  image, and submits. The kernel refuses; the driver reports it.
- `a_userspace_driver_reads_a_file_from_a_virtio_disk`: the legit path still reads a file off the
  disk through the validated transport.
- `the_validator_refuses_an_indirect_descriptor` and `the_kernel_refuses_an_indirect_descriptor_escape`
  the indirect-descriptor bypass below, refused as a unit and end to end.
- `feature_negotiation_strips_indirect_and_packed`: a driver asking for either ring-layout feature
  gets the bit cleared before the device sees it.

Verified the confinement can fail: with the region check stubbed to accept everything, the attack
goes through and the end-to-end test fails.

## Two ways the in-place check was still too weak

A second read of the validator turned up a gap that the "walk the chain, check each address" story
misses. The check assumes it sees every address the device will use. Two virtio features break that
assumption, and both are negotiable by the driver, because the kernel passes `DRIVER_FEATURES`
writes straight through.

**Indirect descriptors (`VIRTIO_RING_F_INDIRECT_DESC`, feature bit 28).** A descriptor flagged
`INDIRECT` does not describe a buffer. Its `addr` points at a *table* of more descriptors, and the
device follows that table. The old validator only understood `NEXT` chains, so it treated the
indirect descriptor as an ordinary buffer: it confirmed the table's address was in-region (it is,
the attacker puts the table in its own region) and stopped. It never walked the inner descriptors,
which is exactly where the attacker writes the kernel address. QEMU's virtio-blk offers this
feature, so a malicious driver could negotiate it and escape. This one is reachable on the current
target, not just on hypothetical async-DMA hardware.

**Packed virtqueues (`VIRTIO_F_RING_PACKED`, feature bit 34).** The whole ring format changes. The
validator understands only the split ring (separate descriptor table, available ring, used ring),
so against a packed ring it would be reading the wrong bytes while the device reads a ring nobody
checked.

The fix has two layers, both cheap:

1. **Refuse the features at negotiation.** `sanitize_driver_features` strips bit 28 from the low
   feature word and bit 34 from the high word before the value reaches the device. Without the
   negotiated feature, the spec forbids the driver from using it and QEMU rejects an `INDIRECT`
   descriptor outright. The honest driver asks for neither, so nothing legitimate breaks.
2. **Refuse the flag at validation.** The validator returns false on any descriptor carrying
   `VIRTQ_DESC_F_INDIRECT`. With the feature already stripped no honest descriptor sets it, so this
   costs one branch and makes the confinement fail closed if layer 1 ever regresses.

The deeper lesson is that both of these, and the time-of-check/time-of-use question below, are the
same shape of problem: **the validator reads descriptors out of memory the driver keeps mapped
writable.** Patching each feature is treating symptoms. The cure is to stop reading from shared
memory at all.

## The residual race, and the complete fix: a shadow descriptor ring

Even for a plain split ring with no fancy features, the check and the device's use are separated in
time. The kernel validates the descriptors, then writes `QUEUE_NOTIFY`, then the device reads the
descriptors. If the device reads them by its own asynchronous DMA (as real hardware does), the
driver can change a descriptor's `addr` after the kernel validated it and before the device
dereferences it. Validating memory the writer still controls is a time-of-check/time-of-use race by
construction.

On the current target it does not fire: the driver is a single EL0 thread that cannot run between
the kernel's validation and its `QUEUE_NOTIFY` write, and QEMU's virtio-blk pops descriptors
synchronously inside that MMIO write, capturing the addresses before any guest code resumes. So
this is recorded as a design limit, not a live hole. On hardware where the device fetches
descriptors asynchronously, it is real.

The complete fix is a **shadow descriptor ring**:

- Allocate a kernel-private page (not mapped into the driver) for the descriptor table and the
  available ring.
- In `SETUP_QUEUE`, program the device with the addresses of that private page instead of offsets
  in the driver's region.
- On `NOTIFY`, copy the driver's descriptors into the private ring, validate the copy, and only then
  ring the device.

Now the device reads descriptors the driver physically cannot touch, so there is nothing left to
race. The data buffers stay in the driver's region (it has to fill and drain them), and that is
fine: a driver racing its own buffer *contents* only corrupts its own I/O, never anyone else's
memory, because the *addresses* the device uses are the validated copies. The used ring can stay
shared too, since the device writes only indices and lengths there, not addresses. The copy also
subsumes the feature problem: the copy step either refuses `INDIRECT`/packed or recursively copies
and validates an indirect table, so there is one place that decides what the device is allowed to
read.

The cost is small (one page per device, a 128-byte copy per submit, the same validation against the
copy), which is the real argument for doing it rather than masking feature bits forever.

**This is now built.** `Device` carries a `shadow_base` (one frame, allocated and zeroed in
`register`, never mapped into the driver). `setup_queue` programs `QUEUE_DESC` and `QUEUE_DRIVER`
(descriptor table and available ring) at the shadow page, and leaves `QUEUE_DEVICE` (the used ring)
in the driver's region so the driver still reads its own completions. `validate_and_shadow` replaces
the old in-place `validate_avail`: for each newly-available head it walks the driver's chain,
validates every descriptor, and copies the validated bytes into the shadow at the same index, then
mirrors the head into the shadow available ring and publishes the shadow's `avail.idx` last. A
`dsb` (`arch::dma_wmb`) orders the shadow writes before the `QUEUE_NOTIFY`, because the device is a
separate observer. The driver's ABI does not change at all: it still builds its rings at the same
offsets in its own region and reads the same used ring, unaware that the device now reads a copy.

The invariant that makes it airtight: **the kernel only ever writes a validated descriptor into the
shadow.** So every descriptor the device can reach in the shadow has an in-region address, the
device bounds its own chain walk to the queue size, and a descriptor the driver mutates after the
check lands only in the driver's own copy, which nothing reads. `the_shadow_ring_is_immune_to_a_
descriptor_mutated_after_validation` proves exactly that: validate, then point the driver's
descriptor at the kernel, and assert the shadow still holds the validated address. The end-to-end
disk read (`a_userspace_driver_reads_a_file_from_a_virtio_disk`) still passes, which is the proof
the copy is functionally transparent: the device reads its descriptors from the shadow, DMAs into
the driver's data buffers, and the driver gets its file.

The feature-stripping and the `INDIRECT`-flag refusal stay as defence in depth in front of the
copy. The copy step does not walk indirect tables, so it refuses the flag rather than recursively
shadowing them, which is the simpler and stricter choice for a device that never needs indirect
descriptors.

## The write direction (milestone 32 phase 1)

The write-capable block path needed **no kernel change**, and it is worth recording why that is a
property and not luck: the validator bounds *addresses*, never directions. A blk read marks the
data descriptor device-writable (the device fills the buffer); a blk write leaves the flag clear
(the device consumes the buffer). Either way `validate_and_shadow` checks the same thing, that
`addr..addr+len` stays inside the driver's region, so both hazards are closed by one check: a read
descriptor aimed at kernel memory would let the device *overwrite* the kernel, a write descriptor
aimed there would let it *exfiltrate* kernel memory to disk, and neither address ever reaches the
device. The roadmap's claim that the transport "already speaks both directions" was verified
against the code and is accurate.

What was new: the driver's write verb (`user/src/virtio.rs::write_block`, one flag away from the
read), attaching QEMU's disks writable (one image file per transport, because QEMU write-locks a
file once any attachment can write; see the runners), and the tests. The write tests run the same
matrix as the read path: both ISAs, both transports (mmio and PCIe), a full pattern round trip
through a wiped buffer, plus a re-check of the superblock and directory so a write that strayed
off its own block would be caught.

**Kill-mid-write.** Errors here eat filesystems, so the teardown case is tested on both ISAs: a
driver submits a validated write and dies (panic, kill, reap) without collecting the completion or
acknowledging the interrupt, then a fresh driver resets the same device and must complete its own
round trip. Three facts make the abandoned request harmless, and all three are load-bearing:

- **The dead driver's DMA frame is never reclaimed.** A `Spawn`-mapped page goes through
  `map_physical`, whose contract is "Drop leaves it alone", so the in-flight completion (the used
  ring entry, the status byte) lands in a frame the allocator never re-issued. This is currently a
  deliberate leak, one frame per spawned driver. **The caveat for whoever builds DMA-region
  reclaim later:** freeing that frame on thread death would hand the device a landing zone the
  allocator may already have re-issued to someone else, which is a use-after-free performed by
  hardware. Reclaim must quiesce the device first (device reset, then confirm no requests are
  outstanding), and nothing enforces that today except this note and the test.
- **The kernel's transport state carries nothing in-flight.** `Device` tracks `last_avail`, a
  count of validated submissions, not completions, so an uncollected completion leaves nothing
  dangling; the next operator of the physical device resets it (status 0) and programs its own
  registration's rings from scratch.
- **The completion is the used ring, not the interrupt.** This one was missing at first, and the
  test caught it: the kill-mid-write case failed intermittently, `report_code(0xE3)` ("woke, but
  the device did not complete the request"), roughly one run in two. The abandoned write's
  completion still raises the device's interrupt line, and the kernel turns that into a pending
  signal on the interrupt's routing endpoint. That endpoint is shared across every driver of the
  same physical line, so the *survivor's* first `WAIT` consumed the dead driver's stale completion
  signal and then found its own request not yet on the used ring. The read path never noticed
  because it only ever expects one completion. The fix is the correct virtio discipline anyway: a
  driver treats the used ring advancing, not a single wakeup, as the completion, so
  `complete_block` (`user/src/virtio.rs`) now loops WAIT/ack until `used.idx` moves, discarding a
  wakeup that was really someone else's. Every real completion also raises the line, so the loop
  always makes progress. This hardens the read path too: coalesced and spurious interrupts were
  always possible and were only tolerated by luck before.

The riscv half of this test is also what exposed that the riscv trap path could not kill a
faulting user thread at all (it stepped over a U-mode `ebreak` and kernel-panicked on any other
U-mode fault, behind a comment stale since milestone 20). Fixed alongside; see
notes/riscv-parity-scope.md.

## The tradeoff, stated plainly

This moves the virtio *transport* into the kernel, which slightly walks back milestone 9's "the
driver operates the device." That is a real cost, taken deliberately, and it is defensible:
confining DMA *is* a transport concern, and the kernel still knows nothing about block devices. The
alternative (trusting the driver with all of physical memory) is what a monolithic kernel does
with an in-kernel driver, and the whole point of this project is not to.

## And now, in hardware (milestone 16b)

The "why not an IOMMU" section above was written when we had none reachable. Milestone 16b reaches
one, on both boards: an SMMUv3 on aarch64 and the ratified RISC-V IOMMU on riscv, in front of the
PCIe bus, each confining a device to a domain the kernel programs (notes/iommu.md, DECISIONS §20).
So the clean answer is now the real answer for the PCIe transport.

This shadow ring is **not** removed. It is demoted to defence in depth. Two reasons it stays. First,
virtio-mmio has no IOMMU in front of it on either board, so the software confinement is still the
only thing guarding the mmio disk. Second, even where the IOMMU is present, keeping both means a
regression in either layer is caught by the other: the transport still refuses a format it cannot
police (indirect, packed) before a descriptor is built, and the IOMMU still faults an escaping
address even if a validator bug ever let one through. The feature-stripping and the shadow copy cost
almost nothing and buy a second independent barrier, which is the right trade for the one DMA path
that can reach the whole machine.

## Multiple queues, and the receive direction (milestone 30)

The disk uses one virtqueue. A NIC uses two: receive (queue 0) and transmit (queue 1). virtio-net is
the reason the confinement grew a second queue, and receive is the reason it is worth stating what
"a second direction" does and does not mean.

**The plumbing that is new.** Each device now carries a per-queue last-validated index and a
per-queue ring block. `setup_queue(id, num, queue)` and `notify(id, queue)` take a queue number, and
queue `q`'s descriptor table, available ring, and used ring sit at `q * RING_BLOCK` (0x200) in both
the driver's DMA region and the one kernel-private shadow frame (two queues fit in 0x400 of a 4 KiB
frame, asserted at compile time). Queue 0's offsets are exactly the old single-queue layout, so the
disk driver did not change: its data buffers already start at 0x200, which is queue 1's block, and a
disk has no queue 1. The `Virtio` capability's methods grew a queue argument rather than gaining new
methods; the disk passes queue 0 and its ABI is byte-identical (DECISIONS §23).

**The validation that is NOT new, and why that is the honest finding.** `validate_and_shadow` did not
change. It bounds the *address* of every descriptor, `addr..addr+len` inside the driver's region,
whichever way the device moves the bytes. Receive is where the *device writes into* driver memory
(the driver posts an empty buffer, the device fills it with a packet), and that is exactly the shape
milestone 32's block write already leaned on: the validator "bounds addresses, never directions." A
receive descriptor aimed at kernel memory would let the device *overwrite* the kernel with an inbound
packet; a transmit descriptor aimed there would let it *exfiltrate* kernel memory onto the wire; both
are refused by the one in-region check, before the device is ever rung. So the "second direction" is
proved, not implemented: `the_validator_refuses_an_rx_descriptor_that_escapes_the_region` sets the
device-writable flag and asserts the refusal is about the address, not the flag, and
`a_second_queue_validates_on_its_own_block` proves queue 1 validates on its own block without
touching queue 0's shadow. Both run on both ISAs.

The used ring for the receive queue needs no extra confinement either: the device writes the received
length into `used.ring[i].len` and the packet bytes into the descriptor buffer, and both landing
zones are the driver's own in-region memory (the used ring is placed by the kernel; the buffer is
address-checked). The device never writes an *address* anywhere the driver chose, which is the whole
invariant.
