# The network stack as a confined component (milestone 30)

Milestone 30 is three pieces in order: multi-queue transport confinement, a userspace virtio-net
driver behind it, and a TCP/IP stack (smoltcp) in a net server speaking a capability-shaped socket
contract. This note records what is built, the prior art read before drawing the contract, the
contract proposal (a design fork left for calef), the smoltcp pin, and the remaining work.

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

**seL4 net_stack componentization** (and the CAmkES/lwIP and later Rust efforts). The stack is a
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

### Prior art for the *inbound* half: who may claim a listening port

Read for milestone 107, after the contract above was already built. The outbound question ("how does
a process reach a stack") and the inbound one ("who decides this program may serve port 80") turn out
to have almost disjoint prior art, which is itself worth knowing.

**POSIX, Linux and the BSDs: ambient above 1024.** Any process binds any free port. Below 1024 needs
`CAP_NET_BIND_SERVICE` on Linux, or historically root, and that is a **privilege rather than a
designation**: it says "this process may bind low ports", never *which* ones. Linux later added
`net.ipv4.ip_unprivileged_port_start` to move the line, which is the same knob in a different place.
`SO_REUSEPORT` lets several sockets share one port, so even exclusivity is opt-out.

**systemd socket activation, and `inetd` before it: the closest mainstream precedent, and it does not
enforce.** The service manager binds the port and passes the listening descriptor to the service
(`LISTEN_FDS`); the service never calls `bind`. That is exactly this milestone's shape: whoever
spawns you decides what you may serve. **The difference is enforcement.** Nothing stops a systemd
service binding port 9999 itself afterwards, because the pre-bound descriptor is a convenience rather
than a boundary. Here `NO_LISTEN_GRANT` means the server *cannot*, because the grant is checked at
the only place a port can be claimed. Read as: the mainstream pattern with the hole closed.

**Capsicum (FreeBSD): the same instinct, taken further.** After `cap_enter()` a process cannot `bind`
at all and must receive pre-bound sockets. Capsicum's answer to "which ports may this program serve"
is "none, ever, only what it is handed". That is stricter than ours and it is the honest upper bound
on this design.

**seL4: silent, and the silence is the finding.** The kernel models no ports, no sockets and no TCP;
networking is a userspace component (CAmkES/lwIP, later Rust). Whatever a net server implements *is*
the policy. So there is no seL4 answer to diverge from here, and any claim that "seL4 does X" means
"some unverified seL4 component does X", since seL4's proofs cover the kernel and a port policy is
outside them. **The same is true of us**: nothing in `crates/socket_proto` is machine-checked, and
this grant is ordinary code.

**Plan 9: authority by namespace.** You write `announce 80` into `/net/tcp/clone`, and what
constrains you is whether `/net/tcp` is in your namespace at all. Restriction is by what you can
*see* rather than by an explicit grant, which is the counter-design already recorded above.

### Where this differs from seL4, and what it costs

The data plane here **is** seL4's, deliberately (see the synthesis above). The difference is one level
up, in how a component gets its authority:

**seL4 systems wire statically.** CAmkES describes the whole system at build time (components,
endpoints, dataports, connections) and generates the glue; seL4 Microkit does the same with protection
domains and channels in a system description. There is no "spawn a net server and hand it port 7778
because somebody just asked". The topology is fixed before the machine boots.

**This tree grants at spawn, at run time.** `wire_net_server(..., listen_grant(7778, 7778))` is a
decision taken while the system runs, by whoever holds the authority to take it.

**What that buys**: authority that moves during operation, which is the whole of milestone 47's
caretakers and the shell's grant model. A static system cannot express a user handing a program a
port range it was never declared with.

**What it costs, and this is the honest half.** A static topology is checkable *as a whole*: a CAmkES
spec can be read to see every channel that will ever exist. Our grant is a `u64` computed at a call
site, so "which components may serve inbound ports?" has no artifact anyone can read. That is a real
loss against seL4, not a tie, and it is the same weakness this tree keeps meeting in other forms: a
fact that exists only at a call site (compare §76's roadmap status, and `script/names`' reason for
existing).

### One correction to the claim below

The section on listeners says POSIX "conflates" a listener and a connection. That is imprecise and
the sharper version is worth having: POSIX's `accept()` already returns a **new** descriptor, so the
two are distinct objects there too. What POSIX does is make them the same *type*, with hidden state
deciding which calls are valid on which. The claim that survives is narrower and stronger: here they
are distinct **authorities**, because a listener carries no frame and therefore structurally cannot
move bytes, where a POSIX listening descriptor is a `read`/`write`-shaped thing that merely fails.

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

1. **Socket identity: DECIDED 2026-07-28 (calef): a socket id on the stack endpoint for phase
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
(milestone 27) require. In the build we shipped, net_stack does use `alloc` (over user_rt's `UntypedHeap`,
milestone 27) because it is available and makes the socket set and per-frame buffers simpler; the
`alloc` feature is a convenience, not a precondition, so a fixed-capacity server remains possible if
that heap were ever unavailable.

## Piece 3 phase A: smoltcp doing DHCP over the confined NIC (built, both ISAs)

The net server, `net_stack` (user/src/net_stack.rs), is the networking form of the userspace-reuse thesis: a
real, reused TCP/IP stack (smoltcp 0.13.1, not hand-built) running entirely at EL0 over a NIC the
kernel confines by DMA. The kernel knows nothing about DHCP.

- `user/src/net_transport.rs` presents smoltcp's `phy::Device` over the receive/transmit virtqueues: it brings
  the NIC up through the `Virtio` capability, posts receive buffers, copies received frames out (RX
  tokens own their bytes so they never borrow the device), and transmits via the DMA ring (TX tokens
  carry a raw pointer to the device, sound because net_stack is single-threaded and the device outlives
  any token within a poll).
- `net_stack` links `alloc` over user_rt's `UntypedHeap`, builds a smoltcp `Interface` and a DHCP socket,
  and runs the poll loop, blocking on the NIC interrupt between polls. It reports the acquired
  address, which the test asserts lands in slirp's 10.0.2.0/24 (`the_net_server_acquires_a_dhcp_lease_over_smoltcp`
  and its `_pci` twin, both ISAs). Only a real DHCP handshake driven by smoltcp over the confined NIC
  produces that.
