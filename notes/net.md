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
filesystem-shaped **namespace**, which milestone 32's FS server deliberately does not provide (a
client holds a directory capability, and open-by-path exists only inside the server), and "everything
a file"
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

1. **Socket identity: DECIDED 2026-07-28 (Chris): a socket id on the stack endpoint for phase
   one; minted-endpoint-per-socket is the deliberate later step, tracked in DECISIONS §25.** The
   trade as it was put to him: a minted endpoint per socket is the purest capability shape (a
   socket IS an unforgeable object, delegatable on its own), but it spends a kernel object (a
   page) per connection and needs the server to retype untyped per socket. A socket id (small
   integer) on the one stack endpoint is cheap and matches what `std::net`'s PAL wants (a
   file-descriptor-like handle), but "which socket" then rides in a message word rather than
   being the capability itself, which is weaker designation. The shared frame stays the real
   per-connection resource either way, which is what makes the later migration cheap.

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
(milestone 27) require. In the build we shipped, netstack does use `alloc` (over user_rt's `UntypedHeap`,
milestone 27) because it is available and makes the socket set and per-frame buffers simpler; the
`alloc` feature is a convenience, not a precondition, so a fixed-capacity server remains possible if
that heap were ever unavailable.

## Piece 3 phase A: smoltcp doing DHCP over the confined NIC (built, both ISAs)

The net server, `netstack` (user/src/netstack.rs), is the networking form of the userspace-reuse thesis: a
real, reused TCP/IP stack (smoltcp 0.13.1, not hand-built) running entirely at EL0 over a NIC the
kernel confines by DMA. The kernel knows nothing about DHCP.

- `user/src/vnet.rs` presents smoltcp's `phy::Device` over the receive/transmit virtqueues: it brings
  the NIC up through the `Virtio` capability, posts receive buffers, copies received frames out (RX
  tokens own their bytes so they never borrow the device), and transmits via the DMA ring (TX tokens
  carry a raw pointer to the device, sound because netstack is single-threaded and the device outlives
  any token within a poll).
- `netstack` links `alloc` over user_rt's `UntypedHeap`, builds a smoltcp `Interface` and a DHCP socket,
  and runs the poll loop, blocking on the NIC interrupt between polls. It reports the acquired
  address, which the test asserts lands in slirp's 10.0.2.0/24 (`the_net_server_acquires_a_dhcp_lease_over_smoltcp`
  and its `_pci` twin, both ISAs). Only a real DHCP handshake driven by smoltcp over the confined NIC
  produces that.
- The spawn service (`virtio_service::start_net_server{,_pci}`) grants netstack the confined transport,
  the interrupt, a DMA page, a report endpoint, and an **untyped budget** for the heap, plus extra
  stack pages for smoltcp's packet building.
- **Caveat (recorded):** the DMA region is one 4 KiB page, so the buffers are small and the MTU is
  small (`vnet::MTU`, 576). DHCP, DNS, and small TCP segments fit; a full 1514-byte frame does not. A
  larger MTU needs a multi-page contiguous DMA region, which the spawn path does not build yet. This
  is a demonstrator limit, not a protocol one.

DHCP is itself UDP, so smoltcp's UDP path over our NIC is exercised end to end by this test. What is
not yet built is the client-facing socket contract that lets *other* processes use the stack.

## Piece 3 phase B: the client-facing socket contract (built, both ISAs)

The §25 contract, so a process other than netstack can open sockets. netstack, after DHCP, serves requests
on a `Stack` endpoint; a client holds `WRITE` on it plus its own untyped budget. Files:
`crates/socket_proto/src/lib.rs` (the wire format), the serve loop in `user/src/netstack.rs`, and the client in
`user/src/netcli.rs` (a module of the netstack binary, dispatched by the entry role, see the archive
note below).

- **A socket is a socket id.** Open returns a small integer, carried in the request word of every
  later call; the per-connection **shared frame** is the real granted resource, delegated once at
  open via `SEND_CAP` and mapped by netstack at a per-socket VA. No ambient network: the client acts only
  through the `Stack` capability it was granted, and bytes cross in the shared frame, never in a
  message.
- **Operations.** `ATTACH_FRAME` is a `SEND_CAP` (it carries the frame). The rest are `CALL`s (which
  mint the reply cap netstack answers on), the socket id packed into the request word: `OPEN_UDP` /
  `OPEN_TCP`; `SENDTO(len)` and `RECV() -> len` for UDP (destination and payload in the shared
  frame); `CONNECT` / `SEND(len)` / `RECV()` for TCP; `CLOSE`. A blocking `RECV` is netstack driving the
  smoltcp poll loop (WAIT on the NIC interrupt) until the socket has data, then replying, the disk
  driver's discipline one layer up.
- **Frame layout, pinned.** One data region reused per operation, NOT a split TX/RX ring: the
  phase-one contract is one *synchronous* exchange per `CALL` (the client blocks in the CALL while
  netstack drives the network), so a request's payload and its reply never coexist. A split ring becomes
  necessary only with asynchronous or streaming sockets, deferred with the concurrency model.
