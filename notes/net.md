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

The mechanics are in notes/dma.md ("Multiple queues, and the receive direction") and DECISIONS §23.
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

## Piece 2: the virtio-net driver (built, both ISAs)

A NIC driven from EL0, behind Piece 1's confinement, the same shape as the disk driver. The kernel
enumerates the device (`find_net_device` beside `find_block_device`, on the mmio bus in
kernel/src/virtio.rs and the PCI bus in kernel/src/pci.rs; the enumeration structs were generalized
from block-specific to transport-neutral, since a register base and an interrupt are all the kernel
hands a driver either way), owns the registers and the two DMA-critical powers, and hands the driver
a confined `Virtio` capability, a DMA page, and an interrupt. On PCIe the NIC sits behind the IOMMU
(`iommu_platform=on`), the disk's pattern exactly, so it is confined in hardware too.

The driver is `user/src/virtio.rs::run_net`, dispatched by both driver binaries (the aarch64 tests
run it as a role of `hello`, the riscv tests as a role of the dedicated `blk` binary; both include
the shared `virtio` module). It brings up **both** virtqueues (receive = 0, transmit = 1) through the
one capability, passing the queue number to `SETUP_QUEUE` and `NOTIFY`. The whole net-specific DMA
layout (two ring blocks at the kernel's 0x200 stride, a receive buffer, a transmit buffer) fits in
the single 4 KiB DMA page the spawn service already hands every driver.

**The proof is a DHCP round trip**, no TCP/IP stack in the loop: post a receive buffer, hand-build a
DHCP DISCOVER (Ethernet + IPv4 + UDP + BOOTP, broadcast flag set), transmit it, and receive the OFFER
that QEMU user-mode networking (slirp) sends back. The driver parses the OFFER and reports the offered
address (`yiaddr`), which the test asserts lands in slirp's 10.0.2.0/24. A valid OFFER for our
transaction id is the only path to that report, so a match proves the DISCOVER left (TX) and the OFFER
returned (RX), across both queues and both directions of the confinement. Tests (both ISAs, both
transports): `a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net` and its `_pci` twin.

The runners attach two NICs (mmio + PCI-behind-IOMMU) on slirp when `CRICKER_NET` is set, which xtask
sets for both test legs. slirp needs no host file, so unlike the disk there is nothing for the runner
to fail loud on; the manufactured-fact hazard (a NIC asked for but not enumerated) is caught by the
test asserting the exchange rather than skipping.

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

## The socket contract (resolved: DECISIONS §25)

The roadmap sketched "an endpoint plus shared frames per connection; no ambient network." The
socket-identity question below was a genuine design fork, raised rather than built through; the
architect resolved it (DECISIONS §25 on main): **a socket is a socket id carried on the one `Stack`
endpoint, and the per-connection shared frame is the real granted resource. Minted-endpoint-per-socket
is deliberately deferred**, with the recorded trigger being a socket that must be delegated to a third
process. The rest of the shape below stands as the contract Piece 3 implements. Recommendation first,
then the questions (question 1 now answered).

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

**The questions.**

1. **Socket identity (RESOLVED, §25): a socket id on the stack endpoint.** A minted endpoint per
   socket is the purest capability shape (a socket IS an unforgeable object, delegatable on its own),
   but it spends a kernel object (a page) per connection and needs the server to retype untyped per
   socket. A socket id (small integer) on the one stack endpoint is cheap and matches what
   `std::net`'s PAL wants (a file-descriptor-like handle); "which socket" rides in a message word,
   with the shared frame as the real per-connection granted resource. The architect chose the socket
   id and deferred minted-endpoint-per-socket to when a socket must be delegated onward.

2. **How does the shared frame's producer/consumer protocol work without a syscall per byte?** A
   ring buffer in the shared frame with head/tail indices, the driver pattern, so `send(len)` just
   advances a tail and messages the server. Straightforward, but the exact layout (one frame split
   TX/RX, or two frames) is a contract detail to pin.

3. **Blocking vs. poll for the PAL.** `std::net` is blocking by default. A blocked `RECV` on the
   socket endpoint gives blocking cleanly. Non-blocking/`poll` is a later PAL concern; phase one can
   be blocking-only and still satisfy the roadmap's "no sockets-API mimicry beyond what the PAL
   needs."

The driver (Piece 2) did not depend on any of this: it needs only Piece 1's confinement, and it is
built.

## smoltcp: the pin, and a corrected assumption

**Pin: smoltcp 0.13.1** (current on crates.io at 2026-07-28), `default-features = false`. Features
to enable: `proto-ipv4`, `proto-dhcpv4`, `socket-tcp`, `socket-udp`, `medium-ethernet`. Divergence
policy is the vendored-engine discipline (DECISIONS §18 point 3, and the RedoxFS pin): pin the
version, carry any patch as a recorded diff, note the reason. No patch is known to be needed yet;
smoltcp is no_std-clean and used across embedded Rust.

**Corrected assumption.** smoltcp bills itself as "for bare-metal, real-time systems **without a
heap**." It can run with fixed socket buffers and a static `SocketSet`, so the net server does **not**
strictly need the untyped-backed `GlobalAlloc` that RedoxFS (milestone 32) and the `std` PAL
(milestone 27) require. In the build we shipped, netd does use `alloc` (over user_rt's `UntypedHeap`,
milestone 27) because it is available and makes the socket set and per-frame buffers simpler; the
`alloc` feature is a convenience, not a precondition, so a fixed-capacity server remains possible if
that heap were ever unavailable.

## Piece 3 phase A: smoltcp doing DHCP over the confined NIC (built, both ISAs)

The net server, `netd` (user/src/netd.rs), is the networking form of the userspace-reuse thesis: a
real, reused TCP/IP stack (smoltcp 0.13.1, not hand-built) running entirely at EL0 over a NIC the
kernel confines by DMA. The kernel knows nothing about DHCP.

- `user/src/vnet.rs` presents smoltcp's `phy::Device` over the receive/transmit virtqueues: it brings
  the NIC up through the `Virtio` capability, posts receive buffers, copies received frames out (RX
  tokens own their bytes so they never borrow the device), and transmits via the DMA ring (TX tokens
  carry a raw pointer to the device, sound because netd is single-threaded and the device outlives
  any token within a poll).
- `netd` links `alloc` over user_rt's `UntypedHeap`, builds a smoltcp `Interface` and a DHCP socket,
  and runs the poll loop, blocking on the NIC interrupt between polls. It reports the acquired
  address, which the test asserts lands in slirp's 10.0.2.0/24 (`the_net_server_acquires_a_dhcp_lease_over_smoltcp`
  and its `_pci` twin, both ISAs). Only a real DHCP handshake driven by smoltcp over the confined NIC
  produces that.