- The spawn service (`virtio_service::start_net_server{,_pci}`) grants net_stack the confined transport,
  the interrupt, a DMA page, a report endpoint, and an **untyped budget** for the heap, plus extra
  stack pages for smoltcp's packet building.
- **Caveat (recorded):** the DMA region is one 4 KiB page, so the buffers are small and the MTU is
  small (`net_transport::MTU`, 576). DHCP, DNS, and small TCP segments fit; a full 1514-byte frame does not. A
  larger MTU needs a multi-page contiguous DMA region, which the spawn path does not build yet. This
  is a demonstrator limit, not a protocol one.

DHCP is itself UDP, so smoltcp's UDP path over our NIC is exercised end to end by this test. What is
not yet built is the client-facing socket contract that lets *other* processes use the stack.

## Piece 3 phase B: the client-facing socket contract (built, both ISAs)

The §25 contract, so a process other than net_stack can open sockets. net_stack, after DHCP, serves requests
on a `Stack` endpoint; a client holds `WRITE` on it plus its own untyped budget. Files:
`crates/socket_proto/src/lib.rs` (the wire format), the serve loop in `user/src/net_stack.rs`, and the client in
`user/src/socket_test_client.rs` (a module of the net_stack binary, dispatched by the entry role, see the archive
note below).

- **A socket is a socket id.** Open returns a small integer, carried in the request word of every
  later call; the per-connection **shared frame** is the real granted resource, delegated once at
  open via `SEND_CAP` and mapped by net_stack at a per-socket VA. No ambient network: the client acts only
  through the `Stack` capability it was granted, and bytes cross in the shared frame, never in a
  message.