- **Concurrency model, phase one:** single-threaded netstack, one synchronous exchange per request. netstack
  blocks on the `Stack` endpoint between requests and drives the network inside handling one. This
  suits the `std::net` PAL's blocking calls; concurrent connections and listening sockets want either
  userspace threads (milestone 19c TCBs) or a select-like wait, the phase-two extension.
- **One binary, one archive entry.** The client rides in the netstack binary (a nonzero entry role runs
  it) rather than a separate binary, because the crickerfs archive directory held at most 15 files at
  the time and the initrd was already near that ceiling. (The ceiling is `crickerfs::MAX_FILES`, 76
  since 2026-08-01; see [crickerfs.md](crickerfs.md). The decision stands on its own merits, but the
  pressure behind it is gone.) A subtlety worth recording: netstack reports its DHCP
  lease with a *blocking* `send`, so the spawn service drains that report before returning, or netstack
  never reaches its serve loop and the client's first request hangs. That was the one real bug in
  bring-up, caught by a watchdog hang.

### What the gate proves, and what it does not

Both tests are deterministic and zero-host-setup, run over both the mmio and the PCI-behind-IOMMU
transports, on aarch64 and riscv64:

- **UDP, `a_client_resolves_dns_through_the_socket_contract`.** A real DNS A-query for `example.com`
  to slirp's built-in resolver (10.0.2.3:53); the client verifies the reply is a response (QR bit) to
  its own transaction id. Proves UDP send and receive through the whole path (client, netstack, smoltcp,
  confined NIC). It relies on the test host being able to resolve DNS, which slirp forwards; a host
  with no resolver would make this time out.
- **TCP, `a_client_echoes_over_tcp_through_the_socket_contract`.** A full round trip against a slirp
  `guestfwd` echo peer: the runners add `guestfwd=tcp:10.0.2.9:7777-cmd:/bin/cat` to each NIC's
  `-netdev user`, so a guest connection to 10.0.2.9:7777 is piped to a fresh `/bin/cat`. The client
  does OPEN_TCP, CONNECT (the three-way handshake completes against a real peer), SEND a payload,
  RECV the echo and check it byte for byte, then CLOSE (the FIN). No host port is bound and nothing
  outlives QEMU, so the whole round trip, handshake through bidirectional data to teardown, is in the
  committed gate with zero host setup. Verified against QEMU 11.0.2.

**Not proven by the gate:** inbound connections. A `LISTEN`/`accept` verb plus a QEMU `hostfwd` (host
port -> guest) is the way to test the guest accepting a connection, and that is future work; the
contract has no listen verb yet. The concurrency model above is the other limit: one synchronous
exchange at a time, no overlapping connections.

This binds milestone 27's `std::net` PAL, replacing its `Unsupported`. Scope discipline held: TCP,
UDP, DHCP, no sockets-API mimicry beyond what the PAL needs.

### Ephemeral ports must be independent of the socket id (a fix the PAL found)

The first version of netstack derived a socket's local port from its socket id (`LOCAL_PORT_BASE + sid`).
That is wrong, and the `std::net` PAL flushed it out: a program that opens a TCP socket, closes it,
and opens another reuses the same socket id, so it reused the exact local port. Reconnecting to the
same peer on an identical 4-tuple whose slirp flow had not yet cleared makes the SYN go unanswered,
and netstack stalled in its bounded connect poll forever. Bisection confirmed it: a fresh id connects, a
reused id hangs.

The fix is what any real stack does: netstack allocates ephemeral local ports from a private range with a
**rotating allocator** independent of the socket id (`user/src/netstack.rs`, `PortAllocator`). Each open
advances the cursor, so a just-closed connection's port is not handed out again until the whole range
has cycled, and a port a live socket still holds is skipped outright. Socket-id reuse is then safe:
the reopened socket gets a new local port, a new 4-tuple, and a new slirp flow.

The regression is `a_reopened_socket_id_connects_again_over_tcp` (both ISAs): open a TCP socket,
connect to the guestfwd echo peer, close, reopen the *same* id, and connect again; the client reports
OK only if both connects complete. Before the fix the second connect hangs the way the PAL saw.

### The RX poll must honor smoltcp's timers, not just the interrupt (a riscv-SMP lost-wakeup)

`std_net` hung on riscv under the 4-hart boot, watchdog-killed with every core idle and every thread
blocked, while the same test passed on aarch64 and the lighter DHCP and TCP-echo tests passed on
riscv in the same run. The shape said lost wakeup; the cause was subtler than a dropped interrupt.

