# The network stack as a confined component (milestone 30)

Milestone 30 is three pieces in order: multi-queue transport confinement, a userspace virtio-net
driver behind it, and a TCP/IP stack (smoltcp) in a net server speaking a capability-shaped socket
contract. This note records what is built, the prior art read before drawing the contract, the
contract proposal (a design fork left for Chris), the smoltcp pin, and the remaining work.

## Piece 1: multi-queue confinement (built, both ISAs)

The disk uses one virtqueue; a NIC uses two (receive on queue 0, transmit on queue 1). The §18
transport seam and the shadow-ring validator were queue-0-only. Piece 1 grew them to N queues
(N = 2 today, fixed and asserted) under the same confinement discipline, so the driver work sits on
proved ground rather than a NIC forcing a retrofit.

The mechanics are in notes/dma.md ("Multiple queues, and the receive direction") and DECISIONS §21.
The short version:

- `setup_queue(id, num, queue)` and `notify(id, queue)` take a queue number; the `Virtio`
  capability's methods grew an argument rather than gaining new methods, so the surface stays narrow
  and the disk's ABI is byte-identical (it passes queue 0).
- Queue `q`'s rings live at `q * RING_BLOCK` (0x200) in both the driver's DMA region and one
  kernel-private shadow frame. Per-queue last-validated index; per-queue PCI doorbell.
- **The validator did not change.** It bounds descriptor addresses, not directions. Receive is the
  direction where the device *writes into* driver memory, and the same in-region check that stops a
  read descriptor aimed at the kernel stops a receive descriptor aimed there. This is the property
  milestone 32's block write already relied on, now proved for the device-as-writer direction.

Tests (both ISAs): `the_validator_refuses_an_rx_descriptor_that_escapes_the_region` and
`a_second_queue_validates_on_its_own_block`, beside the existing confinement suite.

## Prior art, read before the contract

The reuse call (smoltcp, not a hand-built TCP) is settled in the roadmap. What the prior art informs
is the *contract*: how a process asks a userspace stack to open a socket, and how bytes and events
cross the boundary.