- **Operations.** `ATTACH_FRAME` is a `SEND_CAP` (it carries the frame). The rest are `CALL`s (which
  mint the reply cap net_stack answers on), the socket id packed into the request word: `OPEN_UDP` /
  `OPEN_TCP`; `SENDTO(len)` and `RECV() -> len` for UDP (destination and payload in the shared
  frame); `CONNECT` / `SEND(len)` / `RECV()` for TCP; `CLOSE`. A blocking `RECV` is net_stack driving the
  smoltcp poll loop (WAIT on the NIC interrupt) until the socket has data, then replying, the disk
  driver's discipline one layer up.
- **Frame layout, pinned.** One data region reused per operation, NOT a split TX/RX ring: the
  phase-one contract is one *synchronous* exchange per `CALL` (the client blocks in the CALL while
  net_stack drives the network), so a request's payload and its reply never coexist. A split ring becomes
  necessary only with asynchronous or streaming sockets, deferred with the concurrency model.
- **Concurrency model, phase one:** single-threaded net_stack, one synchronous exchange per request. net_stack
  blocks on the `Stack` endpoint between requests and drives the network inside handling one. This
  suits the `std::net` PAL's blocking calls; concurrent connections and listening sockets want either
  userspace threads (milestone 19c TCBs) or a select-like wait, the phase-two extension.
- **One binary, one archive entry.** The client rides in the net_stack binary (a nonzero entry role runs
  it) rather than a separate binary, because the crickerfs archive directory held at most 15 files at
  the time and the initrd was already near that ceiling. (The ceiling is `crickerfs::MAX_FILES`, 76
  since 2026-08-01; see [crickerfs.md](crickerfs.md). The decision stands on its own merits, but the
  pressure behind it is gone.) A subtlety worth recording: net_stack reports its DHCP
  lease with a *blocking* `send`, so the spawn service drains that report before returning, or net_stack
  never reaches its serve loop and the client's first request hangs. That was the one real bug in
  bring-up, caught by a watchdog hang.

### What the gate proves, and what it does not

Both tests are deterministic and zero-host-setup, run over both the mmio and the PCI-behind-IOMMU
transports, on aarch64 and riscv64:

- **UDP, `a_client_resolves_dns_through_the_socket_contract`.** A real DNS A-query for `example.com`
  to slirp's built-in resolver (10.0.2.3:53); the client verifies the reply is a response (QR bit) to
  its own transaction id. Proves UDP send and receive through the whole path (client, net_stack, smoltcp,
  confined NIC). It relies on the test host being able to resolve DNS, which slirp forwards; a host
  with no resolver would make this time out.
- **TCP, `a_client_echoes_over_tcp_through_the_socket_contract`.** A full round trip against a slirp
  `guestfwd` echo peer: the runners add `guestfwd=tcp:10.0.2.9:7777-cmd:/bin/cat` to each NIC's
  `-netdev user`, so a guest connection to 10.0.2.9:7777 is piped to a fresh `/bin/cat`. The client
  does OPEN_TCP, CONNECT (the three-way handshake completes against a real peer), SEND a payload,
  RECV the echo and check it byte for byte, then CLOSE (the FIN). No host port is bound and nothing
  outlives QEMU, so the whole round trip, handshake through bidirectional data to teardown, is in the
  committed gate with zero host setup. Verified against QEMU 11.0.2.

**Not proven by the gate, when this was written:** inbound connections. A `LISTEN`/`accept` verb
plus a QEMU `hostfwd` (host port -> guest) is the way to test the guest accepting a connection, and
that is future work; the contract has no listen verb yet. The concurrency model above is the other
limit: one synchronous exchange at a time, no overlapping connections.

**Milestone 107 built exactly that**, and the paragraph above is kept as the record of the gap
rather than edited away. See "The inbound half" below for the verbs, the two design questions they
raised, and what the gate proves now. The concurrency sentence still stands, with one qualification
the accept work earned: a single listener now serves connections one after another without dropping
the next, which is not the same as serving two at once.

This binds milestone 27's `std::net` PAL, replacing its `Unsupported`. Scope discipline held: TCP,
UDP, DHCP, no sockets-API mimicry beyond what the PAL needs.

### Ephemeral ports must be independent of the socket id (a fix the PAL found)

