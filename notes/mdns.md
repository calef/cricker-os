# mDNS/DNS-SD: the Time Machine advertisement

Milestone 55's second protocol. A Mac's Time Machine UI lists only servers it discovered over
multicast DNS, and the reference router advertises exactly three service types: `_smb._tcp` (the
file service and its port), `_adisk._tcp` (the Time Machine flags, which are what populate the
backup-disk list), and `_device-info._tcp` (a model string, which picks the icon). The requirement
was measured, not assumed: `dns-sd -B _adisk._tcp` on the family network returns the router, so the
working reference does it, and proving it unnecessary would mean disabling it on a working family
backup system (design/roadmap/55-time-machine.md, "mDNS is required after all").

Three pieces exist or are pending:

- **`crates/mdns_proto`** (built): the DNS wire format, compression handling, the DNS-SD PTR/SRV/TXT
  structuring, the probe-before-claim wire halves, and `respond()`, the responder's entire decision
  as a pure function. Host-tested against real captured router packets; Kani harnesses cover the
  parser's termination and bounds.
- **The stack half** (built, both ISAs): smoltcp's `multicast` feature is on and `net_stack` joins
  224.0.0.251 at startup; `BIND_UDP` claims a fixed port against a granted range
  (`socket_proto::udp_bind_grant`, the UDP twin of milestone 107's listen grant, riding the high
  half of the same spawn word); a UDP `RECV` reply carries the datagram's source endpoint in the
  frame's dst fields. The gate is below.
- **The responder program** (not built, deliberately): the next lane. Everything it needs from the
  stack now exists; what remains is `mdns_proto`'s `respond()` wired to a `BIND_UDP(5353)` socket,
  spawned with the grant.

## The measured reference, captured 2026-08-15

All three service types were captured from calef's router (GL.iNet GL-BE9300 running OpenWrt,
Samba with `vfs_fruit`, the working family Time Machine server) by sending one-shot PTR queries
from the dev machine. The router answered from `192.168.8.1:5353`; the full hex is in
`crates/mdns_proto/src/tests.rs` as the primary test vectors. Decoded:

| Record | `_smb._tcp` | `_adisk._tcp` | `_device-info._tcp` |
|---|---|---|---|
| PTR target | `GL-BE9300.<type>.local` | same | same |
| SRV port | **445** | **0** | **0** |
| SRV target | `GL-BE9300.local` | same | same |
| TXT | one empty string | `dk0=adVN=corinne,adVF=0x82`, `dk1=adVN=chris,adVF=0x82`, `sys=adVF=0x100` | `model=MacSamba` |

Plus an A record (`192.168.8.1`) and an AAAA in every response. Findings that beat the documentation:

- **One `_adisk` instance for all shares.** The disks are `dkN=` entries inside a single TXT record.
  Emitting one announcement per share would be wrong.
- **Two disks is correct.** The roadmap block described three users; graeme migrated from macOS to
  Windows and his share was dropped, so the reference advertises corinne and chris (calef confirmed,
  2026-08-15).
- **`model=MacSamba`, not `TimeCapsule`.** The router's own Samba config sets
  `fruit:model = TimeCapsule`, and its mDNS advertisement says `MacSamba` anyway: the SMB-side AAPL
  model and the `_device-info` TXT are separate knobs, and the working reference runs with them
  disagreeing. Whatever `fruit:model` buys, it is not this record. The crate therefore takes the
  model as data.
- **`_adisk` and `_device-info` advertise SRV port 0.** They carry flags, not a connectable service.
- **Legacy unicast shape confirmed** (RFC 6762 §6.7): our queries came from an ephemeral port, and
  the router echoed the ID, included the question, put all five records in the answer section, set
  no cache-flush bits, and capped every TTL at 10.

The flag values are copied as measured; **the meaning of the `adVF` bits is not decoded here**, and
does not need to be until something wants to emit different ones.

## The smoltcp multicast answer (the question the roadmap block asks first)

**The tree's smoltcp is 0.13.1** (`Cargo.lock`; pinned in `user/Cargo.toml` with
`default-features = false` and features `alloc`, `medium-ethernet`, `proto-ipv4`, `proto-dhcpv4`,
`socket-udp`, `socket-tcp`, `socket-dhcpv4`).