**seL4 netstack componentization** (and the CAmkES/lwIP and later Rust efforts). The stack is a
component; clients reach it over seL4 endpoints, and bulk data moves through **shared dataports**
(pre-shared memory regions), not through the IPC message. Control (open, connect, close, "data
ready") travels as small messages on the endpoint; payload lives in the shared region. This is the
cleanest match to what this kernel already has: an `Endpoint` for control and a delegated `Frame`
for the shared buffer. The lesson taken: keep the per-connection data plane in shared frames and the
control plane in three-word messages, exactly the split DECISIONS §10 already chose for IPC.

**Fuchsia Netstack3** (Rust, the closest cousin). Sockets are **handles** (Fuchsia's capabilities);
there is no ambient network, and a component reaches the stack only through a handle routed to it by
the component framework. Netstack3 is a state machine driven by events, which is also smoltcp's
shape. The lesson: a socket is a capability, the stack is named by a capability, and "no ambient
network" is enforced by the same mechanism as everything else (you hold a handle or you do not).
This is the model the roadmap already commits to; Netstack3 is the evidence it works in Rust at
scale.

**Plan 9 /net, the counter-design.** Everything is a file: a connection is a directory
(`/net/tcp/clone`, then `ctl`, `data`, `status`), you `write` "connect 1.2.3.4!80" into `ctl`, and
`read`/`write` the `data` file. It is elegant and it is the wrong fit here, twice over: it needs a
filesystem-shaped namespace (which arrives with milestone 32, not now), and "everything a file"
means "everything reachable by path," which is the ambient authority this project inverts. Read as
the road not taken: the capability contract is what /net's `ctl` file would be if designation were
authorization instead of a path lookup.

Synthesis: seL4's endpoint-plus-dataport data plane, Fuchsia's socket-is-a-capability control plane.
Neither is novel here; both are what this kernel's primitives already point at.

## The socket contract (proposal, a design fork for Chris)

The roadmap sketches "an endpoint plus shared frames per connection; no ambient network." The
concrete method set, connection lifecycle, and how `std::net`'s PAL (milestone 27) binds are not
decided. This is a genuine design fork under the milestone rules, so it is written up here rather
than built through. Recommendation first, then the open questions.

**Recommended shape.** A process holds one capability to the stack: a `Stack` endpoint. Everything
is a `CALL` on it or on a per-connection reply channel, control in the three-word message, bytes in
a per-connection shared frame delegated at open time.

- `Stack::open(kind, ...)` where `kind` is TCP or UDP -> a **socket capability** (a fresh endpoint
  minted by the server, or a small integer socket id carried on the stack endpoint; see open
  question 1). At open, the client delegates (or the server delegates back) a shared `Frame` that
  becomes that connection's TX/RX ring, seL4-dataport style.
- `Socket::connect(ip, port)`, `Socket::bind(port)`, `Socket::listen`, `Socket::send(len)`,
  `Socket::recv() -> len`, `Socket::close`. `send`/`recv` carry only a length; the bytes are already
  in the shared frame. "Data ready" is a message on the socket endpoint, the same way an `Irq`
  capability delivers an interrupt (WAIT-shaped), so a blocking read is a blocked `RECV`.
- DHCP and the interface config live entirely inside the server; a client never sees "the network,"
  only its own sockets. The server runs smoltcp's DHCP socket at startup and does not expose it.

**Why this fits.** It is the disk driver's discipline one layer up: the kernel confines the NIC's
DMA (Piece 1), the net server owns the smoltcp state and the NIC driver capability, and a client
gets exactly the sockets it was granted and no interface handle at all. Bytes never cross the
syscall boundary in a message (DECISIONS §10); they cross in a shared frame the two parties both
map.

**Open questions (the fork).**

1. **Socket identity: a minted endpoint per socket, or a socket id on the stack endpoint?** A minted
   endpoint per socket is the purest capability shape (a socket IS an unforgeable object, delegatable
   on its own), but it spends a kernel object (a page) per connection and needs the server to retype
   untyped per socket. A socket id (small integer) on the one stack endpoint is cheap and matches
   what `std::net`'s PAL wants (a file-descriptor-like handle), but "which socket" then rides in a
   message word rather than being the capability itself, which is weaker designation. Recommendation:
   **socket id for phase one** (cheap, and the PAL needs an fd-like anyway), with the shared frame as
   the real per-connection resource; revisit minted-endpoint-per-socket if a socket ever needs to be
   delegated to a third process. This is the decision most worth Chris's eye.

2. **How does the shared frame's producer/consumer protocol work without a syscall per byte?** A
   ring buffer in the shared frame with head/tail indices, the driver pattern, so `send(len)` just
   advances a tail and messages the server. Straightforward, but the exact layout (one frame split
   TX/RX, or two frames) is a contract detail to pin.

3. **Blocking vs. poll for the PAL.** `std::net` is blocking by default. A blocked `RECV` on the
   socket endpoint gives blocking cleanly. Non-blocking/`poll` is a later PAL concern; phase one can
   be blocking-only and still satisfy the roadmap's "no sockets-API mimicry beyond what the PAL
   needs."

Until these are decided, the driver (Piece 2) can proceed independently: it does not depend on the
socket contract, only on Piece 1's confinement.

## smoltcp: the pin, and a corrected assumption

**Pin: smoltcp 0.13.1** (current on crates.io at 2026-07-28), `default-features = false`. Features
to enable: `proto-ipv4`, `proto-dhcpv4`, `socket-tcp`, `socket-udp`, `medium-ethernet`. Divergence
policy is the vendored-engine discipline (DECISIONS §18 point 3, and the RedoxFS pin): pin the
version, carry any patch as a recorded diff, note the reason. No patch is known to be needed yet;
smoltcp is no_std-clean and used across embedded Rust.

**Corrected assumption.** smoltcp bills itself as "for bare-metal, real-time systems **without a
heap**." It can run with fixed socket buffers and a static `SocketSet`, so the net server does **not**
strictly need the untyped-backed `GlobalAlloc` that RedoxFS (milestone 32) and the `std` PAL
(milestone 27) require. The `alloc` feature is a convenience (dynamic socket sets, DNS), not a
precondition. So Piece 3 is not gated on the allocator the way the roadmap's RedoxFS note is; a
fixed-capacity net server can ship first, and the allocator can arrive with milestone 27 as planned.
If the server later wants dynamic sockets, enabling smoltcp's `alloc` feature is the switch.

## Remaining work (Pieces 2 and 3)

Piece 1 is the prerequisite and it is done. The rest, in order, with the honest dependencies:

1. **Runner wiring + net enumeration (Piece 2 groundwork, independent, testable).** Attach
   `-netdev user -device virtio-net-{device,pci}` in both QEMU runners (user-mode NAT, zero host
   setup). Add `find_net_device` to kernel/src/virtio.rs (DeviceID 1) beside `find_block_device`,
   and a boot print, mirroring the disk. First testable increment: "a net device is found on the
   bus, the kernel owns its transport."
2. **The virtio-net driver (Piece 2).** Same discipline as the disk: kernel owns registers and the
   DMA-critical operations, driver gets a `Virtio` capability and a larger DMA region (RX needs
   posted buffers). Uses queue 0 (RX) and queue 1 (TX) through Piece 1's confinement. On PCIe it
   sits behind the IOMMU (`iommu_platform=on`, §20), following the disk's pattern. Testable
   end-to-end with QEMU user-mode networking (ping the gateway, or a loopback frame).
3. **The net server (Piece 3).** smoltcp behind the socket contract above, once the fork is
   resolved. DHCP at startup, TCP + UDP sockets, blocking PAL shape. This is what milestone 27's
   `std::net` PAL binds to, replacing its `Unsupported`. Scope discipline: TCP, UDP, DHCP, done.

The design fork (the socket contract) and the smoltcp/allocator finding above are the two items to
settle before Piece 3. Piece 2 can start now.