The old server loop blocked on the NIC interrupt between polls: `poll; if done break; WAIT; ack`. That
is fine until an exchange depends on one of smoltcp's *own* timers. smoltcp drives TCP retransmits,
delayed ACKs, and DNS timeouts from its clock, which only advances when we call `poll`. If a segment's
ACK is dropped, the peer goes quiet waiting for our retransmit, and our retransmit is a timer event
that only fires on the next `poll`, which we are not doing because we are blocked on an interrupt the
peer will never send. netstack waits for the peer, the peer waits for netstack, both idle. Instrumenting the
PLIC at the hang showed the truth: **no source pending, the net source still enabled**. Not a masked
line, not a lost IRQ; the device was simply idle, because both ends were waiting on the same stalled
timer. aarch64 happened never to drop a segment (different servicing latency), so it never armed the
retransmit path; the riscv SMP scatter, which moves the driver and its wakes across harts, was slow
enough to drop one and expose the hole. It was not the IRQ affinity: forcing every source back to the
boot hart's PLIC context still hung, which ruled the PLIC out.

The fix (`wait_for_nic`, `user/src/netstack.rs`) asks smoltcp when it next needs to run. With **no** timer
pending (`poll_delay` is `None`), it blocks on the interrupt, the common case, 0% CPU until a frame
arrives, and correct because with nothing of our own outstanding we are purely waiting on the peer,
whose retransmit will wake us. With a timer **pending**, it does not block: it yields and lets the
loop re-`poll`, so the timer fires and the retransmit goes out. The busy interval is confined to the
short retransmit window rather than the whole exchange. Both the DHCP bring-up loop and the
service loop use it. `std_net` then completes on both ISAs at the 4-hart/4-core boot.

The honest caveat: yielding across a retransmit window spins a hart until the timer is due (bounded by
the exchange, and by a 15 s per-call backstop well under the 60 s watchdog). The clean version is a
*timed* wait, a `WAIT` that returns on either the interrupt or a deadline, so the server sleeps
through the backoff instead of spinning. That is a small kernel-surface addition (an `Irq::WAIT`
timeout, or a timer notification) and is left as the follow-up; the yield-poll is correct and needs no
new syscall.

### The UDP gate must not depend on the host's resolver (a testing-hygiene defect, fixed)

The UDP socket-contract test queried `10.0.2.3:53` and called it "slirp's built-in resolver". That
description was wrong, and the error was not cosmetic: **10.0.2.3 is not a resolver.** libslirp
implements no DNS server. It NATs anything sent to its guest-visible nameserver address to the
*host's* configured nameserver, which it looks up with `get_dns_addr_libresolv`. So every run of that
"zero host setup" gate sent a real query out of the machine to whatever resolver the developer's
laptop happened to be using, and passed only if that resolver answered in time.

It was measured, not argued. A temporary instrument in the client sent the same `example.com` query
40 times in one boot and reported what came back:

- **1 of 40 queries got no answer** (2.5%), with a 15 s wait per query, so it is loss and not slowness.
- The answer's `ANCOUNT` was 2 and its first A record was `0x6814179a` = `104.20.23.154`, byte for
  byte what `dig @192.168.8.1 example.com` returned on the host at that moment. libslirp carries no
  zone data for `example.com` and cannot invent Cloudflare's rotating addresses, so this is direct
  proof that the host's resolver answered the guest's query.
- The same host resolver, probed directly with `dig +tries=1 +time=2`, dropped 1 of 30.

Two DNS queries ran per suite (mmio and PCIe), so a suite failed a few percent of the time from
nothing but a dropped packet on somebody's LAN, which matches the roughly one-in-three seen across a
handful of runs once network conditions were worse. UDP has no retransmit of its own and the client
sent exactly once, so a single lost datagram was a failed gate.

**The fix keeps the coverage and removes the dependency.** The gating UDP test now talks to slirp's
**own TFTP server** (`tftp=` on the netdev, at the gateway `10.0.2.2:69`), which libslirp answers
itself. The client sends a read request for a fixture the runners plant and asserts the reply is
`DATA`, block 1, with the fixture's exact bytes. This is the UDP twin of the guestfwd `/bin/cat` echo
peer the TCP gate already used: QEMU provides the service, nothing leaves the emulator, and no packet
can be dropped by a third party.

What the gate proves now: a client holding only a `Stack` endpoint and a shared frame can open a UDP
socket by id, send a datagram to an address of its choosing, and read the reply back through the same
frame, over both the mmio and PCIe transports, on both ISAs. That is the whole client-to-netstack-to-
smoltcp-to-confined-NIC path, which is what the test was ever really for.

What it no longer proves, deliberately: that DNS resolution works, or that the guest can reach
anything outside the emulator. That case did not get deleted; it became **non-gating**. The client
still sends a real query (now with three attempts, which is ordinary resolver behaviour rather than a
widened timeout) and reports a distinct `NO_ANSWER` when the host never replies, which the kernel test
prints and skips. A reply that arrives but is *not* a valid answer to our transaction still fails the
suite, because that would be our defect rather than the network's. So a broken host resolver, or an
offline laptop, now skips a check instead of failing a build, and a broken socket contract still fails
loudly.

The PCIe DNS variant is gone, not lost: UDP over the PCIe transport is now covered by the TFTP gate's
PCIe twin, deterministically.