The first version of net_stack derived a socket's local port from its socket id (`LOCAL_PORT_BASE + sid`).
That is wrong, and the `std::net` PAL flushed it out: a program that opens a TCP socket, closes it,
and opens another reuses the same socket id, so it reused the exact local port. Reconnecting to the
same peer on an identical 4-tuple whose slirp flow had not yet cleared makes the SYN go unanswered,
and net_stack stalled in its bounded connect poll forever. Bisection confirmed it: a fresh id connects, a
reused id hangs.

The fix is what any real stack does: net_stack allocates ephemeral local ports from a private range with a
**rotating allocator** independent of the socket id (`user/src/net_stack.rs`, `PortAllocator`). Each open
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
peer will never send. net_stack waits for the peer, the peer waits for net_stack, both idle. Instrumenting the
PLIC at the hang showed the truth: **no source pending, the net source still enabled**. Not a masked
line, not a lost IRQ; the device was simply idle, because both ends were waiting on the same stalled
timer. aarch64 happened never to drop a segment (different servicing latency), so it never armed the
retransmit path; the riscv SMP scatter, which moves the driver and its wakes across harts, was slow
enough to drop one and expose the hole. It was not the IRQ affinity: forcing every source back to the
boot hart's PLIC context still hung, which ruled the PLIC out.

The fix (`wait_for_nic`, `user/src/net_stack.rs`) asks smoltcp when it next needs to run. With **no** timer
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
frame, over both the mmio and PCIe transports, on both ISAs. That is the whole client-to-net_stack-to-
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

## The inbound half: the guest can be connected to (milestone 107)

Everything above is the guest as a client. The TCP gate connects out to a slirp `guestfwd` peer, the
UDP gate sends a request, DHCP is a client protocol. cricker-os could reach the network and could not
be reached, and the contract had no listen verb to fix that with.

Milestone 107 adds `LISTEN` and `ACCEPT` (`crates/socket_proto`, opcodes 9 and 10, **names
provisional**), the smoltcp side in `user/src/net_stack.rs`, and a gate in which a **host process
connects into the guest** and gets an answer the guest composed. Two design questions came with the
verbs, and neither was copied from POSIX.

### A listener is not a connection, because they are not the same authority

(Prior art and one correction to the wording below: see "Prior art for the *inbound* half" above.)

POSIX makes a listening socket and an accepted one both file descriptors; the only difference is
which calls happen to work on each. That conflation is why "give this program port 80" and "give this
program this connection" are the same kind of grant there, and it is not a shape this tree should
inherit.

Here they are two objects:

- A **listener** names a port. It is exclusive (two programs cannot both hold 80) and it carries **no
  shared frame at all**, because no bytes ever cross on it.
- A **connection** names a peer. It is the object `CONNECT` already produced, reached from the other
  direction, and the shared frame is its real granted resource exactly as §25 says.

The frame is what makes this more than a naming preference. §25 already decided that the per-socket
shared frame is the granted thing and the socket id is bookkeeping; a listener has no frame, so under
that decision it has nothing to grant, and the split falls out rather than being imposed. This is the
tree's existing habit of splitting authority by what a holder can *do* rather than by what it names,
which is `Frame` versus `DeviceFrame` and `WRITE` versus `GRANT` on one object.

So `ACCEPT` carries the socket id to install the connection at (`CALL(req(OP_ACCEPT, lsid),
target_sid)`), the client must have attached a frame there first, and **`target_sid == lsid` is
refused**: the contract will not let a listener become a connection in place. The listener keeps its
id, its port, and its authority.

The client-side proof costs nothing and is worth having: `TEST_TCP_LISTEN_GRANT` in
`user/src/socket_test_client.rs` attaches no frame anywhere and still listens, binds and collides.

### Who binds the port: the spawn service grants a range, the client does not ask

