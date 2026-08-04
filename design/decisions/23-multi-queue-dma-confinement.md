# 23. Multi-queue DMA confinement: the validator's second direction (milestone 30)

**Decided and built 2026-07-28.** A virtio-net device needs two virtqueues (receive on queue 0,
transmit on queue 1), and receive is the direction where the *device writes into* the driver's
memory rather than reading from it. The §18 seam and the shadow-ring validator were queue-0-only,
so the net driver's prerequisite is a proved second queue and a proved second direction, built
under the same confinement discipline as the disk rather than bolted on when a NIC needs it.

**What changed, and what deliberately did not.** The validator (`validate_and_shadow`) did not
change at all: it bounds the address of every descriptor, `addr..addr+len` inside the driver's DMA
region, whichever way the device moves the bytes. That is the same property milestone 32's write
path relied on (§ notes/dma.md, "The write direction"), now asserted for the direction where the
*device* is the writer: a receive descriptor aimed at kernel memory would let the device overwrite
the kernel with an inbound packet, and it is refused before the device is rung, for the same reason
and by the same check as a read descriptor aimed there. The new work is per-queue **state and
plumbing**: each device carries a per-queue last-validated index and a per-queue ring block, and
`setup_queue`/`notify` take a queue number.

**The queue-layout contract.** Queue `q`'s descriptor table, available ring, and used ring sit at
`q * RING_BLOCK` (0x200) in both the driver's DMA region and the kernel-private shadow frame. One
shadow frame still holds every queue (MAX_QUEUES = 2, so 0x400 of a 4 KiB frame), asserted at
compile time. Queue 0's layout is byte-identical to the old single-queue layout, so the disk driver
needs no change: its data buffers already begin at 0x200 (= queue 1's block), free because a disk
has no queue 1.

**The surface stays narrow (§4 rule 3, the syscall-boundary discipline).** No new syscall and no
new object: the `Virtio` capability's existing `SETUP_QUEUE` and `NOTIFY` methods each grew a queue
argument (`SETUP_QUEUE(num, queue)`, `NOTIFY(queue)`), which is the established way this project
adds capability semantics (object revocation grew `Untyped` the same way). The disk passes queue 0
for both, so its ABI is unchanged. An out-of-range queue is `BadQueue`.

**Proof.** Two new unit tests beside the existing confinement suite, on both ISAs:
`the_validator_refuses_an_rx_descriptor_that_escapes_the_region` (an in-region device-writable
receive buffer validates; the same buffer aimed at kernel memory is refused) and
`a_second_queue_validates_on_its_own_block` (a good chain on queue 1 lands in queue 1's shadow
block while a sentinel in queue 0's block is untouched, and a queue-1 escape is refused the same as
queue 0). See notes/net.md and notes/dma.md.