- The spawn service (`virtio_service::start_net_server{,_pci}`) grants netd the confined transport,
  the interrupt, a DMA page, a report endpoint, and an **untyped budget** for the heap, plus extra
  stack pages for smoltcp's packet building.
- **Caveat (recorded):** the DMA region is one 4 KiB page, so the buffers are small and the MTU is
  small (`vnet::MTU`, 576). DHCP, DNS, and small TCP segments fit; a full 1514-byte frame does not. A
  larger MTU needs a multi-page contiguous DMA region, which the spawn path does not build yet. This
  is a demonstrator limit, not a protocol one.

DHCP is itself UDP, so smoltcp's UDP path over our NIC is exercised end to end by this test. What is
not yet built is the client-facing socket contract that lets *other* processes use the stack.

## Remaining work (Piece 3 phase B: the client-facing socket contract)

The §25 contract, so a process other than netd can open sockets. Design, concrete enough to build
from:

- **The Stack endpoint.** netd, after DHCP, serves requests on a `Stack` endpoint (RECV_CAP). A
  client holds `WRITE` on it plus an untyped budget (to mint the per-connection shared frame). A
  socket is a small integer **socket id** returned by open and carried in the request word of every
  later call; the per-connection **shared frame** is the real granted resource, delegated once at
  open via `SEND_CAP` and mapped by netd at a per-socket VA (§25).
- **Operations**, each a `CALL` on the Stack endpoint (which mints the reply cap netd answers on),
  the socket id packed into the request word: `OPEN_UDP`/`OPEN_TCP` -> socket id; `BIND(port)`;
  `CONNECT(ip, port)` (ip/port in the shared frame header, since CALL carries only two words);
  `SEND(len)` and `RECV() -> len` (payload already in / left in the shared frame); `CLOSE`. A
  blocking `RECV` is netd driving the smoltcp poll loop (WAIT on the NIC interrupt) until the socket
  has data, then replying, the disk driver's discipline one layer up.
- **Concurrency model, phase one:** single-threaded netd, one synchronous exchange per request. netd
  blocks on the Stack endpoint between requests and drives the network inside handling one request.
  This suffices for the `std::net` PAL's blocking calls and for request/response traffic; concurrent
  connections and listening sockets want either userspace threads (milestone 19c TCBs) or a
  select-like wait, which is the phase-two extension.
- **Tests, and the honest gap.** UDP is deterministically testable over slirp: a client opens a UDP
  socket, sends a DNS query to slirp's built-in resolver (10.0.2.3:53), and verifies the response,
  exercising the whole contract with a real protocol. TCP end to end is the gap: slirp NATs outbound
  TCP to the host, and a deterministic peer needs host setup (a listener, or `guestfwd`), which the
  QEMU-only, zero-host-setup test model does not provide. So the TCP socket type is built to the same
  contract, but its end-to-end test is limited to what a deterministic peer allows (a connect whose
  handshake or refusal is observable); a full TCP data round trip is recorded as needing a test peer,
  not left as a silent gap.

This binds milestone 27's `std::net` PAL, replacing its `Unsupported`. Scope discipline holds: TCP,
UDP, DHCP, no sockets-API mimicry beyond what the PAL needs.