Outbound needs no permission beyond the `Stack` capability, and that is not laxity: an ephemeral
local port is allocated by `net_stack`'s own rotating allocator and contended by nobody, so there is
nothing to arbitrate. **Inbound is different in kind.** A listening port is a claim on an exclusive
name in a shared namespace, which is the same property that makes a directory a capability here
rather than a path (milestone 32's FS server) and the same reason `bind` (§50) is a grant.

So the port is not the client's to pick. `wire_net_server` spawns `net_stack` with a **listen
grant**, an inclusive range packed by `socket_proto::listen_grant` and carried in the spawn's `arg2`;
`LISTEN` outside it replies `LISTEN_DENIED`. Every outbound test passes `NO_LISTEN_GRANT`, which is
also the default, so **a net server that was never told which ports it may serve refuses all of
them**: inbound authority is granted, never assumed. `LISTEN_DENIED` is deliberately a different
reply from `LISTEN_IN_USE`, because "you were not granted this" and "somebody else has it" call for
opposite responses from a client.

**The honest limit, and it is the interesting one.** The grant's granularity is the `Stack` endpoint,
not the client, because an endpoint carries no sender identity: `net_stack` cannot tell two callers
apart, so two clients sharing an endpoint share its grant. In this tree each stack endpoint has
exactly one client today, so nothing is wrong yet, but a real multi-client net server needs a
**minted endpoint per client** and the grant then rides on that. That is the same deferred step §25
already recorded for socket identity, arriving from a second direction, which is itself the finding:
the thing that would make a socket an unforgeable object is the same thing that would make a port
grant per-client. If that step is ever taken, it should be taken once, for both.

### The concurrency model, and what "accept" had to mean to be worth shipping

The roadmap block warned that adding `LISTEN` to a stack handling one exchange at a time produces a
server that accepts a connection and then cannot accept the next one. smoltcp has no accept queue: a
`tcp::Socket` in `Listen` state *becomes* the established connection, and there is no separate
listener object to accept from again.

The answer is that `ACCEPT` **re-arms**. It hands the established socket to the target id and
immediately creates a fresh `tcp::Socket` parked in `Listen` on the same port under the listener's
id, before returning. Two smoltcp sockets then share a local port, one established and one listening,
which is safe because smoltcp's dispatch refuses to hand an ACK-bearing segment to a `Listen`-state
socket and refuses a segment from the wrong peer to an established one.

What that buys, honestly stated:

- A listener serves connections **one after another**, indefinitely, and does not go deaf after one.
- A handshake that arrives while the client is busy on an earlier connection **completes underneath
  it**, because `net_stack` drives the poll loop inside every blocking operation, and is waiting when
  the client next calls `ACCEPT`.
- The backlog is **one connection deep**. A second peer arriving in the window between a handshake
  completing and `ACCEPT` re-arming gets a RST rather than a wait.
- Two connections cannot be served *simultaneously*, because the client blocks in one `CALL` at a
  time. That is the phase-one limit recorded above, unchanged.

For milestone 54's file service that is the difference between usable and not: a Mac mounting a share
opens connections in sequence. For milestone 55's SMB3, which uses several at once, the remaining
step is real concurrency (userspace threads, or a select-shaped wait), not another verb.

### EXAMPLES: serving a port, end to end

**Spawn side.** Whoever wires the pair decides the inbound authority, and the client never asks for
it. `socket_proto::listen_grant(lo, hi)` packs an inclusive range into the one word `arg2` carries:

```rust
// A stack whose client may listen on 7778 and nothing else.
let report = virtio_service::start_net_stack(
    image,
    NET_TEST_TCP_ACCEPT,                                    // which exchange the client drives
    false,                                                  // mmio, not PCIe
    socket_proto::listen_grant(NET_LISTEN_PORT, NET_LISTEN_PORT),
)?;

// A stack that serves nobody inbound, which is every other net test in the tree.
let report = virtio_service::start_net_stack(image, NET_TEST_TCP_ECHO, false,
                                             socket_proto::NO_LISTEN_GRANT)?;
```

**Client side.** Bind the port, then accept into a *different* socket id that already has a frame.
The listener never gets a frame, and `ACCEPT` refuses to install a connection at the listener's own
id:

```rust
// 1. Bind. No frame is attached anywhere yet, because a listener carries no bytes.
match call(STACK, req(OP_LISTEN, LISTEN_SID), 7778).0 {
    LISTEN_GRANTED => {}
    LISTEN_DENIED  => /* this stack was never granted 7778: ask the spawner, not again */,
    LISTEN_IN_USE  => /* somebody already holds it: pick another port */,
    _ => unreachable!(),
}

// 2. The connection id is what carries data, so it is what gets the frame.
attach_frame(CONN_SID);

// 3. Accept, use, close, repeat. The listener re-arms inside ACCEPT, so this loop keeps working.
loop {
    if call(STACK, req(OP_ACCEPT, LISTEN_SID), CONN_SID).0 != REP_OK { break; }
    let (len, _) = call(STACK, req(OP_RECV, CONN_SID), 0);
    // ... read the request out of the frame at FRAME_VA + OFF_PAYLOAD, write the answer back ...
    let _ = call(STACK, req(OP_SEND, CONN_SID), reply_len);
    let _ = call(STACK, req(OP_CLOSE, CONN_SID), 0);   // the listener is untouched by this
}
```

`user/src/socket_test_client.rs::tcp_accept_inbound` is that sequence with the assertions in it.

**Running the gate.** It is part of the ordinary suite and needs no host setup; xtask picks a free
loopback port, hands it to the runner as `CRICKER_HOSTFWD_PORT`, and runs the prober thread itself:

```console
$ script/test                                  # both ISAs, inbound gate included
$ cargo xtask test --arch aarch64              # just the aarch64 leg
```

A plain `cargo xtask run` sets no `CRICKER_HOSTFWD_PORT`, so nothing binds a port on your machine
outside a test run.

### The gate: a host process connects to the guest, twice

`hostfwd` is `guestfwd`'s mirror, so this costs no host setup: QEMU listens on a loopback port and
forwards connections to the guest's `10.0.2.15:7778`. The runners add it to the **mmio** NIC only,
and only when `CRICKER_HOSTFWD_PORT` is set, which is the test flow and nothing else. That flag is
the one thing here that **binds a port on the developer's machine**, so it stays off a plain
`cargo xtask run` and off the benchmark boot, both of which share the runner.

The port is chosen by xtask, from the OS (`free_loopback_port`), rather than fixed. A fixed port
collides every time two lanes run the suite at once on one machine; an asked-for one collides only if
it is taken between the ask and QEMU's bind, and that failure is loud (QEMU refuses to start).

The host side is `InboundProber`, a thread beside the scanout referee and there for the same reason:
**nothing inside the guest can open a connection to the guest from outside it.** It connects, sends
`cricker-in!`, and requires `cricker-out!` back. The two strings differ on purpose: an echo would pass
even if the guest were only reflecting our bytes, and the claim is that the guest *composed* an
answer to a connection it did not make. It retries for the whole run, because nothing on the host
knows when the accept test starts; a connection that lands while another net test holds the NIC finds
no listener and is reset by smoltcp, which costs nothing.

It requires **two** completed connections, and the second is the load-bearing one. A listener that
accepts once and goes deaf would pass a one-connection gate, and is exactly what a file server cannot
use.

**A host prober must never abandon a connection because it is slow**, which the first green run
taught by failing in the most instructive way available: the guest passed and the prober reported
serving *zero*. A `connect` to a `hostfwd` port succeeds the moment QEMU accepts the host side; slirp
only then starts the guest side, and nothing answers that SYN until the guest is inside an operation
that polls smoltcp. Dropping the connection on a read timeout does **not** take back the payload
already written: slirp keeps the guest-side connection, completes the handshake whenever the guest
next polls, and delivers those bytes to a socket whose host end has gone. One retry every 100 ms for
a whole boot builds a queue of those, and the guest cheerfully served both of its rounds from
abandoned connections while the prober timed out on its own live one.

The fix is a rule rather than a longer timeout: a **timeout is not a reason to give up** (keep
reading the same connection until it answers, dies, or the run ends), while a **hard error is**, and
a cheap one, because a RST means the guest had no listener and consumed nothing. The general shape is
worth remembering for any host-side actor: an abandoned request may still be executed, so "I gave up
waiting" and "it did not happen" are different claims.

Both halves assert. The guest reports `OK` only if both rounds arrived with the right bytes and its
answers were sent (`a_host_process_connects_to_the_guest_and_is_answered`, both ISAs); the prober
fails the leg if what came back was not the guest's answer. The guest's half covers "somebody
connected"; the host's covers "and got the right answer", which the guest cannot know.

The **grant** half rides in the same exchange, ahead of the first connection: 8080 is refused as a
matter of authority, 7778 binds, and asking for 7778 again on a second socket id reports
`LISTEN_IN_USE`. No frame is attached until all of that has passed, which is the two-object claim
proved by construction rather than by assertion.

### The aarch64 test boot has run out of memory, and this is the receipt

That grant half wanted a test of its own, and could not have one. A second `net_stack` spawn costs a
**192-page untyped region that nothing ever reclaims** (`NET_SERVER_BUDGET_PAGES`), because the
server blocks in its serve loop forever and no supervisor reaps it. With two extra net servers in the
boot, a later test asking for 128 contiguous pages found **137 free frames and no run that long**;
the failure surfaced far away, as `time_tests` reporting "no swish program in the initrd archive, or
no memory to wire one", which reads like a packaging bug and is not one.

**The fix taken here was to stop over-provisioning.** `NET_SERVER_BUDGET_PAGES` was 192 on the
recorded reasoning that `net_stack` caps its heap at 128 pages "so 192 leaves headroom without being
unbounded", which is a margin nobody measured. It is now **128**, with `net_stack`'s `HEAP_MAX`
lowered to **96** so the budget still covers the heap's declared worst case with 32 pages for page
tables and clients' frame mappings. Ten net servers per boot at 64 pages saved each is **640 frames**
returned, which is six times what the new gate spends. The suite is what proves 96 is enough.

Three facts worth carrying forward, because milestones 54, 55 and 66 will all want more net tests:

- **RAM is pinned at 128 MiB and asserted.** `memory_map_came_from_the_device_tree` fails on any
  other size, deliberately, because a wrong number there means a misparsed `reg`. So "give QEMU more
  memory" is a decision with a test attached, not a knob.
- **The margin before this milestone was about 315 frames**, 1.3 MiB, at the end of the aarch64
  boot. One net test spends ~208 of them permanently (192 for the server, 16 for the client), so the
  suite could afford exactly one more net test and not two.
- **It is exhaustion, and it was measured, not inferred.** A temporary instrument in
  `untyped::create` printed `free=107, largest_run>=96` at the failing allocation, which settles the
  question a total-free number alone leaves open. Worth repeating if this bites again: the
  instrument is four lines and it turned "somebody's test is flaky" into a number in one run.

The fix is the same one `virtio::MAX_DEVICES` has now asked for eight times: reclaim what a dead or
finished service held. It is one piece of work that would relieve both ceilings, and it is
increasingly what stands between this suite and the next network milestone.

### What is still not proven, and what is deliberately out of scope

- **`std::net::TcpListener` is still `Unsupported`.** The block asked whether the PAL binding lands
  here or in milestone 27's follow-on, and the answer taken was: not here. The contract had to settle
  first, and it now has, so the PAL work is small and mechanical: `TcpListener::bind` is `OP_LISTEN`
  on a client-allocated socket id, `accept` is `OP_ACCEPT` into another one the PAL allocates and
  attaches a frame to, and the std client's stack must be spawned with a listen grant instead of
  `NO_LISTEN_GRANT`. `start_net_std` passes no grant today for exactly that reason: granting ports to
  a client that cannot spend them would be a lie in the capability graph.
- **Only the mmio transport carries the inbound gate.** A PCIe twin would need a second host port,
  and the transport is orthogonal to the accept path, which the outbound gates already prove over
  both buses. This is the same reasoning that retired the PCIe DNS variant.
- **No inbound UDP.** A UDP socket already binds a local port, but it binds an *ephemeral* one from
  the allocator, so nothing can address it from outside. A server-side UDP bind is a third use of the
  listen grant and is not built.