**smoltcp 0.13.1 supports what mDNS needs, and the tree has it switched off.** The `multicast`
cargo feature (in smoltcp's own default set, which `default-features = false` discards) provides:

- `Interface::join_multicast_group` / `leave_multicast_group` (`iface/interface/multicast.rs`),
  with IGMP membership reports sent and IGMP queries answered for IPv4 groups (MLD for IPv6).
- Receive-path acceptance: `process_ipv4` drops any packet whose destination is not us, broadcast,
  or a joined group (`has_multicast_group`). **Without the feature, the only IPv4 multicast group
  accepted is `224.0.0.1`** (all-systems, hardcoded), so datagrams to `224.0.0.251` are discarded
  before UDP ever sees them. The ethernet layer already accepts multicast MAC frames either way;
  the filter that matters is the IP one.

So the responder is *nearly* ordinary socket code, and the distance was measured in small pieces.
**All three landed with the stack half** (milestone 55, the lane after the one that wrote this
note), the shapes the sizing proposed:

1. **The feature flag**: `"multicast"` in `user/Cargo.toml`'s smoltcp features. Landed as its own
   commit, being a change to a vendored-engine pin's configuration.
2. **The join**: `net_stack` joins 224.0.0.251 right after DHCP configures, then polls so the IGMP
   membership report carries a real source address. Membership is interface state, not socket
   state, so the join is unconditional; what is granted per client is the port.
3. **Socket surface.** The three gaps, closed:
   - `OP_BIND_UDP` (name provisional) binds a **fixed** UDP port, checked against a **UDP bind
     grant** the spawn site packs with `socket_proto::udp_bind_grant` into the high half of the
     same spawn word milestone 107's listen grant occupies. The halves are independent
     authorities; the zero word still grants nothing anywhere. The reply vocabulary is `LISTEN`'s
     three outcomes, which are properties of claiming a port, not of TCP.
   - A UDP `RECV` reply now writes the datagram's **source endpoint** into the shared frame's
     `dst_ip`/`dst_port` fields, the dead-space proposal above, taken. TCP `RECV` leaves them
     untouched; the peer is fixed by the connection.
   - `OP_SENDTO` to a multicast destination was measured, not trusted: the QEMU gate's host-side
     prober takes the guest's group-addressed datagram off the raw wire.

What was *not* needed is any change to smoltcp itself.

## The QEMU gate: what it proves, and how

Slirp cannot carry multicast in either direction, so the gate goes under it. When xtask runs the
suite, the runners attach the mmio NIC to a **QEMU hub** (`-netdev hubport`) with two backends:
slirp, unchanged (DHCP, TFTP, guestfwd, hostfwd all keep working, because a hub floods every frame
to every port), and a `-netdev socket` listener that xtask's **multicast prober** connects to,
speaking QEMU's frame protocol (4-byte big-endian length, then the raw ethernet frame). The prober
is the multicast twin of milestone 107's inbound prober: constructed before the child so the
runner inherits `NIFE_MCAST_PORT`, passive for the whole boot, reported after the suite.

The exchange rides **inside milestone 107's accept test**
(`a_host_process_connects_to_the_guest_and_is_answered`, both ISAs), after its TCP rounds, rather
than in a spawn of its own: a net server's spawn is ~154 frames nothing ever reclaims, and a
twelfth one died as `Unmappable(OutOfFrames)` in an unrelated later test, the exact failure
notes/net.md's memory receipt predicted. The fold has a side benefit: that spawn's grant word
carries `listen_grant(7778, 7778) | udp_bind_grant(5353, 5353)`, so the *composed* packing is what
the machine exercises, not one half alone. The mDNS half
(`udp_mdns_half` in `user/src/socket_test_client.rs`):

1. The client asks to bind a port outside the grant: refused as **authority** (`LISTEN_DENIED`).
   5353 binds; a second bind of 5353 collides (`LISTEN_IN_USE`).
2. The guest multicasts a trigger to 224.0.0.251:5353. The prober receives it off the raw wire,
   which is the measurement that multicast `SENDTO` reaches it (smoltcp maps the group to the
   multicast MAC without ARP, as predicted; now proven).
3. The prober injects a UDP datagram **addressed to the group**, not to the guest, from a spoofed
   source (10.0.2.99:5353) nothing on the virtual network holds. The guest's `RECV` returns it,
   which is the RX-acceptance proof the `multicast` feature exists for, and the client asserts the
   spoofed source came back in the frame header, ip and port both.
4. The guest multicasts a composed answer (different bytes, the inbound gate's anti-echo
   discipline), and the prober requires it, closing the loop from outside.

The TFTP gate carries the slirp-shaped half of source reporting on both ISAs: it asserts the DATA
packet's source and ACKs to it, which is what TFTP's TID scheme (RFC 1350 §4) wanted all along and
is the same reply-to-the-querier move an mDNS legacy-unicast responder makes.

## EXAMPLES

Re-deriving the reference vectors needs no special tooling. From any machine on the router's
network, a one-shot legacy query (Python, ephemeral port, so the answer arrives unicast):

```python
import socket, struct
def qname(n):
    return b"".join(bytes([len(l)]) + l.encode() for l in n.split(".") if l) + b"\x00"
q = struct.pack(">HHHHHH", 0, 0, 1, 0, 0, 0) + qname("_adisk._tcp.local") + struct.pack(">HH", 12, 1)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(3)
s.sendto(q, ("224.0.0.251", 5353))
print(s.recvfrom(4096)[0].hex())
```

Or, on macOS, the resolver's own view (which exercises the multicast path a Mac actually uses):

```sh
dns-sd -B _adisk._tcp             # browse: who advertises Time Machine disks
dns-sd -L GL-BE9300 _adisk._tcp local   # resolve: the TXT keys and SRV
```

The `-L` output presents decoded TXT entries; the Python capture gives the wire bytes, which is
what a test vector needs.

## BUGS

- **What the QEMU gate cannot prove, for the bench to pick up.** The hub is a wire with no router
  on it, so everything a real network's multicast turns on is out of its reach: **IGMP snooping**
  (a switch that forwards group traffic only to reported members; the gate never checks the
  guest's membership report is well-formed enough to satisfy one, only that acceptance works),
  **TTL handling by real forwarding** (the injected frame carries TTL 255 but nothing routes it),
  coexistence with a real network's **mDNS chatter** (the gate's group traffic is exactly three
  known datagrams; a live segment delivers a firehose of other hosts' queries and announcements to
  every member, and nothing here proves the stack keeps up or that the 2048-byte socket buffer
  survives it), and **a real querier**: no Mac's mDNSResponder has asked this stack anything. The
  bench on hardware, on the family network, with `dns-sd -B` as the client, is where those claims
  get proven; the gate's job is only that the stack's own filters, grants, and headers are right.
- **The injected query is a payload marker, not a DNS message.** The gate proves carriage, not
  protocol; `mdns_proto`'s host tests against the captured router packets prove protocol. The
  responder lane joins the two, and its gate should inject a real query through this same hub.
- **The prober holds one TCP connection and never reconnects.** If QEMU drops the frame socket
  mid-run the check fails as "reading frames failed" rather than retrying; acceptable for a gate,
  recorded so its first flake is not a mystery.
- **The multicast-response shape is asserted from the RFC, not from a capture.** All three captured
  packets are legacy unicast responses (the capture tool cannot bind 5353 while mDNSResponder holds
  it). The crate's multicast responses (ID 0, additionals, cache-flush, TTL 4500/120) follow RFC
  6762 and are pinned by tests, but no working implementation's multicast bytes have been compared.
  Capturing the router's *multicast* answer to a Mac's real browse (tcpdump on port 5353) would
  close that, and is cheap for whoever next holds a root shell on the network.
- **No AAAA emission.** `Advertisement` carries an optional IPv4 address only. The reference emits
  AAAA; a Mac on an IPv6-only network would not find us. Add the field when the responder exists to
  use it.
- **`respond()` does not act on the QU bit** (it parses; the responder answering multicast either
  way is always legal, just occasionally chattier).
- The crate's own BUGS section (`crates/mdns_proto/src/lib.rs`) records the wire-level limits:
  uncompressed emission, PTR-only known-answer suppression, probe timing left to the caller, no
  TC-bit delay.
- **smoltcp's `socket-mdns` feature was not evaluated.** It exists in 0.13.1 for *client-side*
  DNS-over-multicast lookups (a `socket-dns` variant), not for a responder, so it does not change
  the verdict above; recorded so nobody re-derives that.
